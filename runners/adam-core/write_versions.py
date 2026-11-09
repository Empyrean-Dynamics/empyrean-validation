"""Write runners/adam-core/.venv/adam-core-versions.json.

Provenance stamp for the public adam-core channel: the resolved package versions,
the adam-assist native backend, the public data packages that carry every kernel
and table the run reads (so the sealed-at-run-time inputs are recorded), the bound
CORE_OD_V1 profile pipeline and its declared-vs-measured EFCC18 bias-table SHA-256.
Nothing private is installed, so nothing private is recorded.

Run by setup.sh under the venv interpreter, with VENV in the environment.
"""

from __future__ import annotations

import hashlib
import json
import os
from importlib import metadata
from pathlib import Path


def _version(dist_name: str) -> str | None:
    try:
        return metadata.version(dist_name)
    except metadata.PackageNotFoundError:
        return None


# The public PyPI packages that carry the kernels / tables the run reads. Recording
# their versions documents exactly which sealed inputs were installed.
DATA_PACKAGES = [
    "jpl-debias-2018",            # EFCC18 star-catalog bias table (CORE_OD_V1 resource)
    "jpl-small-bodies-de441-n16", # SB441-n16 massive-asteroid perturbers (ASSIST)
    "naif-de440",                 # DE440 planetary ephemeris (ASSIST)
    "naif-earth-itrf93",          # Earth body-fixed frame
    "naif-eop-high-prec",         # Earth orientation (high precision)
    "naif-eop-historical",
    "naif-eop-predict",
    "naif-leapseconds",
    "mpc-obscodes",               # MPC observatory codes
]


def _native_backend() -> dict:
    try:
        from adam_assist import ASSISTPropagator

        nb = ASSISTPropagator().native_backend
        return {"name": nb.name, "version": nb.version, "work_units": sorted(nb.work_units)}
    except Exception as e:  # noqa: BLE001
        return {"status": f"not exposed ({type(e).__name__}: {e})"}


def _core_od_profile() -> dict:
    """The bound public CORE_OD_V1 pipeline + its EFCC18 resource identity
    (declared SHA-256 vs the SHA-256 of the file actually bound)."""
    try:
        from adam_core.orbit_determination import CORE_OD_V1, core_od_v1_resource_bindings

        transforms = [type(t).__name__ for t in CORE_OD_V1.observation_pipeline.transforms]
        reqs = [
            {"slot": r.slot, "product": r.product, "version": r.product_version,
             "sha256_declared": r.sha256, "byte_size": r.byte_size}
            for r in CORE_OD_V1.requirements
        ]
        rb = core_od_v1_resource_bindings()
        bound = {}
        for slot, path in dict(rb.paths).items():
            p = Path(path)
            measured = hashlib.sha256(p.read_bytes()).hexdigest() if p.is_file() else None
            bound[slot] = {"path": str(p), "sha256_measured": measured}
        return {
            "name": CORE_OD_V1.name,
            "version": CORE_OD_V1.version,
            "definition_sha256": CORE_OD_V1.definition_sha256,
            "pipeline": transforms,
            "requirements": reqs,
            "bound_resources": bound,
        }
    except Exception as e:  # noqa: BLE001
        return {"status": f"not exposed ({type(e).__name__}: {e})"}


def main() -> int:
    venv = Path(os.environ["VENV"])
    stamp = {
        "generated": __import__("datetime").datetime.now(
            __import__("datetime").timezone.utc
        ).isoformat(),
        "adam_core": {"version": _version("adam-core")},
        "adam_assist": {
            "version": _version("adam-assist"),
            "native_backend": _native_backend(),
        },
        "data_packages": {name: _version(name) for name in DATA_PACKAGES},
        "core_od_v1": _core_od_profile(),
        "python": __import__("platform").python_version(),
        "note": "public release only; no private adam-observatory-uncertainties tables",
    }
    out = venv / "adam-core-versions.json"
    out.write_text(json.dumps(stamp, indent=2) + "\n")
    print(json.dumps(stamp, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
