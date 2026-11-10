#!/usr/bin/env python3
"""Validate one external comparator's output before its leg uploads it.

`merge-external` (src/comparators.rs) reads each tool's per-row JSON on its own
and skips a tool whose file is unusable, naming it on the report. That keeps a
bad file from sinking the report, but the better place to catch it is the leg
that wrote it: a leg that produced garbage should fail as itself, with the tool
named, rather than upload the garbage and let reduce discover it hours later.

The shape checked is the one the merge reads, mirrored from
`comparators::read_tool_records`: the file exists, is not empty, parses as JSON,
is an array, and every element is an object carrying a string `object`. An
empty array is allowed (a tool that compared nothing says so with `[]`).

    validate_external.py --tool kete --results-dir results

checks `results/validation_kete.json`, plus `results/validation_kete_radar.json`
when it exists (find_orb and GRSS write a second, radar pass).
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path


def check_file(path: Path) -> str | None:
    """Return why the file is unusable, or None when it has the shape the merge reads."""
    try:
        text = path.read_text()
    except OSError as e:
        return f"read {path}: {e}"
    if not text.strip():
        return f"{path}: file is empty"
    try:
        value = json.loads(text)
    except json.JSONDecodeError as e:
        return f"parse {path}: {e}"
    if not isinstance(value, list):
        return f"{path}: expected a JSON array of per-row records, found {type(value).__name__}"
    for i, rec in enumerate(value):
        if not isinstance(rec, dict) or not isinstance(rec.get("object"), str):
            return f"{path}: record {i} is not an object with a string `object` field"
    return None


def expected_files(tool: str, results_dir: Path) -> list[Path]:
    """The tool's main file (required) and its radar pass (checked when present)."""
    main = results_dir / f"validation_{tool}.json"
    radar = results_dir / f"validation_{tool}_radar.json"
    return [main] + ([radar] if radar.exists() else [])


def validate(tool: str, results_dir: Path) -> list[str]:
    problems = []
    for path in expected_files(tool, results_dir):
        why = check_file(path)
        if why:
            problems.append(why)
    return problems


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--tool", required=True)
    ap.add_argument("--results-dir", type=Path, default=Path("results"))
    args = ap.parse_args(argv)
    problems = validate(args.tool, args.results_dir)
    for why in problems:
        print(f"ERROR: external comparator `{args.tool}` produced an unusable file: {why}", file=sys.stderr)
    if problems:
        print(
            f"       This leg fails as `{args.tool}` so the bad file never reaches reduce "
            "(where merge-external would skip the comparator and name it on the report).",
            file=sys.stderr,
        )
        return 1
    files = ", ".join(str(p) for p in expected_files(args.tool, args.results_dir))
    print(f"external `{args.tool}`: output validated ({files})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
