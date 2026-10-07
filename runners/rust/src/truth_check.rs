//! `validate truth-check` — state-space covariance test of the synthetic
//! lane, transport-free.
//!
//! The walk scores predictions on the sky, which tests the fitted
//! covariance *and* its transport to a target epoch together. In the
//! synthetic lane the truth orbit is known, so the fitted 6×6 can be tested
//! directly: propagate the truth to each window's fit epoch, difference it
//! from the fitted state in Sun-centered ICRF Cartesian, and take the
//! Mahalanobis distance under the fitted covariance. Under a perfect model
//! and Gaussian noise \( d^2_6 \sim \chi^2_6 \) (mean 6) and the
//! position-block \( d^2_3 \sim \chi^2_3 \) (mean 3). Rejection, nightly
//! de-weighting and heavy tails move these exactly as they move the sky
//! statistics — but here no propagation of the covariance is involved, so
//! a sky-plane departure that does not show up here is the transport's.
//!
//! One propagation call per object over all its fit epochs; the fitted
//! states come from the walk's `captured` window views.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use empyrean::{Context, CoordinateState, Epoch, Frame, Orbit, Origin, PropagationConfig};
use empyrean_validation::predict_schema::WalkWindowRecord;
use empyrean_validation::synthetic::{
    SyntheticProvenance, TruthCheckRecord, TruthOrbit, mahalanobis_6x6,
};

pub struct TruthCheckArgs {
    pub provenance: PathBuf,
    pub windows: Vec<PathBuf>,
    pub output: PathBuf,
    pub tier: empyrean::ForceModelTier,
    pub only: Vec<String>,
}

fn truth_orbit(t: &TruthOrbit, object: &str) -> Result<Orbit, String> {
    if t.representation != "Cartesian" || t.frame != "ICRF" {
        return Err(format!(
            "{object}: truth orbit is {} / {} — only Cartesian ICRF truths are rebuilt",
            t.representation, t.frame
        ));
    }
    let origin = match t.origin.as_str() {
        "SolarSystemBarycenter" => Origin::SSB,
        "Sun" => Origin::SUN,
        other => return Err(format!("{object}: truth origin {other:?} not supported")),
    };
    let state = CoordinateState::cartesian(
        Epoch::from_mjd_tdb(t.epoch_mjd_tdb),
        t.elements,
        Frame::ICRF,
        origin,
    );
    let mut orbit = Orbit::new(state);
    orbit.a1 = t.a1;
    orbit.a2 = t.a2;
    orbit.a3 = t.a3;
    orbit.ng_alpha = t.ng_alpha;
    orbit.ng_r0 = t.ng_r0;
    orbit.ng_m = t.ng_m;
    orbit.ng_n = t.ng_n;
    orbit.ng_k = t.ng_k;
    orbit.non_grav_dt = t.non_grav_dt;
    Ok(orbit)
}

fn read_windows(paths: &[PathBuf]) -> Result<Vec<WalkWindowRecord>, String> {
    let mut out = Vec::new();
    for p in paths {
        let text = std::fs::read_to_string(p).map_err(|e| format!("read {}: {e}", p.display()))?;
        for (i, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let r: WalkWindowRecord = serde_json::from_str(line)
                .map_err(|e| format!("{}:{}: {e}", p.display(), i + 1))?;
            out.push(r);
        }
    }
    Ok(out)
}

pub fn run(ctx: &Context, args: &TruthCheckArgs) -> Result<usize, String> {
    let prov: SyntheticProvenance = serde_json::from_str(
        &std::fs::read_to_string(&args.provenance)
            .map_err(|e| format!("read {}: {e}", args.provenance.display()))?,
    )
    .map_err(|e| format!("parse {}: {e}", args.provenance.display()))?;
    let truths: BTreeMap<&str, &TruthOrbit> = prov
        .objects
        .iter()
        .filter_map(|o| o.truth.as_ref().map(|t| (o.object.as_str(), t)))
        .collect();
    let windows = read_windows(&args.windows)?;
    eprintln!(
        "truth-check: {} window records, {} truth orbits ({})",
        windows.len(),
        truths.len(),
        prov.snapshot_id
    );

    // Group the checkable records per object; one propagation per object.
    let mut by_object: BTreeMap<&str, Vec<&WalkWindowRecord>> = BTreeMap::new();
    let mut skipped_no_capture = 0usize;
    for w in &windows {
        if !args.only.is_empty() && !args.only.iter().any(|n| n == &w.object) {
            continue;
        }
        if !w.converged {
            continue;
        }
        if w.captured.is_none() {
            skipped_no_capture += 1;
            continue;
        }
        by_object.entry(w.object.as_str()).or_default().push(w);
    }

    let mut records: Vec<TruthCheckRecord> = Vec::new();
    let mut n_truth_missing = 0usize;
    for (object, recs) in &by_object {
        let Some(truth) = truths.get(object) else {
            n_truth_missing += recs.len();
            eprintln!(
                "  {object}: no truth orbit in provenance — {} records skipped",
                recs.len()
            );
            continue;
        };
        let orbit = truth_orbit(truth, object)?;
        // The truth propagates under the same exclusion its fit used — a
        // Self-Perturber pulled by its own ephemeris is not the truth.
        let cfg = PropagationConfig {
            force_model: args.tier,
            excluded_perturbers: crate::runner::naif_to_origins(&truth.excluded_perturbers_naif)?,
            ..PropagationConfig::default()
        };
        // Distinct fit epochs, ascending.
        let mut epochs: Vec<f64> = recs
            .iter()
            .map(|r| r.captured.as_ref().unwrap().epoch_mjd_tdb)
            .collect();
        epochs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        epochs.dedup();
        let t0 = Instant::now();
        let ep: Vec<Epoch> = epochs.iter().map(|&m| Epoch::from_mjd_tdb(m)).collect();
        let result = ctx
            .propagate(std::slice::from_ref(&orbit), &ep, &cfg)
            .map_err(|e| format!("{object}: propagate truth: {e}"))?;
        if result.states.len() != ep.len() {
            return Err(format!(
                "{object}: propagate returned {} states for {} epochs",
                result.states.len(),
                ep.len()
            ));
        }
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        // Truth at each epoch in Sun-centered ICRF Cartesian — the captured
        // view's frame — via the same transform the capture used.
        let mut truth_sun: BTreeMap<u64, [f64; 6]> = BTreeMap::new();
        for (m, s) in epochs.iter().zip(result.states.iter()) {
            let native = CoordinateState::cartesian(
                s.epoch,
                [
                    s.position[0],
                    s.position[1],
                    s.position[2],
                    s.velocity[0],
                    s.velocity[1],
                    s.velocity[2],
                ],
                s.frame,
                s.origin,
            );
            let sun = ctx
                .transform_coordinates_single(
                    &native,
                    empyrean::Representation::Cartesian,
                    Frame::ICRF,
                    Origin::SUN,
                )
                .map_err(|e| format!("{object}: transform truth to Sun ICRF: {e}"))?;
            truth_sun.insert(m.to_bits(), sun.elements);
        }
        for r in recs {
            let cap = r.captured.as_ref().unwrap();
            let truth_state = truth_sun[&cap.epoch_mjd_tdb.to_bits()];
            let mut delta = [0.0; 6];
            for i in 0..6 {
                delta[i] = cap.state_cart_icrf_sun[i] - truth_state[i];
            }
            let m = cap
                .cov_cart_icrf_sun_6x6
                .and_then(|c| mahalanobis_6x6(&delta, &c));
            records.push(TruthCheckRecord {
                object: r.object.clone(),
                tool: r.tool.clone(),
                config_arm: r.config_arm.clone(),
                window_index: r.window_index,
                fit_epoch_mjd_tdb: cap.epoch_mjd_tdb,
                n_obs_used: r.n_obs_used,
                n_solve_for: r.n_solve_for,
                covariance_trust: r.covariance_trust.clone(),
                delta_au_au_day: delta,
                d2_state6: m.as_ref().map(|x| x.d2_6),
                d2_pos3: m.as_ref().map(|x| x.d2_3),
                z_per_axis: m.as_ref().map(|x| x.z),
                dpos_km: (delta[0].powi(2) + delta[1].powi(2) + delta[2].powi(2)).sqrt()
                    * 149_597_870.7,
                sigma_pos_km: cap
                    .cov_cart_icrf_sun_6x6
                    .map(|c| (c[0][0] + c[1][1] + c[2][2]).max(0.0).sqrt() * 149_597_870.7),
                failure: match (&cap.cov_cart_icrf_sun_6x6, &m) {
                    (None, _) => Some("no fitted covariance".into()),
                    (Some(_), None) => Some("covariance not positive definite".into()),
                    _ => None,
                },
            });
        }
        eprintln!(
            "  {object:<16} {} records over {} epochs ({ms:.0} ms propagate)",
            recs.len(),
            epochs.len()
        );
    }

    if let Some(parent) = args.output.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let mut text = String::new();
    for r in &records {
        text.push_str(&serde_json::to_string(r).map_err(|e| format!("serialize: {e}"))?);
        text.push('\n');
    }
    std::fs::write(&args.output, text)
        .map_err(|e| format!("write {}: {e}", args.output.display()))?;
    let n_scored = records.iter().filter(|r| r.d2_state6.is_some()).count();
    eprintln!(
        "truth-check: {} records ({} scored, {} without a usable covariance); {} skipped without \
         a captured orbit, {} without a truth orbit → {}",
        records.len(),
        n_scored,
        records.len() - n_scored,
        skipped_no_capture,
        n_truth_missing,
        args.output.display()
    );
    Ok(records.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Cartesian-ICRF truth carrying non-gravitational parameters.
    fn sample_truth(representation: &str, frame: &str, origin: &str) -> TruthOrbit {
        TruthOrbit {
            epoch_mjd_tdb: 60000.0,
            representation: representation.into(),
            frame: frame.into(),
            origin: origin.into(),
            elements: [1.0, 2.0, 3.0, -0.01, 0.02, -0.03],
            a1: 1.5e-10,
            a2: -2.5e-11,
            a3: 3.5e-12,
            ng_alpha: 0.1112620426,
            ng_r0: 2.808,
            ng_m: 2.15,
            ng_n: 5.093,
            ng_k: 4.6142,
            non_grav_dt: Some(30.0),
            excluded_perturbers_naif: Vec::new(),
            n_solve_for: 9,
            n_obs_used: 100,
            n_obs_rejected: 2,
            reduced_chi2: 1.01,
            iterations: 7,
        }
    }

    #[test]
    fn truth_orbit_rebuilds_a_cartesian_icrf_truth_with_its_nongravs() {
        let t = sample_truth("Cartesian", "ICRF", "SolarSystemBarycenter");
        let orbit = truth_orbit(&t, "Eros").unwrap();
        assert_eq!(orbit.state.elements, t.elements);
        assert_eq!(orbit.a1, t.a1);
        assert_eq!(orbit.a2, t.a2);
        assert_eq!(orbit.a3, t.a3);
        assert_eq!(orbit.ng_alpha, t.ng_alpha);
        assert_eq!(orbit.ng_r0, t.ng_r0);
        assert_eq!(orbit.ng_m, t.ng_m);
        assert_eq!(orbit.ng_n, t.ng_n);
        assert_eq!(orbit.ng_k, t.ng_k);
        assert_eq!(orbit.non_grav_dt, t.non_grav_dt);
        // The Sun-centered origin is accepted too.
        assert!(truth_orbit(&sample_truth("Cartesian", "ICRF", "Sun"), "Eros").is_ok());
    }

    #[test]
    fn truth_orbit_refuses_unrebuildable_truths_loudly() {
        // Only Cartesian ICRF truths are rebuilt — a Keplerian or a
        // non-ICRF frame is an error, never a silent reinterpretation.
        let e = truth_orbit(&sample_truth("Keplerian", "ICRF", "Sun"), "Eros").unwrap_err();
        assert!(
            e.contains("Keplerian") && e.contains("only Cartesian ICRF"),
            "{e}"
        );
        let e = truth_orbit(&sample_truth("Cartesian", "Ecliptic", "Sun"), "Eros").unwrap_err();
        assert!(e.contains("Ecliptic"), "{e}");
        // An unsupported origin surfaces by name.
        let e = truth_orbit(&sample_truth("Cartesian", "ICRF", "Earth"), "Eros").unwrap_err();
        assert!(e.contains("Earth") && e.contains("origin"), "{e}");
    }
}
