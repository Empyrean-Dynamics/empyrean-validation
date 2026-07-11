#!/usr/bin/env python3
"""OrbFit external-reference runner for the empyrean validation suite.

Reads the canonical test plan (`validation_plan.json`) and replays each
orbit-determination row through the OrbFit Fortran pipeline packaged in
the MPC's `minorplanetcenter/orbfit` Docker container (the Smithsonian
`mpc-orbfit` source repo, fitobs / neofit2 / neocp_prelim / comets_od
binaries under `/sa/god_fit/bin/`). Emits one row per input OD row with
OrbFit-specific fields populated. Output JSON is consumed by the
empyrean-validation report renderer alongside ASSIST, OpenOrb, find_orb
(and optionally kete + jorbit).

Schema mirrors the ValidationResult shape used by every other channel.
OrbFit-specific fields:

    orbfit_orbit_pos_au      — fitted Cartesian state at OrbFit's epoch
    orbfit_orbit_vel_au_d    — fitted Cartesian velocity
    orbfit_epoch_mjd_tdb     — fit epoch (MJD TDT; treated as ≈ TDB at
                                solar-system scales, sub-ms offset)
    orbfit_covariance_6x6    — fitted 6×6 covariance (equinoctial)
    orbfit_rms_arcsec        — weighted RMS of post-fit residuals
                                (`RMSast` from the .rwo header)
    orbfit_n_obs_total       — observations submitted
    orbfit_n_obs_used        — observations accepted (SEL flag = 1)
    orbfit_n_obs_rejected    — observations rejected (SEL flag = 0)
    orbfit_time_ms           — wall-clock per row

Propagation and ephemeris rows are skipped — OrbFit's natural domain is
OD, matching the pattern of `run_findorb.py`. ASSIST is the external
reference for propagation; Horizons for ephemeris.

OrbFit is GPL-3.0 licensed and never linked into empyrean. This runner
shells out to the upstream container; see `setup.sh` for the pull.

═══════════════════════════════════════════════════════════════════════
Container interface (probed against minorplanetcenter/orbfit:latest)
═══════════════════════════════════════════════════════════════════════

ENTRYPOINT: /bin/bash
WORKDIR:    /sa/god_fit
PLATFORM:   linux/amd64 (Apple Silicon hosts: pass --platform linux/amd64)
BINARIES:   bin/{fitobs.x, neofit2.x, neocp_prelim.x, comets_od.x}

fitobs.x is interactive — it reads menu choices from stdin. The
canonical invocation pattern (from /sa/god_fit/unit_tests/test_fitobs.py):

    cd <workdir>
    ln -sf ../bin/fitobs.x .
    ./fitobs.x < <desig>.rnc

Required input files in `<workdir>`:

    <desig>.fop            Fit-option template (Fortran namelist-ish).
                            References:
                              .obsdir0 = 'mpcobs'    ← where the
                                                       observations live
                              .elefi0  = 'epoch/<desig>.eq0'  ← seed
                                                                 orbit
                              .error_model = 'gaiaDR2_mix'
                              .ecclim, .samax, .ahmax  (eccentricity /
                                                        semi-major-axis
                                                        cuts)
                              propag.{irel, ilun, iast=17, filbe='AST17',
                                      npoint, dmea, dter, ...}

    <desig>.rnc            Menu sequence fed to fitobs.x via stdin:
                              line 1: <desig>
                              line 2: 3   (= "differential corrections")
                              line 3: 1   (sub-option)
                              line 4: 0   (exit)

    mpcobs/<desig>.obs      MPC80 observations, OR
    mpcobs/<desig>.psv      ADES PSV, OR
    mpcobs/<desig>.ades     ADES XML
                            (fitobs auto-detects from extension)

    epoch/<desig>.eq0       Seed Cartesian orbit at epoch (required
                            unless we run IOD first via fitobs menu
                            option 2 = "acquire orbital elements")

Output files (in `<workdir>`):

    <desig>.fel            Fitted equinoctial elements + 6×6 covariance.
                            Format `OEF2.0`, ML record:
                              EQU  <a> <h> <k> <p> <q> <lambda>  (deg)
                              MJD  <epoch> TDT
                              MAG  <H> <G>
                              LSP  0  0  6     (non-grav: none)
                              COV  ... (21 values, upper triangle row-major)
                              NOR  ... (21 values, normal matrix)
                              MOID, NODES (derived)

    mpcobs/<desig>.rwo     Weighted residuals.
                            Header: `RMSast`, `RMSmag`, `errmod`
                            Body: one row per observation with SEL flag,
                                   bias, residual, AT/AC residual.

    <desig>.err            Error log
    <desig>.fou            Verbose fit log
    <desig>.rms            RMS summary

The COV block on the .fel file gives equinoctial elements covariance.
empyrean reports Cartesian — converting requires the Jacobian from
equinoctial to Cartesian, which empyrean already implements. For the
runner, we'll emit both: the raw equinoctial + cov from OrbFit, AND
the Cartesian state derived via empyrean's conversion at the same
epoch. The report compares Cartesian-vs-Cartesian.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Dict, Optional

_AU_KM = 149_597_870.700

# Container identity. Override via --image for testing alternate tags
# (e.g. a Federica Spoto personal build) without editing source.
DEFAULT_IMAGE = "minorplanetcenter/orbfit:latest"

# OrbFit's WORKDIR inside the container. The bin/ symlinks are relative
# to this, so we mount our scratch dir as a subdirectory of it.
CONTAINER_WORKDIR = "/sa/god_fit"

# Apple Silicon hosts: the MPC image is amd64-only; force emulation.
DEFAULT_PLATFORM = "linux/amd64"

# ── .fop template ───────────────────────────────────────────────────
# Mirrors the unit_tests/105.fop template (NEA, MBA flavour). For
# comet OD we'd want a different template (comets_od.x rather than
# fitobs.x), but the catalog this runner exercises is primarily NEAs +
# Trojans + KBOs which all live in fitobs.x's regime.
FOP_TEMPLATE = """\
fitobs.
\t.astna0='{desig}'
\t.obsdir0='mpcobs'
\t.elefi0='epoch/{desig}.eq0'
\t.ecclim=0.99d0
\t.samax=2000.
\t.ahmax=6000.
\t.error_model='gaiaDR2_mix'
\t.gaia_mpc=.FALSE.
IERS.
\t.extrapolation=.T.
propag.
\t.irel=1
\t.ilun=1
\t.iast=17
\t.filbe='AST17'
\t.npoint=600
\t.dmea=0.2d0
\t.dter=0.05d0
"""

# .rnc menu sequence for fitobs.x: input the seed orbit from a
# pre-written .eq0 file, then run DC with CMC2003 autoreject.
#
# Menu navigation (decoded from /sa/god_fit/src/fitobs/fitobs.f90):
#
#   <desig>   designation prompt
#   2         mainmenu: acquire orbital elements
#   1         inputele: input arc 1 (reads epoch/<desig>.eq0)
#   3         mainmenu: differential corrections
#   1         difcomod: correct all, autoreject (CMC2003)
#   0         exit DC menu / return to main
#   0         exit main menu (terminate cleanly)
#
# This mirrors OrbFit's production usage: refine an existing orbit
# against new observations, rather than re-IOD from scratch. Blind
# Gauss IOD on a multi-year arc is filtered as "bizarre orbits" by
# OrbFit's reasonability checks — it's not the intended path.
# Seed comes from empyrean's plan row (Cartesian, ICRF) converted to
# OrbFit's OEF2.0 ecliptic-J2000 format by `write_eq0_seed()` below.
RNC_TEMPLATE = "{desig}\n2\n1\n3\n1\n0\n0\n"

# Obliquity at J2000.0 (IAU 1976) — empyrean's ICRF Cartesian states
# rotate to OrbFit's ECLM J2000 by R_x(-ε):
#   x_ecl =  x_icrf
#   y_ecl =  cos(ε)·y_icrf + sin(ε)·z_icrf
#   z_ecl = -sin(ε)·y_icrf + cos(ε)·z_icrf
# Same rotation applies to velocity. ε is fixed to IAU 1976 J2000.
_J2000_OBL_RAD = math.radians(23.439_291_111_111_11)
_COS_OBL = math.cos(_J2000_OBL_RAD)
_SIN_OBL = math.sin(_J2000_OBL_RAD)


# ───────────────────────────────────────────────────────────────────
# Container invocation
# ───────────────────────────────────────────────────────────────────

def run_orbfit_container(
    desig: str,
    obs_psv: str,
    image: str,
    platform: str,
    timeout_s: float = 300.0,
) -> Optional[Dict[str, Any]]:
    """Replay one OD case through the OrbFit container.

    Uses `fitobs.x` end-to-end: Gauss IOD (main menu 2 → inputele 4 →
    prelimet 2) followed by CMC2003-autoreject DC (main menu 3 →
    difcomod 1). No seed orbit is required — Gauss IOD computes
    elements from observations alone, matching empyrean's IOD + DC
    pipeline against the same astrometry.

    NEOCP fresh-discovery cases (arcs days-to-weeks) would normally
    use `neocp_prelim.x` instead, but empyrean's validation catalog
    is dominated by known objects with multi-year arcs where Gauss
    IOD is the right tool.

    Sets up a scratch workdir matching OrbFit's expected layout:

        scratch/
        ├── <desig>.fop      (fit options)
        ├── <desig>.rnc      (fitobs menu sequence — IOD + DC)
        ├── AST17.bai        (symlink to container's ast_files/AST17.bai)
        ├── AST17.bep        (symlink to container's ast_files/AST17.bep)
        ├── mpcobs/
        │   └── <desig>.psv  (ADES PSV — fitobs auto-detects)
        └── epoch/           (fitobs writes IOD + DC outputs here)

    Mounts the scratch dir at `/sa/god_fit/work` and runs:

        cd /sa/god_fit/work
        ln -sf ../ast_files/AST17.{bai,bep} .   # perturber files
        ln -sf ../bin/fitobs.x .
        ./fitobs.x < <desig>.rnc

    The .rnc walks fitobs through Gauss IOD on arc 1, then CMC2003
    autoreject DC. Output files:

        <desig>.fel          equinoctial elements + 6×6 covariance
        mpcobs/<desig>.rwo   per-observation residuals + SEL flags
        <desig>.err          error log
        <desig>.fou          verbose fit log

    Returns a dict with parsed `.fel` + `.rwo` data, or None on failure.
    """
    scratch = Path(tempfile.mkdtemp(prefix="orbfit_empyrean_"))
    try:
        # Write fit options + menu sequence
        (scratch / f"{desig}.fop").write_text(FOP_TEMPLATE.format(desig=desig))
        (scratch / f"{desig}.rnc").write_text(RNC_TEMPLATE.format(desig=desig))

        # Write observations (PSV)
        mpcobs = scratch / "mpcobs"
        mpcobs.mkdir()
        (mpcobs / f"{desig}.psv").write_text(obs_psv)

        # Empty epoch dir — fitobs writes IOD outputs here
        epoch_dir = scratch / "epoch"
        epoch_dir.mkdir()

        # ── docker run: Gauss IOD + CMC2003 DC via fitobs.x ─────────
        cmd = [
            "docker", "run", "--rm",
            "--platform", platform,
            "-v", f"{scratch}:{CONTAINER_WORKDIR}/work",
            image,
            "-c", (
                f"cd {CONTAINER_WORKDIR}/work && "
                # Link the asteroid perturber + Earth orientation files
                # that the .fop's `filbe='AST17'` references.
                f"ln -sf ../ast_files/AST17.bai AST17.bai && "
                f"ln -sf ../ast_files/AST17.bep AST17.bep && "
                # IOD + DC in one fitobs invocation (.rnc drives the menus)
                f"ln -sf ../bin/fitobs.x . && "
                f"./fitobs.x < {desig}.rnc"
            ),
        ]
        result = subprocess.run(
            cmd, text=True, capture_output=True, timeout=timeout_s,
        )

        # fitobs.x often exits 0 even on failure; we judge by output
        # files. .fel + mpcobs/.rwo must both exist and be non-empty.
        fel_path = scratch / f"{desig}.fel"
        rwo_path = mpcobs / f"{desig}.rwo"
        if not fel_path.exists() or fel_path.stat().st_size == 0:
            print(f"    orbfit produced no .fel for {desig}", file=sys.stderr)
            if result.stdout:
                print(f"    stdout tail: {result.stdout[-300:]}", file=sys.stderr)
            return None
        if not rwo_path.exists() or rwo_path.stat().st_size == 0:
            print(f"    orbfit produced no .rwo for {desig}", file=sys.stderr)
            return None

        return _parse_orbfit_outputs(fel_path, rwo_path)

    except subprocess.TimeoutExpired:
        print(f"    orbfit timed out after {timeout_s:.0f}s for {desig}", file=sys.stderr)
        return None
    finally:
        shutil.rmtree(scratch, ignore_errors=True)


# ───────────────────────────────────────────────────────────────────
# Output parsing
# ───────────────────────────────────────────────────────────────────

def _parse_orbfit_outputs(fel_path: Path, rwo_path: Path) -> Optional[Dict[str, Any]]:
    """Parse OrbFit's `.fel` + `.rwo` into a runner-friendly dict.

    Returns:
        {
            "equinoctial": [a, h, k, p, q, lambda_deg],   # OEF2.0 EQU row
            "epoch_mjd_tdt": float,                        # MJD TDT
            "h_mag": float, "g_mag": float,                # MAG row
            "covariance_6x6_equ": [[float; 6]; 6],         # COV upper triangle expanded
            "rms_arcsec": float,                           # RMSast from .rwo
            "rms_mag": float,                              # RMSmag from .rwo
            "n_obs_total": int,
            "n_obs_used": int,
            "n_obs_rejected": int,
            "error_model": str,
        }
    """
    fel_text = fel_path.read_text()
    rwo_text = rwo_path.read_text()

    # ── .fel: EQU row ───────────────────────────────────────────────
    m = re.search(r"^\s*EQU\s+(\S+)\s+(\S+)\s+(\S+)\s+(\S+)\s+(\S+)\s+(\S+)",
                  fel_text, re.MULTILINE)
    if not m:
        return None
    equinoctial = [float(m.group(i).replace("D", "E").replace("d", "e"))
                   for i in range(1, 7)]

    # ── .fel: MJD epoch ─────────────────────────────────────────────
    m = re.search(r"^\s*MJD\s+(\S+)\s+TDT", fel_text, re.MULTILINE)
    epoch_mjd_tdt = float(m.group(1).replace("D", "E").replace("d", "e")) if m else float("nan")

    # ── .fel: MAG ───────────────────────────────────────────────────
    m = re.search(r"^\s*MAG\s+(\S+)\s+(\S+)", fel_text, re.MULTILINE)
    h_mag = float(m.group(1)) if m else float("nan")
    g_mag = float(m.group(2)) if m else float("nan")

    # ── .fel: COV — 6 lines × 3 values = upper triangle of 6×6 ──────
    # The OEF2.0 format packs the 21 upper-triangle values into 6 COV
    # records (rows of 3 floats each). The first record is the
    # diagonal element (1,1) and two next-row values; standard packing
    # is row-major.
    cov_values: list[float] = []
    for line in fel_text.splitlines():
        if line.startswith(" COV ") or line.startswith("COV "):
            parts = line.split()[1:]
            cov_values.extend(float(p.replace("D", "E").replace("d", "e")) for p in parts)
    cov_6x6 = _unpack_upper_triangle(cov_values) if len(cov_values) >= 21 else None

    # ── .rwo: RMSast / RMSmag header ────────────────────────────────
    m = re.search(r"^\s*RMSast\s*=\s*(\S+)", rwo_text, re.MULTILINE)
    rms_arcsec = float(m.group(1).replace("D", "E").replace("d", "e")) if m else float("nan")
    m = re.search(r"^\s*RMSmag\s*=\s*(\S+)", rwo_text, re.MULTILINE)
    rms_mag = float(m.group(1).replace("D", "E").replace("d", "e")) if m else float("nan")
    m = re.search(r"^\s*errmod\s*=\s*'(\S+)'", rwo_text, re.MULTILINE)
    error_model = m.group(1) if m else ""

    # ── .rwo: per-observation SEL flag ──────────────────────────────
    # The body of .rwo has one line per obs after END_OF_HEADER. The
    # SEL flag (1 = used, 0 = rejected) is column-aligned but easier to
    # extract by parsing the right-hand fixed columns. For now we just
    # count the lines and check the SEL column.
    n_total = 0
    n_used = 0
    in_body = False
    for line in rwo_text.splitlines():
        if "END_OF_HEADER" in line:
            in_body = True
            continue
        if not in_body:
            continue
        if line.startswith(" !") or not line.strip():
            continue
        # SEL is towards the right; tokenise and assume the SEL column
        # is the last small integer in the line. Robust extraction
        # would key off the column header, but the .rwo format is
        # stable enough that token-position works.
        toks = line.split()
        if len(toks) < 10:
            continue
        n_total += 1
        # Heuristic: SEL is the int immediately after the residuals.
        # We look for the first standalone "0" or "1" in the trailing
        # half of the line.
        for t in reversed(toks):
            if t in ("0", "1"):
                if t == "1":
                    n_used += 1
                break

    return {
        "equinoctial": equinoctial,
        "epoch_mjd_tdt": epoch_mjd_tdt,
        "h_mag": h_mag,
        "g_mag": g_mag,
        "covariance_6x6_equ": cov_6x6,
        "rms_arcsec": rms_arcsec,
        "rms_mag": rms_mag,
        "n_obs_total": n_total,
        "n_obs_used": n_used,
        "n_obs_rejected": n_total - n_used,
        "error_model": error_model,
    }


def _unpack_upper_triangle(packed: list[float]) -> list[list[float]]:
    """Expand row-major 21-element upper-triangle packing into 6×6."""
    out = [[0.0] * 6 for _ in range(6)]
    k = 0
    for i in range(6):
        for j in range(i, 6):
            out[i][j] = packed[k]
            out[j][i] = packed[k]
            k += 1
    return out


# ───────────────────────────────────────────────────────────────────
# Row processing
# ───────────────────────────────────────────────────────────────────

def process_od_row(row: Dict[str, Any], image: str, platform: str) -> Dict[str, Any]:
    """Run OrbFit on a single OD row from the validation plan.

    ⚠️ TODO: end-to-end OD fit is not yet implemented. The runner
    plumbing (container probe, docker exec, output parsers) is in
    place, but none of OrbFit's binaries fit empyrean's multi-year-arc
    catalog without a Cartesian seed orbit written from empyrean's IC.
    See `runners/orbfit/TODO.md` for the investigation summary and the
    `neofit2.x` + seed-writer path that should be wired next.

    This function returns an early-exit row with `orbfit_error` set;
    `merge_orbfit` in the empyrean-validation CLI will not overwrite
    the empyrean fit metrics with these placeholder values.
    """
    out = dict(row)
    out.setdefault("channel", "orbfit")

    # Hard short-circuit until the seed-writer + neofit2.x path is in
    # place. Keeps the runner safe to invoke (e.g. via WITH_ORBFIT=1)
    # without producing misleading fit data.
    out["orbfit_error"] = (
        "OrbFit IOD not yet implemented; "
        "see runners/orbfit/TODO.md for the neofit2.x + Cartesian-seed plan"
    )
    return out


def process_od_row_real(row: Dict[str, Any], image: str, platform: str) -> Dict[str, Any]:
    """Unwired reference implementation — the version that actually
    exec's the container. Keeping the body in source as a reference
    for the next session; the working path is gated above. To re-arm,
    rename this back to `process_od_row` and delete the short-circuit.
    """
    out = dict(row)
    out.setdefault("channel", "orbfit")

    if row.get("test_type") != "orbit_determination":
        return out

    obs_psv = row.get("observations_psv")
    if not obs_psv:
        out["orbfit_error"] = "no observations_psv in plan row"
        return out

    desig = _orbfit_designation(row.get("object", ""))

    # IOD is handled inside the container by `neocp_prelim.x` running
    # before `fitobs.x`, so no caller-supplied seed orbit is required.
    # The two-binary chain mirrors empyrean's IOD + DC pipeline.
    t0 = time.monotonic()
    res = run_orbfit_container(
        desig=desig, obs_psv=obs_psv,
        image=image, platform=platform,
    )
    elapsed_ms = (time.monotonic() - t0) * 1000.0

    if res is None:
        out["orbfit_error"] = "orbfit invocation failed"
        out["orbfit_time_ms"] = elapsed_ms
        return out

    # The natively-Cartesian fields stay None for now — we report
    # equinoctial + covariance verbatim. Cartesian conversion (which
    # the report consumes for the cross-channel position-delta plot)
    # is best done downstream where empyrean's Cartesian/equinoctial
    # bridge already lives. The runner just emits raw OrbFit data.
    out["orbfit_equinoctial"] = res["equinoctial"]
    out["orbfit_epoch_mjd_tdb"] = res["epoch_mjd_tdt"]  # ≈ TDB to sub-ms
    out["orbfit_covariance_6x6_equ"] = res["covariance_6x6_equ"]
    out["orbfit_rms_arcsec"] = res["rms_arcsec"]
    out["orbfit_n_obs_total"] = res["n_obs_total"]
    out["orbfit_n_obs_used"] = res["n_obs_used"]
    out["orbfit_n_obs_rejected"] = res["n_obs_rejected"]
    out["orbfit_h_mag"] = res["h_mag"]
    out["orbfit_g_mag"] = res["g_mag"]
    out["orbfit_error_model"] = res["error_model"]
    out["orbfit_time_ms"] = elapsed_ms
    return out


def _orbfit_designation(obj_name: str) -> str:
    """Coerce a plan-row object name into OrbFit's packed-designation
    style. For numbered asteroids ("(105) Artemis" or just "105"),
    OrbFit accepts the number directly. For provisional designations
    we'd want the packed form (e.g. 2024 YR4 → K24Y04R), but the
    container's `mpc-orbfit` build has packed-designation utilities
    we can shell out to if needed. For now, normalise whitespace and
    let OrbFit's own parser handle the rest."""
    return obj_name.strip().replace(" ", "")


# ───────────────────────────────────────────────────────────────────
# Entry point
# ───────────────────────────────────────────────────────────────────

def main() -> int:
    parser = argparse.ArgumentParser(
        description="Replay validation plan OD rows through OrbFit.",
    )
    parser.add_argument("--plan", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--image", default=DEFAULT_IMAGE)
    parser.add_argument("--platform", default=DEFAULT_PLATFORM,
                        help="docker --platform (Apple Silicon must use linux/amd64)")
    parser.add_argument("--timeout", type=float, default=300.0,
                        help="per-row timeout in seconds (default: 300)")
    args = parser.parse_args()

    if not args.plan.exists():
        print(f"error: plan not found at {args.plan}", file=sys.stderr)
        return 2

    print(f"OrbFit runner ({args.image}, --platform {args.platform})")
    print(f"  reading plan from {args.plan}")
    with open(args.plan) as f:
        plan = json.load(f)

    rows = plan if isinstance(plan, list) else plan.get("rows", [])
    od_rows = [r for r in rows if r.get("test_type") == "orbit_determination"]
    print(f"  {len(rows)} total rows, {len(od_rows)} OD rows to replay")

    if not od_rows:
        with open(args.output, "w") as f:
            json.dump([], f)
        return 0

    out_rows = []
    timings = []
    failures = 0
    for i, row in enumerate(od_rows):
        desig = row.get("object", "?")
        print(f"  [{i + 1}/{len(od_rows)}] {desig} ...", flush=True)
        decorated = process_od_row(row, image=args.image, platform=args.platform)
        out_rows.append(decorated)
        if decorated.get("orbfit_error"):
            failures += 1
        if "orbfit_time_ms" in decorated:
            timings.append(decorated["orbfit_time_ms"])

    print(f"\nOrbFit runner: {len(out_rows)} rows emitted "
          f"({failures} failures, {len(out_rows) - failures} fits)")
    if timings:
        print(f"  per-row wall-clock: median {statistics.median(timings):.0f} ms, "
              f"mean {statistics.mean(timings):.0f} ms")

    with open(args.output, "w") as f:
        json.dump(out_rows, f, indent=2)
    print(f"  wrote {args.output}")
    return 0 if failures == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
