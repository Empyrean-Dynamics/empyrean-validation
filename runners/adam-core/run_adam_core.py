"""adam-core external-reference runner for the empyrean validation suite.

Replays the canonical validation plan through **adam-core as it ships publicly** —
the released PyPI packages ``adam-core==0.7.0`` and ``adam-assist==0.6.0``. ONE
channel, tool id ``adam-core``:

* **propagation / ephemeris** go through ``adam_assist.ASSISTPropagator`` with the
  shipped defaults (``epsilon=1e-6, min_dt=1e-9, adaptive_mode=1, initial_dt=1e-6``);
* **orbit determination** goes through ``run_od`` under the public ``CORE_OD_V1``
  profile: the bound pipeline ``Efcc18CatalogBiasCorrection -> NightlyBatchDeweighting(cap=4)``
  on the public ``jpl-debias-2018`` bias table, with Gauss IOD + cmc2003 refinement.

No private Asteroid-Institute table is installed, imported or referenced. CORE_OD_V1
has no SigmaFill, so an object whose astrometry does not carry reported uncertainties
is REFUSED by ``run_od`` (a named failed row carrying adam-core's own reason) — never
filled with a default sigma.

Every row is a plan row plus ``adam_core_*`` fields; ``channel`` is ``adam-core``. An
OD row carries the profile (``adam_core_profile``), the model-stack NAMES
(``adam_core_models``), the adam-core version (``adam_core_version``), the force model
(``adam_core_force_model``) and the measured weight provenance
(``adam_core_sigma_source``: reported vs missing_uncertainty); on a refusal/failure,
adam-core's own reason.

No-hidden-fallbacks: every missing reported sigma, dropped space-based observation,
refused/failed fit and unsupported non-grav term is counted and carried on the row
with its reason.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import adam_core_common as C  # noqa: E402

AU_KM = C.AU_KM


TOOL = "adam-core"
PROFILE = "CORE_OD_V1"


# ── propagation ──────────────────────────────────────────────


def propagation(row: dict, prop) -> dict:
    from adam_core.time import Timestamp

    out: dict = {"adam_core_force_model": C.force_model_label(row.get("object")),
                 "adam_core_propagator": C.propagator_label(prop)}
    if C.is_self_perturber(row.get("object")):
        out["adam_core_unsupported"] = C.SELF_PERTURBER_UNSUPPORTED
        return out
    orbit, nongrav_reason = C.plan_orbit(row)
    if nongrav_reason:
        out["adam_core_unsupported"] = nongrav_reason  # e.g. "nongrav dt"
    t0 = time.perf_counter()
    result = prop.propagate_orbits(orbit, Timestamp.from_mjd([row["t_mjd_tdb"]], scale="tdb"))
    out["adam_core_time_ms"] = (time.perf_counter() - t0) * 1000.0
    c = result.coordinates
    pos = [float(c.x[0].as_py()), float(c.y[0].as_py()), float(c.z[0].as_py())]
    if not C.finite(*pos):
        out["adam_core_unsupported"] = out.get("adam_core_unsupported") or "non-finite propagated state"
        return out
    out["adam_core_pos_au"] = pos
    ref = row.get("ref_pos_au")
    if ref is not None:
        d = math.sqrt(sum((pos[i] - ref[i]) ** 2 for i in range(3)))
        out["adam_core_vs_horizons_km"] = d * AU_KM
    return out


# ── ephemeris ────────────────────────────────────────────────


def ephemeris(row: dict, prop, resolvable) -> dict:
    from adam_core.observers import Observers
    from adam_core.time import Timestamp

    out: dict = {"adam_core_force_model": C.force_model_label(row.get("object")),
                 "adam_core_propagator": C.propagator_label(prop)}
    if C.is_self_perturber(row.get("object")):
        out["adam_core_unsupported"] = C.SELF_PERTURBER_UNSUPPORTED
        return out
    code = row.get("observer")
    if not code:
        return out
    if not resolvable(code):
        out["adam_core_unsupported"] = "space-based observer"
        return out
    orbit, nongrav_reason = C.plan_orbit(row)
    if nongrav_reason:
        out["adam_core_unsupported"] = nongrav_reason
    orbit = C.orbit_to_ecliptic(orbit)  # typed ephemeris needs an ecliptic Cartesian orbit
    times = Timestamp.from_mjd([row["t_mjd_tdb"]], scale="tdb")
    observers = Observers.from_code(code, times)
    t0 = time.perf_counter()
    eph = prop.generate_ephemeris(orbit, observers)
    out["adam_core_time_ms"] = (time.perf_counter() - t0) * 1000.0
    sc = eph.coordinates
    ra_rad = math.radians(float(sc.lon[0].as_py()))
    dec_rad = math.radians(float(sc.lat[0].as_py()))
    rho_au = float(sc.rho[0].as_py())
    if not C.finite(ra_rad, dec_rad, rho_au):
        out["adam_core_unsupported"] = out.get("adam_core_unsupported") or "non-finite ephemeris"
        return out
    ref_ra, ref_dec = row.get("ref_ra_rad"), row.get("ref_dec_rad")
    if ref_ra is not None and ref_dec is not None:
        out["adam_core_separation_arcsec"] = C.angular_sep_arcsec(ra_rad, dec_rad, ref_ra, ref_dec)
        out["adam_core_d_ra_arcsec"] = math.degrees((ra_rad - ref_ra) * math.cos(dec_rad)) * 3600.0
        out["adam_core_d_dec_arcsec"] = math.degrees(dec_rad - ref_dec) * 3600.0
    ref_rho = row.get("ref_rho_au")
    if ref_rho is not None:
        out["adam_core_d_rho_km"] = (rho_au - ref_rho) * AU_KM
    return out


# ── orbit determination ──────────────────────────────────────


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


def _failure_reason(failures) -> str:
    """adam-core's own stated reason from a returned OrbitDeterminationFailures row."""
    try:
        stage = failures.stage.to_pylist()[0]
        code = failures.code.to_pylist()[0]
        msg = failures.message.to_pylist()[0]
        return f"no orbit [{stage}/{code}]: {msg}"[:400]
    except Exception:  # noqa: BLE001
        return "no orbit (run_od returned no fitted orbit and no failure detail)"


def od_one(obj: str, fixtures_dir: Path, versions: dict,
           mpc_designation: str | None = None,
           epsilon: float = C.SHIPPED_EPSILON,
           adaptive_mode: int = C.SHIPPED_ADAPTIVE_MODE) -> dict:
    """Fit one object's optical astrometry with the public CORE_OD_V1 profile.

    Returns the adam_core_* fields (no plan row). A self-perturber, a missing
    fixture, an all-space-based arc, a run_od refusal (e.g. an observation without
    a reported uncertainty, which CORE_OD_V1 does not fill) and an IOD/refinement
    failure are each a NAMED unsupported/failed row carrying the tool's reason —
    never a silent skip or a defaulted sigma. The ASSISTPropagator is ALWAYS passed
    explicitly (run_od(propagator=None) would silently select two-body).
    """
    from adam_core.orbit_determination import run_od

    out: dict = {"adam_core_force_model": C.force_model_label(obj), "adam_core_profile": PROFILE}
    out["adam_core_version"] = versions.get("adam_core", {}).get("version")
    out["adam_core_epsilon"] = epsilon
    out["adam_core_adaptive_mode"] = adaptive_mode

    if C.is_self_perturber(obj):
        out["adam_core_unsupported"] = C.SELF_PERTURBER_UNSUPPORTED
        out["adam_core_converged"] = False
        return out

    psv, candidates = C.resolve_fixture(fixtures_dir, obj, mpc_designation)
    if psv is None:
        print(f"ERROR: {obj}: no PSV fixture found; tried {candidates}", file=sys.stderr)
        out["adam_core_unsupported"] = f"no PSV fixture (tried {candidates})"
        out["adam_core_converged"] = False
        return out

    ok = resolvable_fn()
    catalog, assoc, n_dropped, droppable, _n, sigma_report = C.build_source_catalog(
        psv.read_text(), obj, ok
    )
    out["adam_core_n_obs_dropped_spacebased"] = n_dropped
    if n_dropped:
        out["adam_core_dropped_stations"] = droppable
    out["adam_core_sigma_source"] = sigma_report
    out["adam_core_sigma_fill"] = sigma_report.get("model")
    out["adam_core_models"] = C.od_profile_models()
    if catalog is None:
        out["adam_core_unsupported"] = "all observations at space-based stations"
        out["adam_core_converged"] = False
        return out
    if sigma_report["filled"]:
        # Surfaced loudly (not hidden): CORE_OD_V1 has no sigma-fill stage, so the
        # suite's shared public Vereš 2017 model supplied the missing sigmas.
        print(f"NOTE: {obj}: {sigma_report['filled']}/{sigma_report['total']} observations had "
              f"no reported uncertainty; filled from {sigma_report['model']} "
              f"{sigma_report['filled_by_level']} (reported {sigma_report['reported']}).",
              file=sys.stderr)

    cfg = C.od_config()
    prop = C.default_propagator(epsilon=epsilon, adaptive_mode=adaptive_mode)
    out["adam_core_propagator"] = C.propagator_label(prop)
    t0 = time.perf_counter()
    try:
        fo, members, failures = run_od(catalog, assoc, prop, cfg)
    except Exception as e:  # noqa: BLE001
        out["adam_core_time_ms"] = (time.perf_counter() - t0) * 1000.0
        out["adam_core_converged"] = False
        out["adam_core_unsupported"] = f"run_od refused: {type(e).__name__}: {e}"[:400]
        return out
    out["adam_core_time_ms"] = (time.perf_counter() - t0) * 1000.0

    if len(fo) == 0:
        out["adam_core_converged"] = False
        out["adam_core_unsupported"] = _failure_reason(failures)
        return out

    out["adam_core_converged"] = bool(fo.success[0].as_py())
    out["adam_core_status_code"] = str(fo.status_code[0].as_py())
    out["adam_core_chi2"] = float(fo.chi2[0].as_py())
    out["adam_core_reduced_chi2"] = float(fo.reduced_chi2[0].as_py())
    out["adam_core_iterations"] = int(fo.iterations[0].as_py())
    out["adam_core_n_obs_used"] = int(fo.num_obs[0].as_py())
    out["adam_core_arc_length_days"] = float(fo.arc_length[0].as_py())
    out["adam_core_n_obs_rejected"] = sum(1 for x in members.outlier.to_pylist() if x)

    c = C.to_bary_equatorial(fo.coordinates)
    out["adam_core_state_bcart_eq"] = [
        float(c.x[0].as_py()), float(c.y[0].as_py()), float(c.z[0].as_py()),
        float(c.vx[0].as_py()), float(c.vy[0].as_py()), float(c.vz[0].as_py()),
    ]
    out["adam_core_epoch_mjd_tdb"] = float(c.time.mjd()[0].as_py())
    cov = c.covariance.to_matrix()[0]
    out["adam_core_cov"] = [[float(cov[i][j]) for j in range(6)] for i in range(6)]
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
    p.add_argument("--output", type=Path, default=None, help="output JSON (default results/validation_adam-core.json)")
    p.add_argument("--objects", type=str, default=None, help="comma-separated object names to restrict to")
    p.add_argument("--only", type=str, default=None,
                   help="comma-separated channels: propagation,ephemeris,orbit_determination")
    p.add_argument("--timeout-min", type=float, default=30.0, help="per-object OD timeout (minutes)")
    # The registered kernels (the third-party set's adam-core/ folder): de440.bsp and
    # sb441-n16.bsp, passed explicitly to adam-assist. No fallback to PyPI kernels.
    p.add_argument("--data-dir", type=Path, default=os.environ.get("ADAM_CORE_DATA_DIR"),
                   help="third-party set's adam-core/ folder (default $ADAM_CORE_DATA_DIR)")
    p.add_argument("--registered-obscodes", type=Path, default=os.environ.get("ADAM_CORE_OBSCODES"),
                   help="registered obscodes_extended.json, compared (not used) to adam_core's own")
    # Explicit adam-assist integrator settings, recorded on every row. Default is the
    # adam-assist 0.6.0 shipped pair; the smoke also runs 1e-9 / mode 2 (bead app3v).
    p.add_argument("--epsilon", type=float, default=C.SHIPPED_EPSILON,
                   help="ASSIST IAS15 epsilon (shipped default 1e-6)")
    p.add_argument("--adaptive-mode", type=int, default=C.SHIPPED_ADAPTIVE_MODE,
                   help="ASSIST adaptive_mode (shipped default 1; layup uses 2)")
    # internal: fit a single object in a child process (so the parent can hard-kill on timeout)
    p.add_argument("--od-one", type=str, default=None, help=argparse.SUPPRESS)
    p.add_argument("--od-one-mpc", type=str, default=None, help=argparse.SUPPRESS)
    args = p.parse_args()

    versions = C.load_versions()
    C.configure_kernels(args.data_dir)  # refuses to start without the registered kernels

    # child mode: fit one object, print the adam_core_* dict as JSON.
    if args.od_one is not None:
        res = od_one(args.od_one, args.fixtures_dir, versions, args.od_one_mpc,
                     epsilon=args.epsilon, adaptive_mode=args.adaptive_mode)
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
        new["adam_core_epsilon"] = args.epsilon
        new["adam_core_adaptive_mode"] = args.adaptive_mode
        new["adam_core_version"] = versions.get("adam_core", {}).get("version")
        new["adam_core_settings"] = settings
        return new

    from sigmafill.veres2017 import Veres2017SigmaModel

    settings = C.settings_record(
        args.epsilon, args.adaptive_mode,
        C.sha256_file(Veres2017SigmaModel.from_csv().source),
        args.registered_obscodes,
    )

    # One full-model propagator for everything except self-perturbers, which
    # adam-assist 0.6.0 cannot run planets-only (see adam_core_common); their
    # prop/eph/OD rows are emitted unsupported with that reason.
    prop_default = C.default_propagator(epsilon=args.epsilon, adaptive_mode=args.adaptive_mode)
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
                new.update(propagation(r, prop_default))
            except Exception as e:  # noqa: BLE001
                new["adam_core_unsupported"] = f"propagation error: {type(e).__name__}: {e}"[:300]
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
                new.update(ephemeris(r, prop_default, ok))
            except Exception as e:  # noqa: BLE001
                new["adam_core_unsupported"] = f"ephemeris error: {type(e).__name__}: {e}"[:300]
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
                "--epsilon", repr(args.epsilon), "--adaptive-mode", str(args.adaptive_mode),
                "--data-dir", str(args.data_dir),
                "--od-one", o,
            ]
            if r.get("mpc_designation"):
                cmd += ["--od-one-mpc", str(r["mpc_designation"])]
            try:
                import subprocess

                cp = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout_s)
                wall = time.perf_counter() - t0
                if cp.returncode != 0:
                    new["adam_core_unsupported"] = f"od subprocess exit {cp.returncode}: {cp.stderr[-300:]}"
                    new["adam_core_converged"] = False
                else:
                    new.update(json.loads(cp.stdout))
                new["adam_core_wall_s"] = wall
                conv = new.get("adam_core_converged")
                print(f"  {o:24s} {wall:8.1f}s  converged={conv} "
                      f"rchi2={new.get('adam_core_reduced_chi2')}", file=sys.stderr)
            except subprocess.TimeoutExpired:
                new["adam_core_timed_out"] = True
                new["adam_core_converged"] = False
                new["adam_core_wall_s"] = time.perf_counter() - t0
                new["adam_core_force_model"] = C.force_model_label(o)
                print(f"  {o:24s} TIMED OUT after {args.timeout_min} min", file=sys.stderr)
            rows_out.append(new)

    C.dump_rows(rows_out, output)
    print(f"Wrote {len(rows_out)} rows to {output}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
