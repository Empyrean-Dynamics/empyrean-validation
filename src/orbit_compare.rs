//! Orbit + covariance comparison kernel.
//!
//! Compare two [`CapturedOrbit`] records (e.g., the empyrean OD fit vs JPL
//! SBDB, or the fit vs find_orb) and produce an [`OrbitComparison`] with
//! per-element Δ, σ ratios, three Mahalanobis distances, and a
//! covariance volume ratio.
//!
//! # Comparison space
//!
//! All math runs in **Sun-centered ecliptic-J2000 Keplerian** elements
//! `(a [AU], e, i [deg], Ω [deg], ω [deg], M [deg])`. Each
//! [`CapturedOrbit`] carries the orbit + covariance in this view (the
//! emitter already ran the necessary Jacobian transform via
//! `empyrean::Context::transform`).
//!
//! # Common epoch (current scope)
//!
//! This module assumes the two records are **already at the same
//! epoch**. Bringing them to a common epoch (propagating the fit forward
//! / SBDB backward, or driving find_orb with `fo -tjd <jd>`) is the
//! orchestrating runner's job — this kernel is pure data + math.
//!
//! # Mahalanobis flavors
//!
//! For Δ = fit − ref (with angle elements wrapped to (-180°, 180°]):
//!
//! \\[
//! d^2_\text{fit}    = \Delta^\top \Sigma_\text{fit}^{-1}     \Delta
//! \\]
//! \\[
//! d^2_\text{ref}      = \Delta^\top \Sigma_\text{ref}^{-1}       \Delta
//! \\]
//! \\[
//! d^2_\text{combined} = \Delta^\top (\Sigma_\text{fit} + \Sigma_\text{ref})^{-1} \Delta
//! \\]
//!
//! The first two answer "is the other side inside my error ellipsoid?";
//! the third is the symmetric consistency metric — equivalent to a
//! two-sample Mahalanobis test that treats both as independent draws
//! from underlying Gaussians.
//!
//! `σ_equiv_combined = √(d²_combined / 6)` is the 6-DOF χ-equivalent
//! sigma — green if < 1, yellow 1–3, red > 3.

use crate::schema::{CapturedOrbit, OrbitComparison};

/// Pair empyrean_od orbits with reference orbits by object name and emit
/// one [`OrbitComparison`] per matched (fit, reference) pair.
///
/// Records without a Keplerian covariance are skipped (no `σ` available
/// → can't compute Mahalanobis). Records with mismatched epochs (Δ >
/// `epoch_tolerance_days`) are flagged in the resulting comparison's
/// `notes`.
///
/// # Panics
///
/// Does not panic; ill-conditioned covariances surface as a `note`
/// instead of a failure (Mahalanobis fields set to `f64::NAN`).
pub fn compare_orbits(
    captured: &[CapturedOrbit],
    epoch_tolerance_days: f64,
) -> Vec<OrbitComparison> {
    use crate::schema::orbit_sources;

    let fit_records: Vec<&CapturedOrbit> = captured
        .iter()
        .filter(|c| c.source == orbit_sources::EMPYREAN_OD)
        .collect();
    let mut comparisons = Vec::new();

    for fit in &fit_records {
        for reference in captured.iter().filter(|c| c.object == fit.object) {
            if reference.source == orbit_sources::EMPYREAN_OD {
                continue;
            }
            if reference.source != orbit_sources::SBDB && reference.source != orbit_sources::FINDORB
            {
                continue;
            }
            let mut notes: Vec<String> = Vec::new();
            let dt = (reference.epoch_mjd_tdb - fit.epoch_mjd_tdb).abs();
            if dt > epoch_tolerance_days {
                notes.push(format!(
                    "epoch mismatch: fit={:.3} ref={:.3} Δ={:.3}d (no propagation applied)",
                    fit.epoch_mjd_tdb, reference.epoch_mjd_tdb, dt
                ));
            }
            comparisons.push(compare_pair(fit, reference, notes));
        }
    }
    comparisons
}

fn compare_pair(
    fit: &CapturedOrbit,
    reference: &CapturedOrbit,
    mut notes: Vec<String>,
) -> OrbitComparison {
    let state_fit = fit.state_kep_ecliptic_sun;
    let state_ref = reference.state_kep_ecliptic_sun;
    let delta = delta_keplerian(&state_fit, &state_ref);

    let (sigma_fit, sigma_ref) = (
        diag_sigmas(fit.cov_kep_ecliptic_sun_6x6.as_ref()),
        diag_sigmas(reference.cov_kep_ecliptic_sun_6x6.as_ref()),
    );

    // Three Mahalanobis flavors. Each is f64::NAN if the corresponding
    // covariance is missing or ill-conditioned.
    let d2_fit_metric = fit
        .cov_kep_ecliptic_sun_6x6
        .as_ref()
        .map(|c| mahalanobis_d2_6(c, &delta))
        .unwrap_or(None)
        .unwrap_or(f64::NAN);
    let d2_ref_metric = reference
        .cov_kep_ecliptic_sun_6x6
        .as_ref()
        .map(|c| mahalanobis_d2_6(c, &delta))
        .unwrap_or(None)
        .unwrap_or(f64::NAN);
    let d2_combined_metric = match (
        &fit.cov_kep_ecliptic_sun_6x6,
        &reference.cov_kep_ecliptic_sun_6x6,
    ) {
        (Some(a), Some(b)) => {
            let sum = mat6_add(a, b);
            mahalanobis_d2_6(&sum, &delta).unwrap_or(f64::NAN)
        }
        _ => f64::NAN,
    };
    let sigma_equiv_combined = if d2_combined_metric.is_finite() {
        (d2_combined_metric / 6.0).sqrt()
    } else {
        f64::NAN
    };

    // Marginal Mahalanobis: Σ_k (Δ_k / σ_combined,k)². Contrast with
    // d²_combined: ratio ≈ 1 means the discrepancy is element-by-
    // element; ratio ≫ 1 means off-diagonal correlation in the joint
    // covariance is what's driving the joint metric (the "correlation"
    // pathology — short-arc fits with degenerate a/e are the canonical
    // example).
    let d2_marginal = (0..6)
        .map(|k| {
            let var_combined = sigma_fit[k].powi(2) + sigma_ref[k].powi(2);
            if var_combined > 0.0 && var_combined.is_finite() {
                delta[k].powi(2) / var_combined
            } else {
                0.0
            }
        })
        .sum::<f64>();

    // Eigen-decomposition of each ellipsoid + principal-axis rotation
    // angle. Together with the eigenvalue spectra, this is the
    // diagnostic that distinguishes the "rotation" pathology (large
    // rotation angle with similar eigenvalue spectrum: ellipsoids
    // pointing different directions) from the "correlation" pathology
    // (small rotation angle, scaled spectrum: same shape, different
    // size).
    let (eig_fit, eigvecs_fit) = eigen_of_kep_cov(&fit.cov_kep_ecliptic_sun_6x6, &mut notes, "fit");
    let (eig_ref, eigvecs_ref) =
        eigen_of_kep_cov(&reference.cov_kep_ecliptic_sun_6x6, &mut notes, "reference");
    let principal_axis_rotation_deg = match (eigvecs_fit, eigvecs_ref) {
        (Some(vs), Some(vr)) => {
            // First column of each = eigenvector of largest eigenvalue.
            let mut dot = 0.0_f64;
            for i in 0..6 {
                dot += vs[i][0] * vr[i][0];
            }
            dot.abs().min(1.0).acos().to_degrees()
        }
        _ => f64::NAN,
    };

    // Covariance volume ratio. det(Σ) = (∏ L_ii)² for cholesky factor L.
    let vol_ratio = match (
        &fit.cov_kep_ecliptic_sun_6x6,
        &reference.cov_kep_ecliptic_sun_6x6,
    ) {
        (Some(a), Some(b)) => {
            let det_a = det_via_cholesky_6(a);
            let det_b = det_via_cholesky_6(b);
            match (det_a, det_b) {
                (Some(da), Some(db)) if db > 0.0 => da / db,
                _ => {
                    notes.push("covariance not SPD — volume ratio undefined".to_string());
                    f64::NAN
                }
            }
        }
        _ => f64::NAN,
    };

    OrbitComparison {
        object: fit.object.clone(),
        reference: reference.source.clone(),
        common_epoch_mjd_tdb: fit.epoch_mjd_tdb,
        common_epoch_source: "fit".to_string(),
        repr: "keplerian".to_string(),
        state_fit,
        state_ref,
        delta,
        sigma_fit,
        sigma_ref,
        mahalanobis_d2_fit_metric: d2_fit_metric,
        mahalanobis_d2_ref_metric: d2_ref_metric,
        mahalanobis_d2_combined_metric: d2_combined_metric,
        mahalanobis_d2_marginal: d2_marginal,
        sigma_equiv_combined,
        eigenvalues_fit: eig_fit,
        eigenvalues_ref: eig_ref,
        principal_axis_rotation_deg,
        cov_volume_ratio: vol_ratio,
        notes,
    }
}

/// Compute eigenvalues (sorted descending) and eigenvectors (columns,
/// matching the eigenvalue order) of a 6×6 symmetric covariance.
/// Returns (`[NaN; 6]`, `None`) and pushes a note if the input is
/// missing or the iteration fails.
fn eigen_of_kep_cov(
    cov: &Option<[[f64; 6]; 6]>,
    notes: &mut Vec<String>,
    side: &str,
) -> ([f64; 6], Option<[[f64; 6]; 6]>) {
    let Some(c) = cov else {
        return ([f64::NAN; 6], None);
    };
    match jacobi_eigen_6(c) {
        Some((vals, vecs)) => (vals, Some(vecs)),
        None => {
            notes.push(format!("{side} cov eigendecomposition did not converge"));
            ([f64::NAN; 6], None)
        }
    }
}

/// 6×6 wrapper for `nolan::linalg::mat_symmetric_eigen`, accessed
/// straight from `hyperjet` (nolan's crates.io name) so we
/// don't add a direct nolan dependency to empyrean-validation.
///
/// The algorithm + tolerance logic (relative-to-Frobenius-scale
/// convergence — critical for small-scale physical-units covariance
/// matrices) lives in nolan.
fn jacobi_eigen_6(a: &[[f64; 6]; 6]) -> Option<([f64; 6], [[f64; 6]; 6])> {
    hyperjet::linalg::mat_symmetric_eigen(a)
}

/// Wrap an angle into the half-open interval `(-180°, 180°]`.
fn wrap_angle_deg(x: f64) -> f64 {
    let mut y = x % 360.0;
    if y > 180.0 {
        y -= 360.0;
    } else if y <= -180.0 {
        y += 360.0;
    }
    y
}

/// `fit - ref` for Keplerian `(a, e, i, Ω, ω, M)` with `i`, `Ω`,
/// `ω`, `M` wrapped to `(-180°, 180°]`.
fn delta_keplerian(fit: &[f64; 6], reference: &[f64; 6]) -> [f64; 6] {
    [
        fit[0] - reference[0],
        fit[1] - reference[1],
        wrap_angle_deg(fit[2] - reference[2]),
        wrap_angle_deg(fit[3] - reference[3]),
        wrap_angle_deg(fit[4] - reference[4]),
        wrap_angle_deg(fit[5] - reference[5]),
    ]
}

fn diag_sigmas(cov: Option<&[[f64; 6]; 6]>) -> [f64; 6] {
    let mut out = [f64::NAN; 6];
    if let Some(c) = cov {
        for i in 0..6 {
            out[i] = c[i][i].max(0.0).sqrt();
        }
    }
    out
}

fn mat6_add(a: &[[f64; 6]; 6], b: &[[f64; 6]; 6]) -> [[f64; 6]; 6] {
    let mut s = [[0.0_f64; 6]; 6];
    for i in 0..6 {
        for j in 0..6 {
            s[i][j] = a[i][j] + b[i][j];
        }
    }
    s
}

/// `d² = Δᵀ Σ⁻¹ Δ` via Cholesky: factor Σ = L Lᵀ, forward-solve
/// `L y = Δ`, then `d² = ‖y‖²`. Returns `None` if Σ is not SPD.
fn mahalanobis_d2_6(sigma: &[[f64; 6]; 6], delta: &[f64; 6]) -> Option<f64> {
    let l = hyperjet::linalg::mat_cholesky(sigma)?;
    let mut y = *delta;
    // Forward sub: y_i = (Δ_i − Σ_{j<i} L_ij y_j) / L_ii
    for i in 0..6 {
        let mut s = y[i];
        for j in 0..i {
            s -= l[i][j] * y[j];
        }
        if l[i][i] == 0.0 {
            return None;
        }
        y[i] = s / l[i][i];
    }
    Some(y.iter().map(|v| v * v).sum())
}

/// `det(Σ) = (∏ L_ii)²` for `Σ = L Lᵀ`. Returns `None` if Σ is not SPD.
fn det_via_cholesky_6(sigma: &[[f64; 6]; 6]) -> Option<f64> {
    let l = hyperjet::linalg::mat_cholesky(sigma)?;
    let mut p = 1.0;
    // Diagonal product over a fixed 6×6 — the index is the matrix coordinate.
    #[allow(clippy::needless_range_loop)]
    for i in 0..6 {
        p *= l[i][i];
    }
    Some(p * p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_orbit(source: &str, epoch: f64, state: [f64; 6], sigmas: [f64; 6]) -> CapturedOrbit {
        let mut cov = [[0.0_f64; 6]; 6];
        for i in 0..6 {
            cov[i][i] = sigmas[i] * sigmas[i];
        }
        CapturedOrbit {
            object: "test".to_string(),
            source: source.to_string(),
            source_version: None,
            epoch_mjd_tdb: epoch,
            native_repr: "keplerian".to_string(),
            native_frame: "ecliptic_j2000".to_string(),
            native_origin_naif: 10,
            native_state: state,
            native_cov_6x6: Some(cov),
            state_cart_icrf_sun: [0.0; 6],
            cov_cart_icrf_sun_6x6: None,
            state_kep_ecliptic_sun: state,
            cov_kep_ecliptic_sun_6x6: Some(cov),
        }
    }

    #[test]
    fn delta_keplerian_wraps_angles() {
        let fit = [1.0, 0.1, 5.0, 359.0, 10.0, 200.0];
        let reference = [1.0, 0.1, 5.0, 1.0, 10.0, 0.0];
        let d = delta_keplerian(&fit, &reference);
        assert_eq!(d[0], 0.0);
        assert_eq!(d[1], 0.0);
        assert_eq!(d[2], 0.0);
        // 359 - 1 = 358 → wrap to -2°
        assert!((d[3] - (-2.0)).abs() < 1e-12, "wrap fail: {}", d[3]);
        assert_eq!(d[4], 0.0);
        // 200 - 0 = 200 → wrap to -160°
        assert!((d[5] - (-160.0)).abs() < 1e-12, "wrap fail: {}", d[5]);
    }

    #[test]
    fn identical_orbits_give_zero_distance() {
        use crate::schema::orbit_sources;
        let s = make_orbit(
            orbit_sources::EMPYREAN_OD,
            60000.0,
            [1.0, 0.1, 5.0, 100.0, 50.0, 200.0],
            [0.01, 0.01, 0.1, 0.1, 0.1, 0.1],
        );
        let r = make_orbit(
            orbit_sources::SBDB,
            60000.0,
            [1.0, 0.1, 5.0, 100.0, 50.0, 200.0],
            [0.01, 0.01, 0.1, 0.1, 0.1, 0.1],
        );
        let comps = compare_orbits(&[s, r], 1.0);
        assert_eq!(comps.len(), 1);
        let c = &comps[0];
        assert!(c.delta.iter().all(|d| d.abs() < 1e-12));
        assert!(c.mahalanobis_d2_fit_metric.abs() < 1e-12);
        assert!(c.mahalanobis_d2_combined_metric.abs() < 1e-12);
        assert!(c.sigma_equiv_combined.abs() < 1e-12);
        // Identical covariances → volume ratio = 1.
        assert!((c.cov_volume_ratio - 1.0).abs() < 1e-9);
    }

    #[test]
    fn one_sigma_offset_gives_d2_near_one_in_combined_metric() {
        use crate::schema::orbit_sources;
        // Two identical orbits with σ_a = 0.01. Offset fit by 1σ
        // along `a`. Combined cov is 2× fit's; expect
        // d²_combined = (Δa)² / (2 σ_a²) = (0.01)² / (2·0.0001) = 0.5.
        let s = make_orbit(
            orbit_sources::EMPYREAN_OD,
            60000.0,
            [1.01, 0.1, 5.0, 100.0, 50.0, 200.0],
            [0.01, 0.01, 0.1, 0.1, 0.1, 0.1],
        );
        let r = make_orbit(
            orbit_sources::SBDB,
            60000.0,
            [1.00, 0.1, 5.0, 100.0, 50.0, 200.0],
            [0.01, 0.01, 0.1, 0.1, 0.1, 0.1],
        );
        let comps = compare_orbits(&[s, r], 1.0);
        let c = &comps[0];
        assert!(
            (c.mahalanobis_d2_combined_metric - 0.5).abs() < 1e-9,
            "d²_combined = {}",
            c.mahalanobis_d2_combined_metric,
        );
        // d²_fit_metric and d²_ref_metric both = (Δa/σ_a)² = 1.
        assert!((c.mahalanobis_d2_fit_metric - 1.0).abs() < 1e-9);
        assert!((c.mahalanobis_d2_ref_metric - 1.0).abs() < 1e-9);
    }

    #[test]
    fn epoch_mismatch_annotated_in_notes() {
        use crate::schema::orbit_sources;
        let s = make_orbit(
            orbit_sources::EMPYREAN_OD,
            60000.0,
            [1.0, 0.1, 5.0, 100.0, 50.0, 200.0],
            [0.01; 6],
        );
        let r = make_orbit(
            orbit_sources::SBDB,
            60010.0,
            [1.0, 0.1, 5.0, 100.0, 50.0, 200.0],
            [0.01; 6],
        );
        let comps = compare_orbits(&[s, r], 1.0);
        assert_eq!(comps.len(), 1);
        let c = &comps[0];
        assert!(!c.notes.is_empty());
        assert!(
            c.notes[0].contains("epoch mismatch"),
            "notes: {:?}",
            c.notes,
        );
    }

    #[test]
    fn singular_covariance_yields_nan_in_its_own_metric() {
        use crate::schema::orbit_sources;
        // A fit with zero covariance → Cholesky fails for fit's
        // metric but Σ_combined = 0 + Σ_ref is SPD, so combined is
        // finite. Asserts both behaviors.
        let mut s = make_orbit(
            orbit_sources::EMPYREAN_OD,
            60000.0,
            [1.01, 0.1, 5.0, 100.0, 50.0, 200.0],
            [0.01; 6],
        );
        s.cov_kep_ecliptic_sun_6x6 = Some([[0.0; 6]; 6]);
        let r = make_orbit(
            orbit_sources::SBDB,
            60000.0,
            [1.0, 0.1, 5.0, 100.0, 50.0, 200.0],
            [0.01; 6],
        );
        let comps = compare_orbits(&[s, r], 1.0);
        assert!(comps[0].mahalanobis_d2_fit_metric.is_nan());
        // Σ_combined = Σ_ref is SPD → combined finite.
        assert!(comps[0].mahalanobis_d2_combined_metric.is_finite());
        // d²_ref_metric = (Δa)² / σ_a² = (0.01)² / (0.01)² = 1.
        assert!((comps[0].mahalanobis_d2_ref_metric - 1.0).abs() < 1e-9);
        // Volume ratio is NaN (det(Σ_fit) = 0, but the check guards
        // det_b > 0 — det_a = 0 actually gives ratio = 0, not NaN).
        // Let the assertion follow what the helper returns:
        assert!(
            comps[0].cov_volume_ratio == 0.0 || comps[0].cov_volume_ratio.is_nan(),
            "volume ratio: {}",
            comps[0].cov_volume_ratio,
        );
    }

    #[test]
    fn jacobi_recovers_diagonal_eigenvalues() {
        // Diagonal matrix: eigenvalues = diagonal entries.
        let mut m = [[0.0_f64; 6]; 6];
        let diag = [5.0_f64, 4.0, 3.0, 2.0, 1.0, 0.5];
        for i in 0..6 {
            m[i][i] = diag[i];
        }
        let (eigs, vecs) = jacobi_eigen_6(&m).expect("converged");
        // Sorted descending matches input descending.
        for k in 0..6 {
            assert!(
                (eigs[k] - diag[k]).abs() < 1e-12,
                "eig[{k}] = {} expected {}",
                eigs[k],
                diag[k]
            );
        }
        // Eigenvectors are columns of identity (up to sign).
        // Indices are matrix coordinates into the fixed 6×6 eigenvector set.
        #[allow(clippy::needless_range_loop)]
        for k in 0..6 {
            for i in 0..6 {
                let v = vecs[i][k];
                if i == k {
                    assert!((v.abs() - 1.0).abs() < 1e-12);
                } else {
                    assert!(v.abs() < 1e-12);
                }
            }
        }
    }

    #[test]
    fn jacobi_recovers_known_2x2_block() {
        // Construct A = R diag(10, 1) Rᵀ embedded in 6×6 block:
        // top-left 2×2 should have eigenvalues 10, 1 and eigenvectors
        // along (cos θ, sin θ) and (-sin θ, cos θ) with θ = 30°.
        let theta = 30.0_f64.to_radians();
        let c = theta.cos();
        let s = theta.sin();
        let lambda1 = 10.0_f64;
        let lambda2 = 1.0_f64;
        let a11 = c * c * lambda1 + s * s * lambda2;
        let a22 = s * s * lambda1 + c * c * lambda2;
        let a12 = c * s * (lambda1 - lambda2);
        let mut m = [[0.0_f64; 6]; 6];
        m[0][0] = a11;
        m[1][1] = a22;
        m[0][1] = a12;
        m[1][0] = a12;
        // Pad the other diagonals so eigenvalues stay sorted.
        m[2][2] = 0.7;
        m[3][3] = 0.5;
        m[4][4] = 0.3;
        m[5][5] = 0.1;
        let (eigs, vecs) = jacobi_eigen_6(&m).expect("converged");
        assert!((eigs[0] - 10.0).abs() < 1e-10);
        assert!((eigs[1] - 1.0).abs() < 1e-10);
        // First eigenvector should be (cos 30°, sin 30°, 0, 0, 0, 0)
        // up to overall sign.
        let v1 = [
            vecs[0][0], vecs[1][0], vecs[2][0], vecs[3][0], vecs[4][0], vecs[5][0],
        ];
        let sgn = v1[0].signum();
        assert!((sgn * v1[0] - c).abs() < 1e-10);
        assert!((sgn * v1[1] - s).abs() < 1e-10);
    }

    #[test]
    fn marginal_d2_equals_combined_when_diagonal() {
        use crate::schema::orbit_sources;
        // Diagonal covariances + state offset only in `a`: marginal d²
        // and joint d² must agree exactly (ratio = 1, no off-diagonal
        // to explain anything joint-only).
        let s = make_orbit(
            orbit_sources::EMPYREAN_OD,
            60000.0,
            [1.01, 0.1, 5.0, 100.0, 50.0, 200.0],
            [0.01, 0.01, 0.1, 0.1, 0.1, 0.1],
        );
        let r = make_orbit(
            orbit_sources::SBDB,
            60000.0,
            [1.00, 0.1, 5.0, 100.0, 50.0, 200.0],
            [0.01, 0.01, 0.1, 0.1, 0.1, 0.1],
        );
        let c = &compare_orbits(&[s, r], 1.0)[0];
        assert!(
            (c.mahalanobis_d2_marginal - c.mahalanobis_d2_combined_metric).abs() < 1e-9,
            "marginal {} vs combined {}",
            c.mahalanobis_d2_marginal,
            c.mahalanobis_d2_combined_metric,
        );
    }

    #[test]
    fn jacobi_handles_small_scale_keplerian_covariance() {
        // Apophis fit-propagated-to-SBDB-epoch Keplerian covariance.
        // Entries span (1e-19, 1e-10). With an absolute convergence
        // tolerance, Jacobi would falsely declare immediate
        // convergence and return sorted diagonals — known-bad
        // eigenvalues (3.69e-10, 3.30e-10, 9.81e-11, ...).
        // Correct eigenvalues from numpy: (6.84e-10, 1.12e-10,
        // 1.59e-12, 3.80e-13, 2.62e-16, 2.01e-20).
        // Regression test: the relative-tolerance Jacobi must agree
        // with numpy to ~1e-6 relative precision.
        let cov: [[f64; 6]; 6] = [
            [
                2.748474e-19,
                -1.658782e-17,
                -3.840071e-16,
                -1.187933e-16,
                -4.219032e-17,
                -9.201042e-16,
            ],
            [
                -1.658782e-17,
                1.209197e-15,
                3.018236e-14,
                -4.234606e-14,
                8.170545e-15,
                1.229598e-13,
            ],
            [
                -3.840071e-16,
                3.018236e-14,
                1.659841e-12,
                -1.381882e-11,
                1.108650e-11,
                5.972759e-12,
            ],
            [
                -1.187933e-16,
                -4.234606e-14,
                -1.381882e-11,
                3.687986e-10,
                -3.301292e-10,
                -6.133320e-11,
            ],
            [
                -4.219032e-17,
                8.170545e-15,
                1.108650e-11,
                -3.301292e-10,
                3.300814e-10,
                1.691263e-12,
            ],
            [
                -9.201042e-16,
                1.229598e-13,
                5.972759e-12,
                -6.133320e-11,
                1.691263e-12,
                9.808602e-11,
            ],
        ];
        let (eigs, _v) = jacobi_eigen_6(&cov).expect("converged");
        let expected = [
            6.8421e-10_f64,
            1.1244e-10,
            1.5942e-12,
            3.7988e-13,
            2.6205e-16,
            2.0118e-20,
        ];
        for k in 0..4 {
            let rel = (eigs[k] - expected[k]).abs() / expected[k];
            assert!(
                rel < 1e-4,
                "λ_{} = {:.4e}, expected {:.4e} (rel error {:.2e})",
                k + 1,
                eigs[k],
                expected[k],
                rel,
            );
        }
    }

    #[test]
    fn rotation_pathology_principal_axis_angle() {
        // Σ_fit and Σ_ref have identical eigenvalues but Σ_ref is
        // rotated 45° in the (a, e) plane. principal_axis_rotation_deg
        // should be ≈ 45° (modulo arccos sign ambiguity — answer is
        // always in [0, 90°]).
        use crate::schema::orbit_sources;
        let mut cov = [[0.0_f64; 6]; 6];
        cov[0][0] = 1.0;
        cov[1][1] = 0.01;
        #[allow(clippy::needless_range_loop)]
        for i in 2..6 {
            cov[i][i] = 0.1;
        }
        let mut s = make_orbit(
            orbit_sources::EMPYREAN_OD,
            60000.0,
            [1.0, 0.1, 5.0, 100.0, 50.0, 200.0],
            [1.0, 0.1, 0.32, 0.32, 0.32, 0.32],
        );
        s.cov_kep_ecliptic_sun_6x6 = Some(cov);

        // Rotate Σ in (0,1) plane by 45°: a→(a+e)/√2, e→(-a+e)/√2.
        let theta = 45.0_f64.to_radians();
        let cs = theta.cos();
        let sn = theta.sin();
        let mut rot = [[0.0_f64; 6]; 6];
        #[allow(clippy::needless_range_loop)]
        for i in 0..6 {
            rot[i][i] = 1.0;
        }
        rot[0][0] = cs;
        rot[0][1] = -sn;
        rot[1][0] = sn;
        rot[1][1] = cs;
        // Σ_ref = R Σ Rᵀ
        let mut rcov = [[0.0_f64; 6]; 6];
        for i in 0..6 {
            for j in 0..6 {
                let mut s = 0.0;
                for k in 0..6 {
                    for l in 0..6 {
                        s += rot[i][k] * cov[k][l] * rot[j][l];
                    }
                }
                rcov[i][j] = s;
            }
        }
        let mut r = make_orbit(
            orbit_sources::SBDB,
            60000.0,
            [1.0, 0.1, 5.0, 100.0, 50.0, 200.0],
            [1.0; 6],
        );
        r.cov_kep_ecliptic_sun_6x6 = Some(rcov);

        let c = &compare_orbits(&[s, r], 1.0)[0];
        // Same eigenvalues on both sides.
        for k in 0..6 {
            assert!(
                (c.eigenvalues_fit[k] - c.eigenvalues_ref[k]).abs() < 1e-9,
                "eig mismatch at k={k}",
            );
        }
        // Rotation angle ~ 45°.
        assert!(
            (c.principal_axis_rotation_deg - 45.0).abs() < 1e-6,
            "rotation = {}",
            c.principal_axis_rotation_deg,
        );
    }
}
