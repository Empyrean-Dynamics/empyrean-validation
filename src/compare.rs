//! Metric helpers shared by every channel runner.
//!
//! Position-error-km, angular-separation-arcsec, and format helpers — so
//! every channel measures the same thing the same way.
//!
//! Lifted verbatim from the original `validation/runners/rust/src/compare.rs`
//! in the empyrean monorepo. Shared here so any future channel runner
//! (e.g., a third-party tool comparison) computes residuals via the
//! same code path.

/// Astronomical Unit in km. Matches IAU 2012 nominal value.
pub const AU_KM: f64 = 149_597_870.700;

/// Position error in km between two SSB ICRF states (AU).
///
/// Inputs are 3-component Cartesian vectors in AU. Output is the
/// Euclidean norm of the difference, converted to km.
pub fn position_error_km(a: &[f64; 3], b: &[f64; 3]) -> f64 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    let dz = a[2] - b[2];
    (dx * dx + dy * dy + dz * dz).sqrt() * AU_KM
}

/// Great-circle angular separation in arcseconds (Vincenty formula).
///
/// Inputs are RA / Dec pairs in radians. Output is the great-circle
/// separation in arcseconds. The Vincenty form is preferred over the
/// simpler Haversine because it is numerically stable for both very
/// small (sub-arcsecond) and very large (anti-pode) separations.
pub fn angular_separation_arcsec(ra1: f64, dec1: f64, ra2: f64, dec2: f64) -> f64 {
    let dra = ra2 - ra1;
    let cos_d1 = dec1.cos();
    let cos_d2 = dec2.cos();
    let sin_d1 = dec1.sin();
    let sin_d2 = dec2.sin();

    let num1 = cos_d2 * dra.sin();
    let num2 = cos_d1 * sin_d2 - sin_d1 * cos_d2 * dra.cos();
    let numerator = (num1 * num1 + num2 * num2).sqrt();
    let denominator = sin_d1 * sin_d2 + cos_d1 * cos_d2 * dra.cos();

    numerator.atan2(denominator).to_degrees() * 3600.0
}

/// Format a distance error in human-friendly units.
///
/// Auto-scales: sub-millimeter → mm, sub-meter → m, sub-kilometer → km
/// with three decimals, then km with one decimal at the kilometer scale.
/// Output is right-padded so columns align in the validation report
/// heatmap.
pub fn fmt_km(km: f64) -> String {
    if km < 0.001 {
        format!("{:>8.1} mm", km * 1e6)
    } else if km < 1.0 {
        format!("{:>8.1}  m", km * 1000.0)
    } else if km < 1000.0 {
        format!("{:>8.3} km", km)
    } else {
        format!("{:>8.1} km", km)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn au_km_matches_iau_2012_nominal() {
        // IAU 2012 nominal: 149_597_870_700 m exactly.
        assert_eq!(AU_KM, 149_597_870.700);
    }

    #[test]
    fn position_error_zero_for_identical_states() {
        let a = [1.0, 2.0, 3.0];
        assert_eq!(position_error_km(&a, &a), 0.0);
    }

    #[test]
    fn position_error_one_au_along_x() {
        let a = [0.0, 0.0, 0.0];
        let b = [1.0, 0.0, 0.0];
        assert_eq!(position_error_km(&a, &b), AU_KM);
    }

    #[test]
    fn angular_separation_zero_at_same_point() {
        let sep = angular_separation_arcsec(1.0, 0.5, 1.0, 0.5);
        assert!(sep.abs() < 1e-9, "got {sep}");
    }

    #[test]
    fn angular_separation_one_degree() {
        // Two points one degree apart at Dec=0 should give 3600″.
        let sep = angular_separation_arcsec(0.0, 0.0, 1.0_f64.to_radians(), 0.0);
        assert!((sep - 3600.0).abs() < 1e-6, "got {sep}");
    }

    #[test]
    fn angular_separation_at_pole() {
        // Two points at the pole — RA is degenerate. Vincenty stays stable.
        let sep = angular_separation_arcsec(
            0.0,
            std::f64::consts::FRAC_PI_2,
            std::f64::consts::PI,
            std::f64::consts::FRAC_PI_2,
        );
        assert!(sep.abs() < 1e-6, "got {sep}");
    }

    #[test]
    fn fmt_km_unit_scaling() {
        assert!(fmt_km(0.0).contains("mm"));
        assert!(fmt_km(1e-7).contains("mm"));
        assert!(fmt_km(0.5).contains("m"));
        assert!(fmt_km(50.0).contains("km"));
        assert!(fmt_km(1234.0).contains("km"));
    }
}
