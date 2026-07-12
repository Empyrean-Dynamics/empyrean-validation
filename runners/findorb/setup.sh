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

echo "find_orb setup"
echo "  build dir: $BUILD_DIR"
echo "  prefix:    $PREFIX"
echo

mkdir -p "$BUILD_DIR" "$PREFIX/bin"

# ── Clone repositories ──────────────────────────────────
cd "$BUILD_DIR"

for repo in lunar jpl_eph sat_code find_orb; do
    if [ ! -d "$repo" ]; then
        echo "Cloning $repo..."
        git clone "https://github.com/Bill-Gray/$repo.git"
    else
        echo "$repo: already cloned"
        cd "$repo" && git pull --ff-only && cd ..
    fi
done

# ── Build and install lunar (dependency) ─────────────────
echo
echo "Building lunar library..."
cd "$BUILD_DIR/lunar"
make -j$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)
make install
cd ..

# ── Build and install jpl_eph (dependency) ───────────────
echo
echo "Building jpl_eph library..."
cd "$BUILD_DIR/jpl_eph"
make -j$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)
make install
cd ..

# ── Build sat_code (optional dependency) ────────────────
echo
echo "Building sat_code library..."
cd "$BUILD_DIR/sat_code"
make -j$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4) || {
    echo "Warning: sat_code build failed (non-critical, continuing)"
}
cd ..

# ── Build find_orb (command-line version) ───────────────
echo
echo "Building find_orb..."
cd "$BUILD_DIR/find_orb"

# Build the command-line version (fo), not the GUI. find_orb and its
# dependencies (lunar/jpl_eph/sat_code) are cloned at upstream HEAD and
# are not release-tagged, so a version skew between the four repos can
# break this build (e.g. a symbol jpl_url.c references that liblunar.a
# doesn't yet export). find_orb is one of several optional external OD
# references, so treat a build failure as non-fatal — like sat_code
# above — and let the suite run without this comparator rather than
# failing the whole validation. The missing channel is surfaced by the
# runner skip below and by its absence from the report (not silently
# substituted).
if make -j$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4) fo \
    && cp fo "$PREFIX/bin/fo"; then
    echo
    echo "find_orb built successfully."
    echo "  binary: $PREFIX/bin/fo"
    echo
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
echo "Test with:"
echo "  $PREFIX/bin/fo --help"
echo
echo "Run OD comparison with:"
echo "  python $SCRIPT_DIR/run_findorb.py --findorb-bin $PREFIX/bin/fo --input observations.psv --output findorb_results.json"
