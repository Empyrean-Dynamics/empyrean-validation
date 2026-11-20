#!/usr/bin/env python3
"""One reference leg's inputs and its completeness check.

The reference is computed in cost-balanced legs (scripts/schedule.py
pack-reference): a leg runs `validate run --select` over its candidate rows and
`validate od --only` over the objects whose OD run it owns. This script turns
the reference schedule into that leg's files, and checks what the leg wrote
before it uploads anything.

    reference-legs.py select --candidates results/reference-candidates.json \\
        --schedule results/reference-schedule.json --leg 03 \\
        --rows-out leg.rows.json --reps-out leg.reps.json --objects-out leg.objects
        writes the leg's candidate rows (the `--select` file; not written when
        the leg has no propagation or ephemeris rows), its repetition
        overrides (the `--reps-override` file, `[]` when none), and the
        objects of its OD run, comma-separated (not written when none). It
        refuses a schedule built for another candidate list, an unknown leg,
        and a leg with nothing to do.

    reference-legs.py check --dir results/shards/03 \\
        --rows leg.rows.json --objects "A,B"
        refuses a leg whose output does not account for its selection: every
        selected row must be a result row or named in the dropped file, no
        other row may appear, and a leg that owns OD objects must carry the OD
        file, its two sidecars, and OD rows for no other object. This is the
        leg-level half of the completeness check; assemble-reference.py does
        the whole-reference half.

A leg always writes validation_rust_propeph.json (with its .dropped.json) and
validation_rust_od.json, empty lists when it has nothing of that kind, so the
assemble step finds the same files in every leg.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import schedule as sched  # the sibling script, found beside this file

PROPEPH = "validation_rust_propeph.json"
DROPPED = PROPEPH + ".dropped.json"
OD = "validation_rust_od.json"
ORBITS = "validation_rust_od_orbits.jsonl"
COMPARE = "validation_rust_od_compare.jsonl"


def load_schedule(path: Path, cands_path: Path) -> dict:
    s = sched.load_json(path)
    if s.get("schema") != sched.SCHEMA or s.get("kind") != "reference":
        raise SystemExit(f"ERROR: {path} is not a reference schedule of schema {sched.SCHEMA}.")
    h = sched.file_sha256(cands_path)
    if s["candidates_sha256"] != h:
        raise SystemExit(
            f"ERROR: {path} was built for a different candidate list (its candidates_sha256 is "
            f"{s['candidates_sha256'][:12]}, {cands_path.name} hashes to {h[:12]}). "
            "Refusing to deal rows from a stale schedule."
        )
    return s


def select(cands: list, s: dict, leg: str) -> tuple[list, list, list[str]]:
    legs = {x["leg"]: x for x in s["legs"]}
    if leg not in legs:
        raise SystemExit(f"ERROR: the reference schedule has no leg {leg!r}; its legs are {', '.join(sorted(legs))}.")
    x = legs[leg]
    rows = x["rows"]
    if not rows and not x["od_objects"]:
        raise SystemExit(f"ERROR: reference leg {leg} has nothing to run.")
    if rows != sorted(set(rows)) or (rows and (rows[0] < 0 or rows[-1] >= len(cands))):
        raise SystemExit(f"ERROR: reference leg {leg} lists rows that are unsorted, repeated or outside the candidates.")
    reps = []
    for i_str, n in sorted(x.get("reps", {}).items(), key=lambda t: int(t[0])):
        r = cands[int(i_str)]
        reps.append({**{k: r[k] for k in ("object", "test_type", "propagation_uncertainty", "dt_days", "observer", "force_model")}, "reps": n})
    return [cands[i] for i in rows], reps, list(x["od_objects"])


def check_leg(d: Path, rows: list, objects: list[str]) -> list[str]:
    """Every way a leg's output can fail to account for its selection."""
    problems: list[str] = []

    def load(name: str):
        f = d / name
        if not f.is_file():
            problems.append(f"{name}: missing")
            return None
        try:
            return sched.load_json(f)
        except Exception as exc:  # noqa: BLE001
            problems.append(f"{name}: unparseable ({exc})")
            return None

    out = load(PROPEPH)
    dropped = load(DROPPED)
    od = load(OD)
    if out is not None and dropped is not None:
        want = {sched.key_str(sched.row_key(r)) for r in rows}
        got = [sched.key_str(sched.row_key(r)) for r in out]
        gone = [sched.key_str(sched.row_key(r)) for r in dropped]
        for k in sorted(set(got) & set(gone)):
            problems.append(f"{k}: both a result row and a dropped row")
        for k in sorted({k for k in got if got.count(k) > 1}):
            problems.append(f"{k}: delivered twice")
        for k in sorted(set(got) - want):
            problems.append(f"{k}: a result row the leg was not given")
        for k in sorted(set(gone) - want):
            problems.append(f"{k}: a dropped row the leg was not given")
        lost = sorted(want - set(got) - set(gone))
        if lost:
            problems.append(f"{len(lost)} selected row(s) are neither a result nor dropped (first: {lost[0]})")
        for r in dropped:
            if not r.get("reason"):
                problems.append(f"{sched.key_str(sched.row_key(r))}: a dropped row with no reason")
    if od is not None:
        for o in sorted({r["object"] for r in od} - set(objects)):
            problems.append(f"{OD}: OD rows for {o!r}, which this leg does not own")
        if not objects and od:
            problems.append(f"{OD}: OD rows, but the leg owns no OD object")
        if od:
            for side in (ORBITS, COMPARE):
                if not (d / side).is_file():
                    problems.append(f"{side}: missing, but the OD pass emitted rows")
    return problems


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("select")
    s.add_argument("--candidates", required=True, type=Path)
    s.add_argument("--schedule", required=True, type=Path)
    s.add_argument("--leg", required=True)
    s.add_argument("--rows-out", required=True, type=Path)
    s.add_argument("--reps-out", required=True, type=Path)
    s.add_argument("--objects-out", required=True, type=Path)
    c = sub.add_parser("check")
    c.add_argument("--dir", required=True, type=Path)
    c.add_argument("--rows", type=Path, help="the leg's --select file; omit for a leg with no rows")
    c.add_argument("--objects", default="", help="the leg's OD objects, comma-separated")
    args = ap.parse_args(argv)
    if args.cmd == "select":
        cands = sched.load_json(args.candidates)
        s_ = load_schedule(args.schedule, args.candidates)
        rows, reps, objects = select(cands, s_, args.leg)
        for p in (args.rows_out, args.objects_out):
            p.unlink(missing_ok=True)
        if rows:
            sched.write_json(rows, args.rows_out)
        sched.write_json(reps, args.reps_out)
        if objects:
            args.objects_out.parent.mkdir(parents=True, exist_ok=True)
            args.objects_out.write_text(",".join(objects))
        predicted = next(x["predicted_min"] for x in s_["legs"] if x["leg"] == args.leg)
        print(
            f"Reference leg {args.leg} of {len(s_['legs'])}: {len(rows)} candidate rows, "
            f"{len(reps)} repetition override(s), OD of {len(objects)} object(s) "
            f"({', '.join(objects) or 'none'}); predicted {predicted:.1f} min"
        )
        return 0
    rows = sched.load_json(args.rows) if args.rows else []
    objects = [o for o in args.objects.split(",") if o]
    problems = check_leg(args.dir, rows, objects)
    if problems:
        print(f"ERROR: reference leg in {args.dir} is not usable:", file=sys.stderr)
        for p in problems[:40]:
            print(f"  - {p}", file=sys.stderr)
        raise SystemExit(1)
    od = sched.load_json(args.dir / OD)
    dropped = sched.load_json(args.dir / DROPPED)
    print(f"  leg OK: {len(sched.load_json(args.dir / PROPEPH))} prop+eph rows, {len(dropped)} dropped, {len(od)} OD rows")
    return 0


if __name__ == "__main__":
    sys.exit(main())
