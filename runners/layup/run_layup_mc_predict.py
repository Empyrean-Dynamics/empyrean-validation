#!/usr/bin/env python3
"""Monte-Carlo sky-covariance channel for layup — the ``default+mc`` series.

layup publishes a sky covariance for every prediction by contracting its own
state covariance through its own observable partials (``compute_single_predict``
forms \\(B \\Sigma B^\\top\\) on the unit A/D tangent vectors). That is a
*linearized* transport. This runner answers the question that linearization
always raises — **is the linearization honest?** — by transporting the same
fitted 6×6 through the same propagator by brute force: draw \\(N\\) state
variants from the fitted covariance, predict each one, and take the second
moment of the resulting sky positions **about the nominal prediction**.

The two numbers then sit side by side for every prediction, and their ratio is
the finding: near 1 means layup's internal covariance machinery agrees with
sampling through its own dynamics; a systematic departure is a real result about
the tool, measured with the tool's own propagator and the tool's own fit.

Emissions (both ``#[serde(deny_unknown_fields)]``; see ``src/predict_schema.rs``)::

    {stem}_windows.jsonl      WalkWindowRecord, config_arm "default+mc"
    {stem}_predictions.jsonl  PredictedObservation, config_arm "default+mc"
    {stem}_mc_crossval.json   the MC-vs-native comparison (this runner's own
                              product; not a schema type, not scored)

The ``default+mc`` arm is a *series*, exactly as the reference channel treats its
own non-linear uncertainty modes (``runners/rust/src/walk.rs``): the window
records are duplicated under the suffixed arm so per-series attempted/failed
counts and delivered fractions stay honest, and the predictions carry the same
nominal position as the ``default`` arm with only the 2×2 replaced.

Why this runner refits
----------------------
It cannot do otherwise today. ``WalkWindowRecord`` carries fit *statistics*
(χ², ndof, epoch, timings) and never the fitted state or its 6×6 — by design,
since the sidecar is a record of what happened, not a warm-start format. Sampling
needs both. So every window whose covariance is to be sampled is **refit**, by
calling ``run_layup_walk``'s own ``_fit_window`` on the same fit set, which
reproduces the reference run's fit bit-for-bit (the LM is deterministic given the
same observations and the same guess) — and this runner *verifies* that rather
than assuming it, comparing χ², epoch and n_obs against the reference record and
failing the window loudly on any disagreement.

That refit is pure overhead: on the ci profile it is ~3.1 s × the converged
window count, spent recomputing something the walk already computed and threw
away. **The fix is a joint mode inside the walk runner** — emit the MC series in
the same pass that produces the fit, while the fitted row is still in hand. This
runner exists as a standalone script because the walk runner could not be touched
while a full-catalog run was live; the joint mode is the shape this belongs in
once that lands, and the sampling/emission code here transfers to it unchanged.

Given ``--windows-in``, windows the reference run recorded as *failed* are not
refit at all: a failed window converged to nothing, delivered nothing, and (per
§2.5) left the next window cold, so the reference verdict is sufficient and the
series inherits the record verbatim. That is what makes the pass affordable —
757 fits instead of 2795 on the ci profile. Without ``--windows-in`` the runner
walks standalone, fitting every window exactly as the walk does.

Sampling convention (mirrors ``runners/rust/src/walk.rs``, the empyrean reference)
---------------------------------------------------------------------------------
* **Seed** — FNV-1a 64 over the object name, XOR the window index. Deterministic
  and distinct per (object, window); reruns reproduce bit-for-bit.
* **Normals** — the same LCG (Knuth MMIX constants) + Box–Muller pair the Rust
  channel uses, so the two channels' sample streams are the same construction.
* **Variants** — \\(x_k = \\hat{x} + L z_k\\) with \\(L\\) the lower-triangular
  Cholesky of the fitted 6×6 in the fitted state's own basis (BCART_EQ, which is
  Cartesian — no angle element can wrap). A covariance that is not positive
  definite yields **no** Cholesky and the window's rows are emitted with
  ``uncertainty_form: "none"`` — delivered and countable, never repaired.
* **Second moment about the NOMINAL** — not about the sample mean. The published
  point is the nominal prediction, so the honest error covariance of that point
  is \\(\\frac{1}{N}\\sum (\\Delta_k)(\\Delta_k)^\\top\\) with \\(\\Delta_k\\)
  the tangent-plane offset of variant \\(k\\) from the nominal. Using the sample
  mean would subtract off exactly the non-linear bias this channel exists to
  expose.
* **Tangent plane** — gnomonic (east, north) about the nominal, the same
  projection the reference channel and the scoring kernel use, in radians so the
  emitted 2×2 lands in layup's own ``rad2`` / ``great_circle`` convention. A
  variant behind the tangent plane (a cloud that wraps the sky) has no honest
  2×2: that prediction says ``none``.

The variants carry **no covariance columns at all** — not a zeroed block, not an
epsilon. ``parse_fit_result`` substitutes 0.0 for absent covariance columns, so
"the variant carries the dispersion, never a covariance of its own" is true by
construction rather than by trust, the same argument the walk runner's guess file
makes. (Zeroing works too: layup's ellipse conversion is well-behaved on a zero
matrix. Omission is simply the stronger statement — and it is what the nominal
row's *presence* of a covariance block then distinguishes.)

Cross-validation (the point of the exercise)
--------------------------------------------
The nominal row rides in the **same** ``predict`` call as its variants, as row 0
of the batch. So layup's native 2×2 and the MC 2×2 for a prediction come from one
call, one kernel furnish, one set of observer positions — an apples-to-apples
comparison that cannot be skewed by a stale sidecar or a re-furnished ephemeris.
Per prediction the runner records the variance ratios MC/native on both diagonal
elements, both correlation coefficients, and the ellipse-area ratio
\\(\\sqrt{\\det \\Sigma_{MC} / \\det \\Sigma_{native}}\\); the summary reports
median/p10/p90 of each.

``--predictions-in`` adds a second, independent check: the same nominal rows
against the reference run's emitted predictions. That is a *determinism* check on
the refit (did this pass reproduce the reference fit?), not the cross-validation,
and it is reported separately.

Usage (must run under the layup venv's interpreter, see ``layup/setup.sh``):

    runners/layup/.venv/bin/python runners/layup/run_layup_mc_predict.py \\
        --profile ci --only Lutetia --mc-samples 200 \\
        --windows-in results/validation_layup_walk_ci_windows.jsonl \\
        --predictions-in results/validation_layup_walk_ci_predictions.jsonl \\
        --output results/validation_layup_mc_ci.json
"""

from __future__ import annotations

import argparse
import json
import math
import sys
import tempfile
import time
from argparse import Namespace
from pathlib import Path

# The walk runner is this runner's library: fit invocation, PSV windowing, the
# schema-faithful record builders and the JSONL writer all live there and are
# reused verbatim so the two channels cannot drift. Nothing here modifies it.
sys.path.insert(0, str(Path(__file__).resolve().parent))
import run_layup_walk as walk

try:
    import numpy as np

    _HAVE_NUMPY = True
    _NUMPY_ERROR = ""
except Exception as e:  # noqa: BLE001
    _HAVE_NUMPY = False
    _NUMPY_ERROR = str(e)


# The series name. Mirrors the reference channel's `<arm>+<suffix>` convention
# for a non-linear uncertainty mode riding an existing fit arm.
MC_ARM = f"{walk.CONFIG_ARM}+mc"
# Default sample count. The reference channel's own default is 1000; 200 is the
# default here because every sample is a full ASSIST integration through layup's
# Python/C++ boundary rather than a batched ephemeris call.
DEFAULT_MC_SAMPLES = 200
RAD2ARCSEC = 3600.0 * 180.0 / math.pi
# Relative tolerance for "the refit reproduced the reference fit". The LM is
# deterministic given the same observations and guess, so this is a tripwire for
# a mismatched reference file or a changed fit configuration, not a convergence
# tolerance.
REFIT_RTOL = 1e-9


# ── the empyrean sampling convention (runners/rust/src/walk.rs) ──


def _fnv1a64(data: bytes) -> int:
    h = 0xCBF29CE484222325
    for b in data:
        h ^= b
        h = (h * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return h


def _window_seed(obj: str, window_index: int) -> int:
    """FNV-1a over the object name, XOR the window index — per (object, window)."""
    return _fnv1a64(obj.encode("utf-8")) ^ (int(window_index) & 0xFFFFFFFFFFFFFFFF)


def _standard_normals(seed: int, n: int):
    """``n`` standard normals from a seeded LCG + Box–Muller.

    The generator, its constants, the ``max(1)`` on the seed, the ``max(1e-16)``
    on the first uniform and the cos-before-sin ordering are all the reference
    channel's (`state_offsets`, `UncertaintyMode::MonteCarlo`) — reproduced here
    so the two channels draw the same construction rather than merely "some
    Gaussian".
    """
    state = max(int(seed), 1)
    out = np.empty(n, dtype=np.float64)
    i = 0
    spare: float | None = None
    while i < n:
        if spare is not None:
            out[i] = spare
            spare = None
            i += 1
            continue
        state = (state * 6364136223846793005 + 1442695040888963407) & 0xFFFFFFFFFFFFFFFF
        u1 = max((state >> 11) / float(1 << 53), 1e-16)
        state = (state * 6364136223846793005 + 1442695040888963407) & 0xFFFFFFFFFFFFFFFF
        u2 = (state >> 11) / float(1 << 53)
        r = math.sqrt(-2.0 * math.log(u1))
        ang = 2.0 * math.pi * u2
        out[i] = r * math.cos(ang)
        i += 1
        spare = r * math.sin(ang)
    return out


def _chol6(m):
    """Lower-triangular Cholesky of a 6×6, or ``None`` when not positive definite.

    Not-positive-definite is a verdict, not a defect to repair: the caller emits
    ``uncertainty_form: none`` for the window rather than conditioning the matrix
    into something that would sample a covariance layup never fit.
    """
    lo = np.zeros((6, 6), dtype=np.float64)
    for i in range(6):
        for j in range(i + 1):
            s = float(m[i][j]) - float(np.dot(lo[i, :j], lo[j, :j]))
            if i == j:
                if not math.isfinite(s) or s <= 0.0:
                    return None
                lo[i, j] = math.sqrt(s)
            else:
                lo[i, j] = s / lo[j, j]
    return lo


def _state_offsets(lo, seed: int, mc_samples: int):
    """``(mc_samples, 6)`` offsets \\(L z\\) in the fitted state's own basis."""
    z = _standard_normals(seed, mc_samples * 6).reshape(mc_samples, 6)
    return z @ lo.T


def _gnomonic_offsets_rad(nom_ra_deg: float, nom_dec_deg: float, ra_deg, dec_deg):
    """Tangent-plane (east, north) offsets of variants about the nominal, radians.

    The same gnomonic projection the reference channel uses (its version returns
    arcsec; radians here so the second moment lands directly in layup's ``rad2``).
    Returns ``None`` when any variant is on or behind the tangent plane — a
    variant cloud that wraps the sky has no honest 2×2.
    """
    a = np.radians(np.asarray(ra_deg, dtype=np.float64))
    d = np.radians(np.asarray(dec_deg, dtype=np.float64))
    cd = np.cos(d)
    u = np.stack([cd * np.cos(a), cd * np.sin(a), np.sin(d)], axis=-1)

    a0 = math.radians(nom_ra_deg)
    d0 = math.radians(nom_dec_deg)
    p = np.array(
        [math.cos(d0) * math.cos(a0), math.cos(d0) * math.sin(a0), math.sin(d0)],
        dtype=np.float64,
    )
    dot = u @ p
    if not np.all(np.isfinite(dot)) or np.any(dot <= 0.0):
        return None
    east = np.array([-math.sin(a0), math.cos(a0), 0.0], dtype=np.float64)
    north = np.array(
        [-math.sin(d0) * math.cos(a0), -math.sin(d0) * math.sin(a0), math.cos(d0)],
        dtype=np.float64,
    )
    t = u / dot[:, None]
    return t @ east, t @ north


def _second_moment_rad2(east, north):
    """Equal-weight second moment **about the origin** — i.e. about the nominal.

    Deliberately not the sample covariance: the origin of this tangent plane is
    the published prediction, and any offset of the variant cloud's centroid from
    it is non-linear bias that belongs *inside* the reported uncertainty, not
    subtracted out of it.
    """
    n = float(east.size)
    xx = float(np.dot(east, east) / n)
    xy = float(np.dot(east, north) / n)
    yy = float(np.dot(north, north) / n)
    if not (math.isfinite(xx) and math.isfinite(xy) and math.isfinite(yy)):
        return None
    return xx, xy, yy


# ── batched prediction ──────────────────────────────────────


def _variant_array(row: dict, offsets):
    """``(1 + N)``-row structured array: the nominal, then the variants.

    Row 0 is the fitted row verbatim — covariance block included, which is what
    makes layup emit its native 2×2 for the nominal in this same call. Rows 1..N
    are the same dtype with the state displaced and **every covariance column
    dropped to zero**; ``parse_fit_result`` reads that as no covariance, so a
    variant transports a point, not an ellipse.

    Built by repeating the walk runner's own ``_orbit_array`` so the nominal row
    is byte-identical to the one the ``default`` arm predicts from.
    """
    base = walk._orbit_array(row)
    n = 0 if offsets is None else len(offsets)
    arr = np.repeat(base, 1 + n)
    if n == 0:
        return arr
    for k, col in enumerate(walk._STATE_COLS):
        arr[col][1:] = float(row[col]) + offsets[:, k]
    for i in range(6):
        for j in range(6):
            col = f"cov_{i}_{j}"
            if col in arr.dtype.names:
                arr[col][1:] = 0.0
    return arr


def _predict_variants(data, stations: list[str], times_jd_tdb: list[float], opts):
    """One ``layup.predict.predict`` call over the whole (nominal + variants) batch.

    layup's ``_predict`` walks the orbit rows in order and appends one record per
    epoch for each, so results are variant-major: block ``k`` is rows
    ``k*T .. (k+1)*T``, block 0 being the nominal.

    Same ``kclear``-in-``finally`` hygiene as the walk runner: ``predict``
    furnishes the full kernel set on every call but only its on-sky path ever
    clears the pool, so a long-lived process accumulates duplicate loads until
    SpiceNOMOREROOM. Clearing is safe precisely because every call re-furnishes.
    """
    import spiceypy as spice
    from layup.predict import predict

    try:
        return predict(
            data,
            obscode=stations,
            times=times_jd_tdb,
            primary_id_column_name=walk._PRIMARY_ID,
            num_workers=opts.predict_workers,
            cache_dir=opts.ar_data_path,
            args=Namespace(onsky_data=False, primary_id_column_name=walk._PRIMARY_ID),
            configs=None,
        )
    finally:
        spice.kclear()


# ── records ─────────────────────────────────────────────────


def _mc_window_record(*args, **kwargs) -> dict:
    """A ``WalkWindowRecord`` for this series — the walk runner's builder, resuffixed."""
    rec = walk._window_record(*args, **kwargs)
    rec["config_arm"] = MC_ARM
    return rec


def _mc_prediction_record(native: dict, moment) -> dict:
    """The MC row for a prediction: the nominal row with only the 2×2 replaced.

    Position, identity, in-sample flag and time scale are the ``default`` arm's
    values unchanged — the two series differ in exactly one thing, which is what
    makes their d² comparable.
    """
    rec = dict(native)
    rec["config_arm"] = MC_ARM
    if moment is None:
        rec["uncertainty_form"] = "none"
        for k in ("cov_radec", "cov_units", "cov_basis"):
            rec.pop(k, None)
        return rec
    xx, xy, yy = moment
    rec["uncertainty_form"] = "radec_2x2"
    rec["cov_radec"] = [[xx, xy], [xy, yy]]
    # Same convention as layup's native rows: the tangent-plane basis is the unit
    # A/D pair, so the RA component is already cos δ-scaled.
    rec["cov_units"] = "rad2"
    rec["cov_basis"] = "great_circle"
    return rec


def _refit_mismatch(ref: dict, row: dict) -> str | None:
    """Describe the first disagreement between a refit and its reference record.

    The LM is deterministic given the same fit set and the same guess, so any
    disagreement means the reference file does not describe this configuration —
    a mismatched profile, a different weighting flag, a stale sidecar. That must
    surface as a failed window, never be absorbed.
    """
    for field, key in (
        ("chi2", "csq"),
        ("fit_epoch_mjd_tdb", "epochMJD_TDB"),
    ):
        rv = ref.get(field)
        mv = walk._fnum(row, key)
        if rv is None or mv is None:
            continue
        denom = max(abs(float(rv)), 1.0)
        if abs(float(rv) - mv) / denom > REFIT_RTOL:
            return f"{field} ref={rv!r} refit={mv!r}"
    rn = ref.get("n_obs_used")
    mn = walk._inum(row, "nobs_fit")
    if rn is not None and mn is not None and int(rn) != int(mn):
        return f"n_obs_used ref={rn} refit={mn}"
    return None


# ── the walk ────────────────────────────────────────────────


def _walk_object(obj: dict, fixtures_dir: Path, ref_windows: dict | None, opts) -> tuple:
    """Walk one object's windows, emitting the MC series. Returns (windows, preds, stats)."""
    name = obj["object"]
    stats = {
        "windows": 0,
        "sampled": 0,
        "inherited_failed": 0,
        "failed": 0,
        "predictions": 0,
        "with_cov": 0,
        "variants": 0,
        "predict_ms": 0.0,
        "fit_ms": 0.0,
        "error": None,
    }
    xval: list[tuple] = []
    windows = [w for w in obj["windows"] if opts.profile in w["profiles"]]
    if opts.max_windows is not None:
        windows = windows[: opts.max_windows]
    if not windows:
        stats["error"] = f"no windows in profile {opts.profile!r}"
        return [], [], stats, xval

    fixture_path = walk._resolve_fixture(fixtures_dir, name, obj["mpc_designation"])
    if fixture_path is None:
        stats["error"] = "no PSV fixture found"
        return [], [], stats, xval
    try:
        fx = walk.Fixture(fixture_path)
    except Exception as e:  # noqa: BLE001
        stats["error"] = f"fixture parse failed: {e}"
        return [], [], stats, xval

    observations = obj["observations"]
    obj_token = name.replace("/", "_")
    win_records: list[dict] = []
    predictions: list[dict] = []
    guess: dict | None = None

    def fail(idx: int, warm: bool, failure: str, fit_ms: float, **kw) -> None:
        print(f"    {name} w{idx}: {failure}", file=sys.stderr)
        win_records.append(_mc_window_record(name, idx, False, warm, fit_ms, failure=failure, **kw))
        stats["failed"] += 1

    with tempfile.TemporaryDirectory(prefix="layup_mc_") as td:
        work = Path(td)
        for w in windows:
            idx = int(w["index"])
            stats["windows"] += 1
            warm = opts.warm_start and guess is not None

            ref = None
            if ref_windows is not None:
                ref = ref_windows.get((name, idx))
                if ref is None:
                    fail(idx, False, "no_reference_window_record", 0.0)
                    guess = None
                    continue
                if not ref["converged"]:
                    # The reference fit failed: it produced no state to sample, no
                    # prediction to attach a 2×2 to, and left the next window cold.
                    # The series inherits that record verbatim rather than
                    # re-deriving a failure this pass did not observe.
                    inherited = dict(ref)
                    inherited["config_arm"] = MC_ARM
                    win_records.append(inherited)
                    stats["inherited_failed"] += 1
                    guess = None
                    continue

            # ── Fit set (§2.4): the manifest's non-excluded rows strictly before
            # the cut, matched back onto the ORIGINAL fixture lines by
            # (stn, obsTime). Mirrors run_layup_walk's slice exactly — fit-set
            # identity is what makes "same window" mean the same thing, so the
            # refit must reconstruct the same bytes the reference fit saw.
            cut_mjd_utc = walk.iso_utc_to_mjd(w["cut_utc"])
            keep = {
                (o["stn"], o["obs_time"])
                for o in observations
                if o.get("excluded") is None and o["mjd_utc"] < cut_mjd_utc
            }
            window_psv = work / "window.psv"
            n_kept = walk._write_window_psv(fx, keep, window_psv)
            expected = int(w["n_obs_fit"])
            if n_kept != expected:
                fail(
                    idx,
                    False,
                    f"fit_set_mismatch: kept {n_kept} PSV lines, manifest n_obs_fit={expected}",
                    0.0,
                )
                guess = None
                continue

            outcome = walk._fit_window(
                window_psv,
                obj_token,
                opts.orbitfit_bin,
                work,
                guess if opts.warm_start else None,
                opts,
            )
            stats["fit_ms"] += outcome.fit_time_ms
            if outcome.row is None:
                fail(idx, warm, outcome.failure, outcome.fit_time_ms)
                guess = None
                continue
            row = outcome.row
            flag = walk._inum(row, "flag")
            csq = walk._fnum(row, "csq")
            if not (flag == 0 and csq is not None):
                failure = (
                    f"orbitfit_flag_{flag}"
                    if flag not in (0, None)
                    else ("orbitfit_nonfinite_chi2" if flag == 0 else "orbitfit_missing_flag")
                )
                fail(idx, warm, failure, outcome.fit_time_ms, row=row)
                guess = None
                continue

            if ref is not None:
                mismatch = _refit_mismatch(ref, row)
                if mismatch is not None:
                    fail(
                        idx,
                        warm,
                        f"refit_mismatch: {mismatch}",
                        outcome.fit_time_ms,
                        row=row,
                    )
                    guess = None
                    continue

            # ── Sample: the fitted 6×6 in the fitted state's own basis.
            cov = np.array(
                [[walk._fnum(row, f"cov_{i}_{j}") or 0.0 for j in range(6)] for i in range(6)],
                dtype=np.float64,
            )
            lo = _chol6(cov)
            offsets = (
                None
                if lo is None
                else _state_offsets(lo, _window_seed(name, idx), opts.mc_samples)
            )

            entries: list[tuple[int, bool]] = [(int(t["obs_idx"]), False) for t in w["targets"]]
            entries += [(int(i), True) for i in w["in_sample"]]
            if not entries:
                win_records.append(
                    _mc_window_record(name, idx, True, warm, outcome.fit_time_ms, row=row)
                )
                stats["sampled"] += 1
                guess = {k: row[k] for k in (*walk._STATE_COLS, "epochMJD_TDB")}
                continue

            stations = [observations[i]["stn"] for i, _ in entries]
            times = [walk.mjd_utc_to_jd_tdb(observations[i]["mjd_utc"]) for i, _ in entries]
            n_t = len(entries)
            n_var = 0 if offsets is None else len(offsets)

            t0 = time.perf_counter()
            try:
                preds = _predict_variants(_variant_array(row, offsets), stations, times, opts)
            except Exception as e:  # noqa: BLE001
                fail(
                    idx,
                    warm,
                    f"predict_failed: {type(e).__name__}: {e}"[:500],
                    outcome.fit_time_ms,
                    row=row,
                    predict_time_ms=(time.perf_counter() - t0) * 1000.0,
                )
                guess = None
                continue
            predict_ms = (time.perf_counter() - t0) * 1000.0
            stats["predict_ms"] += predict_ms
            stats["variants"] += n_var

            expect_rows = (1 + n_var) * n_t
            if len(preds) != expect_rows:
                fail(
                    idx,
                    warm,
                    f"predict_count_mismatch: got {len(preds)} for {1 + n_var} orbits × {n_t} epochs",
                    outcome.fit_time_ms,
                    row=row,
                    predict_time_ms=predict_ms,
                )
                guess = None
                continue
            # layup documents "results are returned in input order"; verify rather
            # than trust. A silent reordering — across epochs *or* across the
            # variant blocks — would attach every sample to the wrong target.
            epochs = np.asarray(preds["epoch_JD_TDB"], dtype=np.float64)
            skew = float(np.max(np.abs(epochs.reshape(1 + n_var, n_t) - np.array(times))))
            if skew > 1e-6:
                fail(
                    idx,
                    warm,
                    f"predict_epoch_reordered: max |Δepoch| = {skew:.3e} d",
                    outcome.fit_time_ms,
                    row=row,
                    predict_time_ms=predict_ms,
                )
                guess = None
                continue

            var_ra = np.asarray(preds["ra_deg"], dtype=np.float64).reshape(1 + n_var, n_t)
            var_dec = np.asarray(preds["dec_deg"], dtype=np.float64).reshape(1 + n_var, n_t)

            for m, (obs_idx, in_sample) in enumerate(entries):
                native = walk._prediction_record(
                    name, idx, observations[obs_idx], obs_idx, in_sample, preds[m]
                )
                if native is None:
                    print(
                        f"    {name} w{idx}: non-finite predicted position for obs_idx "
                        f"{obs_idx}; prediction dropped",
                        file=sys.stderr,
                    )
                    continue
                moment = None
                if n_var:
                    off = _gnomonic_offsets_rad(
                        native["ra_deg"], native["dec_deg"], var_ra[1:, m], var_dec[1:, m]
                    )
                    if off is not None:
                        moment = _second_moment_rad2(*off)
                rec = _mc_prediction_record(native, moment)
                predictions.append(rec)
                stats["predictions"] += 1
                if rec["uncertainty_form"] == "radec_2x2":
                    stats["with_cov"] += 1
                if moment is not None and native["uncertainty_form"] == "radec_2x2":
                    xval.append(
                        (name, idx, obs_idx, in_sample, native["cov_radec"], rec["cov_radec"])
                    )

            win_records.append(
                _mc_window_record(
                    name,
                    idx,
                    True,
                    warm,
                    outcome.fit_time_ms,
                    row=row,
                    predict_time_ms=predict_ms,
                )
            )
            stats["sampled"] += 1
            # Heartbeat: a window here costs seconds, not milliseconds, and a
            # full-catalog pass runs for hours — a silent runner at that scale is
            # indistinguishable from a hung one.
            print(
                f"    {name} w{idx}: {n_var} variants × {n_t} targets in "
                f"{predict_ms / 1000.0:.1f}s (fit {outcome.fit_time_ms / 1000.0:.1f}s)",
                file=sys.stderr,
            )
            guess = {k: row[k] for k in (*walk._STATE_COLS, "epochMJD_TDB")}

    return win_records, predictions, stats, xval


# ── cross-validation ────────────────────────────────────────


def _pct(a, q):
    return float(np.percentile(np.asarray(a, dtype=np.float64), q))


def _crossval_summary(xval: list[tuple]) -> dict:
    """MC-vs-native ratio distributions — the validation this channel exists for."""
    if not xval:
        return {"n": 0}
    nat = np.array([[r[4][0][0], r[4][0][1], r[4][1][1]] for r in xval], dtype=np.float64)
    mc = np.array([[r[5][0][0], r[5][0][1], r[5][1][1]] for r in xval], dtype=np.float64)
    ok = (nat[:, 0] > 0) & (nat[:, 2] > 0) & (mc[:, 0] > 0) & (mc[:, 2] > 0)
    nat, mc = nat[ok], mc[ok]
    if len(nat) == 0:
        return {"n": 0}

    r_xx = mc[:, 0] / nat[:, 0]
    r_yy = mc[:, 2] / nat[:, 2]
    rho_nat = nat[:, 1] / np.sqrt(nat[:, 0] * nat[:, 2])
    rho_mc = mc[:, 1] / np.sqrt(mc[:, 0] * mc[:, 2])
    det_nat = nat[:, 0] * nat[:, 2] - nat[:, 1] ** 2
    det_mc = mc[:, 0] * mc[:, 2] - mc[:, 1] ** 2
    good = (det_nat > 0) & (det_mc > 0)
    area = np.sqrt(det_mc[good] / det_nat[good])

    def dist(a) -> dict:
        return {
            "p10": _pct(a, 10),
            "median": _pct(a, 50),
            "p90": _pct(a, 90),
            "min": float(np.min(a)),
            "max": float(np.max(a)),
        }

    return {
        "n": len(nat),
        "variance_ratio_xx": dist(r_xx),
        "variance_ratio_yy": dist(r_yy),
        "area_ratio": dist(area),
        "correlation_native": dist(rho_nat),
        "correlation_mc": dist(rho_mc),
        "correlation_delta": dist(rho_mc - rho_nat),
    }


def _determinism_summary(preds: list[dict], ref_preds: dict) -> dict:
    """Nominal rows of this pass against the reference run's — did the refit repeat it?

    Compares position only: the emitted MC rows carry the nominal position
    verbatim, so a match here is a direct statement that this pass reproduced the
    reference fit. A miss means the reference sidecar describes a different run,
    and every MC-vs-``default`` comparison downstream would be joining across two
    different fits.
    """
    if not ref_preds:
        return {"n": 0}
    seps = []
    missing = 0
    for p in preds:
        r = ref_preds.get((p["object"], p["window_index"], p["obs_idx"]))
        if r is None:
            missing += 1
            continue
        d0 = math.radians(r["dec_deg"])
        dra = (p["ra_deg"] - r["ra_deg"]) * math.cos(d0)
        ddec = p["dec_deg"] - r["dec_deg"]
        seps.append(math.hypot(dra, ddec) * 3600.0)
    if not seps:
        return {"n": 0, "unmatched": missing}
    return {
        "n": len(seps),
        "unmatched": missing,
        "max_sep_arcsec": float(np.max(seps)),
        "median_sep_arcsec": float(np.median(seps)),
    }


# ── main ────────────────────────────────────────────────────


def _load_jsonl_index(path: Path, key) -> dict:
    out = {}
    with open(path) as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            rec = json.loads(line)
            if rec.get("config_arm") != walk.CONFIG_ARM or rec.get("tool") != walk.TOOL:
                continue
            out[key(rec)] = rec
    return out


def main() -> int:
    here = Path(__file__).resolve().parent
    repo = here.parent.parent

    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    p.add_argument("--manifest", type=Path, default=repo / "fixtures" / "windows.json")
    p.add_argument("--fixtures-dir", type=Path, default=repo / "fixtures" / "psv")
    p.add_argument("--profile", type=str, default="ladder", choices=["full", "ci", "ladder"])
    p.add_argument("--only", type=str, default=None)
    p.add_argument(
        "--output",
        type=Path,
        default=repo / "results" / "validation_layup_mc.json",
        help="Thin-row JSON path (empty array, pipeline symmetry). The real output is the "
        "sidecars beside it: {stem}_windows.jsonl, {stem}_predictions.jsonl, "
        "{stem}_mc_crossval.json.",
    )
    p.add_argument(
        "--windows-in",
        type=Path,
        default=None,
        help="Window sidecar of an existing layup walk. Windows it records as failed are "
        "inherited rather than refit, and every refit is verified against it. Without it "
        "the runner walks standalone and fits every window.",
    )
    p.add_argument(
        "--predictions-in",
        type=Path,
        default=None,
        help="Prediction sidecar of that same run — enables the determinism check that this "
        "pass's refits reproduced the reference fits.",
    )
    p.add_argument(
        "--mc-samples",
        type=int,
        default=DEFAULT_MC_SAMPLES,
        help=f"State variants per window (default {DEFAULT_MC_SAMPLES}). Every sample is a full "
        "propagation through layup, so this is the runner's whole cost.",
    )
    p.add_argument(
        "--predict-workers",
        type=int,
        default=1,
        help="Workers for layup's prediction process pool. The variants of one window split "
        "across them; >1 trades cores for wall-clock.",
    )
    p.add_argument("--max-windows", type=int, default=None, help="Cap windows per object (smoke).")
    p.add_argument("--ar-data-path", type=str, default=None)
    p.add_argument("--timeout", type=float, default=900.0)
    p.add_argument("--weight-data", action=argparse.BooleanOptionalAction, default=True)
    p.add_argument("--debias", action="store_true")
    p.add_argument("--no-warm-start", action="store_true")
    args = p.parse_args()

    if not (_HAVE_NUMPY and walk._HAVE_DEPS):
        print(
            f"ERROR: numpy/pandas import failed ({_NUMPY_ERROR or walk._DEPS_ERROR}).\n"
            "       Run under runners/layup/.venv/bin/python (`make setup-layup`).",
            file=sys.stderr,
        )
        return 1
    if args.mc_samples < 2:
        print("ERROR: --mc-samples must be at least 2 to form a second moment.", file=sys.stderr)
        return 1

    orbitfit_bin = walk._orbitfit_binary()
    if orbitfit_bin is None:
        print(
            "ERROR: `layup-orbitfit` was not found next to this interpreter and not on $PATH.\n"
            "       Run under runners/layup/.venv/bin/python (`make setup-layup`).",
            file=sys.stderr,
        )
        return 1

    manifest_path = args.manifest.expanduser().resolve()
    if not manifest_path.exists():
        print(f"ERROR: manifest not found: {manifest_path}", file=sys.stderr)
        return 1
    manifest = json.loads(manifest_path.read_text())
    fixtures_dir = args.fixtures_dir.expanduser().resolve()
    if not fixtures_dir.is_dir():
        print(f"ERROR: fixtures directory not found: {fixtures_dir}", file=sys.stderr)
        return 1

    ref_windows = None
    if args.windows_in is not None:
        wp = args.windows_in.expanduser().resolve()
        if not wp.exists():
            print(f"ERROR: --windows-in not found: {wp}", file=sys.stderr)
            return 1
        ref_windows = _load_jsonl_index(wp, lambda r: (r["object"], int(r["window_index"])))
        if not ref_windows:
            print(
                f"ERROR: {wp} carries no layup/{walk.CONFIG_ARM} window records.\n"
                "       Inheriting failures from an empty reference would silently emit nothing.",
                file=sys.stderr,
            )
            return 1
    ref_preds: dict = {}
    if args.predictions_in is not None:
        pp = args.predictions_in.expanduser().resolve()
        if not pp.exists():
            print(f"ERROR: --predictions-in not found: {pp}", file=sys.stderr)
            return 1
        ref_preds = _load_jsonl_index(
            pp, lambda r: (r["object"], int(r["window_index"]), int(r["obs_idx"]))
        )

    only = {s.strip() for s in args.only.split(",") if s.strip()} if args.only else None
    selected = []
    for obj in manifest.get("objects", []):
        if not obj.get("eligible"):
            continue
        if only is not None and not (
            obj["object"] in only
            or obj["object"].replace("/", "_") in only
            or obj.get("mpc_designation") in only
        ):
            continue
        selected.append(obj)
    if not selected:
        print(
            f"ERROR: no eligible objects selected from {manifest_path}"
            + (f" with --only {args.only!r}" if only else "")
            + ".",
            file=sys.stderr,
        )
        return 1

    opts = Namespace(
        weight_data=args.weight_data,
        debias=args.debias,
        ar_data_path=args.ar_data_path,
        timeout=args.timeout,
        warm_start=not args.no_warm_start,
        profile=args.profile,
        mc_samples=args.mc_samples,
        predict_workers=args.predict_workers,
        max_windows=args.max_windows,
        orbitfit_bin=orbitfit_bin,
    )

    print(
        f"layup MC sky covariance ({len(selected)} objects, profile {args.profile!r}, "
        f"N={args.mc_samples})\n"
        f"  layup: {orbitfit_bin} (version {walk._layup_version() or 'unknown'})\n"
        f"  reference windows: {args.windows_in or '(standalone — every window refit)'}\n"
        f"  weighting: {'veres2017' if args.weight_data else 'flat_default'}"
        f"{'+debias' if args.debias else ''}  warm_start: {not args.no_warm_start}",
        file=sys.stderr,
    )

    all_windows: list[dict] = []
    all_preds: list[dict] = []
    all_xval: list[tuple] = []
    n_delivered = 0
    totals = {
        k: 0
        for k in ("windows", "sampled", "inherited_failed", "failed", "predictions", "with_cov", "variants")
    }
    totals["predict_ms"] = 0.0
    totals["fit_ms"] = 0.0

    t_start = time.perf_counter()
    for obj in selected:
        wins, preds, stats, xval = _walk_object(obj, fixtures_dir, ref_windows, opts)
        all_windows.extend(wins)
        all_preds.extend(preds)
        all_xval.extend(xval)
        for k in totals:
            totals[k] += stats[k]
        if wins:
            n_delivered += 1
        if stats["error"]:
            print(f"{obj['object']}: SKIPPED — {stats['error']}", file=sys.stderr)
        else:
            per = stats["predict_ms"] / stats["sampled"] / 1000.0 if stats["sampled"] else 0.0
            print(
                f"{obj['object']}: {stats['sampled']}/{stats['windows']} windows sampled "
                f"({stats['inherited_failed']} inherited-failed, {stats['failed']} failed), "
                f"{stats['predictions']} predictions, {per:.1f}s/window MC",
                file=sys.stderr,
            )

    wall_s = time.perf_counter() - t_start
    out = args.output.expanduser().resolve()
    out.parent.mkdir(parents=True, exist_ok=True)
    stem = out.with_suffix("")
    windows_path = stem.with_name(stem.name + "_windows.jsonl")
    preds_path = stem.with_name(stem.name + "_predictions.jsonl")
    xval_path = stem.with_name(stem.name + "_mc_crossval.json")

    walk._write_jsonl(windows_path, all_windows)
    walk._write_jsonl(preds_path, all_preds)

    xsum = _crossval_summary(all_xval)
    det = _determinism_summary(all_preds, ref_preds)
    xval_path.write_text(
        json.dumps(
            {
                "tool": walk.TOOL,
                "config_arm": MC_ARM,
                "profile": args.profile,
                "mc_samples": args.mc_samples,
                "windows_sampled": totals["sampled"],
                "wall_seconds": wall_s,
                "mean_mc_predict_seconds_per_window": (
                    totals["predict_ms"] / totals["sampled"] / 1000.0 if totals["sampled"] else None
                ),
                "crossval_mc_over_native": xsum,
                "refit_determinism_vs_reference": det,
            },
            indent=2,
        )
        + "\n"
    )
    out.write_text("[]\n")

    lines = [
        f"\n──── layup MC sky covariance ({args.profile}, N={args.mc_samples}) ────",
        f"  Objects:          {n_delivered}/{len(selected)}",
        (
            f"  Windows:          {totals['windows']}  (sampled {totals['sampled']}, "
            f"inherited-failed {totals['inherited_failed']}, failed {totals['failed']})"
        ),
        (
            f"  Predictions:      {totals['predictions']}  (with 2x2 {totals['with_cov']}, "
            f"none {totals['predictions'] - totals['with_cov']})"
        ),
        (
            f"  Variants:         {totals['variants']} propagations, "
            f"{totals['predict_ms'] / 1000.0:.1f}s MC predict, "
            f"{totals['fit_ms'] / 1000.0:.1f}s refit, {wall_s:.1f}s wall"
        ),
    ]
    if xsum.get("n"):

        def ratio_line(label: str, d: dict, fmt: str = ".4f") -> str:
            return (
                f"  {label:<19} p10 {d['p10']:{fmt}}  "
                f"median {d['median']:{fmt}}  p90 {d['p90']:{fmt}}"
            )

        lines += [
            "  ── MC / native (layup's own linearized 2x2) ──",
            ratio_line("variance ratio xx:", xsum["variance_ratio_xx"]),
            ratio_line("variance ratio yy:", xsum["variance_ratio_yy"]),
            ratio_line("ellipse area ratio:", xsum["area_ratio"]),
            ratio_line("correlation Δ:", xsum["correlation_delta"], "+.5f")
            + f"   (n = {xsum['n']})",
        ]
    if det.get("n"):
        lines.append(
            f"  refit vs reference: max {det['max_sep_arcsec']:.3e}\", "
            f"median {det['median_sep_arcsec']:.3e}\" over {det['n']} nominal positions"
        )
    lines += [
        f"  Windows sidecar:  {windows_path}",
        f"  Predictions:      {preds_path}",
        f"  Cross-validation: {xval_path}",
        "─" * 60,
    ]
    print("\n".join(lines), file=sys.stderr)

    if n_delivered == 0:
        print("ERROR: no object produced a single window record.", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
