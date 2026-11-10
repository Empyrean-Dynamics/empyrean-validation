#!/usr/bin/env bash
# Set up a self-contained venv with GRSS — the Gauss-Radau Small-body Simulator
# (Makadia et al., github.com/rahil-makadia/grss) — for use as an external
# propagation / ephemeris / orbit-determination reference in the validation
# suite. GRSS is one of only two external references in the suite that ingests
# radar astrometry, so it turns radar OD from a single-witness find_orb
# comparison into a real cross-check.
#
# GRSS is GPL-3.0 and is never linked into empyrean — same rule as ASSIST /
# jorbit / OrbFit: a C++ core behind a Python interface, installed from PyPI
# into this directory's .venv and driven only as a subprocess
# (`make run-grss`).
#
# ── Three things this script does that `pip install grss` does not ──────────
#
# 1. INTERPRETER. GRSS depends on basemap (for its plotting helpers), which
#    caps at Python < 3.14. A 3.14 interpreter fails with an unhelpful
#    "No matching distribution found for basemap>=1.4.0" from the middle of a
#    resolver backtrack, so the version is checked up front and named.
#
# 2. KERNELS. `import grss` calls grss.utils.initialize(), which shells out to
#    the package's own get_kernels.py — and that pulls the FULL kernel set,
#    4.4 GB, including DE441 (3.3 GB) and the long-span SB441-N16 (646 MB),
#    from dated NAIF file names that move. The validation grid spans
#    ~2005-2040, entirely inside DE440's 1550-2650 coverage, so this script
#    stages only the DE440-case set (~235 MB) under GRSS's own names:
#
#        de440.bsp  sb441-n16s.bsp  earth_{latest,historic,predict}.bpc
#        moon_pa_de440.bpc
#
#    (get_kernels.py also fetches pck00011.tpc, which GRSS never opens: no
#    file name in the package's Python or its compiled core names it, so it is
#    not staged.)
#
#    Every kernel comes from GRSS's folder of the third-party tool set
#    (dataset third-party-tools-*, folder `grss/`, synced by the suite under
#    DATA_DIR/third-party; CI restores it before this script runs). The folder
#    lists every file GRSS reads, shared kernels included, so GRSS reads the
#    same bytes the engine does and nothing is ever downloaded: a file the
#    folder does not carry is a hard stop that names the set.
#
#    run_grss.py passes `--de-kernel 440` to match and STAMPS the kernel
#    version into every row's `source_version`, so the report always records
#    which planetary ephemeris produced the comparison. Set
#    GRSS_DE_KERNEL=441 here (and `--de-kernel 441` on the runner) for an
#    exact-DE441 run against empyrean's DE441 + SB441-N16 reference; that needs
#    a tool-set folder carrying de441.bsp and sb441-n16.bsp.
#
#    A missing kernel is never papered over: run_grss.py asserts every file
#    the requested version needs is present and non-empty before it fits or
#    propagates anything, and names this script when one is not.
#
# 3. ASTROMETRY TABLES. GRSS opens three more files under its package
#    directory, which it otherwise downloads at run time: the low-resolution
#    Eggl et al. (2020) debiasing table debias/lowres_data/bias.dat (every
#    optical fit; from JPL SSD's debias_2018.tgz) and the observatory tables
#    fit/codes.json and fit/special_codes.json (written from the MPC
#    observatory-codes API). The same grss/ folder carries all three, so they
#    are copied there too and setup reaches none of JPL, NAIF and the MPC. They
#    are copies, not links: run_grss.py refreshes codes.json's modification
#    time so GRSS's daily re-download never fires.
set -euo pipefail

cd "$(dirname "$0")"

GRSS_VERSION="${GRSS_VERSION:-4.5.7}"
GRSS_DE_KERNEL="${GRSS_DE_KERNEL:-440}"

case "$GRSS_DE_KERNEL" in
    440|441) ;;
    *)
        echo "ERROR: GRSS_DE_KERNEL=$GRSS_DE_KERNEL is not supported." >&2
        echo "       GRSS's PropSimulation takes 440 (DE440 + short-span SB441-N16," >&2
        echo "       1550-2650, ~235 MB) or 441 (DE441 + long-span SB441-N16, ~4.4 GB)." >&2
        exit 1
        ;;
esac

# ── 1. Interpreter ─────────────────────────────────────────
# basemap (a hard grss dependency) publishes no wheel for 3.14+, so an
# out-of-range interpreter is a hard stop with the reason named, not a
# resolver error 200 lines into a pip backtrack.
py_in_range() {
    "$1" -c 'import sys; raise SystemExit(0 if (3, 10) <= sys.version_info[:2] < (3, 14) else 1)' \
        >/dev/null 2>&1
}

PY=""
if [ -n "${GRSS_PYTHON:-}" ]; then
    if ! command -v "$GRSS_PYTHON" >/dev/null 2>&1; then
        echo "ERROR: GRSS_PYTHON=$GRSS_PYTHON is not on PATH." >&2
        exit 1
    fi
    if ! py_in_range "$GRSS_PYTHON"; then
        echo "ERROR: GRSS_PYTHON=$GRSS_PYTHON is $("$GRSS_PYTHON" -c 'import sys;print(".".join(map(str,sys.version_info[:3])))')." >&2
        echo "       GRSS needs 3.10 <= python < 3.14 (its basemap dependency has no 3.14 wheel)." >&2
        exit 1
    fi
    PY="$GRSS_PYTHON"
else
    for cand in python3 python3.13 python3.12 python3.11 python3.10; do
        if command -v "$cand" >/dev/null 2>&1 && py_in_range "$cand"; then
            PY="$cand"
            break
        fi
    done
fi

if [ -z "$PY" ]; then
    echo "ERROR: no usable Python interpreter found for GRSS." >&2
    echo "       GRSS requires 3.10 <= python < 3.14: its basemap dependency publishes" >&2
    echo "       no wheel for 3.14+, and pip reports that as an unrelated-looking" >&2
    echo "       'No matching distribution found for basemap>=1.4.0'." >&2
    echo "       Tried: python3 python3.13 python3.12 python3.11 python3.10" >&2
    if command -v python3 >/dev/null 2>&1; then
        echo "       python3 here is $(python3 -c 'import sys;print(".".join(map(str,sys.version_info[:3])))')." >&2
    fi
    echo "       Install one of the above, or point GRSS_PYTHON at it." >&2
    exit 1
fi

echo "Using $PY ($("$PY" -c 'import sys;print(".".join(map(str,sys.version_info[:3])))'))"

if [ ! -d .venv ]; then
    "$PY" -m venv .venv
fi

VENV_PY="$(pwd)/.venv/bin/python"
"$VENV_PY" -m pip install --upgrade pip
# Pinned. Update intentionally so the comparison reference does not change
# between runs without a reviewed diff.
"$VENV_PY" -m pip install "grss==${GRSS_VERSION}"

# Located without importing it: `import grss` (or `import grss.utils`) runs
# grss.utils.initialize(), which shells out to get_kernels.py and pulls the full
# kernel set from NAIF and the SSD (DE441 included) and replaces a day-old
# earth_latest.bpc with NAIF's current one. find_spec reads the package's
# location and runs none of it.
GRSS_PKG="$("$VENV_PY" -c 'import importlib.util, os; print(os.path.dirname(importlib.util.find_spec("grss").origin))' 2>/dev/null | tail -1)"
if [ -z "$GRSS_PKG" ] || [ ! -d "$GRSS_PKG" ]; then
    echo "ERROR: grss installed but its package directory could not be resolved." >&2
    exit 1
fi
echo "grss package: $GRSS_PKG"

# ── 2. SPICE kernels ───────────────────────────────────────
# Staged here, deliberately, instead of letting `import grss` shell out to
# get_kernels.py — see the header.
KDIR="$GRSS_PKG/kernels"
mkdir -p "$KDIR" "$GRSS_PKG/debias/lowres_data" "$GRSS_PKG/fit"

# GRSS_DATA_DIR is GRSS's folder of the third-party tool set (the Makefile
# passes it; the suite's default is ~/.empyrean/data/third-party/grss). Each
# file is matched by the pattern its NAIF name follows across set versions and
# must match exactly once; a missing or ambiguous file is a hard stop with the
# set named, never a download.
GRSS_DATA_DIR="${GRSS_DATA_DIR:-$HOME/.empyrean/data/third-party/grss}"
if [ ! -d "$GRSS_DATA_DIR" ]; then
    echo "ERROR: GRSS_DATA_DIR=$GRSS_DATA_DIR does not exist." >&2
    echo "       GRSS stages every SPICE kernel from the grss/ folder of the third-party" >&2
    echo "       tool set the suite syncs under DATA_DIR/third-party; sync it first (the CI" >&2
    echo "       prep job does)." >&2
    exit 1
fi
stage_from_set() {  # stage_from_set <pattern> <name under the package> [dir, default kernels]
    local pattern="$1" name="$2" dir="${3:-$KDIR}"
    local matches=()
    while IFS= read -r f; do matches+=("$f"); done < <(find "$GRSS_DATA_DIR" -maxdepth 1 -name "$pattern" \( -type f -o -type l \) | sort)
    if [ "${#matches[@]}" -ne 1 ] || [ ! -s "${matches[0]}" ]; then
        echo "ERROR: expected exactly one non-empty '$pattern' in the third-party tool set's" >&2
        echo "       grss/ folder at $GRSS_DATA_DIR; found ${#matches[@]}. GRSS never downloads a file." >&2
        exit 1
    fi
    local src="${matches[0]}" dest="$dir/$name"
    if [ -f "$dest" ] && cmp -s "$src" "$dest"; then
        printf '  ok       %-22s %s (set)\n' "$name" "$(basename "$src")"
    else
        printf '  staging  %-22s %s (set)\n' "$name" "$(basename "$src")"
        cp -f "$src" "$dest.part" && mv "$dest.part" "$dest"
    fi
}
echo
echo "──── SPICE kernels (DE$GRSS_DE_KERNEL case) → $KDIR ────"
stage_from_set "earth_latest_high_prec.bpc"   "earth_latest.bpc"
stage_from_set "earth_620120_*.bpc"           "earth_historic.bpc"
stage_from_set "earth_*_predict.bpc"          "earth_predict.bpc"
stage_from_set "moon_pa_de440_*.bpc"          "moon_pa_de440.bpc"
if [ "$GRSS_DE_KERNEL" = "441" ]; then
    stage_from_set "sb441-n16.bsp" "sb441-n16.bsp"
    stage_from_set "de441.bsp"     "de441.bsp"
else
    stage_from_set "de440.bsp"      "de440.bsp"
    stage_from_set "sb441-n16s.bsp" "sb441-n16s.bsp"
fi

# ── 3. Astrometry tables ───────────────────────────────────
# Copied from the same folder, never downloaded (see the header).
echo
echo "──── Debiasing and observatory tables → $GRSS_PKG ────"
stage_from_set "bias.dat"           "bias.dat"           "$GRSS_PKG/debias/lowres_data"
stage_from_set "codes.json"         "codes.json"         "$GRSS_PKG/fit"
stage_from_set "special_codes.json" "special_codes.json" "$GRSS_PKG/fit"

echo
echo "grss venv ready: $(pwd)/.venv"
echo "  grss           $(cat "$GRSS_PKG/version.txt")"
echo "  DE kernel case $GRSS_DE_KERNEL"
echo "  kernels        $KDIR"
