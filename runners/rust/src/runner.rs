//! Validation runner: propagate orbits and compare to JPL Horizons.
//!
//! Channel: rust, going through the empyrean safe wrapper (same C ABI
//! that python / c / cli ride) — so Section 09 fidelity diffs reflect
//! only binding-translation drift, not propagator-path drift.

use std::collections::HashMap;
use std::time::Instant;

use rayon::prelude::*;

use empyrean::{
    Context, CoordinateState, EphemerisConfig, EphemerisEntry, Epoch, ForceModelTier, Frame,
    ODConfig, Orbit, Origin, PropagationConfig, Representation, UncertaintyMethod,
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
    horizons_cache_dir: &std::path::Path,
    sbdb_cache_dir: &std::path::Path,
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
        horizons_ephemeris: HashMap<(&'static str, i64), EphemerisEntry>,
    }

    let obs_codes = empyrean_validation::catalog::OBSERVER_CODES;
    let mut obj_data: Vec<ObjData> = Vec::new();

    for obj in objs {
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

        let (hor_pos, hor_vel) = match empyrean::query_horizons_vectors(
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

        // Extract Marsden non-grav from SBDB and pass through with the
        // explicit g(r) parameters so the C ABI builds the correct
        // model. SBDB defaults to inverse_square for asteroids and
        // water-ice for comets. `dt` is the SBDB time-delay (days)
        // applied to g(r) — non-zero for Jupiter-family comets and
        // some interstellar objects (67P=+45.7d, 2I/Borisov=−65.1d).
        let (a1, a2, a3, ng_alpha, ng_r0, ng_m, ng_n, ng_k, ng_dt) = {
            let o = &sbdb.orbits[0];
            // The wrapper carries the Marsden g(r) parameters as flat
            // fields with an all-zero sentinel for the inverse-square
            // default; record the canonical inverse-square constants
            // (α=1, r0=1, m=2, n=0, k=0) in that case.
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

        // Ephemeris (RA/Dec) is observer-dependent — fetch from every site so
        // the report can average the sky-plane separation over the sites.
        let mut horizons_ephemeris: HashMap<(&'static str, i64), EphemerisEntry> = HashMap::new();
        for &obs_code in obs_codes {
            for &dt in dt_list {
                let target = epoch + dt;
                match empyrean::query_horizons(
                    &[obj.horizons_command],
                    obs_code,
                    &[target],
                    Some(horizons_cache_dir),
                ) {
                    Ok(r) if !r.is_empty() => {
                        horizons_ephemeris
                            .insert((obs_code, dt as i64), r.into_iter().next().unwrap());
                    }
                    Ok(_) => {
                        eprintln!(
                            "  {}: {obs_code} dt={dt:+.0}d ephemeris SKIP (empty)",
                            obj.name
                        );
                    }
                    Err(e) => {
                        eprintln!(
                            "  {}: {obs_code} dt={dt:+.0}d ephemeris SKIP ({e})",
                            obj.name
                        );
                    }
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
    //   - "auto"                   — UncertaintyMethod::Auto: the
    //                                 engine's Phase A/B/C cascade
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
                        let mut emp_pos_cov: Option<[[f64; 3]; 3]> = None;
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
                                        // Propagated position 3×3 covariance (AU²) — present only
                                        // when a covariance was propagated (first_order_with_cov).
                                        emp_pos_cov = result.covariance_at_cartesian(0, 0).ok().map(|tc| {
                                            let m = tc.matrix;
                                            [
                                                [m[0][0], m[0][1], m[0][2]],
                                                [m[1][0], m[1][1], m[1][2]],
                                                [m[2][0], m[2][1], m[2][2]],
                                            ]
                                        });
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
                            emp_pos_cov_au2: emp_pos_cov,
                            emp_time_ms: Some(emp_ms),
                            separation_arcsec: None,
                            d_ra_arcsec: None,
                            d_dec_arcsec: None,
                            emp_radec_cov_arcsec2: None,
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
                            ref_sun_pos_au: None,
                            ref_sun_vel_au_d: None,
                            ref_od_rms_normalized: None,
                            ref_od_reduced_chi2: None,
                            ref_od_n_obs_used: None,
                            ref_od_n_del_obs_used: None,
                            ref_od_n_dop_obs_used: None,
                            ref_od_data_arc_days: None,
                            ref_od_condition_code: None,
                            ref_od_soln_date: None,
                            ref_od_pe_used: None,
                            ref_od_sb_used: None,
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
                            od_dt: None,
                            od_dt_sigma: None,
                            od_h: None,
                            od_h_sigma: None,
                            od_g1: None,
                            od_g1_sigma: None,
                            od_g2: None,
                            od_g2_sigma: None,
                            od_photometry_model: None,
                            od_photometry_reduced_chi2: None,
                            od_thrust_dv_m_per_s: Vec::new(),
                            od_thrust_dv_sigma_m_per_s: Vec::new(),
                            excluded_perturbers_naif: Vec::new(),
                            propagation_uncertainty: Some(uncertainty_tag.to_string()),
                            assist_vs_horizons_km: None,
                            emp_vs_assist_km: None,
                            assist_time_ms: None,
                            speed_ratio: None,
                            findorb_rms_residual: None,
                            findorb_n_obs_used: None,
                            findorb_n_obs_rejected: None,
                            findorb_vs_horizons_km: None,
                            emp_vs_findorb_km: None,
                            findorb_separation_arcsec: None,
                            findorb_d_ra_arcsec: None,
                            findorb_d_dec_arcsec: None,
                            findorb_d_rho_km: None,
                            findorb_time_ms: None,
                            kete_time_ms: None,
                            jorbit_time_ms: None,
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
                            layup_chi2: None,
                            layup_reduced_chi2: None,
                            layup_n_obs_used: None,
                            layup_converged: None,
                            layup_time_ms: None,
                            timestamp: timestamp.clone(),
                            notes: data.notes.clone(),
                        });
                    }
                }

                // Ephemeris tests (Standard tier) — one row per observing site.
                for &obs_code in obs_codes {
                for &dt in data.dt_list {
                    let Some(hor) = data.horizons_ephemeris.get(&(obs_code, dt as i64)) else {
                        continue;
                    };
                    // Reference entries carry degrees over the wrapper
                    // surface; comparisons below are in radians.
                    let hor_ra_rad = hor.ra_deg.to_radians();
                    let hor_dec_rad = hor.dec_deg.to_radians();
                    let hor_light_time_d =
                        (!hor.light_time_days.is_nan()).then_some(hor.light_time_days);
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
                        Ok(eph) => {
                            let Some(entry) = eph.entries.first() else {
                                continue;
                            };
                            // Wrapper returns degrees; compare in radians.
                            let emp_ra_rad = entry.ra_deg.to_radians();
                            let emp_dec_rad = entry.dec_deg.to_radians();

                            let sep = compare::angular_separation_arcsec(
                                emp_ra_rad,
                                emp_dec_rad,
                                hor_ra_rad,
                                hor_dec_rad,
                            );
                            // Wrap the RA difference to [-π, π] so an object
                            // near RA = 0 / 2π doesn't produce a spurious ~2π
                            // residual. (Dec needs no wrap; separation below is
                            // great-circle and already wrap-safe.)
                            let mut d_ra_wrapped =
                                (emp_ra_rad - hor_ra_rad).rem_euclid(std::f64::consts::TAU);
                            if d_ra_wrapped > std::f64::consts::PI {
                                d_ra_wrapped -= std::f64::consts::TAU;
                            }
                            let d_ra = d_ra_wrapped * emp_dec_rad.cos();
                            let d_dec = emp_dec_rad - hor_dec_rad;
                            let d_ra_arcsec = d_ra.to_degrees() * 3600.0;
                            let d_dec_arcsec = d_dec.to_degrees() * 3600.0;

                            // Sky-plane 2×2 covariance (arcsec², RA·cosδ) = the input
                            // covariance mapped through the ephemeris Jacobian. Rows 0,1
                            // of the [6][n_params] Jacobian are ∂RA,∂Dec (deg per input
                            // unit); project only the 6 state columns (C_in is the 6×6
                            // state covariance), scale RA by cosδ, convert deg→arcsec.
                            let emp_radec_cov: Option<[[f64; 2]; 2]> =
                                match (&covariance, eph.sensitivity.first()) {
                                    (Some(cin), Some(sens))
                                        if sens.jacobian.len() >= 2 * (sens.n_params as usize) =>
                                    {
                                        let np = sens.n_params as usize;
                                        let hra = &sens.jacobian[0..np];
                                        let hdec = &sens.jacobian[np..2 * np];
                                        let quad = |ha: &[f64], hb: &[f64]| {
                                            let mut s = 0.0;
                                            for i in 0..6 {
                                                for j in 0..6 {
                                                    s += ha[i] * cin[i][j] * hb[j];
                                                }
                                            }
                                            s
                                        };
                                        let cosd = emp_dec_rad.cos();
                                        let a2 = 3600.0_f64 * 3600.0;
                                        Some([
                                            [
                                                quad(hra, hra) * cosd * cosd * a2,
                                                quad(hra, hdec) * cosd * a2,
                                            ],
                                            [
                                                quad(hra, hdec) * cosd * a2,
                                                quad(hdec, hdec) * a2,
                                            ],
                                        ])
                                    }
                                    _ => None,
                                };

                            let d_rho_km = Some((entry.rho_au - hor.rho_au) * compare::AU_KM);
                            let d_lt_s = if entry.light_time_days.is_finite() {
                                hor_light_time_d
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
                                emp_pos_cov_au2: None,
                                emp_time_ms: None,
                                separation_arcsec: Some(sep),
                                d_ra_arcsec: Some(d_ra_arcsec),
                                d_dec_arcsec: Some(d_dec_arcsec),
                                emp_radec_cov_arcsec2: emp_radec_cov,
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
                                ref_ra_rad: Some(hor_ra_rad),
                                ref_dec_rad: Some(hor_dec_rad),
                                ref_rho_au: Some(hor.rho_au),
                                ref_light_time_d: hor_light_time_d,
                                ref_sun_pos_au: None,
                                ref_sun_vel_au_d: None,
                                ref_od_rms_normalized: None,
                                ref_od_reduced_chi2: None,
                                ref_od_n_obs_used: None,
                                ref_od_n_del_obs_used: None,
                                ref_od_n_dop_obs_used: None,
                                ref_od_data_arc_days: None,
                                ref_od_condition_code: None,
                                ref_od_soln_date: None,
                                ref_od_pe_used: None,
                                ref_od_sb_used: None,
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
                                od_dt: None,
                                od_dt_sigma: None,
                                od_h: None,
                                od_h_sigma: None,
                                od_g1: None,
                                od_g1_sigma: None,
                                od_g2: None,
                                od_g2_sigma: None,
                                od_photometry_model: None,
                                od_photometry_reduced_chi2: None,
                                od_thrust_dv_m_per_s: Vec::new(),
                                od_thrust_dv_sigma_m_per_s: Vec::new(),
                                excluded_perturbers_naif: Vec::new(),
                                propagation_uncertainty: Some(uncertainty_tag.to_string()),
                                assist_vs_horizons_km: None,
                                emp_vs_assist_km: None,
                                assist_time_ms: None,
                                speed_ratio: None,
                                findorb_rms_residual: None,
                                findorb_n_obs_used: None,
                                findorb_n_obs_rejected: None,
                                findorb_vs_horizons_km: None,
                                emp_vs_findorb_km: None,
                                findorb_separation_arcsec: None,
                                findorb_d_ra_arcsec: None,
                                findorb_d_dec_arcsec: None,
                                findorb_d_rho_km: None,
                                findorb_time_ms: None,
                                kete_time_ms: None,
                                jorbit_time_ms: None,
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
                                layup_chi2: None,
                                layup_reduced_chi2: None,
                                layup_n_obs_used: None,
                                layup_converged: None,
                                layup_time_ms: None,
                                timestamp: timestamp.clone(),
                                notes: data.notes.clone(),
                            });
                        }
                        Err(e) => {
                            eprintln!("  {} dt={dt:+.0}d FAIL ({e})", data.name);
                        }
                    }
                }
                } // end for &obs_code (observing sites)
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
    /// Bidirectional comparisons: per (empyrean_od, reference) pair, one
    /// comparison at the fit epoch and one at the reference epoch.
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
/// - one per successful OD fit (`source = "empyrean_od"`), and
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
    // shared mutable state in the engine's determine pipeline); empyrean::Context
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
        // Try common PSV filename variants. Object names with a "/"
        // (the comets — "2P/Encke", "103P/Hartley 2", the interstellars)
        // are stored with the slash rewritten to "_" so the name is not
        // read as a path separator; try that sanitized form too.
        let candidates = [
            fixtures_dir.join(format!("{}.psv", obj.name)),
            fixtures_dir.join(format!("{}.psv", obj.name.replace('/', "_"))),
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

        // Capture the fitted state + cov in three coordinate views
        // (native Cartesian, Sun-centered ICRF Cartesian, Sun-centered
        // ecliptic-J2000 Keplerian) for the orbit-comparison panel.
        // Transformation via `ctx.transform` propagates covariance
        // through the Jacobian.
        let fit_native = propagated_state_to_coord(&orbit);
        let empy_version = empyrean::version_string().ok();
        let fit_captured = match capture_orbit(
            ctx,
            obj.name,
            orbit_sources::EMPYREAN_OD,
            empy_version.clone(),
            &fit_native,
        ) {
            Ok(captured) => Some(captured),
            Err(e) => {
                eprintln!("  {}: fit orbit-capture transform FAIL ({e})", obj.name);
                None
            }
        };
        if let Some(c) = &fit_captured {
            captured_orbits.push(c.clone());
        }

        // Capture SBDB's published orbit (if any) so the comparison
        // kernel can pair empyrean_od ↔ sbdb. SBDB returns
        // CometaryCoordinates with covariance for objects that have
        // a published solution; short-arc impactors typically do not.
        // `sbdb_nongrav` carries SBDB's published Marsden (A1, A2, A3) for
        // this object — the reference signal the non-grav-recovery second
        // pass below compares against. `None` when SBDB has no orbit.
        let (sbdb_native, sbdb_captured, sbdb_nongrav) =
            match empyrean::query_sbdb(&[obj.sbdb_query], sbdb_cache_dir) {
                Ok(batch) if !batch.orbits.is_empty() => {
                    let sbdb_state = batch.orbits[0].state;
                    let sbdb_orbit_id = batch.orbit_ids.first().cloned();
                    let sbdb_ng = (
                        batch.orbits[0].a1,
                        batch.orbits[0].a2,
                        batch.orbits[0].a3,
                    );
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
                    (Some(sbdb_state), captured, Some(sbdb_ng))
                }
                Ok(_) => {
                    eprintln!("  {}: SBDB returned empty batch", obj.name);
                    (None, None, None)
                }
                Err(e) => {
                    eprintln!("  {}: SBDB SKIP ({e})", obj.name);
                    (None, None, None)
                }
            };

        // Bidirectional orbit-vs-orbit comparison. For each (fit,
        // sbdb) pair, propagate one side to the other's epoch and
        // compare in Keplerian space. This produces two rows per pair
        // — one at the fit epoch, one at the sbdb epoch — so the
        // report can show how much each side's uncertainty inflates
        // under propagation.
        if let (Some(fit_c), Some(sbdb_native), Some(sbdb_c)) =
            (&fit_captured, &sbdb_native, &sbdb_captured)
        {
            let prop_cfg = empyrean::PropagationConfig {
                force_model: tier,
                uncertainty_method: empyrean::UncertaintyMethod::FirstOrder,
                frame: empyrean::Frame::ICRF,
                ..empyrean::PropagationConfig::default()
            };

            // Direction A: bring sbdb to fit's epoch; compare at fit's epoch.
            match propagate_and_capture(
                ctx,
                obj.name,
                "sbdb_at_fit_epoch",
                empy_version.clone(),
                sbdb_native,
                fit_c.epoch_mjd_tdb,
                &prop_cfg,
            ) {
                Ok(sbdb_at_fit) => {
                    captured_orbits.push(sbdb_at_fit.clone());
                    // Re-tag as canonical SBDB so the kernel pairs it
                    // with empyrean_od (kernel only matches the two
                    // canonical source tags).
                    let mut sbdb_at_fit_as_sbdb = sbdb_at_fit.clone();
                    sbdb_at_fit_as_sbdb.source = orbit_sources::SBDB.to_string();
                    let mut rows = compare_orbits(&[fit_c.clone(), sbdb_at_fit_as_sbdb], 1.0);
                    for r in rows.iter_mut() {
                        r.common_epoch_source = "fit".to_string();
                        r.notes
                            .push("reference (SBDB) propagated to fit epoch via STM".to_string());
                    }
                    orbit_comparisons.extend(rows);
                }
                Err(e) => {
                    eprintln!("  {}: propagate SBDB→fit_epoch FAIL ({e})", obj.name,);
                }
            }

            // Direction B: bring fit to sbdb's epoch; compare at sbdb's epoch.
            match propagate_and_capture(
                ctx,
                obj.name,
                "fit_at_sbdb_epoch",
                empy_version.clone(),
                &fit_native,
                sbdb_c.epoch_mjd_tdb,
                &prop_cfg,
            ) {
                Ok(fit_at_sbdb) => {
                    captured_orbits.push(fit_at_sbdb.clone());
                    // Manually pair: kernel only pairs empyrean_od ↔
                    // sbdb|findorb, so we synthesize a comparison
                    // record by feeding an empyrean_od-tagged copy of the
                    // propagated state.
                    let mut fit_at_sbdb_as_fit = fit_at_sbdb.clone();
                    fit_at_sbdb_as_fit.source = orbit_sources::EMPYREAN_OD.to_string();
                    let mut rows = compare_orbits(&[fit_at_sbdb_as_fit, sbdb_c.clone()], 1.0);
                    for r in rows.iter_mut() {
                        r.common_epoch_source = "sbdb".to_string();
                        r.notes
                            .push("fit propagated to SBDB epoch via STM".to_string());
                    }
                    orbit_comparisons.extend(rows);
                }
                Err(e) => {
                    eprintln!("  {}: propagate fit→sbdb_epoch FAIL ({e})", obj.name,);
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
            emp_pos_cov_au2: None,
            emp_time_ms: Some(ms),
            separation_arcsec: None,
            d_ra_arcsec: None,
            d_dec_arcsec: None,
            emp_radec_cov_arcsec2: None,
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
            ref_sun_pos_au: None,
            ref_sun_vel_au_d: None,
            ref_od_rms_normalized: None,
            ref_od_reduced_chi2: None,
            ref_od_n_obs_used: None,
            ref_od_n_del_obs_used: None,
            ref_od_n_dop_obs_used: None,
            ref_od_data_arc_days: None,
            ref_od_condition_code: None,
            ref_od_soln_date: None,
            ref_od_pe_used: None,
            ref_od_sb_used: None,
            n_obs_used: Some(determine_result.summary.num_selected as u32),
            od_iterations: Some(determine_result.iterations),
            od_converged: Some(determine_result.converged),
            od_rms_ra_arcsec: Some(determine_result.summary.rms_ra_arcsec),
            od_rms_dec_arcsec: Some(determine_result.summary.rms_dec_arcsec),
            od_rms_combined_arcsec: Some(determine_result.summary.rms_combined_arcsec),
            od_chi2: Some(determine_result.summary.chi2),
            od_reduced_chi2: Some(determine_result.summary.reduced_chi2),
            od_a1: None,
            od_a2: None,
            od_a3: None,
            od_a1_sigma: None,
            od_a2_sigma: None,
            od_a3_sigma: None,
            od_dt: None,
            od_dt_sigma: None,
            od_h: None,
            od_h_sigma: None,
            od_g1: None,
            od_g1_sigma: None,
            od_g2: None,
            od_g2_sigma: None,
            od_photometry_model: None,
            od_photometry_reduced_chi2: None,
            od_thrust_dv_m_per_s: Vec::new(),
            od_thrust_dv_sigma_m_per_s: Vec::new(),
            excluded_perturbers_naif: excluded_naif,
            propagation_uncertainty: None,
            assist_vs_horizons_km: None,
            emp_vs_assist_km: None,
            assist_time_ms: None,
            speed_ratio: None,
            findorb_rms_residual: None,
            findorb_n_obs_used: None,
            findorb_n_obs_rejected: None,
            findorb_vs_horizons_km: None,
            emp_vs_findorb_km: None,
            findorb_separation_arcsec: None,
            findorb_d_ra_arcsec: None,
            findorb_d_dec_arcsec: None,
            findorb_d_rho_km: None,
            findorb_time_ms: None,
            kete_time_ms: None,
            jorbit_time_ms: None,
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
            layup_chi2: None,
            layup_reduced_chi2: None,
            layup_n_obs_used: None,
            layup_converged: None,
            layup_time_ms: None,
            timestamp: timestamp.clone(),
            notes: obj.notes.to_string(),
        });

        // ── Second OD: optical + radar (objects with a psv-radar fixture) ──
        // For objects that have radar astrometry, read the radar-augmented
        // fixture (the same optical arc plus the ADES `<radar>` delay/Doppler
        // table, in the sibling `psv-radar/` dir) and run a second determine,
        // reusing the same `od_config`. The radar-tightened orbit is emitted as
        // a separate `orbit_determination_radar` row so the report / find_orb
        // merge cross-checks it the same way as the optical-only fit. Objects
        // without a psv-radar fixture (the bulk of the catalog) are untouched.
        let radar_psv = fixtures_dir
            .parent()
            .map(|p| p.join("psv-radar"))
            .into_iter()
            .flat_map(|d| {
                [
                    d.join(format!("{}.psv", obj.name)),
                    d.join(format!("{}.psv", obj.name.replace('/', "_"))),
                    d.join(format!("{}.psv", obj.mpc_designation)),
                ]
            })
            .find(|p| p.exists());
        if let Some(radar_psv) = radar_psv {
            match std::fs::read_to_string(&radar_psv)
                .ok()
                .and_then(|s| ctx.read_ades(&s).ok())
            {
                Some(obs_r) if obs_r.radar_len() > 0 => {
                    eprintln!(
                        "  {}: + radar OD ({} obs incl {} radar)...",
                        obj.name,
                        obs_r.len(),
                        obs_r.radar_len()
                    );
                    let t0r = std::time::Instant::now();
                    match ctx.determine(&obs_r, None, &od_config) {
                        Ok(dr) => {
                            let ms_r = t0r.elapsed().as_secs_f64() * 1000.0;
                            let orbit_r = dr.state();
                            eprintln!(
                                "    radar: converged={} rms_combined={:.4} ({:.0}ms)",
                                dr.converged, dr.summary.rms_combined_arcsec, ms_r
                            );
                            let excluded_naif_r: Vec<i32> = od_config
                                .excluded_perturbers
                                .iter()
                                .copied()
                                .map(Origin::naif_id)
                                .collect();
                            results.push(ValidationResult {
                                object: obj.name.to_string(),
                                population: obj.population.to_string(),
                                epoch_mjd_tdb: orbit_r.epoch.mjd_tdb().unwrap_or(f64::NAN),
                                dt_days: 0.0,
                                t_mjd_tdb: orbit_r.epoch.mjd_tdb().unwrap_or(f64::NAN),
                                force_model: tier_str.clone(),
                                test_type:
                                    empyrean_validation::schema::test_types::ORBIT_DETERMINATION_RADAR
                                        .to_string(),
                                channel: channel.clone(),
                                observer: None,
                                emp_vs_horizons_km: None,
                                emp_pos_au: Some(orbit_r.position),
                                emp_pos_cov_au2: None,
                                emp_time_ms: Some(ms_r),
                                separation_arcsec: None,
                                d_ra_arcsec: None,
                                d_dec_arcsec: None,
                                emp_radec_cov_arcsec2: None,
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
                                ref_sun_pos_au: None,
                                ref_sun_vel_au_d: None,
                                ref_od_rms_normalized: None,
                                ref_od_reduced_chi2: None,
                                ref_od_n_obs_used: None,
                                ref_od_n_del_obs_used: None,
                                ref_od_n_dop_obs_used: None,
                                ref_od_data_arc_days: None,
                                ref_od_condition_code: None,
                                ref_od_soln_date: None,
                                ref_od_pe_used: None,
                                ref_od_sb_used: None,
                                n_obs_used: Some(dr.summary.num_selected as u32),
                                od_iterations: Some(dr.iterations),
                                od_converged: Some(dr.converged),
                                od_rms_ra_arcsec: Some(dr.summary.rms_ra_arcsec),
                                od_rms_dec_arcsec: Some(dr.summary.rms_dec_arcsec),
                                od_rms_combined_arcsec: Some(dr.summary.rms_combined_arcsec),
                                od_chi2: Some(dr.summary.chi2),
                                od_reduced_chi2: Some(dr.summary.reduced_chi2),
                                od_a1: None,
                                od_a2: None,
                                od_a3: None,
                                od_a1_sigma: None,
                                od_a2_sigma: None,
                                od_a3_sigma: None,
                                od_dt: None,
                                od_dt_sigma: None,
                                od_h: None,
                                od_h_sigma: None,
                                od_g1: None,
                                od_g1_sigma: None,
                                od_g2: None,
                                od_g2_sigma: None,
                                od_photometry_model: None,
                                od_photometry_reduced_chi2: None,
                                od_thrust_dv_m_per_s: Vec::new(),
                                od_thrust_dv_sigma_m_per_s: Vec::new(),
                                excluded_perturbers_naif: excluded_naif_r,
                                propagation_uncertainty: None,
                                assist_vs_horizons_km: None,
                                emp_vs_assist_km: None,
                                assist_time_ms: None,
                                speed_ratio: None,
                                findorb_rms_residual: None,
                                findorb_n_obs_used: None,
                                findorb_n_obs_rejected: None,
                                findorb_vs_horizons_km: None,
                                emp_vs_findorb_km: None,
                                findorb_separation_arcsec: None,
                                findorb_d_ra_arcsec: None,
                                findorb_d_dec_arcsec: None,
                                findorb_d_rho_km: None,
                                findorb_time_ms: None,
                                kete_time_ms: None,
                                jorbit_time_ms: None,
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
                                layup_chi2: None,
                                layup_reduced_chi2: None,
                                layup_n_obs_used: None,
                                layup_converged: None,
                                layup_time_ms: None,
                                timestamp: timestamp.clone(),
                                notes: format!("optical+radar ({} radar obs)", obs_r.radar_len()),
                            });
                        }
                        Err(e) => {
                            eprintln!("  {}: radar OD FAIL ({e})", obj.name);
                        }
                    }
                }
                _ => {}
            }
        }

        // ── Third OD: non-grav recovery (objects with an SBDB A2 signal) ──
        // For objects whose JPL SBDB reference carries a non-zero
        // transverse non-grav coefficient (Yarkovsky NEOs like Apophis /
        // Bennu and the comets), re-fit the SAME optical arc with
        // `solve_for = StateAndNonGrav` and emit a separate
        // `non_grav_recovery` row carrying the FITTED A1/A2/A3 ± 1σ so the
        // report can compare fitted-vs-JPL in σ. The 1σ comes from the
        // fitted 9×9 (state + A1/A2/A3) covariance diagonal: σ_aᵢ =
        // sqrt(C9x9[6+i][6+i]).
        //
        // Loud-failure rule: if the fit did NOT actually recover non-grav
        // — the 9×9 is absent (`covariance_9x9 == None`) or a fitted a-value
        // is non-finite — that axis is emitted as `None` (never 0, never
        // NaN) so a missing value reads as "non-grav not recovered". (The
        // engine currently has a bug where StateAndNonGrav can silently
        // fall back to a 6-param state-only fit, so most of these rows
        // legitimately come back `None` for now — that is correct.)
        if let Some((ref_a1, ref_a2, ref_a3)) = sbdb_nongrav
            && ref_a2 != 0.0 {
                eprintln!("  {}: + non-grav recovery OD (SBDB A2={ref_a2:.3e})...", obj.name);
                let ng_config = ODConfig {
                    solve_for: empyrean::SolveForParams::StateAndNonGrav,
                    ..od_config.clone()
                };
                let t0n = std::time::Instant::now();
                match ctx.determine(&observations, None, &ng_config) {
                    Ok(dr) => {
                        let ms_n = t0n.elapsed().as_secs_f64() * 1000.0;
                        let orbit_n = dr.state();
                        // Per-axis sigma from the 9×9 covariance diagonal,
                        // present only when non-grav was actually solved.
                        // sqrt() of a non-finite / negative variance yields
                        // NaN, which the guard below maps back to None.
                        let sigma = |i: usize| -> Option<f64> {
                            dr.covariance_9x9
                                .map(|c| c[6 + i][6 + i].sqrt())
                                .filter(|s| s.is_finite())
                        };
                        // Fitted a-value → Some only when finite; otherwise
                        // None ("non-grav not recovered"). Pair each fitted
                        // value with its sigma so a None value never carries
                        // a stray sigma.
                        let fitted = |v: f64, i: usize| -> (Option<f64>, Option<f64>) {
                            if v.is_finite() {
                                (Some(v), sigma(i))
                            } else {
                                (None, None)
                            }
                        };
                        let (od_a1, od_a1_sigma) = fitted(dr.orbit.a1, 0);
                        let (od_a2, od_a2_sigma) = fitted(dr.orbit.a2, 1);
                        let (od_a3, od_a3_sigma) = fitted(dr.orbit.a3, 2);
                        eprintln!(
                            "    non-grav: converged={} 9x9={} a2_fit={:?} σ_a2={:?} ({:.0}ms)",
                            dr.converged,
                            dr.covariance_9x9.is_some(),
                            od_a2,
                            od_a2_sigma,
                            ms_n
                        );
                        let excluded_naif_n: Vec<i32> = ng_config
                            .excluded_perturbers
                            .iter()
                            .copied()
                            .map(Origin::naif_id)
                            .collect();
                        results.push(ValidationResult {
                            object: obj.name.to_string(),
                            population: obj.population.to_string(),
                            epoch_mjd_tdb: orbit_n.epoch.mjd_tdb().unwrap_or(f64::NAN),
                            dt_days: 0.0,
                            t_mjd_tdb: orbit_n.epoch.mjd_tdb().unwrap_or(f64::NAN),
                            force_model: tier_str.clone(),
                            test_type:
                                empyrean_validation::schema::test_types::NON_GRAV_RECOVERY
                                    .to_string(),
                            channel: channel.clone(),
                            observer: None,
                            emp_vs_horizons_km: None,
                            emp_pos_au: Some(orbit_n.position),
                            emp_pos_cov_au2: None,
                            emp_time_ms: Some(ms_n),
                            separation_arcsec: None,
                            d_ra_arcsec: None,
                            d_dec_arcsec: None,
                            emp_radec_cov_arcsec2: None,
                            d_rho_km: None,
                            d_light_time_s: None,
                            ic_pos_au: None,
                            ic_vel_au_d: None,
                            ic_a1: Some(ref_a1),
                            ic_a2: Some(ref_a2),
                            ic_a3: Some(ref_a3),
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
                            ref_sun_pos_au: None,
                            ref_sun_vel_au_d: None,
                            ref_od_rms_normalized: None,
                            ref_od_reduced_chi2: None,
                            ref_od_n_obs_used: None,
                            ref_od_n_del_obs_used: None,
                            ref_od_n_dop_obs_used: None,
                            ref_od_data_arc_days: None,
                            ref_od_condition_code: None,
                            ref_od_soln_date: None,
                            ref_od_pe_used: None,
                            ref_od_sb_used: None,
                            n_obs_used: Some(dr.summary.num_selected as u32),
                            od_iterations: Some(dr.iterations),
                            od_converged: Some(dr.converged),
                            od_rms_ra_arcsec: Some(dr.summary.rms_ra_arcsec),
                            od_rms_dec_arcsec: Some(dr.summary.rms_dec_arcsec),
                            od_rms_combined_arcsec: Some(dr.summary.rms_combined_arcsec),
                            od_chi2: Some(dr.summary.chi2),
                            od_reduced_chi2: Some(dr.summary.reduced_chi2),
                            od_a1,
                            od_a2,
                            od_a3,
                            od_a1_sigma,
                            od_a2_sigma,
                            od_a3_sigma,
                            od_dt: None,
                            od_dt_sigma: None,
                            od_h: None,
                            od_h_sigma: None,
                            od_g1: None,
                            od_g1_sigma: None,
                            od_g2: None,
                            od_g2_sigma: None,
                            od_photometry_model: None,
                            od_photometry_reduced_chi2: None,
                            od_thrust_dv_m_per_s: Vec::new(),
                            od_thrust_dv_sigma_m_per_s: Vec::new(),
                            excluded_perturbers_naif: excluded_naif_n,
                            propagation_uncertainty: None,
                            assist_vs_horizons_km: None,
                            emp_vs_assist_km: None,
                            assist_time_ms: None,
                            speed_ratio: None,
                            findorb_rms_residual: None,
                            findorb_n_obs_used: None,
                            findorb_n_obs_rejected: None,
                            findorb_vs_horizons_km: None,
                            emp_vs_findorb_km: None,
                            findorb_separation_arcsec: None,
                            findorb_d_ra_arcsec: None,
                            findorb_d_dec_arcsec: None,
                            findorb_d_rho_km: None,
                            findorb_time_ms: None,
                            kete_time_ms: None,
                            jorbit_time_ms: None,
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
                            layup_chi2: None,
                            layup_reduced_chi2: None,
                            layup_n_obs_used: None,
                            layup_converged: None,
                            layup_time_ms: None,
                            timestamp: timestamp.clone(),
                            notes: format!(
                                "non-grav recovery (solve_for=StateAndNonGrav, 9x9={})",
                                dr.covariance_9x9.is_some()
                            ),
                        });
                    }
                    Err(e) => {
                        eprintln!("  {}: non-grav recovery OD FAIL ({e})", obj.name);
                    }
                }
            }

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
/// [`CoordinateState`] so it can flow through `ctx.transform`. fit's
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
