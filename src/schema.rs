//! Shared row schema for every channel runner.
//!
//! [`ValidationResult`] is the per-row shape that every channel emits and
//! every consumer (report renderer, website, CI gate) deserializes. The
//! schema is intentionally flat — one struct, no tagged unions — because
//! the consumers (Python pandas, Rust serde, the website's TypeScript
//! types) all benefit from a uniform record layout that can be round-
//! tripped without an enum dispatch.
//!
//! # Field groups
//!
//! Fields fall into six conceptual groups, kept in struct order:
//!
//! 1. **Test identity** — `object`, `population`, `epoch_mjd_tdb`, `dt_days`,
//!    `t_mjd_tdb`, `force_model`, `test_type`, `channel`, `observer`.
//! 2. **Empyrean output** — `emp_*` and per-test-type metric fields
//!    (`separation_arcsec`, `d_ra_arcsec`, `d_dec_arcsec`, `d_rho_km`,
//!    `d_light_time_s`).
//! 3. **Initial conditions** — the SBDB / Horizons IC the channel started
//!    from. Carried on every row so non-network channels (python, c, cli)
//!    can replay a row without re-hitting Horizons.
//! 4. **Reference values** — Horizons truth at the target epoch.
//! 5. **OD-specific output** — `n_obs_used`, `od_*`, plus the
//!    `excluded_perturbers_naif` field that records which bodies were
//!    dropped from the force model during the fit.
//! 6. **Test-configuration tags** — `propagation_uncertainty` (Jet1 vs f64).
//! 7. **External-reference comparisons** — ASSIST and find_orb fields,
//!    populated by the `merge-external` post-processing step.
//! 8. **Metadata** — `timestamp`, `notes`.
//!
//! # JSON conventions
//!
//! - `Option::None` fields are written as `null` *unless* they have
//!   `#[serde(skip_serializing_if = "Option::is_none")]` — applied only to
//!   the new optional fields (`ic_non_grav_dt`, `propagation_uncertainty`,
//!   `excluded_perturbers_naif`) so older JSON files round-trip cleanly.
//! - `#[serde(deny_unknown_fields)]` is enabled so a renamed or added field
//!   surfaces as a deserialization error rather than silent data loss.
//!   Schema bumps require a coordinated PR across this crate and the
//!   consuming repos.

use serde::{Deserialize, Serialize};

/// Canonical channel names used in [`ValidationResult::channel`].
pub mod channels {
    /// Test plan — the canonical fixture every replay channel consumes.
    pub const PLAN: &str = "plan";
    /// Rust wrapper distribution channel (`empyrean::Context::propagate`).
    pub const RUST: &str = "rust";
    /// Python wheel distribution channel (`empyrean.propagate` via PyO3).
    pub const PYTHON: &str = "python";
    /// C ABI distribution channel (`libempyrean.dylib` via the C runner).
    pub const C: &str = "c";
    /// CLI binary distribution channel (`empyrean-cli-runner` per row).
    pub const CLI: &str = "cli";
    /// empyrean-core direct (no FFI / no wrapper).
    pub const CORE: &str = "core";
    /// ASSIST external propagator reference.
    pub const ASSIST: &str = "assist";
    /// find_orb external OD reference.
    pub const FINDORB: &str = "findorb";
    /// kete external NEO toolkit reference.
    pub const KETE: &str = "kete";
}

/// Canonical [`ValidationResult::test_type`] values.
pub mod test_types {
    /// Propagate IC to a target epoch; compare position to Horizons.
    pub const PROPAGATION: &str = "propagation";
    /// Generate an ephemeris at a target epoch from an observer; compare
    /// RA/Dec/range/light-time to Horizons.
    pub const EPHEMERIS: &str = "ephemeris";
    /// Run differential correction on a PSV fixture; compare fitted state
    /// + chi² + post-fit RMS across channels.
    pub const ORBIT_DETERMINATION: &str = "orbit_determination";
}

/// Canonical [`ValidationResult::propagation_uncertainty`] values.
pub mod uncertainty_modes {
    /// Input orbit carries a covariance, so the propagator dispatches to
    /// Jet1 / STM integration. The production hot path because empyrean
    /// is uncertainty-first by design.
    pub const FIRST_ORDER_WITH_COV: &str = "first_order_with_cov";
    /// Covariance stripped; pure f64 state-only propagation. Used to
    /// measure Jet1 overhead and to compare against external propagators
    /// that don't carry uncertainty.
    pub const F64_NO_COV: &str = "f64_no_cov";
}

/// One row in the validation result table.
///
/// Every channel runner emits a `Vec<ValidationResult>` as JSON. The plan
/// generator emits the same shape with `channel = "plan"` and `emp_*` /
/// channel-specific fields set to `None` — see [`channels::PLAN`].
///
/// The schema is contract-tight: `#[serde(deny_unknown_fields)]` rejects
/// JSON files that carry fields this crate version does not know about.
/// When you add a field, bump this crate's minor version and update every
/// consumer in the same PR cycle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ValidationResult {
    // ── Test identity ───────────────────────────────────────────────
    /// Object name. Matches [`crate::catalog`] entries.
    pub object: String,
    /// Population tag (`NEO`, `Comet`, `Self-Perturber`, …) used by the
    /// report's per-population grouping.
    pub population: String,
    /// IC epoch (MJD TDB) the channel propagated from.
    pub epoch_mjd_tdb: f64,
    /// Days from `epoch_mjd_tdb` to `t_mjd_tdb`. Negative for past tests.
    pub dt_days: f64,
    /// Target epoch the channel propagated / generated ephemeris at, or
    /// the OD fit's output epoch.
    pub t_mjd_tdb: f64,
    /// Force-model tier name (e.g., `"approximate"`, `"basic"`,
    /// `"standard"`). Lowercase string matching the upstream enum's serde.
    pub force_model: String,
    /// One of [`test_types::PROPAGATION`], [`test_types::EPHEMERIS`],
    /// [`test_types::ORBIT_DETERMINATION`].
    pub test_type: String,
    /// One of the [`channels`] string constants.
    pub channel: String,
    /// MPC observer code for ephemeris rows; `None` for propagation / OD.
    pub observer: Option<String>,

    // ── Empyrean output ─────────────────────────────────────────────
    /// Position diff vs Horizons reference (km). Propagation rows.
    pub emp_vs_horizons_km: Option<f64>,
    /// Empyrean output Cartesian position (AU, ICRF, SSB-centered).
    /// Propagation + OD rows.
    pub emp_pos_au: Option<[f64; 3]>,
    /// Wall-clock per row (ms). Best-of `n_timing_runs`.
    pub emp_time_ms: Option<f64>,
    /// Angular separation vs Horizons (arcsec). Ephemeris rows.
    pub separation_arcsec: Option<f64>,
    /// dRA·cos(δ) vs Horizons (arcsec). Ephemeris rows.
    pub d_ra_arcsec: Option<f64>,
    /// dDec vs Horizons (arcsec). Ephemeris rows.
    pub d_dec_arcsec: Option<f64>,
    /// Range diff vs Horizons (km). Ephemeris rows.
    pub d_rho_km: Option<f64>,
    /// Light-time diff vs Horizons (s). Ephemeris rows.
    pub d_light_time_s: Option<f64>,

    // ── Initial conditions ──────────────────────────────────────────
    /// Initial-condition Cartesian state (SSB ICRF) so non-network
    /// channels (python, c, cli) can reproduce this row without
    /// re-hitting Horizons.
    pub ic_pos_au: Option<[f64; 3]>,
    /// IC velocity (AU/day, ICRF, SSB-centered).
    pub ic_vel_au_d: Option<[f64; 3]>,
    /// Marsden A1 non-grav coefficient (or 0 if not provided / asteroid).
    pub ic_a1: Option<f64>,
    /// Marsden A2 non-grav coefficient.
    pub ic_a2: Option<f64>,
    /// Marsden A3 non-grav coefficient.
    pub ic_a3: Option<f64>,
    /// Marsden g(r) parameters from SBDB (or 0 if not provided / asteroid).
    pub ic_g_alpha: Option<f64>,
    /// g(r) reference distance r0 (AU).
    pub ic_g_r0: Option<f64>,
    /// g(r) m exponent.
    pub ic_g_m: Option<f64>,
    /// g(r) n exponent.
    pub ic_g_n: Option<f64>,
    /// g(r) k exponent.
    pub ic_g_k: Option<f64>,
    /// SBDB non-grav time-delay (days). `None` for asteroids and
    /// short-period comets without a fitted delay; populated for
    /// Jupiter-family comets (67P = +45.689 d) and 2I/Borisov (-65.130 d).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ic_non_grav_dt: Option<f64>,

    // ── Reference (Horizons) values at the target epoch ─────────────
    /// Horizons truth Cartesian position (AU, ICRF, SSB-centered).
    pub ref_pos_au: Option<[f64; 3]>,
    /// Horizons truth velocity (AU/day).
    pub ref_vel_au_d: Option<[f64; 3]>,
    /// Horizons truth RA (rad).
    pub ref_ra_rad: Option<f64>,
    /// Horizons truth Dec (rad).
    pub ref_dec_rad: Option<f64>,
    /// Horizons truth observer-target range (AU).
    pub ref_rho_au: Option<f64>,
    /// Horizons truth one-way light time (days).
    pub ref_light_time_d: Option<f64>,

    // ── OD-specific output ──────────────────────────────────────────
    /// Observation count after rejection. Populated when
    /// `test_type == "orbit_determination"`.
    pub n_obs_used: Option<u32>,
    /// DC iteration count to convergence.
    pub od_iterations: Option<u32>,
    /// Whether the differential corrector reached strict convergence.
    pub od_converged: Option<bool>,
    /// Post-fit RMS of RA residuals (arcsec).
    pub od_rms_ra_arcsec: Option<f64>,
    /// Post-fit RMS of Dec residuals (arcsec).
    pub od_rms_dec_arcsec: Option<f64>,
    /// Combined RA·cosδ + Dec residual RMS (arcsec). Matches the
    /// find_orb / OrbFit `rms` reporting convention — directly
    /// comparable to `findorb_rms_residual`.
    pub od_rms_combined_arcsec: Option<f64>,
    /// Post-fit chi-squared.
    pub od_chi2: Option<f64>,
    /// Reduced chi-squared (`chi2 / (N - k)`).
    pub od_reduced_chi2: Option<f64>,
    /// NAIF IDs of perturbers excluded from the force model during this
    /// OD fit. Populated for SB441-N16 self-perturbers (so the body's
    /// own gravity does not act on itself during integration). Empty
    /// for all other rows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded_perturbers_naif: Vec<i32>,

    // ── Test-configuration axis ─────────────────────────────────────
    /// Tag distinguishing prop+eph rows by uncertainty-propagation mode.
    /// One of [`uncertainty_modes::FIRST_ORDER_WITH_COV`] or
    /// [`uncertainty_modes::F64_NO_COV`]. `None` on OD rows because OD
    /// always produces a post-fit covariance — the axis doesn't apply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub propagation_uncertainty: Option<String>,

    // ── External-reference comparisons ──────────────────────────────
    /// |ASSIST − Horizons| in km. ASSIST is an independent N-body
    /// propagator (Holman et al. 2023, REBOUND + IAS15 + DE441 +
    /// SB441-N16). Populated by `merge-external` from the ASSIST
    /// runner's output.
    pub assist_vs_horizons_km: Option<f64>,
    /// |empyrean − ASSIST| in km.
    pub emp_vs_assist_km: Option<f64>,
    /// ASSIST wall-clock per row (ms).
    pub assist_time_ms: Option<f64>,
    /// `empyrean_time_ms / assist_time_ms`.
    pub speed_ratio: Option<f64>,
    /// find_orb post-fit residual RMS (arcsec). find_orb is an
    /// independent OD package (Bill Gray / Project Pluto).
    pub findorb_rms_residual: Option<f64>,
    /// find_orb observation count after rejection.
    pub findorb_n_obs_used: Option<u32>,
    /// find_orb observation count rejected.
    pub findorb_n_obs_rejected: Option<u32>,

    // ── Metadata ────────────────────────────────────────────────────
    /// ISO 8601 timestamp at row creation.
    pub timestamp: String,
    /// Free-form notes (e.g., known close approaches, IOD pathologies).
    pub notes: String,
}

impl ValidationResult {
    /// Construct a row with all `Option` fields set to `None` and
    /// `Vec` fields empty. Convenient for tests; production code should
    /// use the channel-specific runner builders that fill in test
    /// identity, IC, ref values, etc.
    pub fn empty() -> Self {
        Self {
            object: String::new(),
            population: String::new(),
            epoch_mjd_tdb: 0.0,
            dt_days: 0.0,
            t_mjd_tdb: 0.0,
            force_model: String::new(),
            test_type: String::new(),
            channel: String::new(),
            observer: None,
            emp_vs_horizons_km: None,
            emp_pos_au: None,
            emp_time_ms: None,
            separation_arcsec: None,
            d_ra_arcsec: None,
            d_dec_arcsec: None,
            d_rho_km: None,
            d_light_time_s: None,
            ic_pos_au: None,
            ic_vel_au_d: None,
            ic_a1: None,
            ic_a2: None,
            ic_a3: None,
            ic_g_alpha: None,
            ic_g_r0: None,
            ic_g_m: None,
            ic_g_n: None,
            ic_g_k: None,
            ic_non_grav_dt: None,
            ref_pos_au: None,
            ref_vel_au_d: None,
            ref_ra_rad: None,
            ref_dec_rad: None,
            ref_rho_au: None,
            ref_light_time_d: None,
            n_obs_used: None,
            od_iterations: None,
            od_converged: None,
            od_rms_ra_arcsec: None,
            od_rms_dec_arcsec: None,
            od_rms_combined_arcsec: None,
            od_chi2: None,
            od_reduced_chi2: None,
            excluded_perturbers_naif: Vec::new(),
            propagation_uncertainty: None,
            assist_vs_horizons_km: None,
            emp_vs_assist_km: None,
            assist_time_ms: None,
            speed_ratio: None,
            findorb_rms_residual: None,
            findorb_n_obs_used: None,
            findorb_n_obs_rejected: None,
            timestamp: String::new(),
            notes: String::new(),
        }
    }
}

/// A canonical test plan — `Vec<ValidationResult>` with `channel = "plan"`
/// on every row and channel-specific output fields nulled.
///
/// This is a type alias rather than a wrapper struct because the wire
/// shape is just a `Vec<ValidationResult>` — there is no plan-specific
/// metadata that could not be carried on the rows themselves. The
/// alias gives the consumer a way to spell "I want the plan" without
/// committing to a wrapper.
pub type ValidationPlan = Vec<ValidationResult>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_round_trips_through_json() {
        let r = ValidationResult::empty();
        let s = serde_json::to_string(&r).unwrap();
        let r2: ValidationResult = serde_json::from_str(&s).unwrap();
        assert_eq!(r, r2);
    }

    #[test]
    fn skip_serializing_options_omits_nulls_for_added_fields() {
        // The three fields tagged with skip_serializing_if must NOT
        // appear in the JSON when they are at their default value, so
        // older JSON files (written before the field existed) round-
        // trip cleanly through this type without the parser objecting
        // to "missing" keys. `deny_unknown_fields` would otherwise
        // require the writer to omit the key.
        let r = ValidationResult::empty();
        let s = serde_json::to_string(&r).unwrap();
        assert!(
            !s.contains("ic_non_grav_dt"),
            "ic_non_grav_dt should be omitted"
        );
        assert!(
            !s.contains("propagation_uncertainty"),
            "propagation_uncertainty should be omitted",
        );
        assert!(
            !s.contains("excluded_perturbers_naif"),
            "excluded_perturbers_naif should be omitted",
        );
    }

    #[test]
    fn unknown_field_is_rejected() {
        // Trip-wire: a JSON file with an extra field must error rather
        // than silently dropping it. This is what catches schema drift
        // between this crate and a consumer.
        let s = r#"{
            "object": "Apophis", "population": "NEO",
            "epoch_mjd_tdb": 0.0, "dt_days": 0.0, "t_mjd_tdb": 0.0,
            "force_model": "standard", "test_type": "propagation",
            "channel": "rust", "observer": null,
            "emp_vs_horizons_km": null, "emp_pos_au": null, "emp_time_ms": null,
            "separation_arcsec": null, "d_ra_arcsec": null, "d_dec_arcsec": null,
            "d_rho_km": null, "d_light_time_s": null,
            "ic_pos_au": null, "ic_vel_au_d": null,
            "ic_a1": null, "ic_a2": null, "ic_a3": null,
            "ic_g_alpha": null, "ic_g_r0": null, "ic_g_m": null,
            "ic_g_n": null, "ic_g_k": null,
            "ref_pos_au": null, "ref_vel_au_d": null,
            "ref_ra_rad": null, "ref_dec_rad": null,
            "ref_rho_au": null, "ref_light_time_d": null,
            "n_obs_used": null, "od_iterations": null, "od_converged": null,
            "od_rms_ra_arcsec": null, "od_rms_dec_arcsec": null,
            "od_chi2": null, "od_reduced_chi2": null,
            "assist_vs_horizons_km": null, "emp_vs_assist_km": null,
            "assist_time_ms": null, "speed_ratio": null,
            "findorb_rms_residual": null, "findorb_n_obs_used": null,
            "findorb_n_obs_rejected": null,
            "timestamp": "", "notes": "",
            "frobnicate": 42
        }"#;
        let result: Result<ValidationResult, _> = serde_json::from_str(s);
        assert!(result.is_err(), "unknown field should be rejected");
    }

    #[test]
    fn channel_constants_match_existing_jsons() {
        // Cross-check the constants against the values written by the
        // existing channel runners. If anyone renames a channel, this
        // test wedges the rename until every consumer is updated.
        assert_eq!(channels::RUST, "rust");
        assert_eq!(channels::PYTHON, "python");
        assert_eq!(channels::C, "c");
        assert_eq!(channels::CLI, "cli");
        assert_eq!(channels::CORE, "core");
        assert_eq!(channels::ASSIST, "assist");
        assert_eq!(channels::FINDORB, "findorb");
        assert_eq!(channels::KETE, "kete");
        assert_eq!(channels::PLAN, "plan");
    }
}
