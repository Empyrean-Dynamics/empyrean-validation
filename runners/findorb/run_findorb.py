#!/usr/bin/env python3
"""Run find_orb orbit determination on MPC observations for validation comparison.

Fetches observations from MPC, writes PSV files, runs find_orb, and
produces a JSON file that `empyrean validate --findorb-results` can merge
into the validation report.

find_orb is GPL-licensed and never linked into empyrean. This script runs
it as an external tool.

Usage:
    python run_findorb.py --output findorb_results.json
    python run_findorb.py --objects Apophis,Bennu --output findorb_results.json
    python run_findorb.py --populations NEO,MBA --output findorb_results.json

Setup:
    ./setup.sh  # builds find_orb from source

References:
    Based on B612 Asteroid Institute's adam_fo wrapper:
    https://github.com/B612-Asteroid-Institute/adam_fo
"""

import argparse
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request
from datetime import datetime, timezone
from typing import Any, Dict, List, Optional, Tuple

# Local: ADES radar table -> MPC 80-column R/r records. find_orb reads 80-column
# radar natively but cannot parse the ADES <radar> table at all.
from radar_mpc80 import RadarConversionError, convert_radar_psv

# ── Constants ───────────────────────────────────────────

MJD_TO_JD = 2_400_000.5
SCRIPT_DIR = pathlib.Path(__file__).parent
FO_BINARY = SCRIPT_DIR / "install" / "bin" / "fo"
# find_orb's config/support files live directly in build/find_orb (the
# previous build/find_orb/find_orb path never existed, so populate_fo_directory
# silently copied nothing and fo fell back to ~/.find_orb).
FO_FILES_DIR = SCRIPT_DIR / "build" / "find_orb"

# Required find_orb support files
REQUIRED_FILES = [
    "ObsCodes.htm",
    "jpl_eph.txt",
    "orbitdef.sof",
    "rovers.txt",
    "xdesig.txt",
    "cospar.txt",
    "efindorb.txt",
    "odd_name.txt",
    "sigma.txt",
    "mu1.txt",
    "link_def.json",
]


# ── Object catalog ──────────────────────────────────────

class ODObject:
    """Object for OD validation."""
    def __init__(self, name: str, mpc_query: str, population: str, notes: str = ""):
        self.name = name
        self.mpc_query = mpc_query
        self.population = population
        self.notes = notes


ALL_OBJECTS = [
    # Well-observed MBAs
    ODObject("Eros", "433", "MBA", "NEAR target, hundreds of obs"),
    ODObject("Lutetia", "21", "MBA", "Rosetta flyby target"),
    ODObject("Holman", "3666", "MBA", "ASSIST test object"),

    # Well-observed NEOs
    ODObject("Apophis", "99942", "NEO", "2029 Earth CA"),
    ODObject("Bennu", "101955", "NEO", "OSIRIS-REx target"),
    ODObject("Didymos", "65803", "NEO", "DART target"),

    # More NEOs
    ODObject("Farnocchia", "84100", "NEO", "Multi-opposition"),
    ODObject("Duende", "367943", "NEO", "2013 close approach"),

    # Self-perturbers
    ODObject("Vesta", "4", "Self-Perturber", "Dawn target"),
    ODObject("Pallas", "2", "Self-Perturber", "2nd most massive"),
    ODObject("Iris", "7", "Self-Perturber", "SB441-N16 perturber"),

    # Comets (non-grav)
    ODObject("67P", "67P", "Comet", "Rosetta target, non-grav"),
    ODObject("2P/Encke", "2P", "Comet", "Shortest period comet"),
]


# ── Provenance ──────────────────────────────────────────


def resolve_source_version() -> str:
    """Provenance string stamped on every find_orb-channel record.

    find_orb is built from a git checkout under ``build/find_orb`` (see
    setup.sh); report the checked-out commit so the merged report records
    exactly which find_orb build produced the fit. No-hidden-fallbacks: when
    the sha cannot be resolved (build tree absent, git missing), stamp an
    explicit ``find_orb unknown (<reason>)`` rather than a silent blank.
    """
    try:
        proc = subprocess.run(
            ["git", "-C", str(FO_FILES_DIR), "rev-parse", "--short", "HEAD"],
            capture_output=True,
            text=True,
            timeout=10,
        )
    except Exception as e:
        return f"find_orb unknown ({e})"
    if proc.returncode != 0:
        tail = (proc.stderr or "").strip().splitlines()
        reason = tail[-1] if tail else f"git rev-parse rc={proc.returncode}"
        return f"find_orb unknown ({reason})"
    sha = proc.stdout.strip()
    return f"find_orb {sha}" if sha else "find_orb unknown (empty git sha)"


# ── MPC Observation Fetching ────────────────────────────


def fetch_mpc_observations(designation: str) -> Optional[str]:
    """Fetch observations from MPC API and return as ADES PSV string.

    The MPC API returns JSON which we convert to ADES PSV format
    for consumption by both empyrean and find_orb.
    """
    url = f"https://data.minorplanetcenter.net/api/get-obs?designation={urllib.parse.quote(designation)}&return_type=json"
    try:
        req = urllib.request.Request(url, headers={"Accept": "application/json"})
        with urllib.request.urlopen(req, timeout=30) as resp:
            data = json.loads(resp.read())
    except Exception as e:
        print(f"  MPC fetch failed: {e}")
        return None

    if not data:
        return None

    # Convert MPC JSON to ADES PSV
    lines = ["# version=2017"]
    lines.append("permID|trkSub|mode|stn|obsTime|ra|dec|rmsRA|rmsDec|astCat|mag|band")

    for obs in data:
        perm_id = obs.get("number", "")
        trk_sub = obs.get("trksub", obs.get("provid", ""))
        mode = obs.get("mode", "CCD")
        stn = obs.get("stn", "")
        obs_time = obs.get("obstime", obs.get("jd_obs", ""))
        ra = obs.get("ra", "")
        dec = obs.get("dec", "")
        rms_ra = obs.get("rmsRA", "")
        rms_dec = obs.get("rmsDec", "")
        ast_cat = obs.get("catalog", "")
        mag = obs.get("mag", "")
        band = obs.get("band", "")

        # Skip if missing essential fields
        if not stn or not obs_time or not ra or not dec:
            continue

        fields = [
            str(perm_id), str(trk_sub), str(mode), str(stn),
            str(obs_time), str(ra), str(dec), str(rms_ra), str(rms_dec),
            str(ast_cat), str(mag), str(band),
        ]
        lines.append("|".join(fields))

    if len(lines) <= 2:
        return None

    return "\n".join(lines) + "\n"


# ── find_orb Execution ──────────────────────────────────


def populate_fo_directory(working_dir: str, data_dir: Optional[pathlib.Path] = None):
    """Copy required find_orb support files into the working directory."""
    os.makedirs(working_dir, exist_ok=True)

    for fname in REQUIRED_FILES:
        src = FO_FILES_DIR / fname
        if src.exists():
            shutil.copy2(src, os.path.join(working_dir, fname))

    # Write environ.dat
    # Based on: https://github.com/B612-Asteroid-Institute/adam_fo/blob/main/src/adam_fo/environ.dat.tpl
    lines = []

    # JPL ephemeris. Linked into the working directory and named RELATIVELY,
    # never by absolute path: find_orb copies this string into a 94-byte buffer,
    # and CI's workspace nests the checkout three deep, so the absolute path is
    #   /home/runner/work/empyrean-validation/empyrean-validation/empyrean-validation/data/linux_p1550p2650.440
    # — 103 characters, 9 over. On macOS that strlcpy overflow prints a warning
    # and continues; on Linux, where the binary is built with _FORTIFY_SOURCE,
    # it ABORTS, so `fo` died with SIGABRT (rc=134) on every single object and
    # the channel fitted nothing. The runner already uses bare relative names
    # elsewhere for exactly this reason; this was the last absolute path left.
    #
    # A symlink rather than a copy: the DE file is ~100 MB and this runs once
    # per object.
    jpl_path = data_dir / "linux_p1550p2650.440" if data_dir else None
    if jpl_path and jpl_path.exists():
        link = os.path.join(working_dir, "jpl_de.440")
        if not os.path.lexists(link):
            os.symlink(jpl_path.absolute(), link)
        lines.append("LINUX_JPL_FILENAME=jpl_de.440")

    # Perturbers: all planets + Pluto + Moon + asteroid perturbers (hex)
    lines.append('PERTURBERS=1007fe')

    # Asteroid perturbers: SB441-N16 set (matches empyrean/ASSIST)
    lines.append('ASTEROID_PERT_LIST=1,3,4,7,10,15,16,31,52,65,70,87,88,107,511,704')

    # Observation weighting on, auto central object, no EFCC18 debiasing
    lines.append('SETTINGS2=1 22.00 0 -2 0')

    # Use Encke's method
    lines.append('ENCKE=1')

    # 3-sigma outlier rejection
    lines.append('OUTLIER_REJECTION_LIMIT=3')

    with open(os.path.join(working_dir, "environ.dat"), "w") as f:
        f.write('\n'.join(lines) + '\n')


def sanitize_psv(psv: str) -> str:
    """Normalize integer-valued ADES pos1-3 columns to carry a decimal point.

    Space-based / roving observations (TESS C57, WISE C51, rover 247/250/270)
    legally report integer observer positions (e.g. ``pos2=356814`` km), but
    find_orb's ADES converter (``ades2mpc.cpp``) asserts on a position value
    with no decimal point and aborts the whole fit. Appending ``.0`` is
    numerically identical and keeps fo alive.
    """
    lines = psv.strip().split("\n")
    if len(lines) < 2 or "pos1" not in lines[1]:
        return psv
    hdr = [h.strip() for h in lines[1].split("|")]
    idx = [hdr.index(k) for k in ("pos1", "pos2", "pos3") if k in hdr]
    out = lines[:2]
    for ln in lines[2:]:
        cols = ln.split("|")
        for i in idx:
            if i < len(cols):
                v = cols[i].strip()
                if v and "." not in v and "e" not in v.lower():
                    cols[i] = cols[i].replace(v, v + ".0", 1)
        out.append("|".join(cols))
    return "\n".join(out) + "\n"


def _parse_fo_vectors(path: str) -> list:
    """Parse fo's computer-friendly state-vector ephemeris.

    Line format: ``JD_TT  x y z vx vy vz`` — geocentric (code 500),
    equatorial J2000, AU and AU/day, GEOMETRIC (fo applies light-time lag
    only to observables, not state vectors). The first line is a header.
    """
    rows = []
    for ln in open(path).read().strip().split("\n")[1:]:
        parts = ln.split()
        if len(parts) >= 7:
            try:
                rows.append({
                    "jd_tt": float(parts[0]),
                    "pos": [float(parts[1]), float(parts[2]), float(parts[3])],
                    "vel": [float(parts[4]), float(parts[5]), float(parts[6])],
                })
            except ValueError:
                continue
    return rows


def _parse_fo_observables(path: str) -> list:
    """Parse fo's computer-friendly observables ephemeris.

    Line format: ``JD  RA_deg  Dec_deg  delta_AU  r_AU  elong  mag`` with a
    ``#(code) name`` header line. Astrometric (light-time-lagged) RA/Dec.
    """
    rows = []
    for ln in open(path).read().strip().split("\n"):
        if ln.startswith("#"):
            continue
        parts = ln.split()
        if len(parts) >= 5:
            try:
                rows.append({
                    "jd": float(parts[0]),
                    "ra_deg": float(parts[1]),
                    "dec_deg": float(parts[2]),
                    "delta_au": float(parts[3]),
                })
            except ValueError:
                continue
    return rows


def run_findorb(
    psv_string: str,
    data_dir: Optional[pathlib.Path] = None,
    fo_binary: Optional[pathlib.Path] = None,
    ephem_spec: Optional[Dict[str, Any]] = None,
) -> Optional[Dict[str, Any]]:
    """Run find_orb on ADES PSV observations.

    Returns dict with elements, covariance, residuals, or None on failure.
    Based on adam_fo's approach.
    """
    fo_bin = fo_binary or FO_BINARY
    if not fo_bin.exists():
        raise FileNotFoundError(
            f"find_orb binary not found at {fo_bin}. Run ./setup.sh first."
        )

    # Create temporary working directory
    tmp_dir = tempfile.mkdtemp(prefix="fo_empyrean_")
    try:
        populate_fo_directory(tmp_dir, data_dir)

        # Write observations
        obs_file = os.path.join(tmp_dir, "observations.ades")
        with open(obs_file, "w") as f:
            f.write(psv_string)

        # Run find_orb: -c = command line mode
        cmd = f"{fo_bin} {obs_file} -c -d 2 -D {tmp_dir}/environ.dat -O {tmp_dir}"
        t_fit0 = time.perf_counter()
        result = subprocess.run(
            cmd, shell=True, cwd=tmp_dir,
            text=True, capture_output=True, timeout=120,
        )
        fit_ms = (time.perf_counter() - t_fit0) * 1000.0

        if result.returncode != 0:
            # Surface find_orb's OWN diagnostics. Printing only the return code
            # made a channel that failed on all 50 objects undiagnosable from
            # CI: `capture_output=True` swallowed everything fo said about why,
            # leaving `rc=134` and nothing else, which is not a cause. Signal
            # deaths are decoded because 134 is the common one here (SIGABRT)
            # and "rc=134" does not read as "the process was killed".
            rc = result.returncode
            how = f"rc={rc}"
            if rc < 0:
                how = f"killed by signal {-rc}"
            elif rc > 128:
                how = f"rc={rc} (killed by signal {rc - 128})"
            print(f"    find_orb failed ({how})")
            for stream, text in (("stdout", result.stdout), ("stderr", result.stderr)):
                text = (text or "").strip()
                if not text:
                    continue
                lines = text.splitlines()
                shown = lines[-25:]
                if len(lines) > len(shown):
                    print(f"      [{stream}: last {len(shown)} of {len(lines)} lines]")
                else:
                    print(f"      [{stream}]")
                for line in shown:
                    print(f"        {line}")
            return None

        # Parse output files
        total_path = os.path.join(tmp_dir, "total.json")
        covar_path = os.path.join(tmp_dir, "covar.json")

        if not os.path.exists(total_path) or not os.path.exists(covar_path):
            print("    find_orb failed: output files not found")
            return None

        with open(total_path) as f:
            total_json = json.load(f)
        with open(covar_path) as f:
            covar_json = json.load(f)

        # Extract elements for first object
        objects = total_json.get("objects", {})
        if not objects:
            return None

        obj_id = next(iter(objects))
        elements = objects[obj_id].get("elements", {})
        residuals = objects[obj_id].get("observations", {}).get("residuals", [])

        # ── Optional ephemeris stage: state vectors + per-site RA/Dec of the
        # fitted orbit at the plan's exact epochs, in two more fo runs against
        # the already-computed solution (same tmp dir). Note fo's time list is
        # TT while plan epochs are TDB; |TT−TDB| ≤ 1.7 ms (≤ ~70 m at NEO
        # speeds), far below fit-vs-reference differences.
        vectors, observables = None, None
        if ephem_spec:
            # fo's EPHEM_STEPS value is sscanf'd with %79s — an absolute tmp
            # path silently truncates. The fo subprocess runs with
            # cwd=tmp_dir, so a bare relative filename is both short and
            # unambiguous.
            times_path = os.path.join(tmp_dir, "ephem_times.txt")
            with open(times_path, "w") as f:
                f.write("OPTION T\n")
                for dt in ephem_spec["dt_days"]:
                    f.write(f"JD {ephem_spec['epoch_mjd_tdb'] + dt + 2400000.5:.6f}\n")
            with open(os.path.join(tmp_dir, "environ.dat"), "a") as f:
                f.write("EPHEM_STEPS=1 tephem_times.txt\nTT_EPHEMERIS=1\n")
            base = f"{fo_bin} {obs_file} -c -d 2 -D {tmp_dir}/environ.dat -O {tmp_dir}"
            # State vectors (-E 0,17: type 1 = state vectors + computer
            # friendly), geocentric (500) — geometric, equatorial J2000,
            # AU / AU/day. The merge converts to SSB via Earth's state.
            # fo's exit code after an ephemeris pass is unreliable (255 seen
            # with a fully-written output file) — judge by the file contents.
            vec_path = os.path.join(tmp_dir, "ephem_vec.txt")
            rv = subprocess.run(
                f"{base} -e {vec_path} -E 0,17 -C 500",
                shell=True, cwd=tmp_dir, text=True, capture_output=True, timeout=300,
            )
            if os.path.exists(vec_path):
                vectors = _parse_fo_vectors(vec_path)
            if not vectors:
                print(f"    find_orb vector ephemeris produced no rows (rc={rv.returncode})")
            # Observables (-E 17: type 0 + computer friendly) per site.
            codes = ",".join(ephem_spec["obs_codes"])
            obs_tmpl = os.path.join(tmp_dir, "ephem_obs_%c.txt")
            ro = subprocess.run(
                f"{base} -e {obs_tmpl} -E 17 -C {codes}",
                shell=True, cwd=tmp_dir, text=True, capture_output=True, timeout=300,
            )
            observables = {}
            for code in ephem_spec["obs_codes"]:
                p = os.path.join(tmp_dir, f"ephem_obs_{code}.txt")
                if os.path.exists(p):
                    parsed = _parse_fo_observables(p)
                    if parsed:
                        observables[code] = parsed
            if not observables:
                observables = None
                print(f"    find_orb observables ephemeris produced no rows (rc={ro.returncode})")

        return {
            "vectors": vectors,
            "observables": observables,
            "fo_time_ms": fit_ms,
            "elements": elements,
            "covariance_6x6": covar_json.get("covar"),
            "state_vector": covar_json.get("state_vect"),
            "epoch_jd_tt": covar_json.get("epoch"),
            "residuals": residuals,
            "n_obs_total": len(residuals),
            "n_obs_used": sum(1 for r in residuals if r.get("incl", 1) == 1),
            "n_obs_rejected": sum(1 for r in residuals if r.get("incl", 1) == 0),
            "rms_residual": elements.get("rms_residual"),
            "weighted_rms_residual": elements.get("weighted_rms_residual"),
        }

    except subprocess.TimeoutExpired:
        print("    find_orb timed out")
        return None
    finally:
        shutil.rmtree(tmp_dir, ignore_errors=True)


# ── SBDB Fetch (for comparison) ─────────────────────────


def fetch_sbdb_elements(designation: str) -> Optional[Dict[str, Any]]:
    """Fetch SBDB orbital elements for comparison."""
    url = f"https://ssd-api.jpl.nasa.gov/sbdb.api?sstr={urllib.parse.quote(designation)}&phys-par=true&cov=true"
    try:
        with urllib.request.urlopen(url, timeout=15) as resp:
            data = json.loads(resp.read())
    except Exception:
        return None

    orbit = data.get("orbit", {})
    elements = orbit.get("elements", [])

    result = {}
    for elem in elements:
        name = elem.get("name", "")
        value = elem.get("value")
        sigma = elem.get("sigma")
        if value is not None:
            try:
                result[name] = float(value)
            except (ValueError, TypeError):
                result[name] = value
        if sigma is not None:
            try:
                result[f"{name}_sigma"] = float(sigma)
            except (ValueError, TypeError):
                pass

    # Get covariance if available
    cov_data = orbit.get("covariance", {})
    if cov_data:
        result["sbdb_covariance"] = cov_data

    return result if result else None


# ── Main ────────────────────────────────────────────────


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Run find_orb OD on PSV observation files"
    )
    parser.add_argument(
        "psv_dir", type=str,
        help="Directory containing PSV observation files",
    )
    parser.add_argument(
        "--output", "-o", type=str, default="findorb_results.json",
        help="Output JSON file path",
    )
    parser.add_argument(
        "--data-dir", type=str, default=None,
        help="Data directory with JPL ephemeris. Default: ~/.empyrean/data",
    )
    parser.add_argument(
        "--fo-binary", type=str, default=None,
        help="Path to fo binary. Default: scripts/findorb/install/bin/fo",
    )
    parser.add_argument(
        "--test-type", type=str, default="orbit_determination",
        help="Stamp each record's test_type so the merge can route it. Use "
             "'orbit_determination_radar' when fitting the optical+radar "
             "fixtures/psv-radar/ files (find_orb ingests the ADES <radar> table).",
    )
    parser.add_argument(
        "--plan", type=str, default=None,
        help="Validation plan JSON. When given, each fitted orbit is also "
             "propagated by find_orb to the plan's epochs: geocentric state "
             "vectors (propagation axis) and per-site RA/Dec (ephemeris axis) "
             "are emitted alongside the OD row.",
    )
    args = parser.parse_args()

    # Plan-driven ephemeris spec: per-object epoch + dt grid, plus the
    # distinct observatory codes the plan's ephemeris rows use.
    plan_epoch: Dict[str, float] = {}
    plan_dts: Dict[str, list] = {}
    plan_obs_codes: list = []
    if args.plan:
        with open(args.plan) as f:
            plan_rows = json.load(f)
        codes = set()
        for r in plan_rows:
            obj = r.get("object")
            if r.get("test_type") == "propagation":
                plan_epoch[obj] = r.get("epoch_mjd_tdb")
                plan_dts.setdefault(obj, set()).add(r.get("dt_days"))
            elif r.get("test_type") == "ephemeris" and r.get("observer"):
                codes.add(r["observer"])
                plan_dts.setdefault(obj, set()).add(r.get("dt_days"))
        plan_obs_codes = sorted(codes)
        plan_dts = {k: sorted(v) for k, v in plan_dts.items()}
        print(f"  Plan: {len(plan_dts)} objects, observers: {plan_obs_codes}")

    data_dir = pathlib.Path(args.data_dir) if args.data_dir else pathlib.Path.home() / ".empyrean" / "data"
    fo_binary = pathlib.Path(args.fo_binary) if args.fo_binary else FO_BINARY
    psv_dir = pathlib.Path(args.psv_dir)

    # Find all PSV files in the directory
    psv_files = sorted(psv_dir.glob("*.psv"))
    if not psv_files:
        # This used to print and `return` — exit status 0, no output file
        # written. reduce then reported "skip --findorb — not staged (leg
        # failed or was disabled)", which was false on both counts: the leg
        # ran, succeeded, and was enabled. A comparator with nothing to
        # compare is a failure with a cause, and the cause is named here.
        print(
            f"ERROR: no PSV files found in {psv_dir}\n"
            "       find_orb had nothing to fit. The fixtures come from the GCS "
            "snapshot pinned by fixtures/manifest.json;\n"
            "       `make fixtures` fetches + verifies them.",
            file=sys.stderr,
        )
        return 1

    print(f"find_orb OD Validation")
    print(f"  Binary: {fo_binary}")
    print(f"  PSV dir: {psv_dir}")
    print(f"  Files: {len(psv_files)}")
    print()

    timestamp = datetime.now(timezone.utc).isoformat()
    source_version = resolve_source_version()
    results = []
    n_failed = 0

    for psv_path in psv_files:
        name = psv_path.stem  # filename without .psv
        print(f"{name}")

        # Split the fixture's optical and radar tables, converting the radar
        # rows to MPC 80-column R/r records. find_orb's ADES reader cannot take
        # our radar table at all — it aborts in _format_without_decimal even on
        # fixtures our sanitizer never touches — but it reads 80-column radar
        # natively, so the conversion is what makes this comparator possible.
        try:
            optical_psv, radar_lines = convert_radar_psv(name, psv_path.read_text())
        except RadarConversionError as e:
            print(f"  FAIL: radar conversion refused this fixture: {e}")
            n_failed += 1
            results.append(
                {
                    "object": name.replace("_", "/"),
                    "test_type": args.test_type,
                    "timestamp": timestamp,
                    "source_version": source_version,
                    "fo_error": f"radar conversion failed: {e}",
                }
            )
            continue

        # ORDER IS LOAD-BEARING. sanitize_psv is only correct on a SINGLE table:
        # it takes the pos1/pos2/pos3 column indices from the optical header and
        # applies them to every following line, so on a two-table fixture it
        # rewrote the radar header's `com`/`frq` tags to `com.0`/`frq.0`,
        # find_tag returned -1, and find_orb died on
        #   ades2mpc.cpp:1252 check_for_psv_header: assert(psv_tags[n] > 0)
        # — 51 mutated lines on Apophis alone. Sanitize the optical table FIRST,
        # then append the 80-column radar records, which have no `|` and so are
        # out of the sanitizer's reach entirely.
        n_radar = len(radar_lines) // 2
        psv = sanitize_psv(optical_psv)
        if radar_lines:
            psv = psv.rstrip("\n") + "\n" + "".join(ln + "\n" for ln in radar_lines)
        n_optical = len(optical_psv.strip().split("\n")) - 2  # subtract header lines
        n_obs = n_optical + n_radar
        if n_radar:
            print(f"  {n_optical} observations + {n_radar} radar")
        else:
            print(f"  {n_obs} observations")

        # Plan-driven ephemeris spec for this object (optical pass only).
        obj_name = name.replace("_", "/")
        ephem_spec = None
        if args.plan and obj_name in plan_epoch and args.test_type == "orbit_determination":
            ephem_spec = {
                "epoch_mjd_tdb": plan_epoch[obj_name],
                "dt_days": plan_dts[obj_name],
                "obs_codes": plan_obs_codes,
            }

        # Run find_orb
        print("  Running find_orb...")
        fo_result = run_findorb(psv, data_dir, fo_binary, ephem_spec)

        if fo_result is None:
            # Failure ROW, following the OrbFit runner's pattern: the reason
            # travels in a dedicated error field alongside the object's
            # identity, so the merge sees an object that find_orb could not
            # fit rather than an object find_orb was never asked about. The
            # merge reads fo_* with `.as_f64()`, so a row without them folds
            # as all-None — no fabricated numbers.
            print("  FAIL: find_orb produced no solution — emitting failure row")
            print()
            n_failed += 1
            results.append({
                "object": name.replace("_", "/"),
                "test_type": args.test_type,
                "timestamp": timestamp,
                "source_version": source_version,
                "fo_error": "find_orb produced no solution (see log above)",
                "fo_n_obs_total": n_obs,
            })
            continue

        elements = fo_result["elements"]
        print(f"  find_orb: q={elements.get('q', '?')}, e={elements.get('e', '?')}, "
              f"i={elements.get('i', '?')}, rms={fo_result.get('weighted_rms_residual', '?')}\"")
        print(f"  obs: {fo_result['n_obs_used']} used, {fo_result['n_obs_rejected']} rejected")

        result = {
            "object": name.replace("_", "/"),  # undo safe_name encoding
            "test_type": args.test_type,
            "timestamp": timestamp,
            "source_version": source_version,

            # find_orb results
            "fo_elements": elements,
            "fo_covariance_6x6": fo_result["covariance_6x6"],
            "fo_state_vector": fo_result["state_vector"],
            "fo_epoch_jd_tt": fo_result["epoch_jd_tt"],
            "fo_rms_residual": fo_result["rms_residual"],
            "fo_weighted_rms_residual": fo_result["weighted_rms_residual"],
            "fo_n_obs_total": fo_result["n_obs_total"],
            "fo_n_obs_used": fo_result["n_obs_used"],
            "fo_n_obs_rejected": fo_result["n_obs_rejected"],
            "fo_time_ms": fo_result.get("fo_time_ms"),
            "fo_residuals": fo_result["residuals"],
        }
        results.append(result)

        # Ephemeris-stage rows: find_orb's fitted orbit propagated to the
        # plan's epochs. NOTE the semantics — these are fit-then-propagate
        # (find_orb's own fitted orbit), not a replay of the plan's initial
        # conditions like the ASSIST / OpenOrb runners.
        if ephem_spec and fo_result:
            epoch = ephem_spec["epoch_mjd_tdb"]

            def match_dt(jd):
                dt = jd - 2400000.5 - epoch
                best = min(ephem_spec["dt_days"], key=lambda d: abs(d - dt))
                return best if abs(best - dt) < 5e-3 else None

            n_vec = n_obs_rows = 0
            for v in (fo_result.get("vectors") or []):
                dt = match_dt(v["jd_tt"])
                if dt is None:
                    continue
                results.append({
                    "object": obj_name, "test_type": "propagation",
                    "dt_days": dt, "timestamp": timestamp,
                    "source_version": source_version,
                    "fo_geo_pos_au": v["pos"], "fo_geo_vel_au_d": v["vel"],
                })
                n_vec += 1
            for code, obs_rows in (fo_result.get("observables") or {}).items():
                for o in obs_rows:
                    dt = match_dt(o["jd"])
                    if dt is None:
                        continue
                    results.append({
                        "object": obj_name, "test_type": "ephemeris",
                        "dt_days": dt, "observer": code, "timestamp": timestamp,
                        "source_version": source_version,
                        "fo_ra_deg": o["ra_deg"], "fo_dec_deg": o["dec_deg"],
                        "fo_delta_au": o["delta_au"],
                    })
                    n_obs_rows += 1
            print(f"  ephemerides: {n_vec} state vectors, {n_obs_rows} RA/Dec rows")
        print()

    # Save
    output_path = pathlib.Path(args.output)
    output_path.parent.mkdir(parents=True, exist_ok=True)
    with open(output_path, "w") as f:
        try:
            _payload = json.dumps(results, indent=2, default=str, allow_nan=False)
        except ValueError as _e:
            print(
                f"ERROR: refusing to write non-finite values to {args.output}: {_e}\n"
                "       Bare NaN/Infinity is invalid JSON — Rust's serde_json rejects it, so this\n"
                "       whole channel would fail the reduce merge with a line number and no cause.\n"
                "       A quantity that could not be computed must be null.",
                file=sys.stderr,
            )
            raise
        f.write(_payload)

    n_fits = len(psv_files) - n_failed
    print(f"{'=' * 80}")
    print(f"  Results: {output_path}")
    print(f"  {len(results)} rows from {len(psv_files)} fixtures "
          f"({n_fits} fits, {n_failed} failures)")
    print(f"{'=' * 80}")

    # Non-zero only when EVERY object failed — a partial failure set is still
    # useful output and is surfaced per-row via fo_error. Same rule the OrbFit
    # runner applies.
    if psv_files and n_fits == 0:
        print(
            f"ERROR: find_orb fitted NONE of the {len(psv_files)} fixtures. "
            "The channel computed nothing;\n"
            "       every row carries fo_error. See the per-object output above.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
