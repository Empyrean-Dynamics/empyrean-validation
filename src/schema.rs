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

/// Serde adapter for `f64` fields that may be NaN. `serde_json` writes
/// non-finite floats as JSON `null` (NaN/Infinity aren't valid JSON
/// numbers) but the default deserializer rejects `null` → `f64`. This
/// adapter round-trips: NaN ↔ null, finite ↔ number.
pub mod f64_nan_null {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
        if v.is_finite() {
            s.serialize_f64(*v)
        } else {
            s.serialize_none()
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
        let opt: Option<f64> = Deserialize::deserialize(d)?;
        Ok(opt.unwrap_or(f64::NAN))
    }
}

/// Same as [`f64_nan_null`] but for `[f64; 6]` arrays — each element
/// round-trips NaN ↔ null.
pub mod f64_array6_nan_null {
    use serde::{Deserialize, Deserializer, Serializer, ser::SerializeSeq};

    pub fn serialize<S: Serializer>(v: &[f64; 6], s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(6))?;
        for x in v.iter() {
            if x.is_finite() {
                seq.serialize_element(x)?;
            } else {
                seq.serialize_element(&Option::<f64>::None)?;
            }
        }
        seq.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[f64; 6], D::Error> {
        let v: Vec<Option<f64>> = Deserialize::deserialize(d)?;
        if v.len() != 6 {
            return Err(serde::de::Error::invalid_length(v.len(), &"expected 6"));
        }
        let mut out = [0.0_f64; 6];
        for (i, o) in v.into_iter().enumerate() {
            out[i] = o.unwrap_or(f64::NAN);
        }
        Ok(out)
    }
}

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
    /// Run differential correction on an optical+radar PSV fixture (the ADES
    /// `<radar>` delay/Doppler table folded into the fit) for objects that
    /// have radar astrometry. Emitted as a second OD row alongside the
    /// optical-only [`ORBIT_DETERMINATION`] row so the radar-tightened orbit
    /// is cross-checked against find_orb the same way.
    pub const ORBIT_DETERMINATION_RADAR: &str = "orbit_determination_radar";
    /// Run differential correction with `solve_for = StateAndNonGrav` on an
    /// object with a known SBDB non-gravitational signal (`ic_a2 != 0` — the
    /// Yarkovsky NEOs and the comets) and compare the **fitted** Marsden
    /// A1/A2/A3 (`od_a1/od_a2/od_a3` ± `od_a*_sigma`) to the JPL SBDB
    /// reference (`ic_a1/ic_a2/ic_a3`). Emitted as a second OD row alongside
    /// the optical-only [`ORBIT_DETERMINATION`] row. Guards non-grav
    /// *recovery* — distinct from [`ORBIT_DETERMINATION`], which only checks
    /// fitted state + χ² + RMS and so cannot see a silent drop to a
    /// gravity-only fit.
    pub const NON_GRAV_RECOVERY: &str = "non_grav_recovery";
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

    // ── Non-grav recovery output ────────────────────────────────────
    // Populated only on `non_grav_recovery` rows: the fitted Marsden
    // coefficients from a `StateAndNonGrav` fit, each with its 1σ from the
    // diagonal of the fitted 9×9 covariance. `None` (not 0, not NaN) when
    // the fit did not actually solve non-grav (e.g. a silent fall-back to a
    // 6-param state-only fit, signalled by `has_covariance_9x9 == 0`) — so a
    // missing σ reads loudly as "non-grav not recovered" in the report.
    /// Fitted Marsden A1 (radial), AU/day².
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub od_a1: Option<f64>,
    /// Fitted Marsden A2 (transverse ≈ Yarkovsky), AU/day².
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub od_a2: Option<f64>,
    /// Fitted Marsden A3 (normal), AU/day².
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub od_a3: Option<f64>,
    /// 1σ on the fitted A1, √(C₉ₓ₉[6][6]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub od_a1_sigma: Option<f64>,
    /// 1σ on the fitted A2, √(C₉ₓ₉[7][7]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub od_a2_sigma: Option<f64>,
    /// 1σ on the fitted A3, √(C₉ₓ₉[8][8]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub od_a3_sigma: Option<f64>,
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

    // ── OpenOrb (oorb) external reference ───────────────────────────
    // Propagation + ephemeris reference. Independent Fortran
    // implementation (Granvik et al. — University of Helsinki).
    // Populated by `merge-external` from the OpenOrb runner's output.
    /// |OpenOrb − Horizons| in km (propagation rows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oorb_vs_horizons_km: Option<f64>,
    /// |empyrean − OpenOrb| in km (propagation rows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emp_vs_oorb_km: Option<f64>,
    /// OpenOrb wall-clock per row (ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oorb_time_ms: Option<f64>,
    /// OpenOrb angular separation vs Horizons (ephemeris rows, arcsec).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oorb_separation_arcsec: Option<f64>,
    /// OpenOrb dRA·cos(Dec) vs Horizons (ephemeris rows, arcsec).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oorb_d_ra_arcsec: Option<f64>,
    /// OpenOrb dDec vs Horizons (ephemeris rows, arcsec).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oorb_d_dec_arcsec: Option<f64>,
    /// OpenOrb d|range| vs Horizons (ephemeris rows, km).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oorb_d_rho_km: Option<f64>,

    // ── OrbFit external reference ───────────────────────────────────
    // OD reference. Canonical implementation of CMC2003 χ²-with-
    // hysteresis rejection (OrbFit Consortium, University of Pisa /
    // IAU Minor Planet Center; Federica Spoto et al.). Populated by
    // `merge-external` from the OrbFit runner's output.
    /// OrbFit post-fit weighted RMS (arcsec). Read from the `.rwo`
    /// header's `RMSast` field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orbfit_rms_arcsec: Option<f64>,
    /// OrbFit observation count after rejection (SEL=1 in `.rwo`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orbfit_n_obs_used: Option<u32>,
    /// OrbFit observation count rejected (SEL=0 in `.rwo`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orbfit_n_obs_rejected: Option<u32>,
    /// OrbFit wall-clock per row (ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orbfit_time_ms: Option<f64>,

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
            od_a1: None,
            od_a2: None,
            od_a3: None,
            od_a1_sigma: None,
            od_a2_sigma: None,
            od_a3_sigma: None,
            excluded_perturbers_naif: Vec::new(),
            propagation_uncertainty: None,
            assist_vs_horizons_km: None,
            emp_vs_assist_km: None,
            assist_time_ms: None,
            speed_ratio: None,
            findorb_rms_residual: None,
            findorb_n_obs_used: None,
            findorb_n_obs_rejected: None,
            oorb_vs_horizons_km: None,
            emp_vs_oorb_km: None,
            oorb_time_ms: None,
            oorb_separation_arcsec: None,
            oorb_d_ra_arcsec: None,
            oorb_d_dec_arcsec: None,
            oorb_d_rho_km: None,
            orbfit_rms_arcsec: None,
            orbfit_n_obs_used: None,
            orbfit_n_obs_rejected: None,
            orbfit_time_ms: None,
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

/// Canonical [`CapturedOrbit::source`] values.
pub mod orbit_sources {
    /// Orbit fitted by scott (this crate's OD runner).
    pub const SCOTT_OD: &str = "scott_od";
    /// Orbit fitted by find_orb (Bill Gray / Project Pluto).
    pub const FINDORB: &str = "findorb";
    /// Orbit published by JPL Small-Body Database.
    pub const SBDB: &str = "sbdb";
}

/// Per-object orbit + 6×6 covariance snapshot, emitted as a sidecar
/// to [`ValidationResult`] for the orbit-comparison panel.
///
/// Each tool that produces a fitted (or published) orbit + covariance
/// — scott OD, find_orb, JPL SBDB — emits one [`CapturedOrbit`] record
/// per object. The orbit-comparison kernel consumes these to compare
/// scott's fit to find_orb and to SBDB in Keplerian element space at a
/// common epoch.
///
/// Each record carries the orbit in three coordinate views:
/// 1. **Native** — the representation the tool produced (Cartesian for
///    scott + find_orb; Cometary for SBDB).
/// 2. **Sun-centered ICRF Cartesian** — the common interchange basis,
///    used by the propagation step when transporting an orbit from one
///    tool's native epoch to another.
/// 3. **Sun-centered ecliptic-J2000 Keplerian** — the comparison basis;
///    Mahalanobis distance and σ-ratios are computed here.
///
/// All three views share the same epoch (`epoch_mjd_tdb`); the
/// covariance is transformed through the Jacobian via
/// `empyrean::Context::transform`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CapturedOrbit {
    /// Object name (matches [`crate::catalog::ValidationObject::name`]).
    pub object: String,
    /// Source tool. One of [`orbit_sources::SCOTT_OD`],
    /// [`orbit_sources::FINDORB`], [`orbit_sources::SBDB`].
    pub source: String,
    /// Optional source-tool version tag (e.g., scott crate version,
    /// find_orb build date, SBDB orbit solution name like "JPL#256").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_version: Option<String>,
    /// Epoch shared by all three coordinate views (MJD TDB).
    pub epoch_mjd_tdb: f64,
    /// Native representation: `"cartesian"`, `"cometary"`, or `"keplerian"`.
    pub native_repr: String,
    /// Native frame: `"icrf"` or `"ecliptic_j2000"`.
    pub native_frame: String,
    /// Native origin NAIF ID (10 = Sun, 0 = SSB).
    pub native_origin_naif: i32,
    /// Native state vector (6 elements). Element order matches the
    /// representation: Cartesian = `[x, y, z, vx, vy, vz]` (AU, AU/day);
    /// Cometary = `[q, e, i, Ω, ω, T_p]` (AU, —, deg, deg, deg, MJD TDB);
    /// Keplerian = `[a, e, i, Ω, ω, M]` (AU, —, deg, deg, deg, deg).
    pub native_state: [f64; 6],
    /// Native 6×6 covariance (units paired with `native_state`).
    /// `None` if the source did not provide one.
    pub native_cov_6x6: Option<[[f64; 6]; 6]>,
    /// State in Sun-centered ICRF Cartesian (AU, AU/day).
    pub state_cart_icrf_sun: [f64; 6],
    /// 6×6 covariance in Sun-centered ICRF Cartesian. `None` if the
    /// native covariance was absent.
    pub cov_cart_icrf_sun_6x6: Option<[[f64; 6]; 6]>,
    /// State in Sun-centered ecliptic-J2000 Keplerian
    /// (a [AU], e, i [deg], Ω [deg], ω [deg], M [deg]).
    pub state_kep_ecliptic_sun: [f64; 6],
    /// 6×6 covariance in Sun-centered ecliptic-J2000 Keplerian
    /// (units paired with `state_kep_ecliptic_sun`).
    pub cov_kep_ecliptic_sun_6x6: Option<[[f64; 6]; 6]>,
}

impl CapturedOrbit {
    /// Construct with default-zero fields. Tests / partial fixtures
    /// should fill specific fields after construction.
    pub fn empty(object: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            object: object.into(),
            source: source.into(),
            source_version: None,
            epoch_mjd_tdb: 0.0,
            native_repr: String::new(),
            native_frame: String::new(),
            native_origin_naif: 0,
            native_state: [0.0; 6],
            native_cov_6x6: None,
            state_cart_icrf_sun: [0.0; 6],
            cov_cart_icrf_sun_6x6: None,
            state_kep_ecliptic_sun: [0.0; 6],
            cov_kep_ecliptic_sun_6x6: None,
        }
    }
}

/// Per-object, per-reference orbit comparison. Emitted by the
/// orbit-comparison kernel; consumed by the report panel.
///
/// Each row compares scott's fitted orbit + covariance to one
/// reference (SBDB or find_orb) in Keplerian element space at one
/// common epoch. The same (object, reference) pair may produce
/// multiple rows: one per common-epoch choice (scott's fit epoch,
/// reference's native epoch).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OrbitComparison {
    /// Object name.
    pub object: String,
    /// Reference tool. One of [`orbit_sources::SBDB`],
    /// [`orbit_sources::FINDORB`].
    pub reference: String,
    /// Common epoch all states are propagated to (MJD TDB).
    pub common_epoch_mjd_tdb: f64,
    /// Whose native epoch is the common epoch: `"scott"`, `"sbdb"`,
    /// or `"findorb"`.
    pub common_epoch_source: String,
    /// Comparison representation (currently always `"keplerian"`).
    pub repr: String,
    /// Scott's state at common epoch (Keplerian).
    pub state_scott: [f64; 6],
    /// Reference's state at common epoch (Keplerian).
    pub state_ref: [f64; 6],
    /// `state_scott - state_ref` (element-wise; angle elements
    /// wrapped to `(-180°, 180°]`).
    pub delta: [f64; 6],
    /// Per-element 1σ from scott's covariance diagonal. Per-element
    /// NaNs (when the source has no covariance) round-trip as JSON
    /// `null`.
    #[serde(with = "f64_array6_nan_null")]
    pub sigma_scott: [f64; 6],
    /// Per-element 1σ from reference's covariance diagonal.
    #[serde(with = "f64_array6_nan_null")]
    pub sigma_ref: [f64; 6],
    /// `Δᵀ Σ_scott⁻¹ Δ` — is the reference inside scott's ellipsoid?
    /// NaN (round-trip as `null`) when Σ_scott is not SPD or missing.
    #[serde(with = "f64_nan_null")]
    pub mahalanobis_d2_scott_metric: f64,
    /// `Δᵀ Σ_ref⁻¹ Δ` — is scott inside the reference's ellipsoid?
    #[serde(with = "f64_nan_null")]
    pub mahalanobis_d2_ref_metric: f64,
    /// `Δᵀ (Σ_scott + Σ_ref)⁻¹ Δ` — symmetric consistency metric.
    #[serde(with = "f64_nan_null")]
    pub mahalanobis_d2_combined_metric: f64,
    /// `Σ_k (Δ_k / σ_combined,k)²` — sum of squared marginal z-scores.
    /// Compare to [`Self::mahalanobis_d2_combined_metric`]: when the
    /// joint d² ≫ marginal d², off-diagonal correlation in the joint
    /// covariance (not any single marginal) is driving the
    /// discrepancy — the "correlation" pathology signature. When
    /// joint d² ≈ marginal d², the discrepancy is element-by-element.
    #[serde(with = "f64_nan_null")]
    pub mahalanobis_d2_marginal: f64,
    /// `√(d²_combined / 6)` — 6-DOF χ-equivalent sigma.
    #[serde(with = "f64_nan_null")]
    pub sigma_equiv_combined: f64,
    /// Sorted-descending eigenvalues of `Σ_scott` (Keplerian
    /// element-space). Same units as `sigma_scott²`.
    #[serde(with = "f64_array6_nan_null")]
    pub eigenvalues_scott: [f64; 6],
    /// Sorted-descending eigenvalues of `Σ_ref`.
    #[serde(with = "f64_array6_nan_null")]
    pub eigenvalues_ref: [f64; 6],
    /// Principal-axis rotation between `Σ_scott` and `Σ_ref` in
    /// degrees: `arccos(|v₁_scott · v₁_ref|)` where v₁ is the
    /// eigenvector of the largest eigenvalue. Small angle means
    /// ellipsoids are aligned; a large angle with similar eigenvalue
    /// spectra is the "rotation" pathology signature.
    #[serde(with = "f64_nan_null")]
    pub principal_axis_rotation_deg: f64,
    /// `det(Σ_scott) / det(Σ_ref)` — overall ellipsoid volume ratio.
    /// Retained for backward compatibility; the eigenvalue spectra
    /// above are more interpretable.
    #[serde(with = "f64_nan_null")]
    pub cov_volume_ratio: f64,
    /// Free-form notes (e.g., propagation method, epoch handling).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

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
            "od_rms_combined_arcsec": null,
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
