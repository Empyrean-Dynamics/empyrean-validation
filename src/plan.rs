//! Canonical-plan generator.
//!
//! [`build_plan`] reads the [`crate::catalog`], queries JPL SBDB for
//! initial conditions and non-gravitational parameters, queries JPL
//! Horizons for reference state vectors and reference ephemerides at
//! every `(object, dt)`, and emits a [`ValidationPlan`] (`Vec<ValidationResult>`
//! with `channel = "plan"`) that every replay channel consumes.
//!
//! # Network behaviour
//!
//! Both SBDB and Horizons queries go through the published `empyrean`
//! crate's disk-cached JPL clients (`cache_dir`-keyed)
//! — the first run hits the network and writes to disk; subsequent runs
//! against the same cache are network-free. CI environments should
//! commit the cache as a workflow artifact (or pull it from GCS) so the
//! plan generation step is deterministic across runs.
//!
//! # Plan row shape
//!
//! Every plan row carries:
//!
//! - **Test identity** (`object`, `population`, `epoch_mjd_tdb`, `dt_days`,
//!   `t_mjd_tdb`, `force_model`, `test_type`, `observer`, …)
//! - **Initial conditions** (`ic_*` — Cartesian state, A1/A2/A3, g(r) tuple,
//!   non-grav time-delay)
//! - **Reference values** (`ref_*` — Horizons truth at the target epoch)
//! - `channel = "plan"`
//! - All `emp_*` and channel-specific output fields nulled
//!
//! Replay channels overwrite `channel`, `timestamp`, and the `emp_*`
//! fields with their own results when consuming the plan.

use std::collections::HashMap;

use empyrean::EphemerisEntry;

use crate::catalog::{DEFAULT_DT_DAYS, OBSERVER_CODES, ValidationObject};
use crate::schema::{ValidationPlan, ValidationResult, channels, test_types, uncertainty_modes};

/// Plan-generation configuration.
#[derive(Debug, Clone)]
pub struct PlanConfig {
    /// Force-model tier names emitted on plan rows. Each tier produces
    /// its own row per (object, dt) combination.
    pub tiers: Vec<String>,
    /// When `true`, emit both `first_order_with_cov` and `f64_no_cov`
    /// propagation+ephemeris rows so the replay channels exercise the
    /// uncertainty axis. When `false`, only `f64_no_cov` rows are
    /// emitted (benchmark mode for head-to-head comparison with external
    /// propagators that don't propagate covariance).
    pub uncertainty_axis: bool,
}

impl Default for PlanConfig {
    fn default() -> Self {
        Self {
            tiers: vec!["standard".to_string()],
            uncertainty_axis: true,
        }
    }
}

/// Build the canonical validation plan.
///
/// For every object in `objects`:
///
/// 1. Fetch its SBDB record (epoch + non-grav parameters).
/// 2. Fetch its Horizons IC at the SBDB epoch.
/// 3. For every `dt` in the object's grid (`obj.dt_days` or
///    [`DEFAULT_DT_DAYS`] as fallback), fetch Horizons vectors and an
///    ephemeris record from observer code [`OBSERVER_CODES`]`[0]`
///    (`W84` — CTIO 4m).
/// 4. Emit propagation plan rows for every `(object, dt, tier,
///    uncertainty_mode)` and ephemeris plan rows for every `(object,
///    dt != 0)`. When `config.uncertainty_axis` is true, every prop+eph
///    row is doubled across both modes.
/// 5. For objects with `skip_od == false`, emit one OD plan row per
///    tier with `excluded_perturbers_naif` populated for self-perturbers
///    (NAIF id = 2_000_000 + asteroid number).
///
/// Network errors on individual objects are logged to stderr and skip
/// that object — the plan continues with whatever objects responded.
pub fn build_plan(
    objects: &[&ValidationObject],
    config: &PlanConfig,
    sbdb_cache_dir: &std::path::Path,
    horizons_cache_dir: &std::path::Path,
) -> ValidationPlan {
    let timestamp = chrono::Utc::now().to_rfc3339();
    let mut plan: ValidationPlan = Vec::new();
    let uncertainty_modes_to_emit: &[Option<&str>] = if config.uncertainty_axis {
        &[
            Some(uncertainty_modes::FIRST_ORDER_WITH_COV),
            Some(uncertainty_modes::F64_NO_COV),
        ]
    } else {
        &[Some(uncertainty_modes::F64_NO_COV)]
    };

    eprintln!("Fetching initial conditions and reference values...");

    for obj in objects {
        // 1. SBDB → IC epoch + non-grav parameters.
        let sbdb = match empyrean::query_sbdb(&[obj.sbdb_query], Some(sbdb_cache_dir)) {
            Ok(b) if !b.orbits.is_empty() => b,
            Ok(_) => {
                eprintln!("  {}: SKIP (SBDB: empty result)", obj.name);
                continue;
            }
            Err(e) => {
                eprintln!("  {}: SKIP (SBDB: {e})", obj.name);
                continue;
            }
        };
        let epoch = match sbdb.orbits[0].state.epoch.mjd_tdb() {
            Ok(t) => t,
            Err(e) => {
                eprintln!("  {}: SKIP (SBDB epoch: {e})", obj.name);
                continue;
            }
        };

        // 2. Horizons IC at SBDB epoch.
        let (ic_pos, ic_vel) = match empyrean::query_horizons_vectors(
            obj.horizons_command,
            epoch,
            Some(horizons_cache_dir),
        ) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("  {}: SKIP (Horizons IC: {e})", obj.name);
                continue;
            }
        };

        // Marsden non-grav from SBDB. SBDB defaults to inverse_square for
        // asteroids and water-ice for comets. `dt` is the SBDB time-delay
        // (days) applied to g(r) — non-zero for Jupiter-family comets and
        // some interstellar objects (67P=+45.7d, 2I/Borisov=−65.1d).
        let (a1, a2, a3, g_alpha, g_r0, g_m, g_n, g_k, ng_dt) = {
            let o = &sbdb.orbits[0];
            // The wrapper carries the Marsden g(r) parameters as flat
            // fields with an all-zero sentinel for the inverse-square
            // default; the plan rows record the canonical inverse-square
            // constants (α=1, r0=1, m=2, n=0, k=0) in that case, exactly
            // as the engine-typed client did.
            let has_g = o.ng_alpha != 0.0
                || o.ng_r0 != 0.0
                || o.ng_m != 0.0
                || o.ng_n != 0.0
                || o.ng_k != 0.0;
            let (ga, gr0, gm, gn, gk) = if has_g {
                (o.ng_alpha, o.ng_r0, o.ng_m, o.ng_n, o.ng_k)
            } else {
                (1.0, 1.0, 2.0, 0.0, 0.0)
            };
            (o.a1, o.a2, o.a3, ga, gr0, gm, gn, gk, o.non_grav_dt)
        };

        eprintln!(
            "  {}: epoch={:.1} MJD TDB (Horizons IC, a1={:.2e}, dt={:?})",
            obj.name, epoch, a1, ng_dt
        );

        // 3. Per-dt Horizons fetches.
        let dt_list = obj.dt_days.unwrap_or(DEFAULT_DT_DAYS);
        let mut horizons_vectors: HashMap<i64, ([f64; 3], [f64; 3])> = HashMap::new();
        // Sun's SSB state at each target epoch (Horizons command "10").
        // Carried on propagation rows so Sun-centered external tools
        // (OpenOrb's heliocentric orbit convention) can convert to/from the
        // plan's SSB frame without their own planetary ephemeris.
        let mut sun_vectors: HashMap<i64, ([f64; 3], [f64; 3])> = HashMap::new();
        for &dt in dt_list {
            let target = epoch + dt;
            match empyrean::query_horizons_vectors(
                obj.horizons_command,
                target,
                Some(horizons_cache_dir),
            ) {
                Ok(h) => {
                    horizons_vectors.insert(dt as i64, h);
                }
                Err(e) => {
                    eprintln!("  {}: dt={dt:+.0}d Horizons SKIP ({e})", obj.name);
                }
            }
            match empyrean::query_horizons_vectors("10", target, Some(horizons_cache_dir)) {
                Ok(h) => {
                    sun_vectors.insert(dt as i64, h);
                }
                Err(e) => {
                    eprintln!("  {}: dt={dt:+.0}d Sun-vector SKIP ({e})", obj.name);
                }
            }
        }

        // Ephemeris (RA/Dec) is observer-dependent (topocentric parallax /
        // light-time), so fetch it from every site in OBSERVER_CODES; the
        // report averages the pairwise sky-plane separation over the sites.
        // (Propagation state vectors above are observer-independent — one
        // per (object, dt) — so they are not looped over observers.)
        let mut horizons_ephemeris: HashMap<(&str, i64), EphemerisEntry> = HashMap::new();
        for &obs in OBSERVER_CODES {
            for &dt in dt_list {
                if dt == 0.0 {
                    continue;
                }
                let target = epoch + dt;
                match empyrean::query_horizons(
                    &[obj.horizons_command],
                    obs,
                    &[target],
                    Some(horizons_cache_dir),
                ) {
                    Ok(r) if !r.is_empty() => {
                        horizons_ephemeris.insert((obs, dt as i64), r.into_iter().next().unwrap());
                    }
                    Ok(_) => {
                        eprintln!("  {}: {obs} dt={dt:+.0}d ephemeris SKIP (empty)", obj.name);
                    }
                    Err(e) => {
                        eprintln!("  {}: {obs} dt={dt:+.0}d ephemeris SKIP ({e})", obj.name);
                    }
                }
            }
        }

        // 4a. Propagation plan rows.
        for tier in &config.tiers {
            for &dt in dt_list {
                let Some(&(ref_pos, ref_vel)) = horizons_vectors.get(&(dt as i64)) else {
                    continue;
                };
                for &uncertainty in uncertainty_modes_to_emit {
                    plan.push(propagation_plan_row(
                        obj,
                        epoch,
                        dt,
                        tier,
                        ic_pos,
                        ic_vel,
                        (a1, a2, a3, g_alpha, g_r0, g_m, g_n, g_k, ng_dt),
                        ref_pos,
                        ref_vel,
                        sun_vectors.get(&(dt as i64)).copied(),
                        uncertainty,
                        &timestamp,
                    ));
                }
            }
        }

        // 4b. Ephemeris plan rows (only the Standard tier — matches the
        // existing rust runner's behaviour where ephemeris validation
        // doesn't iterate force-model tiers).
        for &obs in OBSERVER_CODES {
            for &dt in dt_list {
                if dt == 0.0 {
                    continue;
                }
                let Some(hor) = horizons_ephemeris.get(&(obs, dt as i64)) else {
                    continue;
                };
                for &uncertainty in uncertainty_modes_to_emit {
                    plan.push(ephemeris_plan_row(
                        obj,
                        epoch,
                        dt,
                        obs,
                        ic_pos,
                        ic_vel,
                        (a1, a2, a3, g_alpha, g_r0, g_m, g_n, g_k, ng_dt),
                        hor,
                        uncertainty,
                        &timestamp,
                    ));
                }
            }
        }

        // 5. OD plan rows. self-perturbers stay in (skip_od is honored
        // by the runner via the excluded_perturbers_naif field, not by
        // dropping the row from the plan — the plan tells the runner
        // which perturbers to exclude, the runner does the actual fit).
        if !obj.skip_od {
            for tier in &config.tiers {
                plan.push(od_plan_row(obj, tier, &timestamp));
            }
        } else {
            // Self-perturbers DO get OD plan rows, with their own NAIF id
            // in excluded_perturbers_naif. The skip_od flag was a legacy
            // workaround from before the runner honored exclusions; with
            // empyrean-w351 and the related self-perturber wiring the
            // flag is purely advisory. Emit the row anyway and let the
            // exclusion mechanism do the right thing.
            for tier in &config.tiers {
                plan.push(od_plan_row(obj, tier, &timestamp));
            }
        }
    }

    eprintln!("Plan: {} rows", plan.len());
    plan
}

/// Build a single propagation plan row.
#[allow(clippy::too_many_arguments)]
fn propagation_plan_row(
    obj: &ValidationObject,
    epoch: f64,
    dt: f64,
    tier: &str,
    ic_pos: [f64; 3],
    ic_vel: [f64; 3],
    nongrav: (f64, f64, f64, f64, f64, f64, f64, f64, Option<f64>),
    ref_pos: [f64; 3],
    ref_vel: [f64; 3],
    sun: Option<([f64; 3], [f64; 3])>,
    uncertainty: Option<&str>,
    timestamp: &str,
) -> ValidationResult {
    let (a1, a2, a3, g_alpha, g_r0, g_m, g_n, g_k, ng_dt) = nongrav;
    let mut r = ValidationResult::empty();
    r.object = obj.name.to_string();
    r.population = obj.population.to_string();
    r.epoch_mjd_tdb = epoch;
    r.dt_days = dt;
    r.t_mjd_tdb = epoch + dt;
    r.force_model = tier.to_string();
    r.test_type = test_types::PROPAGATION.to_string();
    r.channel = channels::PLAN.to_string();
    r.ic_pos_au = Some(ic_pos);
    r.ic_vel_au_d = Some(ic_vel);
    r.ic_a1 = Some(a1);
    r.ic_a2 = Some(a2);
    r.ic_a3 = Some(a3);
    r.ic_g_alpha = Some(g_alpha);
    r.ic_g_r0 = Some(g_r0);
    r.ic_g_m = Some(g_m);
    r.ic_g_n = Some(g_n);
    r.ic_g_k = Some(g_k);
    r.ic_non_grav_dt = ng_dt;
    r.ref_pos_au = Some(ref_pos);
    r.ref_vel_au_d = Some(ref_vel);
    r.ref_sun_pos_au = sun.map(|(p, _)| p);
    r.ref_sun_vel_au_d = sun.map(|(_, v)| v);
    r.propagation_uncertainty = uncertainty.map(|s| s.to_string());
    r.timestamp = timestamp.to_string();
    r.notes = obj.notes.to_string();
    r
}

/// Build a single ephemeris plan row.
#[allow(clippy::too_many_arguments)]
fn ephemeris_plan_row(
    obj: &ValidationObject,
    epoch: f64,
    dt: f64,
    obs_code: &str,
    ic_pos: [f64; 3],
    ic_vel: [f64; 3],
    nongrav: (f64, f64, f64, f64, f64, f64, f64, f64, Option<f64>),
    hor: &EphemerisEntry,
    uncertainty: Option<&str>,
    timestamp: &str,
) -> ValidationResult {
    let (a1, a2, a3, g_alpha, g_r0, g_m, g_n, g_k, ng_dt) = nongrav;
    let mut r = ValidationResult::empty();
    r.object = obj.name.to_string();
    r.population = obj.population.to_string();
    r.epoch_mjd_tdb = epoch;
    r.dt_days = dt;
    r.t_mjd_tdb = epoch + dt;
    r.force_model = "standard".to_string();
    r.test_type = test_types::EPHEMERIS.to_string();
    r.channel = channels::PLAN.to_string();
    r.observer = Some(obs_code.to_string());
    r.ic_pos_au = Some(ic_pos);
    r.ic_vel_au_d = Some(ic_vel);
    r.ic_a1 = Some(a1);
    r.ic_a2 = Some(a2);
    r.ic_a3 = Some(a3);
    r.ic_g_alpha = Some(g_alpha);
    r.ic_g_r0 = Some(g_r0);
    r.ic_g_m = Some(g_m);
    r.ic_g_n = Some(g_n);
    r.ic_g_k = Some(g_k);
    r.ic_non_grav_dt = ng_dt;
    r.ref_ra_rad = Some(hor.ra_deg.to_radians());
    r.ref_dec_rad = Some(hor.dec_deg.to_radians());
    r.ref_rho_au = Some(hor.rho_au);
    // The wrapper reports light time as a plain f64 with NaN for
    // unavailable; preserve the Option semantics of the plan schema.
    r.ref_light_time_d = (!hor.light_time_days.is_nan()).then_some(hor.light_time_days);
    r.propagation_uncertainty = uncertainty.map(|s| s.to_string());
    r.timestamp = timestamp.to_string();
    r.notes = obj.notes.to_string();
    r
}

/// Build a single OD plan row.
fn od_plan_row(obj: &ValidationObject, tier: &str, timestamp: &str) -> ValidationResult {
    let mut r = ValidationResult::empty();
    r.object = obj.name.to_string();
    r.population = obj.population.to_string();
    r.epoch_mjd_tdb = 0.0; // filled in by the OD runner
    r.dt_days = 0.0;
    r.t_mjd_tdb = 0.0;
    r.force_model = tier.to_string();
    r.test_type = test_types::ORBIT_DETERMINATION.to_string();
    r.channel = channels::PLAN.to_string();
    r.excluded_perturbers_naif = self_perturber_naif_ids(obj);
    r.timestamp = timestamp.to_string();
    r.notes = obj.notes.to_string();
    r
}

/// Return the NAIF IDs to exclude from the perturber set when running
/// OD on this object. For SB441-N16 self-perturbers (population
/// `"Self-Perturber"`), this is the body's own NAIF id (asteroid id =
/// 2_000_000 + IAU number, as encoded in `mpc_designation`). For other
/// objects, empty.
fn self_perturber_naif_ids(obj: &ValidationObject) -> Vec<i32> {
    if obj.population != "Self-Perturber" {
        return Vec::new();
    }
    obj.mpc_designation
        .parse::<i32>()
        .map(|n| vec![2_000_000 + n])
        .unwrap_or_default()
}

// ── Plan contract (the strip) ───────────────────────────────────────────
//
// `make plan` derives the canonical plan from the rust channel's own output
// rather than re-querying JPL, so the plan inherits whatever field set the
// rust runner happened to emit. That coupling broke the `core` reference
// channel: empyrean-core pins the v0.7.0 schema, whose `ValidationResult` is
// `#[serde(deny_unknown_fields)]`, and the rust runner had since gained
// `emp_pos_cov_au2`, `emp_radec_cov_arcsec2`, and `source_version`. The core
// channel aborted on the unknown keys, which failed the empyrean matrix job,
// which skipped reduce / gate / publish entirely.
//
// The subtlety that makes a blacklist unfixable: the old strip NULLED the
// fields it wanted to remove. `deny_unknown_fields` rejects on key PRESENCE,
// not on value — `"emp_pos_cov_au2": null` fails exactly as hard as a
// populated one. Adding fields to a clear list could never have fixed it.
//
// So the contract is inverted: an explicit whitelist of keys the plan may
// carry, with everything else POPPED. A field added to the rust runner
// tomorrow cannot reach a pinned consumer, because reaching the plan now
// requires being named here.

/// Plan-contract keys whose **values are carried through**: test identity,
/// initial conditions, and the Horizons reference values every replay channel
/// compares against.
pub const PLAN_CARRIED_KEYS: [&str; 30] = [
    // Test identity
    "object",
    "population",
    "epoch_mjd_tdb",
    "dt_days",
    "t_mjd_tdb",
    "force_model",
    "test_type",
    "channel",
    "observer",
    "propagation_uncertainty",
    "excluded_perturbers_naif",
    "timestamp",
    "notes",
    // Initial conditions
    "ic_pos_au",
    "ic_vel_au_d",
    "ic_a1",
    "ic_a2",
    "ic_a3",
    "ic_g_alpha",
    "ic_g_r0",
    "ic_g_m",
    "ic_g_n",
    "ic_g_k",
    "ic_non_grav_dt",
    // Horizons reference values
    "ref_pos_au",
    "ref_vel_au_d",
    "ref_ra_rad",
    "ref_dec_rad",
    "ref_rho_au",
    "ref_light_time_d",
];

/// Plan-contract keys that are **present but null**.
///
/// These carry no plan data — they are result slots a channel fills in. They
/// cannot simply be popped: the schema does not mark them `#[serde(default)]`,
/// on the pinned v0.7.0 shape or the current one, so a channel output derived
/// from a plan missing them would fail to deserialize on a *missing* key
/// instead of an unknown one. Present-and-null is the only value that
/// satisfies both halves of the contract.
pub const PLAN_CLEARED_KEYS: [&str; 23] = [
    "emp_vs_horizons_km",
    "emp_pos_au",
    "emp_time_ms",
    "separation_arcsec",
    "d_ra_arcsec",
    "d_dec_arcsec",
    "d_rho_km",
    "d_light_time_s",
    "n_obs_used",
    "od_iterations",
    "od_converged",
    "od_rms_ra_arcsec",
    "od_rms_dec_arcsec",
    "od_rms_combined_arcsec",
    "od_chi2",
    "od_reduced_chi2",
    "assist_vs_horizons_km",
    "emp_vs_assist_km",
    "assist_time_ms",
    "speed_ratio",
    "findorb_rms_residual",
    "findorb_n_obs_used",
    "findorb_n_obs_rejected",
];

/// Uncertainty axes a replay channel can actually reproduce.
///
/// The rust reference also sweeps `second_order_with_cov`, `auto`,
/// `sigma_point_with_cov`, and `monte_carlo_100_with_cov`. Those are
/// rust-only axes; a plan row asking another channel to replay one is a row
/// that channel will never match. The old strip blacklisted the first two and
/// let the sigma-point and Monte-Carlo rows through, so they rode into the
/// "plan" as unreplayable work. Whitelisted for the same reason the key set
/// is: a new rust-only axis must not silently become everyone's problem.
///
/// `None` (OD rows carry no uncertainty tag) is always in the plan.
pub const PLAN_UNCERTAINTY_AXES: [&str; 2] = [
    uncertainty_modes::FIRST_ORDER_WITH_COV,
    uncertainty_modes::F64_NO_COV,
];

/// Test types no replay channel can reproduce, and so must never reach the
/// plan.
///
/// **Deliberate and temporary — tracked as `empyrean-s1ab`.** Un-nesting the
/// radar OD pass made the rust runner emit `orbit_determination_radar` rows for
/// the first time, and nothing but the rust runner can replay them:
///
/// - `runners/python/run.py` skips any non-`orbit_determination` row that
///   carries no `ic_pos_au` / `ic_vel_au_d`, and the radar rows carry
///   `ic_pos_au: null` by construction (the fit seeds itself from the fixture,
///   not from the plan), so python replays zero of them — a python-side floor
///   on that axis can never be met.
/// - `empyrean-core`'s `validate-core` has no radar arm; it falls through to
///   `_ => {}` and pushes the row anyway with a null position. The report
///   counts such a row as *compared* but never as *passing*, so the core
///   channel fails its `passing == total` strict check on exactly those rows
///   (measured: 448 rows, 446 passing, the 2 missing being the radar pair).
///
/// So a plan carrying these rows does not describe a cross-channel radar
/// comparison — it describes work no channel can do, and makes the gate
/// unsatisfiable for a reason unrelated to any real regression. Stripping them
/// is the honest state: the suite stops implying a comparison that does not
/// exist. The radar axis stays gated by a row floor on the **rust** channel,
/// where the rows are actually produced and visible (see the `--min-rows`
/// handling in `src/bin/cli.rs` and the ci-check step in
/// `.github/workflows/validation.yml`).
///
/// Removing an entry here is the *goal*, not a regression: once the replay
/// drivers grow a real radar arm (`empyrean-s1ab`), the row comes back into the
/// plan and its floor moves back onto the strict channels.
///
/// # Why a blacklist when [`PLAN_UNCERTAINTY_AXES`] is a whitelist
///
/// The polarity is opposite on purpose, because the dangerous direction is
/// opposite. A new *uncertainty axis* is rust-only by default — whitelisting
/// means it cannot silently become every channel's problem. A new *test type*
/// is meant to be replayed by everyone — blacklisting means a new one reaches
/// the plan and fails loudly in the channels that cannot yet run it, instead of
/// silently vanishing from the plan and taking a whole test axis with it. That
/// silent-axis-deletion is the exact defect this branch exists to kill.
pub const PLAN_RUST_ONLY_TEST_TYPES: [&str; 1] = [test_types::ORBIT_DETERMINATION_RADAR];

/// Why a channel-result row was excluded from the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanExclusion {
    /// Row sits on a rust-only uncertainty axis ([`PLAN_UNCERTAINTY_AXES`]).
    UncertaintyAxis,
    /// Row is a rust-only test type ([`PLAN_RUST_ONLY_TEST_TYPES`]).
    RustOnlyTestType,
}

/// Rows [`strip_to_plan`] dropped, counted by reason.
///
/// Counted rather than merely discarded so the caller can print *what* it
/// removed and *why* — a strip that silently shrinks the plan is
/// indistinguishable from a strip that ate a live axis.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlanDrops {
    /// Rows dropped by [`PLAN_UNCERTAINTY_AXES`].
    pub uncertainty_axis: usize,
    /// Rows dropped by [`PLAN_RUST_ONLY_TEST_TYPES`].
    pub rust_only_test_type: usize,
}

impl PlanDrops {
    /// Total rows dropped, across every reason.
    pub fn total(&self) -> usize {
        self.uncertainty_axis + self.rust_only_test_type
    }
}

impl std::fmt::Display for PlanDrops {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} rust-only uncertainty-axis + {} rust-only test-type rows dropped",
            self.uncertainty_axis, self.rust_only_test_type
        )
    }
}

/// Why the plan must not carry this row, or `None` if it belongs in the plan.
///
/// One predicate for both exclusion rules so the strip has a single decision
/// point: a new rule is a new arm here plus a new [`PlanDrops`] counter, never a
/// second filtering pass somewhere else in the pipeline.
fn plan_exclusion(row: &serde_json::Map<String, serde_json::Value>) -> Option<PlanExclusion> {
    let on_plan_axis = match row.get("propagation_uncertainty") {
        None | Some(serde_json::Value::Null) => true,
        Some(serde_json::Value::String(s)) => PLAN_UNCERTAINTY_AXES.contains(&s.as_str()),
        Some(_) => false,
    };
    if !on_plan_axis {
        return Some(PlanExclusion::UncertaintyAxis);
    }
    if let Some(serde_json::Value::String(tt)) = row.get("test_type")
        && PLAN_RUST_ONLY_TEST_TYPES.contains(&tt.as_str())
    {
        return Some(PlanExclusion::RustOnlyTestType);
    }
    None
}

/// Reduce one channel-result row to the plan contract: carried keys keep their
/// values, cleared keys become `null`, everything else is popped.
fn strip_row_to_plan_contract(
    row: &serde_json::Map<String, serde_json::Value>,
) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for key in PLAN_CARRIED_KEYS {
        // A carried key absent from the input stays absent rather than
        // materialising as null — the input row is the authority on what it
        // measured, and a fabricated key is the failure mode this whole
        // function exists to prevent.
        if let Some(v) = row.get(key) {
            out.insert(key.to_string(), v.clone());
        }
    }
    for key in PLAN_CLEARED_KEYS {
        out.insert(key.to_string(), serde_json::Value::Null);
    }
    out.insert(
        "channel".to_string(),
        serde_json::Value::String(channels::PLAN.to_string()),
    );
    serde_json::Value::Object(out)
}

/// Strip a channel-result JSON array down to the canonical plan.
///
/// Returns the plan rows and a [`PlanDrops`] breakdown of everything the strip
/// removed, by reason.
pub fn strip_to_plan(
    rows: &[serde_json::Value],
) -> Result<(Vec<serde_json::Value>, PlanDrops), String> {
    let mut out = Vec::with_capacity(rows.len());
    let mut drops = PlanDrops::default();
    for (i, row) in rows.iter().enumerate() {
        let obj = row
            .as_object()
            .ok_or_else(|| format!("row {i} is not a JSON object"))?;
        match plan_exclusion(obj) {
            Some(PlanExclusion::UncertaintyAxis) => {
                drops.uncertainty_axis += 1;
                continue;
            }
            Some(PlanExclusion::RustOnlyTestType) => {
                drops.rust_only_test_type += 1;
                continue;
            }
            None => {}
        }
        out.push(strip_row_to_plan_contract(obj));
    }
    Ok((out, drops))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog;

    #[test]
    fn self_perturber_naif_ids_for_pallas() {
        let pallas = catalog::filter_by_name(&["Pallas"]);
        assert_eq!(pallas.len(), 1);
        // Pallas is asteroid 2 → NAIF 2_000_002.
        assert_eq!(self_perturber_naif_ids(pallas[0]), vec![2_000_002]);
    }

    #[test]
    fn self_perturber_naif_ids_for_iris() {
        let iris = catalog::filter_by_name(&["Iris"]);
        assert_eq!(iris.len(), 1);
        // Iris is asteroid 7 → NAIF 2_000_007.
        assert_eq!(self_perturber_naif_ids(iris[0]), vec![2_000_007]);
    }

    #[test]
    fn self_perturber_naif_ids_empty_for_neo() {
        let apophis = catalog::filter_by_name(&["Apophis"]);
        assert_eq!(apophis.len(), 1);
        assert!(self_perturber_naif_ids(apophis[0]).is_empty());
    }

    #[test]
    fn plan_config_default_emits_both_uncertainty_modes() {
        let cfg = PlanConfig::default();
        assert_eq!(cfg.tiers, vec!["standard".to_string()]);
        assert!(cfg.uncertainty_axis);
    }

    #[test]
    fn propagation_plan_row_carries_test_identity_and_ic() {
        let obj = catalog::filter_by_name(&["Apophis"])[0];
        let r = propagation_plan_row(
            obj,
            61000.0,
            30.0,
            "standard",
            [1.0, 0.0, 0.0],
            [0.0, 0.017, 0.0],
            (5e-13, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, None),
            [1.0, 0.0, 0.0],
            [0.0, 0.017, 0.0],
            Some(([-0.004, 0.0, 0.0], [0.0, 1e-6, 0.0])),
            Some(uncertainty_modes::F64_NO_COV),
            "2026-04-29T00:00:00Z",
        );
        assert_eq!(r.ref_sun_pos_au, Some([-0.004, 0.0, 0.0]));
        assert_eq!(r.ref_sun_vel_au_d, Some([0.0, 1e-6, 0.0]));
        assert_eq!(r.object, "Apophis");
        assert_eq!(r.population, "NEO");
        assert_eq!(r.test_type, "propagation");
        assert_eq!(r.channel, "plan");
        assert_eq!(r.t_mjd_tdb, 61030.0);
        assert!(r.emp_pos_au.is_none());
        assert_eq!(r.propagation_uncertainty.as_deref(), Some("f64_no_cov"),);
    }

    #[test]
    fn od_plan_row_marks_self_perturbers() {
        let pallas = catalog::filter_by_name(&["Pallas"])[0];
        let r = od_plan_row(pallas, "standard", "ts");
        assert_eq!(r.test_type, "orbit_determination");
        assert_eq!(r.excluded_perturbers_naif, vec![2_000_002]);
    }

    // ── Plan contract ───────────────────────────────────────────────
    //
    // The pinned consumer's schema, reconstructed as a deserialization
    // target. `empyrean-core` depends on `empyrean-validation` at tag v0.7.0,
    // whose `ValidationResult` is `#[serde(deny_unknown_fields)]` over
    // exactly these 70 keys — so this struct accepts precisely what that
    // consumer accepts and rejects precisely what it rejects. Verified
    // against `git show v0.7.0:src/schema.rs`.
    //
    // Every field is `Option<Value>` + `#[serde(default)]` on purpose: this
    // models the KEY contract, which is the axis `deny_unknown_fields`
    // enforces. Value shapes are the live schema's business.
    macro_rules! pinned_schema {
        ($name:ident { $($field:ident),* $(,)? }) => {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            #[allow(dead_code)]
            struct $name {
                $( #[serde(default)] $field: Option<serde_json::Value>, )*
            }
        };
    }

    pinned_schema!(PinnedV070Result {
        object,
        population,
        epoch_mjd_tdb,
        dt_days,
        t_mjd_tdb,
        force_model,
        test_type,
        channel,
        observer,
        emp_vs_horizons_km,
        emp_pos_au,
        emp_time_ms,
        separation_arcsec,
        d_ra_arcsec,
        d_dec_arcsec,
        d_rho_km,
        d_light_time_s,
        ic_pos_au,
        ic_vel_au_d,
        ic_a1,
        ic_a2,
        ic_a3,
        ic_g_alpha,
        ic_g_r0,
        ic_g_m,
        ic_g_n,
        ic_g_k,
        ic_non_grav_dt,
        ref_pos_au,
        ref_vel_au_d,
        ref_ra_rad,
        ref_dec_rad,
        ref_rho_au,
        ref_light_time_d,
        n_obs_used,
        od_iterations,
        od_converged,
        od_rms_ra_arcsec,
        od_rms_dec_arcsec,
        od_rms_combined_arcsec,
        od_chi2,
        od_reduced_chi2,
        od_a1,
        od_a2,
        od_a3,
        od_a1_sigma,
        od_a2_sigma,
        od_a3_sigma,
        excluded_perturbers_naif,
        propagation_uncertainty,
        assist_vs_horizons_km,
        emp_vs_assist_km,
        assist_time_ms,
        speed_ratio,
        findorb_rms_residual,
        findorb_n_obs_used,
        findorb_n_obs_rejected,
        oorb_vs_horizons_km,
        emp_vs_oorb_km,
        oorb_time_ms,
        oorb_separation_arcsec,
        oorb_d_ra_arcsec,
        oorb_d_dec_arcsec,
        oorb_d_rho_km,
        orbfit_rms_arcsec,
        orbfit_n_obs_used,
        orbfit_n_obs_rejected,
        orbfit_time_ms,
        timestamp,
        notes,
    });

    /// A rust-channel result row carrying every current-schema field plus
    /// fields no schema has ever heard of.
    fn rust_row_with_unknown_fields(uncertainty: Option<&str>) -> serde_json::Value {
        let mut row = serde_json::to_value(ValidationResult {
            propagation_uncertainty: uncertainty.map(str::to_string),
            // Fields that exist today but NOT at v0.7.0 — the ones that
            // actually broke the core channel.
            emp_pos_cov_au2: Some([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]),
            emp_radec_cov_arcsec2: Some([[1.0, 0.0], [0.0, 1.0]]),
            source_version: Some("empyrean-core 0.9.2".into()),
            ref_sun_pos_au: Some([1.0, 2.0, 3.0]),
            layup_chi2: Some(1.0),
            orbfit_error: Some("boom".into()),
            object: "Apophis".into(),
            population: "NEO".into(),
            test_type: test_types::PROPAGATION.into(),
            channel: channels::RUST.into(),
            force_model: "standard".into(),
            ic_pos_au: Some([1.0, 2.0, 3.0]),
            ref_pos_au: Some([1.0, 2.0, 3.0]),
            emp_pos_au: Some([9.0, 9.0, 9.0]),
            emp_time_ms: Some(42.0),
            ..ValidationResult::empty()
        })
        .unwrap();
        // …and fields from a hypothetical future runner, which is the case
        // the whitelist is really insuring against.
        let obj = row.as_object_mut().unwrap();
        obj.insert("emp_delta_v_budget_m_s".into(), serde_json::json!(7.0));
        obj.insert("some_future_field".into(), serde_json::json!({"a": 1}));
        row
    }

    /// The same row, retagged to a given test type and off the uncertainty
    /// axis entirely, so the test-type rule is exercised in isolation.
    fn rust_row_with_test_type(test_type: &str) -> serde_json::Value {
        let mut row = rust_row_with_unknown_fields(None);
        row.as_object_mut()
            .unwrap()
            .insert("test_type".into(), serde_json::json!(test_type));
        row
    }

    #[test]
    fn plan_from_rows_with_unknown_fields_deserializes_against_the_pinned_schema() {
        let rows = vec![rust_row_with_unknown_fields(Some(
            uncertainty_modes::F64_NO_COV,
        ))];
        let (plan, drops) = strip_to_plan(&rows).expect("strip");
        assert_eq!(drops.total(), 0);
        assert_eq!(plan.len(), 1);

        // The whole point: a v0.7.0 consumer can read this.
        let json = serde_json::to_string(&plan[0]).unwrap();
        serde_json::from_str::<PinnedV070Result>(&json).unwrap_or_else(|e| {
            panic!("plan row rejected by the pinned v0.7.0 schema: {e}\n{json}")
        });

        // …and so can the current one, which is stricter about value shapes.
        serde_json::from_str::<ValidationResult>(&json)
            .unwrap_or_else(|e| panic!("plan row rejected by the current schema: {e}\n{json}"));
    }

    #[test]
    fn strip_pops_unknown_keys_rather_than_nulling_them() {
        // The defect this replaces: the old strip set `field: None`, and
        // `deny_unknown_fields` rejects on key PRESENCE, so nulling could
        // never have fixed deserialization. Presence is the assertion.
        let rows = vec![rust_row_with_unknown_fields(None)];
        let (plan, _) = strip_to_plan(&rows).unwrap();
        let obj = plan[0].as_object().unwrap();
        for leaked in [
            "emp_pos_cov_au2",
            "emp_radec_cov_arcsec2",
            "source_version",
            "layup_chi2",
            "orbfit_error",
            "some_future_field",
            "emp_delta_v_budget_m_s",
        ] {
            assert!(
                !obj.contains_key(leaked),
                "{leaked} must be POPPED, not present-and-null"
            );
        }
        // Cleared keys are the exception — present, and null.
        for cleared in PLAN_CLEARED_KEYS {
            assert_eq!(
                obj.get(cleared),
                Some(&serde_json::Value::Null),
                "{cleared} must be present and null"
            );
        }
        // Carried keys keep their values; the channel tag is rewritten.
        assert_eq!(obj["object"], serde_json::json!("Apophis"));
        assert_eq!(obj["ic_pos_au"], serde_json::json!([1.0, 2.0, 3.0]));
        assert_eq!(obj["channel"], serde_json::json!(channels::PLAN));
    }

    #[test]
    fn cleared_keys_cover_every_key_the_schema_demands_be_present() {
        // Both the current schema and the pinned v0.7.0 one leave 50 fields
        // without `#[serde(default)]`, so a plan that popped one would make
        // every derived channel output fail to deserialize on a MISSING key.
        // Carried ∪ cleared must therefore cover them. This is what stops a
        // future tidy-up of the cleared list from breaking the other
        // direction.
        let (plan, _) = strip_to_plan(&[rust_row_with_unknown_fields(None)]).unwrap();
        let json = serde_json::to_string(&plan[0]).unwrap();
        serde_json::from_str::<ValidationResult>(&json).expect("no required key was popped");
    }

    #[test]
    fn rust_only_uncertainty_axes_never_reach_the_plan() {
        // sigma_point and monte_carlo rode into the "plan" under the old
        // blacklist, which named only `auto` and `second_order_with_cov`.
        for axis in [
            "auto",
            "second_order_with_cov",
            "sigma_point_with_cov",
            "monte_carlo_100_with_cov",
        ] {
            let (plan, drops) = strip_to_plan(&[rust_row_with_unknown_fields(Some(axis))]).unwrap();
            assert!(plan.is_empty(), "{axis} leaked into the plan");
            assert_eq!(drops.uncertainty_axis, 1);
            assert_eq!(drops.rust_only_test_type, 0);
        }
        for axis in PLAN_UNCERTAINTY_AXES {
            let (plan, drops) = strip_to_plan(&[rust_row_with_unknown_fields(Some(axis))]).unwrap();
            assert_eq!(plan.len(), 1, "{axis} should be replayable");
            assert_eq!(drops.total(), 0);
        }
        // OD rows carry no uncertainty tag and are always in the plan.
        let (plan, drops) = strip_to_plan(&[rust_row_with_unknown_fields(None)]).unwrap();
        assert_eq!((plan.len(), drops.total()), (1, 0));
    }

    #[test]
    fn rust_only_test_types_never_reach_the_plan() {
        // The radar OD rows the rust runner emits are unreplayable by every
        // other channel (empyrean-s1ab); a plan carrying them makes the gate
        // unsatisfiable. They must be dropped, and dropped under their OWN
        // counter so the strip's log says which rule removed them.
        for tt in PLAN_RUST_ONLY_TEST_TYPES {
            let (plan, drops) = strip_to_plan(&[rust_row_with_test_type(tt)]).unwrap();
            assert!(plan.is_empty(), "{tt} leaked into the plan");
            assert_eq!(drops.rust_only_test_type, 1);
            assert_eq!(drops.uncertainty_axis, 0);
        }
        // Every other test type still rides through. Pinned by name so adding
        // a test type to the blacklist is a deliberate, visible act — the
        // whole hazard of a plan strip is that it deletes an axis quietly.
        for tt in [
            test_types::PROPAGATION,
            test_types::EPHEMERIS,
            test_types::ORBIT_DETERMINATION,
            test_types::NON_GRAV_RECOVERY,
            test_types::DT_RECOVERY,
            test_types::PHOTOMETRY_RECOVERY,
            test_types::THRUST_RECOVERY,
        ] {
            let (plan, drops) = strip_to_plan(&[rust_row_with_test_type(tt)]).unwrap();
            assert_eq!(plan.len(), 1, "{tt} should reach the plan");
            assert_eq!(drops.total(), 0, "{tt} was dropped");
        }
    }

    #[test]
    fn strip_counts_each_drop_reason_separately() {
        let rows = vec![
            rust_row_with_unknown_fields(Some("monte_carlo_100_with_cov")),
            rust_row_with_unknown_fields(Some("auto")),
            rust_row_with_test_type(test_types::ORBIT_DETERMINATION_RADAR),
            rust_row_with_test_type(test_types::ORBIT_DETERMINATION),
        ];
        let (plan, drops) = strip_to_plan(&rows).unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(drops.uncertainty_axis, 2);
        assert_eq!(drops.rust_only_test_type, 1);
        assert_eq!(drops.total(), 3);
    }

    #[test]
    fn plan_contract_is_a_subset_of_the_pinned_schema() {
        // Belt and braces on the whitelist itself: a key added to
        // PLAN_CARRIED_KEYS that the pinned consumer does not know about
        // would reintroduce the original bug, and would only show up in the
        // deserialization test if some row happened to populate it.
        let mut row = serde_json::Map::new();
        for k in PLAN_CARRIED_KEYS.iter().chain(PLAN_CLEARED_KEYS.iter()) {
            row.insert((*k).to_string(), serde_json::Value::Null);
        }
        let json = serde_json::to_string(&serde_json::Value::Object(row)).unwrap();
        serde_json::from_str::<PinnedV070Result>(&json).unwrap_or_else(|e| {
            panic!("a plan-contract key is unknown to the pinned v0.7.0 schema: {e}")
        });
    }
}
