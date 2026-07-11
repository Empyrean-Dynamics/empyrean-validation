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
import tempfile
import urllib.parse
import urllib.request
from datetime import datetime, timezone
from typing import Any, Dict, List, Optional, Tuple

# ── Constants ───────────────────────────────────────────

MJD_TO_JD = 2_400_000.5
SCRIPT_DIR = pathlib.Path(__file__).parent
FO_BINARY = SCRIPT_DIR / "install" / "bin" / "fo"
FO_FILES_DIR = SCRIPT_DIR / "build" / "find_orb" / "find_orb"

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

    # JPL ephemeris path
    jpl_path = data_dir / "linux_p1550p2650.440" if data_dir else None
    if jpl_path and jpl_path.exists():
        lines.append(f'LINUX_JPL_FILENAME={jpl_path.absolute()}')

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


def run_findorb(
    psv_string: str,
    data_dir: Optional[pathlib.Path] = None,
    fo_binary: Optional[pathlib.Path] = None,
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
        result = subprocess.run(
            cmd, shell=True, cwd=tmp_dir,
            text=True, capture_output=True, timeout=120,
        )

        if result.returncode != 0:
            print(f"    find_orb failed (rc={result.returncode})")
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

        return {
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


def main():
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
    args = parser.parse_args()

    data_dir = pathlib.Path(args.data_dir) if args.data_dir else pathlib.Path.home() / ".empyrean" / "data"
    fo_binary = pathlib.Path(args.fo_binary) if args.fo_binary else FO_BINARY
    psv_dir = pathlib.Path(args.psv_dir)

    # Find all PSV files in the directory
    psv_files = sorted(psv_dir.glob("*.psv"))
    if not psv_files:
        print(f"No PSV files found in {psv_dir}")
        return

    print(f"find_orb OD Validation")
    print(f"  Binary: {fo_binary}")
    print(f"  PSV dir: {psv_dir}")
    print(f"  Files: {len(psv_files)}")
    print()

    timestamp = datetime.now(timezone.utc).isoformat()
    results = []

    for psv_path in psv_files:
        name = psv_path.stem  # filename without .psv
        print(f"{name}")

        psv = psv_path.read_text()
        n_obs = len(psv.strip().split("\n")) - 2  # subtract header lines
        print(f"  {n_obs} observations")

        # Run find_orb
        print("  Running find_orb...")
        fo_result = run_findorb(psv, data_dir, fo_binary)

        if fo_result is None:
            print("  SKIP: find_orb failed")
            print()
            continue

        elements = fo_result["elements"]
        print(f"  find_orb: q={elements.get('q', '?')}, e={elements.get('e', '?')}, "
              f"i={elements.get('i', '?')}, rms={fo_result.get('weighted_rms_residual', '?')}\"")
        print(f"  obs: {fo_result['n_obs_used']} used, {fo_result['n_obs_rejected']} rejected")

        result = {
            "object": name.replace("_", "/"),  # undo safe_name encoding
            "test_type": args.test_type,
            "timestamp": timestamp,

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
            "fo_residuals": fo_result["residuals"],
        }
        results.append(result)
        print()

    # Save
    output_path = pathlib.Path(args.output)
    output_path.parent.mkdir(parents=True, exist_ok=True)
    with open(output_path, "w") as f:
        json.dump(results, f, indent=2, default=str)

    print(f"{'=' * 80}")
    print(f"  Results: {output_path}")
    print(f"  {len(results)} objects processed")
    print(f"{'=' * 80}")


if __name__ == "__main__":
    main()
