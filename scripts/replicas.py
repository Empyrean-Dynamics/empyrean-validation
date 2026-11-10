#!/usr/bin/env python3
"""Fold a row's timing replicas into the one row the report reads.

A row whose single run costs more than ten minutes (the 2020 CD3 propagation
at dt = -5475 d takes 42 minutes in every channel, and every channel times it
best of three) runs its three timing repetitions on three legs, one each, so
no leg holds a 125-minute unit. The three legs compute the same row; this
module is the merge:

  * the replicas must be IDENTICAL in every field but the volatile set (the
    timestamp and the wall-clock measurements), bit for bit, or the fold fails
    naming the row, the field and the legs. A replica that disagrees means the
    engine is not deterministic, or a leg ran different code or inputs, and
    averaging it away would publish a number nobody can reproduce;
  * the folded row keeps the MINIMUM emp_time_ms, which is the best-of-three
    statistic the channel reported before, measured across three runners of
    one class instead of one;
  * the other volatile fields come from the first replica in leg order, so the
    result does not depend on the order legs finished.

Used by scripts/channel-chunks.py (concat) and scripts/assemble-reference.py.
"""

from __future__ import annotations

import json

# Fields that legitimately differ between two runs of the same row.
VOLATILE_EXACT = {"timestamp", "emp_time_ms", "speed_ratio"}
VOLATILE_SUFFIX = "_time_ms"


def is_volatile(field: str) -> bool:
    return field in VOLATILE_EXACT or field.endswith(VOLATILE_SUFFIX)


class FoldError(Exception):
    """Replicas that disagree, or a replica set that is not whole."""


def _canon(v) -> str:
    """Bit-exact comparison key: the shortest round-trip JSON of the value."""
    return json.dumps(v, sort_keys=True, allow_nan=False)


def first_difference(a: dict, b: dict) -> str | None:
    """The first non-volatile field in which two rows differ, or None."""
    for field in sorted(set(a) | set(b)):
        if is_volatile(field):
            continue
        if field not in a or field not in b or _canon(a[field]) != _canon(b[field]):
            return field
    return None


def fold(row_name: str, replicas: list[tuple[str, dict]]) -> dict:
    """Fold `[(leg, row), ...]` (already in leg order) into one row.

    Raises FoldError naming the row, the field and the two legs when any
    non-volatile field differs."""
    if not replicas:
        raise FoldError(f"{row_name}: no replicas to fold")
    leg0, row0 = replicas[0]
    for leg, row in replicas[1:]:
        field = first_difference(row0, row)
        if field is not None:
            raise FoldError(
                f"{row_name}: timing replicas disagree in `{field}` between legs {leg0} and {leg} "
                f"({_canon(row0.get(field))[:80]} vs {_canon(row.get(field))[:80]}). "
                "Replicas of one row must be identical bit for bit."
            )
    out = dict(row0)
    times = [r.get("emp_time_ms") for _, r in replicas]
    if any(t is None for t in times):
        if any(t is not None for t in times):
            raise FoldError(f"{row_name}: some replicas carry emp_time_ms and some do not")
    else:
        out["emp_time_ms"] = min(times)
    return out
