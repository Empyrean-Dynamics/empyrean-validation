#!/usr/bin/env python3
"""OrbFit external-reference runner for the empyrean validation suite.

Replays each orbit-determination row of the canonical test plan
(`validation_plan.json`) through OrbFit's production NEO refit binary
(`neofit2.x`, from the Smithsonian `mpc-orbfit` source) packaged in the
IAU Minor Planet Center's `minorplanetcenter/orbfit` Docker container.
Emits one row per OD object with the OrbFit-specific fields populated,
consumed by `empyrean-validation merge-external --orbfit`.

How it works
============

OrbFit's production workflow (the one the MPC uses to maintain its NEO
catalog) never does blind IOD on a long arc: it always refines a *prior*
orbit against the astrometry. `neofit2.x` is that refit step. We drive it
exactly as the MPC does, but seed the prior orbit from empyrean's initial
conditions instead of from a previous MPC solution:

  1. **Seed** — each OD object's initial condition (Cartesian state at
     `epoch_mjd_tdb`) is taken from the plan (the object's propagation /
     ephemeris rows carry `ic_pos_au` / `ic_vel_au_d`, ICRF, SSB-centered).
     OrbFit's osculating elements are *heliocentric*, so we subtract the
     Sun's SSB state at the IC epoch, read from the registered DE440
     (`--de440`, the orbfit/ folder of the third-party tool set) by the
     shared helper `runners/sunstate`, then rotate ICRF -> ecliptic-J2000
     (OrbFit's `ECLM J2000` reference system) and write an OEF2.0 `CAR`
     seed at `epoch/<desig>.eq0`.

     The SSB -> heliocentric shift (<= 0.008 AU) is not cosmetic: for
     deep-Earth-approaching NEOs it is a large fraction of the encounter
     distance (~22% of Apophis's 0.029 AU 2020 approach), and seeding
     with the raw SSB state corrupts the two-body encounter segments of
     `neofit2.x`'s initial propagation (`ever_pitkin` overflow, singular
     normal matrix) so the differential correction never converges. With
     the Sun subtracted, Apophis fits cleanly. There is no raw-SSB
     fallback: the plan the suite runs carries no Sun state, so the runner
     refuses to start without the DE440 file, and every row records the
     seed's origin and the file's sha256.

     This is a *refit from empyrean's IC*, not an independent
     initial-orbit determination: the seed lands the differential
     correction in the right basin; the observations pin the orbit.

  2. **Fit** — `neofit2.x < input` (the `input` file holds the
     designation) with the MPC's production option file:
       - `neofit.nop.std` for gravity-only objects,
       - `neofit.nop.ngr` (identical but `.ngr_opt=.T.`) when the plan
         row carries a non-gravitational signal (`ic_a1`/`ic_a2`/`ic_a3`
         populated and non-zero) — e.g. Apophis, Phaethon, 2024 YR4.
     Observations come from `mpcobs/<desig>.psv` (the ADES PSV fixtures);
     `neofit2.x` ingests `.psv` directly and applies the `gaiaDR2_mix`
     error model + CMC2003 outlier rejection.

     Its force model holds the planets and the massive asteroids the option
     file names (`.iast=17`, `.filbe='AST17'`): the first 17 entries of the
     image's `ast_files/AST17.bai`, each named by its number (`4` is Vesta).
     neofit2.x leaves one of them out by its own self-perturbation check
     (`selpert`, `src/propag/force_model.f90`, called at
     `src/neodys/neofit2.f90:277`): the entry whose name equals the name the
     object is run under (the `input` file, which also names its scratch
     files). So when the plan excludes an AST17 body from an object's forces
     (`excluded_perturbers_naif` names 2000000+n), the object runs under that
     body's AST17 name, and the row fails unless neofit2.x's own output
     reports leaving it out (`self perturbation of <n> avoided`). The
     decision reads the row's data only, never its designation. An excluded
     body outside AST17 needs nothing; two excluded AST17 bodies cannot both
     be left out (selpert drops only the one the object is named by), so the
     row is a named unsupported row with the reason in `orbfit_unsupported`
     and is never run.

  3. **Parse** — the fitted Cartesian state + covariance come from the
     `CAR` record of `epoch/<desig>.eq0_postfit`; the weighted RMS and
     the per-observation SEL flags (used / rejected) come from
     `mpcobs/<desig>.rwo`. The fitted state is rotated ecliptic -> ICRF
     so emitted rows carry ICRF (heliocentric origin, OrbFit's native),
     matching the ICRF convention of the other runners.

OrbFit-specific fields emitted per row:

    orbfit_rms_arcsec        weighted RMS of post-fit residuals
                              (`RMSast` from the `.rwo` header) — the
                              field the report + merge consume
    orbfit_n_obs_used        observations kept   (SEL flag = 1)
    orbfit_n_obs_rejected    observations culled  (SEL flag = 0)
    orbfit_n_obs_total       observations submitted
    orbfit_time_ms           wall-clock per row (dominated by qemu on
                              Apple Silicon; the image is amd64-only)
    orbfit_orbit_pos_au      fitted Cartesian position (AU, ICRF,
                              heliocentric) at OrbFit's fit epoch
    orbfit_orbit_vel_au_d    fitted Cartesian velocity (AU/day)
    orbfit_epoch_mjd_tdb     fit epoch (arc-weighted center; MJD TDT,
                              treated as ~= TDB, sub-ms offset)
    orbfit_covariance_6x6    fitted 6x6 Cartesian covariance (ecliptic
                              J2000, OrbFit's native frame — the state
                              is rotated to ICRF but the covariance is
                              emitted verbatim for reference)
    orbfit_is_nongrav        whether the ngr option file was used
    orbfit_left_out_naif     the excluded AST17 body neofit2.x reported
                              leaving out of the object's forces (NAIF id)
    orbfit_unsupported       set (and the row never run) when the plan
                              excludes more than one AST17 body
    orbfit_error             set (with the fit fields left unpopulated)
                              on any failure — never a silent skip

Propagation and ephemeris rows are skipped — OrbFit's natural domain is
OD, matching `run_findorb.py`. ASSIST is the propagation reference;
Horizons the ephemeris reference.

OrbFit is GPL-3.0 licensed and never linked into empyrean. This runner
shells out to the upstream container; see `setup.sh` for the pull.
References: Milani et al. (2004) OrbFit manual; Carpino, Milani &
Chesley (2003) Icarus 166, 248.
"""

from __future__ import annotations

import argparse
import json
import math
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

# Container identity, pinned by digest: the MPC's image carries OrbFit's own
# data (AST17.bai / AST17.bep, the neofit options), so the digest pins those
# too. minorplanetcenter/orbfit:latest resolved to this manifest on 2026-11-09
# (docker buildx imagetools inspect). Override via --image only for testing.
DEFAULT_IMAGE = (
    "minorplanetcenter/orbfit@sha256:"
    "0a72ed7c2c528e0e3fe24a0f280a251f6ab610eb3d5e1d63dbf0f2bd20104f64"
)

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))  # runners/ -> sunstate
from sunstate.de440_sun import SpkError, SunEphemeris  # noqa: E402

# OrbFit's WORKDIR inside the container. Its bin/lib/ast_files symlinks are
# relative to this, so we mount our scratch as a sibling subdirectory
# (`/sa/god_fit/work`); `../lib`, `../ast_files`, `../src` all resolve.
CONTAINER_WORKDIR = "/sa/god_fit"

# The MPC image is amd64-only; Apple Silicon hosts must emulate.
DEFAULT_PLATFORM = "linux/amd64"

# A numbered asteroid's NAIF id is 2000000 + its number (below 1000000), the form
# the plan's `excluded_perturbers_naif` takes.
NAIF_ASTEROID_OFFSET = 2_000_000

# The option files the fit reads and the massive-asteroid table they name, inside
# the image (paths relative to CONTAINER_WORKDIR).
OPTION_FILES = ("unit_tests/neofit.nop.std", "unit_tests/neofit.nop.ngr")
AST_TABLE_DIR = "ast_files"
_TABLE_SEPARATOR = "=====orbfit-table====="


def parse_massive_asteroids(option_texts: list[str], table_texts: dict[str, str]) -> list[str]:
    """The massive asteroids in neofit2.x's force model, by the names it matches.

    Every option file must name the same table (`.filbe`) and count (`.iast`); the
    table (`<filbe>.bai`) opens with its entry count and a header line, then one
    `<mass> <name>` line per asteroid, and neofit2.x integrates its first `iast`
    entries under those names (masjpl, src/propag/force_model.f90). Raises
    ValueError when the files disagree or the table is short, so a run never
    guesses which bodies OrbFit integrates.
    """
    seen = set()
    for text in option_texts:
        iast = re.search(r"^\s*\.iast\s*=\s*(\d+)", text, re.MULTILINE)
        filbe = re.search(r"^\s*\.filbe\s*=\s*'([^']+)'", text, re.MULTILINE)
        if not iast or not filbe:
            raise ValueError("an OrbFit option file names no .iast or no .filbe")
        seen.add((int(iast.group(1)), filbe.group(1)))
    if len(seen) != 1:
        raise ValueError(f"OrbFit's option files disagree on the massive asteroids: {sorted(seen)}")
    ((iast, filbe),) = seen
    if iast == 0:
        return []
    table = table_texts.get(filbe)
    if table is None:
        raise ValueError(f"OrbFit's option files name {filbe}.bai, which was not read")
    lines = [ln for ln in table.splitlines() if ln.strip()]
    count = int(lines[0].split()[0])
    names = [ln.split()[1] for ln in lines[2 : 2 + count]]
    if len(names) != count or iast > count:
        raise ValueError(f"{filbe}.bai lists {len(names)} of {count} entries, fewer than .iast={iast}")
    return names[:iast]


def read_massive_asteroids(image: str, platform: str) -> list[str]:
    """Read the massive asteroids neofit2.x integrates from the image itself.

    The runner links the image's AST17 table into every fit, so its option files
    must name it. The image is pinned by digest, so
    these are the bodies every fit in a run integrates; reading them (rather than
    restating them here) keeps the mapping exactly the image's.
    """
    files = list(OPTION_FILES) + [f"{AST_TABLE_DIR}/AST17.bai"]
    script = f" && echo {_TABLE_SEPARATOR} && ".join(f"cat {f}" for f in files)
    proc = subprocess.run(
        ["docker", "run", "--rm", "--platform", platform, "--network", "none", image,
         "-c", f"cd {CONTAINER_WORKDIR} && {script}"],
        capture_output=True,
        text=True,
        timeout=300,
        check=False,
    )
    if proc.returncode != 0:
        raise ValueError(f"could not read OrbFit's tables from {image}: {(proc.stderr or '').strip()[-300:]}")
    parts = proc.stdout.split(_TABLE_SEPARATOR + "\n")
    if len(parts) != len(files):
        raise ValueError(f"expected {len(files)} OrbFit tables from {image}, read {len(parts)}")
    return parse_massive_asteroids(parts[:2], {"AST17": parts[2]})


def orbfit_exclusion(row: dict[str, Any], massive: list[str]) -> tuple[str | None, str | None]:
    """How neofit2.x leaves out the bodies the plan excludes from this row's forces.

    Returns `(name, unsupported)`. `name` is the massive asteroid's name to run the
    object under, so neofit2.x's self-perturbation check leaves that body out; None
    when no excluded body is among `massive` (nothing to leave out). `unsupported`
    is the reason the row cannot run: more than one excluded body is among
    `massive`, and neofit2.x leaves out only the one the object is named by. Reads
    the row's `excluded_perturbers_naif` only, never its designation.
    """
    hits = set()
    for naif in row.get("excluded_perturbers_naif") or ():
        number = int(naif) - NAIF_ASTEROID_OFFSET
        if 0 < number < 1_000_000 and str(number) in massive:
            hits.add(str(number))
    ordered = sorted(hits, key=int)
    if not ordered:
        return None, None
    if len(ordered) == 1:
        return ordered[0], None
    return None, (
        f"excluded perturbers {', '.join(ordered)} are all among OrbFit's massive asteroids, and "
        "neofit2.x leaves out only the one body the object is run under (its self-perturbation "
        "check), so it would integrate an excluded body"
    )


def neofit_reports_leaving_out(stdout: str, name: str) -> bool:
    """True when neofit2.x's own output says its self-perturbation check left `name` out."""
    return re.search(rf"self perturbation of\s+{re.escape(name)}\s+avoided", stdout or "") is not None

# Obliquity at J2000.0 (IAU 1976). empyrean's ICRF (equatorial) Cartesian
# states rotate to OrbFit's ECLM J2000 (ecliptic) by a rotation about the
# X axis (vernal equinox) through +epsilon:
#   x_ecl =  x_icrf
#   y_ecl =  cos(e)*y_icrf + sin(e)*z_icrf
#   z_ecl = -sin(e)*y_icrf + cos(e)*z_icrf
# The inverse (ecliptic -> ICRF) flips the sign of the sin(e) terms.
_J2000_OBL_RAD = math.radians(23.439_291_111_111_11)
_COS_OBL = math.cos(_J2000_OBL_RAD)
_SIN_OBL = math.sin(_J2000_OBL_RAD)


def icrf_to_ecliptic(v: List[float]) -> List[float]:
    """Rotate an ICRF (equatorial-J2000) 3-vector to ecliptic J2000."""
    x, y, z = v
    return [
        x,
        _COS_OBL * y + _SIN_OBL * z,
        -_SIN_OBL * y + _COS_OBL * z,
    ]


def ecliptic_to_icrf(v: List[float]) -> List[float]:
    """Rotate an ecliptic-J2000 3-vector back to ICRF (equatorial J2000)."""
    x, y, z = v
    return [
        x,
        _COS_OBL * y - _SIN_OBL * z,
        _SIN_OBL * y + _COS_OBL * z,
    ]


# ───────────────────────────────────────────────────────────────────
# Seed writer
# ───────────────────────────────────────────────────────────────────


def write_eq0_seed(
    desig: str,
    ic_pos_au: List[float],
    ic_vel_au_d: List[float],
    epoch_mjd_tdb: float,
) -> str:
    """Return the OEF2.0 `CAR` seed-orbit text for `epoch/<desig>.eq0`.

    `ic_pos_au` / `ic_vel_au_d` are the ICRF Cartesian heliocentric seed
    state at `epoch_mjd_tdb` (the Sun from the registered DE440, see
    `process_od_row`). They are
    rotated to ecliptic J2000 and written as a `CAR` record, which
    OrbFit reads as the seed for the differential correction.
    """
    pos_ecl = icrf_to_ecliptic(list(ic_pos_au))
    vel_ecl = icrf_to_ecliptic(list(ic_vel_au_d))
    state = pos_ecl + vel_ecl
    car = " ".join(f"{c: .15E}" for c in state)
    return (
        "format  = 'OEF2.0'       ! file format\n"
        "rectype = 'ML'           ! record type (1L/ML)\n"
        "refsys  = ECLM J2000     ! default reference system\n"
        "END_OF_HEADER\n"
        f"{desig}\n"
        "! Cartesian position and velocity vectors\n"
        f" CAR {car}\n"
        f" MJD  {epoch_mjd_tdb:.9f} TDT\n"
        " MAG  0.000  0.150\n"
    )


# ───────────────────────────────────────────────────────────────────
# Container invocation
# ───────────────────────────────────────────────────────────────────


def run_neofit_container(
    desig: str,
    obs_psv: str,
    seed_eq0: str,
    is_ngr: bool,
    image: str,
    platform: str,
    timeout_s: float,
    leave_out: str | None = None,
) -> Tuple[Optional[Dict[str, Any]], Optional[str]]:
    """Refit one OD case through `neofit2.x`.

    `leave_out` names the massive asteroid the object runs under so that
    neofit2.x leaves it out of the forces (`desig` is then that name); the fit
    fails unless neofit2.x's own output reports doing so.

    Sets up a scratch workdir mirroring OrbFit's `unit_tests/` layout,
    mounts it at `/sa/god_fit/work`, and runs `neofit2.x < input`.

        scratch/
        ├── input                  (the designation)
        ├── neofit.nop             (copy of neofit.nop.std / .ngr)
        ├── mpcobs/<desig>.psv     (ADES observations)
        ├── epoch/<desig>.eq0      (the Cartesian seed)
        └── err/                   (neofit warning log)

    Returns `(parsed, None)` on success or `(None, error_message)` on
    any failure — the caller surfaces the error loudly; nothing is ever
    silently skipped or defaulted.
    """
    scratch = Path(tempfile.mkdtemp(prefix="orbfit_empyrean_"))
    try:
        (scratch / "input").write_text(desig)
        mpcobs = scratch / "mpcobs"
        mpcobs.mkdir()
        (mpcobs / f"{desig}.psv").write_text(obs_psv)
        epoch_dir = scratch / "epoch"
        epoch_dir.mkdir()
        (epoch_dir / f"{desig}.eq0").write_text(seed_eq0)
        # neofit2.x writes per-object warning logs to err/ and war/.
        (scratch / "err").mkdir()
        (scratch / "war").mkdir()

        nop_src = "neofit.nop.ngr" if is_ngr else "neofit.nop.std"
        cmd = [
            "docker",
            "run",
            "--rm",
            "--platform",
            platform,
            # OrbFit reads only the image and the mounted scratch; a run
            # that reached for the network would fail here, not fetch.
            "--network",
            "none",
            "-v",
            f"{scratch}:{CONTAINER_WORKDIR}/work",
            image,
            "-c",
            (
                f"cd {CONTAINER_WORKDIR}/work && "
                f"cp ../unit_tests/{nop_src} neofit.nop && "
                f"ln -sf ../ast_files/AST17.bai AST17.bai && "
                f"ln -sf ../ast_files/AST17.bep AST17.bep && "
                f"ln -sf ../src/neodys/neofit2.x neofit2.x && "
                f"./neofit2.x < input"
            ),
        ]
        try:
            result = subprocess.run(
                cmd,
                text=True,
                capture_output=True,
                timeout=timeout_s,
            )
        except subprocess.TimeoutExpired:
            return None, f"neofit2.x timed out after {timeout_s:.0f}s"

        # The body the plan excludes is out of the forces only when neofit2.x
        # says so itself; anything else is a failure, never a silent full model.
        if leave_out is not None and not neofit_reports_leaving_out(result.stdout, leave_out):
            return None, (
                f"neofit2.x did not report leaving out massive asteroid {leave_out} "
                f"(no 'self perturbation of {leave_out} avoided' line), so the body the "
                "plan excludes may still be in its forces"
            )

        # neofit2.x exits 0 even on a failed fit; judge by output files.
        # A header-only (152-byte) postfit is OrbFit's empty-fit sentinel.
        postfit = epoch_dir / f"{desig}.eq0_postfit"
        rwo = mpcobs / f"{desig}.rwo"
        if not postfit.exists() or postfit.stat().st_size in (0, 152):
            tail = (result.stdout or "")[-400:]
            return None, (
                "neofit2.x produced no post-fit orbit "
                f"(DC did not converge / bizarre-orbit reject). stdout tail: {tail!r}"
            )
        if not rwo.exists() or rwo.stat().st_size == 0:
            return None, "neofit2.x produced no .rwo residual file"

        return _parse_neofit_outputs(postfit, rwo), None

    finally:
        shutil.rmtree(scratch, ignore_errors=True)


# ───────────────────────────────────────────────────────────────────
# Output parsing
# ───────────────────────────────────────────────────────────────────


def _f(token: str) -> float:
    """Parse a Fortran float token (`D`/`d` exponents -> `E`)."""
    return float(token.replace("D", "E").replace("d", "e"))


def _parse_neofit_outputs(postfit: Path, rwo: Path) -> Dict[str, Any]:
    """Parse the `CAR` record of `<desig>.eq0_postfit` + the `<desig>.rwo`.

    `eq0_postfit` holds the fitted orbit in several coordinate records
    (EQU, KEP, CAR, COM); we read the `CAR` block (Cartesian, ecliptic
    J2000) and its trailing `COV` block. The fitted state is rotated
    ecliptic -> ICRF; the covariance is emitted in OrbFit's native
    ecliptic frame.
    """
    lines = postfit.read_text().splitlines()

    car_state: Optional[List[float]] = None
    car_epoch = float("nan")
    cov_ecl: Optional[List[List[float]]] = None
    ngr_a: Optional[List[float]] = None

    # Walk to the CAR record, then read the MJD + COV that follow it.
    for i, line in enumerate(lines):
        s = line.strip()
        if s.startswith("CAR "):
            car_state = [_f(t) for t in s.split()[1:7]]
            # MJD epoch is on a following line before the next record.
            for j in range(i + 1, min(i + 6, len(lines))):
                mm = re.match(r"\s*MJD\s+(\S+)\s+TD", lines[j])
                if mm:
                    car_epoch = _f(mm.group(1))
                    break
            # COV block (7 records x 3 = 21 upper-triangle values) follows.
            cov_vals: List[float] = []
            for j in range(i + 1, len(lines)):
                cs = lines[j].strip()
                if cs.startswith("COV "):
                    cov_vals.extend(_f(t) for t in cs.split()[1:])
                elif cs.startswith("NGR "):
                    ngr_a = [_f(t) for t in cs.split()[1:]]
                elif cs.startswith(("CAR ", "KEP ", "EQU ", "COM ")):
                    break  # next record — stop before its COV block
            if len(cov_vals) >= 21:
                cov_ecl = _unpack_upper_triangle(cov_vals[:21])
            break

    if car_state is None:
        raise ValueError("no CAR record in post-fit orbit file")

    pos_icrf = ecliptic_to_icrf(car_state[0:3])
    vel_icrf = ecliptic_to_icrf(car_state[3:6])

    rms_arcsec, n_used, n_rejected, error_model = _parse_rwo(rwo)

    return {
        "orbit_pos_au": pos_icrf,
        "orbit_vel_au_d": vel_icrf,
        "epoch_mjd_tdb": car_epoch,
        "covariance_6x6_ecl": cov_ecl,
        "nongrav_a": ngr_a,
        "rms_arcsec": rms_arcsec,
        "n_obs_used": n_used,
        "n_obs_rejected": n_rejected,
        "n_obs_total": n_used + n_rejected,
        "error_model": error_model,
    }


def _parse_rwo(rwo: Path) -> Tuple[float, int, int, str]:
    """Parse `RMSast` and the per-observation SEL flags from a `.rwo`.

    Header line 3 is `RMSast  =  <value>`. In the body (after
    `END_OF_HEADER`) each non-comment observation line carries its SEL
    flag ('1' kept, '0' rejected) in a fixed column — index 194, the
    column OrbFit's own `test_neofit.py` reads.
    """
    text = rwo.read_text()

    m = re.search(r"^\s*RMSast\s*=\s*(\S+)", text, re.MULTILINE)
    rms_arcsec = _f(m.group(1)) if m else float("nan")
    m = re.search(r"^\s*errmod\s*=\s*'?([^'\s]+)", text, re.MULTILINE)
    error_model = m.group(1) if m else ""

    n_used = 0
    n_rejected = 0
    in_body = False
    for line in text.splitlines():
        if "END_OF_HEADER" in line:
            in_body = True
            continue
        if not in_body or not line.strip() or line.lstrip().startswith("!"):
            continue
        if len(line) <= 194:
            continue  # short / non-observation line (e.g. section marker)
        sel = line[194]
        if sel == "0":
            n_rejected += 1
        elif sel == "1":
            n_used += 1
    return rms_arcsec, n_used, n_rejected, error_model


def _unpack_upper_triangle(packed: List[float]) -> List[List[float]]:
    """Expand a row-major 21-element upper triangle into a 6x6 matrix."""
    out = [[0.0] * 6 for _ in range(6)]
    k = 0
    for i in range(6):
        for j in range(i, 6):
            out[i][j] = packed[k]
            out[j][i] = packed[k]
            k += 1
    return out


# ───────────────────────────────────────────────────────────────────
# Plan / observation lookup
# ───────────────────────────────────────────────────────────────────


def _orbfit_designation(obj_name: str) -> str:
    """Coerce a plan-row object name into a filesystem/Fortran-safe token.

    neofit2.x keys its scratch files on this token (`epoch/<desig>.eq0`,
    `mpcobs/<desig>.psv`); it does not need to match the packed
    designation inside the observations (the MPC's own unit tests file
    `2021UA12` observations under the packed id `K21U12A`)."""
    return re.sub(r"[^A-Za-z0-9]", "", obj_name)


def _lookup_ic(
    object_name: str,
    rows_by_object: Dict[str, List[Dict[str, Any]]],
) -> Optional[Dict[str, Any]]:
    """Find the initial-condition seed for an object.

    OD plan rows carry no IC (`od_plan_row` leaves them null — the epoch
    is filled by the OD runner). The object's propagation / ephemeris
    rows carry the same IC (`ic_pos_au`, `ic_vel_au_d`, `epoch_mjd_tdb`,
    `ic_a*`) at the object's reference epoch. Prefer an IC on the OD row
    itself (a future plan may add it), else take the first sibling row
    that has one.

    The Sun's state at the IC epoch comes from the registered DE440
    (`process_od_row`), not from the plan.
    """
    ic: Optional[Dict[str, Any]] = None
    for row in rows_by_object.get(object_name, []):
        if row.get("ic_pos_au") and row.get("ic_vel_au_d"):
            ic = {
                "ic_pos_au": row["ic_pos_au"],
                "ic_vel_au_d": row["ic_vel_au_d"],
                "epoch_mjd_tdb": row["epoch_mjd_tdb"],
                "ic_a1": row.get("ic_a1"),
                "ic_a2": row.get("ic_a2"),
                "ic_a3": row.get("ic_a3"),
            }
            break
    return ic


def _is_nongrav(ic: Dict[str, Any]) -> bool:
    """True if the IC carries a non-gravitational signal (A1/A2/A3)."""
    for key in ("ic_a1", "ic_a2", "ic_a3"):
        v = ic.get(key)
        if v is not None and v != 0.0:
            return True
    return False


# ───────────────────────────────────────────────────────────────────
# Row processing
# ───────────────────────────────────────────────────────────────────


# OrbFit's configuration, stamped on every row (merge-external carries it to the
# report's Configurations appendix).
ORBFIT_SETTINGS = {
    "program": "neofit2.x from the minorplanetcenter/orbfit image",
    "options": "the image's unit_tests neofit.nop.std (gravity) or neofit.nop.ngr "
    "(non-grav objects)",
    "rejection": "OrbFit's own CMC2003 outlier rejection",
    "seed": "the plan's initial state, heliocentric where the plan carries the Sun's state",
    "perturbers": "the planets and the image's AST17 massive asteroids; a body the plan "
    "excludes is left out by neofit2.x's own self-perturbation check",
    "self_perturbers": "",  # the plan's excluded AST17 bodies, left out; set in main()
}


def orbfit_source_version(image: str) -> str:
    """Provenance string stamped on every orbfit-channel row.

    OrbFit runs from the ``minorplanetcenter/orbfit`` Docker container; the
    image's content id (sha256) pins the exact ``neofit2.x`` build. No-hidden-
    fallbacks: when the id can't be resolved (docker missing / image not
    pulled), stamp an explicit ``orbfit unknown (<reason>)`` rather than a
    silent blank.
    """
    try:
        proc = subprocess.run(
            ["docker", "image", "inspect", "--format", "{{.Id}}", image],
            capture_output=True,
            text=True,
            timeout=30,
        )
    except Exception as e:  # noqa: BLE001
        return f"orbfit unknown ({e})"
    if proc.returncode != 0:
        tail = (proc.stderr or "").strip().splitlines()
        reason = tail[-1] if tail else f"docker inspect rc={proc.returncode}"
        return f"orbfit unknown ({reason})"
    image_id = (proc.stdout or "").strip()
    if not image_id:
        return "orbfit unknown (empty image id)"
    digest = image_id.split(":", 1)[1] if ":" in image_id else image_id
    return f"orbfit {digest[:12]}"


def process_od_row(
    row: Dict[str, Any],
    rows_by_object: Dict[str, List[Dict[str, Any]]],
    psv_dir: Path,
    image: str,
    platform: str,
    timeout_s: float,
    source_version: str,
    sun: SunEphemeris,
    massive: list[str],
) -> Dict[str, Any]:
    """Refit one OD object through OrbFit and decorate its row.

    `massive` is neofit2.x's massive-asteroid names (`read_massive_asteroids`):
    an AST17 body the row excludes is left out by running the object under its
    name, and a row that excludes more than one is a named unsupported row,
    never run. On any failure the returned row carries `orbfit_error` (a
    specific, loud message) with the fit fields left unpopulated — never a
    silent skip or defaulted value.
    """
    out = dict(row)
    out["channel"] = "orbfit"
    out["source_version"] = source_version
    out["orbfit_version"] = source_version
    out["orbfit_settings"] = ORBFIT_SETTINGS
    object_name = row.get("object", "")

    def fail(msg: str, t_ms: Optional[float] = None) -> Dict[str, Any]:
        print(f"    ERROR [{object_name}]: {msg}", file=sys.stderr, flush=True)
        out["orbfit_error"] = msg
        if t_ms is not None:
            out["orbfit_time_ms"] = t_ms
        return out

    # ── the bodies the plan excludes from this object's forces ──────
    leave_out, unsupported = orbfit_exclusion(row, massive)
    if unsupported:
        print(f"    unsupported [{object_name}]: {unsupported}", file=sys.stderr, flush=True)
        out["orbfit_unsupported"] = unsupported
        return out

    # ── observations ────────────────────────────────────────────────
    psv_path = psv_dir / f"{object_name}.psv"
    if not psv_path.exists():
        return fail(f"no observation fixture at {psv_path}")
    obs_psv = psv_path.read_text()

    # ── initial-condition seed ──────────────────────────────────────
    ic = _lookup_ic(object_name, rows_by_object)
    if ic is None:
        return fail(
            "no initial condition in plan (need ic_pos_au/ic_vel_au_d on "
            "this object's propagation/ephemeris rows)"
        )

    # Run under the excluded body's own name, so neofit2.x leaves it out.
    desig = leave_out if leave_out is not None else _orbfit_designation(object_name)
    is_ngr = _is_nongrav(ic)

    # OrbFit's osculating elements are heliocentric; empyrean's IC is
    # SSB-centred. Subtract the Sun's SSB state at the IC epoch from the
    # registered DE440 (see the module docstring).
    try:
        sun_pos, sun_vel = sun.state(ic["epoch_mjd_tdb"] + 2_400_000.5)
    except SpkError as e:
        return fail(f"no Sun state at the IC epoch: {e}")
    seed_pos = [p - s for p, s in zip(ic["ic_pos_au"], sun_pos)]
    seed_vel = [v - s for v, s in zip(ic["ic_vel_au_d"], sun_vel)]
    out["orbfit_seed_origin"] = "heliocentric (Sun from the registered DE440)"
    seed_eq0 = write_eq0_seed(desig, seed_pos, seed_vel, ic["epoch_mjd_tdb"])

    t0 = time.monotonic()
    res, err = run_neofit_container(
        desig=desig,
        obs_psv=obs_psv,
        seed_eq0=seed_eq0,
        is_ngr=is_ngr,
        image=image,
        platform=platform,
        timeout_s=timeout_s,
        leave_out=leave_out,
    )
    elapsed_ms = (time.monotonic() - t0) * 1000.0

    if res is None:
        return fail(err or "neofit2.x failed", t_ms=elapsed_ms)

    out["orbfit_rms_arcsec"] = res["rms_arcsec"]
    out["orbfit_n_obs_used"] = res["n_obs_used"]
    out["orbfit_n_obs_rejected"] = res["n_obs_rejected"]
    out["orbfit_n_obs_total"] = res["n_obs_total"]
    out["orbfit_orbit_pos_au"] = res["orbit_pos_au"]
    out["orbfit_orbit_vel_au_d"] = res["orbit_vel_au_d"]
    out["orbfit_epoch_mjd_tdb"] = res["epoch_mjd_tdb"]
    out["orbfit_covariance_6x6"] = res["covariance_6x6_ecl"]
    out["orbfit_is_nongrav"] = is_ngr
    out["orbfit_nongrav_a"] = res["nongrav_a"]
    out["orbfit_error_model"] = res["error_model"]
    if leave_out is not None:
        out["orbfit_left_out_naif"] = [NAIF_ASTEROID_OFFSET + int(leave_out)]
    out["orbfit_time_ms"] = elapsed_ms
    out.pop("orbfit_error", None)
    return out


# ───────────────────────────────────────────────────────────────────
# Entry point
# ───────────────────────────────────────────────────────────────────


def _default_psv_dir() -> Path:
    return Path(__file__).resolve().parents[2] / "fixtures" / "psv"


def select_od_rows(rows: List[Dict[str, Any]]) -> List[Dict[str, Any]]:
    """One orbit-determination row per object: the plan's untagged fit.

    The per-method plan carries one `orbit_determination` row per uncertainty
    method per object (seven or eight rows an object) so the engine channels
    can produce every method's products. OrbFit fits one orbit per object;
    replaying every method row re-ran the same fit seven times (354 fits for
    50 objects — run 169 reached 115 in three hours). The untagged row
    (`propagation_uncertainty` null) is the canonical fit the external
    references compare against; an object without one falls back to its first
    OD row, in plan order, so no object is dropped.
    """
    by_object: Dict[str, List[Dict[str, Any]]] = {}
    for r in rows:
        if r.get("test_type") == "orbit_determination":
            by_object.setdefault(str(r.get("object", "")), []).append(r)
    selected: List[Dict[str, Any]] = []
    for rows_for_object in by_object.values():
        untagged = [r for r in rows_for_object if r.get("propagation_uncertainty") is None]
        selected.append(untagged[0] if untagged else rows_for_object[0])
    return selected


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Replay validation-plan OD rows through OrbFit (neofit2.x).",
    )
    parser.add_argument("--plan", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument(
        "--psv-dir",
        type=Path,
        default=_default_psv_dir(),
        help="ADES PSV observation fixtures (default: ../../fixtures/psv)",
    )
    parser.add_argument(
        "--de440",
        required=True,
        type=Path,
        help="de440.bsp from the orbfit/ folder of the third-party tool set: the "
        "Sun's barycentric state that makes the seed heliocentric",
    )
    parser.add_argument("--image", default=DEFAULT_IMAGE)
    parser.add_argument(
        "--platform",
        default=DEFAULT_PLATFORM,
        help="docker --platform (Apple Silicon must use linux/amd64)",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=600.0,
        help="per-row neofit2.x timeout in seconds (default: 600)",
    )
    parser.add_argument(
        "--only",
        default="",
        help="comma-separated object names to restrict to (testing)",
    )
    parser.add_argument(
        "--limit",
        type=int,
        default=0,
        help="cap the number of OD rows processed (0 = all)",
    )
    parser.add_argument(
        "--defer-all-failed",
        action="store_true",
        help="this run is one object leg of the external schedule: exit 0 once "
        "the rows are written, and leave the every-object-failed verdict to the "
        "merge of all legs (scripts/channel-chunks.py concat-external)",
    )
    args = parser.parse_args()

    if not args.plan.exists():
        print(f"error: plan not found at {args.plan}", file=sys.stderr)
        return 2
    if not args.psv_dir.exists():
        print(f"error: PSV fixture dir not found at {args.psv_dir}", file=sys.stderr)
        return 2

    try:
        sun = SunEphemeris(args.de440)
    except SpkError as e:
        print(f"error: {e}", file=sys.stderr)
        return 2
    de440_sha = sun.sha256()
    ORBFIT_SETTINGS["seed"] = (
        "the plan's initial state made heliocentric with the Sun from "
        f"{args.de440.name} (sha256 {de440_sha[:16]})"
    )
    print(f"OrbFit runner ({args.image}, --platform {args.platform})")
    print(f"  sun:  {args.de440} (sha256 {de440_sha})")
    print(f"  plan: {args.plan}")
    print(f"  psv:  {args.psv_dir}")
    with open(args.plan) as f:
        plan = json.load(f)
    rows = plan if isinstance(plan, list) else plan.get("rows", [])

    rows_by_object: Dict[str, List[Dict[str, Any]]] = {}
    for r in rows:
        rows_by_object.setdefault(r.get("object", ""), []).append(r)

    od_rows = select_od_rows(rows)
    if args.only:
        want = {s.strip() for s in args.only.split(",") if s.strip()}
        unknown = sorted(want - {str(r.get("object", "")) for r in rows})
        if not want or unknown:
            print(f"error: --only names objects the plan does not hold: {unknown}", file=sys.stderr)
            return 2
        od_rows = [r for r in od_rows if r.get("object") in want]
    if args.limit > 0:
        od_rows = od_rows[: args.limit]
    print(f"  {len(rows)} total rows, {len(od_rows)} OD rows to replay")

    source_version = orbfit_source_version(args.image)
    print(f"  source_version: {source_version}")

    # The massive asteroids every fit integrates, read from the image; the
    # bodies the plan excludes from an object's forces are mapped onto them.
    try:
        massive = read_massive_asteroids(args.image, args.platform)
    except (ValueError, OSError, subprocess.SubprocessError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 2
    ORBFIT_SETTINGS["perturbers"] = (
        f"the planets and the {len(massive)} massive asteroids of the image's AST17 table "
        f"({', '.join(massive)}); a body the plan excludes from an object's forces is left "
        "out by neofit2.x's own self-perturbation check, the object run under that body's name"
    )
    left_out = []
    for r in od_rows:
        name, _ = orbfit_exclusion(r, massive)
        if name is not None:
            left_out.append(f"{r.get('object')} ({name})")
    ORBFIT_SETTINGS["self_perturbers"] = ", ".join(left_out) or "none"
    print(f"  massive asteroids: {', '.join(massive)}")
    print(f"  left out by neofit2.x's self-perturbation check: {ORBFIT_SETTINGS['self_perturbers']}")

    out_rows: List[Dict[str, Any]] = []
    timings: List[float] = []
    failures = 0
    n_unsupported = 0
    for i, row in enumerate(od_rows):
        name = row.get("object", "?")
        print(f"  [{i + 1}/{len(od_rows)}] {name} ...", flush=True)
        decorated = process_od_row(
            row,
            rows_by_object,
            args.psv_dir,
            image=args.image,
            platform=args.platform,
            timeout_s=args.timeout,
            source_version=source_version,
            sun=sun,
            massive=massive,
        )
        out_rows.append(decorated)
        if decorated.get("orbfit_error"):
            failures += 1
        elif decorated.get("orbfit_unsupported"):
            n_unsupported += 1
        else:
            print(
                f'        rms={decorated["orbfit_rms_arcsec"]:.4f}" '
                f"used={decorated['orbfit_n_obs_used']} "
                f"rej={decorated['orbfit_n_obs_rejected']} "
                f"ngr={decorated['orbfit_is_nongrav']} "
                f"({decorated['orbfit_time_ms'] / 1000:.1f}s)",
                flush=True,
            )
        if "orbfit_time_ms" in decorated:
            timings.append(decorated["orbfit_time_ms"])

    fits = len(out_rows) - failures - n_unsupported
    print(
        f"\nOrbFit runner: {len(out_rows)} rows emitted "
        f"({fits} fits, {failures} failures, {n_unsupported} named unsupported)"
    )
    if timings:
        print(
            f"  per-row wall-clock: median {statistics.median(timings) / 1000:.1f}s, "
            f"mean {statistics.mean(timings) / 1000:.1f}s"
        )

    args.output.parent.mkdir(parents=True, exist_ok=True)
    with open(args.output, "w") as f:
        try:
            _payload = json.dumps(out_rows, indent=2, allow_nan=False)
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
    print(f"  wrote {args.output}")
    # Non-zero exit only if no object was fitted — a partial failure set is
    # still useful output and is surfaced per-row via orbfit_error (or
    # orbfit_unsupported). One leg of a chunked run defers the verdict to the
    # merge of all its legs.
    if args.defer_all_failed:
        return 0
    return 1 if out_rows and fits == 0 else 0


if __name__ == "__main__":
    sys.exit(main())
