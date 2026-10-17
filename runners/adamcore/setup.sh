#!/usr/bin/env bash
# adam-core external-reference runner — environment setup.
#
# Builds a self-contained Python 3.12 venv holding the Asteroid Institute's
# adam_core orbit-determination stack and the adam-assist ASSIST propagator,
# pinned to exact commits (see "Pins" below). Nothing here imports or links any
# empyrean code; the runner is JSON-in / JSON-out like every other external
# reference.
#
# ONE CHANNEL (tool id `adamcore`): adam-core with its OWN shipped defaults,
#   run_od(models="default") = SigmaFill(v2_sigma_fill) -> EFCC18 debias ->
#   EmpiricalCovariance(v2_full, add) -> NightBatch(cap 4), on the Asteroid
#   Institute's observatory-uncertainty tables. Those tables are PRIVATE /
#   PROPRIETARY (`adam-observatory-uncertainties`, git+ssh), so this runner is
#   LOCAL-ONLY: CI skips it and its results are not published. The tables are never
#   copied/cached/printed into the repo, results or logs — a row carries only the
#   model names, the data-package version string and per-level fill COUNTS.
#
# Pins (settled 2026-10-16):
#   adam_core      git PR #217 `od-obs-uncertainty-rust` head 3d01b9a4...  (maturin/cargo)
#   adam-assist    git branch `kk/native-od-propagator` head ce52ce1e...   (native OD propagator)
#                  Its pyproject pins adam-core>=0.5.7,<0.6 — overridden to the adam_core pin.
#   data tables    git+ssh adam-observatory-uncertainties @ v0.3.0          (REQUIRED; private)
#   EFCC18 bias    download_efcc18_bias_table() from JPL, cached in .cache/efcc18
#
# Fallback (STOP-1): if adam_core @ 3d01b9a4 and adam-assist @ ce52ce1e disagree at the
# capsule/ABI or adapter-namespace level, re-run with ADAMCORE_REV=e856436c (the adam_core
# rev adam-assist built its Rust against). Neither package is patched.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VENV="$HERE/.venv"
PY="$VENV/bin/python"

# ── Pins ────────────────────────────────────────────────────
ADAMCORE_REPO="git+https://github.com/B612-Asteroid-Institute/adam_core"
ADAMCORE_REV="${ADAMCORE_REV:-3d01b9a433294aad1970486ec1daf4c4855b6190}"   # PR #217 head
ADAMASSIST_REPO="git+https://github.com/B612-Asteroid-Institute/adam-assist"
ADAMASSIST_REV="${ADAMASSIST_REV:-ce52ce1ed8f17989bb377d6d42a295d7fb8617d3}"  # kk/native-od-propagator
DATA_REPO="git+ssh://git@github.com/B612-Asteroid-Institute/adam-observatory-uncertainties.git"
B612_TAG="${B612_TAG:-v0.3.0}"   # the private observatory-uncertainty data package

# EFCC18 cache (debiasing table), stamped into versions.json; never a repo file.
export ADAM_CORE_EFCC18_DIR="${ADAM_CORE_EFCC18_DIR:-$HERE/.cache/efcc18}"
mkdir -p "$ADAM_CORE_EFCC18_DIR"

echo "──── adam-core runner setup ────────────────────────────────"
echo "  adam_core   @ $ADAMCORE_REV"
echo "  adam-assist @ $ADAMASSIST_REV"
echo "  data tables : adam-observatory-uncertainties @ $B612_TAG (PRIVATE; required; local-only runner)"
echo "  EFCC18 dir  : $ADAM_CORE_EFCC18_DIR"
command -v cargo >/dev/null || { echo "ERROR: cargo not found; adam_core builds its Rust extension with maturin." >&2; exit 1; }
echo "  cargo       : $(cargo --version)"

# ── venv ────────────────────────────────────────────────────
if [ ! -x "$PY" ]; then
    echo "──── Creating venv (python 3.12) ───────────────────────────"
    uv venv --python 3.12 "$VENV"
fi

# ── adam-assist pins adam-core<0.6; override it to our adam_core pin ─
OVERRIDES="$VENV/adamcore-overrides.txt"
cat > "$OVERRIDES" <<EOF
adam-core @ ${ADAMCORE_REPO}@${ADAMCORE_REV}
EOF

echo "──── Installing adam_core + adam-assist (maturin build) ────"
# --override forces adam-assist's transitive adam-core<0.6 pin onto our git pin.
uv pip install --python "$PY" --override "$OVERRIDES" \
    "adam-core @ ${ADAMCORE_REPO}@${ADAMCORE_REV}" \
    "adam-assist @ ${ADAMASSIST_REPO}@${ADAMASSIST_REV}"

# ── Private observatory-uncertainty data package (REQUIRED) ─
# The default model stack (run_od models="default") loads its sigma/bias tables
# from this package, so the runner cannot fit without it. It is proprietary and
# git+ssh-only; it goes into the venv only and is never vendored into the repo,
# results or logs. A refused SSH clone is a hard, clear failure — this runner is
# local-only and has no public fallback.
echo "──── Installing PRIVATE adam-observatory-uncertainties @ ${B612_TAG} ────"
echo "     (proprietary; into the venv only; never vendored into the repo)"
if ! uv pip install --python "$PY" --override "$OVERRIDES" \
        "adam-observatory-uncertainties @ ${DATA_REPO}@${B612_TAG}"; then
    echo "ERROR: could not install adam-observatory-uncertainties @ ${B612_TAG} over git+ssh." >&2
    echo "       This is a PRIVATE package and this runner is LOCAL-ONLY — it provides the" >&2
    echo "       observatory-uncertainty tables the default fitter requires, and there is no" >&2
    echo "       public fallback. Ensure your SSH key has access to" >&2
    echo "       github.com/B612-Asteroid-Institute/adam-observatory-uncertainties and retry." >&2
    exit 1
fi

# ── EFCC18 debias table (public JPL archive) ────────────────
# adam_core's download_efcc18_bias_table(verify_checksum=True, the default) checks
# the downloaded bias.dat against EFCC18_BIAS_DAT_SHA256 itself and raises on
# mismatch — so the SHA-256 verification is adam_core's, not ours. We additionally
# re-assert the materialized file against that exposed constant here (fail loudly),
# and stamp the measured SHA into versions.json.
echo "──── Fetching EFCC18 bias table (JPL, SHA-256 verified by adam_core) ────"
ADAM_CORE_EFCC18_DIR="$ADAM_CORE_EFCC18_DIR" "$PY" - <<'PYEOF'
import hashlib, os, sys
from adam_core.observations.efcc18 import download_efcc18_bias_table, EFCC18_BIAS_DAT_SHA256
d = os.environ["ADAM_CORE_EFCC18_DIR"]
try:
    download_efcc18_bias_table()  # verify_checksum=True by default
    print(f"  EFCC18 bias table materialized under {d}")
except Exception as e:  # noqa: BLE001
    print(f"ERROR: EFCC18 download failed: {e}", file=sys.stderr)
    raise
# Belt-and-suspenders: re-verify the on-disk bias.dat against adam_core's constant.
bias = os.path.join(d, "bias.dat")
got = hashlib.sha256(open(bias, "rb").read()).hexdigest()
if got != EFCC18_BIAS_DAT_SHA256:
    print(f"ERROR: EFCC18 bias.dat SHA-256 mismatch: got {got}, "
          f"expected {EFCC18_BIAS_DAT_SHA256}", file=sys.stderr)
    sys.exit(1)
print(f"  EFCC18 bias.dat SHA-256 verified: {got}")
PYEOF

# ── Kernel bootstrap (first run online) ─────────────────────
# adam_core's resolver fetches DE440 + SB441-n16 SPICE kernels on first use;
# a one-step ASSISTPropagator construction warms that cache.
echo "──── Bootstrapping SPICE kernels (first run online) ────────"
"$PY" - <<'PYEOF' || echo "  (kernel bootstrap deferred; first real propagation will fetch)"
from adam_assist import ASSISTPropagator
p = ASSISTPropagator()
print("  ASSISTPropagator constructed; kernels resolved.")
PYEOF

# ── versions.json ───────────────────────────────────────────
echo "──── Writing adamcore-versions.json ────────────────────────"
ADAM_CORE_EFCC18_DIR="$ADAM_CORE_EFCC18_DIR" \
ADAMCORE_REV="$ADAMCORE_REV" ADAMASSIST_REV="$ADAMASSIST_REV" B612_TAG="$B612_TAG" \
VENV="$VENV" "$PY" "$HERE/write_versions.py"

echo "──── setup complete ────────────────────────────────────────"
echo "  versions: $VENV/adamcore-versions.json"
