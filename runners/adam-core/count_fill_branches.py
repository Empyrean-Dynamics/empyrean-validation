"""Count how many fixture observations hit each branch of the Vereš 2017 fill.

    runners/adam-core/.venv/bin/python runners/adam-core/count_fill_branches.py \
        --data-dir <third-party>/adam-core [--fixtures-dir fixtures/psv] [--json out.json]

Runs the same ingestion the OD rows use (space-based stations dropped first), and
prints, per object and in total, reported vs filled by level
(station / catalog / no_catalog / catalog_unrecognized). The totals are the numbers
the sigmafill README refers to.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import adam_core_common as C  # noqa: E402
from run_adam_core import resolvable_fn  # noqa: E402
from sigmafill.veres2017 import LEVELS  # noqa: E402


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--fixtures-dir", type=Path, default=C.HERE.parent.parent / "fixtures" / "psv")
    p.add_argument("--data-dir", type=Path, default=os.environ.get("ADAM_CORE_DATA_DIR"))
    p.add_argument("--json", type=Path, default=None)
    args = p.parse_args()
    C.configure_kernels(args.data_dir)  # same start-up contract as the runner
    ok = resolvable_fn()
    model = C.sigma_model()
    total = {"reported": 0, "filled": 0, "total": 0, **{lvl: 0 for lvl in LEVELS}}
    per_obj = {}
    for psv in sorted(args.fixtures_dir.glob("*.psv")):
        cat, _a, _d, _dr, _n, rep = C.build_source_catalog(psv.read_text(), psv.stem, ok, model)
        per_obj[psv.stem] = rep
        for k in ("reported", "filled", "total"):
            total[k] += rep[k]
        for lvl in LEVELS:
            total[lvl] += rep["filled_by_level"].get(lvl, 0)
    print(f"{'object':26s} {'total':>7s} {'reported':>8s} " + " ".join(f"{l:>20s}" for l in LEVELS))
    for o, r in per_obj.items():
        print(f"{o:26s} {r['total']:7d} {r['reported']:8d} "
              + " ".join(f"{r['filled_by_level'].get(l, 0):20d}" for l in LEVELS))
    print(f"{'TOTAL':26s} {total['total']:7d} {total['reported']:8d} "
          + " ".join(f"{total[l]:20d}" for l in LEVELS))
    if args.json:
        args.json.write_text(json.dumps({"total": total, "objects": per_obj}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
