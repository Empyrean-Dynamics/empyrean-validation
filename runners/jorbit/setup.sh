#!/usr/bin/env bash
# Set up a venv with jorbit (Ben Cassese's JAX-based orbital
# integrator) for use as an external propagation/ephemeris reference
# in the validation suite.
#
# jorbit is GPL-licensed and never linked into empyrean — runs in its
# own venv, same isolation pattern as ASSIST / pyoorb / findorb. The
# isolation has the side benefit of keeping JAX off the rest of the
# validation suite's dep tree.
#
# Usage:
#   DATA_DIR=~/.empyrean/data ./jorbit/setup.sh
#
# Kernels: jorbit reads DE440 and SB441-N16 through astropy's download cache,
# and merely importing it runs initialize_jorbit(), which downloads both from
# ssd.jpl.nasa.gov plus its 630 MB mpchecker bundle from Hugging Face (about
# 1.4 GB) into ~/.astropy/cache. This script never imports jorbit directly:
# its smoke test is `run_jorbit.py --check`, which stages the two kernels from
# the registered kernel set in DATA_DIR (the files every empyrean repo pins,
# under the same names) into a runner-private cache with astropy's internet
# access off, imports jorbit and reads DE440. A missing kernel is a hard stop
# that names the file, never a download.
#
# Note: JAX has platform-specific install (CPU vs GPU vs Metal). This
# script installs the CPU build by default, which matches what
# validation needs (deterministic, no per-host GPU dependency).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
VENV_DIR="$SCRIPT_DIR/.venv"

echo "jorbit setup"
echo "  venv: $VENV_DIR"
echo

# ── Create venv ─────────────────────────────────────────
if [ ! -d "$VENV_DIR" ]; then
    echo "Creating virtual environment..."
    if command -v uv &>/dev/null; then
        uv venv --python 3.12 "$VENV_DIR"
    else
        python3 -m venv "$VENV_DIR"
    fi
fi

# ── Install dependencies ────────────────────────────────
# Pinned exactly: the comparison reference must not move between runs without
# a reviewed diff. jorbit 1.7.1 (PyPI, 2026-10-26) requires only jax>=0.4.30;
# jax/jaxlib are pinned to the release this runner was verified with, because
# jorbit's pytree shapes and JAX's numerics both shift between JAX releases.
# Bump all three deliberately, together.
JORBIT_VERSION="${JORBIT_VERSION:-1.7.1}"
JAX_VERSION="${JAX_VERSION:-0.12.0.1}"
echo "Installing jorbit $JORBIT_VERSION + JAX $JAX_VERSION (CPU)..."
if command -v uv &>/dev/null; then
    uv pip install --python "$VENV_DIR/bin/python" \
        "jorbit==$JORBIT_VERSION" "jax==$JAX_VERSION" "jaxlib==$JAX_VERSION" "numpy>=1.24"
else
    "$VENV_DIR/bin/pip" install --upgrade pip
    "$VENV_DIR/bin/pip" install \
        "jorbit==$JORBIT_VERSION" "jax==$JAX_VERSION" "jaxlib==$JAX_VERSION" "numpy>=1.24"
fi

PY="$VENV_DIR/bin/python"
# Offline smoke test (see the header): stages the registered set's kernels and
# imports jorbit with astropy's internet access off. Never `import jorbit`
# here: that is the 1.4 GB download this runner exists to avoid.
DATA_DIR="${DATA_DIR:-$HOME/.empyrean/data}"
"$PY" "$SCRIPT_DIR/run_jorbit.py" --check --data-dir "$DATA_DIR"

echo
echo "Setup complete. Run jorbit comparison with:"
echo "  $PY $SCRIPT_DIR/run_jorbit.py --input validation_plan.json --output validation_jorbit.json --data-dir $DATA_DIR"
