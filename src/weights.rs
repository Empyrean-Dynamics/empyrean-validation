//! Pinned scoring-noise tables for the covariance-realism family.
//!
//! The scoring σ is a property of the **protocol**, not of any tool: every
//! runner's predictions are scored against the same per-observation
//! measurement covariance, resolved as ADES `rmsRA`/`rmsDec` where present,
//! else the Vereš–Farnocchia–Chesley–Chamberlin 2017 station floor, else
//! [`DEFAULT_SIGMA_ARCSEC`]. Nightly de-weighting is deliberately absent —
//! its \( N \) is undefined for a held-out night.
//!
//! Two deliberate differences from the engine's fit-time weighting:
//!
//! - resolution is ADES-**first** and un-floored (the engine floors reported
//!   σ against the station value; scoring wants the reported measurement
//!   noise where it exists — the table-vs-ADES disagreement is exactly what
//!   the `ades_only` sensitivity pass measures);
//! - there is no nightly layer and no scale factor.

/// Fallback scoring σ (arcsec) when neither ADES nor a station floor covers
/// a row. Documented in the manifest; its dominance over the corpus (~56% of
/// rows) is exactly why the realism headline is restricted to the
/// Σ_pred-dominated subset.
pub const DEFAULT_SIGMA_ARCSEC: f64 = 1.0;

/// Station-only "ALL catalogs" simplified floors from Vereš, Farnocchia,
/// Chesley & Chamberlin (2017), *Icarus* 296, 139–149,
/// <https://doi.org/10.1016/j.icarus.2017.05.021> — the same frozen paper
/// snapshot the engine carries (`scott::weighting::VFCC2017_STATION_FLOORS`;
/// copied verbatim from our own table, not transcribed from any external
/// tool). Per-catalog Table 3 refinements are not applied. Each entry is
/// `(obs_code, sigma_arcsec)`, applied to both RA·cos δ and Dec.
pub const VFCC2017_STATION_FLOORS: &[(&str, f64)] = &[
    ("F51", 0.2),
    ("F52", 0.2),
    ("703", 1.0),
    ("G96", 0.5),
    ("V06", 0.4),
    ("I52", 0.3),
    ("291", 0.6),
    ("691", 0.6),
    ("T05", 0.5),
    ("T08", 0.5),
    ("W68", 0.5),
    ("M22", 0.5),
    ("W85", 0.5),
    ("W86", 0.5),
    ("W87", 0.5),
    ("I41", 0.4),
    ("568", 0.3),
    ("H01", 0.3),
    ("G37", 0.8),
    ("Z84", 0.3),
    ("J04", 0.3),
    ("W84", 0.25),
    ("I11", 0.3),
    ("704", 0.7),
    ("G45", 0.5),
    ("C51", 1.0),
    ("W74", 1.0),
    ("F65", 1.0),
    ("T12", 0.8),
    ("Z23", 1.0),
    ("095", 1.0),
];

/// A resolved per-observation scoring σ.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScoringSigma {
    /// σ for \( \alpha\cos\delta \), arcsec.
    pub ra_arcsec: f64,
    /// σ for \( \delta \), arcsec.
    pub dec_arcsec: f64,
    /// Correlation, dimensionless.
    pub corr: f64,
    /// Provenance: `"ades"`, `"vfc17"`, or `"default"`.
    pub source: &'static str,
}

/// VFC17 station floor (arcsec), when the station is in the table.
pub fn vfc17_station_floor(stn: &str) -> Option<f64> {
    VFCC2017_STATION_FLOORS
        .iter()
        .find(|(code, _)| *code == stn)
        .map(|(_, s)| *s)
}

/// Resolve the pinned scoring σ for one observation: ADES where present and
/// positive, else the VFC17 station floor, else the default. A
/// non-positive ADES value is malformed input and falls through — the
/// `sigma_source` on the manifest row records which branch fired, so the
/// fall-through is visible, not silent.
pub fn pinned_scoring_sigma(
    stn: &str,
    ades_rms_ra: Option<f64>,
    ades_rms_dec: Option<f64>,
    ades_rms_corr: Option<f64>,
) -> ScoringSigma {
    match (ades_rms_ra, ades_rms_dec) {
        (Some(ra), Some(dec)) if ra > 0.0 && dec > 0.0 && ra.is_finite() && dec.is_finite() => {
            ScoringSigma {
                ra_arcsec: ra,
                dec_arcsec: dec,
                corr: ades_rms_corr
                    .filter(|c| c.is_finite() && c.abs() < 1.0)
                    .unwrap_or(0.0),
                source: "ades",
            }
        }
        _ => match vfc17_station_floor(stn) {
            Some(floor) => ScoringSigma {
                ra_arcsec: floor,
                dec_arcsec: floor,
                corr: 0.0,
                source: "vfc17",
            },
            None => ScoringSigma {
                ra_arcsec: DEFAULT_SIGMA_ARCSEC,
                dec_arcsec: DEFAULT_SIGMA_ARCSEC,
                corr: 0.0,
                source: "default",
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_order_is_ades_then_floor_then_default() {
        let s = pinned_scoring_sigma("F51", Some(0.15), Some(0.12), Some(0.3));
        assert_eq!(
            (s.ra_arcsec, s.dec_arcsec, s.corr, s.source),
            (0.15, 0.12, 0.3, "ades")
        );
        let s = pinned_scoring_sigma("F51", None, None, None);
        assert_eq!((s.ra_arcsec, s.source), (0.2, "vfc17"));
        let s = pinned_scoring_sigma("X99", None, None, None);
        assert_eq!((s.ra_arcsec, s.source), (DEFAULT_SIGMA_ARCSEC, "default"));
    }

    #[test]
    fn degenerate_ades_values_fall_through() {
        // Zero, negative, or non-finite reported σ is malformed — the floor
        // takes over and sigma_source says so.
        for bad in [0.0, -0.3, f64::NAN, f64::INFINITY] {
            let s = pinned_scoring_sigma("703", Some(bad), Some(0.5), None);
            assert_eq!((s.ra_arcsec, s.source), (1.0, "vfc17"), "bad = {bad}");
        }
        // A degenerate correlation is dropped, not propagated.
        let s = pinned_scoring_sigma("F51", Some(0.2), Some(0.2), Some(1.5));
        assert_eq!(s.corr, 0.0);
    }

    #[test]
    fn known_station_floors() {
        assert_eq!(vfc17_station_floor("703"), Some(1.0));
        assert_eq!(vfc17_station_floor("W84"), Some(0.25));
        assert_eq!(vfc17_station_floor("T05"), Some(0.5));
        assert_eq!(vfc17_station_floor("ZZZ"), None);
    }
}
