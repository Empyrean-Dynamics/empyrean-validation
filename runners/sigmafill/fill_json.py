"""JSON front end to the shared Vereš et al. (2017) sigma fill, for runners that
are not written in Python (the adam-core Rust API runner).

It is the same fill, through the same function, as every Python runner uses
(``veres2017.fill_missing`` on ``veres2017_sigma_table.csv``): a reported sigma is
never overwritten, and each filled observation is labelled by level. Only the
transport differs, one JSON object in on stdin and one out on stdout, so a
runner in another language keeps one implementation instead of a port.

stdin::

    {"stn": [str, ...], "astcat": [str | null, ...],
     "rms_ra": [float | null, ...], "rms_dec": [float | null, ...]}

``rms_ra`` is ADES ``rmsRA`` (RA·cos(Dec)) and ``rms_dec`` is ``rmsDec``, both in
arcseconds; null, NaN or a non-positive value marks a missing sigma.

stdout::

    {"ra_sigma": [float, ...], "dec_sigma": [float, ...], "report": {...}}

where ``report`` is exactly ``fill_missing``'s report (model, table, table_sha256,
reported, filled, partially_reported, filled_by_level, total). A malformed request
exits 2 with the reason on stderr and writes nothing to stdout.

Standard library only, like ``veres2017``, so any ``python3`` runs it.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))  # runners/

from sigmafill.veres2017 import Veres2017SigmaModel, fill_missing

FIELDS = ("stn", "astcat", "rms_ra", "rms_dec")


def fill_request(request: dict, model: Veres2017SigmaModel | None = None) -> dict:
    """Fill one request. Raises TypeError or ValueError naming the defect for a
    malformed one."""
    if not isinstance(request, dict):
        raise TypeError("the request must be a JSON object")
    missing = [f for f in FIELDS if f not in request]
    if missing:
        raise ValueError(f"the request lacks {', '.join(missing)}")
    cols = {f: request[f] for f in FIELDS}
    for f, col in cols.items():
        if not isinstance(col, list):
            raise TypeError(f"{f} must be a list")
    n = len(cols["stn"])
    uneven = [f for f in FIELDS if len(cols[f]) != n]
    if uneven:
        raise ValueError(f"{', '.join(uneven)} must have the length of stn ({n})")
    ra, dec, report = fill_missing(
        cols["stn"], cols["astcat"], cols["rms_ra"], cols["rms_dec"],
        model if model is not None else Veres2017SigmaModel.from_csv(),
    )
    return {"ra_sigma": ra, "dec_sigma": dec, "report": report}


def main() -> int:
    try:
        response = fill_request(json.load(sys.stdin))
    except (TypeError, ValueError) as e:
        print(f"sigmafill: {e}", file=sys.stderr)
        return 2
    json.dump(response, sys.stdout, allow_nan=False)
    return 0


if __name__ == "__main__":
    sys.exit(main())
