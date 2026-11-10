#!/usr/bin/env python3
"""Assemble the reference from its legs.

The reference legs (scripts/schedule.py pack-reference, scripts/reference-legs.py)
each computed some candidate propagation / ephemeris rows and the OD runs of
some objects. This puts them back into the five files the rest of the pipeline
reads, exactly as the old per-object ASSEMBLE_SHARDS did:

    validation_rust_propeph.json        every prop+eph row, objects in catalog order
    validation_rust_od.json             every OD row, objects in catalog order
    validation_rust_od_orbits.jsonl     the OD orbit sidecar, objects in catalog order
    validation_rust_od_compare.jsonl    the OD comparison sidecar, same order
    validation_rust.json                prop+eph rows then OD rows (the unified file)

    assemble-reference.py --schedule results/reference-schedule.json \\
        --candidates results/reference-candidates.json --legs-dir results/shards \\
        --out-propeph P --out-od O --out-orbits R --out-compare C --out-unified U \\
        [--out-dropped D]

This is a fan-in, and fan-ins are where a parallel pipeline goes quietly wrong:
a leg that died leaves its rows simply absent, and a report rendered from a
reference missing some of them looks exactly like a healthy one. So every
candidate row must be accounted for:

  * present in each leg the schedule gave it to, or named in that leg's
    dropped file (a row the engine refused, with the engine's reason): a row
    in neither is MISSING and named;
  * never in a leg that was not given it, never twice in one leg, never both a
    result and dropped, and no row that is not a candidate;
  * a dropped row must have been selected for that leg;
  * a replicated row (the heavy row's timing repetitions, one per leg) is
    folded by scripts/replicas.py: identical bit for bit except the volatile
    fields, minimum emp_time_ms; replicas that disagree on whether the engine
    refused the row fail the assemble.

Every object's OD run must have arrived from the one leg that owns it. Rows
keep candidate order within an object (the order the run emits them), objects
keep catalog order, all prop+eph rows come before all OD rows. A reference
with no OD rows at all is a dead axis, not a pass, and fails.

Heavy decoupling (S5). The light schedule names its `decoupled` rows (heavy
propagation rows); they come from one of two places:

  * --decoupled-plan results/heavy-plan.json (assemble, the plan step): the
    plan rows prep built for them with no engine call (`validate run
    --emit-plan-rows`, the real run's own row constructor), or the emit run's
    dropped file for a row with no input in the snapshot. The reference this
    writes is provisional: it exists to be stripped into the plan the channels
    and externals replay while the heavy legs still run.
  * --heavy-schedule results/reference-heavy-schedule.json (assemble-final):
    the heavy reference legs, whose replicas fold like any other.

    assemble-reference.py check-final --published results/validation_plan.json \
        --final results/validation_plan.final.json --heavy results/heavy.json \
        --dropped results/validation_rust_dropped.json \
        --out-heavy-dropped results/validation_rust_heavy_dropped.json
        the final assemble's equality check: the strip of the full reference
        must equal the published plan row for row and field for field, apart
        from `timestamp`. A published decoupled row the heavy legs found the
        engine refuses is written to the heavy-dropped file (reduce discards it
        from every channel and external, as the in-path plan would have); any
        other difference fails closed, naming the first differing row and field.
"""

from __future__ import annotations

import argparse
import json
import sys
from collections import defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import replicas  # the sibling modules, found beside this file
import schedule as sched

PROPEPH = "validation_rust_propeph.json"
DROPPED = PROPEPH + ".dropped.json"
OD = "validation_rust_od.json"
ORBITS = "validation_rust_od_orbits.jsonl"
COMPARE = "validation_rust_od_compare.jsonl"


def leg_dir(legs_dir: Path, leg: str) -> Path:
    """One leg's directory, whichever layout staged it: a local run writes
    <legs_dir>/<leg>/, CI downloads each artifact under a directory named after
    it (<legs_dir>/reference-leg-<leg>/)."""
    for cand in (legs_dir / leg, legs_dir / f"reference-leg-{leg}"):
        if (cand / PROPEPH).is_file():
            return cand
    return legs_dir / leg


def read_lines(f: Path) -> list[str]:
    return [ln for ln in f.read_text().splitlines() if ln.strip()] if f.is_file() else []


CANDIDATE_LEG = "emitted"


def assemble(cands: list, s: dict, legs_dir: Path, heavy: dict | None = None, emitted: tuple[list, list, set] | None = None) -> dict:
    """The five outputs as data, or SystemExit naming every defect found.

    `heavy` is the heavy reference schedule (its legs deliver the decoupled
    rows); `emitted` is (plan rows, dropped rows, leader keys) of the emit run,
    which stands in for them as one pseudo-leg (its leader rows are warm-up
    rows for the heavy channel legs, delivered here by the light legs, and are
    not taken from it). Exactly one of the two when the schedule decouples rows."""
    objects: list[str] = s["objects"]
    decoupled = set(s.get("decoupled", []))
    if decoupled and (heavy is None) == (emitted is None):
        raise SystemExit("ERROR: the schedule decouples rows: give the heavy reference schedule or the emitted plan rows (exactly one).")
    if heavy is not None:
        if sorted(heavy.get("decoupled", [])) != sorted(decoupled):
            raise SystemExit("ERROR: the heavy reference schedule runs a different row set than the light schedule decoupled.")
        s = dict(s, legs=s["legs"] + heavy["legs"], replicas=s.get("replicas", []) + heavy.get("replicas", []))
    order = {o: i for i, o in enumerate(objects)}
    idx: dict[str, int] = {}
    for i, r in enumerate(cands):
        k = sched.key_str(sched.row_key(r))
        if k in idx:
            raise SystemExit(f"ERROR: candidate rows {idx[k]} and {i} share the row key {k}.")
        idx[k] = i
    expected: dict[int, list[str]] = defaultdict(list)
    selected: dict[str, set[int]] = {}
    for leg in s["legs"]:
        selected[leg["leg"]] = set(leg["rows"])
        for i in leg["rows"]:
            if i in decoupled and heavy is None:
                raise SystemExit(f"ERROR: light leg {leg['leg']} holds decoupled row {i}.")
            expected[i].append(leg["leg"])
    if emitted is not None:
        selected[CANDIDATE_LEG] = set(decoupled)
        for i in sorted(decoupled):
            expected[i].append(CANDIDATE_LEG)
    problems: list[str] = []
    present: dict[int, list[tuple[str, dict]]] = defaultdict(list)
    dropped: dict[int, list[tuple[str, str]]] = defaultdict(list)
    od_rows: dict[str, list[dict]] = defaultdict(list)
    orbit_lines: dict[str, list[str]] = defaultdict(list)
    compare_lines: dict[str, list[str]] = defaultdict(list)
    od_owner = {o: leg["leg"] for leg in s["legs"] for o in leg["od_objects"]}
    arrived: list[str] = []
    if emitted is not None:
        arrived.append(CANDIDATE_LEG)
        seen_here: set[int] = set()
        for kind, rows in (("plan row", emitted[0]), ("dropped row", emitted[1])):
            for r in rows:
                k = sched.key_str(sched.row_key(r))
                i = idx.get(k)
                if k in emitted[2] and i is not None and i not in decoupled:
                    continue  # a leader: the light legs deliver it
                if i is None or i not in decoupled:
                    problems.append(f"the emit run delivered a {kind} that is not a decoupled candidate: {k}")
                elif i in seen_here:
                    problems.append(f"the emit run delivered {k} twice")
                else:
                    seen_here.add(i)
                    if kind == "plan row":
                        present[i].append((CANDIDATE_LEG, r))
                    else:
                        dropped[i].append((CANDIDATE_LEG, r.get("reason") or ""))
    for leg in s["legs"]:
        lid = leg["leg"]
        d = leg_dir(legs_dir, lid)
        files = {n: d / n for n in (PROPEPH, DROPPED, OD)}
        absent = [n for n, f in files.items() if not f.is_file()]
        if absent:
            problems.append(f"leg {lid}: did not arrive ({', '.join(absent)} absent from {d})")
            continue
        arrived.append(lid)
        seen_here: set[int] = set()
        for r in sched.load_json(files[PROPEPH]):
            k = sched.key_str(sched.row_key(r))
            i = idx.get(k)
            if i is None:
                problems.append(f"leg {lid}: a row that is not a candidate: {k}")
            elif i not in selected[lid]:
                problems.append(f"leg {lid}: delivered {k}, which the schedule gave to {expected[i]}")
            elif i in seen_here:
                problems.append(f"leg {lid}: delivered {k} twice")
            else:
                seen_here.add(i)
                present[i].append((lid, r))
        for r in sched.load_json(files[DROPPED]):
            k = sched.key_str(sched.row_key(r))
            i = idx.get(k)
            if i is None:
                problems.append(f"leg {lid}: a dropped row that is not a candidate: {k}")
            elif i not in selected[lid]:
                problems.append(f"leg {lid}: dropped {k}, which was not selected for it")
            elif i in seen_here:
                problems.append(f"leg {lid}: {k} is both a result and dropped (or dropped twice)")
            else:
                seen_here.add(i)
                dropped[i].append((lid, r.get("reason") or ""))
        for r in sched.load_json(files[OD]):
            o = r["object"]
            if od_owner.get(o) != lid:
                problems.append(f"leg {lid}: OD rows for {o!r}, which the schedule gave to leg {od_owner.get(o)}")
            else:
                od_rows[o].append(r)
        for src, dst in ((d / ORBITS, orbit_lines), (d / COMPARE, compare_lines)):
            for ln in read_lines(src):
                o = json.loads(ln).get("object")
                if od_owner.get(o) != lid:
                    problems.append(f"leg {lid}: a {src.name} line for {o!r}, which the schedule gave to leg {od_owner.get(o)}")
                else:
                    dst[o].append(ln)
    # Coverage: every candidate in each leg that was given it.
    missing: list[str] = []
    folded: list[str] = []
    out_rows: list[dict] = []
    out_dropped: list[dict] = []
    for i, r in enumerate(cands):
        k = sched.key_str(sched.row_key(r))
        legs_i = expected.get(i, [])
        if not legs_i:
            problems.append(f"candidate {k} is in no leg of the schedule")
            continue
        got = {lg for lg, _ in present[i]}
        gone = {lg for lg, _ in dropped[i]}
        lost = [lg for lg in legs_i if lg not in got and lg not in gone and lg in arrived]
        for lg in lost:
            missing.append(f"{k} (leg {lg})")
        if lost or any(lg not in arrived for lg in legs_i):
            continue
        if got and gone:
            problems.append(f"{k}: the engine refused it in leg(s) {sorted(gone)} but delivered it in leg(s) {sorted(got)}")
            continue
        if gone:
            out_dropped.append({**{f: r.get(f) for f in ("object", "test_type", "propagation_uncertainty", "dt_days", "observer", "force_model")}, "reason": dropped[i][0][1]})
            continue
        copies = sorted(present[i], key=lambda t: legs_i.index(t[0]))
        if len(copies) == 1:
            out_rows.append(copies[0][1])
            continue
        try:
            row = replicas.fold(k, copies)
        except replicas.FoldError as e:
            problems.append(str(e))
            continue
        out_rows.append(row)
        folded.append(
            f"folded {k}: {len(copies)} replicas (legs {','.join(lg for lg, _ in copies)}) identical; "
            f"emp_time_ms min {row.get('emp_time_ms')} of {[c.get('emp_time_ms') for _, c in copies]}"
        )
    if missing:
        problems.append(
            f"{len(missing)} candidate row(s) are neither a result nor named as dropped by the leg that ran them "
            f"(first: {missing[0]}). A leg that died or was cancelled leaves its rows ABSENT."
        )
    unarrived = [leg["leg"] for leg in s["legs"] if leg["leg"] not in arrived]
    if problems:
        print(f"ERROR: the reference cannot be assembled ({len(problems)} problem(s), {len(unarrived)} leg(s) absent):", file=sys.stderr)
        for p in problems[:60]:
            print(f"  - {p}", file=sys.stderr)
        if len(problems) > 60:
            print(f"  ... and {len(problems) - 60} more", file=sys.stderr)
        print("       Re-run the failed leg(s); do not assemble around them.", file=sys.stderr)
        raise SystemExit(1)
    od: list[dict] = []
    orbits: list[str] = []
    compare: list[str] = []
    for o in sorted(objects, key=order.__getitem__):
        od += od_rows.get(o, [])
        orbits += orbit_lines.get(o, [])
        compare += compare_lines.get(o, [])
    if not od:
        raise SystemExit(
            "ERROR: assembled reference carries zero OD rows across all "
            f"{len(objects)} objects. The OD axis produced nothing, which is a "
            "dead axis, not a passing one. Check the fixture fetch (make fixtures)."
        )
    return {
        "propeph": out_rows,
        "od": od,
        "orbits": orbits,
        "compare": compare,
        "dropped": out_dropped,
        "folded": folded,
        "legs": len(s["legs"]),
    }


def write_outputs(a: dict, out_propeph: Path, out_od: Path, out_orbits: Path, out_compare: Path, out_unified: Path, out_dropped: Path | None) -> None:
    for path, rows in ((out_propeph, a["propeph"]), (out_od, a["od"]), (out_unified, a["propeph"] + a["od"])):
        path.parent.mkdir(parents=True, exist_ok=True)
        with open(path, "w") as f:
            json.dump(rows, f, indent=2, default=str)
    for path, lines in ((out_orbits, a["orbits"]), (out_compare, a["compare"])):
        path.write_text("".join(ln + "\n" for ln in lines))
    if out_dropped:
        with open(out_dropped, "w") as f:
            json.dump(a["dropped"], f, indent=2)


def check_final(published: list, final: list, heavy: dict, rust_dropped: list) -> list[dict]:
    """The equality the final assemble asserts (see the module docstring).
    Returns the heavy-dropped rows; raises SystemExit naming the first
    difference otherwise."""
    dec = {h["key"] for h in heavy["rows"]}
    refused = {sched.key_str(sched.row_key(d)): d.get("reason") or "" for d in rust_dropped}
    fkeys = [sched.key_str(sched.row_key(r)) for r in final]
    if len(set(fkeys)) != len(fkeys):
        raise SystemExit("ERROR: the final reference strip repeats a row key.")
    fidx = {k: i for i, k in enumerate(fkeys)}
    pkeys = [sched.key_str(sched.row_key(r)) for r in published]
    heavy_dropped: list[dict] = []
    kept: list[str] = []
    for i, (k, row) in enumerate(zip(pkeys, published)):
        if k in fidx:
            kept.append(k)
            continue
        if k in dec and k in refused:
            heavy_dropped.append({**{f: row.get(f) for f in sched.IDENTITY}, "reason": refused[k]})
            continue
        why = (
            "it is decoupled, but the heavy legs neither delivered it nor named it refused"
            if k in dec
            else "it is not a decoupled row, so its absence means the light reference changed after the plan was published"
        )
        raise SystemExit(f"ERROR: published plan row {i} ({k}) is not in the final reference strip: {why}. Failing closed.")
    extra = [k for k in fkeys if k not in set(pkeys)]
    if extra:
        raise SystemExit(f"ERROR: the final reference strip holds {extra[0]}, which the published plan lacks. Failing closed.")
    if kept != fkeys:
        j = next(j for j, (a, b) in enumerate(zip(kept, fkeys)) if a != b)
        raise SystemExit(f"ERROR: the final reference strip orders its rows differently from the published plan (first at row {j}: {fkeys[j]} vs {kept[j]}). Failing closed.")
    pidx = {k: i for i, k in enumerate(pkeys)}
    for k in fkeys:
        p, f = published[pidx[k]], final[fidx[k]]
        for field in sorted(set(p) | set(f)):
            if field == "timestamp":
                continue
            if field not in p or field not in f or replicas._canon(p[field]) != replicas._canon(f[field]):
                raise SystemExit(
                    f"ERROR: published plan row {pidx[k]} ({k}) differs from the final reference strip in `{field}` "
                    f"({replicas._canon(p.get(field))[:120]} published vs {replicas._canon(f.get(field))[:120]} final). "
                    "The channels and externals replayed the published row; failing closed."
                )
    return heavy_dropped


def main_check_final(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(prog="assemble-reference.py check-final")
    ap.add_argument("--published", required=True, type=Path, help="the plan assemble published (validation_plan.json)")
    ap.add_argument("--final", required=True, type=Path, help="the strip of the full reference (validation_plan.final.json)")
    ap.add_argument("--heavy", required=True, type=Path, help="results/heavy.json")
    ap.add_argument("--dropped", required=True, type=Path, help="the full reference's dropped rows (validation_rust_dropped.json)")
    ap.add_argument("--out-heavy-dropped", required=True, type=Path)
    args = ap.parse_args(argv)
    heavy_dropped = check_final(
        sched.load_json(args.published), sched.load_json(args.final), sched.load_json(args.heavy), sched.load_json(args.dropped)
    )
    with open(args.out_heavy_dropped, "w") as f:
        json.dump(heavy_dropped, f, indent=2)
    n = len(sched.load_json(args.final))
    print(
        f"Final reference strip equals the published plan: {n} rows, every field but timestamp; "
        f"{len(heavy_dropped)} decoupled row(s) refused by the engine -> {args.out_heavy_dropped}"
    )
    for d in heavy_dropped:
        print(f"  refused: {sched.key_str(sched.row_key(d))}: {d['reason']}")
    return 0


def main(argv: list[str] | None = None) -> int:
    argv = sys.argv[1:] if argv is None else argv
    if argv and argv[0] == "check-final":
        return main_check_final(argv[1:])
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--schedule", required=True, type=Path)
    ap.add_argument("--candidates", required=True, type=Path)
    ap.add_argument("--legs-dir", required=True, type=Path)
    ap.add_argument("--heavy-schedule", type=Path, help="the heavy reference schedule; its legs deliver the decoupled rows")
    ap.add_argument("--decoupled-plan", type=Path, help="the emitted plan rows of the decoupled rows (with --heavy)")
    ap.add_argument("--heavy", type=Path, help="results/heavy.json, naming the leaders the emitted plan also holds")
    for n in ("propeph", "od", "orbits", "compare", "unified"):
        ap.add_argument(f"--out-{n}", required=True, type=Path)
    ap.add_argument("--out-dropped", type=Path)
    args = ap.parse_args(argv)
    s = sched.load_json(args.schedule)
    if s.get("schema") != sched.SCHEMA or s.get("kind") != "reference" or s.get("heavy"):
        raise SystemExit(f"ERROR: {args.schedule} is not a light reference schedule of schema {sched.SCHEMA}.")
    h = sched.file_sha256(args.candidates)
    if s["candidates_sha256"] != h:
        raise SystemExit(f"ERROR: {args.schedule} was built for a different candidate list; refusing to assemble.")
    heavy = emitted = None
    if args.heavy_schedule:
        heavy = sched.load_json(args.heavy_schedule)
        if not heavy.get("heavy") or heavy.get("candidates_sha256") != h:
            raise SystemExit(f"ERROR: {args.heavy_schedule} is not a heavy reference schedule for this candidate list.")
    if args.decoupled_plan:
        if not args.heavy:
            raise SystemExit("ERROR: --decoupled-plan needs --heavy.")
        hv = sched.load_json(args.heavy)
        if hv.get("candidates_sha256") != h:
            raise SystemExit(f"ERROR: {args.heavy} was built for a different candidate list.")
        leaders = {sched.key_str(sched.row_key(x)) for r in hv["rows"] for x in r["leaders"]}
        dropped_file = args.decoupled_plan.with_name(args.decoupled_plan.name + ".dropped.json")
        emitted = (sched.load_json(args.decoupled_plan), sched.load_json(dropped_file) if dropped_file.is_file() else [], leaders)
        if not dropped_file.is_file():
            raise SystemExit(f"ERROR: {dropped_file} is absent; the emit run writes it on every run.")
    a = assemble(sched.load_json(args.candidates), s, args.legs_dir, heavy, emitted)
    write_outputs(a, args.out_propeph, args.out_od, args.out_orbits, args.out_compare, args.out_unified, args.out_dropped)
    for line in a["folded"]:
        print(f"  {line}")
    mode = " (provisional: decoupled rows from the emitted plan rows)" if emitted else (" (with the heavy legs)" if heavy else "")
    print(
        f"Assembled {a['legs']} legs{mode} -> {len(a['propeph'])} prop+eph rows ({len(a['dropped'])} refused by the engine, "
        f"each named in its leg's dropped file), {len(a['od'])} OD rows, {len(a['orbits'])} orbit sidecar rows, "
        f"{len(a['compare'])} compare sidecar rows; unified {len(a['propeph']) + len(a['od'])} rows -> {args.out_unified}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
