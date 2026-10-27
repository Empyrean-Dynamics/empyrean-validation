#!/usr/bin/env bash
# Setup find_orb for OD comparison.
#
# Clones and builds find_orb (Bill Gray's orbit determination software)
# along with its dependencies (lunar, jpl_eph, sat_code), each at the commit
# pinned below.
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
# `--config-only` installs the config directory for an already-built binary
# (a cached build on a fresh runner has none) and must be consumed before the
# optional install prefix, which is the first positional argument.
CONFIG_ONLY=0
if [ "${1:-}" = "--config-only" ]; then
    CONFIG_ONLY=1
    shift
fi
PREFIX="${1:-$SCRIPT_DIR/install}"
BUILD_DIR="$SCRIPT_DIR/build"
NPROC="$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)"

# ── Pinned upstream commits ────────────────────────────────────────────────
# The four repositories carry no tags and no releases, so the build is pinned
# to one coordinated commit set, recorded here (the weekly upstream scan,
# .github/workflows/upstream-scan.yml, mirrors FIND_ORB_COMMIT to report
# drift). An unpinned clone builds whatever upstream pushed last, and
# "empyrean agrees with find_orb to X" says little while find_orb floats.
#
# Pinned 2026-10-27 to each repository's newest commit on that date: find_orb,
# lunar and sat_code were last committed together on 2026-10-21, jpl_eph's
# newest is 2026-07-09. The set builds from scratch without a single warning
# under g++ 13.3 on Ubuntu 24.04 x86_64 (the hosted runner's compiler) and
# aarch64, under g++ 15.2 on Ubuntu 26.04 x86_64, and under Apple clang on
# macOS arm64.
#
# To move the pin, change all four together, rebuild from scratch on Linux
# (x86_64 and aarch64) and macOS, fit a fixture, and update the date above.
# The workflow's tool-build cache is keyed on this file's hash, so a pin
# change also retires the cached build of the previous set.
LUNAR_COMMIT=110ab53d46e3e5f1952318360e7bb01cb4884674
JPL_EPH_COMMIT=a73f25e54d02b99b1c0d9a9d6c61acfbb2fa3a26
SAT_CODE_COMMIT=2814ea074e814d0f69b4ec1aab03425ea7a6d342
FIND_ORB_COMMIT=1de55cca12e7a585c9c7384acbb80538ea2afc3c

# find_orb's lsquare.cpp inverts its normal matrix in __float128 unless it sees
# an ARM target, but the guard tests `__aarch64_`, one underscore short of the
# compiler's `__aarch64__`. On aarch64 Linux g++ therefore takes the __float128
# branch, which GCC does not provide on that target, and lsquare.o fails to
# compile ("'__float128' does not name a type", followed by -Werror'd
# unused-parameter warnings in the code that no longer parses). No find_orb
# commit compiles there unaided: GCC has no __float128 on aarch64, and the
# guard has never excluded it (2016: GCC >= 4.6 alone; 2019: + __arm__; 2024:
# + the misspelled name).
# Defining the misspelled name makes the guard select long double, which is
# what upstream intends on ARM (its commit 3caa731). Nothing is downgraded:
# long double on aarch64 Linux is IEEE binary128, the format __float128 has on
# x86_64. The source stays untouched, so the checked-out commit the runner
# records is exactly what was compiled. On macOS, Apple clang reports itself
# as GCC 4.2, below the guard's 4.6 threshold, so the define is inert there.
FO_ADDED_CXXFLAGS=""
case "$(uname -m)" in
    aarch64|arm64) FO_ADDED_CXXFLAGS="-D__aarch64_" ;;
esac

echo "find_orb setup"
echo "  build dir: $BUILD_DIR"
echo "  prefix:    $PREFIX"
echo
pinned_set() {
    echo "  lunar    $LUNAR_COMMIT"
    echo "  jpl_eph  $JPL_EPH_COMMIT"
    echo "  sat_code $SAT_CODE_COMMIT"
    echo "  find_orb $FIND_ORB_COMMIT"
}

mkdir -p "$BUILD_DIR" "$PREFIX/bin"

# A failure anywhere in this toolchain stops the script with a nonzero exit
# and no `fo` binary. find_orb is one of the suite's external OD references; a
# run without it is a different run, and a comparator that drops out of the
# report because its build broke is a hidden fallback. (build_findorb is
# invoked as an `if` condition, so `set -e` is suspended inside it and the
# explicit `|| return 1` guards drive the control flow.)
#
# find_orb REQUIRES a config directory and asserts if it cannot find one
# (see the comment inside). The directory lives in $HOME, outside the cached
# build tree, so a cached build on a fresh runner has no config dir: this runs
# on every build, and `./setup.sh --config-only` runs it alone at run time when
# the binary is already in place.
install_config_dir() {
    echo
    echo "Installing find_orb config directory (~/.find_orb)..."
    (
        cd "$BUILD_DIR/find_orb" || exit 1
        files=$(sed -n '/^INSTALL_FILES[[:space:]]*=/,/[^\\]$/p' makefile \
                | sed 's/^INSTALL_FILES[[:space:]]*=//; s/\\$//' | tr '\n' ' ')
        test -n "$files" || {
            echo "ERROR: could not read INSTALL_FILES from find_orb's makefile." >&2
            exit 1; }
        mkdir -p "$HOME/.find_orb"
        n=0
        for f in $files; do
            for g in $f; do
                [ -f "$g" ] && { cp "$g" "$HOME/.find_orb/"; n=$((n + 1)); }
            done
        done
        # cospar.txt is the specific file the assertion probes for; if it is not
        # there, fo aborts on every fit, so fail here where the cause is obvious.
        test -f "$HOME/.find_orb/cospar.txt" || {
            echo "ERROR: ~/.find_orb/cospar.txt missing after copying $n files." >&2
            echo "       find_orb asserts on a missing config dir (miscell.cpp)." >&2
            exit 1; }
        echo "  installed $n config files"
    ) || return 1
    return 0
}

# Clone <repo> when absent, then check out <commit> detached and delete every
# untracked file, so the tree is exactly the pinned commit and no object built
# from a previous pin links into this one.
checkout_pinned() {
    local repo=$1 commit=$2
    if [ ! -d "$repo/.git" ]; then
        echo "Cloning $repo..."
        git clone --quiet "https://github.com/Bill-Gray/$repo.git" "$repo" || return 1
    fi
    if ! git -C "$repo" cat-file -e "$commit^{commit}" 2>/dev/null; then
        echo "Fetching $repo (pinned commit not in the local clone)..."
        git -C "$repo" fetch --quiet origin || return 1
    fi
    git -C "$repo" -c advice.detachedHead=false checkout --quiet --force --detach "$commit" || {
        echo "ERROR: could not check out $repo at $commit." >&2
        return 1; }
    git -C "$repo" clean -fdxq || return 1
    echo "  $repo @ $(git -C "$repo" rev-parse HEAD)"
}

build_findorb() {
    cd "$BUILD_DIR" || return 1
    echo "Checking out the pinned commits..."
    checkout_pinned lunar "$LUNAR_COMMIT" || return 1
    checkout_pinned jpl_eph "$JPL_EPH_COMMIT" || return 1
    checkout_pinned sat_code "$SAT_CODE_COMMIT" || return 1
    checkout_pinned find_orb "$FIND_ORB_COMMIT" || return 1

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

    # sat_code must INSTALL, not just build: find_orb's elem2tle.cpp does
    # `#include "norad.h"` unconditionally, and that header only reaches
    # ~/include via sat_code's install. `install_lib` is the library+header
    # target (`install` additionally wants sat_id built).
    echo
    echo "Building sat_code library..."
    ( cd "$BUILD_DIR/sat_code" && make -j"$NPROC" libsatell.a && make install_lib ) || return 1

    echo
    echo "Building find_orb...${FO_ADDED_CXXFLAGS:+ (ADDED_CXXFLAGS=$FO_ADDED_CXXFLAGS)}"
    ( cd "$BUILD_DIR/find_orb" \
        && make -j"$NPROC" fo ADDED_CXXFLAGS="$FO_ADDED_CXXFLAGS" \
        && cp fo "$PREFIX/bin/fo" ) || return 1

    # find_orb REQUIRES a config directory and asserts if it cannot find one.
    # miscell.cpp:212 `default_config_dir_name` tries exactly four locations for
    # cospar.txt — ~/.find_orb/, /software/.find_orb/, /root/.find_orb/, then an
    # alt dir — and calls assert() when none opens. It never looks in the
    # process's cwd, so the per-fit working directory the runner stages does not
    # satisfy it: fo died with SIGABRT on every object.
    #
    # Upstream's `make install` does this, but it depends on `all`, which builds
    # the interactive `find_orb` binary and therefore needs ncurses. Nothing
    # here uses that binary, so the data files are copied directly instead and
    # the toolchain keeps needing only what it consumes.
    #
    # INSTALL_FILES is read from the makefile rather than hardcoded, so an
    # upstream addition to the list is picked up instead of silently missing.
    # One entry is a glob (`?findorb.txt`), hence the inner loop.
    install_config_dir || return 1

    return 0
}

if [ "$CONFIG_ONLY" = 1 ]; then
    if [ ! -x "$PREFIX/bin/fo" ] || [ ! -d "$BUILD_DIR/find_orb" ]; then
        echo "ERROR: --config-only needs the built binary ($PREFIX/bin/fo) and the" >&2
        echo "       source tree ($BUILD_DIR/find_orb); run ./setup.sh without flags first." >&2
        exit 1
    fi
    install_config_dir || exit 1
    exit 0
fi

# A binary from an earlier build must not survive a failed one: `make
# setup-findorb` treats the binary as proof the setup completed.
rm -f "$PREFIX/bin/fo"
if ! build_findorb; then
    rm -f "$PREFIX/bin/fo"
    {
        echo
        echo "############################################################"
        echo "ERROR: find_orb build FAILED at the pinned commit set:"
        pinned_set
        echo "find_orb is a required external OD reference, so setup stops"
        echo "here instead of letting the suite run without it. The failing"
        echo "step's output is above."
        echo "############################################################"
    } >&2
    exit 1
fi

echo
echo "find_orb built successfully."
echo "  binary: $PREFIX/bin/fo"
pinned_set

echo
echo "Run OD comparison with:"
echo "  python $SCRIPT_DIR/run_findorb.py --fo-binary $PREFIX/bin/fo <psv_dir> --output findorb_results.json"
