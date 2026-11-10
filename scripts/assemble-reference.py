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


def assemble(cands: list, s: dict, legs_dir: Path) -> dict:
    """The five outputs as data, or SystemExit naming every defect found."""
    objects: list[str] = s["objects"]
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
            expected[i].append(leg["leg"])
    problems: list[str] = []
    present: dict[int, list[tuple[str, dict]]] = defaultdict(list)
    dropped: dict[int, list[tuple[str, str]]] = defaultdict(list)
    od_rows: dict[str, list[dict]] = defaultdict(list)
    orbit_lines: dict[str, list[str]] = defaultdict(list)
    compare_lines: dict[str, list[str]] = defaultdict(list)
    od_owner = {o: leg["leg"] for leg in s["legs"] for o in leg["od_objects"]}
    arrived: list[str] = []
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


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--schedule", required=True, type=Path)
    ap.add_argument("--candidates", required=True, type=Path)
    ap.add_argument("--legs-dir", required=True, type=Path)
    for n in ("propeph", "od", "orbits", "compare", "unified"):
        ap.add_argument(f"--out-{n}", required=True, type=Path)
    ap.add_argument("--out-dropped", type=Path)
    args = ap.parse_args(argv)
    s = sched.load_json(args.schedule)
    if s.get("schema") != sched.SCHEMA or s.get("kind") != "reference":
        raise SystemExit(f"ERROR: {args.schedule} is not a reference schedule of schema {sched.SCHEMA}.")
    h = sched.file_sha256(args.candidates)
    if s["candidates_sha256"] != h:
        raise SystemExit(f"ERROR: {args.schedule} was built for a different candidate list; refusing to assemble.")
    a = assemble(sched.load_json(args.candidates), s, args.legs_dir)
    write_outputs(a, args.out_propeph, args.out_od, args.out_orbits, args.out_compare, args.out_unified, args.out_dropped)
    for line in a["folded"]:
        print(f"  {line}")
    print(
        f"Assembled {a['legs']} legs -> {len(a['propeph'])} prop+eph rows ({len(a['dropped'])} refused by the engine, "
        f"each named in its leg's dropped file), {len(a['od'])} OD rows, {len(a['orbits'])} orbit sidecar rows, "
        f"{len(a['compare'])} compare sidecar rows; unified {len(a['propeph']) + len(a['od'])} rows -> {args.out_unified}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
