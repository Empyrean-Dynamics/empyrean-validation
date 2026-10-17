"""adam-core walk-forward runner for the empyrean validation suite (Part 4.2).

Walks each object's expanding per-night windows (the `empyrean-validation windows`
manifest) and, at each window, fits the observations strictly before the cut with
adam_core's `run_od` and predicts the window's held-out targets plus in-sample
controls, emitting the predicted RA/Dec and its 2x2 sky covariance from
`generate_ephemeris(..., covariance=True, covariance_method=...)`.

Record shape mirrors the layup walk (predict_schema::WalkWindowRecord /
PredictedObservation): `tool` is `adamcore`, `config_arm` is `default+sp`
(sigma-point) or `default+mc` (monte-carlo); predictions carry `cov_units="deg2"`,
`cov_basis="native_ra"` (the (lon, lat) block of the Rust spherical covariance;
lon/lat in degrees, native-RA basis — the scorer applies cos delta).

ONE channel (same as run_adamcore.py): adam-core's shipped default stack
(run_od models="default") on the Institute's private observatory-uncertainty
tables — LOCAL-ONLY.

Output: windows + predictions JSONL sidecars (external tools emit no thin rows).
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
from run_adamcore import (  # noqa: E402
    TOOL,
    build_models,
    build_models_concrete,
    build_odo,
    resolvable_fn,
)

N_SOLVE_FOR = 6  # adam_core fits the 6-element barycentric Cartesian state


# ── PSV window slicing (fit set = manifest rows before the cut) ──


def slice_psv(fixture_text: str, keep: set[tuple[str, str]]) -> tuple[str, int]:
    """Return a PSV whose data lines' (stn, obsTime) are in `keep`, and the count.

    Filtering is on the ORIGINAL fixture lines (stripped of padding) so the fit set
    is defined by membership, not by any re-parse of the astrometry.
    """
    lines = fixture_text.splitlines()
    pre: list[str] = []
    i = 0
    while i < len(lines) and lines[i].startswith(("#", "!")):
        pre.append(lines[i])
        i += 1
    header = lines[i]
    cols = [c.strip() for c in header.split("|")]
    i_stn, i_time = cols.index("stn"), cols.index("obsTime")
    kept = []
    for line in lines[i + 1 :]:
        if not line.strip():
            continue
        f = [c.strip() for c in line.split("|")]
        if (f[i_stn], f[i_time]) in keep:
            kept.append(line)
    out = "\n".join([*pre, header, *kept]) + "\n"
    return out, len(kept)


# ── fit (cold run_od, or warm refine from the previous window) ──


def fit_window(odo, warm_orbit, warm_enabled, make_prop):
    """(fitted_orbit | None, members | None, fit_ms, failure, warm_used, models).

    Cold (the brief's primary path): run_od (Gauss IOD -> differential correction).
    Warm (opt-in --warm-start): apply the default models, then `refine` the previous
    window's orbit on the used observations (skips re-running IOD). `make_prop` is
    the force-model factory for this object (planets-only for a self-perturber, else
    the full model).
    """
    from adam_core.orbit_determination import (
        NativeOrbitFitter,
        apply_observation_models,
        refine,
        run_od,
    )

    models, model_strings = build_models()
    t0 = time.perf_counter()
    warm_used = False
    try:
        if warm_enabled and warm_orbit is not None:
            used = apply_observation_models(odo, build_models_concrete())
            fo, members = refine(warm_orbit, used, make_prop(), rejection="cmc2003")
            warm_used = True
        else:
            fo, members = run_od(
                odo, NativeOrbitFitter(make_prop()),
                models=models, propagator=make_prop(),
            )
    except Exception as e:  # noqa: BLE001
        return None, None, (time.perf_counter() - t0) * 1000.0, f"fit_error: {type(e).__name__}: {e}"[:300], warm_used, model_strings
    fit_ms = (time.perf_counter() - t0) * 1000.0
    if len(fo) == 0:
        return None, None, fit_ms, "run_od returned no orbit", warm_used, model_strings
    return fo, members, fit_ms, None, warm_used, model_strings


# ── predict (RA/Dec + 2x2 lon/lat covariance) ──


def predict_batch(fitted_orbit, codes, times_mjd_utc, cov_method, num_samples, make_prop):
    """Predicted (ra_deg, dec_deg, cov2x2_deg2 | None) per requested (code, time)."""
    from adam_core.observers import Observers
    from adam_core.time import Timestamp

    orbit = C.orbit_to_ecliptic(fitted_orbit.to_orbits())
    times = Timestamp.from_mjd(list(times_mjd_utc), scale="utc")
    observers = Observers.from_codes(list(codes), times)
    eph = make_prop().generate_ephemeris(
        orbit, observers, covariance=True, covariance_method=cov_method, num_samples=num_samples
    )
    sc = eph.coordinates
    lon = sc.lon.to_pylist()
    lat = sc.lat.to_pylist()
    cov = sc.covariance.to_matrix()  # (N, 6, 6); order [rho, lon, lat, ...]
    results = []
    for k in range(len(codes)):
        m = cov[k]
        # (lon, lat) block = indices 1, 2.
        block = [[float(m[1][1]), float(m[1][2])], [float(m[2][1]), float(m[2][2])]]
        finite = all(math.isfinite(v) for row in block for v in row)
        nonzero = any(v != 0.0 for row in block for v in row)
        results.append((float(lon[k]), float(lat[k]), block if (finite and nonzero) else None))
    return results


# ── record builders (predict_schema shapes) ──


def window_record(obj, tool, arm, idx, converged, warm, fit_ms, *, fo=None, members=None,
                  predict_ms=None, failure=None):
    rec: dict = {
        "object": obj, "tool": tool, "config_arm": arm,
        "window_index": int(idx), "converged": bool(converged),
    }
    if failure is not None:
        rec["failure"] = failure
    if fo is not None and len(fo):
        rec["n_obs_used"] = int(fo.num_obs[0].as_py())
        rec["n_solve_for"] = N_SOLVE_FOR
        rec["iterations"] = int(fo.iterations[0].as_py())
        chi2 = float(fo.chi2[0].as_py())
        rchi2 = float(fo.reduced_chi2[0].as_py())
        if math.isfinite(chi2):
            rec["chi2"] = chi2
        if math.isfinite(rchi2):
            rec["reduced_chi2"] = rchi2
        epoch = float(fo.coordinates.time.mjd()[0].as_py())
        if math.isfinite(epoch):
            rec["fit_epoch_mjd_tdb"] = epoch
        if members is not None:
            # adam_core's cmc2003 has a real rejection stage, so 0 is meaningful.
            rec["n_obs_rejected"] = sum(1 for x in members.outlier.to_pylist() if x)
    rec["warm_start"] = bool(warm)
    rec["seed_source"] = "warm_previous" if warm else "own_iod"
    rec["fit_time_ms"] = float(fit_ms)
    if predict_ms is not None:
        rec["predict_time_ms"] = float(predict_ms)
    return rec


def prediction_record(obj, tool, arm, idx, obs, obs_idx, in_sample, ra_deg, dec_deg, cov2x2):
    if not (math.isfinite(ra_deg) and math.isfinite(dec_deg)):
        return None
    rec: dict = {
        "object": obj, "tool": tool, "config_arm": arm,
        "window_index": int(idx), "obs_idx": int(obs_idx),
        "key_hash": int(obs["key_hash"]), "in_sample": bool(in_sample),
        "ra_deg": ra_deg, "dec_deg": dec_deg, "epoch_scale_used": "utc",
    }
    if cov2x2 is not None:
        rec["uncertainty_form"] = "radec_2x2"
        rec["cov_radec"] = cov2x2
        rec["cov_units"] = "deg2"
        rec["cov_basis"] = "native_ra"
    else:
        rec["uncertainty_form"] = "none"
    return rec


# ── walk one object ──


def walk_object(obj, fixtures_dir, profile, tool, arm, cov_method, num_samples,
                ok, warm_enabled, max_windows=None):
    name = obj["object"]
    stats = {"windows": 0, "converged": 0, "failed": 0, "predictions": 0, "with_cov": 0, "error": None}
    windows = [w for w in obj["windows"] if profile in w["profiles"]]
    if max_windows is not None:
        windows = windows[:max_windows]
    if not windows:
        stats["error"] = f"no windows in profile {profile!r}"
        return [], [], stats

    token = name.replace("/", "_")
    fixture = fixtures_dir / f"{token}.psv"
    if not fixture.exists():
        fixture = fixtures_dir / f"{name}.psv"
    if not fixture.exists():
        stats["error"] = "no PSV fixture"
        return [], [], stats
    fixture_text = fixture.read_text()
    observations = obj["observations"]

    # Force model for this object: planets-only for a self-perturber (ASSIST
    # includes it in SB441-n16), else the full model. Verified once if planets-only.
    make_prop = C.planets_only_propagator if C.is_self_perturber(name) else C.default_propagator
    if C.is_self_perturber(name):
        C.verify_planets_only_active(make_prop())

    win_records, predictions = [], []
    warm_orbit = None

    for w in windows:
        idx = int(w["index"])
        stats["windows"] += 1
        warm = warm_enabled and warm_orbit is not None
        cut = _iso_to_mjd(w["cut_utc"])
        keep = {
            (o["stn"], o["obs_time"])
            for o in observations
            if o.get("excluded") is None and o["mjd_utc"] < cut
        }
        window_psv, n_kept = slice_psv(fixture_text, keep)
        odo, n_dropped, _droppable, _n, _rms = build_odo(window_psv, ok)
        if odo is None or len(odo) < 6:
            win_records.append(window_record(name, tool, arm, idx, False, False, 0.0,
                                              failure=f"insufficient_obs: {0 if odo is None else len(odo)} usable"))
            stats["failed"] += 1
            warm_orbit = None
            continue

        fo, members, fit_ms, failure, warm_used, _models = fit_window(odo, warm_orbit, warm, make_prop)
        if fo is None:
            win_records.append(window_record(name, tool, arm, idx, False, warm_used, fit_ms, failure=failure))
            stats["failed"] += 1
            warm_orbit = None
            continue
        converged = bool(fo.success[0].as_py())
        if not converged:
            win_records.append(window_record(name, tool, arm, idx, False, warm_used, fit_ms,
                                              fo=fo, members=members, failure="not_converged"))
            stats["failed"] += 1
            warm_orbit = None
            continue

        # predict held-out targets + in-sample controls (resolvable observers only)
        entries = [(int(t["obs_idx"]), False) for t in w["targets"]]
        entries += [(int(i), True) for i in w["in_sample"]]
        entries = [(oi, ins) for oi, ins in entries if ok(observations[oi]["stn"])]
        predict_ms = None
        if entries:
            codes = [observations[oi]["stn"] for oi, _ in entries]
            times = [observations[oi]["mjd_utc"] for oi, _ in entries]
            t0 = time.perf_counter()
            try:
                preds = predict_batch(fo, codes, times, cov_method, num_samples, make_prop)
            except Exception as e:  # noqa: BLE001
                predict_ms = (time.perf_counter() - t0) * 1000.0
                win_records.append(window_record(name, tool, arm, idx, False, warm_used, fit_ms,
                                                  fo=fo, members=members, predict_ms=predict_ms,
                                                  failure=f"predict_error: {type(e).__name__}: {e}"[:200]))
                stats["failed"] += 1
                warm_orbit = None
                continue
            predict_ms = (time.perf_counter() - t0) * 1000.0
            for (oi, ins), (ra, dec, cov2x2) in zip(entries, preds):
                rec = prediction_record(name, tool, arm, idx, observations[oi], oi, ins, ra, dec, cov2x2)
                if rec is None:
                    continue
                predictions.append(rec)
                stats["predictions"] += 1
                if rec["uncertainty_form"] == "radec_2x2":
                    stats["with_cov"] += 1

        win_records.append(window_record(name, tool, arm, idx, True, warm_used, fit_ms,
                                          fo=fo, members=members, predict_ms=predict_ms))
        stats["converged"] += 1
        warm_orbit = fo  # seed the next window

    return win_records, predictions, stats


def _iso_to_mjd(s: str) -> float:
    from datetime import datetime as dt

    d = dt.fromisoformat(s.replace("Z", "+00:00"))
    # days since MJD epoch 1858-11-17
    epoch = dt(1858, 11, 17, tzinfo=d.tzinfo)
    return (d - epoch).total_seconds() / 86400.0


def write_jsonl(path: Path, records: list[dict]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    lines = []
    for rec in records:
        lines.append(json.dumps(rec, allow_nan=False))
    path.write_text("".join(x + "\n" for x in lines))


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--manifest", type=Path, default=C.HERE.parent.parent / "fixtures" / "windows.json")
    p.add_argument("--profile", default="ci", choices=["full", "ci", "ladder"])
    p.add_argument("--fixtures-dir", type=Path, default=C.HERE.parent.parent / "fixtures" / "psv")
    p.add_argument("--cov-method", choices=["sigma-point", "monte-carlo"], default="sigma-point")
    p.add_argument("--num-samples", type=int, default=1000, help="monte-carlo sample count")
    p.add_argument("--only", type=str, default=None, help="comma-separated object names")
    p.add_argument("--warm-start", action="store_true",
                   help="warm-start each refit from the previous window's orbit via refine "
                        "(records warm_start=true). Default: cold run_od every window (the brief's primary path).")
    p.add_argument("--max-windows", type=int, default=None,
                   help="cap windows walked per object (for STOP-5 / timing estimates).")
    p.add_argument("--out-windows", type=Path, default=None)
    p.add_argument("--out-predictions", type=Path, default=None)
    args = p.parse_args()

    tool = TOOL
    arm = "default+sp" if args.cov_method == "sigma-point" else "default+mc"
    out_win = args.out_windows or (C.HERE.parent.parent / "results" / f"validation_{tool}_walk_windows.jsonl")
    out_pred = args.out_predictions or (C.HERE.parent.parent / "results" / f"validation_{tool}_walk_predictions.jsonl")

    manifest = json.loads(args.manifest.read_text())
    only = {s.strip() for s in args.only.split(",") if s.strip()} if args.only else None
    selected = []
    for obj in manifest.get("objects", []):
        if not obj.get("eligible"):
            continue
        if only is not None and not (obj["object"] in only or obj["object"].replace("/", "_") in only
                                     or obj.get("mpc_designation") in only):
            continue
        selected.append(obj)
    if not selected:
        print("ERROR: no eligible objects selected", file=sys.stderr)
        return 1

    ok = resolvable_fn()
    warm_enabled = args.warm_start
    print(f"adam-core walk ({len(selected)} objects, profile {args.profile!r}, tool {tool}, "
          f"arm {arm}, warm_start {warm_enabled}, max_windows {args.max_windows})", file=sys.stderr)

    all_win, all_pred = [], []
    totals = {"windows": 0, "converged": 0, "failed": 0, "predictions": 0, "with_cov": 0}
    for obj in selected:
        wins, preds, stats = walk_object(obj, args.fixtures_dir, args.profile, tool, arm,
                                          args.cov_method, args.num_samples, ok,
                                          warm_enabled, args.max_windows)
        all_win.extend(wins)
        all_pred.extend(preds)
        for k in totals:
            totals[k] += stats[k]
        if stats["error"]:
            print(f"{obj['object']}: SKIPPED — {stats['error']}", file=sys.stderr)
        else:
            print(f"{obj['object']}: {stats['converged']}/{stats['windows']} converged, "
                  f"{stats['predictions']} predictions ({stats['with_cov']} with cov)", file=sys.stderr)

    write_jsonl(out_win, all_win)
    write_jsonl(out_pred, all_pred)
    print(f"\n──── adam-core walk summary ({args.profile}, {tool}, {arm}) ────\n"
          f"  windows {totals['windows']} (converged {totals['converged']}, failed {totals['failed']})\n"
          f"  predictions {totals['predictions']} (with cov {totals['with_cov']})\n"
          f"  windows:     {out_win}\n  predictions: {out_pred}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
