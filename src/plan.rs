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
    let obs_code = OBSERVER_CODES[0];
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
        }

        let mut horizons_ephemeris: HashMap<i64, EphemerisEntry> = HashMap::new();
        for &dt in dt_list {
            if dt == 0.0 {
                continue;
            }
            let target = epoch + dt;
            match empyrean::query_horizons(
                &[obj.horizons_command],
                obs_code,
                &[target],
                Some(horizons_cache_dir),
            ) {
                Ok(r) if !r.is_empty() => {
                    horizons_ephemeris.insert(dt as i64, r.into_iter().next().unwrap());
                }
                Ok(_) => {
                    eprintln!("  {}: dt={dt:+.0}d ephemeris SKIP (empty)", obj.name);
                }
                Err(e) => {
                    eprintln!("  {}: dt={dt:+.0}d ephemeris SKIP ({e})", obj.name);
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
                        uncertainty,
                        &timestamp,
                    ));
                }
            }
        }

        // 4b. Ephemeris plan rows (only the Standard tier — matches the
        // existing rust runner's behaviour where ephemeris validation
        // doesn't iterate force-model tiers).
        for &dt in dt_list {
            if dt == 0.0 {
                continue;
            }
            let Some(hor) = horizons_ephemeris.get(&(dt as i64)) else {
                continue;
            };
            for &uncertainty in uncertainty_modes_to_emit {
                plan.push(ephemeris_plan_row(
                    obj,
                    epoch,
                    dt,
                    obs_code,
                    ic_pos,
                    ic_vel,
                    (a1, a2, a3, g_alpha, g_r0, g_m, g_n, g_k, ng_dt),
                    hor,
                    uncertainty,
                    &timestamp,
                ));
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
            Some(uncertainty_modes::F64_NO_COV),
            "2026-04-29T00:00:00Z",
        );
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
}
