#!/usr/bin/env bash
# Setup ASSIST comparison environment.
#
# Creates a Python virtual environment with rebound + ASSIST and downloads
# the required ASSIST ephemeris data files.
#
# Usage:
#   ./scripts/assist/setup.sh [--data-dir DIR]
#
# Default data directory: ~/.empyrean/data/

set -euo pipefail

DATA_DIR="${1:-${EMPYREAN_DATA_DIR:-$HOME/.empyrean/data}}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
VENV_DIR="$SCRIPT_DIR/.venv"

echo "ASSIST setup"
echo "  venv:     $VENV_DIR"
echo "  data dir: $DATA_DIR"
echo

# ── Create venv ─────────────────────────────────────────
if [ ! -d "$VENV_DIR" ]; then
    echo "Creating virtual environment..."
    if command -v uv &>/dev/null; then
        # --seed installs pip/setuptools into the venv. A bare `uv venv`
        # is pip-less, which breaks the `python -m pip install` below
        # ("No module named pip"). We deliberately install with pip
        # rather than `uv pip` (see the dependency-install note), so the
        # venv must actually carry pip.
        uv venv --seed --python 3.12 "$VENV_DIR"
    else
        python3 -m venv "$VENV_DIR"
    fi
fi

source "$VENV_DIR/bin/activate"

# ── Install dependencies ────────────────────────────────
# Exact pins: the comparator must be reproducible, and unpinned floats
# have broken this build before. Installed with pip rather than `uv pip`:
# assist ships sdist-only and its setup.py shells out to git during the
# build, which dies on uv's sdist cache layout
# ("fatal: invalid gitfile format: .../uv/sdists-v9/.git").
echo "Installing dependencies..."
python -m pip install --upgrade pip >/dev/null
python -m pip install "rebound==4.6.0" "assist==1.2.3" "numpy>=1.24,<3"

# ── Download ASSIST ephemeris data ──────────────────────
mkdir -p "$DATA_DIR"

PLANETS_FILE="$DATA_DIR/linux_p1550p2650.440"
ASTEROIDS_FILE="$DATA_DIR/sb441-n16.bsp"

if [ ! -f "$PLANETS_FILE" ]; then
    echo "Downloading ASSIST planets ephemeris (~100 MB)..."
    curl -L -o "$PLANETS_FILE" \
        "https://ssd.jpl.nasa.gov/ftp/eph/planets/Linux/de440/linux_p1550p2650.440"
else
    echo "Planets ephemeris: already present"
fi

if [ ! -f "$ASTEROIDS_FILE" ]; then
    echo "SB441 asteroid ephemeris should already be in $DATA_DIR from empyrean init"
    echo "If not, run: empyrean init"
else
    echo "Asteroid ephemeris: already present"
fi

echo
echo "Setup complete. Run ASSIST comparison with:"
echo "  source $VENV_DIR/bin/activate"
echo "  python $SCRIPT_DIR/run_assist.py --data-dir $DATA_DIR --output assist_results.json"
