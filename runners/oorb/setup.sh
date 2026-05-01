#!/usr/bin/env bash
# Setup OpenOrb (Fortran) for propagation comparison.
#
# Clones and builds oorb (Granvik et al.'s Fortran orbit-computation
# library) from source. The pip-installable Python wrapper (`pyoorb`)
# is not used here — it requires the same Fortran build environment as
# the upstream binary anyway, and its sdist build script breaks on
# Python 3.12 / macOS arm64. Building the upstream `oorb` CLI directly
# and invoking it as a subprocess is the find_orb pattern; same
# isolation properties.
#
# oorb is GPL-3.0 licensed and never linked into empyrean. This script
# builds it as an external reference tool only.
#
# Usage:
#   ./oorb/setup.sh [--prefix DIR]
#
# Default install prefix: oorb/install/

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PREFIX="${1:-$SCRIPT_DIR/install}"
BUILD_DIR="$SCRIPT_DIR/build"

echo "OpenOrb setup"
echo "  build dir: $BUILD_DIR"
echo "  prefix:    $PREFIX"
echo

# ── Toolchain check ─────────────────────────────────────
# oorb's build script needs gfortran. Surface a friendly error rather
# than letting `make` emit a dozen confusing errors.
if ! command -v gfortran &>/dev/null; then
    echo "ERROR: gfortran not found on PATH."
    echo
    echo "Install with:"
    echo "  macOS:   brew install gcc"
    echo "  Ubuntu:  sudo apt-get install gfortran"
    echo "  Fedora:  sudo dnf install gcc-gfortran"
    exit 1
fi

mkdir -p "$BUILD_DIR" "$PREFIX/bin"

# ── Clone ───────────────────────────────────────────────
cd "$BUILD_DIR"
if [ ! -d oorb ]; then
    echo "Cloning oorb..."
    git clone https://github.com/oorb/oorb.git
else
    echo "oorb: already cloned"
    (cd oorb && git pull --ff-only)
fi

# ── Configure + build ───────────────────────────────────
cd "$BUILD_DIR/oorb"

# `./configure` writes a make.config that drives the rest of the build.
# Pass an explicit toolchain ("gfortran") and the "opt" build profile
# so the resulting binary is optimized rather than a debug build.
echo
echo "Configuring oorb..."
./configure gfortran opt --prefix="$PREFIX"

echo
echo "Building oorb..."
make -j"$(sysctl -n hw.ncpu 2>/dev/null || nproc 2>/dev/null || echo 4)"

# `make ephem` builds the SPK→DCB conversion that produces the binary
# JPL ephemeris files oorb expects at runtime. Skip if data is already
# bundled or has been downloaded.
if [ ! -f "$BUILD_DIR/oorb/data/de440.dat" ] \
   && [ ! -f "$BUILD_DIR/oorb/data/de430.dat" ]; then
    echo
    echo "Building JPL ephemeris (de430/de440)..."
    make ephem || echo "WARNING: 'make ephem' failed — runner will fall back to bundled JPL data if available"
fi

# ── Install ─────────────────────────────────────────────
# The `main/oorb` binary is the CLI we drive from Python. Copy it (and
# its data directory) into the install prefix so the runner can find
# them without depending on the build tree layout.
cp "$BUILD_DIR/oorb/main/oorb" "$PREFIX/bin/oorb"
mkdir -p "$PREFIX/data"
cp -R "$BUILD_DIR/oorb/data/." "$PREFIX/data/"

OORB_BIN="$PREFIX/bin/oorb"
OORB_DATA="$PREFIX/data"
OORB_CONF="$BUILD_DIR/oorb/main/oorb.conf"
if [ -f "$OORB_CONF" ]; then
    cp "$OORB_CONF" "$PREFIX/oorb.conf"
fi

echo
echo "oorb built successfully."
echo "  binary:    $OORB_BIN"
echo "  data dir:  $OORB_DATA"
echo "  conf file: $PREFIX/oorb.conf"
echo
echo "Run with:"
echo "  python3 $SCRIPT_DIR/run_oorb.py --input validation_plan.json --output validation_oorb.json"
