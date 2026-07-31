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
# ── Two things this script does that `pip install grss` does not ────────────
#
# 1. INTERPRETER. GRSS depends on basemap (for its plotting helpers), which
#    caps at Python < 3.14. A 3.14 interpreter fails with an unhelpful
#    "No matching distribution found for basemap>=1.4.0" from the middle of a
#    resolver backtrack, so the version is checked up front and named.
#
# 2. KERNELS. `import grss` calls grss.utils.initialize(), which shells out to
#    the package's own get_kernels.py — and that pulls the FULL kernel set,
#    4.4 GB, including DE441 (3.3 GB) and the long-span SB441-N16 (646 MB).
#    The validation grid spans ~2005-2040, entirely inside DE440's 1550-2650
#    coverage, so this script fetches only the DE440-case set (~235 MB):
#
#        de440.bsp  sb441-n16s.bsp  earth_{latest,historic,predict}.bpc
#        moon_pa_de440.bpc  pck00011.tpc
#
#    run_grss.py passes `--de-kernel 440` to match and STAMPS the kernel
#    version into every row's `source_version`, so the report always records
#    which planetary ephemeris produced the comparison. Set
#    GRSS_DE_KERNEL=441 here (and `--de-kernel 441` on the runner) for an
#    exact-DE441 run against empyrean's DE441 + SB441-N16 reference; that
#    costs the full 4.4 GB download.
#
#    A missing kernel is never papered over: run_grss.py asserts every file
#    the requested version needs is present and non-empty before it fits or
#    propagates anything, and names this script when one is not.
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

GRSS_PKG="$("$VENV_PY" -c 'import os,grss.utils as u; print(u.grss_path)' 2>/dev/null | tail -1)"
if [ -z "$GRSS_PKG" ] || [ ! -d "$GRSS_PKG" ]; then
    echo "ERROR: grss installed but its package directory could not be resolved." >&2
    exit 1
fi
echo "grss package: $GRSS_PKG"

# ── 2. SPICE kernels ───────────────────────────────────────
# Fetched here, deliberately, instead of letting `import grss` shell out to
# get_kernels.py — see the header. Downloads are atomic (temp file, then
# rename) and size-verified against the server's Content-Length on every
# invocation, cache hit or miss, so a truncated kernel can never be silently
# reused by a fit. Mirrors the fixture-fetch discipline in
# scripts/fetch-fixtures.sh.
NAIF="https://naif.jpl.nasa.gov/pub/naif/generic_kernels"
SSD="https://ssd.jpl.nasa.gov/ftp"

KERNELS=(
    "$NAIF/pck/earth_latest_high_prec.bpc|earth_latest.bpc"
    "$NAIF/pck/earth_620120_250826.bpc|earth_historic.bpc"
    "$NAIF/pck/earth_2025_250826_2125_predict.bpc|earth_predict.bpc"
    "$NAIF/pck/moon_pa_de440_200625.bpc|moon_pa_de440.bpc"
    "$NAIF/pck/pck00011.tpc|pck00011.tpc"
)
if [ "$GRSS_DE_KERNEL" = "441" ]; then
    KERNELS+=(
        "$SSD/eph/planets/bsp/de441.bsp|de441.bsp"
        "$SSD/eph/small_bodies/asteroids_de441/sb441-n16.bsp|sb441-n16.bsp"
    )
else
    KERNELS+=(
        "$SSD/eph/planets/bsp/de440.bsp|de440.bsp"
        "$SSD/xfr/sb441-n16s.bsp|sb441-n16s.bsp"
    )
fi

KDIR="$GRSS_PKG/kernels"
mkdir -p "$KDIR"

echo
echo "──── SPICE kernels (DE$GRSS_DE_KERNEL case) → $KDIR ────"
for entry in "${KERNELS[@]}"; do
    url="${entry%%|*}"
    name="${entry##*|}"
    dest="$KDIR/$name"

    remote_size="$(curl -sSIL --retry 3 --retry-delay 2 "$url" \
        | tr -d '\r' | awk 'tolower($1) == "content-length:" { n = $2 } END { print n }')"
    if [ -z "$remote_size" ] || [ "$remote_size" -le 0 ] 2>/dev/null; then
        echo "ERROR: could not read Content-Length for $name from $url" >&2
        echo "       Refusing to guess whether a local copy is complete." >&2
        exit 1
    fi

    if [ -f "$dest" ]; then
        local_size="$(wc -c < "$dest" | tr -d ' ')"
        if [ "$local_size" = "$remote_size" ]; then
            printf '  ok       %-22s %s bytes\n' "$name" "$remote_size"
            continue
        fi
        printf '  stale    %-22s local %s != remote %s bytes; refetching\n' \
            "$name" "$local_size" "$remote_size"
    fi

    printf '  fetching %-22s %s bytes\n' "$name" "$remote_size"
    curl -fsSL --retry 3 --retry-delay 2 -o "$dest.part" "$url"
    got="$(wc -c < "$dest.part" | tr -d ' ')"
    if [ "$got" != "$remote_size" ]; then
        rm -f "$dest.part"
        echo "ERROR: $name downloaded $got bytes, expected $remote_size." >&2
        echo "       A partial kernel would silently corrupt every GRSS state." >&2
        exit 1
    fi
    mv "$dest.part" "$dest"
done

# ── 3. Astrometry auxiliary data ───────────────────────────
# Eggl et al. (2020) star-catalog debiasing tables + the MPC observatory-code
# table. Fetched HERE so `make run-grss` needs no network: grss.utils
# .initialize() re-downloads codes.json when it is over a day old, but skips
# the whole step when offline, so the run-time path only works if setup left
# a usable copy behind.
echo
echo "──── Debiasing tables (Eggl et al. 2020) ────────────────"
"$VENV_PY" "$GRSS_PKG/debias/get_debiasing_data.py"
test -f "$GRSS_PKG/debias/lowres_data/bias.dat" || {
    echo "ERROR: debiasing table $GRSS_PKG/debias/lowres_data/bias.dat is missing." >&2
    echo "       GRSS's optical weighting scheme reads it on every fit." >&2
    exit 1
}

echo
echo "──── MPC observatory codes ──────────────────────────────"
"$VENV_PY" -c "
import os, sys
sys.path.insert(0, '$(dirname "$GRSS_PKG")')
# Importing grss runs grss.utils.initialize(), which shells out to the
# package's own get_kernels.py / get_debiasing_data.py. This script owns both
# fetches (see the header), so neuter os.system for the duration of the import
# rather than letting it emit two 'sh: python: command not found' lines into an
# otherwise clean setup log — or, worse, succeed and pull the 4.4 GB set.
# run_grss.py does the same thing, and reports what it blocked.
_real_system = os.system
os.system = lambda cmd: 0
from grss.utils import _download_codes_file
os.system = _real_system
_download_codes_file()
"
test -s "$GRSS_PKG/fit/codes.json" || {
    echo "ERROR: $GRSS_PKG/fit/codes.json is missing or empty." >&2
    exit 1
}

echo
echo "grss venv ready: $(pwd)/.venv"
echo "  grss           $("$VENV_PY" -c 'import grss; print(grss.__version__)' 2>/dev/null | tail -1)"
echo "  DE kernel case $GRSS_DE_KERNEL"
echo "  kernels        $KDIR"
