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
#   ./jorbit/setup.sh
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
# Pin the JAX major version because jorbit's pytree shapes can shift
# between JAX releases. Update both deliberately so we don't
# inadvertently change the comparison reference between runs.
echo "Installing jorbit + JAX (CPU)..."
if command -v uv &>/dev/null; then
    uv pip install --python "$VENV_DIR/bin/python" \
        "jorbit>=1.0" "jax>=0.4.30" "jaxlib>=0.4.30" "numpy>=1.24"
else
    "$VENV_DIR/bin/pip" install --upgrade pip
    "$VENV_DIR/bin/pip" install \
        "jorbit>=1.0" "jax>=0.4.30" "jaxlib>=0.4.30" "numpy>=1.24"
fi

PY="$VENV_DIR/bin/python"
$PY - <<'PY' || echo "WARNING: jorbit import smoke-test failed"
import jorbit, jax  # noqa: F401
print(f"jorbit {jorbit.__version__} on JAX {jax.__version__}")
PY

echo
echo "Setup complete. Run jorbit comparison with:"
echo "  $PY $SCRIPT_DIR/run_jorbit.py --input validation_plan.json --output validation_jorbit.json"
