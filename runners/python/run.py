"""empyrean validation runner — Python channel.

Reads a rust-channel ValidationResult JSON for the inputs (initial
conditions and reference vectors), replays each propagation through
the empyrean Python wheel, and emits a python-channel JSON in the
same schema. The combined JSONs feed into

    validate report --results validation_rust.json,validation_python.json

so the Distribution Channel Fidelity table can compare bindings.

On first-order ephemeris rows this channel publishes the
engine-delivered sky covariance (grouping with the core channel), while
the rust channel keeps the harness projection as its golden with the
delivered covariance beside it as a diagnostic, so the cross-channel
matrix reads projection (rust) versus delivered (python / core) on those
rows by design.
"""

from __future__ import annotations

import argparse
import json
import math
import sys
import time
from datetime import datetime, timezone
from importlib.metadata import PackageNotFoundError, version
from pathlib import Path

import empyrean
import numpy as np
from empyrean import CovarianceKind, OrbitOutcome, UncertaintyMethod
from empyrean._empyrean_rs import (
    _determine,
    _generate_ephemeris,
    _get_observers,
    _propagate,
)

# The distribution's own canonical uncertainty-method → wire-int map, so this
# channel lowers an `UncertaintyMethod` to the int the low-level `_propagate`
# expects exactly as `empyrean.propagate` does (no restated encoding to drift).
from empyrean.propagation.config import _UNCERTAINTY_METHOD_TO_INT

# The 0.11 per-method product builders — the SAME functions `empyrean.propagate`
# runs on the result dict, so the python channel reads its products off the
# wrapper rather than recomputing them (mirrors the rust channel reading the
# safe wrapper's PackedJoint / OrbitStatuses / MixtureComponent).
from empyrean.propagation.mixtures import ComponentStatus, build_mixture_chains
from empyrean.propagation.propagate import _build_tagged_covariance
from empyrean.propagation.status import build_orbit_statuses

# Mirrors empyrean::ForceModelTier integer encoding.
_TIER_TO_INT = {"approximate": 0, "basic": 1, "standard": 2}

# Frame integer mirrors empyrean wrapper: ICRF=0, EclipticJ2000=1.
_FRAME_ICRF = 0
# Origin is a NAIF id on the wire; 0 = Solar System Barycenter.
_ORIGIN_SSB = 0
_REP_CARTESIAN = 0
_AU_KM = 149_597_870.700


# ── Uncertainty-method axis (0.11 per-method products) ──────────────────────
#
# Schema `uncertainty_modes` tag → (attach_covariance, empyrean UncertaintyMethod).
# Mirrors the rust channel's `build_uncertainty_axes` and the cli channel's
# `method_for_tag`: `none` is the covariance-free first-order path; every
# other tag attaches the synthetic covariance and runs its named rung. The
# method is lowered to the low-level `_propagate` wire int via the
# distribution's own `UncertaintyMethod` map, so the python channel asks for
# the identical method the rust / cli channels ask for.
_METHOD_BY_TAG: dict[str, tuple[bool, UncertaintyMethod]] = {
    "none": (False, UncertaintyMethod.FIRST_ORDER),
    "first_order": (True, UncertaintyMethod.FIRST_ORDER),
    "second_order": (True, UncertaintyMethod.SECOND_ORDER),
    "auto": (True, UncertaintyMethod.AUTO),
    "sigma_point": (True, UncertaintyMethod.SIGMA_POINT),
    "monte_carlo": (True, UncertaintyMethod.MONTE_CARLO),
    "gaussian_mixture": (True, UncertaintyMethod.GAUSSIAN_MIXTURE),
}

# Sampling methods cost ~100-120 propagations per call, so — like the rust
# channel's `timing_runs = 1` — they are timed once rather than best-of-N.
_SAMPLING_TAGS = frozenset(
    {"sigma_point", "monte_carlo", "gaussian_mixture"}
)

# The suite-wide Monte-Carlo sample count and seed. A LITERAL mirror of
# `empyrean_validation::schema::uncertainty_modes::MONTE_CARLO_SAMPLE_COUNT` /
# `MONTE_CARLO_SEED` (src/schema.rs): N = 100, seed = the ASCII bytes of
# "EMPYREAN" (0x454D5059_5245414E). The python channel cannot import the Rust
# const; the values are pinned against drift by tests/test_run_products.py.
_MONTE_CARLO_SAMPLE_COUNT = 100
_MONTE_CARLO_SEED = 0x454D_5059_5245_414E

# The OD-fit method-axis note. A LITERAL mirror of
# `empyrean_validation::schema::OD_METHOD_AXIS_NOT_PRODUCED` (src/schema.rs):
# `ODConfig` carries no `uncertainty_method` at this distribution revision
# (ae00643), so OD fits run method-free — every OD fit row records this by name
# rather than a blank or a silent first-order default. Pinned in the tests.
_OD_METHOD_AXIS_NOT_PRODUCED = (
    "OD method axis not produced at this pin: "
    "ODConfig.uncertainty_method not on the wrapper"
)

# The 12 per-method output fields the plan/rust input carries. A python row is
# built as a copy of its input row, so each must be CLEARED before the python
# channel repopulates it from ITS OWN delivery — otherwise the input channel's
# products ride out under this channel's name. `emp_pos_cov_au2` /
# `emp_radec_cov_arcsec2` are the collapsed moment views that travel with them
# and are reset for the same reason.
_PER_METHOD_FIELDS = (
    "resolved_method",
    "cov_kind",
    "cov_joint_width",
    "cov_tri",
    "orbit_delivered",
    "orbit_status",
    "mix_n_components_total",
    "mix_weight_delivered",
    "mix_n_failed",
    "mix_n_unresolved",
    "mix_n_curvature_refused",
    "mix_n_sky_linearization_refused",
    "emp_pos_cov_au2",
    "emp_radec_cov_arcsec2",
)


def _synthetic_covariance() -> np.ndarray:
    """A synthetic typical-NEO 6×6 input covariance: 1 km (1σ) position, 1
    mm·s⁻¹ (1σ) velocity, uncorrelated. Bit-identical to the rust / cli
    channels' ``synthetic_covariance`` (same literals, same AU/km constant) so
    a same-method cross-channel diff reflects binding drift, not a different
    prior.
    """
    pos_var_au2 = (1.0 / _AU_KM) ** 2
    vel_var_au2_d2 = (1e-6 / _AU_KM * 86_400.0) ** 2
    cov = np.zeros((6, 6), dtype=np.float64)
    cov[0, 0] = cov[1, 1] = cov[2, 2] = pos_var_au2
    cov[3, 3] = cov[4, 4] = cov[5, 5] = vel_var_au2_d2
    return cov


# C-ABI wire discriminant for a delivered covariance kind — the `cov_kind` the
# schema carries. Mirrors `empyrean.orbits.joint._KIND_TO_CODE` and the rust /
# cli channels' `cov_kind_wire` (linear 0, second-order 1, mixture 3,
# monte-carlo 4, sigma-point 5); pinned in the tests.
_COV_KIND_WIRE = {
    CovarianceKind.LINEAR: 0,
    CovarianceKind.SECOND_ORDER: 1,
    CovarianceKind.MIXTURE: 3,
    CovarianceKind.MONTE_CARLO: 4,
    CovarianceKind.SIGMA_POINT: 5,
}

# The schema `uncertainty_modes` tag for a DELIVERED covariance kind — the
# `resolved_method`. Mirrors the rust channel's `resolved_method_for`: on an
# explicit method it equals the request when the engine honoured it, and names
# the delivered kind (never the request) if a different kind came back.
_RESOLVED_METHOD_TAG = {
    CovarianceKind.LINEAR: "first_order",
    CovarianceKind.SECOND_ORDER: "second_order",
    CovarianceKind.MIXTURE: "gaussian_mixture",
    CovarianceKind.MONTE_CARLO: "monte_carlo",
    CovarianceKind.SIGMA_POINT: "sigma_point",
}


def _propagate_failure_variant(code: int | None, message: str) -> str:
    """Name an ``EMPYREAN_PROPAGATE_FAILURE_*`` classification code. The integer
    codes are the C-ABI contract; an unrecognized code falls back to the
    engine's own message so no failure is ever reported as a bare number.
    Mirrors the rust channel's ``propagate_failure_variant``.
    """
    names = {
        1: "integration",
        2: "kepler_dt_backprop",
        3: "transform",
        4: "covariance_input",
        5: "sigma_point",
        6: "sampled_parameter",
        7: "ensemble_member",
        8: "output_assembly",
        99: "other",
    }
    if code in names:
        return names[code]
    return f"code_{code}({message})"


def _orbit_outcome_channel(
    outcome: str, err_code: int | None, err_msg: str | None, withheld: str | None
) -> tuple[bool, str]:
    """The per-orbit delivery outcome, read off the status table — the SOLE
    delivery discriminator, never a state count. Returns
    ``(orbit_delivered, orbit_status)``. Mirrors the rust channel's
    ``orbit_outcome_channel``.
    """
    if outcome == OrbitOutcome.FAILED.value:
        return False, f"failed:{_propagate_failure_variant(err_code, err_msg or '')}"
    # Delivered: a covariance that was expected but could not be read back is a
    # withheld covariance, named rather than left blank.
    if withheld is not None:
        return True, f"cov_withheld:{withheld}"
    return True, "delivered"


def _populate_mixture_tallies(result: dict, row: dict) -> None:
    """Fill a row's six Gaussian-mixture tallies from the wrapper's RETAINED
    mixture components (the per-component status table — the 0.11 product).
    Empty on every non-mixture delivery, so the tally only fills on a row the
    engine actually split. Mirrors the rust channel's
    ``populate_mixture_tallies`` (retained components; the core channel reads
    villeneuve's pre-retention tallies — a cross-channel surface difference,
    not physics).
    """
    chains = build_mixture_chains(result)
    if chains is None or len(chains) == 0:
        return
    # Components retained for this (single) orbit.
    comp = chains.select("orbit_index", 0)
    if len(comp) == 0:
        return
    weights = comp.weight.to_numpy(zero_copy_only=False)
    statuses = comp.status.to_pylist()

    def _n(member: ComponentStatus) -> int:
        return int(sum(1 for s in statuses if s == member.value))

    row["mix_n_components_total"] = len(comp)
    row["mix_weight_delivered"] = float(weights.sum())
    row["mix_n_failed"] = _n(ComponentStatus.FAILED)
    row["mix_n_unresolved"] = _n(ComponentStatus.UNRESOLVED)
    row["mix_n_curvature_refused"] = _n(ComponentStatus.CURVATURE_REFUSED)
    row["mix_n_sky_linearization_refused"] = _n(ComponentStatus.SKY_LINEARIZATION_REFUSED)


def _delivered_sky_cov_arcsec2(cov6: np.ndarray, dec_rad: float) -> list[list[float]]:
    """Extract the (RA·cosδ, Dec) sky-plane 2×2 covariance in arcsec² from the
    engine-delivered 6×6 ephemeris covariance, ordered (rho, RA, Dec, vrho,
    vRA, vDec) in (AU, deg). The RA row/column are scaled by cosδ to match
    ``d_ra_arcsec`` and deg² is converted to arcsec². Mirrors the rust
    channel's ``delivered_sky_covariance``.
    """
    ra, dec = 1, 2
    cosd = math.cos(dec_rad)
    deg2_to_arcsec2 = 3600.0 * 3600.0
    c_ra_ra = float(cov6[ra][ra]) * cosd * cosd * deg2_to_arcsec2
    c_ra_dec = float(cov6[ra][dec]) * cosd * deg2_to_arcsec2
    c_dec_dec = float(cov6[dec][dec]) * deg2_to_arcsec2
    return [[c_ra_ra, c_ra_dec], [c_ra_dec, c_dec_dec]]


def _with_od_method_note(base: str) -> str:
    """Append the OD method-axis note to an OD fit row's base note so the row
    records the missing axis by name — never a blank, never a silent
    first-order default. An empty base yields the marker alone. Mirrors the
    rust channel's ``with_od_method_note``.
    """
    if not base:
        return _OD_METHOD_AXIS_NOT_PRODUCED
    return f"{base}; {_OD_METHOD_AXIS_NOT_PRODUCED}"


def _fill_propagation_products(row: dict, result: dict, attach_cov: bool) -> None:
    """Populate a propagation row's 0.11 per-method products from the wrapper
    result dict: the per-orbit delivery outcome, the delivered packed joint
    (`cov_kind` / `cov_joint_width` / `cov_tri`) and its resolved kind, the
    collapsed position 3×3 moment view, and the retained mixture tallies.
    Mirrors the rust channel's propagation-row population.
    """
    status = build_orbit_statuses(result["outcomes"])
    outcome = status.outcome.to_pylist()[0]
    err_code = status.error_code.to_pylist()[0]
    err_msg = status.error_message.to_pylist()[0]

    # The resolved covariance kind + packed joint, read off the delivered
    # tagged covariance (only on a row that carried a covariance). A covariance
    # that was attached (expected) but unreadable is a WITHHELD covariance,
    # named on the outcome below rather than back-filled.
    withheld: str | None = None
    if attach_cov:
        tagged = _build_tagged_covariance(result)
        pj = None
        if (
            tagged is not None
            and len(tagged) > 0
            and bool(tagged.has_tagged.to_pylist()[0])
        ):
            pj = tagged.covariance.joint(0)
        tri = None if pj is None else np.asarray(pj.tri, dtype=np.float64)
        if pj is not None and tri is not None and np.isfinite(tri).all():
            row["resolved_method"] = _RESOLVED_METHOD_TAG[pj.kind]
            row["cov_kind"] = _COV_KIND_WIRE[pj.kind]
            row["cov_joint_width"] = int(pj.width)
            row["cov_tri"] = [float(v) for v in tri]
            # The 6×6 moment view's position 3×3 (AU²) — the compared envelope.
            m = pj.state_block()
            row["emp_pos_cov_au2"] = [[float(m[i][j]) for j in range(3)] for i in range(3)]
        else:
            withheld = "no tagged covariance returned"

    row["orbit_delivered"], row["orbit_status"] = _orbit_outcome_channel(
        outcome, err_code, err_msg, withheld
    )
    _populate_mixture_tallies(result, row)


def _fill_ephemeris_products(row: dict, result: dict, dec_rad: float) -> None:
    """Populate an ephemeris row's products: the per-orbit delivery outcome and
    the delivered sky covariance (projected to the RA·cosδ / Dec 2×2).
    `resolved_method` / `cov_kind` stay ``None`` — the wrapper flattens the
    delivered sky covariance to a bare 6×6 with no resolved-kind tag and no
    packed joint, so back-filling them from the request would be a silent
    substitution (the named gap, exactly as the rust channel).

    The published covariance is the engine-DELIVERED one on every row
    including first order (grouping this channel with the core channel; the
    rust channel keeps the harness projection as its golden), so a
    first-order ephemeris row reads delivered-vs-projection against the rust
    reference in the cross-channel matrix by design.
    """
    status = build_orbit_statuses(result["outcomes"])
    outcome = status.outcome.to_pylist()[0]
    err_code = status.error_code.to_pylist()[0]
    err_msg = status.error_message.to_pylist()[0]

    has_cov = np.asarray(result.get("has_covariance", []), dtype=bool)
    if has_cov.size > 0 and bool(has_cov[0]):
        cov6 = np.asarray(result["covariance"])[0]
        if np.isfinite(cov6).all():
            row["emp_radec_cov_arcsec2"] = _delivered_sky_cov_arcsec2(cov6, dec_rad)

    row["orbit_delivered"], row["orbit_status"] = _orbit_outcome_channel(
        outcome, err_code, err_msg, None
    )


def _wheel_source_version() -> str:
    """Provenance string stamped on every python-channel row.

    The python channel IS the empyrean wheel — it calls the binding directly
    — so it reports the installed wheel version. No-hidden-fallbacks: when the
    version can't be resolved, stamp an explicit ``unknown (<reason>)`` rather
    than a silent blank or a fabricated value.
    """
    try:
        return f"empyrean {version('empyrean')} (python wheel)"
    except PackageNotFoundError:
        return "empyrean unknown (empyrean distribution not found)"
    except Exception as e:  # noqa: BLE001
        return f"empyrean unknown ({e})"


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
    method_tag: str,
    n_timing_runs: int,
) -> tuple[list[float], float, dict] | None:
    """Propagate once under ``method_tag``'s uncertainty method, returning
    ``(out_pos_au, min_time_ms, raw_result)`` or ``None`` on failure.

    The method is mapped to an ``UncertaintyMethod`` and lowered to the
    low-level wire int; every method but ``none`` attaches the synthetic
    covariance and requests the provenance-tagged readback so the caller can
    read the 0.11 per-method products off ``raw_result``.
    """
    tier = _TIER_TO_INT.get(force_model)
    if tier is None:
        return None
    method_entry = _METHOD_BY_TAG.get(method_tag)
    if method_entry is None:
        # Refuse an unknown method by name rather than silently substituting
        # one (the no-silent-substitution invariant).
        print(
            f"  {object_id} {force_model}: SKIP unknown uncertainty_method {method_tag!r}",
            file=sys.stderr,
        )
        return None
    attach_cov, method = method_entry
    um_int = _UNCERTAINTY_METHOD_TO_INT[method]

    times = np.array([target_t_mjd_tdb], dtype=np.float64)
    epochs = np.array([epoch_mjd_tdb], dtype=np.float64)
    elements = np.array(
        [
            [
                ic_pos_au[0],
                ic_pos_au[1],
                ic_pos_au[2],
                ic_vel_au_d[0],
                ic_vel_au_d[1],
                ic_vel_au_d[2],
            ]
        ],
        dtype=np.float64,
    )
    # Attach the synthetic typical-NEO covariance on every method but
    # none (which runs covariance-free), same prior as the rust / cli
    # channels so a same-method cross-channel diff reflects binding drift only.
    covariances = np.zeros((1, 6, 6), dtype=np.float64)
    if attach_cov:
        covariances[0] = _synthetic_covariance()
    has_covariance = np.array([attach_cov])
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

    # Monte-Carlo carries the suite-wide sample count + seed (a cross-channel
    # bit check); the other methods take the wrapper defaults, matching the
    # rust channel's sigma_point() / gaussian_mixture() / auto().
    mc_kwargs = (
        {"mc_n_samples": _MONTE_CARLO_SAMPLE_COUNT, "mc_seed": _MONTE_CARLO_SEED}
        if method_tag == "monte_carlo"
        else {}
    )
    # Sampling methods cost ~100-120 propagations per call — time once.
    runs = 1 if method_tag in _SAMPLING_TAGS else max(1, n_timing_runs)
    timings_ms = []
    last_result = None
    for _ in range(runs):
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
                uncertainty_method=um_int,
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
                with_tagged_covariance=attach_cov,
                **mc_kwargs,
            )
        except Exception as e:  # noqa: BLE001
            print(
                f"  {object_id} {force_model} dt→{target_t_mjd_tdb}: FAIL {e}",
                file=sys.stderr,
            )
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
    return pos, min(timings_ms), last_result


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
    n_timing_runs: int,
    method_tag: str,
) -> tuple[float, float, float, float, float, dict] | BaseException | None:
    """Generate ephemeris at target_t for obs_code under ``method_tag``'s
    uncertainty method.

    Return ``(ra_rad, dec_rad, rho_au, light_time_days, emp_time_ms,
    raw_result)`` where ``emp_time_ms`` is the best-of-``n_timing_runs`` wall
    time of the ``_generate_ephemeris`` call — the same stopwatch boundary the
    core and propagation channels use (array construction outside; the generate
    call inside) — and the sky covariance the engine delivers rides
    ``raw_result["covariance"]`` for the caller to publish. Returns the
    exception on failure (not ``None``) so the caller can emit a FAIL row.
    """
    tier = _TIER_TO_INT.get(force_model)
    if tier is None:
        return None
    method_entry = _METHOD_BY_TAG.get(method_tag)
    if method_entry is None:
        print(
            f"  {object_id} ephemeris: SKIP unknown uncertainty_method {method_tag!r}",
            file=sys.stderr,
        )
        return None
    attach_cov, method = method_entry
    um_int = _UNCERTAINTY_METHOD_TO_INT[method]
    mc_kwargs = (
        {"mc_n_samples": _MONTE_CARLO_SAMPLE_COUNT, "mc_seed": _MONTE_CARLO_SEED}
        if method_tag == "monte_carlo"
        else {}
    )

    # The observer basis is an explicit request as of the 0.10 ABI.
    # (Frame::ICRF, Origin::SSB) is the construction basis — the states come
    # back exactly as built, untransformed — which is what ephemeris
    # generation requires and what this call received implicitly before.
    obs_states = _get_observers(
        [obs_code],
        np.array([target_t_mjd_tdb], dtype=np.float64),
        _FRAME_ICRF,
        _ORIGIN_SSB,
    )
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
        [
            [
                ic_pos_au[0],
                ic_pos_au[1],
                ic_pos_au[2],
                ic_vel_au_d[0],
                ic_vel_au_d[1],
                ic_vel_au_d[2],
            ]
        ],
        dtype=np.float64,
    )
    # Same synthetic prior as the propagation path / rust / cli channels, on
    # every method but none. The delivered sky covariance is then the
    # engine's per-method projection, not the covariance-free default.
    covariances = np.zeros((1, 6, 6), dtype=np.float64)
    if attach_cov:
        covariances[0] = _synthetic_covariance()
    has_covariance = np.array([attach_cov])
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

    # Best-of-`n_timing_runs` like `_propagate_one`: the stopwatch wraps exactly
    # the `_generate_ephemeris` call for this one object / site / epoch, with all
    # array construction above done outside the timed region — the same boundary
    # the core runner's `replay_ephemeris` uses. The call is deterministic, so the
    # delivered sky position is unchanged from the pre-timing single call.
    timings_ms = []
    result = None
    for _ in range(max(1, n_timing_runs)):
        t0 = time.perf_counter()
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
                force_model=tier,
                ng_alphas=ng_alphas,
                ng_r0s=ng_r0s,
                ng_ms=ng_ms,
                ng_ns=ng_ns,
                ng_ks=ng_ks,
                non_grav_dts=non_grav_dts,
                uncertainty_method=um_int,
                **mc_kwargs,
            )
        except Exception as e:  # noqa: BLE001
            print(
                f"  {object_id} ephemeris t={target_t_mjd_tdb}: FAIL {e}",
                file=sys.stderr,
            )
            # Return the exception (not None) so the caller can emit a FAIL row
            # carrying the engine's message instead of silently dropping the row.
            # The other return-None paths (tier invalid, empty observer states,
            # empty result) remain legitimate skips.
            return e
        timings_ms.append((time.perf_counter() - t0) * 1000.0)

    if result is None or len(result.get("ra", [])) == 0:
        return None

    ra_deg = float(result["ra"][0])
    dec_deg = float(result["dec"][0])
    rho_au = float(result["rho"][0])
    lt_d = float(result["light_time"][0]) if "light_time" in result else math.nan

    return (
        math.radians(ra_deg),
        math.radians(dec_deg),
        rho_au,
        lt_d,
        min(timings_ms),
        result,
    )


def _single_fit(batch: dict, object_id: str, what: str) -> dict:
    """Reduce a batch ``_determine`` result to this fixture's one fit.

    ``_determine`` is batch-first: it groups the observations by ADES object
    identifier and returns ``{"objects": [...]}`` with one entry per group,
    where a failed fit is an entry carrying ``delivered = False`` and an
    ``error`` rather than a missing entry or a raised exception.

    Both of those are silent hazards for a per-fixture runner. Indexing
    ``[0]`` blindly would fit whichever object sorted first if a fixture ever
    grouped into several, and reading a non-delivered entry's fit fields
    would harvest absent keys as if they were results. So: exactly one
    delivered entry, or raise with which of the two went wrong.

    Mirrors the rust channel's ``DetermineResults::into_single`` so the two
    channels fail on the same conditions with the same reasons.
    """
    objects = batch.get("objects")
    if objects is None:
        raise ValueError(
            f"{object_id} {what}: determine returned no `objects` table "
            "(expected the batch-first result shape)"
        )
    if len(objects) != 1:
        ids = ", ".join(str(o.get("object_id")) for o in objects)
        raise ValueError(
            f"{object_id} {what}: observations grouped into {len(objects)} "
            f"objects ({ids}); this fixture must carry exactly one"
        )
    entry = objects[0]
    if not entry.get("delivered"):
        raise ValueError(
            f"{object_id} {what}: fit failed for "
            f"{entry.get('object_id')} — {entry.get('error')} "
            f"[{entry.get('error_kind')}]"
        )
    return entry


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
    # Mirror the engine's ODConfig::default(): solve_for=Auto so comets can
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
        batch = _determine(
            obs_dict=obs_dict,
            config_dict=config_dict,
            initial_orbits_dict=None,
        )
        result = _single_fit(batch, object_id, "OD")
    except Exception as e:  # noqa: BLE001
        print(f"  {object_id} OD: FAIL {e}", file=sys.stderr)
        # Return the exception (not None) so the caller can emit a FAIL row
        # carrying the engine's message, mirroring the rust / core channels,
        # instead of silently dropping the object.
        return e
    ms = (time.perf_counter() - t0) * 1000.0

    pos = [float(result["orbit_x"]), float(result["orbit_y"]), float(result["orbit_z"])]
    return pos, result, ms


def _determine_nongrav_one(
    object_id: str,
    psv_text: str,
    force_model: str,
    max_iterations: int,
    excluded_perturbers_naif: list[int] | None = None,
) -> tuple[list[float], dict, float] | None:
    """Run a non-grav-recovery OD on the PSV text via the wheel.

    Forces ``solve_for=state_and_nongrav`` so the fit recovers A1/A2/A3 plus
    their 9×9 (state + non-grav) covariance, mirroring the rust / c / cli /
    core non_grav_recovery channels. Returns (fitted_orbit_pos_au,
    raw_result_dict, time_ms) or None.
    """
    if force_model not in _TIER_TO_INT:
        return None

    obs_dict = {"ades": psv_text}
    config_dict = {
        "force_model": force_model,
        "max_iterations": max_iterations,
        "solve_for": "state_and_nongrav",
    }
    if excluded_perturbers_naif:
        config_dict["excluded_perturbers_naif"] = list(excluded_perturbers_naif)
    t0 = time.perf_counter()
    try:
        batch = _determine(
            obs_dict=obs_dict,
            config_dict=config_dict,
            initial_orbits_dict=None,
        )
        result = _single_fit(batch, object_id, "non-grav OD")
    except Exception as e:  # noqa: BLE001
        print(f"  {object_id} non-grav OD: FAIL {e}", file=sys.stderr)
        return None
    ms = (time.perf_counter() - t0) * 1000.0

    pos = [float(result["orbit_x"]), float(result["orbit_y"]), float(result["orbit_z"])]
    return pos, result, ms


def _solve_metadata(raw: dict) -> dict:
    """Solve-for dispositions + warnings from a fit, in the schema's shape.

    Read off the *result*, never off the config: under ``solve_for=auto``
    the request and the outcome differ by design, and what the report needs
    is the width the fit actually ran at.

    The disposition matters for reading the σ that comes with it — a
    *considered* axis already has its uncertainty inside the delivered
    covariance, a *fixed* one contributed nothing — so a cross-channel σ
    comparison is only meaningful when the dispositions agree. Missing keys
    stay absent rather than defaulting to ``"fixed"``, which would assert a
    partition the fit never reported.
    """
    # Every key is written on every call, cleared first. A python row is
    # built as a copy of the input row, so a key left untouched here would
    # let the *input* channel's dispositions ride out under this channel's
    # name — one channel's fit metadata attributed to another's fit.
    out: dict = {
        "od_disposition_marsden": None,
        "od_disposition_dt": None,
        "od_disposition_amrat": None,
        "od_disposition_thrust": [],
        "od_solve_for_used": None,
        "od_warnings": [],
        "od_joint_covariance_width": None,
    }
    disp = raw.get("dispositions") or {}
    for axis in ("marsden", "dt", "amrat"):
        v = disp.get(axis)
        if v is not None:
            out[f"od_disposition_{axis}"] = str(v)
    # Trailing all-fixed thrust entries carry no information (every orbit
    # declares the full segment budget); trim to the last active one.
    thrust = list(disp.get("thrust") or [])
    last = -1
    for i, t in enumerate(thrust):
        if str(t) != "fixed":
            last = i
    if last >= 0:
        out["od_disposition_thrust"] = [str(t) for t in thrust[: last + 1]]
    if raw.get("solve_for_used") is not None:
        out["od_solve_for_used"] = str(raw["solve_for_used"])
    warnings = raw.get("warnings") or []
    if warnings:
        out["od_warnings"] = [str(w) for w in warnings]
    # Width of the go-forward joint. A state-only fit reports no
    # `solved_covariance` and its joint is the 6×6 — width 6, not absent,
    # so "state-only" stays distinct from "channel reported nothing".
    solved = raw.get("solved_covariance")
    if isinstance(solved, dict) and solved.get("width") is not None:
        out["od_joint_covariance_width"] = int(solved["width"])
    else:
        out["od_joint_covariance_width"] = 6
    return out


def _read_fitted_non_grav(raw: dict) -> tuple[list[float | None], list[float | None]]:
    """Extract fitted (a1, a2, a3) and their 1σ from a non-grav OD result.

    Loud-failure contract: a value reads as "non-grav not recovered" (None,
    never 0 / NaN) when the fit silently fell back to a state-only solve — i.e.
    the 9×9 (state + A1/A2/A3) covariance is absent (`covariance_9x9` missing /
    None) — or when the fitted coefficient itself is non-finite. σ_aᵢ is the
    sqrt of diagonal entry 6/7/8 of the 9×9 covariance.
    """
    # covariance_9x9 is set only when non-grav was actually solved; the wheel
    # emits it as a flat 81-element row-major list (None / absent otherwise).
    cov9 = raw.get("covariance_9x9")
    if cov9 is None or len(cov9) != 81:
        return [None, None, None], [None, None, None]
    cov = np.asarray(cov9, dtype=np.float64).reshape(9, 9)

    # orbit_a1/a2/a3 are present only when the fit carried a non-grav signal.
    a_vals = [raw.get("orbit_a1"), raw.get("orbit_a2"), raw.get("orbit_a3")]
    a_out: list[float | None] = [None, None, None]
    s_out: list[float | None] = [None, None, None]
    for i, a in enumerate(a_vals):
        if a is None:
            continue
        af = float(a)
        if not math.isfinite(af):
            continue
        var = float(cov[6 + i][6 + i])
        if not math.isfinite(var) or var < 0.0:
            continue
        a_out[i] = af
        s_out[i] = math.sqrt(var)
    return a_out, s_out


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
    print(
        f"Loaded {len(rust_rows)} rust rows; replaying through Python channel...",
        file=sys.stderr,
    )

    timestamp = datetime.now(timezone.utc).isoformat()
    source_version = _wheel_source_version()

    # Per-object JPL SBDB reference non-grav, keyed by object name. The
    # optical-only orbit_determination rows carry ic_a1/a2/a3 = None, but the
    # propagation / ephemeris rows for the same object carry the SBDB
    # reference (ic_a1/ic_a2/ic_a3). Harvest it here so the OD branch can run
    # the non_grav_recovery second pass for objects with a known non-grav
    # signal — mirroring the rust runner's `data.a1 != 0.0 || ...` check.
    ref_non_grav: dict[str, tuple[float, float, float]] = {}
    for r in rust_rows:
        a1, a2, a3 = r.get("ic_a1"), r.get("ic_a2"), r.get("ic_a3")
        if a1 is None and a2 is None and a3 is None:
            continue
        a1, a2, a3 = a1 or 0.0, a2 or 0.0, a3 or 0.0
        if a1 != 0.0 or a2 != 0.0 or a3 != 0.0:
            ref_non_grav[r["object"]] = (a1, a2, a3)

    out_rows = []
    n_skipped = 0
    n_missing_fixture = 0
    for r in rust_rows:
        ic_pos = r.get("ic_pos_au")
        ic_vel = r.get("ic_vel_au_d")
        # OD rows (optical and optical+radar) discover the orbit from
        # observations — no IC required. Both seed themselves from a PSV
        # fixture, so a null IC is expected on them, not a reason to skip.
        if r["test_type"] not in (
            "orbit_determination",
            "orbit_determination_radar",
        ) and (ic_pos is None or ic_vel is None):
            n_skipped += 1
            continue
        # Per-method OD transport rows are `not produced` at this pin: this
        # runner's post-fit transport is the orbit-versus-reference compare,
        # which emits no product-bearing result row (the core channel produces
        # them), exactly as the rust channel. Their absence from this channel's
        # output reads as `not produced` in the report — never a blank row.
        if r["test_type"] == "orbit_determination_transport":
            n_skipped += 1
            continue

        new = dict(r)
        new["channel"] = "python"
        new["timestamp"] = timestamp
        # This channel exercised the wheel, not the rust engine the input row
        # carried; overwrite the inherited source_version with our own. The
        # non_grav_recovery row is a dict(new) copy, so it inherits this stamp.
        new["source_version"] = source_version
        # Reset every empyrean-output field; we repopulate from the Python
        # channel below. A python row is a copy of its input (plan / rust) row,
        # so without this the input channel's values — including the 12
        # per-method product fields and the moment views that travel with them
        # — would ride out under this channel's name (a leak).
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
        for k in _PER_METHOD_FIELDS:
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
                method_tag=r.get("propagation_uncertainty"),
                n_timing_runs=args.n_timing_runs,
            )
            if ret is None:
                n_skipped += 1
                continue
            pos, ms, prop_result = ret
            new["emp_pos_au"] = pos
            new["emp_time_ms"] = ms
            ref = r.get("ref_pos_au")
            if ref:
                d = math.sqrt(sum((pos[i] - ref[i]) ** 2 for i in range(3)))
                new["emp_vs_horizons_km"] = d * _AU_KM
            # 0.11 per-method products off the delivered packed joint, the
            # per-orbit outcome table, and the retained mixture components.
            attach_cov = _METHOD_BY_TAG[r["propagation_uncertainty"]][0]
            _fill_propagation_products(new, prop_result, attach_cov)

        elif r["test_type"] == "orbit_determination":
            # "/"-bearing object names (comets / interstellars) store the
            # fixture with the slash rewritten to "_".
            psv_path = args.fixtures_dir / f"{r['object'].replace('/', '_')}.psv"
            if not psv_path.exists():
                # Loudly, and fatally at the end — same seam as the C and CLI
                # drivers. A silent `continue` deleted the OD row from this
                # channel's output while the run still exited 0, so a channel
                # that fitted nothing looked like one that fitted everything.
                # The fixtures are fetched + hash-verified by `make fixtures`
                # (fixtures/README.md); a missing one here means something
                # bypassed that gate, never a normal condition.
                print(
                    f"  {r['object']} OD FAIL: no PSV fixture at {psv_path}",
                    file=sys.stderr,
                )
                n_missing_fixture += 1
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
            if isinstance(ret, BaseException):
                # Non-converged / failed OD: emit a FAIL row carrying the
                # engine's message, exactly as the rust and core channels do,
                # instead of silently dropping the object.
                new["od_converged"] = False
                new["notes"] = f"determine FAIL: {ret}"
                out_rows.append(new)
                n_skipped += 1
                continue
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
            new["od_reduced_chi2"] = float(
                raw.get("summary_reduced_chi2", float("nan"))
            )
            new.update(_solve_metadata(raw))
            # OD fits run method-free at this pin — ODConfig carries no
            # uncertainty_method on the wrapper (ae00643) — so every OD fit row
            # records the missing method axis by name rather than a blank or a
            # silent first-order default; the 12 per-method fields stay None
            # (reset above). The non_grav_recovery row is a dict(new) copy, so
            # it inherits this note.
            new["notes"] = _with_od_method_note(new.get("notes") or "")

            # ── Second OD: state + non-grav recovery ──────────────────────
            # For objects whose JPL SBDB reference carries a non-grav signal
            # (the Yarkovsky NEOs and the comets — looked up by object name
            # since the optical-only OD row itself carries ic_a1/a2/a3 = None),
            # run a second determine with solve_for=state_and_nongrav on the
            # SAME optical fixture and emit a separate non_grav_recovery row
            # carrying the FITTED A1/A2/A3 ± their 1σ so the report can compare
            # fitted-vs-JPL in σ. Mirrors the rust runner's reference-non-grav
            # check and the radar second-pass precedent. Objects with no
            # reference non-grav (the bulk of the catalog) are untouched.
            if r["object"] in ref_non_grav:
                ret_ng = _determine_nongrav_one(
                    object_id=r["object"],
                    psv_text=psv_text,
                    force_model=r["force_model"],
                    max_iterations=100,
                    excluded_perturbers_naif=r.get("excluded_perturbers_naif"),
                )
                if ret_ng is not None:
                    pos_ng, raw_ng, ms_ng = ret_ng
                    a_out, s_out = _read_fitted_non_grav(raw_ng)
                    # Start from the schema-valid OD row to inherit every
                    # required field, then override for the non_grav row.
                    ng_row = dict(new)
                    # Matches empyrean_validation::schema::test_types::
                    # NON_GRAV_RECOVERY so the rust merge / report deserializes
                    # this row.
                    ng_row["test_type"] = "non_grav_recovery"
                    ng_row["emp_pos_au"] = pos_ng
                    ng_row["emp_time_ms"] = ms_ng
                    ng_row["emp_vs_horizons_km"] = None
                    ng_row["n_obs_used"] = int(raw_ng.get("summary_num_selected", 0))
                    ng_row["od_iterations"] = int(raw_ng.get("iterations", 0))
                    ng_row["od_converged"] = bool(raw_ng.get("converged", False))
                    ng_row["od_rms_ra_arcsec"] = float(
                        raw_ng.get("summary_rms_ra", float("nan"))
                    )
                    ng_row["od_rms_dec_arcsec"] = float(
                        raw_ng.get("summary_rms_dec", float("nan"))
                    )
                    ng_row["od_rms_combined_arcsec"] = float(
                        raw_ng.get("summary_rms_combined", float("nan"))
                    )
                    ng_row["od_chi2"] = float(raw_ng.get("summary_chi2", float("nan")))
                    ng_row["od_reduced_chi2"] = float(
                        raw_ng.get("summary_reduced_chi2", float("nan"))
                    )
                    # The non-grav pass's own dispositions — this is the row
                    # where a silent fall-back to a state-only solve shows up
                    # as `marsden: fixed` despite the request.
                    ng_row.update(_solve_metadata(raw_ng))
                    # Fitted Marsden coefficients ± 1σ. None (never 0 / NaN)
                    # when the fit did not actually recover non-grav.
                    ng_row["od_a1"], ng_row["od_a2"], ng_row["od_a3"] = a_out
                    (
                        ng_row["od_a1_sigma"],
                        ng_row["od_a2_sigma"],
                        ng_row["od_a3_sigma"],
                    ) = s_out
                    out_rows.append(ng_row)

        elif r["test_type"] == "orbit_determination_radar":
            # Optical+radar OD: the same fit as the optical row, run on the
            # object's fixtures/psv-radar/ fixture. That fixture is the
            # byte-identical optical arc plus a <radar> delay/Doppler table,
            # which the wheel's read_ades folds into the fit automatically — so
            # this row differs from the optical one only by the observations,
            # never the configuration. No non_grav second pass: this row is the
            # radar-tightened state fit, cross-checked against find_orb's radar
            # fit the same way the optical row is against its optical fit.
            radar_dir = args.fixtures_dir.parent / "psv-radar"
            psv_path = radar_dir / f"{r['object'].replace('/', '_')}.psv"
            if not psv_path.exists():
                print(
                    f"  {r['object']} radar OD FAIL: no radar PSV fixture at {psv_path}",
                    file=sys.stderr,
                )
                n_missing_fixture += 1
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
            if isinstance(ret, BaseException):
                new["od_converged"] = False
                new["notes"] = f"radar determine FAIL: {ret}"
                out_rows.append(new)
                n_skipped += 1
                continue
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
            new["od_reduced_chi2"] = float(
                raw.get("summary_reduced_chi2", float("nan"))
            )
            new.update(_solve_metadata(raw))
            _prev_note = new.get("notes") or ""
            new["notes"] = (
                f"{_prev_note} | optical+radar" if _prev_note else "optical+radar"
            )

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
                n_timing_runs=args.n_timing_runs,
                method_tag=r.get("propagation_uncertainty"),
            )
            if isinstance(ret, BaseException):
                # Failed ephemeris (e.g. "no dense trajectory: initial state
                # overlaps" for a self-perturbing main-belt asteroid): emit a
                # FAIL row carrying the engine's message rather than dropping it.
                new["notes"] = f"ephemeris FAIL: {ret}"
                out_rows.append(new)
                n_skipped += 1
                continue
            if ret is None:
                n_skipped += 1
                continue
            ra_rad, dec_rad, rho_au, lt_d, ms, eph_result = ret
            new["emp_time_ms"] = ms
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
            # Per-orbit outcome + the delivered sky covariance (projected to
            # RA·cosδ / Dec). resolved_method / cov_kind stay None — the
            # ephemeris seam delivers a bare 6×6 with no resolved-kind tag and
            # no packed joint (the named gap, exactly as the rust channel).
            _fill_ephemeris_products(new, eph_result, dec_rad)

        out_rows.append(new)

    args.output.parent.mkdir(parents=True, exist_ok=True)
    try:
        _payload = json.dumps(out_rows, indent=2, default=str, allow_nan=False)
    except ValueError as _e:
        print(
            f"ERROR: refusing to write non-finite values to {args.output}: {_e}\n"
            "       Bare NaN/Infinity is invalid JSON — Rust's serde_json rejects it, so this\n"
            "       whole channel would fail the reduce merge with a line number and no cause.\n"
            "       A quantity that could not be computed must be null.",
            file=sys.stderr,
        )
        raise
    args.output.write_text(_payload)
    print(
        f"Wrote {len(out_rows)} python rows to {args.output} (skipped {n_skipped})",
        file=sys.stderr,
    )
    if n_missing_fixture:
        print(
            f"ERROR: {n_missing_fixture} OD row(s) had no PSV fixture under "
            f"{args.fixtures_dir}.\n"
            "       The fixtures come from the GCS snapshot pinned by "
            "fixtures/manifest.json;\n"
            "       `make fixtures` fetches + verifies them. Those OD rows are "
            "missing from this "
            "channel's output entirely.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
