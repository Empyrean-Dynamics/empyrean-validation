#!/usr/bin/env bash
# Set up a venv with layup — Matthew Holman's (Smithsonian / CfA)
# MIT-licensed, ASSIST-backed orbit fitter — for use as an external
# orbit-determination reference in the validation suite.
#
# layup runs in its own venv, same isolation pattern as ASSIST / find_orb /
# OpenOrb / jorbit / kete. layup is MIT-licensed (unlike the GPL externals),
# but it is still kept out-of-tree and subprocess-isolated: it pulls a heavy
# dependency graph (a C extension built via scikit-build-core against the
# `assist` + `rebound` C libraries, plus `sorcha` from git and `jax`), which
# has no business on the rest of the validation suite's dep tree.
#
# Usage:
#   ./layup/setup.sh                 # clone pinned ref, build, bootstrap data
#   LAYUP_REF=<sha|tag|branch> ./layup/setup.sh   # override the pinned ref
#
# Prerequisites: python >=3.11, git, a C compiler, and cmake (scikit-build-core
# builds the layup C extension against assist/rebound). CI installs these; on a
# dev box they are typically already present.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
VENV_DIR="$SCRIPT_DIR/.venv"
BUILD_DIR="$SCRIPT_DIR/build"
SRC_DIR="$BUILD_DIR/layup"

# Pin to a known commit so the validation-of-record is reproducible; layup is
# early (v0.0.1) and moving fast, so bump this deliberately — same discipline as
# the kete / jorbit pins. Override with LAYUP_REF for local experiments.
LAYUP_REF="${LAYUP_REF:-60b5b753fb74446fa5350fa12e5866de12fb1ba1}"

echo "layup setup"
echo "  venv:  $VENV_DIR"
echo "  src:   $SRC_DIR"
echo "  ref:   $LAYUP_REF"
echo

# ── Create venv (python >=3.11) ─────────────────────────
if [ ! -d "$VENV_DIR" ]; then
    echo "Creating virtual environment..."
    if command -v uv &>/dev/null; then
        uv venv --python 3.12 "$VENV_DIR"
    else
        python3 -m venv "$VENV_DIR"
    fi
fi
PY="$VENV_DIR/bin/python"
if command -v uv &>/dev/null; then
    PIP=(uv pip install --python "$PY")
else
    "$PY" -m pip install --upgrade pip
    PIP=("$PY" -m pip install)
fi

# ── Clone layup (recursive: assist, rebound, eigen submodules) ──
mkdir -p "$BUILD_DIR"
if [ ! -d "$SRC_DIR/.git" ]; then
    echo "Cloning Smithsonian/layup (recursive)..."
    git clone --recursive https://github.com/Smithsonian/layup.git "$SRC_DIR"
else
    echo "Updating existing layup checkout..."
    git -C "$SRC_DIR" fetch --tags origin
fi
git -C "$SRC_DIR" checkout --quiet "$LAYUP_REF"
git -C "$SRC_DIR" submodule update --init --recursive

# ── Install (builds the C extension; pulls sorcha/jax/assist/rebound) ──
echo
echo "Installing layup (this compiles a C extension — may take a few minutes)..."
"${PIP[@]}" "$SRC_DIR"

# ── Bootstrap ephemeris + reference data (best-effort) ──
# `layup bootstrap` downloads a few hundred MB (SPICE kernels, SB kernel, MPC
# obscodes, debias tables). Best-effort: if it fails (e.g. offline), the fit
# path lazily re-downloads what it needs, and a missing bootstrap surfaces as a
# skipped fixture in the runner — never a silently faked result.
echo
# Call the direct `layup-bootstrap` entry point rather than the `layup
# bootstrap` dispatcher: the dispatcher discovers verbs by scanning $PATH for
# layup-* executables and finds none when the venv bin dir is not on $PATH.
echo "Downloading layup reference data (layup-bootstrap)..."
if ! "$VENV_DIR/bin/layup-bootstrap"; then
    echo "WARNING: layup bootstrap failed — fits will attempt lazy downloads." >&2
fi

# ── Smoke test ──────────────────────────────────────────
echo
"$PY" - <<'PY' || echo "WARNING: layup import smoke-test failed"
import layup
print(f"layup {getattr(layup, '__version__', '?')} import OK")
PY

echo
echo "Setup complete. Run the layup OD comparison with:"
echo "  $PY $SCRIPT_DIR/run_layup.py <fixtures/psv> --output results/validation_layup.json"
