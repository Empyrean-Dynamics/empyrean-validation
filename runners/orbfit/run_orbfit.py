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
     Sun's SSB state at the IC epoch (the plan's `ref_sun_pos_au` /
     `ref_sun_vel_au_d`, carried on the dt=0 propagation row), then rotate
     ICRF -> ecliptic-J2000 (OrbFit's `ECLM J2000` reference system) and
     write an OEF2.0 `CAR` seed at `epoch/<desig>.eq0`.

     The SSB -> heliocentric shift (<= 0.008 AU) is not cosmetic: for
     deep-Earth-approaching NEOs it is a large fraction of the encounter
     distance (~22% of Apophis's 0.029 AU 2020 approach), and seeding
     with the raw SSB state corrupts the two-body encounter segments of
     `neofit2.x`'s initial propagation (`ever_pitkin` overflow, singular
     normal matrix) so the differential correction never converges. With
     the Sun subtracted, Apophis fits cleanly. Objects with no deep
     encounter (e.g. Eros) tolerate the raw SSB seed, so when the plan
     lacks a Sun state the runner falls back to SSB, records
     `orbfit_seed_origin="ssb"`, and warns — a fallback that only ever
     fails *loudly* (deep encounters overflow and surface as
     `orbfit_error`), never silently.

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

# Container identity. Override via --image for testing alternate tags.
DEFAULT_IMAGE = "minorplanetcenter/orbfit:latest"

# OrbFit's WORKDIR inside the container. Its bin/lib/ast_files symlinks are
# relative to this, so we mount our scratch as a sibling subdirectory
# (`/sa/god_fit/work`); `../lib`, `../ast_files`, `../src` all resolve.
CONTAINER_WORKDIR = "/sa/god_fit"

# The MPC image is amd64-only; Apple Silicon hosts must emulate.
DEFAULT_PLATFORM = "linux/amd64"

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

    `ic_pos_au` / `ic_vel_au_d` are the ICRF Cartesian seed state at
    `epoch_mjd_tdb` — heliocentric where the caller could subtract the
    Sun's SSB state, else raw SSB (see `process_od_row`). They are
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
) -> Tuple[Optional[Dict[str, Any]], Optional[str]]:
    """Refit one OD case through `neofit2.x`.

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

    Also finds the Sun's SSB state at the IC epoch (`ref_sun_pos_au` /
    `ref_sun_vel_au_d` on the dt=0 propagation row, where the target
    epoch equals the IC epoch) so the caller can build a heliocentric
    seed. `sun_pos_au` / `sun_vel_au_d` are `None` when the plan carries
    no Sun state (older plans).
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
                "sun_pos_au": None,
                "sun_vel_au_d": None,
            }
            break
    if ic is None:
        return None

    epoch = ic["epoch_mjd_tdb"]
    for row in rows_by_object.get(object_name, []):
        if (
            row.get("ref_sun_pos_au")
            and row.get("ref_sun_vel_au_d")
            and abs(row.get("t_mjd_tdb", epoch + 1e9) - epoch) < 1e-6
        ):
            ic["sun_pos_au"] = row["ref_sun_pos_au"]
            ic["sun_vel_au_d"] = row["ref_sun_vel_au_d"]
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


def process_od_row(
    row: Dict[str, Any],
    rows_by_object: Dict[str, List[Dict[str, Any]]],
    psv_dir: Path,
    image: str,
    platform: str,
    timeout_s: float,
) -> Dict[str, Any]:
    """Refit one OD object through OrbFit and decorate its row.

    On any failure the returned row carries `orbfit_error` (a specific,
    loud message) with the fit fields left unpopulated — never a silent
    skip or defaulted value.
    """
    out = dict(row)
    out["channel"] = "orbfit"
    object_name = row.get("object", "")

    def fail(msg: str, t_ms: Optional[float] = None) -> Dict[str, Any]:
        print(f"    ERROR [{object_name}]: {msg}", file=sys.stderr, flush=True)
        out["orbfit_error"] = msg
        if t_ms is not None:
            out["orbfit_time_ms"] = t_ms
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

    desig = _orbfit_designation(object_name)
    is_ngr = _is_nongrav(ic)

    # OrbFit's osculating elements are heliocentric; empyrean's IC is
    # SSB-centered. Subtract the Sun's SSB state when the plan carries
    # it, else fall back to the raw SSB state (works for non-deep
    # objects; deep-encounter objects then fail loudly — see docstring).
    if ic["sun_pos_au"] is not None:
        seed_pos = [p - s for p, s in zip(ic["ic_pos_au"], ic["sun_pos_au"])]
        seed_vel = [v - s for v, s in zip(ic["ic_vel_au_d"], ic["sun_vel_au_d"])]
        seed_origin = "heliocentric"
    else:
        seed_pos = list(ic["ic_pos_au"])
        seed_vel = list(ic["ic_vel_au_d"])
        seed_origin = "ssb"
        print(
            f"    WARN [{object_name}]: no Sun SSB state in plan; seeding from "
            "raw SSB state (deep-encounter objects may fail to converge)",
            file=sys.stderr,
            flush=True,
        )
    out["orbfit_seed_origin"] = seed_origin
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
    out["orbfit_time_ms"] = elapsed_ms
    out.pop("orbfit_error", None)
    return out


# ───────────────────────────────────────────────────────────────────
# Entry point
# ───────────────────────────────────────────────────────────────────


def _default_psv_dir() -> Path:
    return Path(__file__).resolve().parents[2] / "fixtures" / "psv"


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
    args = parser.parse_args()

    if not args.plan.exists():
        print(f"error: plan not found at {args.plan}", file=sys.stderr)
        return 2
    if not args.psv_dir.exists():
        print(f"error: PSV fixture dir not found at {args.psv_dir}", file=sys.stderr)
        return 2

    print(f"OrbFit runner ({args.image}, --platform {args.platform})")
    print(f"  plan: {args.plan}")
    print(f"  psv:  {args.psv_dir}")
    with open(args.plan) as f:
        plan = json.load(f)
    rows = plan if isinstance(plan, list) else plan.get("rows", [])

    rows_by_object: Dict[str, List[Dict[str, Any]]] = {}
    for r in rows:
        rows_by_object.setdefault(r.get("object", ""), []).append(r)

    od_rows = [r for r in rows if r.get("test_type") == "orbit_determination"]
    if args.only:
        want = {s.strip() for s in args.only.split(",") if s.strip()}
        od_rows = [r for r in od_rows if r.get("object") in want]
    if args.limit > 0:
        od_rows = od_rows[: args.limit]
    print(f"  {len(rows)} total rows, {len(od_rows)} OD rows to replay")

    out_rows: List[Dict[str, Any]] = []
    timings: List[float] = []
    failures = 0
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
        )
        out_rows.append(decorated)
        if decorated.get("orbfit_error"):
            failures += 1
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

    fits = len(out_rows) - failures
    print(
        f"\nOrbFit runner: {len(out_rows)} rows emitted "
        f"({fits} fits, {failures} failures)"
    )
    if timings:
        print(
            f"  per-row wall-clock: median {statistics.median(timings) / 1000:.1f}s, "
            f"mean {statistics.mean(timings) / 1000:.1f}s"
        )

    args.output.parent.mkdir(parents=True, exist_ok=True)
    with open(args.output, "w") as f:
        json.dump(out_rows, f, indent=2)
    print(f"  wrote {args.output}")
    # Non-zero exit only if every object failed — a partial failure set is
    # still useful output and is surfaced per-row via orbfit_error.
    return 1 if out_rows and fits == 0 else 0


if __name__ == "__main__":
    sys.exit(main())
