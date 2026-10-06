//! EFCC2020 star-catalog debiasing for the covariance-realism family.
//!
//! Scoring residuals are computed against **debiased** observed positions,
//! applied centrally and identically for every tool. Historical astrometry
//! was reduced against pre-Gaia star catalogs whose positions carry
//! systematic zonal errors relative to the Gaia frame; the observation
//! inherits the local bias of its reference stars. JPL's `bias.dat`
//! (Eggl, Farnocchia, Chamberlin & Chesley 2020, *Icarus* 339, 113596)
//! tabulates, per HEALPix tile and per catalog, the position bias
//! \( (\Delta\alpha\cos\delta, \Delta\delta) \) in arcsec at J2000.0 and
//! its proper-motion terms in mas/yr; the correction at an observation is
//! bias + pm · (epoch − 2000.0), and debiased = observed − correction.
//!
//! Derived solely from the published paper and its public data product;
//! the format handling mirrors our own engine implementation
//! (`scott::debiasing`), including the same HEALPix crate so harness and
//! engine assign identical tiles.
//!
//! One deliberate difference from the engine's fit-time convention: the
//! engine returns a **zero** correction for catalogs it cannot map
//! (counted in its stats); the scoring kernel must instead put those rows
//! in their own bucket, so [`DebiasTable::correction`] returns `None` for
//! an unmapped or uncovered catalog and `Some` — possibly exactly zero,
//! e.g. the Gaia frame catalogs — only when the table genuinely answers.

use std::io::BufRead;
use std::path::Path;

/// A catalog-bias correction for one observation. Debiased = observed −
/// correction, both components in arcsec, RA already \( \cos\delta \)-scaled.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DebiasCorrection {
    pub dra_arcsec: f64,
    pub ddec_arcsec: f64,
}

/// Per-(tile, catalog) entry: position bias (arcsec at J2000.0) and
/// proper-motion bias (mas/yr).
#[derive(Debug, Clone, Copy)]
struct TileBias {
    d_ra_cos_dec: f32,
    d_dec: f32,
    pm_ra: f32,
    pm_dec: f32,
}

/// The loaded EFCC2020 bias table.
pub struct DebiasTable {
    nside: u32,
    /// Ordered MPC single-character catalog codes from the file header.
    catalog_codes: Vec<char>,
    /// `tiles[pixel * n_catalogs + catalog_index]`.
    tiles: Vec<TileBias>,
}

impl std::fmt::Debug for DebiasTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DebiasTable")
            .field("nside", &self.nside)
            .field("n_catalogs", &self.catalog_codes.len())
            .finish()
    }
}

/// Map ADES `astCat` strings to MPC single-character catalog codes
/// (MPC "Reference catalogues" table; same mapping the engine uses).
fn ades_to_mpc_code(ast_cat: &str) -> Option<char> {
    Some(match ast_cat {
        "USNOA1" => 'a',
        "USNOSA1" => 'b',
        "USNOA2" => 'c',
        "USNOSA2" => 'd',
        "UCAC1" => 'e',
        "Tyc1" => 'f',
        "Tyc2" => 'g',
        "GSC1.0" => 'h',
        "GSC1.1" => 'i',
        "GSC1.2" => 'j',
        "GSC2.2" => 'k',
        "ACT" => 'l',
        "GSCACT" => 'm',
        "SDSS8" => 'n',
        "USNOB1" => 'o',
        "PPM" => 'p',
        "UCAC4" => 'q',
        "UCAC2" => 'r',
        "USNOB2" => 's',
        "PPMXL" => 't',
        "UCAC3" => 'u',
        "NOMAD" => 'v',
        "CMC14" => 'w',
        "Hip2" => 'x',
        "Hip1" => 'y',
        "GSC" => 'z',
        "AC" => 'A',
        "SAO1984" => 'B',
        "SAO" => 'C',
        "AGK3" => 'D',
        "FK4" => 'E',
        "ACRS" => 'F',
        "LickGas" => 'G',
        "Ida93" => 'H',
        "Perth70" => 'I',
        "COSMOS" => 'J',
        "Yale" => 'K',
        "2MASS" => 'L',
        "GSC2.3" => 'M',
        "SDSS7" => 'N',
        "SSTRC1" => 'O',
        "MPOSC3" => 'P',
        "CMC15" => 'Q',
        "SSTRC4" => 'R',
        "URAT1" => 'S',
        "URAT2" => 'T',
        "Gaia1" => 'U',
        "Gaia2" => 'V',
        "Gaia3" => 'W',
        "Gaia3E" => 'X',
        "UCAC5" => 'Y',
        "ATLAS2" => 'Z',
        _ => return None,
    })
}

/// (RA, Dec) in radians → HEALPix RING pixel index. Nested→ring via the
/// same crate and route the engine uses (`cdshealpix::ring::hash` has a
/// south-pole overflow bug in debug builds).
fn ang2pix(ra_rad: f64, dec_rad: f64, nside: u32) -> usize {
    let depth = (nside as f64).log2() as u8;
    let layer = cdshealpix::nested::get(depth);
    layer.to_ring(layer.hash(ra_rad, dec_rad)) as usize
}

impl DebiasTable {
    /// Load `bias.dat` from a data directory (the same file the engine
    /// fetches to `~/.empyrean/data`). Errors loudly on a missing or
    /// malformed table — there is no degraded mode.
    pub fn load(dir: &Path) -> Result<Self, String> {
        let path = dir.join("bias.dat");
        let file = std::fs::File::open(&path).map_err(|e| {
            format!(
                "open {}: {e} — the EFCC2020 table is required unless --no-debias is \
                 passed explicitly",
                path.display()
            )
        })?;
        Self::parse(std::io::BufReader::new(file)).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn parse(reader: impl BufRead) -> Result<Self, String> {
        let mut nside: Option<u32> = None;
        let mut catalog_codes: Vec<char> = Vec::new();
        let mut tiles: Vec<TileBias> = Vec::new();
        for (lineno, line) in reader.lines().enumerate() {
            let line = line.map_err(|e| format!("line {}: {e}", lineno + 1))?;
            let t = line.trim();
            if t.is_empty() {
                continue;
            }
            if let Some(rest) = t.strip_prefix('!') {
                let rest = rest.trim();
                if let Some(v) = rest.strip_prefix("NSIDE=") {
                    nside = Some(
                        v.trim()
                            .parse()
                            .map_err(|e| format!("line {}: bad NSIDE: {e}", lineno + 1))?,
                    );
                } else if nside.is_some() && catalog_codes.is_empty() {
                    // The catalog-code line is the only header whose tokens
                    // are all single characters ("! c d e …").
                    let tokens: Vec<&str> = rest.split_whitespace().collect();
                    if !tokens.is_empty() && tokens.iter().all(|t| t.len() == 1) {
                        catalog_codes = tokens.iter().map(|t| t.chars().next().unwrap()).collect();
                    }
                }
                continue;
            }
            if catalog_codes.is_empty() {
                return Err(format!(
                    "line {}: data before the catalog-code header",
                    lineno + 1
                ));
            }
            let values: Vec<f32> = t
                .split_whitespace()
                .map(|s| s.parse().map_err(|e| format!("line {}: {e}", lineno + 1)))
                .collect::<Result<_, _>>()?;
            if values.len() != catalog_codes.len() * 4 {
                return Err(format!(
                    "line {}: {} values for {} catalogs (want {})",
                    lineno + 1,
                    values.len(),
                    catalog_codes.len(),
                    catalog_codes.len() * 4
                ));
            }
            for c in 0..catalog_codes.len() {
                tiles.push(TileBias {
                    d_ra_cos_dec: values[c * 4],
                    d_dec: values[c * 4 + 1],
                    pm_ra: values[c * 4 + 2],
                    pm_dec: values[c * 4 + 3],
                });
            }
        }
        let nside = nside.ok_or("missing '! NSIDE=' header")?;
        let npix = 12 * nside as usize * nside as usize;
        let actual = tiles.len() / catalog_codes.len().max(1);
        if actual != npix {
            return Err(format!(
                "{actual} tiles for NSIDE={nside} (want {npix}) — truncated or corrupt table"
            ));
        }
        Ok(Self {
            nside,
            catalog_codes,
            tiles,
        })
    }

    /// The correction at (RA, Dec, epoch) for an ADES `astCat`, or `None`
    /// when the catalog is unmapped or not a column of the loaded table —
    /// the scoring kernel buckets those rows, it never silently skips
    /// them. Frame catalogs (Gaia) return `Some` with a zero-ish value:
    /// the table answered, and the answer is "no bias".
    pub fn correction(
        &self,
        ra_deg: f64,
        dec_deg: f64,
        epoch_mjd_utc: f64,
        ast_cat: &str,
    ) -> Option<DebiasCorrection> {
        // The Gaia catalogs ARE the EFCC2020 reference frame: their
        // correction is identically zero by construction, and the 2018
        // table omits their columns entirely. Answer Some(0) — the frame
        // — rather than bucketing them as unknown.
        if matches!(ast_cat, "Gaia1" | "Gaia2" | "Gaia3" | "Gaia3E") {
            return Some(DebiasCorrection {
                dra_arcsec: 0.0,
                ddec_arcsec: 0.0,
            });
        }
        let code = ades_to_mpc_code(ast_cat)?;
        let cat_idx = self.catalog_codes.iter().position(|&c| c == code)?;
        let pixel = ang2pix(ra_deg.to_radians(), dec_deg.to_radians(), self.nside);
        let tile = self.tiles[pixel * self.catalog_codes.len() + cat_idx];
        // Julian year from MJD; UTC-vs-TDB matters at mas/yr rates by
        // ~2e-6 mas — irrelevant.
        let dt_years = (epoch_mjd_utc - 51_544.5) / 365.25;
        Some(DebiasCorrection {
            dra_arcsec: f64::from(tile.d_ra_cos_dec) + dt_years * f64::from(tile.pm_ra) / 1000.0,
            ddec_arcsec: f64::from(tile.d_dec) + dt_years * f64::from(tile.pm_dec) / 1000.0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// NSIDE=1 (12 tiles), two catalogs: 'c' (USNO-A2) with a distinct
    /// bias per tile, 'V' (Gaia-DR2) all zero.
    fn mini_table() -> DebiasTable {
        let mut s = String::from("! BIAS_VERSION= test\n! NSIDE= 1\n! c V\n");
        for pix in 0..12 {
            // catalog c: dRA*cosDec = pix*0.1, dDec = -pix*0.05,
            // pmRA = 10 mas/yr, pmDec = -20 mas/yr; catalog V: zeros.
            s.push_str(&format!(
                "{:.3} {:.3} 10.0 -20.0 0.0 0.0 0.0 0.0\n",
                pix as f64 * 0.1,
                -(pix as f64) * 0.05,
            ));
        }
        DebiasTable::parse(std::io::Cursor::new(s)).unwrap()
    }

    #[test]
    fn parses_and_indexes() {
        let t = mini_table();
        assert_eq!(t.nside, 1);
        assert_eq!(t.catalog_codes, vec!['c', 'V']);
        assert_eq!(t.tiles.len(), 24);
    }

    #[test]
    fn proper_motion_epoch_arithmetic() {
        let t = mini_table();
        // J2000.0 is MJD 51544.5: the correction is the position bias
        // alone; +10 Julian years adds pm/1000 * 10.
        let pix = ang2pix(0.3, 0.2, 1);
        let at_j2000 = t
            .correction(
                0.3_f64.to_degrees(),
                0.2_f64.to_degrees(),
                51_544.5,
                "USNOA2",
            )
            .unwrap();
        assert!((at_j2000.dra_arcsec - pix as f64 * 0.1).abs() < 1e-6);
        assert!((at_j2000.ddec_arcsec + pix as f64 * 0.05).abs() < 1e-6);
        let later = t
            .correction(
                0.3_f64.to_degrees(),
                0.2_f64.to_degrees(),
                51_544.5 + 10.0 * 365.25,
                "USNOA2",
            )
            .unwrap();
        assert!((later.dra_arcsec - at_j2000.dra_arcsec - 0.1).abs() < 1e-6);
        assert!((later.ddec_arcsec - at_j2000.ddec_arcsec + 0.2).abs() < 1e-6);
    }

    #[test]
    fn frame_catalog_answers_zero_unknown_answers_none() {
        let t = mini_table();
        // Gaia-DR2 is a table column: the answer is Some(0) — the frame.
        let g = t.correction(10.0, 5.0, 60_000.0, "Gaia2").unwrap();
        assert_eq!((g.dra_arcsec, g.ddec_arcsec), (0.0, 0.0));
        // UNK is unmapped, and UCAC4 maps to 'q' which is not a column of
        // this table: both are None — bucketed, never silently zero.
        assert!(t.correction(10.0, 5.0, 60_000.0, "UNK").is_none());
        assert!(t.correction(10.0, 5.0, 60_000.0, "UCAC4").is_none());
    }

    #[test]
    fn ang2pix_poles_and_equator() {
        // NSIDE=1 RING ordering: pixels 0–3 are the north polar cap,
        // 4–7 the equatorial belt, 8–11 the south cap.
        assert!(ang2pix(0.1, 1.5, 1) < 4);
        assert!((4..8).contains(&ang2pix(0.1, 0.0, 1)));
        assert!(ang2pix(0.1, -1.5, 1) >= 8);
    }

    #[test]
    fn truncated_table_is_a_loud_error() {
        let s = "! NSIDE= 1\n! c V\n0 0 0 0 0 0 0 0\n";
        let err = DebiasTable::parse(std::io::Cursor::new(s)).unwrap_err();
        assert!(err.contains("truncated"), "{err}");
        let s = "! c V\n";
        assert!(DebiasTable::parse(std::io::Cursor::new(s)).is_err());
    }

    #[test]
    #[ignore = "needs ~/.empyrean/data/bias.dat (fetched by the engine)"]
    fn real_table_sanity() {
        let dir = dirs_home().join(".empyrean/data");
        let t = DebiasTable::load(&dir).unwrap();
        assert_eq!(t.nside, 64);
        // Probe a few sky positions / catalogs: corrections exist and are
        // sub-2″ (the EFCC2020 biases are sub-arcsecond except for a few
        // legacy-catalog tiles).
        for (cat, expect_some) in [
            ("USNOA2", true),
            ("UCAC4", true),
            ("Gaia2", true),
            ("UNK", false),
        ] {
            for (ra, dec) in [(10.0, 5.0), (200.0, -40.0), (355.0, 80.0)] {
                let c = t.correction(ra, dec, 58_000.0, cat);
                assert_eq!(c.is_some(), expect_some, "{cat} @ ({ra}, {dec})");
                if let Some(c) = c {
                    assert!(
                        c.dra_arcsec.abs() < 2.0 && c.ddec_arcsec.abs() < 2.0,
                        "{cat} @ ({ra}, {dec}): {c:?}"
                    );
                }
            }
        }
    }

    fn dirs_home() -> std::path::PathBuf {
        std::path::PathBuf::from(std::env::var("HOME").unwrap())
    }
}
