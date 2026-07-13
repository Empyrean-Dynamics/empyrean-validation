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
NPROC="$(sysctl -n hw.ncpu 2>/dev/null || nproc 2>/dev/null || echo 4)"

echo "OpenOrb setup"
echo "  build dir: $BUILD_DIR"
echo "  prefix:    $PREFIX"
echo

# oorb is an optional external comparator built from Fortran source. Any
# failure — missing toolchain (gfortran / LAPACK), an upstream change,
# or the ephemeris build — is non-fatal: the function returns non-zero,
# we warn loudly, and leave no binary, so the suite runs without this one
# comparator rather than failing the whole validation. The absence is
# surfaced by the runner skip and by the report, never silently
# substituted. (Invoked as an `if` condition, so `set -e` is suspended
# inside it and the explicit `|| return 1` guards drive the flow.)
build_oorb() {
    if ! command -v gfortran &>/dev/null; then
        echo "gfortran not found on PATH (install gfortran / gcc-gfortran)."
        return 1
    fi

    mkdir -p "$BUILD_DIR" "$PREFIX/bin"

    cd "$BUILD_DIR"
    if [ ! -d oorb ]; then
        echo "Cloning oorb..."
        git clone https://github.com/oorb/oorb.git || return 1
    else
        echo "oorb: already cloned"
        ( cd oorb && git pull --ff-only ) || return 1
    fi

    cd "$BUILD_DIR/oorb"

    # `./configure` writes a make.config that drives the rest of the
    # build. Pass an explicit toolchain ("gfortran") and the "opt" build
    # profile so the resulting binary is optimized rather than a debug
    # build.
    echo
    echo "Configuring oorb..."
    ./configure gfortran opt --prefix="$PREFIX" || return 1

    echo
    echo "Building oorb..."
    make -j"$NPROC" || return 1

    # `make ephem` builds the SPK→DCB conversion that produces the binary
    # JPL ephemeris files oorb expects at runtime. Skip if data is already
    # present. Non-critical even within a successful build.
    if [ ! -f "$BUILD_DIR/oorb/data/de440.dat" ] \
       && [ ! -f "$BUILD_DIR/oorb/data/de430.dat" ]; then
        echo
        echo "Building JPL ephemeris (de430/de440)..."
        make ephem || echo "WARNING: 'make ephem' failed — runner will fall back to bundled JPL data if available"
    fi

    # The `main/oorb` binary is the CLI we drive from Python. Copy it (and
    # its data directory) into the install prefix so the runner can find
    # them without depending on the build tree layout.
    cp "$BUILD_DIR/oorb/main/oorb" "$PREFIX/bin/oorb" || return 1
    mkdir -p "$PREFIX/data"
    cp -R "$BUILD_DIR/oorb/data/." "$PREFIX/data/" || return 1

    local conf="$BUILD_DIR/oorb/main/oorb.conf"
    [ -f "$conf" ] && cp "$conf" "$PREFIX/oorb.conf"
    return 0
}

if build_oorb; then
    echo
    echo "oorb built successfully."
    echo "  binary:    $PREFIX/bin/oorb"
    echo "  data dir:  $PREFIX/data"
else
    echo
    echo "############################################################"
    echo "WARNING: OpenOrb (oorb) build FAILED."
    echo "The oorb external propagation/ephemeris comparison will be"
    echo "SKIPPED; all other channels and comparators are unaffected."
    echo "############################################################"
    echo
    rm -f "$PREFIX/bin/oorb"
fi
