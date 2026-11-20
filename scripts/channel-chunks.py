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

        With --heavy-plan / --heavy-schedule / --heavy-dir (S5) it also takes
        the heavy legs (results/validation_<channel>_heavy.leg<ID>.json, from
        empyrean-heavy): they replay the heavy plan, their replicas are folded
        the same way, and each decoupled row lands at its own place in plan
        order; a c or cli heavy leg's warm-up leader row is not kept (the light
        legs deliver it). With --dropped it discards the decoupled rows the
        final assemble found the engine refused (validation_rust_heavy_dropped.json),
        which is what the in-path plan would have done; a dropped row that is
        not decoupled is a hard error.

    channel-chunks.py external-objects --plan P --schedule results/external-schedule.json \
        --tool jorbit --leg 03
        prints the leg's objects, comma-separated (nothing for a tool that runs
        as one leg over every object). Refuses a schedule built for another
        plan and an unknown tool or leg.

    channel-chunks.py concat-external --plan P --schedule results/external-schedule.json \
        --tool jorbit [--dropped D] --results-dir results
        puts an external tool's leg files (validation_<stem>.leg<ID>.json, for
        each comparator file the tool writes) back into validation_<stem>.json.
        One leg is copied in its own row order; several legs are merged into
        the order one unchunked run writes: rows that are plan rows in plan
        order, then the tool's own rows (jorbit's per-object summary rows) by
        the object's first plan row, leg by leg. Comparator isolation holds: a
        tool whose legs are incomplete, malformed, hold a row of an object the
        leg was not given, deliver a plan row twice, or (OrbFit) fail on every
        object gets no merged file and a validation_<stem>.json.absent file
        naming the reason, which reduce prints on the report; the command still
        exits 0 so the other comparators and the report go on.

    channel-chunks.py split  --objects results/objects.json --count 10
        prints a JSON list of {"id": "01", "objects": "A,B,C"}, objects dealt
        round-robin in matrix order. Superseded by the cost-balanced external
        schedule (schedule.py pack-external); kept for local use.
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


def load_heavy(plan_path: Path, schedule_path: Path, channel: str) -> tuple[list, dict, dict]:
    """The heavy plan, the heavy schedule and its channel body."""
    with open(schedule_path) as f:
        hs = json.load(f)
    if hs.get("kind") != "heavy":
        raise SystemExit(f"ERROR: {schedule_path} is not a heavy channel schedule.")
    hs_full, body = load_schedule(schedule_path, plan_path, channel)
    with open(plan_path) as f:
        hplan = json.load(f)
    return hplan, hs_full, body


def heavy_rows(plan: list, schedule: dict, hplan: list, hsched: dict, hbody: dict, channel: str, hpaths: list[Path], tmp: Path) -> tuple[dict[int, dict], list[str]]:
    """The decoupled rows the heavy legs delivered, as {main plan index: row},
    and the report lines. The heavy legs replay the heavy plan, so they are
    concatenated (and their replicas folded) against it first; only the
    decoupled rows are kept, never a warm-up leader."""
    n, report = concat(hplan, hbody, channel, hpaths, tmp, hsched.get("replicas", []))
    with open(tmp) as f:
        got = json.load(f)
    tmp.unlink()
    want = {sched.key_str(sched.row_key(plan[i])): i for i in schedule.get("decoupled", [])}
    hkeys = {sched.key_str(sched.row_key(hplan[i])) for i in hsched.get("decoupled", [])}
    lkeys = {sched.key_str(sched.row_key(hplan[i])) for i in hsched.get("leaders", [])}
    missing_from_heavy = sorted(set(want) - hkeys)
    if missing_from_heavy:
        raise SystemExit(f"ERROR: decoupled row(s) {missing_from_heavy[:3]} of the plan are not decoupled in the heavy schedule.")
    out: dict[int, dict] = {}
    leaders = 0
    for r in got:
        k = sched.key_str(sched.row_key(r))
        if k in want:
            out[want[k]] = r
        elif k in lkeys:
            leaders += 1
        else:
            raise SystemExit(f"ERROR: a heavy leg delivered {k}, which is neither a decoupled row of the plan nor a leader.")
    report.append(f"heavy legs: {len(out)} decoupled row(s) kept, {leaders} warm-up leader row(s) not kept (the light legs deliver them)")
    return out, report


def dropped_keys(plan: list, schedule: dict, dropped_path: Path | None) -> set[str]:
    """Keys of the decoupled rows the final assemble found refused. Every one
    must be a decoupled row of this plan."""
    if dropped_path is None:
        return set()
    with open(dropped_path) as f:
        dropped = json.load(f)
    dec = {sched.key_str(sched.row_key(plan[i])) for i in schedule.get("decoupled", [])}
    keys = set()
    for d in dropped:
        k = sched.key_str(sched.row_key(d))
        if k not in dec:
            raise SystemExit(
                f"ERROR: {dropped_path} names {k}, which is not a decoupled row of this plan. Only a decoupled "
                "(heavy) row can be refused after the plan is published; anything else is a broken assemble."
            )
        keys.add(k)
    return keys


def external_body(sched_ext: dict, tool: str) -> dict:
    body = sched_ext["tools"].get(tool)
    if body is None:
        raise SystemExit(f"ERROR: the external schedule has no tool {tool!r}; it has {', '.join(sched_ext['tools'])}.")
    return body


def load_external(schedule_path: Path, plan_path: Path) -> dict:
    with open(schedule_path) as f:
        s = json.load(f)
    if s.get("kind") != "external" or s.get("schema") != sched.EXTERNAL_SCHEMA:
        raise SystemExit(f"ERROR: {schedule_path} is not an external schedule of schema {sched.EXTERNAL_SCHEMA}.")
    if s["plan_sha256"] != sched.file_sha256(plan_path):
        raise SystemExit(f"ERROR: {schedule_path} was built for a different plan; refusing to deal objects from it.")
    return s


def external_objects(sched_ext: dict, tool: str, leg: str) -> str:
    body = external_body(sched_ext, tool)
    legs = {x["leg"]: x for x in body["legs"]}
    if leg not in legs:
        raise SystemExit(f"ERROR: {tool} has no leg {leg!r}; its legs are {', '.join(sorted(legs))}.")
    objs = legs[leg]["objects"]
    return "" if objs is None else ",".join(objs)


class ToolFailure(Exception):
    """A defect confined to one external tool: its comparator is skipped and named."""


def row_key_or_none(r: dict):
    try:
        return sched.key_str(sched.row_key(r))
    except (KeyError, TypeError, ValueError):
        return None


def merge_external_stem(plan: list, body: dict, stem: str, results: Path, drop: set[str]) -> tuple[Path, int, int] | None:
    """Merge one comparator file's legs. Returns (path, rows, rows dropped), or
    None when no leg delivered the file (the comparator is absent, as before
    the chunking). Raises ToolFailure on anything else wrong."""
    expected = {x["leg"]: x for x in body["legs"]}
    got: dict[str, Path] = {}
    for p in sorted(results.glob(f"validation_{stem}.leg*.json")):
        m = LEG_RE.search(p.name)
        if not m or p.name != f"validation_{stem}.leg{m.group(1)}.json":
            continue
        got[m.group(1)] = p
    if not got:
        return None
    extra = sorted(set(got) - set(expected))
    if extra:
        raise ToolFailure(f"leg file(s) {', '.join(extra)} are not legs of the schedule")
    missing = sorted(set(expected) - set(got))
    if missing:
        raise ToolFailure(f"leg(s) {', '.join(missing)} of {len(expected)} did not deliver validation_{stem}; a partial comparator is not merged")
    idx = plan_index(plan)
    first_of: dict[str, int] = {}
    for i, r in enumerate(plan):
        first_of.setdefault(r["object"], i)
    parts: list[tuple[str, list]] = []
    for lid in sorted(got):
        try:
            with open(got[lid]) as f:
                part = json.load(f)
        except (OSError, ValueError) as e:
            raise ToolFailure(f"{got[lid].name} is not readable JSON ({e})")
        if not isinstance(part, list) or not all(isinstance(r, dict) and isinstance(r.get("object"), str) for r in part):
            raise ToolFailure(f"{got[lid].name} is not a JSON list of rows that each name their object")
        allowed = expected[lid]["objects"]
        if allowed is not None:
            bad = sorted({r["object"] for r in part} - set(allowed))
            if bad:
                raise ToolFailure(f"{got[lid].name} holds rows for {bad[:3]}, which leg {lid} was not given")
        parts.append((lid, part))
    if len(parts) == 1:
        rows = list(parts[0][1])
    else:
        keyed: dict[int, tuple[str, dict]] = {}
        tail: list[tuple[int, int, int, dict]] = []
        for li, (lid, part) in enumerate(parts):
            for pos, r in enumerate(part):
                k = row_key_or_none(r)
                i = idx.get(k) if k is not None else None
                if i is None:
                    tail.append((first_of.get(r["object"], len(plan)), li, pos, r))
                    continue
                if i in keyed:
                    raise ToolFailure(f"plan row {k} is delivered by legs {keyed[i][0]} and {lid}")
                keyed[i] = (lid, r)
        rows = [keyed[i][1] for i in sorted(keyed)] + [t[3] for t in sorted(tail, key=lambda t: t[:3])]
    kept = [r for r in rows if row_key_or_none(r) not in drop]
    field = body.get("all_failed_field")
    if field and kept and all(r.get(field) for r in kept):
        raise ToolFailure(f"every one of its {len(kept)} rows carries {field}: the tool ran and fitted nothing")
    out = results / f"validation_{stem}.json"
    write_rows(kept, out)
    return out, len(kept), len(rows) - len(kept)


def concat_external(plan: list, sched_ext: dict, tool: str, results: Path, drop: set[str]) -> list[str]:
    """Merge every comparator file of one tool; report lines. A tool-level
    defect writes validation_<stem>.json.absent with the reason instead."""
    body = external_body(sched_ext, tool)
    lines = []
    for stem in body["stems"]:
        absent = results / f"validation_{stem}.json.absent"
        absent.unlink(missing_ok=True)
        try:
            res = merge_external_stem(plan, body, stem, results, drop)
        except ToolFailure as e:
            (results / f"validation_{stem}.json").unlink(missing_ok=True)
            absent.write_text(f"{e}\n")
            lines.append(f"{stem}: NOT MERGED, {e}")
            continue
        if res is None:
            lines.append(f"{stem}: no leg delivered it (the comparator is absent)")
            continue
        out, n, nd = res
        lines.append(f"{stem}: {n} rows from {len(body['legs'])} leg(s) -> {out}" + (f" ({nd} refused decoupled row(s) discarded)" if nd else ""))
    return lines


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
            c.add_argument("--heavy-plan", type=Path, help="the heavy plan the empyrean-heavy legs replayed")
            c.add_argument("--heavy-schedule", type=Path, help="the heavy channel schedule (schedule.py pack-heavy)")
            c.add_argument("--heavy-dir", type=Path, help="where validation_<channel>_heavy.leg<ID>.json are staged")
            c.add_argument("--dropped", type=Path, help="decoupled rows the final assemble found refused; discarded")
            c.add_argument("legs", nargs="*", type=Path)
    eo = sub.add_parser("external-objects")
    eo.add_argument("--plan", required=True, type=Path)
    eo.add_argument("--schedule", required=True, type=Path, help="results/external-schedule.json")
    eo.add_argument("--tool", required=True)
    eo.add_argument("--leg", required=True)
    ce = sub.add_parser("concat-external")
    ce.add_argument("--plan", required=True, type=Path)
    ce.add_argument("--schedule", required=True, type=Path, help="results/external-schedule.json")
    ce.add_argument("--tool", required=True)
    ce.add_argument("--results-dir", required=True, type=Path)
    ce.add_argument("--dropped", type=Path, help="decoupled rows the final assemble found refused; discarded")
    ce.add_argument("--channel-schedule", type=Path, help="results/schedule.json, whose decoupled rows --dropped must name")
    args = ap.parse_args(argv)
    if args.cmd in ("external-objects", "concat-external"):
        sx = load_external(args.schedule, args.plan)
        if args.cmd == "external-objects":
            print(external_objects(sx, args.tool, args.leg))
            return 0
        with open(args.plan) as fh:
            plan = json.load(fh)
        drop: set[str] = set()
        if args.dropped:
            if not args.channel_schedule:
                raise SystemExit("ERROR: --dropped needs --channel-schedule, whose decoupled rows it must name.")
            with open(args.channel_schedule) as fh:
                drop = dropped_keys(plan, json.load(fh), args.dropped)
        for line in concat_external(plan, sx, args.tool, args.results_dir, drop):
            print(f"{args.tool}: {line}")
        return 0
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
    heavy_args = (args.heavy_plan, args.heavy_schedule, args.heavy_dir)
    if any(heavy_args) and not all(heavy_args):
        raise SystemExit("ERROR: --heavy-plan, --heavy-schedule and --heavy-dir go together.")
    if schedule.get("decoupled") and not all(heavy_args):
        raise SystemExit(f"ERROR: the schedule decouples {len(schedule['decoupled'])} row(s), but no heavy legs were given to deliver them.")
    n, report = concat(plan, body, args.channel, list(args.legs), args.out, schedule.get("replicas", []))
    drop = dropped_keys(plan, schedule, args.dropped)
    if all(heavy_args) or drop:
        with open(args.out) as fh:
            light = json.load(fh)
        rows_at = {}
        idx = plan_index(plan)
        for r in light:
            rows_at[idx[sched.key_str(sched.row_key(r))]] = r
        if all(heavy_args):
            hplan, hsched, hbody = load_heavy(args.heavy_plan, args.heavy_schedule, args.channel)
            hpaths = sorted(args.heavy_dir.glob(f"validation_{args.channel}_heavy.leg*.json"))
            got, hreport = heavy_rows(plan, schedule, hplan, hsched, hbody, args.channel, hpaths, args.out.with_name(args.out.name + ".heavy.tmp"))
            report += hreport
            rows_at.update(got)
        discarded = [i for i in rows_at if sched.key_str(sched.row_key(plan[i])) in drop]
        for i in discarded:
            del rows_at[i]
        if drop:
            report.append(f"discarded {len(discarded)} decoupled row(s) the engine refused after the plan was published: {sorted(drop)}")
        write_rows([rows_at[i] for i in sorted(rows_at)], args.out)
        n = len(rows_at)
    print(f"{args.out}: {n} rows from {len(body['legs'])} {args.channel} legs, in plan order")
    for line in report:
        print(f"  {line}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
