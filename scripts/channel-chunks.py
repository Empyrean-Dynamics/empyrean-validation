#!/usr/bin/env python3
"""Deal the plan to the channel legs row by row, and put their outputs back.

The per-method plan replays every uncertainty method on every channel, so one
channel over the full catalog outruns any job budget, and an object-level leg
cannot hold the one row that costs 42 minutes per run (run 42506609035: the c
and cli legs holding 2020 CD3 hit the 240-minute limit). Legs therefore take
ROWS: scripts/schedule.py packs the plan's scheduling units longest-first into
legs from the committed cost table and writes results/schedule.json; each
channel leg runs `select` for its rows, and reduce runs `concat` to put the
leg outputs back, in plan order, into the one file per channel the merge reads.

    channel-chunks.py select --plan results/validation_plan.json \
        --schedule results/schedule.json --channel python --leg 03 \
        --output results/validation_plan.leg-python.json
        writes the leg's plan rows in plan order. It refuses a schedule built
        for a different plan (hash mismatch), an unknown channel or leg, and a
        leg that selects no rows: a leg handed the wrong rows replays the
        wrong work (2026-10-28: every chunk leg replayed all 30,033 plan rows
        and 38 of 40 hit the 240-minute wall).
        With --reps-output it also writes the leg's per-row timing repetition
        overrides (a list of row identities with `reps`; `[]` when the leg has
        none): the replica legs of a heavy row run it once each. The drivers
        read it as a side file, never as a plan key, because a plan key would
        flow into the outputs through the row copy.

    channel-chunks.py partition --input leg.json --reps leg.reps.json \
        --override-out a.json --rest-out b.json
        splits a leg's rows into those with a repetition override and the rest
        (the core channel runs the first with `--n-timing-runs 1` in its own
        process, so no change to validate-core is needed).

    channel-chunks.py concat --plan results/validation_plan.json \
        --schedule results/schedule.json --channel python \
        --out results/validation_python.json results/validation_python.leg*.json
        maps every output row to its plan row by row key, checks the row
        belongs to the leg that produced it, and writes the rows in plan
        order (what an unchunked run produces). It refuses a missing or
        unexpected leg, a duplicate leg id, a non-list file, a row whose key is
        not in the plan, a row outside its leg's selection, and a row delivered
        twice: a silently short channel would read downstream as "compared and
        agreed". Rows a channel does not produce (skipped transports) are not
        an error; the channel's own count line accounts for them.
        A replicated row (the schedule's `replicas`) is delivered once by each
        of its legs, and concat folds the copies with scripts/replicas.py: the
        copies must be identical bit for bit except the volatile fields, or it
        fails naming the row, the field and the legs; the merged row keeps the
        minimum emp_time_ms. A replica leg that did not deliver the row, or a
        row delivered by a leg that is not one of its replicas, is refused.

    channel-chunks.py split  --objects results/objects.json --count 10
        prints a JSON list of {"id": "01", "objects": "A,B,C"}, objects dealt
        round-robin in matrix order. The channel legs no longer use it; it is
        kept for the external legs' object chunks.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import replicas  # the sibling modules, found beside this file
import schedule as sched

LEG_RE = re.compile(r"\.leg([0-9A-Za-z]+)\.json$")


def split(objects: list, count: int) -> list[dict]:
    if not objects:
        raise SystemExit("ERROR: no objects to chunk (the matrix is empty).")
    if count < 1:
        raise SystemExit(f"ERROR: --count must be at least 1 (got {count}).")
    count = min(count, len(objects))
    width = max(2, len(str(count)))
    buckets: list[list[str]] = [[] for _ in range(count)]
    for i, obj in enumerate(objects):
        name = obj["name"] if isinstance(obj, dict) else str(obj)
        if "," in name:
            raise SystemExit(f"ERROR: object name {name!r} contains a comma; OBJECTS is comma-separated.")
        buckets[i % count].append(name)
    return [{"id": f"{k + 1:0{width}d}", "objects": ",".join(b)} for k, b in enumerate(buckets)]


def write_rows(rows: list, out: Path) -> None:
    """Write through a sibling temp file, so a failed write leaves no partial file."""
    out.parent.mkdir(parents=True, exist_ok=True)
    tmp = out.with_name(out.name + ".tmp")
    try:
        with open(tmp, "w") as f:
            json.dump(rows, f, allow_nan=False)
    except BaseException:
        tmp.unlink(missing_ok=True)
        raise
    tmp.replace(out)


def load_schedule(schedule_path: Path, plan_path: Path, channel: str) -> tuple[dict, dict]:
    """The schedule and the named channel's body, refusing a schedule that was
    not built for this plan."""
    with open(schedule_path) as f:
        schedule = json.load(f)
    if schedule.get("schema") != sched.SCHEMA:
        raise SystemExit(f"ERROR: {schedule_path} has schema {schedule.get('schema')!r}, expected {sched.SCHEMA}.")
    plan_sha = sched.file_sha256(plan_path)
    if schedule["plan_sha256"] != plan_sha:
        raise SystemExit(
            f"ERROR: {schedule_path} was built for a different plan (its plan_sha256 is "
            f"{schedule['plan_sha256'][:12]}, {plan_path.name} hashes to {plan_sha[:12]}). "
            "Refusing to deal rows from a stale schedule."
        )
    body = schedule["channels"].get(channel)
    if body is None:
        raise SystemExit(f"ERROR: the schedule has no channel {channel!r}; it has {', '.join(schedule['channels'])}.")
    return schedule, body


def select(plan: list, body: dict, channel: str, leg: str) -> list:
    """The leg's plan rows, in plan order."""
    legs = {x["leg"]: x for x in body["legs"]}
    if leg not in legs:
        raise SystemExit(f"ERROR: {channel} has no leg {leg!r}; its legs are {', '.join(sorted(legs))}.")
    rows = legs[leg]["rows"]
    if not rows:
        raise SystemExit(f"ERROR: {channel} leg {leg} selects no rows.")
    if rows != sorted(rows) or len(set(rows)) != len(rows) or rows[0] < 0 or rows[-1] >= len(plan):
        raise SystemExit(f"ERROR: {channel} leg {leg} lists rows that are unsorted, repeated or outside the plan.")
    return [plan[i] for i in rows]


def plan_index(plan: list) -> dict[str, int]:
    """Row key -> plan index, refusing a plan whose keys are not unique."""
    idx: dict[str, int] = {}
    for i, r in enumerate(plan):
        k = sched.key_str(sched.row_key(r))
        if k in idx:
            raise SystemExit(f"ERROR: plan rows {idx[k]} and {i} share the row key {k}.")
        idx[k] = i
    return idx


def reps_entries(plan: list, body: dict, leg: str) -> list[dict]:
    """The leg's repetition overrides as identity + `reps` entries."""
    legs = {x["leg"]: x for x in body["legs"]}
    out = []
    for i_str, n in sorted(legs[leg].get("reps", {}).items(), key=lambda t: int(t[0])):
        r = plan[int(i_str)]
        out.append(
            {
                "object": r["object"],
                "test_type": r["test_type"],
                "propagation_uncertainty": r.get("propagation_uncertainty"),
                "dt_days": r["dt_days"],
                "observer": r.get("observer"),
                "force_model": r.get("force_model"),
                "reps": n,
            }
        )
    return out


def partition(rows: list, reps: list[dict]) -> tuple[list, list]:
    """(rows with an override, the rest), each in input order."""
    want = {sched.key_str(sched.row_key(e)) for e in reps}
    hit = [r for r in rows if sched.key_str(sched.row_key(r)) in want]
    rest = [r for r in rows if sched.key_str(sched.row_key(r)) not in want]
    return hit, rest


def concat(plan: list, body: dict, channel: str, paths: list[Path], out: Path, replica_defs: list[dict] | None = None) -> tuple[int, list[str]]:
    """Join the leg outputs in plan order, refusing every shape of wrong, and
    fold the timing replicas. Returns (rows written, fold report lines)."""
    expected = {x["leg"]: x for x in body["legs"]}
    got: dict[str, Path] = {}
    for p in paths:
        m = LEG_RE.search(p.name)
        if not m:
            raise SystemExit(f"ERROR: {p} is not a leg file (expected *.leg<ID>.json).")
        if m.group(1) in got:
            raise SystemExit(f"ERROR: duplicate leg id {m.group(1)} for {out.name}: {got[m.group(1)].name} and {p.name}.")
        got[m.group(1)] = p
    missing = sorted(set(expected) - set(got))
    if missing:
        raise SystemExit(
            f"ERROR: {channel} is missing leg(s) {', '.join(missing)} of {len(expected)} for {out.name}. "
            "A channel leg did not deliver; refusing to assemble a short channel."
        )
    extra = sorted(set(got) - set(expected))
    if extra:
        raise SystemExit(f"ERROR: {channel} has leg file(s) {', '.join(extra)} that the schedule does not hold.")
    idx = plan_index(plan)
    rep_legs = {d["row"]: sorted(d["legs"]) for d in (replica_defs or []) if d["channel"] == channel}
    seen: dict[int, str] = {}
    copies: dict[int, list[tuple[str, dict]]] = {}
    keep: list[tuple[int, dict]] = []
    for leg_id in sorted(got):
        with open(got[leg_id]) as f:
            part = json.load(f)
        if not isinstance(part, list):
            raise SystemExit(f"ERROR: {got[leg_id]} is not a JSON list of rows.")
        allowed = set(expected[leg_id]["rows"])
        for r in part:
            k = sched.key_str(sched.row_key(r))
            i = idx.get(k)
            if i is None:
                raise SystemExit(f"ERROR: {got[leg_id].name} holds a row that is not in the plan: {k}.")
            if i not in allowed:
                raise SystemExit(f"ERROR: {got[leg_id].name} holds plan row {i} ({k}), which the schedule gave to another leg.")
            if i in rep_legs:
                if leg_id in [lg for lg, _ in copies.get(i, [])]:
                    raise SystemExit(f"ERROR: plan row {i} ({k}) is delivered twice by leg {leg_id}.")
                copies.setdefault(i, []).append((leg_id, r))
                continue
            if i in seen:
                raise SystemExit(f"ERROR: plan row {i} ({k}) is delivered twice (legs {seen[i]} and {leg_id}).")
            seen[i] = leg_id
            keep.append((i, r))
    report: list[str] = []
    for i, legs_i in sorted(rep_legs.items()):
        k = sched.key_str(sched.row_key(plan[i]))
        got_legs = [lg for lg, _ in copies.get(i, [])]
        if got_legs and got_legs != legs_i:
            raise SystemExit(
                f"ERROR: replicated row {i} ({k}) was delivered by legs {got_legs}, expected {legs_i}: "
                "a replica leg did not deliver it."
            )
        if not got_legs:
            continue  # a replicated row the channel produced no row for (declined everywhere)
        try:
            folded = replicas.fold(k, copies[i])
        except replicas.FoldError as e:
            raise SystemExit(f"ERROR: {e}")
        keep.append((i, folded))
        times = [r.get("emp_time_ms") for _, r in copies[i]]
        report.append(
            f"folded {k}: {len(got_legs)} replicas (legs {','.join(got_legs)}) identical; "
            f"emp_time_ms min {folded.get('emp_time_ms')} of {times}"
        )
    keep.sort(key=lambda t: t[0])
    rows = [r for _, r in keep]
    write_rows(rows, out)
    return len(rows), report


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("split")
    s.add_argument("--objects", required=True, type=Path, help="results/objects.json from `make list-objects`")
    s.add_argument("--count", required=True, type=int)
    jn = sub.add_parser("join")
    jn.add_argument("--out", required=True, type=Path)
    jn.add_argument("files", nargs="+", type=Path)
    pt = sub.add_parser("partition")
    pt.add_argument("--input", required=True, type=Path)
    pt.add_argument("--reps", required=True, type=Path)
    pt.add_argument("--override-out", required=True, type=Path)
    pt.add_argument("--rest-out", required=True, type=Path)
    for name in ("select", "concat"):
        c = sub.add_parser(name)
        c.add_argument("--plan", required=True, type=Path, help="the full plan, results/validation_plan.json")
        c.add_argument("--schedule", required=True, type=Path, help="results/schedule.json from scripts/schedule.py")
        c.add_argument("--channel", required=True)
        if name == "select":
            c.add_argument("--leg", required=True)
            c.add_argument("--output", required=True, type=Path)
            c.add_argument("--reps-output", type=Path, help="write the leg's timing repetition overrides here")
        else:
            c.add_argument("--out", required=True, type=Path)
            c.add_argument("legs", nargs="*", type=Path)
    args = ap.parse_args(argv)
    if args.cmd == "split":
        with open(args.objects) as f:
            objects = json.load(f)
        print(json.dumps(split(objects, args.count), separators=(",", ":")))
        return 0
    if args.cmd == "join":
        rows: list = []
        for f in args.files:
            with open(f) as fh:
                part = json.load(fh)
            if not isinstance(part, list):
                raise SystemExit(f"ERROR: {f} is not a JSON list of rows.")
            rows += part
        write_rows(rows, args.out)
        print(f"join: {len(rows)} rows from {len(args.files)} files -> {args.out}")
        return 0
    if args.cmd == "partition":
        with open(args.input) as f:
            rows = json.load(f)
        with open(args.reps) as f:
            reps = json.load(f)
        hit, rest = partition(rows, reps)
        write_rows(hit, args.override_out)
        write_rows(rest, args.rest_out)
        print(f"partition: {len(hit)} rows with a repetition override, {len(rest)} without")
        return 0
    schedule, body = load_schedule(args.schedule, args.plan, args.channel)
    with open(args.plan) as fh:
        plan = json.load(fh)
    if not isinstance(plan, list):
        raise SystemExit("ERROR: the plan is not a JSON list of rows.")
    if args.cmd == "select":
        rows = select(plan, body, args.channel, args.leg)
        write_rows(rows, args.output)
        predicted = next(x["predicted_min"] for x in body["legs"] if x["leg"] == args.leg)
        extra = ""
        if args.reps_output:
            entries = reps_entries(plan, body, args.leg)
            write_rows(entries, args.reps_output)
            extra = f"; {len(entries)} repetition override(s) -> {args.reps_output}"
        print(
            f"Plan leg: {len(rows)} rows for {args.channel} leg {args.leg} of {len(body['legs'])} "
            f"(of {len(plan)} rows in {args.plan.name}; predicted {predicted:.1f} min) -> {args.output}{extra}"
        )
        return 0
    n, report = concat(plan, body, args.channel, list(args.legs), args.out, schedule.get("replicas", []))
    print(f"{args.out}: {n} rows from {len(body['legs'])} {args.channel} legs, in plan order")
    for line in report:
        print(f"  {line}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
