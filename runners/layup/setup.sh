#!/usr/bin/env bash
# Set up a venv with layup — Matthew Holman's (Smithsonian / CfA)
# MIT-licensed, ASSIST-backed orbit fitter — for use as an external
# orbit-determination reference in the validation suite.
#
# layup runs in its own venv, same isolation pattern as ASSIST / find_orb /
# OpenOrb / jorbit / kete. layup is MIT-licensed (unlike the GPL externals),
# but it is still kept out-of-tree and subprocess-isolated: it pulls a heavy
# dependency graph (a C extension built via scikit-build-core against the
# `assist` + `rebound` C libraries, plus `sorcha` and `jax`), which has no
# business on the rest of the validation suite's dep tree.
#
# Usage:
#   ./layup/setup.sh                           # pinned release, staged data
#   LAYUP_VERSION=<x.y.z> ./layup/setup.sh     # another PyPI release
#   DATA_DIR=<dir> ./layup/setup.sh            # another synced kernel set
#
# Prerequisites: python >=3.11, a C compiler, and cmake. layup publishes an
# sdist only, so the install compiles its C extension against assist/rebound.
# CI installs these; on a dev box they are typically already present.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
VENV_DIR="$SCRIPT_DIR/.venv"
STAGED_DIR="$SCRIPT_DIR/data"
DATA_DIR="${DATA_DIR:-$HOME/.empyrean/data}"

# Pinned to a release so the validation-of-record is reproducible and every
# setup builds the same layup: 1.1.0 is the PyPI release of 2026-09-21 (the
# upstream v1.1.0 tag). Bump it deliberately — same discipline as the ASSIST /
# GRSS pins; run_layup.py stamps the installed version into every row's
# source_version, so an override is always visible in the report.
LAYUP_VERSION="${LAYUP_VERSION:-1.1.0}"

echo "layup setup"
echo "  venv:     $VENV_DIR"
echo "  version:  $LAYUP_VERSION (PyPI)"
echo "  data:     $STAGED_DIR (staged from $DATA_DIR)"
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

# ── Install the pinned release (builds the C extension; pulls sorcha/jax/assist/rebound) ──
echo
echo "Installing layup==$LAYUP_VERSION (this compiles a C extension — may take a few minutes)..."
"${PIP[@]}" "layup==$LAYUP_VERSION"

installed="$("$PY" -c 'from importlib.metadata import version; print(version("layup"))')"
if [ "$installed" != "$LAYUP_VERSION" ]; then
    echo "ERROR: asked for layup $LAYUP_VERSION but the venv holds layup $installed." >&2
    exit 1
fi
if [ ! -x "$VENV_DIR/bin/layup-orbitfit" ]; then
    echo "ERROR: layup $installed installed without its layup-orbitfit entry point." >&2
    exit 1
fi
"$PY" -c 'import layup' || { echo "ERROR: layup $installed does not import." >&2; exit 1; }
echo "layup $installed installed; layup-orbitfit present."

# ── Stage layup's data directory from the registered kernel set ──
# No `layup bootstrap`: it downloads every kernel from the dated address the
# release names, and the fit path re-downloads whatever is missing, so a
# renamed upstream file fails every fit (NAIF's Earth-orientation rename did
# exactly that). stage_data.py builds layup's data directory from the synced
# set alone and stops, naming each file, when the set cannot fill a role.
# `make run-layup` re-stages before every run, so a restored venv picks up the
# set the run actually synced.
echo
"$PY" "$SCRIPT_DIR/stage_data.py" --data-dir "$DATA_DIR" --dest "$STAGED_DIR"

echo
echo "Setup complete. Run the layup OD comparison with:"
echo "  $PY $SCRIPT_DIR/run_layup.py <fixtures/psv> --ar-data-path $STAGED_DIR --output results/validation_layup.json"
