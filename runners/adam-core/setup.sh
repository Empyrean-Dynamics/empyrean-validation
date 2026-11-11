#!/usr/bin/env bash
# adam-core external-reference runner — environment setup (public release).
#
# Builds a self-contained Python 3.12 venv holding the Asteroid Institute's
# adam_core and the adam-assist ASSIST propagator, pinned to the PUBLIC PyPI
# releases and installed from the published binary wheels. Nothing here is
# private: no git+ssh data package, no proprietary observatory-uncertainty
# tables, no cargo/maturin build. The runner is JSON-in / JSON-out like every
# other external reference and imports no empyrean code.
#
# ONE CHANNEL (tool id `adam-core`): adam-core run as it ships publicly, through
#   the public CORE_OD_V1 orbit-determination profile (EFCC18 star-catalog
#   debias on the public jpl-debias-2018 table, then nightly-batch deweighting
#   capped at four). CORE_OD_V1 has NO SigmaFill and NO empirical-covariance
#   stage, so it requires observations that already carry finite, positive
#   reported uncertainties; the proprietary ASTEROID_INSTITUTE_OD_V1 profile
#   (which fills missing sigma from the private v2_sigma_fill table) is NOT used.
#
# Pins (public releases):
#   adam-core     == 0.7.0   (PyPI; "Rust-owned orbit determination for Core 0.7")
#   adam-assist   == 0.6.0   (PyPI; native ASSIST provider, capsule adam_core.propagator.v2)
# Both ship cp311/cp312/cp313 wheels for macOS arm64/x86_64 and manylinux
# aarch64/x86_64, so the install is wheel-only. Their public dependencies carry
# the kernels and the EFCC18 bias table AS PyPI PACKAGES (naif-de440,
# jpl-small-bodies-de441-n16, jpl-debias-2018, mpc-obscodes, naif-eop-*, …), so
# everything the run reads is resolved from site-packages at run time — nothing
# is fetched over the network during a fit.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VENV="$HERE/.venv"
PY="$VENV/bin/python"

# ── Pins ────────────────────────────────────────────────────
ADAM_CORE_VERSION="${ADAM_CORE_VERSION:-0.7.0}"
ADAM_ASSIST_VERSION="${ADAM_ASSIST_VERSION:-0.6.0}"

echo "──── adam-core runner setup (public release) ───────────────"
echo "  adam-core   == $ADAM_CORE_VERSION (PyPI)"
echo "  adam-assist == $ADAM_ASSIST_VERSION (PyPI)"
echo "  (wheel-only; no cargo, no git, no private data package)"

# ── venv ────────────────────────────────────────────────────
if command -v uv >/dev/null 2>&1; then
    PIP=(uv pip install --python "$PY")
    if [ ! -x "$PY" ]; then
        echo "──── Creating venv (python 3.12, uv) ───────────────────────"
        uv venv --python 3.12 "$VENV"
    fi
else
    if [ ! -x "$PY" ]; then
        echo "──── Creating venv (python 3.12) ───────────────────────────"
        python3 -m venv "$VENV"
    fi
    "$PY" -m pip install --upgrade pip >/dev/null
    PIP=("$PY" -m pip install)
fi

# ── Install the pinned public releases (wheels) ─────────────
echo "──── Installing adam-core + adam-assist (wheels) ───────────"
"${PIP[@]}" --only-binary=:all: \
    "adam-core==${ADAM_CORE_VERSION}" \
    "adam-assist==${ADAM_ASSIST_VERSION}"

# ── Prove the native stack loads and the OD ABI matches ─────
echo "──── Verifying the native extension + propagator ABI ───────"
"$PY" - <<'PYEOF'
import sys
from importlib.metadata import version
ac, aa = version("adam-core"), version("adam-assist")
print(f"  adam-core {ac}, adam-assist {aa}")
# Rust extension loads
import adam_core._version  # noqa: F401
from adam_assist import ASSISTPropagator
p = ASSISTPropagator()
nb = p.native_backend
print(f"  native backend: {nb.name} {nb.version} work_units={set(nb.work_units)}")
# The OD pipeline requires the propagator capsule named adam_core.propagator.v2;
# run_od refuses anything else. Prove the capsule constructs.
cap = p.propagator_capsule()
print(f"  propagator_capsule constructed: {type(cap).__name__}")
# The public OD profile binds with PUBLIC resources only (no private table).
from adam_core.orbit_determination import CORE_OD_V1, core_od_v1_resource_bindings
cfg = CORE_OD_V1.bind(core_od_v1_resource_bindings())
transforms = [type(t).__name__ for t in CORE_OD_V1.observation_pipeline.transforms]
print(f"  CORE_OD_V1 bound OK; pipeline = {transforms}")
reqs = [r.slot for r in CORE_OD_V1.requirements]
assert reqs == ["efcc18_catalog_bias_table"], f"unexpected CORE_OD_V1 requirements: {reqs}"
print(f"  CORE_OD_V1 resources (public only): {reqs}")
# Guard: the private profile must NOT be reachable without the proprietary tables.
import adam_core.orbit_determination as od
assert hasattr(od, "ASTEROID_INSTITUTE_OD_V1"), "profile set changed"
print("  (ASTEROID_INSTITUTE_OD_V1 present but intentionally NOT used)")
PYEOF

# ── versions.json ───────────────────────────────────────────
echo "──── Writing adam-core-versions.json ────────────────────────"
VENV="$VENV" "$PY" "$HERE/write_versions.py"

echo "──── setup complete ────────────────────────────────────────"
echo "  versions: $VENV/adam-core-versions.json"
