//! Validation runner: propagate orbits and compare to JPL Horizons.
//!
//! Channel: rust, going through the empyrean safe wrapper (same C ABI
//! that python / c / cli ride) — so Section 09 fidelity diffs reflect
//! only binding-translation drift, not propagator-path drift.

use std::collections::HashMap;
use std::time::Instant;

use rayon::prelude::*;
use villeneuve::io::cache::DiskCache;
use villeneuve::io::jpl::horizons::HorizonsRecord;

use empyrean::{
    Context, CoordinateState, EphemerisConfig, Epoch, ForceModelTier, Frame, ODConfig, Orbit,
    Origin, PropagationConfig, Representation, UncertaintyMethod,
};
use empyrean_validation::catalog::{DEFAULT_DT_DAYS, FORCE_MODEL_TIERS, ValidationObject};
use empyrean_validation::compare;
use empyrean_validation::orbit_compare::compare_orbits;
use empyrean_validation::schema::{
    CapturedOrbit, OrbitComparison, ValidationResult, orbit_sources,
};

/// Runner config.
pub struct ValidateConfig {
    pub tiers: Vec<String>,
    pub n_timing_runs: usize,
    /// Attach a synthetic Cartesian covariance to every input orbit so
    /// the propagator dispatches to Jet1 (STM-bearing) integration —
    /// the production hot path. Set to false to drop covariance and
    /// exercise the f64-only path (useful for head-to-head timing vs
    /// external propagators that don't propagate uncertainty).
    pub attach_covariance: bool,
}

impl Default for ValidateConfig {
    fn default() -> Self {
        Self {
            tiers: FORCE_MODEL_TIERS.iter().map(|s| s.to_string()).collect(),
            n_timing_runs: 3,
            attach_covariance: true,
        }
    }
}

fn tier_from_str(s: &str) -> ForceModelTier {
    match s {
        "approximate" => ForceModelTier::Approximate,
        "basic" => ForceModelTier::Basic,
        // Standard is the v0.7.0 facade default. "Full" was the legacy
        // default but is excluded in v0.7.0; map it to Standard.
        _ => ForceModelTier::Standard,
    }
}

/// Run propagation + ephemeris validation against Horizons reference.
pub fn run_propagation_validation(
    ctx: &Context,
    objs: &[&ValidationObject],
    config: &ValidateConfig,
    horizons_cache: &mut DiskCache,
    sbdb_cache: &mut DiskCache,
    num_threads: Option<usize>,
) -> Vec<ValidationResult> {
    let timestamp = chrono::Utc::now().to_rfc3339();
    let channel = "rust".to_string();

    eprintln!("Fetching initial conditions...");

    struct ObjData {
        name: String,
        population: String,
        notes: String,
        epoch: f64,
        ic_pos: [f64; 3],
        ic_vel: [f64; 3],
        a1: f64,
        a2: f64,
        a3: f64,
        ng_alpha: f64,
        ng_r0: f64,
        ng_m: f64,
        ng_n: f64,
        ng_k: f64,
        ng_dt: Option<f64>,
        dt_list: &'static [f64],
        horizons_vectors: HashMap<i64, ([f64; 3], [f64; 3])>,
        horizons_ephemeris: HashMap<i64, HorizonsRecord>,
    }

    let obs_codes = empyrean_validation::catalog::OBSERVER_CODES;
    let obs_code = obs_codes[0];
    let mut obj_data: Vec<ObjData> = Vec::new();

    for obj in objs {
        let sbdb = match villeneuve::io::jpl::sbdb::query_sbdb(&[obj.sbdb_query], Some(sbdb_cache))
        {
            Ok(o) => o,
            Err(e) => {
                eprintln!("  {}: SKIP (SBDB: {e})", obj.name);
                continue;
            }
        };
        let epoch = sbdb.coordinates()[0].time().mjd_tdb();

        let (hor_pos, hor_vel) = match villeneuve::io::jpl::horizons::query_horizons_vectors(
            obj.horizons_command,
            epoch,
            Some(horizons_cache),
        ) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("  {}: SKIP (Horizons IC: {e})", obj.name);
                continue;
            }
        };

        // Extract Marsden non-grav from SBDB and pass through with the
        // explicit g(r) parameters so the C ABI builds the correct
        // model. SBDB defaults to inverse_square for asteroids and
        // water-ice for comets. `dt` is the SBDB time-delay (days)
        // applied to g(r) — non-zero for Jupiter-family comets and
        // some interstellar objects (67P=+45.7d, 2I/Borisov=−65.1d).
        let (a1, a2, a3, ng_alpha, ng_r0, ng_m, ng_n, ng_k, ng_dt) = match sbdb.non_grav_params(0) {
            Some(ng) => {
                let g = match &ng.model {
                    villeneuve::dynamics::forces::non_gravitational::NonGravModel::MarsdenSekanina(g) => g.clone(),
                    _ => villeneuve::dynamics::forces::non_gravitational::GFunction::inverse_square(),
                };
                (ng.a1, ng.a2, ng.a3, g.alpha, g.r0, g.m, g.n, g.k, ng.dt)
            }
            None => (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, None),
        };

        eprintln!(
            "  {}: epoch={:.1} MJD TDB (Horizons IC, a1={:.2e}, dt={:?})",
            obj.name, epoch, a1, ng_dt
        );

        let dt_list = obj.dt_days.unwrap_or(DEFAULT_DT_DAYS);
        let mut horizons_vectors: HashMap<i64, ([f64; 3], [f64; 3])> = HashMap::new();
        for &dt in dt_list {
            let target = epoch + dt;
            match villeneuve::io::jpl::horizons::query_horizons_vectors(
                obj.horizons_command,
                target,
                Some(horizons_cache),
            ) {
                Ok(h) => {
                    horizons_vectors.insert(dt as i64, h);
                }
                Err(e) => {
                    eprintln!("  {}: dt={dt:+.0}d Horizons SKIP ({e})", obj.name);
                }
            }
        }

        let mut horizons_ephemeris: HashMap<i64, HorizonsRecord> = HashMap::new();
        for &dt in dt_list {
            if dt == 0.0 {
                continue;
            }
            let target = epoch + dt;
            match villeneuve::io::jpl::horizons::query_horizons(
                &[obj.horizons_command],
                obs_code,
                &[target],
                Some(horizons_cache),
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

        obj_data.push(ObjData {
            name: obj.name.to_string(),
            population: obj.population.to_string(),
            notes: obj.notes.to_string(),
            epoch,
            ic_pos: hor_pos,
            ic_vel: hor_vel,
            a1,
            a2,
            a3,
            ng_alpha,
            ng_r0,
            ng_m,
            ng_n,
            ng_k,
            ng_dt,
            dt_list,
            horizons_vectors,
            horizons_ephemeris,
        });
    }

    eprintln!();
    eprintln!(
        "Running propagation + ephemeris validation ({} objects, {} threads)...",
        obj_data.len(),
        num_threads.unwrap_or(0),
    );

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(num_threads.unwrap_or(0))
        .build()
        .expect("failed to build thread pool");

    // Uncertainty axis. When config.attach_covariance is true (default),
    // every (object, dt, tier) row is propagated FOUR times so the
    // report can compare timing + accuracy across empyrean's
    // production-relevant uncertainty surfaces head-to-head:
    //
    //   - "f64_no_cov"             — single particle, no STM. Pairs
    //                                 with ASSIST single-particle.
    //   - "first_order_with_cov"   — Jet1 STM + 6×6 covariance. Pairs
    //                                 with ASSIST 6 first-order
    //                                 variational particles (28 dual
    //                                 numbers per state component on
    //                                 either side).
    //   - "second_order_with_cov"  — Jet2 STM+STT (6 + 21 partials).
    //                                 Pairs with ASSIST 6 first-order
    //                                 + 21 second-order variational
    //                                 particles (28 + 84 = 112 dual
    //                                 numbers per state).
    //   - "auto"                   — UncertaintyMethod::Auto: villeneuve
    //                                 v1.10.0 Phase A/B/C cascade
    //                                 (FirstOrder / SecondOrder / AGM
    //                                 mixture, driven by per-CA κ and
    //                                 IP-skip thresholds). No REBOUND
    //                                 cascade analogue; baselines
    //                                 against ASSIST's STM row in the
    //                                 report via the merge step's
    //                                 translation table.
    //
    // When attach_covariance is false, only the f64 row is emitted
    // (benchmark mode for head-to-head comparison with external
    // propagators that don't propagate covariance).
    struct UncertaintyAxis {
        tag: &'static str,
        attach: bool,
        method: UncertaintyMethod,
    }
    let modes: Vec<UncertaintyAxis> = if config.attach_covariance {
        vec![
            UncertaintyAxis {
                tag: "first_order_with_cov",
                attach: true,
                method: UncertaintyMethod::FirstOrder,
            },
            UncertaintyAxis {
                tag: "f64_no_cov",
                attach: false,
                method: UncertaintyMethod::FirstOrder,
            },
            UncertaintyAxis {
                tag: "second_order_with_cov",
                attach: true,
                method: UncertaintyMethod::SecondOrder,
            },
            UncertaintyAxis {
                tag: "auto",
                attach: true,
                method: UncertaintyMethod::auto(),
            },
        ]
    } else {
        vec![UncertaintyAxis {
            tag: "f64_no_cov",
            attach: false,
            method: UncertaintyMethod::FirstOrder,
        }]
    };

    let all_results: Vec<Vec<ValidationResult>> = pool.install(|| {
        obj_data
            .par_iter()
            .map(|data| {
                let mut results: Vec<ValidationResult> = Vec::new();

                for axis in &modes {
                // Synthetic typical-NEO 6×6 Cartesian covariance:
                //   1 km position σ, 1 mm/s velocity σ (uncorrelated).
                //
                // This is what triggers empyrean's Jet1 / Auto STM
                // dispatch in the propagator — empyrean is uncertainty-
                // first, so any orbit with a covariance attached
                // propagates STM by default. Numbers are placeholder
                // physical scales; covariance-accuracy validation
                // against Monte Carlo is a separate (future) test.
                let covariance = if axis.attach {
                    let pos_var_au = (1.0 / 149_597_870.700_f64).powi(2);
                    let vel_var_au_d = (1e-6 / 149_597_870.700_f64 * 86_400.0).powi(2);
                    let mut c = [[0.0_f64; 6]; 6];
                    c[0][0] = pos_var_au;
                    c[1][1] = pos_var_au;
                    c[2][2] = pos_var_au;
                    c[3][3] = vel_var_au_d;
                    c[4][4] = vel_var_au_d;
                    c[5][5] = vel_var_au_d;
                    Some(c)
                } else {
                    None
                };
                let uncertainty_tag = axis.tag;
                let state = CoordinateState {
                    epoch: Epoch::from_mjd_tdb(data.epoch),
                    elements: [
                        data.ic_pos[0],
                        data.ic_pos[1],
                        data.ic_pos[2],
                        data.ic_vel[0],
                        data.ic_vel[1],
                        data.ic_vel[2],
                    ],
                    covariance,
                    representation: Representation::Cartesian,
                    frame: Frame::ICRF,
                    origin: Origin::SSB,
                };
                let mut orbit = Orbit::new(state);
                if data.a1 != 0.0 || data.a2 != 0.0 || data.a3 != 0.0 {
                    orbit = orbit
                        .with_nongrav(data.a1, data.a2, data.a3)
                        .with_g_function(
                            data.ng_alpha,
                            data.ng_r0,
                            data.ng_m,
                            data.ng_n,
                            data.ng_k,
                        )
                        .with_non_grav_dt(data.ng_dt);
                }

                for tier_str in &config.tiers {
                    let tier = tier_from_str(tier_str);

                    for &dt in data.dt_list {
                        let Some(&hor) = data.horizons_vectors.get(&(dt as i64)) else {
                            continue;
                        };
                        let target = Epoch::from_mjd_tdb(data.epoch + dt);

                        let prop_config = PropagationConfig {
                            force_model: tier,
                            uncertainty_method: axis.method.clone(),
                            frame: Frame::ICRF,
                            ..PropagationConfig::default()
                        };

                        let mut emp_times = Vec::new();
                        let mut emp_state: Option<[f64; 3]> = None;
                        let mut failed = false;

                        for _ in 0..config.n_timing_runs {
                            let t0 = Instant::now();
                            match ctx.propagate(&[orbit.clone()], &[target], &prop_config) {
                                Ok(result) => {
                                    let ms = t0.elapsed().as_secs_f64() * 1000.0;
                                    emp_times.push(ms);
                                    if !result.states.is_empty() {
                                        emp_state = Some(result.states[0].position);
                                    } else {
                                        eprintln!(
                                            "  {} {tier_str} dt={dt:+.0}d {} empyrean Ok but states.len()=0 (likely AGM mixture-only return; skipping row)",
                                            data.name, axis.tag,
                                        );
                                    }
                                }
                                Err(e) => {
                                    eprintln!(
                                        "  {} {tier_str} dt={dt:+.0}d {} empyrean FAIL ({e})",
                                        data.name, axis.tag,
                                    );
                                    failed = true;
                                    break;
                                }
                            }
                        }
                        if failed || emp_state.is_none() {
                            continue;
                        }
                        let emp_pos = emp_state.unwrap();
                        let emp_ms = emp_times.iter().copied().fold(f64::INFINITY, f64::min);
                        let emp_vs_hor = compare::position_error_km(&emp_pos, &hor.0);

                        eprintln!(
                            "  {} {:>12} dt={:>+6.0}d  e-h={}  {:.1}ms",
                            data.name,
                            tier_str,
                            dt,
                            compare::fmt_km(emp_vs_hor),
                            emp_ms,
                        );

                        results.push(ValidationResult {
                            object: data.name.clone(),
                            population: data.population.clone(),
                            epoch_mjd_tdb: data.epoch,
                            dt_days: dt,
                            t_mjd_tdb: data.epoch + dt,
                            force_model: tier_str.clone(),
                            test_type: "propagation".to_string(),
                            channel: channel.clone(),
                            observer: None,
                            emp_vs_horizons_km: Some(emp_vs_hor),
                            emp_pos_au: Some(emp_pos),
                            emp_time_ms: Some(emp_ms),
                            separation_arcsec: None,
                            d_ra_arcsec: None,
                            d_dec_arcsec: None,
                            d_rho_km: None,
                            d_light_time_s: None,
                            ic_pos_au: Some(data.ic_pos),
                            ic_vel_au_d: Some(data.ic_vel),
                            ic_a1: Some(data.a1),
                            ic_a2: Some(data.a2),
                            ic_a3: Some(data.a3),
                            ic_g_alpha: Some(data.ng_alpha),
                            ic_g_r0: Some(data.ng_r0),
                            ic_g_m: Some(data.ng_m),
                            ic_g_n: Some(data.ng_n),
                            ic_g_k: Some(data.ng_k),
                            ic_non_grav_dt: data.ng_dt,
                            ref_pos_au: Some(hor.0),
                            ref_vel_au_d: Some(hor.1),
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
                            propagation_uncertainty: Some(uncertainty_tag.to_string()),
                            assist_vs_horizons_km: None,
                            emp_vs_assist_km: None,
                            assist_time_ms: None,
                            speed_ratio: None,
                            findorb_rms_residual: None,
                            findorb_n_obs_used: None,
                            findorb_n_obs_rejected: None,
                            // External-reference fields populated by merge-external:
                            // OpenOrb (prop + ephemeris) and OrbFit (OD).
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
                            timestamp: timestamp.clone(),
                            notes: data.notes.clone(),
                        });
                    }
                }

                // Ephemeris tests (Standard tier)
                for &dt in data.dt_list {
                    if dt == 0.0 {
                        continue;
                    }
                    let Some(hor) = data.horizons_ephemeris.get(&(dt as i64)) else {
                        continue;
                    };
                    let target = Epoch::from_mjd_tdb(data.epoch + dt);

                    let observers = match ctx.get_observers(&[obs_code], &[target]) {
                        Ok(o) => o,
                        Err(e) => {
                            eprintln!("  {} dt={dt:+.0}d SKIP (observer: {e})", data.name);
                            continue;
                        }
                    };
                    if observers.is_empty() {
                        continue;
                    }

                    let eph_config = EphemerisConfig::with_force_model(ForceModelTier::Standard);
                    match ctx.generate_ephemeris(&[orbit.clone()], &observers, &eph_config) {
                        Ok(entries) => {
                            let Some(entry) = entries.first() else {
                                continue;
                            };
                            // Wrapper returns degrees; Horizons radians.
                            let emp_ra_rad = entry.ra_deg.to_radians();
                            let emp_dec_rad = entry.dec_deg.to_radians();

                            let sep = compare::angular_separation_arcsec(
                                emp_ra_rad,
                                emp_dec_rad,
                                hor.ra,
                                hor.dec,
                            );
                            let d_ra = (emp_ra_rad - hor.ra) * emp_dec_rad.cos();
                            let d_dec = emp_dec_rad - hor.dec;
                            let d_ra_arcsec = d_ra.to_degrees() * 3600.0;
                            let d_dec_arcsec = d_dec.to_degrees() * 3600.0;

                            let d_rho_km = Some((entry.rho_au - hor.rho) * compare::AU_KM);
                            let d_lt_s = if entry.light_time_days.is_finite() {
                                hor.light_time
                                    .map(|h| (entry.light_time_days - h) * 86400.0)
                            } else {
                                None
                            };

                            eprintln!(
                                "  {} dt={dt:>+6.0}d  sep={sep:.1}mas  dRA={d_ra_arcsec:.1}mas  dDec={d_dec_arcsec:.1}mas",
                                data.name,
                            );

                            results.push(ValidationResult {
                                object: data.name.clone(),
                                population: data.population.clone(),
                                epoch_mjd_tdb: data.epoch,
                                dt_days: dt,
                                t_mjd_tdb: data.epoch + dt,
                                force_model: "standard".to_string(),
                                test_type: "ephemeris".to_string(),
                                channel: channel.clone(),
                                observer: Some(obs_code.to_string()),
                                emp_vs_horizons_km: None,
                                emp_pos_au: None,
                                emp_time_ms: None,
                                separation_arcsec: Some(sep),
                                d_ra_arcsec: Some(d_ra_arcsec),
                                d_dec_arcsec: Some(d_dec_arcsec),
                                d_rho_km,
                                d_light_time_s: d_lt_s,
                                ic_pos_au: Some(data.ic_pos),
                                ic_vel_au_d: Some(data.ic_vel),
                                ic_a1: Some(data.a1),
                                ic_a2: Some(data.a2),
                                ic_a3: Some(data.a3),
                                ic_g_alpha: Some(data.ng_alpha),
                                ic_g_r0: Some(data.ng_r0),
                                ic_g_m: Some(data.ng_m),
                                ic_g_n: Some(data.ng_n),
                                ic_g_k: Some(data.ng_k),
                                ic_non_grav_dt: data.ng_dt,
                                ref_pos_au: None,
                                ref_vel_au_d: None,
                                ref_ra_rad: Some(hor.ra),
                                ref_dec_rad: Some(hor.dec),
                                ref_rho_au: Some(hor.rho),
                                ref_light_time_d: hor.light_time,
                                n_obs_used: None,
                                od_iterations: None,
                                od_converged: None,
                                od_rms_ra_arcsec: None,
                                od_rms_dec_arcsec: None,
                                od_rms_combined_arcsec: None,
                                od_chi2: None,
                                od_reduced_chi2: None,
                                excluded_perturbers_naif: Vec::new(),
                                propagation_uncertainty: Some(uncertainty_tag.to_string()),
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
                                timestamp: timestamp.clone(),
                                notes: data.notes.clone(),
                            });
                        }
                        Err(e) => {
                            eprintln!("  {} dt={dt:+.0}d FAIL ({e})", data.name);
                        }
                    }
                }
                } // end for &attach

                results
            })
            .collect()
    });

    all_results.into_iter().flatten().collect()
}

/// Result of [`run_od_validation`] — the existing per-row metric records
/// plus a per-object orbit + covariance sidecar and the bidirectional
/// orbit comparisons used by the orbit-comparison panel.
pub struct OdValidationOutput {
    /// Per-row metric records (existing `validation_rust_od.json` shape).
    pub results: Vec<ValidationResult>,
    /// Per-object fitted orbit + covariance, in native + ICRF-Cartesian
    /// + ecliptic-Keplerian representations. Includes both the
    ///   at-native-epoch records and any propagated records used as inputs
    ///   to the comparison kernel.
    pub captured_orbits: Vec<CapturedOrbit>,
    /// Bidirectional comparisons: per (scott_od, reference) pair, one
    /// comparison at the scott epoch and one at the reference epoch.
    pub orbit_comparisons: Vec<OrbitComparison>,
}

/// Run orbit-determination validation: load PSV from
/// `validation/fixtures/psv/{name}.psv`, run `ctx.determine`, emit one
/// `ValidationResult` row per object with `test_type =
/// "orbit_determination"`. The fitted Cartesian state at the OD epoch
/// is stored in `emp_pos_au` so Section 09 can compute cross-channel
/// fidelity for OD the same way it does for propagation.
///
/// Also emits [`CapturedOrbit`] sidecar records for the
/// orbit-comparison panel:
/// - one per successful scott fit (`source = "scott_od"`), and
/// - one per object that has a JPL SBDB entry (`source = "sbdb"`).
///
/// The comparison kernel pairs these by `object` to produce the
/// per-object Δstate + Mahalanobis distances + σ ratios shown in the
/// "Fitted orbit + covariance vs references" report panel.
pub fn run_od_validation(
    ctx: &Context,
    objs: &[&ValidationObject],
    fixtures_dir: &std::path::Path,
    max_iterations: u32,
    tier: ForceModelTier,
    sbdb_cache_dir: Option<&std::path::Path>,
) -> OdValidationOutput {
    let timestamp = chrono::Utc::now().to_rfc3339();
    let channel = "rust".to_string();
    let tier_str = match tier {
        ForceModelTier::Approximate => "approximate",
        ForceModelTier::Basic => "basic",
        ForceModelTier::Standard => "standard",
    }
    .to_string();

    // Parallelize across catalog objects. Each fit is independent (no
    // shared mutable state in scott's determine pipeline); empyrean::Context
    // is Send + Sync (see empyrean/src/context.rs:22-23 — "concurrent
    // propagation calls are safe") so the same `&ctx` is safely shared
    // across Rayon workers. Per-object output is collected into a Vec of
    // (results, captured_orbits, orbit_comparisons) triples then flattened
    // back into the top-level accumulators after `.collect()`.
    //
    // Per-object stdout interleaves under parallel execution; every line
    // includes the object name so the log stays readable. Per-call
    // `emp_time_ms` is wall-clock cost of one ctx.determine() call (under
    // N-way contention with concurrent fits) — cross-channel comparability
    // is preserved because every channel measures the same per-call shape.
    let per_object: Vec<(Vec<ValidationResult>, Vec<CapturedOrbit>, Vec<OrbitComparison>)> =
        objs.par_iter().map(|obj| {
            let mut results: Vec<ValidationResult> = Vec::new();
            let mut captured_orbits: Vec<CapturedOrbit> = Vec::new();
            let mut orbit_comparisons: Vec<OrbitComparison> = Vec::new();
        // Try common PSV filename variants.
        let candidates = [
            fixtures_dir.join(format!("{}.psv", obj.name)),
            fixtures_dir.join(format!("{}.psv", obj.mpc_designation)),
        ];
        let path = candidates.iter().find(|p| p.exists());
        let Some(path) = path else {
            eprintln!(
                "  {}: SKIP (no PSV at {})",
                obj.name,
                candidates[0].display()
            );
            return (results, captured_orbits, orbit_comparisons);
        };

        let psv = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("  {}: SKIP (read PSV: {e})", obj.name);
                return (results, captured_orbits, orbit_comparisons);
            }
        };

        let observations = match ctx.read_ades(&psv) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("  {}: SKIP (parse PSV: {e})", obj.name);
                return (results, captured_orbits, orbit_comparisons);
            }
        };

        if observations.is_empty() {
            eprintln!("  {}: SKIP (zero observations)", obj.name);
            return (results, captured_orbits, orbit_comparisons);
        }

        let n_obs = observations.len();
        // SB441-N16 self-perturbers: exclude the body's own gravity from
        // the perturber set during fitting. Without this the integrator
        // self-pulls and converges to junk fixed points (Pallas RMS 8000″,
        // Iris RMS 149″, etc.). The validation catalog tags these with
        // population = "Self-Perturber"; mpc_designation carries the
        // asteroid number for Origin::Asteroid construction.
        let excluded_origins: Vec<Origin> = if obj.population == "Self-Perturber" {
            match obj.mpc_designation.parse::<i32>() {
                Ok(n) => vec![Origin::Asteroid(n)],
                Err(_) => Vec::new(),
            }
        } else {
            Vec::new()
        };
        let excluded_naif: Vec<i32> = excluded_origins
            .iter()
            .copied()
            .map(Origin::naif_id)
            .collect();
        eprintln!(
            "  {}: {} observations, running determine{}...",
            obj.name,
            n_obs,
            if excluded_naif.is_empty() {
                String::new()
            } else {
                format!(" (excluded perturbers: {excluded_naif:?})")
            },
        );

        let t0 = std::time::Instant::now();
        let od_config = ODConfig {
            force_model: tier,
            max_iterations,
            excluded_perturbers: excluded_origins,
            ..ODConfig::default()
        };
        let determine_result = match ctx.determine(&observations, None, &od_config) {
            Ok(r) => r,
            Err(e) => {
                // empyrean-8l28: emit an explicit failure row so the
                // downstream report sees the failure rather than the
                // fixture silently disappearing. Project rule:
                // "no hidden fallbacks in scientific code — every
                // mismatch must surface loudly."
                let ms_fail = t0.elapsed().as_secs_f64() * 1000.0;
                eprintln!(
                    "  {}: determine FAIL ({e}) — emitting failure row",
                    obj.name
                );
                let mut row = empyrean_validation::schema::ValidationResult::empty();
                row.object = obj.name.to_string();
                row.population = obj.population.to_string();
                row.test_type =
                    empyrean_validation::schema::test_types::ORBIT_DETERMINATION.to_string();
                row.channel = channel.clone();
                row.force_model = tier_str.clone();
                row.n_obs_used = Some(n_obs as u32);
                row.od_converged = Some(false);
                row.od_iterations = Some(max_iterations);
                row.emp_time_ms = Some(ms_fail);
                row.excluded_perturbers_naif = excluded_naif.clone();
                row.timestamp = chrono::Utc::now().to_rfc3339();
                row.notes = format!("determine FAIL: {e}");
                results.push(row);
                return (results, captured_orbits, orbit_comparisons);
            }
        };
        let ms = t0.elapsed().as_secs_f64() * 1000.0;

        eprintln!(
            "    converged={} iterations={} rms_ra={:.4} rms_dec={:.4} chi2={:.2} ({:.0}ms)",
            determine_result.converged,
            determine_result.iterations,
            determine_result.summary.rms_ra_arcsec,
            determine_result.summary.rms_dec_arcsec,
            determine_result.summary.chi2,
            ms
        );

        // `DetermineResult.orbit` is now a re-feedable `Orbit`; take the
        // bare state snapshot (epoch/position/velocity/covariance/frame/
        // origin) the validation channel records.
        let orbit = determine_result.state();

        // Capture scott's fitted state + cov in three coordinate views
        // (native Cartesian, Sun-centered ICRF Cartesian, Sun-centered
        // ecliptic-J2000 Keplerian) for the orbit-comparison panel.
        // Transformation via `ctx.transform` propagates covariance
        // through the Jacobian.
        let scott_native = propagated_state_to_coord(&orbit);
        let empy_version = empyrean::version_string().ok();
        let scott_captured = match capture_orbit(
            ctx,
            obj.name,
            orbit_sources::SCOTT_OD,
            empy_version.clone(),
            &scott_native,
        ) {
            Ok(captured) => Some(captured),
            Err(e) => {
                eprintln!("  {}: scott orbit-capture transform FAIL ({e})", obj.name);
                None
            }
        };
        if let Some(c) = &scott_captured {
            captured_orbits.push(c.clone());
        }

        // Capture SBDB's published orbit (if any) so the comparison
        // kernel can pair scott_od ↔ sbdb. SBDB returns
        // CometaryCoordinates with covariance for objects that have
        // a published solution; short-arc impactors typically do not.
        let (sbdb_native, sbdb_captured) =
            match empyrean::query_sbdb(&[obj.sbdb_query], sbdb_cache_dir) {
                Ok(batch) if !batch.orbits.is_empty() => {
                    let sbdb_state = batch.orbits[0].state;
                    let sbdb_orbit_id = batch.orbit_ids.first().cloned();
                    let captured = match capture_orbit(
                        ctx,
                        obj.name,
                        orbit_sources::SBDB,
                        sbdb_orbit_id,
                        &sbdb_state,
                    ) {
                        Ok(c) => {
                            captured_orbits.push(c.clone());
                            Some(c)
                        }
                        Err(e) => {
                            eprintln!("  {}: SBDB orbit-capture transform FAIL ({e})", obj.name,);
                            None
                        }
                    };
                    (Some(sbdb_state), captured)
                }
                Ok(_) => {
                    eprintln!("  {}: SBDB returned empty batch", obj.name);
                    (None, None)
                }
                Err(e) => {
                    eprintln!("  {}: SBDB SKIP ({e})", obj.name);
                    (None, None)
                }
            };

        // Bidirectional orbit-vs-orbit comparison. For each (scott,
        // sbdb) pair, propagate one side to the other's epoch and
        // compare in Keplerian space. This produces two rows per pair
        // — one at the scott epoch, one at the sbdb epoch — so the
        // report can show how much each side's uncertainty inflates
        // under propagation.
        if let (Some(scott_c), Some(sbdb_native), Some(sbdb_c)) =
            (&scott_captured, &sbdb_native, &sbdb_captured)
        {
            let prop_cfg = empyrean::PropagationConfig {
                force_model: tier,
                uncertainty_method: empyrean::UncertaintyMethod::FirstOrder,
                frame: empyrean::Frame::ICRF,
                ..empyrean::PropagationConfig::default()
            };

            // Direction A: bring sbdb to scott's epoch; compare at scott's epoch.
            match propagate_and_capture(
                ctx,
                obj.name,
                "sbdb_at_scott_epoch",
                empy_version.clone(),
                sbdb_native,
                scott_c.epoch_mjd_tdb,
                &prop_cfg,
            ) {
                Ok(sbdb_at_scott) => {
                    captured_orbits.push(sbdb_at_scott.clone());
                    // Re-tag as canonical SBDB so the kernel pairs it
                    // with scott_od (kernel only matches the two
                    // canonical source tags).
                    let mut sbdb_at_scott_as_sbdb = sbdb_at_scott.clone();
                    sbdb_at_scott_as_sbdb.source = orbit_sources::SBDB.to_string();
                    let mut rows = compare_orbits(&[scott_c.clone(), sbdb_at_scott_as_sbdb], 1.0);
                    for r in rows.iter_mut() {
                        r.common_epoch_source = "scott".to_string();
                        r.notes
                            .push("reference (SBDB) propagated to scott epoch via STM".to_string());
                    }
                    orbit_comparisons.extend(rows);
                }
                Err(e) => {
                    eprintln!("  {}: propagate SBDB→scott_epoch FAIL ({e})", obj.name,);
                }
            }

            // Direction B: bring scott to sbdb's epoch; compare at sbdb's epoch.
            match propagate_and_capture(
                ctx,
                obj.name,
                "scott_at_sbdb_epoch",
                empy_version.clone(),
                &scott_native,
                sbdb_c.epoch_mjd_tdb,
                &prop_cfg,
            ) {
                Ok(scott_at_sbdb) => {
                    captured_orbits.push(scott_at_sbdb.clone());
                    // Manually pair: kernel only pairs scott_od ↔
                    // sbdb|findorb, so we synthesize a comparison
                    // record by feeding a scott_od-tagged copy of the
                    // propagated state.
                    let mut scott_at_sbdb_as_scott = scott_at_sbdb.clone();
                    scott_at_sbdb_as_scott.source = orbit_sources::SCOTT_OD.to_string();
                    let mut rows = compare_orbits(&[scott_at_sbdb_as_scott, sbdb_c.clone()], 1.0);
                    for r in rows.iter_mut() {
                        r.common_epoch_source = "sbdb".to_string();
                        r.notes
                            .push("scott propagated to SBDB epoch via STM".to_string());
                    }
                    orbit_comparisons.extend(rows);
                }
                Err(e) => {
                    eprintln!("  {}: propagate scott→sbdb_epoch FAIL ({e})", obj.name,);
                }
            }
        }
        results.push(ValidationResult {
            object: obj.name.to_string(),
            population: obj.population.to_string(),
            epoch_mjd_tdb: orbit.epoch.mjd_tdb().unwrap_or(f64::NAN),
            dt_days: 0.0,
            t_mjd_tdb: orbit.epoch.mjd_tdb().unwrap_or(f64::NAN),
            force_model: tier_str.clone(),
            test_type: "orbit_determination".to_string(),
            channel: channel.clone(),
            observer: None,
            emp_vs_horizons_km: None,
            emp_pos_au: Some(orbit.position),
            emp_time_ms: Some(ms),
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
            n_obs_used: Some(determine_result.summary.num_selected as u32),
            od_iterations: Some(determine_result.iterations),
            od_converged: Some(determine_result.converged),
            od_rms_ra_arcsec: Some(determine_result.summary.rms_ra_arcsec),
            od_rms_dec_arcsec: Some(determine_result.summary.rms_dec_arcsec),
            od_rms_combined_arcsec: Some(determine_result.summary.rms_combined_arcsec),
            od_chi2: Some(determine_result.summary.chi2),
            od_reduced_chi2: Some(determine_result.summary.reduced_chi2),
            excluded_perturbers_naif: excluded_naif,
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
            timestamp: timestamp.clone(),
            notes: obj.notes.to_string(),
        });
        (results, captured_orbits, orbit_comparisons)
    }).collect();

    // Flatten per-object triples back into the function-level accumulators.
    // Input order is preserved by `.par_iter().map().collect()` (Rayon's
    // contract — collect returns results in the input slice's order).
    let mut results: Vec<ValidationResult> = Vec::new();
    let mut captured_orbits: Vec<CapturedOrbit> = Vec::new();
    let mut orbit_comparisons: Vec<OrbitComparison> = Vec::new();
    for (r, c, p) in per_object {
        results.extend(r);
        captured_orbits.extend(c);
        orbit_comparisons.extend(p);
    }

    OdValidationOutput {
        results,
        captured_orbits,
        orbit_comparisons,
    }
}

/// Propagate a native CoordinateState (any representation) to
/// `target_epoch_mjd_tdb` via `ctx.propagate` with the supplied config
/// and capture the result as a [`CapturedOrbit`] tagged with `source`.
/// Covariance is propagated through the STM by the propagator (when
/// the input state carries one).
fn propagate_and_capture(
    ctx: &Context,
    object: &str,
    source: &str,
    source_version: Option<String>,
    native: &CoordinateState,
    target_epoch_mjd_tdb: f64,
    prop_cfg: &empyrean::PropagationConfig,
) -> Result<CapturedOrbit, empyrean::Error> {
    let orbit = empyrean::Orbit::new(*native);
    let target = Epoch::from_mjd_tdb(target_epoch_mjd_tdb);
    let result = ctx.propagate(&[orbit], &[target], prop_cfg)?;
    let propagated = result.states.first().ok_or_else(|| empyrean::Error {
        code: -4,
        message: "propagation returned no states (AGM mixture-only return or empty result)"
            .to_string(),
    })?;
    let propagated_coord = propagated_state_to_coord(propagated);
    capture_orbit(ctx, object, source, source_version, &propagated_coord)
}

/// Promote an `empyrean::PropagatedState` (OD output shape) to a full
/// [`CoordinateState`] so it can flow through `ctx.transform`. scott's
/// OD output is always Cartesian.
fn propagated_state_to_coord(orbit: &empyrean::PropagatedState) -> CoordinateState {
    CoordinateState {
        epoch: orbit.epoch,
        elements: [
            orbit.position[0],
            orbit.position[1],
            orbit.position[2],
            orbit.velocity[0],
            orbit.velocity[1],
            orbit.velocity[2],
        ],
        covariance: orbit.covariance,
        representation: Representation::Cartesian,
        frame: orbit.frame,
        origin: orbit.origin,
    }
}

/// Transform any source's native orbit + covariance into the three
/// canonical views used by the orbit-comparison panel: the native view
/// (as-supplied), Sun-centered ICRF Cartesian, and Sun-centered
/// ecliptic-J2000 Keplerian. Covariance is propagated through the
/// Jacobian by `ctx.transform`.
fn capture_orbit(
    ctx: &Context,
    object: &str,
    source: &str,
    source_version: Option<String>,
    native: &CoordinateState,
) -> Result<CapturedOrbit, empyrean::Error> {
    let epoch_mjd_tdb = native.epoch.mjd_tdb().unwrap_or(f64::NAN);
    let native_repr = match native.representation {
        Representation::Cartesian => "cartesian",
        Representation::Keplerian => "keplerian",
        Representation::Cometary => "cometary",
        Representation::Spherical => "spherical",
    }
    .to_string();
    let native_frame = match native.frame {
        Frame::ICRF => "icrf",
        Frame::EclipticJ2000 => "ecliptic_j2000",
        Frame::ITRF93 => "itrf93",
    }
    .to_string();
    let native_origin_naif = native.origin.naif_id();

    // Sun-centered ICRF Cartesian.
    let cart_sun = ctx.transform(native, Representation::Cartesian, Frame::ICRF, Origin::SUN)?;

    // Sun-centered ecliptic-J2000 Keplerian.
    let kep_sun = ctx.transform(
        native,
        Representation::Keplerian,
        Frame::EclipticJ2000,
        Origin::SUN,
    )?;

    Ok(CapturedOrbit {
        object: object.to_string(),
        source: source.to_string(),
        source_version,
        epoch_mjd_tdb,
        native_repr,
        native_frame,
        native_origin_naif,
        native_state: native.elements,
        native_cov_6x6: native.covariance,
        state_cart_icrf_sun: cart_sun.elements,
        cov_cart_icrf_sun_6x6: cart_sun.covariance,
        state_kep_ecliptic_sun: kep_sun.elements,
        cov_kep_ecliptic_sun_6x6: kep_sun.covariance,
    })
}
