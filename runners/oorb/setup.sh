#!/usr/bin/env bash
# Build OpenOrb (Fortran) at a pinned commit, for propagation comparison.
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
# Data: this script builds the binary and nothing else. Everything oorb
# reads at run time (de440.dat, OBSCODE.dat, ET-UT.dat, TAI-UTC.dat and the
# BC430 asteroid files) comes from the oorb/ folder of the third-party tool
# set, staged by run_oorb.py into a private OORB_DATA directory. `make
# ephem`, which downloads JPL's ASCII ephemeris, is never run.
#
# Usage:
#   ./oorb/setup.sh [PREFIX]          # default prefix: oorb/install/
#   OORB_COMMIT=<sha> ./oorb/setup.sh # another commit (deliberately)
#
# A failed build fails this script: the leg only runs when asked for.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PREFIX="${1:-$SCRIPT_DIR/install}"
BUILD_DIR="$SCRIPT_DIR/build"
NPROC="$(sysctl -n hw.ncpu 2>/dev/null || nproc 2>/dev/null || echo 4)"
# Pinned: the commit whose OBSCODE.dat, ET-UT.dat and TAI-UTC.dat the oorb/
# folder of the third-party tool set carries (master of 2026-08-20).
OORB_COMMIT="${OORB_COMMIT:-02278424f6c13a5c653a5e3ab021aa4ccc537966}"

echo "OpenOrb setup"
echo "  build dir: $BUILD_DIR"
echo "  prefix:    $PREFIX"
echo "  commit:    $OORB_COMMIT"
echo

command -v gfortran >/dev/null || { echo "ERROR: gfortran not found on PATH (install gfortran / gcc-gfortran)." >&2; exit 1; }

mkdir -p "$BUILD_DIR" "$PREFIX/bin"
cd "$BUILD_DIR"
if [ ! -d oorb ]; then
    echo "Cloning oorb..."
    git clone --quiet https://github.com/oorb/oorb.git
fi
cd "$BUILD_DIR/oorb"
if ! git cat-file -e "${OORB_COMMIT}^{commit}" 2>/dev/null; then
    git fetch --quiet origin
fi
git checkout --quiet --detach "$OORB_COMMIT"
head="$(git rev-parse HEAD)"
if [ "$head" != "$OORB_COMMIT" ]; then
    echo "ERROR: oorb checkout is at $head, not the pinned $OORB_COMMIT." >&2
    exit 1
fi

# `./configure` writes a make.config that drives the rest of the build:
# an explicit toolchain ("gfortran") and the optimized "opt" profile.
echo "Configuring oorb..."
./configure gfortran opt --prefix="$PREFIX"
echo "Building oorb..."
make -j"$NPROC"

# The `main/oorb` binary is the CLI the runner drives, and the shipped
# oorb.conf is its configuration (the runner sets the dynamical model and
# the perturbers from it, per row).
cp "$BUILD_DIR/oorb/main/oorb" "$PREFIX/bin/oorb"
cp "$BUILD_DIR/oorb/main/oorb.conf" "$PREFIX/oorb.conf"
printf '%s\n' "$OORB_COMMIT" > "$PREFIX/COMMIT"

echo
echo "oorb built at $OORB_COMMIT."
echo "  binary: $PREFIX/bin/oorb"
echo "  conf:   $PREFIX/oorb.conf"
