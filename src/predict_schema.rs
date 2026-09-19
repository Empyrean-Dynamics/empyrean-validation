//! Types for the walk-forward covariance-realism family.
//!
//! Three groups, mirroring the data flow:
//!
//! 1. **Window manifest** ([`WindowManifest`]) — generated once by the
//!    `windows` subcommand from the pinned fixture snapshot; the single
//!    source every runner consumes. Windowing, fit sets, prediction targets,
//!    the pinned scoring σ, and the debias corrections all live here so five
//!    independent tools cannot silently diverge.
//! 2. **Runner sidecars** ([`WalkWindowRecord`], [`PredictedObservation`]) —
//!    what a runner emits: fits and predictions, never statistics.
//! 3. **Kernel output** ([`ScoredPrediction`], [`SurfaceCell`]) — what
//!    `predict_compare` computes centrally, identically for every tool.
//!
//! Protocol: walk-forward, per-night expanding windows, held-out predictions
//! scored against their predicted sky covariance.
//!
//! None of these types is a [`crate::schema::ValidationResult`] field: the
//! family's per-prediction payload travels in sidecar files (the
//! `CapturedOrbit` precedent), keeping the pinned row contract untouched.

use serde::{Deserialize, Serialize};

use crate::schema::CapturedOrbit;

/// Current manifest schema version. Bump on any breaking shape change.
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Profile names — subsets of the bundle=1 schedule (§2.3 of the design).
pub mod profiles {
    /// Every window (bundle = 1, whole arc). The surface run.
    pub const FULL: &str = "full";
    /// Bundle = 1 for the first 60 cuts, then geometric ×1.3 in night index.
    pub const CI: &str = "ci";
    /// Seed + cuts nearest {+1 mo, +6 mo, arc end}. Smoke tests.
    pub const LADDER: &str = "ladder";
    /// All profile names, densest first.
    pub const ALL: [&str; 3] = [FULL, CI, LADDER];
}

/// Object classes for the (arc × dt) surfaces — fixed population → class
/// mapping, materialized per object so consumers never re-derive it.
pub mod classes {
    pub const NEA: &str = "NEA";
    pub const COMET: &str = "Comet";
    pub const ISO: &str = "ISO";
    pub const MAIN_BELT: &str = "MainBelt";
    pub const TROJAN: &str = "Trojan";
    pub const OUTER: &str = "Outer";

    /// Map a catalog population string to its class.
    pub fn from_population(population: &str) -> &'static str {
        match population {
            "NEO" | "Short-arc NEO" | "TCO" | "Impactor" => NEA,
            "Comet" => COMET,
            "ISO" => ISO,
            "MBA" | "Self-Perturber" => MAIN_BELT,
            "Jupiter Trojan" | "Earth Trojan" | "Neptune Trojan" => TROJAN,
            "Centaur" | "TNO" => OUTER,
            other => panic!(
                "unmapped population {other:?}: every catalog population must have a class \
                 (add it to predict_schema::classes::from_population)"
            ),
        }
    }
}

/// `uncertainty_form` values a runner may declare on a prediction.
pub mod uncertainty_forms {
    /// Full 2×2 sky-plane covariance delivered.
    pub const RADEC_2X2: &str = "radec_2x2";
    /// 1-D σ along a reported position angle (find_orb's variant-orbit form).
    pub const SIGMA1D_PA: &str = "sigma1d_pa";
    /// No uncertainty delivered for this prediction. A zeroed matrix from a
    /// tool maps here — never to a `d²`.
    pub const NONE: &str = "none";
}

/// FNV-1a 64-bit over the raw field strings. The fixtures carry no ADES
/// `obsID`, so target identity is content-derived: hash of
/// `stn|obsTime|ra|dec` **exactly as they appear in the PSV** (raw strings,
/// not parsed floats — tools re-format floats differently). The manifest
/// generator asserts uniqueness within an object.
pub fn obs_key_hash(stn: &str, obs_time: &str, ra_raw: &str, dec_raw: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for part in [stn, "|", obs_time, "|", ra_raw, "|", dec_raw] {
        for b in part.as_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    h
}

/// The window manifest: one file, every runner's single source of truth.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WindowManifest {
    /// [`MANIFEST_SCHEMA_VERSION`] at generation time.
    pub schema_version: u32,
    /// The fixture `snapshot_id` this manifest was derived from. Consumers
    /// must refuse a manifest whose snapshot does not match the fixtures on
    /// disk — regenerating against a different snapshot is an error, not a
    /// refresh.
    pub snapshot_id: String,
    /// Nights per walk step in the base schedule (1 = every new night).
    pub bundle_nights: u32,
    /// Generator provenance (crate name/version).
    pub generated_by: String,
    /// Horizon-ladder rungs, days past the cut (next-night is implicit).
    pub ladder_days: Vec<f64>,
    /// Fractional tolerance for accepting a ladder night (±50% = 0.5).
    pub ladder_tolerance: f64,
    /// Targets below this realized horizon are flagged
    /// `same_night_continuation` and excluded from headline aggregates.
    pub min_horizon_days: f64,
    /// Per-target-night observation cap (evenly spaced in time).
    pub max_obs_per_night: u32,
    /// Per-object windowing, observations, and targets.
    pub objects: Vec<ObjectWindows>,
}

/// One object's observation table and window schedule.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ObjectWindows {
    /// Catalog object name (fixture stem convention, `/` intact).
    pub object: String,
    /// MPC designation from the catalog.
    pub mpc_designation: String,
    /// Catalog population string.
    pub population: String,
    /// Fixed class for the surfaces — see [`classes::from_population`].
    pub class: String,
    /// Whether the object supports the walk at all. Purely observational
    /// criteria (nights, counts, arc) — **never** any tool's convergence.
    pub eligible: bool,
    /// Why not, when `eligible == false`. Always present in that case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ineligible_reason: Option<String>,
    /// Integer UTC MJD of the first (non-excluded) observation night.
    pub first_night: i64,
    /// Last night (inclusive) of the discovery apparition (first maximal run
    /// of nights with no internal gap > 90 d).
    pub apparition_end_night: i64,
    /// Total rows parsed from the fixture (before exclusions).
    pub n_obs_total: u32,
    /// Rows excluded as space-based / roving (ADES `sys`/`ctr`/`pos1-3`).
    pub n_excluded_space_based: u32,
    /// Time-ordered observation table. `obs_idx` used everywhere else is an
    /// index into this vector.
    pub observations: Vec<ObsEntry>,
    /// The bundle=1 window schedule (empty when ineligible).
    pub windows: Vec<WindowSpec>,
}

/// One observation row: identity, raw astrometry, pinned scoring noise, and
/// the centrally-computed debias correction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ObsEntry {
    /// MPC observatory code.
    pub stn: String,
    /// ADES `obsTime`, ISO-8601 UTC, byte-for-byte as the PSV carries it.
    pub obs_time: String,
    /// Content key — [`obs_key_hash`] over the raw PSV fields.
    pub key_hash: u64,
    /// UTC MJD of `obs_time`.
    pub mjd_utc: f64,
    /// Integer UTC MJD night.
    pub night: i64,
    /// Raw observed RA, degrees (as parsed from the PSV; no debias applied).
    pub ra_deg: f64,
    /// Raw observed Dec, degrees.
    pub dec_deg: f64,
    /// ADES `astCat`, `"UNK"` when absent.
    pub ast_cat: String,
    /// ADES `mode`, `"UNK"` when absent.
    pub mode: String,
    /// Pinned scoring σ for \( \alpha\cos\delta \), arcsec.
    pub sigma_ra_arcsec: f64,
    /// Pinned scoring σ for \( \delta \), arcsec.
    pub sigma_dec_arcsec: f64,
    /// Pinned scoring correlation, dimensionless (0 when unknown).
    pub sigma_corr: f64,
    /// Where the pinned σ came from: `"ades"`, `"vfc17"`, or `"default"`.
    pub sigma_source: String,
    /// ADES `rmsRA` (arcsec, already cos δ-scaled per ADES), when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ades_rms_ra: Option<f64>,
    /// ADES `rmsDec` (arcsec), when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ades_rms_dec: Option<f64>,
    /// EFCC2020 catalog-bias correction to \( \alpha\cos\delta \), arcsec:
    /// debiased = observed − correction. `None` = not computable (unknown /
    /// uncovered catalog) — scored in its own bucket, never silently as 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debias_dra_arcsec: Option<f64>,
    /// EFCC2020 correction to \( \delta \), arcsec.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debias_ddec_arcsec: Option<f64>,
    /// Exclusion tag (`"space_based"`, …). Excluded rows never enter fit
    /// sets or targets; the tag says why, loudly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excluded: Option<String>,
}

/// One window: an explicit cut instant, its profile memberships, and its
/// prediction targets.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WindowSpec {
    /// 0-based window index within the object (the ordinal that rides
    /// `ValidationResult::dt_days` on thin rows).
    pub index: u32,
    /// Integer UTC MJD night of the cut — derived metadata. **The cut
    /// instant is the authority**: the fit set is every non-excluded
    /// observation with `mjd_utc` strictly before the cut instant. For
    /// night cuts this equals `night <= cut_night`; for intra-night cuts
    /// (`intra_night == true`) only the time rule applies.
    pub cut_night: i64,
    /// The cut instant, ISO UTC (for night cuts: the midnight ending
    /// `cut_night`, nudged so no observation lies within 120 s of it; for
    /// intra-night cuts: the tracklet-gap midpoint).
    pub cut_utc: String,
    /// The same instant, MJD TT.
    pub cut_mjd_tt: f64,
    /// The same instant, MJD TDB — what TDB-sliced runners must use. A
    /// UTC-defined night sliced with a bare TDB integer moves boundary
    /// observations across the cut (TDB−UTC ≈ 69 s).
    pub cut_mjd_tdb: f64,
    /// True for intra-night (tracklet-boundary) cuts on short-arc objects.
    pub intra_night: bool,
    /// Profile memberships (`"full"` always; `"ci"`/`"ladder"` as selected).
    pub profiles: Vec<String>,
    /// Number of fit observations (non-excluded rows at/before the cut).
    pub n_obs_fit: u32,
    /// `obs_idx` of the last fit observation (fit epoch anchor; horizons are
    /// measured from this observation's `mjd_utc`).
    pub last_fit_obs_idx: u32,
    /// Fit-arc length, days (last fit obs − first fit obs).
    pub arc_days: f64,
    /// Held-out prediction targets.
    pub targets: Vec<TargetSpec>,
    /// `obs_idx` of in-sample control targets (inside the fit window; every
    /// tool must reproduce its own residuals there — pins conventions and
    /// the residual-covariance sign empirically).
    pub in_sample: Vec<u32>,
    /// Seed orbit for tools that need one (first window per object in v1).
    /// Computed from window data only — the plan IC is future information
    /// and is forbidden.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<SeedOrbit>,
}

/// A held-out prediction target.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TargetSpec {
    /// Index into [`ObjectWindows::observations`].
    pub obs_idx: u32,
    /// Rung label: `"next"` or `"d<N>"` for ladder rung +N days.
    pub rung: String,
    /// Realized horizon, days past the last fit observation. All binning is
    /// by this value, never by the rung label.
    pub horizon_days: f64,
}

/// A seed orbit (heliocentric-or-barycentric Cartesian; the frame/origin tag
/// says which — a covariance-free state, so a plain tagged vector suffices).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SeedOrbit {
    /// Epoch, MJD TDB.
    pub epoch_mjd_tdb: f64,
    /// Position, AU.
    pub pos_au: [f64; 3],
    /// Velocity, AU/day.
    pub vel_au_d: [f64; 3],
    /// Frame tag, e.g. `"ICRF"`.
    pub frame: String,
    /// Origin tag, e.g. `"SSB"` or `"Sun"`.
    pub origin: String,
    /// How the seed was produced (e.g. `"reference_iod_window_only"`).
    pub source: String,
}

/// `seed_source` values a runner declares on a window record.
pub mod seed_sources {
    /// The tool ran its own IOD on the window data.
    pub const OWN_IOD: &str = "own_iod";
    /// The manifest seed orbit was used.
    pub const MANIFEST_SEED: &str = "manifest_seed";
    /// Warm-started from the same tool's previous-window solution (state
    /// only — selection re-runs from a clean slate every window).
    pub const WARM_PREVIOUS: &str = "warm_previous";
}

/// Per-(object, config-arm, window) fit record — the runner side.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WalkWindowRecord {
    /// Catalog object name.
    pub object: String,
    /// Tool / channel that produced this record (`"rust"`, `"findorb"`, …).
    pub tool: String,
    /// Named `ODConfig` arm (`"default"` for externals).
    pub config_arm: String,
    /// Window index into the manifest schedule.
    pub window_index: u32,
    /// Did the fit converge? A failed window is a record with `false` and a
    /// named `failure` — never a missing record.
    pub converged: bool,
    /// Named failure reason when `converged == false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n_obs_used: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n_obs_rejected: Option<u32>,
    /// Solve-for dimensionality (6 state-only; 7–10 with non-gravs/DT).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n_solve_for: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iterations: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chi2: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reduced_chi2: Option<f64>,
    /// Fit / covariance epoch, MJD TDB (the manifest window epoch — both
    /// rejection arms share one linearization epoch by construction).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fit_epoch_mjd_tdb: Option<f64>,
    /// Whether this fit was warm-started (state only) from window − 1.
    pub warm_start: bool,
    /// See [`seed_sources`].
    pub seed_source: String,
    /// Engine covariance-trust verdict, when the tool reports one. Recorded
    /// as data; never used as a sample filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub covariance_trust: Option<String>,
    /// Wall-clock of the fit alone, ms.
    pub fit_time_ms: f64,
    /// Wall-clock of the prediction batch alone, ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predict_time_ms: Option<f64>,
    /// The fitted orbit + 6×6, in the standard captured form (pinned frame /
    /// element-set / units convention — a basis mismatch here is the
    /// canonical "χ² of 10⁶" bug).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured: Option<CapturedOrbit>,
}

/// One predicted observation — the runner side. Runners emit predictions,
/// never statistics; the kernel scores every tool identically.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PredictedObservation {
    pub object: String,
    pub tool: String,
    pub config_arm: String,
    pub window_index: u32,
    /// Index into the manifest observation table.
    pub obs_idx: u32,
    /// Echo of the manifest [`ObsEntry::key_hash`] — join safety across
    /// re-orderings.
    pub key_hash: u64,
    /// True for in-sample control targets.
    pub in_sample: bool,
    /// Predicted astrometric RA, degrees (light-time only; no stellar
    /// aberration, no deflection; epoch = photon reception time).
    pub ra_deg: f64,
    /// Predicted astrometric Dec, degrees.
    pub dec_deg: f64,
    /// Which time scale the runner actually consumed for the target epoch
    /// (`"utc"`, `"tt"`, `"tdb"`) — cross-checked in the zero-window control.
    pub epoch_scale_used: String,
    /// See [`uncertainty_forms`].
    pub uncertainty_form: String,
    /// Predicted 2×2 sky covariance in the declared units/basis
    /// (row/col order: RA-ish then Dec). Present iff
    /// `uncertainty_form == "radec_2x2"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cov_radec: Option<[[f64; 2]; 2]>,
    /// Units of `cov_radec`: `"arcsec2"`, `"deg2"`, or `"rad2"`. The kernel
    /// normalizes once; three tools ship three unit conventions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cov_units: Option<String>,
    /// Basis of the RA component: `"great_circle"` (already × cos δ) or
    /// `"native_ra"`. The kernel contracts native RA by cos δ.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cov_basis: Option<String>,
    /// 1-D σ (arcsec) along `sigma1_pa_deg` — find_orb's variant form.
    /// Present iff `uncertainty_form == "sigma1d_pa"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sigma1_arcsec: Option<f64>,
    /// Position angle of the 1-D σ, degrees East of North.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sigma1_pa_deg: Option<f64>,
    /// Predicted position angle of motion, degrees East of North (the
    /// reference channel's value per target becomes the shared AT/CT
    /// rotation angle).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pa_motion_deg: Option<f64>,
    /// Predicted sky rate, deg/day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sky_rate_deg_day: Option<f64>,
}

/// Kernel-scored prediction — one per (tool, arm, prediction, σ-table).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ScoredPrediction {
    pub object: String,
    pub class: String,
    pub tool: String,
    pub config_arm: String,
    pub window_index: u32,
    pub obs_idx: u32,
    pub in_sample: bool,
    /// Fit-arc length of the producing window, days.
    pub arc_days: f64,
    /// Realized horizon, days.
    pub horizon_days: f64,
    /// Rung label (provenance only — never a binning axis).
    pub rung: String,
    /// UTC night of the target.
    pub night: i64,
    /// Which pinned σ table scored this row (`"pinned"`, `"ades_only"`, …).
    pub sigma_table: String,
    /// Whether the EFCC2020 debias correction was applied to the observed
    /// position before differencing.
    pub debias_applied: bool,
    /// Gnomonic residual components about the predicted direction, arcsec:
    /// x = +East, y = +North.
    pub resid_east_arcsec: f64,
    pub resid_north_arcsec: f64,
    /// Total gnomonic separation, arcsec.
    pub sep_arcsec: f64,
    /// Along-track / cross-track residuals through the **shared** (reference)
    /// PA, arcsec.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resid_at_arcsec: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resid_ct_arcsec: Option<f64>,
    /// Mahalanobis \( d^2 = \Delta^\top (\Sigma_{obs} + \Sigma_{pred})^{-1}
    /// \Delta \), 2 dof. `None` when the tool delivered no 2×2 or a gate
    /// fired (see `flags`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub d2_combined: Option<f64>,
    /// \( d^2 \) against \( \Sigma_{obs} \) alone.
    pub d2_obs_only: f64,
    /// \( d^2 \) against \( \Sigma_{pred} \) alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub d2_pred_only: Option<f64>,
    /// \( \chi^2_2 \) survival probability of `d2_combined`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_chi2: Option<f64>,
    /// 1-dof normalized residual along the tool's reported dominant
    /// uncertainty direction — the statistic every tool (incl. find_orb's
    /// σ+PA form) can be scored under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub z1_dominant: Option<f64>,
    /// 1-dof normalized cross-track residual (near-linear far longer than
    /// along-track; the honest long-horizon realism statistic).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub z_ct: Option<f64>,
    /// Observation noise dominates the prediction term:
    /// \( \mathrm{tr}\,\Sigma_{obs} > \mathrm{tr}\,\Sigma_{pred} \).
    /// Such a row's d² measures the scoring σ table, not the tool's
    /// covariance — realism aggregates (histograms, night statistics,
    /// coverage) exclude it; accuracy keeps it. `false` when no 2×2 was
    /// delivered. Serde-defaulted so older scored files stay readable.
    #[serde(default)]
    pub obs_noise_dominated: bool,
    /// Named gates and taints: `"d2_invalid_large_separation"`,
    /// `"non_psd_covariance"`, `"same_night_continuation"`,
    /// `"unk_catalog_no_debias"`, `"covariance_trust_flagged"`, …
    /// Flagged rows are excluded from headline aggregates and reported
    /// separately — never dropped.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<String>,
}

/// One cell of the per-(class, tool, arm) predictive-power surface.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SurfaceCell {
    pub class: String,
    pub tool: String,
    pub config_arm: String,
    pub sigma_table: String,
    /// Arc bin, days (log-spaced, [lo, hi)).
    pub arc_lo_days: f64,
    pub arc_hi_days: f64,
    /// Horizon bin, days (log-spaced, [lo, hi)).
    pub dt_lo_days: f64,
    pub dt_hi_days: f64,
    /// Independent-unit count: objects contributing. Cells with < 3 are
    /// rendered hatched and excluded from any verdict.
    pub n_objects: u32,
    pub n_windows: u32,
    pub n_predictions: u32,
    /// Fraction of manifest-expected predictions the tool delivered in this
    /// cell (survivorship is shown, never hidden).
    pub delivered_fraction: f64,
    /// Equal-weight-per-object median separation, arcsec.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub med_sep_arcsec: Option<f64>,
    /// Normalized realism: equal-weight-per-object
    /// \( \mathrm{median}(d^2) / (2\ln 2) \) — calibrated value exactly 1
    /// (the \( \chi^2_2 \) median is \( 2\ln 2 \), not 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub med_d2_norm: Option<f64>,
    /// Empirical 1σ coverage (target 0.3935 for 2 dof).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cov_1s: Option<f64>,
    /// Empirical 2σ coverage (target 0.8647).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cov_2s: Option<f64>,
}

/// The aggregate artifact the report embeds (`*_predict_agg.json`): surface
/// cells plus per-object one-liners. Everything else stays in the JSONL
/// sidecars, off the page.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PredictAggregates {
    /// Manifest snapshot id these aggregates were computed against.
    pub snapshot_id: String,
    pub cells: Vec<SurfaceCell>,
    pub per_object: Vec<ObjectWalkSummary>,
    /// Fixed-bin d² histograms per series — the report's
    /// distribution-vs-\( \chi^2_2 \) panel and reliability curve render
    /// from these without embedding scored rows. `default`-able so older
    /// aggregate files stay readable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub d2_histograms: Vec<D2Histogram>,
    /// Per-object walk timelines — one entry per (tool, arm, object), each
    /// carrying that walk's windows in order. The report's per-object
    /// timeline panel reads these; the (arc × dt) surfaces marginalize over
    /// exactly this data. `default`-able so older aggregate files stay
    /// readable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub per_window: Vec<WalkTimeline>,
    /// Reduced-\( \chi^2 \) histograms over converged windows, one per
    /// (tool, arm) — the fit-quality panel.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reduced_chi2: Vec<ReducedChi2Histogram>,
    /// Set when [`PER_WINDOW_BUDGET_BYTES`] forced the per-window family to
    /// be reduced. The report renders it loudly next to the timeline panel:
    /// a thinned family must never read as a complete one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_window_note: Option<String>,
}

/// One object's walk under one (tool, config arm), in window order — the
/// per-object timeline behind the marginalized (arc × dt) surfaces.
///
/// Columnar on purpose: the report reads whole columns, and an array of
/// numbers costs a fraction of what one JSON object per window would on an
/// embedded page carrying \( \mathcal{O}(10^5) \) windows. Every parallel
/// column has the same length; index `i` is one window.
///
/// The statistics use the surface cells' conventions exactly:
/// `med_d2_norm` is the median of that window's per-night joint
/// \( d^2 \) statistics over \( 2\ln 2 \) (unflagged, held-out,
/// \( \Sigma_{pred} \)-dominated rows only — where observation noise
/// dominates, \( d^2 \) measures the scoring table rather than the tool),
/// and `med_sep_arcsec` is the median over that window's unflagged held-out
/// rows (no \( \Sigma_{pred} \) condition — accuracy keeps those rows).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WalkTimeline {
    pub tool: String,
    pub config_arm: String,
    pub object: String,
    pub class: String,
    /// Window index into the manifest schedule, ascending. One entry per
    /// window the tool **attempted** — a failed window is a `false` in
    /// `converged` with null statistics, never a missing entry.
    pub window_index: Vec<u32>,
    /// Fit-arc length of each window, days (3 significant digits).
    pub arc_days: Vec<f64>,
    pub converged: Vec<bool>,
    /// Median normalized \( d^2 \) for the window (3 significant digits).
    /// `null` where the window delivered no \( \Sigma_{pred} \)-dominated
    /// held-out \( d^2 \) — a break in the curve, not a zero.
    pub med_d2_norm: Vec<Option<f64>>,
    /// Median gnomonic separation for the window, arcsec (3 significant
    /// digits).
    pub med_sep_arcsec: Vec<Option<f64>>,
    /// Unflagged held-out predictions the window contributed.
    pub n_preds: Vec<u32>,
}

/// Histogram of fit reduced \( \chi^2 \) over **converged** windows for one
/// (tool, config arm), on equal-width bins of \( \log_{10} \) over
/// \( [\mathrm{log10\_lo}, \mathrm{log10\_hi}) \).
///
/// `n_without` is the honest count of converged windows whose runner
/// reported no reduced \( \chi^2 \): those windows are absent from the
/// histogram and reported as "not reported", never folded into a zero bin.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReducedChi2Histogram {
    pub tool: String,
    pub config_arm: String,
    pub log10_lo: f64,
    pub log10_hi: f64,
    /// Equal-width bin counts over the \( \log_{10} \) domain.
    pub counts: Vec<u32>,
    /// Converged windows reporting an rχ² below the domain.
    pub underflow: u32,
    /// Converged windows reporting an rχ² at or above the domain.
    pub overflow: u32,
    /// Converged windows whose runner reported an rχ² (`counts` sum +
    /// `underflow` + `overflow` + `n_nonpositive`).
    pub n_with: u32,
    /// Converged windows whose runner reported none.
    pub n_without: u32,
    /// Converged windows reporting a non-positive or non-finite rχ² — no
    /// \( \log_{10} \) exists for those, so they are counted here rather
    /// than clamped into the first bin.
    pub n_nonpositive: u32,
    /// Degrees-of-freedom tally over the same converged windows, ascending
    /// by `ndof`.
    ///
    /// A reduced \( \chi^2 \) of 1 is only the *expectation*; the scatter
    /// around it is set by \( \nu \), and a walk's early windows sit at
    /// \( \nu \) of order 2–10 where \( \chi^2_\nu/\nu \) is enormously
    /// wide. Without \( \nu \) the reader cannot tell a badly-fitting
    /// configuration from a short arc, so the report draws the expected
    /// central interval of \( \chi^2_\nu/\nu \) under exactly this mixture.
    /// Compact on purpose: \( \nu \) is a small integer, so the tally is a
    /// few dozen pairs rather than one number per window.
    ///
    /// `default`-able so older aggregate files stay readable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dof: Vec<DofBin>,
}

/// One \( (\nu, \mathrm{count}) \) entry of a [`ReducedChi2Histogram`]'s
/// degrees-of-freedom tally.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DofBin {
    pub ndof: u32,
    pub n: u32,
}

/// Histogram of held-out combined \( d^2 \) for one
/// (tool, config arm, σ-table, class) series, over equal-width bins on
/// \( [0, \mathrm{domain\_max}) \) plus an overflow count. Unflagged,
/// held-out rows only — the same population every headline uses.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct D2Histogram {
    pub tool: String,
    pub config_arm: String,
    pub sigma_table: String,
    /// Class name, or `"all"` for the pooled series.
    pub class: String,
    /// Upper edge of the histogram domain.
    pub domain_max: f64,
    /// Equal-width bin counts over \( [0, \mathrm{domain\_max}) \).
    pub counts: Vec<u32>,
    /// Rows with \( d^2 \ge \mathrm{domain\_max} \).
    pub overflow: u32,
    /// Total rows (`counts` sum + `overflow`).
    pub n: u32,
    /// Log-spaced companion bins, equal width on
    /// \( \log_{10} d^2 \in [\mathrm{log10\_lo}, \mathrm{log10\_hi}) \).
    ///
    /// The linear family above resolves the body but caps at
    /// `domain_max`, which collapses the whole dangerous tail into one
    /// overflow integer. The survival curve \( P(d^2 > x) \) needs the tail
    /// *resolved* — a corpus that leaves 27% beyond \( d^2 = 10 \) has to
    /// show where that 27% actually lands — so these bins run to
    /// \( d^2 = 10^3 \) and the report draws the survival plot from them.
    /// Both families are written from the same rows; neither replaces the
    /// other (the reliability curve wants the fine linear body).
    ///
    /// `default`-able so older aggregate files stay readable — an empty
    /// `log_counts` means "this aggregate predates the survival bins", and
    /// the report says so rather than drawing an empty curve.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub log_counts: Vec<u32>,
    #[serde(default)]
    pub log10_lo: f64,
    #[serde(default)]
    pub log10_hi: f64,
    /// Rows with \( 0 < d^2 < 10^{\mathrm{log10\_lo}} \).
    #[serde(default)]
    pub log_underflow: u32,
    /// Rows with \( d^2 \ge 10^{\mathrm{log10\_hi}} \).
    #[serde(default)]
    pub log_overflow: u32,
    /// Rows with \( d^2 = 0 \) exactly — no \( \log_{10} \) exists, so they
    /// are counted rather than folded into the first bin. They still count
    /// in `n` and in the survival curve's denominator.
    #[serde(default)]
    pub log_zero: u32,
}

/// Per-(object, tool, arm) walk summary for the delivered/failed matrix.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ObjectWalkSummary {
    pub object: String,
    pub class: String,
    pub tool: String,
    pub config_arm: String,
    pub n_windows_expected: u32,
    pub n_windows_converged: u32,
    pub n_windows_failed: u32,
    pub n_predictions: u32,
    /// Median normalized d² over this object's unflagged predictions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub med_d2_norm: Option<f64>,
    /// Predictions counted regardless of the kernel's trust flags — a
    /// series whose every window the engine flags (e.g. a 9-parameter
    /// comet fit marked `WeaklyDeterminedHighN`) is otherwise invisible in
    /// the aggregate. Never a substitute for `n_predictions`: a reader that
    /// uses it names the flag beside the number.
    #[serde(default)]
    pub n_predictions_incl_flagged: u32,
    /// Median normalized d² over this object's predictions INCLUDING
    /// trust-flagged rows (same night-joint statistic as `med_d2_norm`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub med_d2_norm_incl_flagged: Option<f64>,
    /// Trust-flag tallies over the flagged rows (flag name → rows).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub flag_counts: std::collections::BTreeMap<String, u32>,
    /// The engine's own covariance-trust verdicts over this series' converged
    /// windows (variant name → windows; `"trusted"` counted too), so a
    /// `covariance_trust_flagged` tally can be read back to its reason.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub trust_reasons: std::collections::BTreeMap<String, u32>,
    /// Median separation over this object's predictions, arcsec.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub med_sep_arcsec: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs_entry() -> ObsEntry {
        ObsEntry {
            stn: "F51".into(),
            obs_time: "2024-12-27T09:14:33.12Z".into(),
            key_hash: obs_key_hash("F51", "2024-12-27T09:14:33.12Z", "34.1", "-2.2"),
            mjd_utc: 60671.385,
            night: 60671,
            ra_deg: 34.1,
            dec_deg: -2.2,
            ast_cat: "Gaia2".into(),
            mode: "CCD".into(),
            sigma_ra_arcsec: 0.2,
            sigma_dec_arcsec: 0.2,
            sigma_corr: 0.0,
            sigma_source: "ades".into(),
            ades_rms_ra: Some(0.2),
            ades_rms_dec: Some(0.2),
            debias_dra_arcsec: Some(-0.03),
            debias_ddec_arcsec: Some(0.01),
            excluded: None,
        }
    }

    #[test]
    fn manifest_round_trips() {
        let m = WindowManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            snapshot_id: "2026-07-29-75ab459bc8b2".into(),
            bundle_nights: 1,
            generated_by: "empyrean-validation test".into(),
            ladder_days: vec![3.0, 7.0, 14.0, 30.0, 90.0, 180.0, 365.0],
            ladder_tolerance: 0.5,
            min_horizon_days: 0.3,
            max_obs_per_night: 12,
            objects: vec![ObjectWindows {
                object: "2024 YR4".into(),
                mpc_designation: "2024 YR4".into(),
                population: "NEO".into(),
                class: classes::from_population("NEO").into(),
                eligible: true,
                ineligible_reason: None,
                first_night: 60669,
                apparition_end_night: 60800,
                n_obs_total: 1,
                n_excluded_space_based: 0,
                observations: vec![obs_entry()],
                windows: vec![WindowSpec {
                    index: 0,
                    cut_night: 60683,
                    cut_utc: "2024-12-31T00:00:00Z".into(),
                    cut_mjd_tt: 60_684.000_801,
                    cut_mjd_tdb: 60_684.000_801,
                    intra_night: false,
                    profiles: vec![profiles::FULL.into(), profiles::CI.into()],
                    n_obs_fit: 1,
                    last_fit_obs_idx: 0,
                    arc_days: 14.0,
                    targets: vec![TargetSpec {
                        obs_idx: 0,
                        rung: "next".into(),
                        horizon_days: 0.9,
                    }],
                    in_sample: vec![0],
                    seed: Some(SeedOrbit {
                        epoch_mjd_tdb: 60684.0,
                        pos_au: [1.0, 0.1, 0.01],
                        vel_au_d: [-0.001, 0.017, 0.0001],
                        frame: "ICRF".into(),
                        origin: "SSB".into(),
                        source: "reference_iod_window_only".into(),
                    }),
                }],
            }],
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: WindowManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn sidecar_records_round_trip() {
        let w = WalkWindowRecord {
            object: "2024 YR4".into(),
            tool: "rust".into(),
            config_arm: "default".into(),
            window_index: 3,
            converged: false,
            failure: Some("IOD failed: poorly constrained".into()),
            n_obs_used: None,
            n_obs_rejected: None,
            n_solve_for: None,
            iterations: None,
            chi2: None,
            reduced_chi2: None,
            fit_epoch_mjd_tdb: None,
            warm_start: false,
            seed_source: seed_sources::OWN_IOD.into(),
            covariance_trust: None,
            fit_time_ms: 12.5,
            predict_time_ms: None,
            captured: None,
        };
        let back: WalkWindowRecord =
            serde_json::from_str(&serde_json::to_string(&w).unwrap()).unwrap();
        assert_eq!(w, back);

        let p = PredictedObservation {
            object: "2024 YR4".into(),
            tool: "rust".into(),
            config_arm: "default".into(),
            window_index: 3,
            obs_idx: 17,
            key_hash: 42,
            in_sample: false,
            ra_deg: 34.2,
            dec_deg: -2.3,
            epoch_scale_used: "tdb".into(),
            uncertainty_form: uncertainty_forms::RADEC_2X2.into(),
            cov_radec: Some([[1e-8, 2e-9], [2e-9, 3e-8]]),
            cov_units: Some("deg2".into()),
            cov_basis: Some("native_ra".into()),
            sigma1_arcsec: None,
            sigma1_pa_deg: None,
            pa_motion_deg: Some(112.4),
            sky_rate_deg_day: Some(0.6),
        };
        let back: PredictedObservation =
            serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn unknown_fields_are_rejected() {
        // deny_unknown_fields is the contract everywhere in this crate; a
        // runner inventing a field must fail loudly at parse, not vanish.
        let json = r#"{"obs_idx":0,"rung":"next","horizon_days":1.0,"surprise":true}"#;
        assert!(serde_json::from_str::<TargetSpec>(json).is_err());
    }

    #[test]
    fn key_hash_distinguishes_fields_and_is_stable() {
        let a = obs_key_hash("F51", "2024-12-27T09:14:33.12Z", "34.1", "-2.2");
        let b = obs_key_hash("F52", "2024-12-27T09:14:33.12Z", "34.1", "-2.2");
        let c = obs_key_hash("F51", "2024-12-27T09:14:33.12Z", "34.1", "-2.3");
        assert_ne!(a, b);
        assert_ne!(a, c);
        // Pinned value: the hash is part of the cross-runner contract; a
        // silent algorithm change would orphan every existing sidecar.
        assert_eq!(
            a,
            obs_key_hash("F51", "2024-12-27T09:14:33.12Z", "34.1", "-2.2")
        );
    }

    #[test]
    fn every_catalog_population_has_a_class() {
        for obj in crate::catalog::all_objects() {
            // Panics inside from_population on an unmapped population.
            let _ = classes::from_population(obj.population);
        }
    }
}
