"""adam-core external-reference runner for the empyrean validation suite.

Replays the canonical validation plan through the Asteroid Institute's adam_core
orbit-determination stack with the adam-assist ASSIST propagator. ONE channel,
tool id `adamcore`: adam-core with its OWN shipped defaults,
`run_od(models="default")` = SigmaFill(v2_sigma_fill) -> EFCC18 debias ->
EmpiricalCovariance(v2_full, add) -> NightBatch(cap 4), on the Institute's
observatory-uncertainty tables. Those tables are PRIVATE
(adam-observatory-uncertainties), so this runner is LOCAL-ONLY: CI skips it and
its results are not published.

Fitter `NativeOrbitFitter(ASSISTPropagator())` (cmc2003 outlier rejection),
`propagator=ASSISTPropagator()` (adam-assist defaults epsilon 1e-6, min_dt 1e-9,
adaptive_mode 1, initial_dt 1e-6 — the same numbers as the suite's
`assist_asteroid_institute` ASSIST arm).

Every row is a plan row plus `adamcore_*` fields; `channel` is `adamcore`. A row
carries the model NAMES (`adamcore_models`), the adam_core version
(`adamcore_version`), the data-package version (`adamcore_data_version`), the
force model (`adamcore_force_model`), the measured σ-source classification
(`adamcore_sigma_source`: reported / filled-by-level / defaulted_by_adamcore) and,
on a refusal, adam_core's own reason — never any σ value from the private tables.

No-hidden-fallbacks: every filled sigma, dropped space-based observation,
refused/timed-out fit and unsupported non-grav term is counted and carried on the
row with its reason.
"""

from __future__ import annotations

import argparse
import json
import math
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import adamcore_common as C  # noqa: E402

AU_KM = C.AU_KM


TOOL = "adamcore"

# ── model stack (adam-core's shipped default, on the Institute's tables) ─────


def build_models():
    """(models_arg, models_strings) for run_od: adam_core's own "default" stack."""
    return "default", [
        "SigmaFillModel(v2_sigma_fill)",
        "EFCC18DebiasModel",
        "EmpiricalCovarianceModel(v2_full,add)",
        "NightBatchDeweightingModel(cap=4)",
    ]


def build_models_concrete():
    """A concrete model list identical to adam_core's `models="default"`
    (_default_models): the four defaults, which load v2_sigma_fill / v2_full / the
    EFCC18 cache themselves. Used by the warm-start path, which applies the models
    before `refine`."""
    from adam_core.orbit_determination import (
        EFCC18DebiasModel,
        EmpiricalCovarianceModel,
        NightBatchDeweightingModel,
        SigmaFillModel,
    )

    return [
        SigmaFillModel(),
        EFCC18DebiasModel(),
        EmpiricalCovarianceModel(),
        NightBatchDeweightingModel(cap=4),
    ]


# ── propagation ──────────────────────────────────────────────


def propagation(row: dict, prop) -> dict:
    from adam_core.time import Timestamp

    out: dict = {"adamcore_force_model": C.force_model_label(row.get("object"))}
    orbit, nongrav_reason = C.plan_orbit(row)
    if nongrav_reason:
        out["adamcore_unsupported"] = nongrav_reason  # e.g. "nongrav dt"
    t0 = time.perf_counter()
    result = prop.propagate_orbits(orbit, Timestamp.from_mjd([row["t_mjd_tdb"]], scale="tdb"))
    out["adamcore_time_ms"] = (time.perf_counter() - t0) * 1000.0
    c = result.coordinates
    pos = [float(c.x[0].as_py()), float(c.y[0].as_py()), float(c.z[0].as_py())]
    if not C.finite(*pos):
        out["adamcore_unsupported"] = out.get("adamcore_unsupported") or "non-finite propagated state"
        return out
    out["adamcore_pos_au"] = pos
    ref = row.get("ref_pos_au")
    if ref is not None:
        d = math.sqrt(sum((pos[i] - ref[i]) ** 2 for i in range(3)))
        out["adamcore_vs_horizons_km"] = d * AU_KM
    return out


# ── ephemeris ────────────────────────────────────────────────


def ephemeris(row: dict, prop, resolvable) -> dict:
    from adam_core.observers import Observers
    from adam_core.time import Timestamp

    out: dict = {"adamcore_force_model": C.force_model_label(row.get("object"))}
    code = row.get("observer")
    if not code:
        return out
    if not resolvable(code):
        out["adamcore_unsupported"] = "space-based observer"
        return out
    orbit, nongrav_reason = C.plan_orbit(row)
    if nongrav_reason:
        out["adamcore_unsupported"] = nongrav_reason
    orbit = C.orbit_to_ecliptic(orbit)  # typed ephemeris needs an ecliptic Cartesian orbit
    times = Timestamp.from_mjd([row["t_mjd_tdb"]], scale="tdb")
    observers = Observers.from_code(code, times)
    t0 = time.perf_counter()
    eph = prop.generate_ephemeris(orbit, observers)
    out["adamcore_time_ms"] = (time.perf_counter() - t0) * 1000.0
    sc = eph.coordinates
    ra_rad = math.radians(float(sc.lon[0].as_py()))
    dec_rad = math.radians(float(sc.lat[0].as_py()))
    rho_au = float(sc.rho[0].as_py())
    if not C.finite(ra_rad, dec_rad, rho_au):
        out["adamcore_unsupported"] = out.get("adamcore_unsupported") or "non-finite ephemeris"
        return out
    ref_ra, ref_dec = row.get("ref_ra_rad"), row.get("ref_dec_rad")
    if ref_ra is not None and ref_dec is not None:
        out["adamcore_separation_arcsec"] = C.angular_sep_arcsec(ra_rad, dec_rad, ref_ra, ref_dec)
        out["adamcore_d_ra_arcsec"] = math.degrees((ra_rad - ref_ra) * math.cos(dec_rad)) * 3600.0
        out["adamcore_d_dec_arcsec"] = math.degrees(dec_rad - ref_dec) * 3600.0
    ref_rho = row.get("ref_rho_au")
    if ref_rho is not None:
        out["adamcore_d_rho_km"] = (rho_au - ref_rho) * AU_KM
    return out


# ── orbit determination ──────────────────────────────────────


def normalize_psv(text: str) -> str:
    """Strip ADES fixed-width padding (adam_core's parser rejects padded fields)."""
    out = []
    for line in text.splitlines():
        if line.startswith(("#", "!")) or not line.strip():
            out.append(line)
            continue
        out.append("|".join(c.strip() for c in line.split("|")))
    return "\n".join(out) + "\n"


def resolvable_fn():
    """A cached predicate: can adam_core place this observatory code (ground site)?

    Space-based / virtual codes (satellites, geocentric occultation) raise in
    Observers.from_code — they need satellite SPICE states that are not loaded —
    so they are the rows the OD channel drops and the ephemeris channel marks.
    """
    from adam_core.observers import Observers
    from adam_core.time import Timestamp

    t = Timestamp.from_mjd([60000.0], scale="tdb")
    cache: dict[str, bool] = {}

    def ok(code: str) -> bool:
        if code not in cache:
            try:
                o = Observers.from_code(code, t)
                v = o.coordinates.values[0][:3]
                cache[code] = all(math.isfinite(float(x)) for x in v)
            except Exception:  # noqa: BLE001
                cache[code] = False
        return cache[code]

    return ok


def build_odo(psv_text: str, ok):
    """Parse an ADES PSV, drop space-based/unresolvable stations, build an ODO.

    Returns (odo | None, n_dropped, droppable_stations, n_parsed, ades_rms_present),
    where ades_rms_present[i] is True iff the kept ADES row i carried BOTH
    rmsRACosDec and rmsDec (aligned to the ODO rows). Shared by the OD channel and
    the walk so the space-based policy is identical everywhere.
    """
    import pyarrow as pa
    import pyarrow.compute as pc
    from adam_core.observations.ades import ADES_string_to_tables
    from adam_core.orbit_determination import OrbitDeterminationObservations

    _ctx, ades = ADES_string_to_tables(normalize_psv(psv_text))
    stns = ades.stn.to_pylist()
    n_parsed = len(stns)
    droppable = sorted({s for s in set(stns) if not ok(s)})
    n_dropped = sum(1 for s in stns if s in set(droppable))
    if droppable:
        keep = pa.array(sorted(s for s in set(stns) if ok(s)))
        ades = ades.apply_mask(pc.is_in(ades.stn, value_set=keep))
    if len(ades) == 0:
        return None, n_dropped, droppable, n_parsed, []
    rra = ades.rmsRACosDec.to_pylist()
    rdec = ades.rmsDec.to_pylist()
    rms_present = [a is not None and b is not None for a, b in zip(rra, rdec)]
    return (
        OrbitDeterminationObservations.from_ades(ades),
        n_dropped, droppable, n_parsed, rms_present,
    )


def _sigma_source(odo, rms_present) -> tuple[dict, int]:
    """(sigma_source dict, defaulted_count), over the ORIGINAL observations straight
    after from_ades. An observation is `reported` when its lon variance is finite AND
    it carried an ADES rms; `defaulted_by_adamcore` when its variance is finite but it
    had NO ADES rms (adam_core injected a sigma — a hidden fallback the brief forbids);
    `filled` when its variance is NaN (SigmaFill supplies it, tagged by table level).
    `defaulted_by_adamcore` is MEASURED here, not assumed: from_ades is documented to
    leave a null rms as NaN, so it should be 0, and a non-zero count is surfaced loudly."""
    cov = odo.coordinates.covariance.to_matrix()  # (N, 6, 6); lon variance = [1][1]
    codes = odo.observers.code.to_pylist()
    cats = odo.astcat.to_pylist()
    sets = C.sigma_table_row_sets(C.data_sigma_table())
    reported = defaulted = 0
    by_level: dict[str, int] = {}
    n = len(odo)
    has_rms = rms_present if len(rms_present) == n else [True] * n
    for i in range(n):
        if math.isfinite(cov[i][1][1]):
            if has_rms[i]:
                reported += 1
            else:
                defaulted += 1  # finite variance with no ADES rms => adam_core default
        else:
            lvl = C.classify_fill_level(codes[i], cats[i], sets)
            by_level[lvl] = by_level.get(lvl, 0) + 1
    filled = sum(by_level.values())
    return {
        "reported": reported,
        "filled": filled,
        "filled_by_level": by_level,
        "defaulted_by_adamcore": defaulted,
        "total": n,
    }, defaulted


def od_one(obj: str, fixtures_dir: Path, versions: dict,
           mpc_designation: str | None = None) -> dict:
    """Fit one object's optical astrometry. Returns the adamcore_* fields (no plan row).

    Self-perturbers (Iris/Vesta/Pallas/Hygiea) are fit with the planets-only force
    model (ASSIST includes the object in SB441-n16, so the full model is self-singular
    and the fit times out). Each OD object runs in its own process, so constructing
    only the planets-only propagator here keeps this object's fit planets-only.
    """
    from adam_core.orbit_determination import NativeOrbitFitter, run_od

    out: dict = {"adamcore_force_model": C.force_model_label(obj)}
    psv, candidates = C.resolve_fixture(fixtures_dir, obj, mpc_designation)
    if psv is None:
        print(f"ERROR: {obj}: no PSV fixture found; tried {candidates}", file=sys.stderr)
        out["adamcore_unsupported"] = f"no PSV fixture (tried {candidates})"
        return out

    ok = resolvable_fn()
    odo, n_dropped, droppable, _n, rms_present = build_odo(psv.read_text(), ok)
    out["adamcore_n_obs_dropped_spacebased"] = n_dropped
    if n_dropped:
        out["adamcore_dropped_stations"] = droppable
    if odo is None:
        out["adamcore_unsupported"] = "all observations at space-based stations"
        out["adamcore_converged"] = False
        return out

    # sigma-source provenance, computed from the ORIGINAL observations straight
    # after from_ades — NOT from the fitter's members (whose original_astrometry
    # carries a value for every used row). `from_ades` yields a NaN lon/lat
    # variance for a null rms, so a finite-variance observation is `reported` when
    # it carried an ADES rms and `defaulted_by_adamcore` when it did not (measured,
    # never assumed); a NaN-variance observation is `filled` (tagged by level).
    out["adamcore_sigma_source"], n_defaulted = _sigma_source(odo, rms_present)
    if n_defaulted:
        print(f"WARNING: {obj}: adam_core assigned a finite covariance to {n_defaulted} "
              f"observation(s) with NO ADES rms (from_ades default) — counted as "
              f"adamcore_sigma_source.defaulted_by_adamcore, NOT as reported.", file=sys.stderr)

    models, model_strings = build_models()
    out["adamcore_models"] = model_strings
    out["adamcore_version"] = versions.get("adam_core", {}).get("version")
    out["adamcore_data_version"] = versions.get("b612_data_package", {}).get("version")

    make_prop = C.planets_only_propagator if C.is_self_perturber(obj) else C.default_propagator
    if C.is_self_perturber(obj):
        C.verify_planets_only_active(make_prop())
    t0 = time.perf_counter()
    fo, members = run_od(
        odo,
        NativeOrbitFitter(make_prop()),
        models=models,
        propagator=make_prop(),
    )
    out["adamcore_time_ms"] = (time.perf_counter() - t0) * 1000.0

    if len(fo) == 0:
        out["adamcore_converged"] = False
        # Recover adam_core's OWN stated reason: re-run the IOD stage (on the
        # model-applied observations run_od used) and read IodReport.not_found_reason
        # (e.g. "arc of 0.79 days, shorter than min_arc_length = 1", "no finite Gauss
        # candidate from 32 triplets", "all roots failed correction ...").
        try:
            from adam_core.orbit_determination import apply_observation_models, run_iod

            used = apply_observation_models(odo, build_models_concrete())
            _fo, _mem, report = run_iod(
                used, make_prop(), min_obs=6, min_arc_length=1.0, rchi2_threshold=200.0
            )
            reason = getattr(report, "not_found_reason", None)
            out["adamcore_unsupported"] = (
                f"no orbit: {reason}" if reason else "no orbit (run_od returned empty; no IOD reason)"
            )
        except Exception as e:  # noqa: BLE001
            out["adamcore_unsupported"] = f"no orbit (reason probe failed: {type(e).__name__}: {e})"[:300]
        return out

    out["adamcore_converged"] = bool(fo.success[0].as_py())
    out["adamcore_chi2"] = float(fo.chi2[0].as_py())
    out["adamcore_reduced_chi2"] = float(fo.reduced_chi2[0].as_py())
    out["adamcore_iterations"] = int(fo.iterations[0].as_py())
    out["adamcore_n_obs_used"] = int(fo.num_obs[0].as_py())
    out["adamcore_n_obs_rejected"] = sum(1 for x in members.outlier.to_pylist() if x)

    c = C.to_bary_equatorial(fo.coordinates)
    out["adamcore_state_bcart_eq"] = [
        float(c.x[0].as_py()), float(c.y[0].as_py()), float(c.z[0].as_py()),
        float(c.vx[0].as_py()), float(c.vy[0].as_py()), float(c.vz[0].as_py()),
    ]
    out["adamcore_epoch_mjd_tdb"] = float(c.time.mjd()[0].as_py())
    cov = c.covariance.to_matrix()[0]
    out["adamcore_cov"] = [[float(cov[i][j]) for j in range(6)] for i in range(6)]
    return out


# ── main ─────────────────────────────────────────────────────


def _is_radar(row: dict) -> bool:
    return (row.get("test_type") or "").startswith("orbit_determination_radar") or "radar" in (
        row.get("test_type") or ""
    )


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--input", required=True, type=Path, help="validation plan JSON")
    p.add_argument("--fixtures-dir", type=Path, default=C.HERE.parent.parent / "fixtures" / "psv")
    p.add_argument("--output", type=Path, default=None, help="output JSON (default results/validation_adamcore.json)")
    p.add_argument("--objects", type=str, default=None, help="comma-separated object names to restrict to")
    p.add_argument("--only", type=str, default=None,
                   help="comma-separated channels: propagation,ephemeris,orbit_determination")
    p.add_argument("--timeout-min", type=float, default=30.0, help="per-object OD timeout (minutes)")
    # internal: fit a single object in a child process (so the parent can hard-kill on timeout)
    p.add_argument("--od-one", type=str, default=None, help=argparse.SUPPRESS)
    p.add_argument("--od-one-mpc", type=str, default=None, help=argparse.SUPPRESS)
    args = p.parse_args()

    versions = C.load_versions()

    # child mode: fit one object, print the adamcore_* dict as JSON.
    if args.od_one is not None:
        res = od_one(args.od_one, args.fixtures_dir, versions, args.od_one_mpc)
        sys.stdout.write(json.dumps(res, allow_nan=False))
        return 0

    output = args.output or (C.HERE.parent.parent / "results" / f"validation_{TOOL}.json")
    channels = (
        {c.strip() for c in args.only.split(",") if c.strip()}
        if args.only
        else {"propagation", "ephemeris", "orbit_determination"}
    )
    objects = {o.strip() for o in args.objects.split(",") if o.strip()} if args.objects else None

    plan = json.loads(args.input.read_text())
    if not plan:
        print("input plan empty", file=sys.stderr)
        return 1
    if objects is not None:
        plan = [r for r in plan if r.get("object") in objects]

    timestamp = datetime.now(timezone.utc).isoformat()
    sv = C.source_version()
    rows_out: list[dict] = []

    def base(row: dict) -> dict:
        new = dict(row)
        new["channel"] = TOOL
        new["timestamp"] = timestamp
        new["source_version"] = sv
        return new

    # Independent propagator instances (verified: adam-assist @ce52ce1e holds its
    # ASSIST ephemeris per-instance, so the two force models coexist in one process).
    # Self-perturbers (Iris/Vesta/Pallas/Hygiea) run planets-only; everything else
    # runs the full planets+SB441-n16 model.
    prop_default = C.default_propagator()
    prop_po = None  # built lazily + verified on first self-perturber

    def prop_for(obj):
        nonlocal prop_po
        if C.is_self_perturber(obj):
            if prop_po is None:
                prop_po = C.planets_only_propagator()
                div = C.verify_planets_only_active(prop_po)
                print(f"planets-only verified (self-perturber probe diverges {div:.2f} km from default)",
                      file=sys.stderr)
            return prop_po
        return prop_default

    ok = resolvable_fn()

    # ── propagation (one row per (object, dt); state is arm-independent) ──
    if "propagation" in channels:
        seen = set()
        n = 0
        for r in plan:
            if r.get("test_type") != "propagation" or r.get("ic_pos_au") is None:
                continue
            key = (r.get("object"), r.get("dt_days"))
            if key in seen:
                continue
            seen.add(key)
            new = base(r)
            try:
                new.update(propagation(r, prop_for(r.get("object"))))
            except Exception as e:  # noqa: BLE001
                new["adamcore_unsupported"] = f"propagation error: {type(e).__name__}: {e}"[:300]
            rows_out.append(new)
            n += 1
        print(f"propagation: {n} rows", file=sys.stderr)

    # ── ephemeris (one row per (object, dt, observer)) ──
    if "ephemeris" in channels:
        seen = set()
        n = 0
        for r in plan:
            if r.get("test_type") != "ephemeris" or r.get("ic_pos_au") is None:
                continue
            key = (r.get("object"), r.get("dt_days"), r.get("observer"))
            if key in seen:
                continue
            seen.add(key)
            new = base(r)
            try:
                new.update(ephemeris(r, prop_for(r.get("object")), ok))
            except Exception as e:  # noqa: BLE001
                new["adamcore_unsupported"] = f"ephemeris error: {type(e).__name__}: {e}"[:300]
            rows_out.append(new)
            n += 1
        print(f"ephemeris: {n} rows", file=sys.stderr)

    # ── orbit determination (optical only; radar skipped by name) ──
    if "orbit_determination" in channels:
        od_objs = []
        seen = set()
        for r in plan:
            if r.get("test_type") != "orbit_determination" or _is_radar(r):
                continue
            o = r.get("object")
            if o in seen:
                continue
            seen.add(o)
            od_objs.append(r)
        print(f"orbit_determination: {len(od_objs)} objects", file=sys.stderr)
        timeout_s = args.timeout_min * 60.0
        for r in od_objs:
            o = r["object"]
            new = base(r)
            t0 = time.perf_counter()
            cmd = [
                sys.executable, str(Path(__file__).resolve()),
                "--input", str(args.input), "--fixtures-dir", str(args.fixtures_dir),
                "--od-one", o,
            ]
            if r.get("mpc_designation"):
                cmd += ["--od-one-mpc", str(r["mpc_designation"])]
            try:
                import subprocess

                cp = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout_s)
                wall = time.perf_counter() - t0
                if cp.returncode != 0:
                    new["adamcore_unsupported"] = f"od subprocess exit {cp.returncode}: {cp.stderr[-300:]}"
                else:
                    new.update(json.loads(cp.stdout))
                new["adamcore_wall_s"] = wall
                conv = new.get("adamcore_converged")
                print(f"  {o:24s} {wall:8.1f}s  converged={conv} "
                      f"rchi2={new.get('adamcore_reduced_chi2')}", file=sys.stderr)
            except subprocess.TimeoutExpired:
                new["adamcore_timed_out"] = True
                new["adamcore_wall_s"] = time.perf_counter() - t0
                new["adamcore_force_model"] = C.force_model_label(o)
                print(f"  {o:24s} TIMED OUT after {args.timeout_min} min", file=sys.stderr)
            rows_out.append(new)

    C.dump_rows(rows_out, output)
    print(f"Wrote {len(rows_out)} rows to {output}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
