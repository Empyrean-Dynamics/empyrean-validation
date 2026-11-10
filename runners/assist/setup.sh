#!/usr/bin/env bash
# Setup ASSIST comparison environment.
#
# Creates a Python virtual environment with rebound + ASSIST and checks the two
# ephemeris files ASSIST reads are present in its folder of the third-party
# tool set (dataset third-party-tools-*, folder `assist/`, synced by the suite
# under $DATA_DIR/third-party). Nothing is downloaded: the planets file
# (linux_p1550p2650.440) and the asteroid file (sb441-n16.bsp) come only from
# that folder, and a missing one is a hard stop naming the set.
#
# Usage:
#   ./runners/assist/setup.sh [ASSIST_DATA_DIR]
#
# Default ASSIST_DATA_DIR: $EMPYREAN_DATA_DIR/third-party/assist
# (or ~/.empyrean/data/third-party/assist).

set -euo pipefail

DATA_DIR="${1:-${EMPYREAN_DATA_DIR:-$HOME/.empyrean/data}/third-party/assist}"
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

# ── ASSIST ephemeris data, from the third-party tool set ─
# The assist/ folder lists every file ASSIST reads. Never downloaded: a missing
# or empty file is a stop, so a run can never use a planets file the pinned set
# does not carry.
PLANETS_FILE="$DATA_DIR/linux_p1550p2650.440"
ASTEROIDS_FILE="$DATA_DIR/sb441-n16.bsp"
for f in "$PLANETS_FILE" "$ASTEROIDS_FILE"; do
    if [ ! -s "$f" ]; then
        echo "ERROR: $f is missing or empty." >&2
        echo "       ASSIST reads its ephemerides from the assist/ folder of the third-party" >&2
        echo "       tool set (synced by CI prep into \$DATA_DIR/third-party). Sync the set," >&2
        echo "       or pass its assist/ folder as the first argument. Nothing is downloaded." >&2
        exit 1
    fi
    echo "  $(basename "$f"): present ($(wc -c < "$f" | tr -d ' ') bytes, third-party set)"
done

echo
echo "Setup complete. Run ASSIST comparison with:"
echo "  source $VENV_DIR/bin/activate"
echo "  python $SCRIPT_DIR/run_assist.py --data-dir $DATA_DIR --output assist_results.json"
