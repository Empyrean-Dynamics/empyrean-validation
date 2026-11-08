#!/usr/bin/env python3
"""Chunk the per-object matrix for the channel legs, and reassemble their outputs.

The per-method plan replays every uncertainty method on every channel, so one
channel over the full catalog outruns any job budget (run 171, 2026-10-22: the
python and core legs were each still replaying at 240 minutes). The channel
job therefore fans out over object chunks the way the reference job fans out
over objects, each leg replays the plan filtered to its chunk, and the reduce
job concatenates each channel's chunks back into the one file the merge reads.

    channel-chunks.py split  --objects results/objects.json --count 10
        prints a JSON list of {"id": "01", "objects": "A,B,C"}; objects are
        dealt round-robin in matrix order so heavy neighbours spread out; the
        count is capped at the number of objects, so a subset dispatch never
        yields an empty chunk.

    channel-chunks.py filter --plan results/validation_plan.json \
        --objects "A,B,C" --output results/validation_plan.chunk.json
        writes the plan rows whose "object" is one of the named objects, in
        plan order, and prints each object's row count and the total. Names
        are matched exactly, in the plan's own spelling, comma-separated like
        split's "objects" field. An empty list, an empty or repeated entry,
        or a name with no plan rows is a hard stop: a leg handed the wrong
        rows replays the wrong work, and one handed the whole plan replays
        every object and outruns its budget (2026-10-28: every chunk leg
        replayed all 30,033 plan rows; 38 of the 40 hit the 240-minute
        wall and the other two lost their runners first).

    channel-chunks.py concat --out results/validation_python.json --expect 10 \
        results/validation_python.chunk*.json
        requires exactly --expect files, each a JSON list, and writes their
        rows in chunk order. A missing chunk, a duplicate id or a non-list
        file is a hard stop: a silently short channel would read downstream
        as "compared and agreed".
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

CHUNK_RE = re.compile(r"\.chunk(\d+)\.json$")


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


def parse_objects(spec: str) -> list[str]:
    names = [n.strip() for n in spec.split(",")]
    if not any(names):
        raise SystemExit("ERROR: --objects is empty; a plan chunk names at least one object.")
    if not all(names):
        raise SystemExit(f"ERROR: --objects {spec!r} has an empty entry; OBJECTS is comma-separated.")
    repeated = sorted({n for n in names if names.count(n) > 1})
    if repeated:
        raise SystemExit("ERROR: --objects names " + ", ".join(repeated) + " more than once.")
    return names


def filter_plan(rows: list, objects: list[str]) -> tuple[list, dict[str, int]]:
    if not isinstance(rows, list):
        raise SystemExit("ERROR: the plan is not a JSON list of rows.")
    counts = dict.fromkeys(objects, 0)
    kept: list = []
    for i, row in enumerate(rows):
        name = row.get("object") if isinstance(row, dict) else None
        if not isinstance(name, str):
            raise SystemExit(f'ERROR: plan row {i} carries no "object" name.')
        if name in counts:
            counts[name] += 1
            kept.append(row)
    missing = [n for n, c in counts.items() if c == 0]
    if missing:
        held = sorted({r["object"] for r in rows})
        raise SystemExit(
            "ERROR: the plan holds no rows for "
            + ", ".join(repr(n) for n in missing)
            + f". Names match the plan's spelling exactly; it holds {len(held)} objects: "
            + ", ".join(held)
            + "."
        )
    return kept, counts


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


def concat(paths: list[Path], expect: int, out: Path) -> int:
    ids = []
    for p in paths:
        m = CHUNK_RE.search(p.name)
        if not m:
            raise SystemExit(f"ERROR: {p} is not a chunk file (expected *.chunkNN.json).")
        ids.append((int(m.group(1)), p))
    if len(ids) != expect:
        raise SystemExit(
            f"ERROR: expected {expect} chunk files for {out.name}, found {len(ids)}: "
            + ", ".join(p.name for _, p in sorted(ids))
            + ". A channel leg did not deliver; refusing to assemble a short channel."
        )
    seen = {k for k, _ in ids}
    if len(seen) != len(ids):
        raise SystemExit(f"ERROR: duplicate chunk ids for {out.name}.")
    rows: list = []
    for _, p in sorted(ids):
        with open(p) as f:
            part = json.load(f)
        if not isinstance(part, list):
            raise SystemExit(f"ERROR: {p} is not a JSON list of rows.")
        rows.extend(part)
    write_rows(rows, out)
    return len(rows)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("split")
    s.add_argument("--objects", required=True, type=Path, help="results/objects.json from `make list-objects`")
    s.add_argument("--count", required=True, type=int)
    f = sub.add_parser("filter")
    f.add_argument("--plan", required=True, type=Path, help="the full plan, results/validation_plan.json")
    f.add_argument("--objects", required=True, help='comma-separated object names, as split\'s "objects" field')
    f.add_argument("--output", required=True, type=Path)
    c = sub.add_parser("concat")
    c.add_argument("--out", required=True, type=Path)
    c.add_argument("--expect", required=True, type=int)
    c.add_argument("chunks", nargs="*", type=Path)
    args = ap.parse_args(argv)
    if args.cmd == "split":
        with open(args.objects) as f:
            objects = json.load(f)
        print(json.dumps(split(objects, args.count), separators=(",", ":")))
        return 0
    if args.cmd == "filter":
        # The list is checked before the plan is read: a bad OBJECTS fails in
        # milliseconds, not after parsing the full plan.
        objects = parse_objects(args.objects)
        with open(args.plan) as fh:
            rows = json.load(fh)
        kept, counts = filter_plan(rows, objects)
        write_rows(kept, args.output)
        for name, n in counts.items():
            print(f"  {name}: {n} rows")
        print(
            f"Plan chunk: {len(kept)} rows for OBJECTS={','.join(objects)} "
            f"(of {len(rows)} rows in {args.plan.name}) -> {args.output}"
        )
        return 0
    n = concat(list(args.chunks), args.expect, args.out)
    print(f"{args.out}: {n} rows from {args.expect} chunks")
    return 0


if __name__ == "__main__":
    sys.exit(main())
