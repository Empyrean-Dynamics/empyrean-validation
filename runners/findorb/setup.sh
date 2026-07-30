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
# unpinned upstream HEAD and aren't release-tagged (all four repos carry
# zero tags and zero releases), so upstream can change under us at any
# time. Pinning to a coordinated commit set is tracked separately as
# bd empyrean-t4bp — it is a validation-of-record concern, since
# "empyrean agrees with find_orb to X" says little while X floats.
#
# It is NOT what keeps this build working: the breakages seen so far have
# been in utility targets find_orb never links, and the fix for those is
# to stop building them (see the library-only builds below).
#
# find_orb is one of several optional external OD
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

    # Build ONLY the libraries find_orb links, never each repo's default
    # target. find_orb's link line is `-llunar -ljpl -lsatell`
    # (find_orb/makefile), so the ~40 command-line utilities in these repos'
    # `all:` targets are pure cost — and one of them breaking takes the whole
    # comparator down with it for no reason. That is exactly what happened:
    # lunar's `jpl_url` utility stopped linking upstream, while liblunar.a
    # itself archived fine immediately afterwards in the same run.
    #
    # Each repo needs a different incantation, so they are not factored:
    #   lunar     `install:` depends on $(LIBLUNAR), which is conditionally
    #             liblunar.a or liblunar.so.1.0.1 — so let make resolve it
    #             rather than naming the file here.
    #   jpl_eph   `install:` has NO prerequisite (it just copies libjpl.a), so
    #             the library must be built explicitly first.
    #   sat_code  build the library only; install is left as it was.
    echo
    echo "Building lunar library..."
    ( cd "$BUILD_DIR/lunar" && make -j"$NPROC" install ) || return 1

    echo
    echo "Building jpl_eph library..."
    ( cd "$BUILD_DIR/jpl_eph" && make -j"$NPROC" libjpl.a && make install ) || return 1

    # sat_code is itself optional even when the rest builds.
    echo
    echo "Building sat_code library..."
    ( cd "$BUILD_DIR/sat_code" && make -j"$NPROC" libsatell.a ) \
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
