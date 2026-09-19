//! Synthetic (perfect-model) lane of the covariance-realism family.
//!
//! The real-data walk measures the *joint* realism of estimator, force
//! model, and astrometric-error model. This lane removes the last two by
//! construction: every object keeps its real observation times and
//! stations, the "observed" astrometry is the engine's own prediction from
//! a full-arc truth orbit plus noise of a **known** per-station σ, and that
//! σ is what the rows report. Under Gaussian noise the held-out \( d^2 \)
//! must be \( \chi^2_2 \); whatever departure remains is the estimator and
//! the transport alone — the baseline every real-data scaling/weighting
//! scheme is measured against.
//!
//! Engine calls (truth fit, truth ephemeris) live in the rust runner; this
//! module is the engine-free half: σ assignment, deterministic noise draws,
//! the tangent-plane offset, the PSV rewrite, and the provenance record.
//!
//! Conventions fixed here and consumed downstream:
//!
//! - offsets are drawn in the tangent plane \( (\Delta\alpha\cos\delta,
//!   \Delta\delta) \) and applied with the exact inverse of the scoring
//!   kernel's gnomonic projection, so the injected offset **is** the
//!   residual the kernel recovers, to floating-point precision;
//! - `rmsRA`/`rmsDec` carry the injected σ (ADES: `rmsRA` is already
//!   \( \cos\delta \)-scaled), `rmsCorr` is left blank (independent axes);
//! - `astCat` is [`SYNTHETIC_AST_CAT`] — the EFCC2020 reference frame, whose
//!   correction is identically zero at fit time and at scoring time, so the
//!   debias axis collapses instead of injecting a bias into bias-free data;
//! - space-based rows (`sys`/`pos1` populated) are dropped: the walk
//!   excludes them from fits and targets anyway, and the truth ephemeris is
//!   generated per ground station.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::weights::vfc17_station_floor;

/// Default fiducial σ (arcsec) for every station without a survey entry.
pub const FIDUCIAL_SIGMA_ARCSEC: f64 = 0.2;

/// Catalog label written on every synthetic row: the EFCC2020 reference
/// frame, so both the fit-time and the scoring-time debias are exactly 0.
pub const SYNTHETIC_AST_CAT: &str = "Gaia2";

/// Provenance tags for the injected σ.
pub mod sigma_sources {
    /// Station is in the VFC17 survey table; its published value was used.
    pub const SURVEY_VFC17: &str = "survey_vfc17";
    /// Every other station: the single fiducial σ.
    pub const FIDUCIAL: &str = "fiducial";
}

/// The noise law the offsets are drawn from. Every law is scaled to the
/// **same variance** \( \sigma^2 \) per axis, and the rows always report
/// that Gaussian-equivalent σ — the heavy-tailed variant tests what each
/// rejection scheme does with tails, not with a mis-stated σ.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "law", rename_all = "snake_case")]
pub enum NoiseModel {
    /// \( \mathcal N(0, \sigma^2) \) per axis, independent.
    Gaussian,
    /// Student's \( t_\nu \) per axis, independent, scaled by
    /// \( \sqrt{(\nu-2)/\nu} \) so the variance is \( \sigma^2 \).
    /// Requires \( \nu \ge 3 \) (finite variance).
    StudentT { nu: u32 },
}

impl NoiseModel {
    /// Parse `gaussian` | `student-t` (with `nu`).
    pub fn parse(law: &str, nu: Option<u32>) -> Result<Self, String> {
        match law {
            "gaussian" | "normal" => Ok(Self::Gaussian),
            "student-t" | "student_t" | "t" => {
                let nu = nu.ok_or("student-t noise needs --nu")?;
                if nu < 3 {
                    return Err(format!(
                        "student-t nu = {nu}: the variance is infinite below nu = 3, so no \
                         Gaussian-equivalent sigma exists to report"
                    ));
                }
                Ok(Self::StudentT { nu })
            }
            other => Err(format!(
                "unknown noise law {other:?} (gaussian | student-t)"
            )),
        }
    }

    /// Short tag for file names and series labels: `gaussian`, `t4`, …
    pub fn tag(self) -> String {
        match self {
            Self::Gaussian => "gaussian".into(),
            Self::StudentT { nu } => format!("t{nu}"),
        }
    }

    /// Multiplier taking a unit-scale draw of the law to unit variance.
    pub fn unit_variance_scale(self) -> f64 {
        match self {
            Self::Gaussian => 1.0,
            Self::StudentT { nu } => ((f64::from(nu) - 2.0) / f64::from(nu)).sqrt(),
        }
    }
}

/// The σ injected on one row and where it came from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InjectedSigma {
    pub arcsec: f64,
    pub source: &'static str,
}

/// Per-station injected σ: the VFC17 published value for survey stations,
/// the fiducial for everyone else. The survey values coincide with the
/// engine's VFCC2017 floor, so a fit under the floor policy would not
/// re-floor them; the fiducial (0.2″) is below the preset's 1″ default
/// floor, which is why synthetic fits run with `sigma_policy = reported`.
pub fn injected_sigma(stn: &str, fiducial_arcsec: f64) -> InjectedSigma {
    match vfc17_station_floor(stn) {
        Some(s) => InjectedSigma {
            arcsec: s,
            source: sigma_sources::SURVEY_VFC17,
        },
        None => InjectedSigma {
            arcsec: fiducial_arcsec,
            source: sigma_sources::FIDUCIAL,
        },
    }
}

/// Deterministic per-row seed: FNV-1a over the global seed and the row's
/// identity, so a row's draw depends on nothing but its own key — file
/// order, object order, and thread scheduling cannot change it.
/// `occurrence` is the row's ordinal among rows sharing the same
/// (station, obsTime): MPC data legitimately carries such duplicates, and
/// two of them must not receive the same draw (identical positions
/// collide in the engine's observation ids).
pub fn row_seed(global_seed: u64, object: &str, stn: &str, obs_time: &str, occurrence: u32) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut feed = |bytes: &[u8]| {
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x100000001b3);
        }
        h ^= 0xff;
        h = h.wrapping_mul(0x100000001b3);
    };
    feed(&global_seed.to_le_bytes());
    feed(object.as_bytes());
    feed(stn.as_bytes());
    feed(obs_time.as_bytes());
    feed(&occurrence.to_le_bytes());
    h
}

/// A small deterministic generator (PCG-style LCG + Box–Muller), the same
/// family the walk runner uses for its Monte-Carlo variants. Not a crypto
/// generator; it is a reproducible one.
#[derive(Debug, Clone)]
pub struct RowRng {
    state: u64,
    spare: Option<f64>,
}

impl RowRng {
    pub fn new(seed: u64) -> Self {
        let mut r = Self {
            state: seed.max(1),
            spare: None,
        };
        // Burn a few outputs so nearby seeds decorrelate.
        for _ in 0..4 {
            r.uniform();
        }
        r
    }

    /// Uniform on \( (0, 1) \).
    pub fn uniform(&mut self) -> f64 {
        self.state = self
            .state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (((self.state >> 11) as f64) + 0.5) / ((1u64 << 53) as f64)
    }

    /// Standard normal.
    pub fn normal(&mut self) -> f64 {
        if let Some(z) = self.spare.take() {
            return z;
        }
        let (u1, u2) = (self.uniform(), self.uniform());
        let r = (-2.0 * u1.ln()).sqrt();
        let (sn, cs) = (2.0 * std::f64::consts::PI * u2).sin_cos();
        self.spare = Some(r * sn);
        r * cs
    }

    /// Student's \( t_\nu \): \( z / \sqrt{\chi^2_\nu / \nu} \) with the
    /// \( \chi^2_\nu \) built from \( \nu \) independent normals.
    pub fn student_t(&mut self, nu: u32) -> f64 {
        let z = self.normal();
        let chi2: f64 = (0..nu).map(|_| self.normal().powi(2)).sum();
        z / (chi2 / f64::from(nu)).sqrt()
    }

    /// One draw of the law at unit variance.
    pub fn unit_draw(&mut self, model: NoiseModel) -> f64 {
        match model {
            NoiseModel::Gaussian => self.normal(),
            NoiseModel::StudentT { nu } => self.student_t(nu) * model.unit_variance_scale(),
        }
    }
}

/// Independent per-axis offsets \( (\Delta\alpha\cos\delta, \Delta\delta) \)
/// in arcsec with standard deviation `sigma_arcsec` each.
pub fn draw_offset_arcsec(rng: &mut RowRng, model: NoiseModel, sigma_arcsec: f64) -> (f64, f64) {
    let dx = sigma_arcsec * rng.unit_draw(model);
    let dy = sigma_arcsec * rng.unit_draw(model);
    (dx, dy)
}

const RAD2ARCSEC: f64 = 3600.0 * 180.0 / std::f64::consts::PI;

/// Apply a tangent-plane offset (east, north; arcsec) about the nominal
/// \( (\alpha_0, \delta_0) \) in degrees — the exact inverse of the
/// gnomonic projection the scoring kernel uses: the direction is
/// \( \hat u \propto \hat p + \xi\,\hat e + \eta\,\hat n \), so projecting
/// the result back onto the plane at \( \hat p \) returns exactly
/// \( (\xi, \eta) \). RA is returned on \( [0, 360) \).
pub fn offset_position(
    ra0_deg: f64,
    dec0_deg: f64,
    east_arcsec: f64,
    north_arcsec: f64,
) -> (f64, f64) {
    let (a0, d0) = (ra0_deg.to_radians(), dec0_deg.to_radians());
    let p = [d0.cos() * a0.cos(), d0.cos() * a0.sin(), d0.sin()];
    let e = [-a0.sin(), a0.cos(), 0.0];
    let n = [-d0.sin() * a0.cos(), -d0.sin() * a0.sin(), d0.cos()];
    let (xi, eta) = (east_arcsec / RAD2ARCSEC, north_arcsec / RAD2ARCSEC);
    let u = [
        p[0] + xi * e[0] + eta * n[0],
        p[1] + xi * e[1] + eta * n[1],
        p[2] + xi * e[2] + eta * n[2],
    ];
    let norm = (u[0] * u[0] + u[1] * u[1] + u[2] * u[2]).sqrt();
    let (x, y, z) = (u[0] / norm, u[1] / norm, u[2] / norm);
    let mut ra = y.atan2(x).to_degrees();
    if ra < 0.0 {
        ra += 360.0;
    }
    let dec = z.clamp(-1.0, 1.0).asin().to_degrees();
    (ra, dec)
}

/// Gnomonic (east, north) offset in arcsec of `(ra, dec)` about the nominal
/// — the scoring kernel's forward projection, kept here so the inverse
/// above is pinned against it by test and so callers can self-check the
/// injected offset they wrote.
pub fn gnomonic_offset_arcsec(
    nom_ra_deg: f64,
    nom_dec_deg: f64,
    ra_deg: f64,
    dec_deg: f64,
) -> Option<(f64, f64)> {
    let unit = |ra: f64, dec: f64| -> [f64; 3] {
        let (a, d) = (ra.to_radians(), dec.to_radians());
        [d.cos() * a.cos(), d.cos() * a.sin(), d.sin()]
    };
    let u = unit(ra_deg, dec_deg);
    let p = unit(nom_ra_deg, nom_dec_deg);
    let dot = u[0] * p[0] + u[1] * p[1] + u[2] * p[2];
    if dot <= 0.0 {
        return None;
    }
    let (a0, d0) = (nom_ra_deg.to_radians(), nom_dec_deg.to_radians());
    let east = [-a0.sin(), a0.cos(), 0.0];
    let north = [-d0.sin() * a0.cos(), -d0.sin() * a0.sin(), d0.cos()];
    let t = [u[0] / dot, u[1] / dot, u[2] / dot];
    Some((
        (t[0] * east[0] + t[1] * east[1] + t[2] * east[2]) * RAD2ARCSEC,
        (t[0] * north[0] + t[1] * north[1] + t[2] * north[2]) * RAD2ARCSEC,
    ))
}

// ---------------------------------------------------------------------------
// PSV table: parse, drop space-based rows, rewrite astrometry columns.
// ---------------------------------------------------------------------------

/// A parsed ADES PSV: preamble lines (`# version=…`), the header, and the
/// data rows as trimmed string cells. Cell order is the header's.
#[derive(Debug, Clone, PartialEq)]
pub struct PsvTable {
    pub preamble: Vec<String>,
    pub header: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl PsvTable {
    /// Parse the fixture text. Lines before the header (the one containing
    /// `obsTime`) are kept verbatim as the preamble.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut preamble = Vec::new();
        let mut header: Option<Vec<String>> = None;
        let mut rows = Vec::new();
        for (lineno, line) in text.lines().enumerate() {
            let line = line.trim_end();
            if line.is_empty() {
                continue;
            }
            match &header {
                None => {
                    let cells: Vec<&str> = line.split('|').map(str::trim).collect();
                    if cells.contains(&"obsTime") {
                        header = Some(cells.iter().map(|s| s.to_string()).collect());
                    } else {
                        preamble.push(line.to_string());
                    }
                }
                Some(h) => {
                    let cells: Vec<String> =
                        line.split('|').map(|c| c.trim().to_string()).collect();
                    if cells.len() != h.len() {
                        return Err(format!(
                            "line {}: {} cells for a {}-column header",
                            lineno + 1,
                            cells.len(),
                            h.len()
                        ));
                    }
                    rows.push(cells);
                }
            }
        }
        let header = header.ok_or("no PSV header line (none contains obsTime)")?;
        for required in ["stn", "obsTime", "ra", "dec"] {
            if !header.iter().any(|h| h == required) {
                return Err(format!("header lacks the {required} column"));
            }
        }
        Ok(Self {
            preamble,
            header,
            rows,
        })
    }

    pub fn col(&self, name: &str) -> Option<usize> {
        self.header.iter().position(|h| h == name)
    }

    fn cell<'a>(&self, row: &'a [String], name: &str) -> Option<&'a str> {
        self.col(name).map(|i| row[i].as_str())
    }

    /// The generator's rule: a populated `sys` or `pos1` marks a
    /// space-based (or roving) observation.
    pub fn is_space_based(&self, row: &[String]) -> bool {
        self.cell(row, "sys").is_some_and(|v| !v.is_empty())
            || self.cell(row, "pos1").is_some_and(|v| !v.is_empty())
    }

    /// Drop space-based rows; returns how many were dropped.
    pub fn drop_space_based(&mut self) -> usize {
        let before = self.rows.len();
        let keep: Vec<Vec<String>> = self
            .rows
            .iter()
            .filter(|r| !self.is_space_based(r))
            .cloned()
            .collect();
        self.rows = keep;
        before - self.rows.len()
    }

    /// Ensure the named columns exist (appended right after `dec` when
    /// missing, in the given order), with empty cells on every row.
    pub fn ensure_columns_after_dec(&mut self, names: &[&str]) {
        let mut insert_at = self.col("dec").expect("dec column verified at parse") + 1;
        for name in names {
            if self.col(name).is_some() {
                continue;
            }
            self.header.insert(insert_at, (*name).to_string());
            for row in &mut self.rows {
                row.insert(insert_at, String::new());
            }
            insert_at += 1;
        }
    }

    /// Identity of one row for the synthetic pass: `(stn, obsTime)`.
    pub fn row_identity(&self, idx: usize) -> (&str, &str) {
        let row = &self.rows[idx];
        (
            self.cell(row, "stn").unwrap_or(""),
            self.cell(row, "obsTime").unwrap_or(""),
        )
    }

    /// Render back to PSV text: preamble, header, rows — cells joined by
    /// `|` with one space of padding, no column width alignment (every
    /// consumer trims cells).
    pub fn render(&self) -> String {
        let mut out = String::new();
        for l in &self.preamble {
            out.push_str(l);
            out.push('\n');
        }
        out.push_str(&self.header.join("|"));
        out.push('\n');
        for row in &self.rows {
            out.push_str(&row.join("|"));
            out.push('\n');
        }
        out
    }
}

/// One synthesized row: what was injected and what was written.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyntheticRow {
    pub stn: String,
    pub obs_time: String,
    /// The engine's truth prediction at this (station, time), degrees.
    pub ra_true_deg: f64,
    pub dec_true_deg: f64,
    /// Injected σ per axis (arcsec) and its provenance.
    pub sigma_arcsec: f64,
    pub sigma_source: String,
    /// Injected tangent-plane offset (east, north), arcsec.
    pub dx_arcsec: f64,
    pub dy_arcsec: f64,
    /// The written ("observed") position, degrees.
    pub ra_syn_deg: f64,
    pub dec_syn_deg: f64,
}

/// Draw noise for every row of `table` (which must already have its
/// space-based rows dropped) against the aligned truth positions, and
/// write the synthetic astrometry back into the table.
pub fn synthesize_table(
    object: &str,
    table: &mut PsvTable,
    truth_radec_deg: &[(f64, f64)],
    model: NoiseModel,
    fiducial_sigma_arcsec: f64,
    global_seed: u64,
) -> Result<Vec<SyntheticRow>, String> {
    if truth_radec_deg.len() != table.rows.len() {
        return Err(format!(
            "{object}: {} truth positions for {} PSV rows",
            truth_radec_deg.len(),
            table.rows.len()
        ));
    }
    table.ensure_columns_after_dec(&["rmsRA", "rmsDec", "rmsCorr"]);
    table.ensure_columns_after_dec(&["astCat"]);
    let (c_ra, c_dec) = (table.col("ra").unwrap(), table.col("dec").unwrap());
    let (c_rra, c_rdec, c_corr) = (
        table.col("rmsRA").unwrap(),
        table.col("rmsDec").unwrap(),
        table.col("rmsCorr").unwrap(),
    );
    let c_cat = table.col("astCat").unwrap();

    let mut out = Vec::with_capacity(table.rows.len());
    let mut seen: BTreeMap<(String, String), u32> = BTreeMap::new();
    for (i, &(ra_true, dec_true)) in truth_radec_deg.iter().enumerate() {
        let (stn, obs_time) = {
            let (s, t) = table.row_identity(i);
            (s.to_string(), t.to_string())
        };
        let occurrence = {
            let c = seen.entry((stn.clone(), obs_time.clone())).or_insert(0);
            let k = *c;
            *c += 1;
            k
        };
        if !ra_true.is_finite() || !dec_true.is_finite() {
            return Err(format!(
                "{object}: non-finite truth position for {stn} @ {obs_time}"
            ));
        }
        let sigma = injected_sigma(&stn, fiducial_sigma_arcsec);
        let mut rng = RowRng::new(row_seed(global_seed, object, &stn, &obs_time, occurrence));
        let (dx, dy) = draw_offset_arcsec(&mut rng, model, sigma.arcsec);
        let (ra_syn, dec_syn) = offset_position(ra_true, dec_true, dx, dy);
        let row = &mut table.rows[i];
        row[c_ra] = format!("{ra_syn:.9}");
        row[c_dec] = format!("{dec_syn:.9}");
        row[c_rra] = format!("{:.4}", sigma.arcsec);
        row[c_rdec] = format!("{:.4}", sigma.arcsec);
        row[c_corr].clear();
        row[c_cat] = SYNTHETIC_AST_CAT.to_string();
        out.push(SyntheticRow {
            stn,
            obs_time,
            ra_true_deg: ra_true,
            dec_true_deg: dec_true,
            sigma_arcsec: sigma.arcsec,
            sigma_source: sigma.source.to_string(),
            dx_arcsec: dx,
            dy_arcsec: dy,
            ra_syn_deg: ra_syn,
            dec_syn_deg: dec_syn,
        });
    }
    Ok(out)
}

/// Empirical check of the draws actually written for one object: the RMS
/// of the normalized offsets \( \Delta/\sigma \) per axis (→ 1 under any
/// law at unit variance) and the fraction beyond \( 3\sigma \) (→ 0.27%
/// Gaussian; larger under Student-t — that is the point).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NoiseSelfCheck {
    pub n: usize,
    pub rms_east_over_sigma: f64,
    pub rms_north_over_sigma: f64,
    pub frac_beyond_3sigma: f64,
}

pub fn noise_self_check(rows: &[SyntheticRow]) -> NoiseSelfCheck {
    let n = rows.len();
    if n == 0 {
        return NoiseSelfCheck {
            n: 0,
            rms_east_over_sigma: f64::NAN,
            rms_north_over_sigma: f64::NAN,
            frac_beyond_3sigma: f64::NAN,
        };
    }
    let (mut se, mut sn, mut tails) = (0.0, 0.0, 0usize);
    for r in rows {
        let (ze, zn) = (r.dx_arcsec / r.sigma_arcsec, r.dy_arcsec / r.sigma_arcsec);
        se += ze * ze;
        sn += zn * zn;
        tails += usize::from(ze.abs() > 3.0) + usize::from(zn.abs() > 3.0);
    }
    NoiseSelfCheck {
        n,
        rms_east_over_sigma: (se / n as f64).sqrt(),
        rms_north_over_sigma: (sn / n as f64).sqrt(),
        frac_beyond_3sigma: tails as f64 / (2 * n) as f64,
    }
}

/// The truth orbit the ephemeris was generated from — enough to
/// regenerate it, and to see what the full-arc fit solved for.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TruthOrbit {
    pub epoch_mjd_tdb: f64,
    pub representation: String,
    pub frame: String,
    pub origin: String,
    pub elements: [f64; 6],
    pub a1: f64,
    pub a2: f64,
    pub a3: f64,
    /// Marsden \( g(r) \) shape parameters carried by the orbit.
    pub ng_alpha: f64,
    pub ng_r0: f64,
    pub ng_m: f64,
    pub ng_n: f64,
    pub ng_k: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub non_grav_dt: Option<f64>,
    /// Perturbers excluded from the truth fit (a Self-Perturber's own
    /// NAIF id), empty otherwise.
    #[serde(default)]
    pub excluded_perturbers_naif: Vec<i32>,
    pub n_solve_for: u32,
    pub n_obs_used: u32,
    pub n_obs_rejected: u32,
    pub reduced_chi2: f64,
    pub iterations: u32,
}

/// Per-object provenance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ObjectProvenance {
    pub object: String,
    pub mpc_designation: String,
    pub population: String,
    /// `None` when the truth fit did not converge — the object's PSV is
    /// then written header-only and the manifest marks it ineligible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truth: Option<TruthOrbit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    pub n_rows_source: usize,
    pub n_rows_space_based_dropped: usize,
    /// Rows dropped because the engine could not place their observer
    /// (e.g. a space-based station whose mission SPK is not loaded and
    /// whose row carries no `sys`/`pos` columns). Tallied per station.
    pub n_rows_observer_unresolvable: usize,
    pub observer_unresolvable_stations: BTreeMap<String, usize>,
    pub n_rows_written: usize,
    /// Row counts per injected-σ provenance.
    pub sigma_sources: BTreeMap<String, usize>,
    /// Distinct injected σ values (arcsec) → row counts.
    pub sigma_histogram: BTreeMap<String, usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_check: Option<NoiseSelfCheck>,
    pub truth_fit_ms: f64,
    pub ephemeris_ms: f64,
}

/// The lane-level provenance record, written beside the synthetic
/// fixtures as `synthetic.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyntheticProvenance {
    pub schema_version: u32,
    /// The synthetic snapshot id (what the manifest and the walk carry).
    pub snapshot_id: String,
    /// The real fixture snapshot the times/stations came from.
    pub source_snapshot_id: String,
    pub noise: NoiseModel,
    pub seed: u64,
    pub fiducial_sigma_arcsec: f64,
    pub survey_sigma_table: String,
    pub ast_cat: String,
    pub engine_version: Option<String>,
    pub force_model_tier: String,
    pub generated_by: String,
    pub objects: Vec<ObjectProvenance>,
}

pub const SYNTHETIC_SCHEMA_VERSION: u32 = 1;

/// Mahalanobis products of one fitted state against the truth.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mahalanobis6 {
    /// \( \Delta^\top \Sigma^{-1} \Delta \) over the full 6-state.
    pub d2_6: f64,
    /// The same over the 3×3 position block alone.
    pub d2_3: f64,
    /// Per-axis \( \Delta_i / \sqrt{\Sigma_{ii}} \).
    pub z: [f64; 6],
}

/// Solve \( \Sigma x = \Delta \) by Cholesky and return the quadratic
/// forms; `None` when \( \Sigma \) (or its position block) is not
/// positive definite — reported, never repaired.
pub fn mahalanobis_6x6(delta: &[f64; 6], sigma: &[[f64; 6]; 6]) -> Option<Mahalanobis6> {
    fn quad(delta: &[f64], sigma: &[[f64; 6]; 6], n: usize) -> Option<f64> {
        // L L^T = Σ (leading n×n); y = L^{-1} Δ; d² = yᵀy.
        let mut l = [[0.0f64; 6]; 6];
        for i in 0..n {
            for j in 0..=i {
                let mut sum = sigma[i][j];
                for (lik, ljk) in l[i].iter().zip(l[j].iter()).take(j) {
                    sum -= lik * ljk;
                }
                if i == j {
                    if sum.is_nan() || sum <= 0.0 || !sum.is_finite() {
                        return None;
                    }
                    l[i][j] = sum.sqrt();
                } else {
                    l[i][j] = sum / l[j][j];
                }
            }
        }
        let mut y = [0.0f64; 6];
        for i in 0..n {
            let mut sum = delta[i];
            for k in 0..i {
                sum -= l[i][k] * y[k];
            }
            y[i] = sum / l[i][i];
        }
        Some(y.iter().take(n).map(|v| v * v).sum())
    }
    let d2_6 = quad(delta, sigma, 6)?;
    let d2_3 = quad(delta, sigma, 3)?;
    let mut z = [0.0; 6];
    for i in 0..6 {
        if sigma[i][i].is_nan() || sigma[i][i] <= 0.0 {
            return None;
        }
        z[i] = delta[i] / sigma[i][i].sqrt();
    }
    Some(Mahalanobis6 { d2_6, d2_3, z })
}

/// One window's state-space check: the fitted state at its fit epoch
/// against the truth propagated to that epoch, both Sun-centered ICRF
/// Cartesian (AU, AU/day), under the fitted 6×6.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TruthCheckRecord {
    pub object: String,
    pub tool: String,
    pub config_arm: String,
    pub window_index: u32,
    pub fit_epoch_mjd_tdb: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n_obs_used: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n_solve_for: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub covariance_trust: Option<String>,
    /// Fitted − truth, \( (x, y, z, \dot x, \dot y, \dot z) \).
    pub delta_au_au_day: [f64; 6],
    /// \( \chi^2_6 \) under a calibrated covariance (mean 6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub d2_state6: Option<f64>,
    /// Position block, \( \chi^2_3 \) (mean 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub d2_pos3: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub z_per_axis: Option<[f64; 6]>,
    pub dpos_km: f64,
    /// \( \sqrt{\mathrm{tr}\,\Sigma_{pos}} \), km.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sigma_pos_km: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

/// Snapshot id of a synthetic lane: the source snapshot, the law, the seed.
pub fn synthetic_snapshot_id(source_snapshot_id: &str, model: NoiseModel, seed: u64) -> String {
    format!("{source_snapshot_id}-synthetic-{}-seed{seed}", model.tag())
}

pub fn sigma_key(sigma_arcsec: f64) -> String {
    format!("{sigma_arcsec:.4}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_model_parses_and_refuses_infinite_variance() {
        assert_eq!(
            NoiseModel::parse("gaussian", None).unwrap(),
            NoiseModel::Gaussian
        );
        assert_eq!(
            NoiseModel::parse("student-t", Some(4)).unwrap(),
            NoiseModel::StudentT { nu: 4 }
        );
        assert!(
            NoiseModel::parse("student-t", Some(2))
                .unwrap_err()
                .contains("infinite")
        );
        assert!(NoiseModel::parse("student-t", None).is_err());
        assert!(NoiseModel::parse("cauchy", None).is_err());
        assert_eq!(NoiseModel::StudentT { nu: 4 }.tag(), "t4");
    }

    #[test]
    fn draws_have_unit_variance_under_both_laws() {
        // 200k draws: the sample variance of a unit-variance law lands
        // within ~1% (Gaussian) / a few % (t4 — its kurtosis is infinite,
        // so the variance estimate is noisier; 5% bound).
        for (model, tol) in [
            (NoiseModel::Gaussian, 0.01),
            (NoiseModel::StudentT { nu: 4 }, 0.06),
        ] {
            let mut rng = RowRng::new(12345);
            let n = 200_000;
            let (mut s2, mut tails) = (0.0, 0usize);
            for _ in 0..n {
                let z = rng.unit_draw(model);
                s2 += z * z;
                tails += usize::from(z.abs() > 3.0);
            }
            let var = s2 / n as f64;
            assert!((var - 1.0).abs() < tol, "{model:?}: var = {var}");
            let tail = tails as f64 / n as f64;
            match model {
                // Gaussian: P(|z|>3) = 0.0027.
                NoiseModel::Gaussian => assert!((tail - 0.0027).abs() < 0.0008, "tail {tail}"),
                // t4 scaled to unit variance: heavier — P(|t4|>3/√(1/2)=4.24) ≈ 0.0133.
                NoiseModel::StudentT { .. } => {
                    assert!(tail > 0.008 && tail < 0.02, "tail {tail}")
                }
            }
        }
    }

    #[test]
    fn row_seed_depends_only_on_identity() {
        let a = row_seed(7, "Eros", "F51", "2024-01-01T00:00:00.000Z", 0);
        assert_eq!(a, row_seed(7, "Eros", "F51", "2024-01-01T00:00:00.000Z", 0));
        assert_ne!(a, row_seed(8, "Eros", "F51", "2024-01-01T00:00:00.000Z", 0));
        assert_ne!(a, row_seed(7, "Eros", "F52", "2024-01-01T00:00:00.000Z", 0));
        assert_ne!(a, row_seed(7, "Iris", "F51", "2024-01-01T00:00:00.000Z", 0));
        // A duplicate (station, obsTime) row is its own key.
        assert_ne!(a, row_seed(7, "Eros", "F51", "2024-01-01T00:00:00.000Z", 1));
        // Distinct rows get distinct draws.
        let mut r1 = RowRng::new(a);
        let mut r2 = RowRng::new(row_seed(7, "Eros", "F51", "2024-01-01T00:00:01.000Z", 0));
        assert_ne!(r1.normal(), r2.normal());
    }

    #[test]
    fn offset_is_the_exact_inverse_of_the_kernel_projection() {
        for (ra0, dec0) in [(10.0, 5.0), (359.99, -45.0), (180.0, 89.0), (0.001, -89.5)] {
            for (dx, dy) in [(0.0, 0.0), (0.3, -0.2), (-25.0, 40.0), (1000.0, -1000.0)] {
                let (ra, dec) = offset_position(ra0, dec0, dx, dy);
                assert!((0.0..360.0).contains(&ra), "ra {ra}");
                let (bx, by) = gnomonic_offset_arcsec(ra0, dec0, ra, dec).unwrap();
                assert!((bx - dx).abs() < 1e-7, "{ra0},{dec0}: east {bx} vs {dx}");
                assert!((by - dy).abs() < 1e-7, "{ra0},{dec0}: north {by} vs {dy}");
            }
        }
        // Zero offset is the identity.
        let (ra, dec) = offset_position(123.456, -7.89, 0.0, 0.0);
        assert!((ra - 123.456).abs() < 1e-12 && (dec + 7.89).abs() < 1e-12);
    }

    #[test]
    fn injected_sigma_is_survey_table_then_fiducial() {
        let s = injected_sigma("F51", 0.2);
        assert_eq!((s.arcsec, s.source), (0.2, sigma_sources::SURVEY_VFC17));
        let s = injected_sigma("703", 0.2);
        assert_eq!((s.arcsec, s.source), (1.0, sigma_sources::SURVEY_VFC17));
        let s = injected_sigma("Q62", 0.2);
        assert_eq!((s.arcsec, s.source), (0.2, sigma_sources::FIDUCIAL));
        let s = injected_sigma("Q62", 0.35);
        assert_eq!(s.arcsec, 0.35);
    }

    const PSV: &str = "# version=2017\n\
permID |provID     |trkSub|mode|stn |sys  |ctr|pos1        |pos2        |pos3        |prog|obsTime                 | ra        |dec        |  astCat|mag  |band\n\
433    |           |      |CCD |F51 |     |   |            |            |            |    |2024-01-01T00:00:00.000Z|10.0000000 |5.0000000  |  Gaia2 |15.1 |G\n\
433    |           |      |CCD |Q62 |     |   |            |            |            |    |2024-01-02T00:00:00.000Z|10.1000000 |5.1000000  |  UCAC4 |15.2 |R\n\
433    |           |      |CCD |C51 |ICRF_KM|399|1.0       |2.0         |3.0         |    |2024-01-03T00:00:00.000Z|10.2000000 |5.2000000  |  2MASS |15.3 |W\n";

    #[test]
    fn psv_table_round_trips_drops_space_based_and_adds_columns() {
        let mut t = PsvTable::parse(PSV).unwrap();
        assert_eq!(t.preamble, vec!["# version=2017".to_string()]);
        assert_eq!(t.rows.len(), 3);
        assert_eq!(t.drop_space_based(), 1);
        assert_eq!(t.rows.len(), 2);
        assert!(t.col("rmsRA").is_none());
        t.ensure_columns_after_dec(&["rmsRA", "rmsDec", "rmsCorr"]);
        let d = t.col("dec").unwrap();
        assert_eq!(
            &t.header[d + 1..d + 4],
            &[
                "rmsRA".to_string(),
                "rmsDec".to_string(),
                "rmsCorr".to_string()
            ]
        );
        assert!(t.rows.iter().all(|r| r.len() == t.header.len()));
        // Idempotent.
        t.ensure_columns_after_dec(&["rmsRA"]);
        assert_eq!(t.header.iter().filter(|h| *h == "rmsRA").count(), 1);
        // Render → parse is stable.
        let again = PsvTable::parse(&t.render()).unwrap();
        assert_eq!(again, t);
    }

    #[test]
    fn synthesize_table_writes_reported_sigma_and_recoverable_offsets() {
        let mut t = PsvTable::parse(PSV).unwrap();
        t.drop_space_based();
        let truth = vec![(10.0, 5.0), (10.1, 5.1)];
        let rows = synthesize_table("433", &mut t, &truth, NoiseModel::Gaussian, 0.2, 1).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].sigma_source, sigma_sources::SURVEY_VFC17);
        assert_eq!(rows[1].sigma_source, sigma_sources::FIDUCIAL);
        let c = |n: &str| t.col(n).unwrap();
        for (i, r) in rows.iter().enumerate() {
            let row = &t.rows[i];
            assert_eq!(row[c("rmsRA")], format!("{:.4}", r.sigma_arcsec));
            assert_eq!(row[c("rmsDec")], format!("{:.4}", r.sigma_arcsec));
            assert_eq!(row[c("rmsCorr")], "");
            assert_eq!(row[c("astCat")], SYNTHETIC_AST_CAT);
            let ra: f64 = row[c("ra")].parse().unwrap();
            let dec: f64 = row[c("dec")].parse().unwrap();
            // The written text recovers the injected offset to the 1e-9°
            // quantization (3.6 µas).
            let (bx, by) = gnomonic_offset_arcsec(r.ra_true_deg, r.dec_true_deg, ra, dec).unwrap();
            assert!((bx - r.dx_arcsec).abs() < 1e-4 && (by - r.dy_arcsec).abs() < 1e-4);
            assert_ne!((r.dx_arcsec, r.dy_arcsec), (0.0, 0.0));
        }
        // Identity columns survive.
        assert_eq!(t.rows[0][c("permID")], "433");
        assert_eq!(t.rows[1][c("stn")], "Q62");
        // The regenerated text parses with the harness's own manifest
        // parser conventions (header contains obsTime; cells trimmed).
        assert!(t.render().lines().nth(1).unwrap().contains("rmsRA"));
        // Length mismatch is loud.
        let mut t2 = PsvTable::parse(PSV).unwrap();
        t2.drop_space_based();
        assert!(
            synthesize_table("433", &mut t2, &truth[..1], NoiseModel::Gaussian, 0.2, 1)
                .unwrap_err()
                .contains("truth positions")
        );
    }

    #[test]
    fn synthesis_is_deterministic_in_the_seed_and_row_order_free() {
        let mut a = PsvTable::parse(PSV).unwrap();
        a.drop_space_based();
        let truth = vec![(10.0, 5.0), (10.1, 5.1)];
        let ra = synthesize_table(
            "433",
            &mut a,
            &truth,
            NoiseModel::StudentT { nu: 4 },
            0.2,
            9,
        )
        .unwrap();
        let mut b = PsvTable::parse(PSV).unwrap();
        b.drop_space_based();
        b.rows.reverse();
        let mut rev = truth.clone();
        rev.reverse();
        let rb =
            synthesize_table("433", &mut b, &rev, NoiseModel::StudentT { nu: 4 }, 0.2, 9).unwrap();
        assert_eq!(ra[0], rb[1]);
        assert_eq!(ra[1], rb[0]);
        let mut c = PsvTable::parse(PSV).unwrap();
        c.drop_space_based();
        let rc = synthesize_table(
            "433",
            &mut c,
            &truth,
            NoiseModel::StudentT { nu: 4 },
            0.2,
            10,
        )
        .unwrap();
        assert_ne!(ra[0].dx_arcsec, rc[0].dx_arcsec);
    }

    #[test]
    fn duplicate_station_time_rows_get_distinct_draws() {
        // Two rows with identical (stn, obsTime) — legitimate MPC duplicates.
        let text = "permID|trkSub|mode|stn|obsTime|ra|dec|astCat\n\
7|a|CCD|248|1992-03-02T03:09:07.171Z|10.0|5.0|UNK\n\
7|b|CCD|248|1992-03-02T03:09:07.171Z|10.0|5.0|UNK\n";
        let mut t = PsvTable::parse(text).unwrap();
        let rows = synthesize_table(
            "Iris",
            &mut t,
            &[(10.0, 5.0), (10.0, 5.0)],
            NoiseModel::Gaussian,
            0.2,
            1,
        )
        .unwrap();
        assert_ne!(
            (rows[0].dx_arcsec, rows[0].dy_arcsec),
            (rows[1].dx_arcsec, rows[1].dy_arcsec)
        );
        assert_ne!(
            t.rows[0][t.col("ra").unwrap()],
            t.rows[1][t.col("ra").unwrap()]
        );
        // Reversing the file order swaps which row is "first" — the pair
        // of draws is the same set either way.
        let mut u = PsvTable::parse(text).unwrap();
        u.rows.reverse();
        let r2 = synthesize_table(
            "Iris",
            &mut u,
            &[(10.0, 5.0), (10.0, 5.0)],
            NoiseModel::Gaussian,
            0.2,
            1,
        )
        .unwrap();
        assert_eq!(
            (rows[0].dx_arcsec, rows[1].dx_arcsec),
            (r2[0].dx_arcsec, r2[1].dx_arcsec)
        );
    }

    #[test]
    fn self_check_reports_unit_rms() {
        let rows: Vec<SyntheticRow> = (0..20_000)
            .map(|i| {
                let mut rng = RowRng::new(row_seed(3, "x", "F51", &i.to_string(), 0));
                let (dx, dy) = draw_offset_arcsec(&mut rng, NoiseModel::Gaussian, 0.5);
                SyntheticRow {
                    stn: "F51".into(),
                    obs_time: i.to_string(),
                    ra_true_deg: 0.0,
                    dec_true_deg: 0.0,
                    sigma_arcsec: 0.5,
                    sigma_source: sigma_sources::SURVEY_VFC17.into(),
                    dx_arcsec: dx,
                    dy_arcsec: dy,
                    ra_syn_deg: 0.0,
                    dec_syn_deg: 0.0,
                }
            })
            .collect();
        let c = noise_self_check(&rows);
        assert_eq!(c.n, 20_000);
        assert!((c.rms_east_over_sigma - 1.0).abs() < 0.03, "{c:?}");
        assert!((c.rms_north_over_sigma - 1.0).abs() < 0.03, "{c:?}");
        assert!((c.frac_beyond_3sigma - 0.0027).abs() < 0.002, "{c:?}");
        assert!(noise_self_check(&[]).rms_east_over_sigma.is_nan());
    }

    #[test]
    fn mahalanobis_matches_a_diagonal_case_and_refuses_indefinite() {
        let mut sig = [[0.0; 6]; 6];
        for (i, s) in [4.0, 9.0, 16.0, 1.0, 1.0, 1.0].into_iter().enumerate() {
            sig[i][i] = s;
        }
        let delta = [2.0, 3.0, 4.0, 0.0, 0.0, 1.0];
        let m = mahalanobis_6x6(&delta, &sig).unwrap();
        assert!((m.d2_6 - 4.0).abs() < 1e-12, "{m:?}");
        assert!((m.d2_3 - 3.0).abs() < 1e-12, "{m:?}");
        assert_eq!(m.z, [1.0, 1.0, 1.0, 0.0, 0.0, 1.0]);
        // Correlated 2×2 block: Σ = [[2,1],[1,2]], Δ = (1,1) → ΔᵀΣ⁻¹Δ = 2/3.
        let mut sig2 = sig;
        sig2[0][0] = 2.0;
        sig2[0][1] = 1.0;
        sig2[1][0] = 1.0;
        sig2[1][1] = 2.0;
        let m = mahalanobis_6x6(&[1.0, 1.0, 0.0, 0.0, 0.0, 0.0], &sig2).unwrap();
        assert!((m.d2_6 - 2.0 / 3.0).abs() < 1e-12, "{m:?}");
        assert!((m.d2_3 - 2.0 / 3.0).abs() < 1e-12, "{m:?}");
        // Indefinite → None, never a repaired number.
        let mut bad = sig;
        bad[2][2] = -1.0;
        assert!(mahalanobis_6x6(&delta, &bad).is_none());
        let mut nan = sig;
        nan[0][0] = f64::NAN;
        assert!(mahalanobis_6x6(&delta, &nan).is_none());
    }

    #[test]
    fn snapshot_id_names_source_law_and_seed() {
        assert_eq!(
            synthetic_snapshot_id("2026-07-29-abc", NoiseModel::StudentT { nu: 4 }, 42),
            "2026-07-29-abc-synthetic-t4-seed42"
        );
    }
}
