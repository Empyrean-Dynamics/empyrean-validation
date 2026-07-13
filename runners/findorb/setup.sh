#!/usr/bin/env bash
# Setup find_orb for OD comparison.
#
# Clones and builds find_orb (Bill Gray's orbit determination software)
# along with its dependencies (lunar, jpl_eph, sat_code).
#
# find_orb is GPL-licensed and never linked into empyrean. This script
# builds it as an external tool for comparison purposes only.
#
# Usage:
#   ./scripts/findorb/setup.sh [--prefix DIR]
#
# Default install prefix: scripts/findorb/install/

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PREFIX="${1:-$SCRIPT_DIR/install}"
BUILD_DIR="$SCRIPT_DIR/build"
NPROC="$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)"

echo "find_orb setup"
echo "  build dir: $BUILD_DIR"
echo "  prefix:    $PREFIX"
echo

mkdir -p "$BUILD_DIR" "$PREFIX/bin"

# find_orb and its dependencies (lunar/jpl_eph/sat_code) are cloned at
# unpinned upstream HEAD and aren't release-tagged, so a version skew
# between the four Bill-Gray repos periodically breaks the from-source
# build (e.g. jpl_url.c in lunar references a symbol liblunar.a doesn't
# yet export). find_orb is one of several optional external OD
# references, so any failure anywhere in this toolchain is non-fatal:
# the function returns non-zero, we warn loudly, and leave no `fo`
# binary — so the suite runs without this one comparator rather than
# failing the whole validation. The absence is surfaced by the runner
# skip and by the report, never silently substituted. (Invoked as an
# `if` condition, so `set -e` is suspended inside it and the explicit
# `|| return 1` guards drive the control flow.)
build_findorb() {
    # Clone / update the four repos.
    cd "$BUILD_DIR"
    for repo in lunar jpl_eph sat_code find_orb; do
        if [ ! -d "$repo" ]; then
            echo "Cloning $repo..."
            git clone "https://github.com/Bill-Gray/$repo.git" || return 1
        else
            echo "$repo: already cloned"
            ( cd "$repo" && git pull --ff-only ) || return 1
        fi
    done

    echo
    echo "Building lunar library..."
    ( cd "$BUILD_DIR/lunar" && make -j"$NPROC" && make install ) || return 1

    echo
    echo "Building jpl_eph library..."
    ( cd "$BUILD_DIR/jpl_eph" && make -j"$NPROC" && make install ) || return 1

    # sat_code is itself optional even when the rest builds.
    echo
    echo "Building sat_code library..."
    ( cd "$BUILD_DIR/sat_code" && make -j"$NPROC" ) \
        || echo "Warning: sat_code build failed (non-critical, continuing)"

    echo
    echo "Building find_orb..."
    ( cd "$BUILD_DIR/find_orb" && make -j"$NPROC" fo && cp fo "$PREFIX/bin/fo" ) || return 1

    return 0
}

if build_findorb; then
    echo
    echo "find_orb built successfully."
    echo "  binary: $PREFIX/bin/fo"
else
    echo
    echo "############################################################"
    echo "WARNING: find_orb build FAILED (upstream HEAD version skew)."
    echo "The find_orb external OD comparison will be SKIPPED; all other"
    echo "channels and comparators are unaffected. Pin the four Bill-Gray"
    echo "repos to a compatible commit set to restore it."
    echo "############################################################"
    echo
    rm -f "$PREFIX/bin/fo"
fi

echo
echo "Run OD comparison with:"
echo "  python $SCRIPT_DIR/run_findorb.py --fo-binary $PREFIX/bin/fo <psv_dir> --output findorb_results.json"
