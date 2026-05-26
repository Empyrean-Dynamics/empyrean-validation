"""empyrean validation runner — Python channel.

Reads a rust-channel ValidationResult JSON for the inputs (initial
conditions and reference vectors), replays each propagation through
the empyrean Python wheel, and emits a python-channel JSON in the
same schema. The combined JSONs feed into

    validate report --results validation_rust.json,validation_python.json

so the Distribution Channel Fidelity table can compare bindings.
"""

from __future__ import annotations

import argparse
import json
import math
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

import numpy as np

import empyrean
from empyrean._empyrean_rs import (
    _determine,
    _generate_ephemeris,
    _get_observers,
    _propagate,
)


# Mirrors empyrean::ForceModelTier integer encoding.
_TIER_TO_INT = {"approximate": 0, "basic": 1, "standard": 2}

# Frame integer mirrors empyrean wrapper: ICRF=0, EclipticJ2000=1.
_FRAME_ICRF = 0
_REP_CARTESIAN = 0
_AU_KM = 149_597_870.700


def _ensure_initialized(data_dir: str | None) -> None:
    if data_dir:
        empyrean.initialize(data_dir=data_dir)
    else:
        empyrean.initialize()


def _propagate_one(
    object_id: str,
    population: str,
    epoch_mjd_tdb: float,
    target_t_mjd_tdb: float,
    ic_pos_au: list[float],
    ic_vel_au_d: list[float],
    ic_a1: float,
    ic_a2: float,
    ic_a3: float,
    ic_g_alpha: float,
    ic_g_r0: float,
    ic_g_m: float,
    ic_g_n: float,
    ic_g_k: float,
    ic_non_grav_dt: float | None,
    force_model: str,
    n_timing_runs: int,
) -> tuple[list[float], float] | None:
    """Run propagation once, return (out_pos_au, min_time_ms) or None on failure."""
    tier = _TIER_TO_INT.get(force_model)
    if tier is None:
        return None

    times = np.array([target_t_mjd_tdb], dtype=np.float64)
    epochs = np.array([epoch_mjd_tdb], dtype=np.float64)
    elements = np.array(
        [[ic_pos_au[0], ic_pos_au[1], ic_pos_au[2], ic_vel_au_d[0], ic_vel_au_d[1], ic_vel_au_d[2]]],
        dtype=np.float64,
    )
    covariances = np.zeros((1, 6, 6), dtype=np.float64)
    has_covariance = np.array([False])
    representations = np.array([_REP_CARTESIAN], dtype=np.int32)
    frames = np.array([_FRAME_ICRF], dtype=np.int32)
    origins = np.array([0], dtype=np.int32)  # SSB
    a1s = np.array([ic_a1 or 0.0], dtype=np.float64)
    a2s = np.array([ic_a2 or 0.0], dtype=np.float64)
    a3s = np.array([ic_a3 or 0.0], dtype=np.float64)
    phot_h = np.array([np.nan], dtype=np.float64)
    phot_slope1 = np.array([np.nan], dtype=np.float64)
    phot_system = np.array([-1], dtype=np.int32)
    # g(r) parameters: pass through if any are non-zero, else let the
    # binding default to inverse_square.
    has_g = any(v != 0.0 for v in (ic_g_alpha, ic_g_r0, ic_g_m, ic_g_n, ic_g_k))
    ng_alphas = np.array([ic_g_alpha], dtype=np.float64) if has_g else None
    ng_r0s = np.array([ic_g_r0], dtype=np.float64) if has_g else None
    ng_ms = np.array([ic_g_m], dtype=np.float64) if has_g else None
    ng_ns = np.array([ic_g_n], dtype=np.float64) if has_g else None
    ng_ks = np.array([ic_g_k], dtype=np.float64) if has_g else None
    # SBDB non-grav DT (days) — populated for Jupiter-family comets +
    # 2I/Borisov; NaN means "no delay". Pass through as a length-1
    # array when populated, else None to skip the FFI marshal.
    non_grav_dts = (
        np.array([ic_non_grav_dt], dtype=np.float64)
        if ic_non_grav_dt is not None
        else None
    )

    timings_ms = []
    last_result = None
    for _ in range(max(1, n_timing_runs)):
        t0 = time.perf_counter()
        try:
            result = _propagate(
                orbit_ids=[object_id],
                object_ids=[object_id],
                epochs=epochs,
                elements=elements,
                covariances=covariances,
                has_covariance=has_covariance,
                representations=representations,
                frames=frames,
                origins=origins,
                times_mjd_tdb=times,
                force_model=tier,
                uncertainty_method=0,
                a1s=a1s,
                a2s=a2s,
                a3s=a3s,
                phot_h=phot_h,
                phot_slope1=phot_slope1,
                phot_system=phot_system,
                ng_alphas=ng_alphas,
                ng_r0s=ng_r0s,
                ng_ms=ng_ms,
                ng_ns=ng_ns,
                ng_ks=ng_ks,
                non_grav_dts=non_grav_dts,
            )
        except Exception as e:  # noqa: BLE001
            print(f"  {object_id} {force_model} dt→{target_t_mjd_tdb}: FAIL {e}", file=sys.stderr)
            return None
        timings_ms.append((time.perf_counter() - t0) * 1000.0)
        last_result = result

    if last_result is None or len(last_result.get("x", [])) == 0:
        return None
    pos = [
        float(last_result["x"][0]),
        float(last_result["y"][0]),
        float(last_result["z"][0]),
    ]
    return pos, min(timings_ms)


def _ephemeris_one(
    object_id: str,
    epoch_mjd_tdb: float,
    target_t_mjd_tdb: float,
    ic_pos_au: list[float],
    ic_vel_au_d: list[float],
    ic_a1: float,
    ic_a2: float,
    ic_a3: float,
    ic_g_alpha: float,
    ic_g_r0: float,
    ic_g_m: float,
    ic_g_n: float,
    ic_g_k: float,
    ic_non_grav_dt: float | None,
    obs_code: str,
    force_model: str,
) -> tuple[float, float, float, float] | None:
    """Generate ephemeris at target_t for obs_code; return (ra_rad, dec_rad, rho_au, light_time_days)."""
    tier = _TIER_TO_INT.get(force_model)
    if tier is None:
        return None

    obs_states = _get_observers([obs_code], np.array([target_t_mjd_tdb], dtype=np.float64))
    if len(obs_states.get("x", [])) == 0:
        return None

    obs_x = np.array(obs_states["x"], dtype=np.float64)
    obs_y = np.array(obs_states["y"], dtype=np.float64)
    obs_z = np.array(obs_states["z"], dtype=np.float64)
    obs_vx = np.array(obs_states["vx"], dtype=np.float64)
    obs_vy = np.array(obs_states["vy"], dtype=np.float64)
    obs_vz = np.array(obs_states["vz"], dtype=np.float64)
    obs_epochs = np.array([target_t_mjd_tdb], dtype=np.float64)

    epochs = np.array([epoch_mjd_tdb], dtype=np.float64)
    elements = np.array(
        [[ic_pos_au[0], ic_pos_au[1], ic_pos_au[2], ic_vel_au_d[0], ic_vel_au_d[1], ic_vel_au_d[2]]],
        dtype=np.float64,
    )
    covariances = np.zeros((1, 6, 6), dtype=np.float64)
    has_covariance = np.array([False])
    representations = np.array([_REP_CARTESIAN], dtype=np.int32)
    frames = np.array([_FRAME_ICRF], dtype=np.int32)
    origins = np.array([0], dtype=np.int32)
    a1s = np.array([ic_a1 or 0.0], dtype=np.float64)
    a2s = np.array([ic_a2 or 0.0], dtype=np.float64)
    a3s = np.array([ic_a3 or 0.0], dtype=np.float64)
    phot_h = np.array([np.nan], dtype=np.float64)
    phot_slope1 = np.array([np.nan], dtype=np.float64)
    phot_system = np.array([-1], dtype=np.int32)
    has_g = any(v != 0.0 for v in (ic_g_alpha, ic_g_r0, ic_g_m, ic_g_n, ic_g_k))
    ng_alphas = np.array([ic_g_alpha], dtype=np.float64) if has_g else None
    ng_r0s = np.array([ic_g_r0], dtype=np.float64) if has_g else None
    ng_ms = np.array([ic_g_m], dtype=np.float64) if has_g else None
    ng_ns = np.array([ic_g_n], dtype=np.float64) if has_g else None
    ng_ks = np.array([ic_g_k], dtype=np.float64) if has_g else None
    non_grav_dts = (
        np.array([ic_non_grav_dt], dtype=np.float64)
        if ic_non_grav_dt is not None
        else None
    )

    try:
        result = _generate_ephemeris(
            orbit_ids=[object_id],
            object_ids=[object_id],
            epochs=epochs,
            elements=elements,
            covariances=covariances,
            has_covariance=has_covariance,
            representations=representations,
            frames=frames,
            origins=origins,
            a1s=a1s,
            a2s=a2s,
            a3s=a3s,
            phot_h=phot_h,
            phot_slope1=phot_slope1,
            phot_system=phot_system,
            obs_codes=[obs_code],
            obs_epochs=obs_epochs,
            obs_x=obs_x,
            obs_y=obs_y,
            obs_z=obs_z,
            obs_vx=obs_vx,
            obs_vy=obs_vy,
            obs_vz=obs_vz,
            force_model=tier,
            ng_alphas=ng_alphas,
            ng_r0s=ng_r0s,
            ng_ms=ng_ms,
            ng_ns=ng_ns,
            ng_ks=ng_ks,
            non_grav_dts=non_grav_dts,
        )
    except Exception as e:  # noqa: BLE001
        print(f"  {object_id} ephemeris t={target_t_mjd_tdb}: FAIL {e}", file=sys.stderr)
        return None

    if len(result.get("ra", [])) == 0:
        return None

    ra_deg = float(result["ra"][0])
    dec_deg = float(result["dec"][0])
    rho_au = float(result["rho"][0])
    lt_d = float(result["light_time"][0]) if "light_time" in result else math.nan

    return math.radians(ra_deg), math.radians(dec_deg), rho_au, lt_d


def _determine_one(
    object_id: str,
    psv_text: str,
    force_model: str,
    max_iterations: int,
    excluded_perturbers_naif: list[int] | None = None,
) -> tuple[list[float], dict, float] | None:
    """Run OD on the PSV PSV-text via the wheel.

    Returns (fitted_orbit_pos_au, raw_result_dict, time_ms) or None.
    """
    if force_model not in _TIER_TO_INT:
        return None

    obs_dict = {"ades": psv_text}
    # Mirror scott::od::ODConfig::default(): solve_for=Auto so comets can
    # escalate to a non-grav fit. Inherit convergence_tol from the library
    # default (1e-3, sigma-quality) — overriding it here would diverge
    # this channel from the rust / c / cli / core channels.
    config_dict = {
        "force_model": force_model,
        "max_iterations": max_iterations,
        "solve_for": "auto",
    }
    if excluded_perturbers_naif:
        config_dict["excluded_perturbers_naif"] = list(excluded_perturbers_naif)
    t0 = time.perf_counter()
    try:
        result = _determine(
            obs_dict=obs_dict,
            config_dict=config_dict,
            initial_orbits_dict=None,
        )
    except Exception as e:  # noqa: BLE001
        print(f"  {object_id} OD: FAIL {e}", file=sys.stderr)
        return None
    ms = (time.perf_counter() - t0) * 1000.0

    pos = [float(result["orbit_x"]), float(result["orbit_y"]), float(result["orbit_z"])]
    return pos, result, ms


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--input", required=True, type=Path, help="rust-channel JSON")
    p.add_argument(
        "--output",
        type=Path,
        default=Path("results/validation_python.json"),
        help="output python-channel JSON",
    )
    p.add_argument("--data-dir", type=str, default=None)
    p.add_argument("--n-timing-runs", type=int, default=3)
    p.add_argument(
        "--fixtures-dir",
        type=Path,
        default=Path(__file__).parent.parent.parent / "fixtures" / "psv",
        help="PSV fixture directory for OD rows",
    )
    args = p.parse_args()

    rust_rows = json.loads(args.input.read_text())
    if not rust_rows:
        print("input JSON is empty", file=sys.stderr)
        return 1

    _ensure_initialized(args.data_dir)
    print(f"Loaded {len(rust_rows)} rust rows; replaying through Python channel...", file=sys.stderr)

    timestamp = datetime.now(timezone.utc).isoformat()
    out_rows = []
    n_skipped = 0
    for r in rust_rows:
        ic_pos = r.get("ic_pos_au")
        ic_vel = r.get("ic_vel_au_d")
        # OD rows discover the orbit from observations — no IC required.
        if r["test_type"] != "orbit_determination" and (ic_pos is None or ic_vel is None):
            n_skipped += 1
            continue
        # Uncertainty axis: skip Jet1 rows for now. The PyO3 _propagate
        # entry doesn't accept a covariance arg yet; cross-channel Jet1
        # parity is a follow-up. Rust + core handle both modes today.
        if r.get("propagation_uncertainty") == "first_order_with_cov":
            n_skipped += 1
            continue

        new = dict(r)
        new["channel"] = "python"
        new["timestamp"] = timestamp
        # Reset all empyrean-output fields; we'll repopulate from the Python channel.
        for k in (
            "emp_vs_horizons_km",
            "emp_pos_au",
            "emp_time_ms",
            "separation_arcsec",
            "d_ra_arcsec",
            "d_dec_arcsec",
            "d_rho_km",
            "d_light_time_s",
        ):
            new[k] = None

        if r["test_type"] == "propagation":
            ret = _propagate_one(
                object_id=r["object"],
                population=r["population"],
                epoch_mjd_tdb=r["epoch_mjd_tdb"],
                target_t_mjd_tdb=r["t_mjd_tdb"],
                ic_pos_au=ic_pos,
                ic_vel_au_d=ic_vel,
                ic_a1=r.get("ic_a1") or 0.0,
                ic_a2=r.get("ic_a2") or 0.0,
                ic_a3=r.get("ic_a3") or 0.0,
                ic_g_alpha=r.get("ic_g_alpha") or 0.0,
                ic_g_r0=r.get("ic_g_r0") or 0.0,
                ic_g_m=r.get("ic_g_m") or 0.0,
                ic_g_n=r.get("ic_g_n") or 0.0,
                ic_g_k=r.get("ic_g_k") or 0.0,
                ic_non_grav_dt=r.get("ic_non_grav_dt"),
                force_model=r["force_model"],
                n_timing_runs=args.n_timing_runs,
            )
            if ret is None:
                n_skipped += 1
                continue
            pos, ms = ret
            new["emp_pos_au"] = pos
            new["emp_time_ms"] = ms
            ref = r.get("ref_pos_au")
            if ref:
                d = math.sqrt(sum((pos[i] - ref[i]) ** 2 for i in range(3)))
                new["emp_vs_horizons_km"] = d * _AU_KM

        elif r["test_type"] == "orbit_determination":
            psv_path = args.fixtures_dir / f"{r['object']}.psv"
            if not psv_path.exists():
                n_skipped += 1
                continue
            psv_text = psv_path.read_text()
            ret = _determine_one(
                object_id=r["object"],
                psv_text=psv_text,
                force_model=r["force_model"],
                max_iterations=100,
                excluded_perturbers_naif=r.get("excluded_perturbers_naif"),
            )
            if ret is None:
                n_skipped += 1
                continue
            pos, raw, ms = ret
            new["emp_pos_au"] = pos
            new["emp_time_ms"] = ms
            new["n_obs_used"] = int(raw.get("summary_num_selected", 0))
            new["od_iterations"] = int(raw.get("iterations", 0))
            new["od_converged"] = bool(raw.get("converged", False))
            new["od_rms_ra_arcsec"] = float(raw.get("summary_rms_ra", float("nan")))
            new["od_rms_dec_arcsec"] = float(raw.get("summary_rms_dec", float("nan")))
            new["od_rms_combined_arcsec"] = float(
                raw.get("summary_rms_combined", float("nan"))
            )
            new["od_chi2"] = float(raw.get("summary_chi2", float("nan")))
            new["od_reduced_chi2"] = float(raw.get("summary_reduced_chi2", float("nan")))

        elif r["test_type"] == "ephemeris":
            obs_code = r.get("observer")
            if not obs_code:
                n_skipped += 1
                continue
            ret = _ephemeris_one(
                object_id=r["object"],
                epoch_mjd_tdb=r["epoch_mjd_tdb"],
                target_t_mjd_tdb=r["t_mjd_tdb"],
                ic_pos_au=ic_pos,
                ic_vel_au_d=ic_vel,
                ic_a1=r.get("ic_a1") or 0.0,
                ic_a2=r.get("ic_a2") or 0.0,
                ic_a3=r.get("ic_a3") or 0.0,
                ic_g_alpha=r.get("ic_g_alpha") or 0.0,
                ic_g_r0=r.get("ic_g_r0") or 0.0,
                ic_g_m=r.get("ic_g_m") or 0.0,
                ic_g_n=r.get("ic_g_n") or 0.0,
                ic_g_k=r.get("ic_g_k") or 0.0,
                ic_non_grav_dt=r.get("ic_non_grav_dt"),
                obs_code=obs_code,
                force_model=r["force_model"],
            )
            if ret is None:
                n_skipped += 1
                continue
            ra_rad, dec_rad, rho_au, lt_d = ret
            ref_ra = r.get("ref_ra_rad")
            ref_dec = r.get("ref_dec_rad")
            ref_rho = r.get("ref_rho_au")
            ref_lt = r.get("ref_light_time_d")
            if ref_ra is not None and ref_dec is not None:
                cos_d1, cos_d2 = math.cos(dec_rad), math.cos(ref_dec)
                sin_d1, sin_d2 = math.sin(dec_rad), math.sin(ref_dec)
                dra = ref_ra - ra_rad
                num1 = cos_d2 * math.sin(dra)
                num2 = cos_d1 * sin_d2 - sin_d1 * cos_d2 * math.cos(dra)
                num = math.sqrt(num1**2 + num2**2)
                den = sin_d1 * sin_d2 + cos_d1 * cos_d2 * math.cos(dra)
                sep_arcsec = math.degrees(math.atan2(num, den)) * 3600.0
                new["separation_arcsec"] = sep_arcsec
                d_ra = (ra_rad - ref_ra) * cos_d1
                d_dec = dec_rad - ref_dec
                new["d_ra_arcsec"] = math.degrees(d_ra) * 3600.0
                new["d_dec_arcsec"] = math.degrees(d_dec) * 3600.0
            if ref_rho is not None:
                new["d_rho_km"] = (rho_au - ref_rho) * _AU_KM
            if ref_lt is not None and not math.isnan(lt_d):
                new["d_light_time_s"] = (lt_d - ref_lt) * 86400.0

        out_rows.append(new)

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(out_rows, indent=2, default=str))
    print(
        f"Wrote {len(out_rows)} python rows to {args.output} (skipped {n_skipped})",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
