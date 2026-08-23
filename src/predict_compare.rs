//! Central scoring kernel for the covariance-realism family.
//!
//! Runners emit predictions; this kernel scores every tool identically.
//! Per prediction, against the pinned observed position:
//!
//! - gnomonic (tangent-plane) residual about the **predicted** direction —
//!   exact at any separation, and the tangent plane is defined by the
//!   prediction, so there is no observed-vs-predicted \( \delta \)
//!   ambiguity in the \( \cos\delta \) scaling;
//! - the held-out combined covariance
//!   \( \Sigma = \Sigma_{obs} + \Sigma_{pred} \) (prediction confidence
//!   regions; the sign counterpart of the in-sample \( -HPH^\top \));
//! - Mahalanobis \( d^2 \) at 2 dof with its \( \chi^2_2 \) survival
//!   probability, plus \( d^2 \) against each term alone;
//! - 1-dof projections: along the tool's dominant uncertainty direction
//!   (every tool participates, including σ+PA-only tools) and cross-track;
//! - the along/cross-track split through one **shared** position angle per
//!   target (the reference channel's), so every tool's AT/CT numbers live
//!   in the same basis;
//! - per-night joint statistics (within a night the orbit error is
//!   common-mode: \( n \) observations are not \( n \) independent draws).
//!
//! Every degraded path is a named flag on the scored row — never a silent
//! drop, never a clamp. Flagged rows are excluded from headline aggregates
//! and remain in the scored output.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{BufRead, Write as IoWrite};
use std::path::{Path, PathBuf};

use crate::predict_schema::{
    ObjectWalkSummary, ObjectWindows, ObsEntry, PredictAggregates, PredictedObservation,
    ScoredPrediction, SurfaceCell, WalkWindowRecord, WindowManifest, WindowSpec, uncertainty_forms,
};

/// Arc-length bin edges (days) for the predictive-power surface, log-spaced.
/// A cell is `[lo, hi)`.
pub const ARC_BIN_EDGES_DAYS: [f64; 9] =
    [1.0, 3.0, 10.0, 30.0, 100.0, 300.0, 1000.0, 3000.0, 30000.0];

/// Horizon (dt) bin edges (days), log-spaced. A cell is `[lo, hi)`.
pub const DT_BIN_EDGES_DAYS: [f64; 8] = [0.3, 1.0, 3.0, 10.0, 30.0, 100.0, 400.0, 1600.0];

/// The tool whose predicted position angle of motion defines the shared
/// AT/CT rotation basis, in preference order. Falls back to the row's own
/// PA with the `pa_fallback_own` flag when neither delivered one.
pub const REFERENCE_PA_TOOLS: [&str; 2] = ["rust", "core"];

/// Gnomonic separations beyond this are outside the tangent-plane validity
/// the statistics assume; the row is flagged and carries no `d²`.
pub const MAX_VALID_SEPARATION_DEG: f64 = 1.0;

/// d²-histogram domain for the aggregate distribution panel — the χ²₂ tail
/// beyond 10 carries 0.67% of a calibrated sample, so overflow is the
/// tail-excess statistic.
pub const D2_HIST_DOMAIN: f64 = 10.0;
/// Equal-width bins over the d²-histogram domain.
pub const D2_HIST_BINS: usize = 50;

/// Log₁₀ domain of the survival-curve companion bins. The linear family
/// above stops at d² = 10 and buries the entire tail in one overflow
/// integer; this corpus puts 27% of its held-out rows there, so the tail is
/// the finding and has to be resolved. 10⁻³ catches the over-conservative
/// pile-up, 10³ reaches past the worst comet windows.
pub const D2_LOG_LO: f64 = -3.0;
pub const D2_LOG_HI: f64 = 3.0;
/// Equal-width log₁₀ bins over the survival domain — 20 per decade, which
/// resolves the χ²₂ 99% point (d² = 9.21) to better than a bin width.
pub const D2_LOG_BINS: usize = 120;

/// Reduced-χ² histogram domain, log₁₀ units. Fits span six decades in the
/// tail; ±2 holds the body and the shoulders, and what falls outside is
/// counted as under/overflow rather than clamped into an edge bin.
pub const RCHI2_LOG10_LO: f64 = -2.0;
pub const RCHI2_LOG10_HI: f64 = 2.0;
/// Equal-width log₁₀ bins over the reduced-χ² domain.
pub const RCHI2_HIST_BINS: usize = 60;

/// Serialized-size ceiling for the per-window timeline family. Past this the
/// family is reduced to the series that actually carry a d² and the
/// aggregate says so out loud — the report embeds its aggregate inline, so
/// an unbounded family would be paid for on every page load.
pub const PER_WINDOW_BUDGET_BYTES: usize = 12 * 1024 * 1024;

/// Round to three significant digits. The timeline family is a plotting
/// aid on log axes spanning decades; three digits is well past what a
/// rendered pixel resolves, and the saving is the difference between a
/// family that fits the page budget and one that does not.
fn round3(v: f64) -> f64 {
    if !v.is_finite() || v == 0.0 {
        return v;
    }
    let mag = v.abs().log10().floor();
    let scale = 10f64.powf(2.0 - mag);
    (v * scale).round() / scale
}

const RAD2ARCSEC: f64 = 3600.0 * 180.0 / std::f64::consts::PI;
const DEG2ARCSEC: f64 = 3600.0;

/// Scoring options, mirrored from the `score-predictions` CLI subcommand.
#[derive(Debug, Clone)]
pub struct ScoreOptions {
    /// Which σ table scores this pass: `"pinned"` (the manifest columns) or
    /// `"ades_only"` (ADES σ where present; rows without one are skipped and
    /// counted). Any other name is an error.
    pub sigma_table: String,
    /// Score against debiased observed positions (the protocol default).
    /// `false` is the raw sensitivity arm — an explicit choice, never a
    /// fallback.
    pub apply_debias: bool,
}

/// Score prediction sidecars against the manifest: write the scored JSONL to
/// `out_scored` and the surface aggregates to `out_agg`.
pub fn run(
    manifest_path: &Path,
    prediction_sidecars: &[PathBuf],
    window_sidecars: &[PathBuf],
    out_scored: &Path,
    out_agg: &Path,
    opts: &ScoreOptions,
) -> Result<(), String> {
    let manifest: WindowManifest = {
        let txt = std::fs::read_to_string(manifest_path)
            .map_err(|e| format!("read manifest {}: {e}", manifest_path.display()))?;
        serde_json::from_str(&txt)
            .map_err(|e| format!("parse manifest {}: {e}", manifest_path.display()))?
    };
    let predictions: Vec<PredictedObservation> = read_jsonl_files(prediction_sidecars)?;
    let windows: Vec<WalkWindowRecord> = read_jsonl_files(window_sidecars)?;

    let (scored, sp_matrices) = score_all(&manifest, &predictions, &windows, opts)?;
    let agg = aggregate(&manifest, &scored, &windows, &sp_matrices);

    if let Some(dir) = out_scored.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    }
    let mut f = std::io::BufWriter::new(
        std::fs::File::create(out_scored)
            .map_err(|e| format!("create {}: {e}", out_scored.display()))?,
    );
    for s in &scored {
        serde_json::to_writer(&mut f, s).map_err(|e| format!("write scored: {e}"))?;
        f.write_all(b"\n")
            .map_err(|e| format!("write scored: {e}"))?;
    }
    f.flush()
        .map_err(|e| format!("flush {}: {e}", out_scored.display()))?;
    std::fs::write(
        out_agg,
        serde_json::to_string_pretty(&agg).map_err(|e| format!("serialize agg: {e}"))?,
    )
    .map_err(|e| format!("write {}: {e}", out_agg.display()))?;

    let flagged: usize = scored.iter().filter(|s| !s.flags.is_empty()).count();
    eprintln!(
        "score-predictions: {} rows scored ({} flagged), {} surface cells, {} objects, \
         {} walk timelines, {} reduced-χ² histograms — sigma_table={} debias={}",
        scored.len(),
        flagged,
        agg.cells.len(),
        agg.per_object.len(),
        agg.per_window.len(),
        agg.reduced_chi2.len(),
        opts.sigma_table,
        opts.apply_debias,
    );
    if let Some(note) = &agg.per_window_note {
        eprintln!("score-predictions: {note}");
    }
    Ok(())
}

fn read_jsonl_files<T: serde::de::DeserializeOwned>(paths: &[PathBuf]) -> Result<Vec<T>, String> {
    let mut out = Vec::new();
    for p in paths {
        if p.extension().is_some_and(|e| e == "gz") {
            return Err(format!(
                "{}: gzip sidecars not supported yet — decompress first",
                p.display()
            ));
        }
        let f = std::fs::File::open(p).map_err(|e| format!("open {}: {e}", p.display()))?;
        let mut it = std::io::BufReader::new(f).lines().enumerate().peekable();
        while let Some((i, line)) = it.next() {
            let line = line.map_err(|e| format!("read {}: {e}", p.display()))?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str(&line) {
                Ok(v) => out.push(v),
                // A running walk streams sidecars shard-by-shard, so the
                // FINAL line of a mid-run snapshot can be torn. That one
                // line is skipped loudly; a bad line anywhere else is
                // still a hard error.
                Err(e) if it.peek().is_none() => {
                    eprintln!(
                        "WARNING: {} line {} is unparseable and is the final line — \
                         treating as a torn mid-run write and skipping it ({e})",
                        p.display(),
                        i + 1
                    );
                }
                Err(e) => return Err(format!("parse {} line {}: {e}", p.display(), i + 1)),
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// 2×2 linear algebra — closed forms, PSD-gated. No clamps: a matrix that is
// not positive-definite is a named failure, never a repaired number.
// ---------------------------------------------------------------------------

type Mat2 = [[f64; 2]; 2];

fn chol2_ok(m: &Mat2) -> bool {
    m[0][0] > 0.0 && {
        let l21 = m[0][1] / m[0][0].sqrt();
        m[1][1] - l21 * l21 > 0.0
    }
}

fn add2(a: &Mat2, b: &Mat2) -> Mat2 {
    [
        [a[0][0] + b[0][0], a[0][1] + b[0][1]],
        [a[1][0] + b[1][0], a[1][1] + b[1][1]],
    ]
}

fn quad_inv2(m: &Mat2, r: [f64; 2]) -> Option<f64> {
    if !chol2_ok(m) {
        return None;
    }
    let det = m[0][0] * m[1][1] - m[0][1] * m[1][0];
    if det <= 0.0 || !det.is_finite() {
        return None;
    }
    let d2 =
        (r[0] * (m[1][1] * r[0] - m[0][1] * r[1]) + r[1] * (m[0][0] * r[1] - m[1][0] * r[0])) / det;
    d2.is_finite().then_some(d2)
}

fn quad_form2(m: &Mat2, e: [f64; 2]) -> f64 {
    e[0] * e[0] * m[0][0] + 2.0 * e[0] * e[1] * m[0][1] + e[1] * e[1] * m[1][1]
}

/// Unit eigenvector of the larger eigenvalue of a symmetric 2×2.
fn dominant_eigenvector2(m: &Mat2) -> [f64; 2] {
    let (a, b, c) = (m[0][0], m[0][1], m[1][1]);
    let tr = a + c;
    let disc = ((a - c) * (a - c) + 4.0 * b * b).sqrt();
    let l1 = 0.5 * (tr + disc);
    // (l1 − c, b) and (b, l1 − a) are both eigenvectors; pick the better-
    // conditioned one.
    let (x, y) = if (l1 - c).abs() > (l1 - a).abs() {
        (l1 - c, b)
    } else {
        (b, l1 - a)
    };
    let n = (x * x + y * y).sqrt();
    if n == 0.0 {
        // Isotropic matrix: every direction is dominant; East is as good a
        // convention as any and is stated here.
        [1.0, 0.0]
    } else {
        [x / n, y / n]
    }
}

// ---------------------------------------------------------------------------
// Sky geometry
// ---------------------------------------------------------------------------

fn unit_vector(ra_deg: f64, dec_deg: f64) -> [f64; 3] {
    let (a, d) = (ra_deg.to_radians(), dec_deg.to_radians());
    [d.cos() * a.cos(), d.cos() * a.sin(), d.sin()]
}

/// Azimuthal-equidistant (arc) projection: components along the tangent
/// basis scaled so the norm is the exact great-circle separation. Defined
/// for any separation < 180° — the fallback for observations behind the
/// gnomonic tangent plane (> 90° misses), where only the separation is a
/// meaningful statistic.
fn arc_projection_arcsec(
    pred_ra_deg: f64,
    pred_dec_deg: f64,
    obs_ra_deg: f64,
    obs_dec_deg: f64,
) -> (f64, f64) {
    let u = unit_vector(obs_ra_deg, obs_dec_deg);
    let p = unit_vector(pred_ra_deg, pred_dec_deg);
    let dot = (u[0] * p[0] + u[1] * p[1] + u[2] * p[2]).clamp(-1.0, 1.0);
    let (a0, d0) = (pred_ra_deg.to_radians(), pred_dec_deg.to_radians());
    let east = [-a0.sin(), a0.cos(), 0.0];
    let north = [-d0.sin() * a0.cos(), -d0.sin() * a0.sin(), d0.cos()];
    // Component of u perpendicular to p, scaled to the great-circle angle.
    let perp = [u[0] - dot * p[0], u[1] - dot * p[1], u[2] - dot * p[2]];
    let norm = (perp[0] * perp[0] + perp[1] * perp[1] + perp[2] * perp[2]).sqrt();
    let theta = dot.acos();
    if norm == 0.0 {
        return (0.0, 0.0);
    }
    let scale = theta / norm * RAD2ARCSEC;
    (
        (perp[0] * east[0] + perp[1] * east[1] + perp[2] * east[2]) * scale,
        (perp[0] * north[0] + perp[1] * north[1] + perp[2] * north[2]) * scale,
    )
}

/// Gnomonic projection of the observed direction onto the tangent plane at
/// the predicted direction. Returns (east, north) in arcsec, or `None` when
/// the observed direction is on or behind the tangent point's horizon.
fn gnomonic_arcsec(
    pred_ra_deg: f64,
    pred_dec_deg: f64,
    obs_ra_deg: f64,
    obs_dec_deg: f64,
) -> Option<(f64, f64)> {
    let u = unit_vector(obs_ra_deg, obs_dec_deg);
    let p = unit_vector(pred_ra_deg, pred_dec_deg);
    let dot = u[0] * p[0] + u[1] * p[1] + u[2] * p[2];
    if dot <= 0.0 {
        return None;
    }
    let (a0, d0) = (pred_ra_deg.to_radians(), pred_dec_deg.to_radians());
    let east = [-a0.sin(), a0.cos(), 0.0];
    let north = [-d0.sin() * a0.cos(), -d0.sin() * a0.sin(), d0.cos()];
    let t = [u[0] / dot, u[1] / dot, u[2] / dot];
    let xi = t[0] * east[0] + t[1] * east[1] + t[2] * east[2];
    let eta = t[0] * north[0] + t[1] * north[1] + t[2] * north[2];
    Some((xi * RAD2ARCSEC, eta * RAD2ARCSEC))
}

// ---------------------------------------------------------------------------
// Scoring
// ---------------------------------------------------------------------------

struct ObjIndex<'a> {
    obj: &'a ObjectWindows,
    windows: HashMap<u32, &'a WindowSpec>,
    /// (window_index, obs_idx) → (rung, horizon_days) for held-out targets.
    targets: HashMap<(u32, u32), (&'a str, f64)>,
}

fn index_manifest(manifest: &WindowManifest) -> HashMap<&str, ObjIndex<'_>> {
    manifest
        .objects
        .iter()
        .map(|o| {
            let windows: HashMap<u32, &WindowSpec> =
                o.windows.iter().map(|w| (w.index, w)).collect();
            let mut targets = HashMap::new();
            for w in &o.windows {
                for t in &w.targets {
                    targets.insert((w.index, t.obs_idx), (t.rung.as_str(), t.horizon_days));
                }
            }
            (
                o.object.as_str(),
                ObjIndex {
                    obj: o,
                    windows,
                    targets,
                },
            )
        })
        .collect()
}

/// The per-observation measurement covariance under the selected σ table
/// (great-circle basis, arcsec²), or `None` when the table does not cover
/// the row (`ades_only` on a row without ADES σ → the row is skipped).
fn sigma_obs(entry: &ObsEntry, table: &str) -> Result<Option<Mat2>, String> {
    let (sra, sdec, corr) = match table {
        "pinned" => (
            entry.sigma_ra_arcsec,
            entry.sigma_dec_arcsec,
            entry.sigma_corr,
        ),
        "ades_only" => match (entry.ades_rms_ra, entry.ades_rms_dec) {
            (Some(ra), Some(dec)) if ra > 0.0 && dec > 0.0 => (ra, dec, entry.sigma_corr),
            _ => return Ok(None),
        },
        other => {
            return Err(format!(
                "unknown sigma table {other:?} (pinned | ades_only)"
            ));
        }
    };
    let c = corr * sra * sdec;
    Ok(Some([[sra * sra, c], [c, sdec * sdec]]))
}

/// Normalize a runner-declared predicted covariance to great-circle arcsec².
fn normalize_pred_cov(
    cov: &Mat2,
    units: &str,
    basis: &str,
    pred_dec_deg: f64,
) -> Result<Mat2, String> {
    let scale = match units {
        "arcsec2" => 1.0,
        "deg2" => DEG2ARCSEC * DEG2ARCSEC,
        "rad2" => RAD2ARCSEC * RAD2ARCSEC,
        other => return Err(format!("unknown cov_units {other:?}")),
    };
    let mut m = [
        [cov[0][0] * scale, cov[0][1] * scale],
        [cov[1][0] * scale, cov[1][1] * scale],
    ];
    match basis {
        "great_circle" => {}
        "native_ra" => {
            // Σ' = SΣSᵀ with S = diag(cos δ, 1): cos²δ on the RA variance,
            // cos δ on the cross terms.
            let c = pred_dec_deg.to_radians().cos();
            m[0][0] *= c * c;
            m[0][1] *= c;
            m[1][0] *= c;
        }
        other => return Err(format!("unknown cov_basis {other:?}")),
    }
    Ok(m)
}

/// Build the shared AT/CT position-angle map from the reference tool's
/// predictions: (object, window, obs_idx) → PA (deg, East of North).
fn reference_pa_map(predictions: &[PredictedObservation]) -> HashMap<(String, u32, u32), f64> {
    let mut map = HashMap::new();
    for reference in REFERENCE_PA_TOOLS {
        for p in predictions {
            if p.tool == *reference
                && let Some(pa) = p.pa_motion_deg
            {
                map.entry((p.object.clone(), p.window_index, p.obs_idx))
                    .or_insert(pa);
            }
        }
        if !map.is_empty() {
            break;
        }
    }
    map
}

/// Along/cross-track unit vectors in (east, north) for a position angle
/// East of North. \( \hat e_{AT} = (\sin\theta, \cos\theta) \),
/// \( \hat e_{CT} = (-\cos\theta, \sin\theta) \) — the same sense as
/// scott's residual decomposition (`e_ct = (−μ_δ, +μ_α)/|μ|`).
fn at_ct_basis(pa_deg: f64) -> ([f64; 2], [f64; 2]) {
    let th = pa_deg.to_radians();
    ([th.sin(), th.cos()], [-th.cos(), th.sin()])
}

/// Key of one scored row's producing prediction:
/// (tool, config_arm, object, window_index, obs_idx).
type RowKey = (String, String, String, u32, u32);

fn score_all(
    manifest: &WindowManifest,
    predictions: &[PredictedObservation],
    windows: &[WalkWindowRecord],
    opts: &ScoreOptions,
) -> Result<(Vec<ScoredPrediction>, HashMap<RowKey, Mat2>), String> {
    let idx = index_manifest(manifest);
    let pa_map = reference_pa_map(predictions);
    let trust: HashMap<(&str, &str, &str, u32), &Option<String>> = windows
        .iter()
        .map(|w| {
            (
                (
                    w.tool.as_str(),
                    w.config_arm.as_str(),
                    w.object.as_str(),
                    w.window_index,
                ),
                &w.covariance_trust,
            )
        })
        .collect();

    let mut scored = Vec::with_capacity(predictions.len());
    let mut sp_matrices: HashMap<RowKey, Mat2> = HashMap::new();
    let mut errors: Vec<String> = Vec::new();
    let mut skipped_by_table = 0usize;

    for p in predictions {
        let Some(oi) = idx.get(p.object.as_str()) else {
            errors.push(format!("{}: object not in manifest", p.object));
            continue;
        };
        let Some(w) = oi.windows.get(&p.window_index) else {
            errors.push(format!(
                "{} w{}: window not in manifest",
                p.object, p.window_index
            ));
            continue;
        };
        let Some(entry) = oi.obj.observations.get(p.obs_idx as usize) else {
            errors.push(format!(
                "{} w{} obs{}: obs_idx out of range",
                p.object, p.window_index, p.obs_idx
            ));
            continue;
        };
        if entry.key_hash != p.key_hash {
            errors.push(format!(
                "{} w{} obs{}: key_hash mismatch (manifest {:x}, prediction {:x})",
                p.object, p.window_index, p.obs_idx, entry.key_hash, p.key_hash
            ));
            continue;
        }
        let Some(so) = sigma_obs(entry, &opts.sigma_table)? else {
            skipped_by_table += 1;
            continue;
        };

        let mut flags: Vec<String> = Vec::new();

        // Observed position, debiased when the protocol says so. The
        // correction is stored in great-circle arcsec; RA moves by
        // Δα = Δ(α cos δ)/cos δ.
        let (mut obs_ra, mut obs_dec) = (entry.ra_deg, entry.dec_deg);
        let mut debias_applied = false;
        if opts.apply_debias {
            match (entry.debias_dra_arcsec, entry.debias_ddec_arcsec) {
                (Some(dra), Some(ddec)) => {
                    let cosd = entry.dec_deg.to_radians().cos();
                    if cosd.abs() > 1e-12 {
                        obs_ra -= dra / DEG2ARCSEC / cosd;
                        obs_dec -= ddec / DEG2ARCSEC;
                        debias_applied = true;
                    } else {
                        flags.push("debias_pole_degenerate".into());
                    }
                }
                _ => flags.push("unk_catalog_no_debias".into()),
            }
        }

        // Residual: gnomonic about the predicted direction. A miss beyond
        // 90° puts the observation behind the tangent plane — a legitimate
        // outcome of a badly wrong short-arc orbit at long horizon (the
        // fixtures produce these), so it is a *flagged row*, never an
        // error: fall back to the azimuthal-equidistant projection, which
        // is defined to 180° and keeps the separation honest while every
        // d² stays withheld under the large-separation flag.
        let (east, north) = match gnomonic_arcsec(p.ra_deg, p.dec_deg, obs_ra, obs_dec) {
            Some(en) => en,
            None => {
                flags.push("behind_tangent_plane".into());
                arc_projection_arcsec(p.ra_deg, p.dec_deg, obs_ra, obs_dec)
            }
        };
        let sep = east.hypot(north);
        let sep_valid = sep <= MAX_VALID_SEPARATION_DEG * DEG2ARCSEC;
        if !sep_valid {
            flags.push("d2_invalid_large_separation".into());
        }
        let r = [east, north];

        // Predicted covariance in great-circle arcsec², PSD-gated.
        let sp: Option<Mat2> = match p.uncertainty_form.as_str() {
            uncertainty_forms::RADEC_2X2 => {
                match (&p.cov_radec, p.cov_units.as_deref(), p.cov_basis.as_deref()) {
                    (Some(cov), Some(units), Some(basis)) => {
                        let m = normalize_pred_cov(cov, units, basis, p.dec_deg).map_err(|e| {
                            format!("{} w{} obs{}: {e}", p.object, p.window_index, p.obs_idx)
                        })?;
                        if chol2_ok(&m) {
                            sp_matrices.insert(
                                (
                                    p.tool.clone(),
                                    p.config_arm.clone(),
                                    p.object.clone(),
                                    p.window_index,
                                    p.obs_idx,
                                ),
                                m,
                            );
                            Some(m)
                        } else {
                            flags.push("non_psd_covariance".into());
                            None
                        }
                    }
                    _ => {
                        errors.push(format!(
                            "{} w{} obs{}: uncertainty_form=radec_2x2 without \
                             cov_radec/cov_units/cov_basis",
                            p.object, p.window_index, p.obs_idx
                        ));
                        continue;
                    }
                }
            }
            uncertainty_forms::SIGMA1D_PA | uncertainty_forms::NONE => None,
            other => {
                errors.push(format!(
                    "{} w{} obs{}: unknown uncertainty_form {other:?}",
                    p.object, p.window_index, p.obs_idx
                ));
                continue;
            }
        };

        // Statistics — only inside the tangent-plane validity region.
        let obs_noise_dominated = sp
            .as_ref()
            .is_some_and(|sp| so[0][0] + so[1][1] > sp[0][0] + sp[1][1]);
        let (mut d2_combined, mut d2_pred_only, mut p_chi2, mut z1, mut z_ct) =
            (None, None, None, None, None);
        let d2_obs_only = quad_inv2(&so, r).unwrap_or(f64::INFINITY);
        if sep_valid {
            if let Some(sp) = &sp {
                let combined = add2(&so, sp);
                d2_combined = quad_inv2(&combined, r);
                if d2_combined.is_none() && !flags.iter().any(|f| f == "non_psd_covariance") {
                    flags.push("non_psd_covariance".into());
                }
                d2_pred_only = quad_inv2(sp, r);
                p_chi2 = d2_combined.map(|d2| hyperjet::statistics::chi2_sf(d2, 2));
                let e = dominant_eigenvector2(sp);
                let var = quad_form2(&combined, e);
                if var > 0.0 {
                    z1 = Some((r[0] * e[0] + r[1] * e[1]) / var.sqrt());
                }
            } else if p.uncertainty_form == uncertainty_forms::SIGMA1D_PA {
                match (p.sigma1_arcsec, p.sigma1_pa_deg) {
                    (Some(s1), Some(pa)) if s1 > 0.0 => {
                        let th = pa.to_radians();
                        let e = [th.sin(), th.cos()];
                        let var = s1 * s1 + quad_form2(&so, e);
                        z1 = Some((r[0] * e[0] + r[1] * e[1]) / var.sqrt());
                    }
                    _ => {
                        errors.push(format!(
                            "{} w{} obs{}: uncertainty_form=sigma1d_pa without positive \
                             sigma1_arcsec + sigma1_pa_deg",
                            p.object, p.window_index, p.obs_idx
                        ));
                        continue;
                    }
                }
            }
        }

        // Shared AT/CT rotation.
        let shared_pa = pa_map
            .get(&(p.object.clone(), p.window_index, p.obs_idx))
            .copied();
        let pa = match (shared_pa, p.pa_motion_deg) {
            (Some(pa), _) => Some(pa),
            (None, Some(own)) => {
                flags.push("pa_fallback_own".into());
                Some(own)
            }
            (None, None) => None,
        };
        let (mut resid_at, mut resid_ct) = (None, None);
        if let Some(pa) = pa {
            let (e_at, e_ct) = at_ct_basis(pa);
            resid_at = Some(r[0] * e_at[0] + r[1] * e_at[1]);
            resid_ct = Some(r[0] * e_ct[0] + r[1] * e_ct[1]);
            if sep_valid && let Some(sp) = &sp {
                let var_ct = quad_form2(&add2(&so, sp), e_ct);
                if var_ct > 0.0 {
                    z_ct = Some(resid_ct.unwrap() / var_ct.sqrt());
                }
            }
        }

        // Target provenance + horizon flags. In-sample controls are scored
        // identically but are not targets.
        let (rung, horizon) = if p.in_sample {
            ("in_sample", 0.0)
        } else {
            match oi.targets.get(&(p.window_index, p.obs_idx)) {
                Some((rung, h)) => (*rung, *h),
                None => {
                    errors.push(format!(
                        "{} w{} obs{}: not a manifest target for this window",
                        p.object, p.window_index, p.obs_idx
                    ));
                    continue;
                }
            }
        };
        if !p.in_sample && horizon < manifest.min_horizon_days {
            flags.push("same_night_continuation".into());
        }
        if let Some(Some(t)) = trust.get(&(
            p.tool.as_str(),
            p.config_arm.as_str(),
            p.object.as_str(),
            p.window_index,
        )) && t != "trusted"
        {
            flags.push("covariance_trust_flagged".into());
        }

        scored.push(ScoredPrediction {
            object: p.object.clone(),
            class: oi.obj.class.clone(),
            tool: p.tool.clone(),
            config_arm: p.config_arm.clone(),
            window_index: p.window_index,
            obs_idx: p.obs_idx,
            in_sample: p.in_sample,
            arc_days: w.arc_days,
            horizon_days: horizon,
            rung: rung.to_string(),
            night: entry.night,
            sigma_table: opts.sigma_table.clone(),
            debias_applied,
            resid_east_arcsec: east,
            resid_north_arcsec: north,
            sep_arcsec: sep,
            resid_at_arcsec: resid_at,
            resid_ct_arcsec: resid_ct,
            d2_combined,
            d2_obs_only,
            d2_pred_only,
            p_chi2,
            z1_dominant: z1,
            z_ct,
            obs_noise_dominated,
            flags,
        });
    }

    if !errors.is_empty() {
        let shown: Vec<&String> = errors.iter().take(8).collect();
        return Err(format!(
            "{} prediction records failed validation; first {}:\n  {}",
            errors.len(),
            shown.len(),
            shown
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n  ")
        ));
    }
    if skipped_by_table > 0 {
        eprintln!(
            "score-predictions: {skipped_by_table} rows skipped by sigma_table (no usable σ)"
        );
    }
    Ok((scored, sp_matrices))
}

// ---------------------------------------------------------------------------
// Aggregation
// ---------------------------------------------------------------------------

fn bin_index(edges: &[f64], v: f64) -> Option<usize> {
    if !v.is_finite() {
        return None;
    }
    (0..edges.len() - 1).find(|&i| v >= edges[i] && v < edges[i + 1])
}

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Some(values[values.len() / 2])
}

/// One d² per (tool, arm, object, window, night): the night-mean residual
/// against \( \bar\Sigma_{pred} + \bar\Sigma_{obs}/n \). Within a night the
/// orbit error is common-mode, so \( \Sigma_{pred} \) does not average down;
/// only the measurement term does. \( \Sigma_{pred} \) varies negligibly
/// across one night's targets, so the per-row matrices are averaged.
struct NightStat {
    class: String,
    window_index: u32,
    arc_days: f64,
    horizon_days: f64,
    d2: f64,
}

fn night_stats(
    manifest: &WindowManifest,
    scored: &[ScoredPrediction],
    sp_matrices: &HashMap<RowKey, Mat2>,
) -> HashMap<(String, String, String), Vec<NightStat>> {
    let idx = index_manifest(manifest);
    // (tool, arm, object) → (window, night) → rows
    type NightGroups<'a> =
        HashMap<(String, String, String), HashMap<(u32, i64), Vec<&'a ScoredPrediction>>>;
    let mut groups: NightGroups = HashMap::new();
    for s in scored {
        if s.in_sample || !s.flags.is_empty() || s.d2_combined.is_none() || s.obs_noise_dominated {
            continue;
        }
        groups
            .entry((s.tool.clone(), s.config_arm.clone(), s.object.clone()))
            .or_default()
            .entry((s.window_index, s.night))
            .or_default()
            .push(s);
    }
    let mut out: HashMap<(String, String, String), Vec<NightStat>> = HashMap::new();
    for ((tool, arm, object), nights) in groups {
        let oi = &idx[object.as_str()];
        let mut stats = Vec::new();
        for ((window_index, _night), rows) in nights {
            let n = rows.len() as f64;
            let mut r = [0.0, 0.0];
            let mut so_mean = [[0.0; 2]; 2];
            let mut sp_mean = [[0.0; 2]; 2];
            let mut horizons: Vec<f64> = Vec::new();
            let mut complete = true;
            for s in &rows {
                r[0] += s.resid_east_arcsec / n;
                r[1] += s.resid_north_arcsec / n;
                horizons.push(s.horizon_days);
                let entry = &oi.obj.observations[s.obs_idx as usize];
                match sigma_obs(entry, &s.sigma_table) {
                    Ok(Some(so)) => {
                        for i in 0..2 {
                            for j in 0..2 {
                                so_mean[i][j] += so[i][j] / n;
                            }
                        }
                    }
                    _ => complete = false,
                }
                match sp_matrices.get(&(
                    s.tool.clone(),
                    s.config_arm.clone(),
                    s.object.clone(),
                    s.window_index,
                    s.obs_idx,
                )) {
                    Some(sp) => {
                        for i in 0..2 {
                            for j in 0..2 {
                                sp_mean[i][j] += sp[i][j] / n;
                            }
                        }
                    }
                    None => complete = false,
                }
            }
            // Every row in the group carried a valid d2_combined, so both
            // matrices must exist; a hole here is a kernel bug, not data.
            if !complete {
                unreachable!(
                    "night statistic for {object} w{window_index}: matrix missing for a row \
                     that scored a d²"
                );
            }
            let sigma_night = add2(
                &sp_mean,
                &[
                    [so_mean[0][0] / n, so_mean[0][1] / n],
                    [so_mean[1][0] / n, so_mean[1][1] / n],
                ],
            );
            let Some(d2) = quad_inv2(&sigma_night, r) else {
                continue;
            };
            horizons.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let w = &oi.windows[&window_index];
            stats.push(NightStat {
                class: oi.obj.class.clone(),
                window_index,
                arc_days: w.arc_days,
                horizon_days: horizons[horizons.len() / 2],
                d2,
            });
        }
        out.insert((tool, arm, object), stats);
    }
    out
}

fn aggregate(
    manifest: &WindowManifest,
    scored: &[ScoredPrediction],
    windows: &[WalkWindowRecord],
    sp_matrices: &HashMap<RowKey, Mat2>,
) -> PredictAggregates {
    let nights = night_stats(manifest, scored, sp_matrices);
    let idx = index_manifest(manifest);

    // ---- Surface cells ----------------------------------------------------
    type CellKey = (String, String, String, String, usize, usize);
    struct CellAcc {
        per_object_sep: HashMap<String, Vec<f64>>,
        per_object_d2: HashMap<String, Vec<f64>>,
        windows: BTreeSet<(String, u32)>,
        n_predictions: u32,
        expected: u32,
        delivered: u32,
    }
    let mut cells: BTreeMap<CellKey, CellAcc> = BTreeMap::new();
    fn cell(cells: &mut BTreeMap<CellKey, CellAcc>, key: CellKey) -> &mut CellAcc {
        cells.entry(key).or_insert_with(|| CellAcc {
            per_object_sep: HashMap::new(),
            per_object_d2: HashMap::new(),
            windows: BTreeSet::new(),
            n_predictions: 0,
            expected: 0,
            delivered: 0,
        })
    }

    // Delivered / expected: expected = manifest targets of windows the tool
    // attempted (a WalkWindowRecord exists, converged or not); a window with
    // no record was not part of the tool's profile and is not expected.
    let attempted: BTreeSet<(String, String, String, u32)> = windows
        .iter()
        .map(|w| {
            (
                w.tool.clone(),
                w.config_arm.clone(),
                w.object.clone(),
                w.window_index,
            )
        })
        .collect();
    let sigma_table = scored
        .first()
        .map(|s| s.sigma_table.clone())
        .unwrap_or_else(|| "pinned".to_string());
    let tools: BTreeSet<(String, String)> = scored
        .iter()
        .map(|s| (s.tool.clone(), s.config_arm.clone()))
        .collect();
    for (tool, arm) in &tools {
        for o in &manifest.objects {
            for w in &o.windows {
                if !attempted.contains(&(tool.clone(), arm.clone(), o.object.clone(), w.index)) {
                    continue;
                }
                for t in &w.targets {
                    let (Some(ai), Some(di)) = (
                        bin_index(&ARC_BIN_EDGES_DAYS, w.arc_days),
                        bin_index(&DT_BIN_EDGES_DAYS, t.horizon_days),
                    ) else {
                        continue;
                    };
                    let key = (
                        o.class.clone(),
                        tool.clone(),
                        arm.clone(),
                        sigma_table.clone(),
                        ai,
                        di,
                    );
                    cell(&mut cells, key).expected += 1;
                }
            }
        }
    }

    for s in scored {
        if s.in_sample {
            continue;
        }
        let (Some(ai), Some(di)) = (
            bin_index(&ARC_BIN_EDGES_DAYS, s.arc_days),
            bin_index(&DT_BIN_EDGES_DAYS, s.horizon_days),
        ) else {
            continue;
        };
        let key = (
            s.class.clone(),
            s.tool.clone(),
            s.config_arm.clone(),
            s.sigma_table.clone(),
            ai,
            di,
        );
        let acc = cell(&mut cells, key);
        acc.delivered += 1;
        if s.flags.is_empty() {
            acc.n_predictions += 1;
            acc.windows.insert((s.object.clone(), s.window_index));
            acc.per_object_sep
                .entry(s.object.clone())
                .or_default()
                .push(s.sep_arcsec);
        }
    }
    for ((tool, arm, object), stats) in &nights {
        for st in stats {
            let class = st.class.clone();
            let (Some(ai), Some(di)) = (
                bin_index(&ARC_BIN_EDGES_DAYS, st.arc_days),
                bin_index(&DT_BIN_EDGES_DAYS, st.horizon_days),
            ) else {
                continue;
            };
            let key = (
                class.clone(),
                tool.clone(),
                arm.clone(),
                sigma_table.clone(),
                ai,
                di,
            );
            cell(&mut cells, key)
                .per_object_d2
                .entry(object.clone())
                .or_default()
                .push(st.d2);
        }
    }

    const LN2X2: f64 = 2.0 * std::f64::consts::LN_2;
    let cells: Vec<SurfaceCell> = cells
        .into_iter()
        .map(|((class, tool, config_arm, sigma_table, ai, di), acc)| {
            let mut sep_meds: Vec<f64> = acc
                .per_object_sep
                .values()
                .filter_map(|v| median(&mut v.clone()))
                .collect();
            let mut d2_meds: Vec<f64> = acc
                .per_object_d2
                .values()
                .filter_map(|v| median(&mut v.clone()))
                .collect();
            let cov = |thr: f64| -> Option<f64> {
                let per_obj: Vec<f64> = acc
                    .per_object_d2
                    .values()
                    .map(|v| v.iter().filter(|d| **d <= thr).count() as f64 / v.len() as f64)
                    .collect();
                (!per_obj.is_empty()).then(|| per_obj.iter().sum::<f64>() / per_obj.len() as f64)
            };
            let n_objects = acc
                .per_object_sep
                .keys()
                .chain(acc.per_object_d2.keys())
                .collect::<BTreeSet<_>>()
                .len() as u32;
            SurfaceCell {
                class,
                tool,
                config_arm,
                sigma_table,
                arc_lo_days: ARC_BIN_EDGES_DAYS[ai],
                arc_hi_days: ARC_BIN_EDGES_DAYS[ai + 1],
                dt_lo_days: DT_BIN_EDGES_DAYS[di],
                dt_hi_days: DT_BIN_EDGES_DAYS[di + 1],
                n_objects,
                n_windows: acc.windows.len() as u32,
                n_predictions: acc.n_predictions,
                delivered_fraction: if acc.expected > 0 {
                    f64::from(acc.delivered) / f64::from(acc.expected)
                } else {
                    0.0
                },
                med_sep_arcsec: median(&mut sep_meds),
                med_d2_norm: median(&mut d2_meds).map(|m| m / LN2X2),
                cov_1s: cov(1.0),
                cov_2s: cov(4.0),
            }
        })
        .collect();

    // ---- Per-object summaries --------------------------------------------
    let mut summaries: BTreeMap<(String, String, String), ObjectWalkSummary> = BTreeMap::new();
    for w in windows {
        let class = idx
            .get(w.object.as_str())
            .map(|oi| oi.obj.class.clone())
            .unwrap_or_else(|| "unknown".into());
        let s = summaries
            .entry((w.object.clone(), w.tool.clone(), w.config_arm.clone()))
            .or_insert_with(|| ObjectWalkSummary {
                object: w.object.clone(),
                class,
                tool: w.tool.clone(),
                config_arm: w.config_arm.clone(),
                n_windows_expected: 0,
                n_windows_converged: 0,
                n_windows_failed: 0,
                n_predictions: 0,
                med_d2_norm: None,
                med_sep_arcsec: None,
            });
        s.n_windows_expected += 1;
        if w.converged {
            s.n_windows_converged += 1;
        } else {
            s.n_windows_failed += 1;
        }
    }
    for s in scored {
        if s.in_sample || !s.flags.is_empty() {
            continue;
        }
        if let Some(sum) =
            summaries.get_mut(&(s.object.clone(), s.tool.clone(), s.config_arm.clone()))
        {
            sum.n_predictions += 1;
        }
    }
    let mut per_obj_sep: HashMap<(String, String, String), Vec<f64>> = HashMap::new();
    for s in scored {
        if !s.in_sample && s.flags.is_empty() {
            per_obj_sep
                .entry((s.object.clone(), s.tool.clone(), s.config_arm.clone()))
                .or_default()
                .push(s.sep_arcsec);
        }
    }
    for (key, sum) in summaries.iter_mut() {
        if let Some(v) = per_obj_sep.get_mut(key) {
            sum.med_sep_arcsec = median(v);
        }
        if let Some(stats) = nights.get(&(key.1.clone(), key.2.clone(), key.0.clone())) {
            let mut d2s: Vec<f64> = stats.iter().map(|st| st.d2).collect();
            sum.med_d2_norm = median(&mut d2s).map(|m| m / LN2X2);
        }
    }

    // ---- d² histograms — per (tool, arm, σ-table, class + "all") -------
    type HistKey = (String, String, String, String);
    /// The linear body plus the log-spaced survival companion, filled from
    /// the same rows.
    #[derive(Default)]
    struct HistAcc {
        counts: Vec<u32>,
        overflow: u32,
        n: u32,
        log_counts: Vec<u32>,
        log_underflow: u32,
        log_overflow: u32,
        log_zero: u32,
    }
    let mut hists: BTreeMap<HistKey, HistAcc> = BTreeMap::new();
    for s in scored {
        if s.in_sample || !s.flags.is_empty() || s.obs_noise_dominated {
            continue;
        }
        let Some(d2) = s.d2_combined else { continue };
        for class in [s.class.as_str(), "all"] {
            let key = (
                s.tool.clone(),
                s.config_arm.clone(),
                s.sigma_table.clone(),
                class.to_string(),
            );
            let e = hists.entry(key).or_insert_with(|| HistAcc {
                counts: vec![0u32; D2_HIST_BINS],
                log_counts: vec![0u32; D2_LOG_BINS],
                ..HistAcc::default()
            });
            if d2 >= D2_HIST_DOMAIN {
                e.overflow += 1;
            } else {
                let b = ((d2 / D2_HIST_DOMAIN) * D2_HIST_BINS as f64) as usize;
                e.counts[b.min(D2_HIST_BINS - 1)] += 1;
            }
            if d2 <= 0.0 {
                e.log_zero += 1;
            } else {
                let l = d2.log10();
                if l < D2_LOG_LO {
                    e.log_underflow += 1;
                } else if l >= D2_LOG_HI {
                    e.log_overflow += 1;
                } else {
                    let b =
                        ((l - D2_LOG_LO) / (D2_LOG_HI - D2_LOG_LO) * D2_LOG_BINS as f64) as usize;
                    e.log_counts[b.min(D2_LOG_BINS - 1)] += 1;
                }
            }
            e.n += 1;
        }
    }
    let d2_histograms = hists
        .into_iter()
        .map(
            |((tool, config_arm, sigma_table, class), a)| crate::predict_schema::D2Histogram {
                tool,
                config_arm,
                sigma_table,
                class,
                domain_max: D2_HIST_DOMAIN,
                counts: a.counts,
                overflow: a.overflow,
                n: a.n,
                log_counts: a.log_counts,
                log10_lo: D2_LOG_LO,
                log10_hi: D2_LOG_HI,
                log_underflow: a.log_underflow,
                log_overflow: a.log_overflow,
                log_zero: a.log_zero,
            },
        )
        .collect();

    // ---- Per-window timelines — per (tool, arm, object) ------------------
    // The same two statistics the surfaces marginalize, resolved to the
    // window that produced them: the "watch the covariance become honest (or
    // not) as nights accumulate" view for one object.
    type WalkKey = (String, String, String);
    let mut win_d2: HashMap<(WalkKey, u32), Vec<f64>> = HashMap::new();
    for ((tool, arm, object), stats) in &nights {
        for st in stats {
            win_d2
                .entry(((tool.clone(), arm.clone(), object.clone()), st.window_index))
                .or_default()
                .push(st.d2);
        }
    }
    let mut win_sep: HashMap<(WalkKey, u32), Vec<f64>> = HashMap::new();
    for s in scored {
        if s.in_sample || !s.flags.is_empty() {
            continue;
        }
        win_sep
            .entry((
                (s.tool.clone(), s.config_arm.clone(), s.object.clone()),
                s.window_index,
            ))
            .or_default()
            .push(s.sep_arcsec);
    }
    // Attempted windows in schedule order — a failed window is a break in
    // the curve with its `converged` false, never a missing point.
    let mut walks: BTreeMap<WalkKey, BTreeMap<u32, bool>> = BTreeMap::new();
    for w in windows {
        walks
            .entry((w.tool.clone(), w.config_arm.clone(), w.object.clone()))
            .or_default()
            .insert(w.window_index, w.converged);
    }
    let mut per_window: Vec<crate::predict_schema::WalkTimeline> = Vec::new();
    for ((tool, arm, object), by_index) in walks {
        let Some(oi) = idx.get(object.as_str()) else {
            continue;
        };
        let key = (tool.clone(), arm.clone(), object.clone());
        let mut tl = crate::predict_schema::WalkTimeline {
            tool: tool.clone(),
            config_arm: arm.clone(),
            object: object.clone(),
            class: oi.obj.class.clone(),
            window_index: Vec::with_capacity(by_index.len()),
            arc_days: Vec::with_capacity(by_index.len()),
            converged: Vec::with_capacity(by_index.len()),
            med_d2_norm: Vec::with_capacity(by_index.len()),
            med_sep_arcsec: Vec::with_capacity(by_index.len()),
            n_preds: Vec::with_capacity(by_index.len()),
        };
        for (window_index, converged) in by_index {
            let Some(spec) = oi.windows.get(&window_index) else {
                continue;
            };
            let seps = win_sep.get(&(key.clone(), window_index));
            let d2s = win_d2.get(&(key.clone(), window_index));
            tl.window_index.push(window_index);
            tl.arc_days.push(round3(spec.arc_days));
            tl.converged.push(converged);
            tl.med_d2_norm.push(
                d2s.and_then(|v| median(&mut v.clone()))
                    .map(|m| round3(m / LN2X2)),
            );
            tl.med_sep_arcsec
                .push(seps.and_then(|v| median(&mut v.clone())).map(round3));
            tl.n_preds.push(seps.map_or(0, |v| v.len()) as u32);
        }
        if !tl.window_index.is_empty() {
            per_window.push(tl);
        }
    }
    // Embed budget. Reducing to the d²-carrying walks keeps the panel's
    // headline statistic and drops the accuracy-only walks; the note rides
    // the aggregate so the page can say what is missing.
    let mut per_window_note = None;
    let full_bytes = serde_json::to_string(&per_window).map_or(0, |s| s.len());
    if full_bytes > PER_WINDOW_BUDGET_BYTES {
        let before = per_window.len();
        per_window.retain(|t| t.med_d2_norm.iter().any(Option::is_some));
        per_window_note = Some(format!(
            "per-window timelines reduced to fit the {} MB embed budget: the full family \
             serialized to {:.1} MB, so only the {} of {} walks carrying a held-out d² are \
             embedded. The dropped walks are accuracy-only; their separations remain in the \
             scoring sidecars and in the surfaces above.",
            PER_WINDOW_BUDGET_BYTES / (1024 * 1024),
            full_bytes as f64 / (1024.0 * 1024.0),
            per_window.len(),
            before,
        ));
    }

    // ---- Reduced-χ² histograms — per (tool, arm), converged windows ------
    struct RChi2Acc {
        counts: Vec<u32>,
        underflow: u32,
        overflow: u32,
        n_with: u32,
        n_without: u32,
        n_nonpositive: u32,
        dof: BTreeMap<u32, u32>,
    }
    let mut rchi2: BTreeMap<(String, String), RChi2Acc> = BTreeMap::new();
    for w in windows {
        if !w.converged {
            continue;
        }
        let acc = rchi2
            .entry((w.tool.clone(), w.config_arm.clone()))
            .or_insert_with(|| RChi2Acc {
                counts: vec![0u32; RCHI2_HIST_BINS],
                underflow: 0,
                overflow: 0,
                n_with: 0,
                n_without: 0,
                n_nonpositive: 0,
                dof: BTreeMap::new(),
            });
        let Some(r) = w.reduced_chi2 else {
            acc.n_without += 1;
            continue;
        };
        acc.n_with += 1;
        if !r.is_finite() || r <= 0.0 {
            acc.n_nonpositive += 1;
            continue;
        }
        // ν from the fit's own two numbers rather than from an assumed
        // 2·n_obs − n_solve_for: a runner that rejects observations reports
        // the χ² it actually formed, and only the ratio knows how many rows
        // survived that rejection.
        if let Some(chi2) = w.chi2 {
            let nu = chi2 / r;
            if nu.is_finite() && (1.0..1.0e6).contains(&nu) {
                // Two significant figures, so the tally stays a few dozen
                // pairs instead of one per distinct ν. Exact below 100 —
                // which is the whole range where χ²_ν/ν is wide enough to
                // matter — and past that the distribution is so close to a
                // spike at 1 that the rounding is invisible in the envelope.
                let mag = 10f64.powi(nu.log10().floor() as i32 - 1).max(1.0);
                let q = (nu / mag).round() * mag;
                *acc.dof.entry(q as u32).or_insert(0) += 1;
            }
        }
        let l = r.log10();
        if l < RCHI2_LOG10_LO {
            acc.underflow += 1;
        } else if l >= RCHI2_LOG10_HI {
            acc.overflow += 1;
        } else {
            let b = ((l - RCHI2_LOG10_LO) / (RCHI2_LOG10_HI - RCHI2_LOG10_LO)
                * RCHI2_HIST_BINS as f64) as usize;
            acc.counts[b.min(RCHI2_HIST_BINS - 1)] += 1;
        }
    }
    let reduced_chi2 = rchi2
        .into_iter()
        .map(
            |((tool, config_arm), a)| crate::predict_schema::ReducedChi2Histogram {
                tool,
                config_arm,
                log10_lo: RCHI2_LOG10_LO,
                log10_hi: RCHI2_LOG10_HI,
                counts: a.counts,
                underflow: a.underflow,
                overflow: a.overflow,
                n_with: a.n_with,
                n_without: a.n_without,
                n_nonpositive: a.n_nonpositive,
                dof: a
                    .dof
                    .into_iter()
                    .map(|(ndof, n)| crate::predict_schema::DofBin { ndof, n })
                    .collect(),
            },
        )
        .collect();

    PredictAggregates {
        snapshot_id: manifest.snapshot_id.clone(),
        cells,
        per_object: summaries.into_values().collect(),
        d2_histograms,
        per_window,
        reduced_chi2,
        per_window_note,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::predict_schema::{
        ObjectWindows, ObsEntry, TargetSpec, WindowManifest, WindowSpec, classes,
    };

    const ARCSEC2DEG: f64 = 1.0 / 3600.0;

    #[test]
    fn read_jsonl_tolerates_torn_final_line_only() {
        let dir = std::env::temp_dir().join(format!("covreal_torn_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Torn FINAL line: skipped loudly, prior rows survive.
        let torn_last = dir.join("torn_last.jsonl");
        std::fs::write(&torn_last, "{\"a\":1}\n{\"a\":2}\n{\"a\":3,\"tr").unwrap();
        let rows: Vec<serde_json::Value> = read_jsonl_files(&[torn_last]).unwrap();
        assert_eq!(rows.len(), 2);
        // Torn MIDDLE line: still a hard error naming the line.
        let torn_mid = dir.join("torn_mid.jsonl");
        std::fs::write(&torn_mid, "{\"a\":1}\n{\"a\":2,\"tr\n{\"a\":3}\n").unwrap();
        let err = read_jsonl_files::<serde_json::Value>(&[torn_mid]).unwrap_err();
        assert!(err.contains("line 2"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Deterministic standard-normal sampler: 64-bit LCG + Box–Muller. No
    /// external RNG dependency, fully reproducible.
    struct Normal {
        state: u64,
        spare: Option<f64>,
    }
    impl Normal {
        fn new(seed: u64) -> Self {
            Self {
                state: seed.max(1),
                spare: None,
            }
        }
        fn uniform(&mut self) -> f64 {
            self.state = self
                .state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.state >> 11) as f64) / ((1u64 << 53) as f64)
        }
        fn sample(&mut self) -> f64 {
            if let Some(z) = self.spare.take() {
                return z;
            }
            let (u1, u2) = (self.uniform().max(1e-16), self.uniform());
            let r = (-2.0 * u1.ln()).sqrt();
            let (s, c) = (2.0 * std::f64::consts::PI * u2).sin_cos();
            self.spare = Some(r * s);
            r * c
        }
    }

    fn obs(ra_deg: f64, dec_deg: f64, night: i64, sigma: f64) -> ObsEntry {
        ObsEntry {
            stn: "F51".into(),
            obs_time: format!("2025-01-01T00:00:00Z#{night}"),
            key_hash: crate::predict_schema::obs_key_hash(
                "F51",
                &format!("2025-01-01T00:00:00Z#{night}"),
                &ra_deg.to_string(),
                &dec_deg.to_string(),
            ),
            mjd_utc: night as f64 + 0.5,
            night,
            ra_deg,
            dec_deg,
            ast_cat: "Gaia2".into(),
            mode: "CCD".into(),
            sigma_ra_arcsec: sigma,
            sigma_dec_arcsec: sigma,
            sigma_corr: 0.0,
            sigma_source: "ades".into(),
            ades_rms_ra: Some(sigma),
            ades_rms_dec: Some(sigma),
            debias_dra_arcsec: Some(0.0),
            debias_ddec_arcsec: Some(0.0),
            excluded: None,
        }
    }

    /// One object, one window, `entries` as held-out targets at 5-day
    /// horizon (arc 50 d) unless overridden per test.
    fn manifest(entries: Vec<ObsEntry>) -> WindowManifest {
        let targets = (0..entries.len() as u32)
            .map(|i| TargetSpec {
                obs_idx: i,
                rung: "next".into(),
                horizon_days: 5.0,
            })
            .collect();
        WindowManifest {
            schema_version: crate::predict_schema::MANIFEST_SCHEMA_VERSION,
            snapshot_id: "test-snap".into(),
            bundle_nights: 1,
            generated_by: "test".into(),
            ladder_days: vec![7.0],
            ladder_tolerance: 0.5,
            min_horizon_days: 0.3,
            max_obs_per_night: 12,
            objects: vec![ObjectWindows {
                object: "TestObj".into(),
                mpc_designation: "T1".into(),
                population: "NEO".into(),
                class: classes::NEA.into(),
                eligible: true,
                ineligible_reason: None,
                first_night: 59000,
                apparition_end_night: 59100,
                n_obs_total: entries.len() as u32,
                n_excluded_space_based: 0,
                observations: entries,
                windows: vec![WindowSpec {
                    index: 0,
                    cut_night: 59050,
                    cut_utc: "2020-08-19T00:00:00Z".into(),
                    cut_mjd_tt: 59050.0008,
                    cut_mjd_tdb: 59050.0008,
                    intra_night: false,
                    profiles: vec!["full".into()],
                    n_obs_fit: 10,
                    last_fit_obs_idx: 0,
                    arc_days: 50.0,
                    targets,
                    in_sample: vec![],
                    seed: None,
                }],
            }],
        }
    }

    fn prediction(
        m: &WindowManifest,
        obs_idx: u32,
        ra_deg: f64,
        dec_deg: f64,
        cov_arcsec2: Option<Mat2>,
    ) -> PredictedObservation {
        let e = &m.objects[0].observations[obs_idx as usize];
        PredictedObservation {
            object: "TestObj".into(),
            tool: "rust".into(),
            config_arm: "default".into(),
            window_index: 0,
            obs_idx,
            key_hash: e.key_hash,
            in_sample: false,
            ra_deg,
            dec_deg,
            epoch_scale_used: "tdb".into(),
            uncertainty_form: if cov_arcsec2.is_some() {
                uncertainty_forms::RADEC_2X2.into()
            } else {
                uncertainty_forms::NONE.into()
            },
            cov_radec: cov_arcsec2,
            cov_units: cov_arcsec2.map(|_| "arcsec2".into()),
            cov_basis: cov_arcsec2.map(|_| "great_circle".into()),
            sigma1_arcsec: None,
            sigma1_pa_deg: None,
            pa_motion_deg: Some(90.0),
            sky_rate_deg_day: Some(0.5),
        }
    }

    fn opts() -> ScoreOptions {
        ScoreOptions {
            sigma_table: "pinned".into(),
            apply_debias: true,
        }
    }

    /// A converged window record for `tool`/`arm` on the test object.
    fn window_record(tool: &str, arm: &str, reduced_chi2: Option<f64>) -> WalkWindowRecord {
        WalkWindowRecord {
            object: "TestObj".into(),
            tool: tool.into(),
            config_arm: arm.into(),
            window_index: 0,
            converged: true,
            failure: None,
            n_obs_used: Some(10),
            n_obs_rejected: Some(0),
            n_solve_for: Some(6),
            iterations: Some(3),
            chi2: Some(8.0),
            reduced_chi2,
            fit_epoch_mjd_tdb: Some(59050.0),
            warm_start: false,
            seed_source: "own_iod".into(),
            covariance_trust: None,
            fit_time_ms: 1.0,
            predict_time_ms: Some(1.0),
            captured: None,
        }
    }

    #[test]
    fn per_window_timelines_carry_the_cells_statistics_per_window() {
        // The timeline family is the surfaces' own data resolved to the
        // window that produced it: same filters, same normalization. A
        // window a tool attempted but did not converge is a false in
        // `converged` with null statistics — never a missing point.
        let (s_obs, s_pred) = (0.3_f64, 0.4_f64);
        let (ra0, dec0) = (100.0_f64, 20.0_f64);
        let cosd = dec0.to_radians().cos();
        let mut rng = Normal::new(0xBEEF);
        let sig = (s_obs * s_obs + s_pred * s_pred).sqrt();
        let n = 40u32;
        let entries: Vec<ObsEntry> = (0..n)
            .map(|i| {
                obs(
                    ra0 + rng.sample() * sig * ARCSEC2DEG / cosd,
                    dec0 + rng.sample() * sig * ARCSEC2DEG,
                    59060 + i as i64,
                    s_obs,
                )
            })
            .collect();
        let m = manifest(entries);
        let preds: Vec<PredictedObservation> = (0..n)
            .map(|i| {
                prediction(
                    &m,
                    i,
                    ra0,
                    dec0,
                    Some([[s_pred * s_pred, 0.0], [0.0, s_pred * s_pred]]),
                )
            })
            .collect();
        let mut failed = window_record("rust", "no-rejection", Some(1.5));
        failed.converged = false;
        failed.failure = Some("IOD failed".into());
        failed.reduced_chi2 = None;
        let windows = vec![window_record("rust", "default", Some(0.9)), failed];
        let (scored, sp) = score_all(&m, &preds, &windows, &opts()).unwrap();
        let agg = aggregate(&m, &scored, &windows, &sp);

        assert_eq!(
            agg.per_window.len(),
            2,
            "one timeline per (tool, arm, object)"
        );
        let tl = agg
            .per_window
            .iter()
            .find(|t| t.config_arm == "default")
            .expect("the scored arm has a timeline");
        assert_eq!(tl.object, "TestObj");
        assert_eq!(tl.class, classes::NEA);
        assert_eq!(tl.window_index, vec![0]);
        assert_eq!(tl.arc_days, vec![50.0]);
        assert_eq!(tl.converged, vec![true]);
        assert_eq!(tl.n_preds, vec![n]);
        // Same statistic the (arc × dt) cell publishes for this window.
        let cell = agg
            .cells
            .iter()
            .find(|c| c.config_arm == "default")
            .unwrap();
        let (tl_d2, cell_d2) = (tl.med_d2_norm[0].unwrap(), cell.med_d2_norm.unwrap());
        assert!(
            (tl_d2 - cell_d2).abs() < 5e-3,
            "timeline d²ₙ {tl_d2} disagrees with the cell's {cell_d2}"
        );
        let (tl_sep, cell_sep) = (tl.med_sep_arcsec[0].unwrap(), cell.med_sep_arcsec.unwrap());
        assert!(
            (tl_sep - cell_sep).abs() < 5e-3,
            "timeline separation {tl_sep} disagrees with the cell's {cell_sep}"
        );
        // The failed arm still gets a point, with nulls rather than zeros.
        let bad = agg
            .per_window
            .iter()
            .find(|t| t.config_arm == "no-rejection")
            .expect("an all-failed walk is still a timeline");
        assert_eq!(bad.converged, vec![false]);
        assert_eq!(bad.med_d2_norm, vec![None]);
        assert_eq!(bad.med_sep_arcsec, vec![None]);
        assert_eq!(bad.n_preds, vec![0]);
        assert!(agg.per_window_note.is_none(), "family is far under budget");
    }

    #[test]
    fn reduced_chi2_histograms_bin_converged_windows_and_name_the_gaps() {
        // Log₁₀ binning over converged windows only, with three honest
        // counters: not-reported, non-positive, and the two tails.
        let m = manifest(vec![obs(100.0, 20.0, 59060, 0.3)]);
        let mut not_converged = window_record("rust", "default", Some(1.0));
        not_converged.converged = false;
        let windows = vec![
            window_record("rust", "default", Some(1.0)),
            window_record("rust", "default", Some(10.0)),
            window_record("rust", "default", Some(1e-6)),
            window_record("rust", "default", Some(1e6)),
            window_record("rust", "default", Some(0.0)),
            window_record("rust", "default", None),
            // A non-converged window is out of scope entirely.
            not_converged,
            window_record("layup", "default", None),
        ];
        let agg = aggregate(&m, &[], &windows, &HashMap::new());
        let h = agg
            .reduced_chi2
            .iter()
            .find(|h| h.tool == "rust")
            .expect("rust histogram");
        assert_eq!((h.log10_lo, h.log10_hi), (RCHI2_LOG10_LO, RCHI2_LOG10_HI));
        assert_eq!(h.counts.len(), RCHI2_HIST_BINS);
        assert_eq!(h.n_with, 5, "five converged windows reported an rχ²");
        assert_eq!(h.n_without, 1, "the sixth reported none");
        assert_eq!(h.n_nonpositive, 1, "rχ² = 0 has no log₁₀");
        assert_eq!(h.underflow, 1);
        assert_eq!(h.overflow, 1);
        assert_eq!(
            h.counts.iter().sum::<u32>(),
            2,
            "1.0 and 10.0 are in-domain"
        );
        // rχ² = 1 sits at log₁₀ = 0, the centre of a [-2, +2] domain.
        assert_eq!(h.counts[RCHI2_HIST_BINS / 2], 1);
        // A runner that never publishes one shows as "not reported", not as
        // an absent series and not as a zero.
        let l = agg
            .reduced_chi2
            .iter()
            .find(|h| h.tool == "layup")
            .expect("layup histogram");
        assert_eq!((l.n_with, l.n_without), (0, 1));
        assert_eq!(l.counts.iter().sum::<u32>(), 0);
    }

    #[test]
    fn reduced_chi2_carries_the_dof_tally_the_fit_quality_envelope_needs() {
        // A reduced χ² of 1 is only the expectation; the scatter around it is
        // set by ν. The report draws the expected spread of χ²_ν/ν under the
        // ν actually present, so the aggregate has to carry them — derived
        // from the fit's own χ² / rχ², never from an assumed
        // 2·n_obs − n_solve_for, because a runner that rejected observations
        // formed its χ² on the rows it kept.
        let m = manifest(vec![obs(100.0, 20.0, 59060, 0.3)]);
        let mut windows = Vec::new();
        for (chi2, rchi2, times) in [(8.0, 1.0, 3), (40.0, 1.0, 2), (8.0, 2.0, 1)] {
            for _ in 0..times {
                let mut w = window_record("rust", "default", Some(rchi2));
                w.chi2 = Some(chi2);
                windows.push(w);
            }
        }
        // A window whose runner published no χ² contributes no ν, and must
        // not be invented from n_obs.
        let mut no_chi2 = window_record("rust", "default", Some(1.0));
        no_chi2.chi2 = None;
        windows.push(no_chi2);
        let agg = aggregate(&m, &[], &windows, &HashMap::new());
        let h = agg
            .reduced_chi2
            .iter()
            .find(|h| h.tool == "rust")
            .expect("rust histogram");
        assert_eq!(
            h.dof
                .iter()
                .map(|d| (d.ndof, d.n))
                .collect::<Vec<(u32, u32)>>(),
            vec![(4, 1), (8, 3), (40, 2)],
            "ν = χ²/rχ², tallied ascending; the χ²-less window contributes none"
        );
        assert_eq!(
            h.n_with, 7,
            "all seven converged windows still report a reduced χ²"
        );
    }

    #[test]
    fn d2_histograms_carry_log_spaced_survival_bins_reaching_the_far_tail() {
        // The linear family stops at d² = 10 and buries the whole tail in one
        // overflow integer. The survival curve needs the tail RESOLVED, so a
        // log-spaced companion runs to 10³ off the same rows. Both families
        // must describe the same sample: equal totals, and a survival read at
        // the χ²₂ 99% point that agrees with counting the rows by hand.
        let (s_obs, s_pred) = (0.05_f64, 1.0_f64);
        let (ra0, dec0) = (100.0_f64, 20.0_f64);
        let cosd = dec0.to_radians().cos();
        // Separations chosen to land on both sides of the χ²₂ 99% point and
        // past the top of the linear domain, plus one beyond 10³.
        let offsets = [0.2_f64, 0.9, 1.5, 2.2, 3.1, 6.0, 15.0, 60.0];
        let mut entries = Vec::new();
        for (i, dx) in offsets.iter().enumerate() {
            entries.push(obs(
                ra0 + dx * ARCSEC2DEG / cosd,
                dec0,
                59060 + i as i64,
                s_obs,
            ));
        }
        let m = manifest(entries);
        let preds: Vec<PredictedObservation> = (0..offsets.len() as u32)
            .map(|i| {
                prediction(
                    &m,
                    i,
                    ra0,
                    dec0,
                    Some([[s_pred * s_pred, 0.0], [0.0, s_pred * s_pred]]),
                )
            })
            .collect();
        let windows = vec![window_record("rust", "default", Some(0.9))];
        let (scored, sp) = score_all(&m, &preds, &windows, &opts()).unwrap();
        let agg = aggregate(&m, &scored, &windows, &sp);
        let h = agg
            .d2_histograms
            .iter()
            .find(|h| h.class == "all" && h.tool == "rust")
            .expect("pooled histogram");

        assert_eq!((h.log10_lo, h.log10_hi), (D2_LOG_LO, D2_LOG_HI));
        assert_eq!(h.log_counts.len(), D2_LOG_BINS);
        let log_total: u32 =
            h.log_counts.iter().sum::<u32>() + h.log_underflow + h.log_overflow + h.log_zero;
        assert_eq!(
            log_total, h.n,
            "the log family must describe exactly the rows the linear one does"
        );
        assert_eq!(
            h.counts.iter().sum::<u32>() + h.overflow,
            h.n,
            "the linear family must still total n"
        );
        let width = (D2_LOG_HI - D2_LOG_LO) / D2_LOG_BINS as f64;
        let edge = |i: usize| D2_LOG_LO + i as f64 * width;
        // The far tail is RESOLVED rather than collapsed: the rows the linear
        // family folds into one overflow integer land in distinct log bins,
        // and the ones past 10³ are their own counter.
        let above_domain: Vec<usize> = h
            .log_counts
            .iter()
            .enumerate()
            .filter(|(i, c)| **c > 0 && edge(*i) >= 1.0)
            .map(|(i, _)| i)
            .collect();
        assert!(
            above_domain.len() > 1,
            "the tail beyond d² = 10 must occupy more than one bin: {above_domain:?}"
        );
        assert_eq!(
            h.overflow,
            above_domain.iter().map(|i| h.log_counts[*i]).sum::<u32>() + h.log_overflow,
            "the linear overflow and the resolved log tail must be the same rows"
        );
        assert!(
            h.log_overflow > 0,
            "the rows past 10³ have their own counter"
        );
        // Read the survival at a bin edge, where the histogram is exact, and
        // check it against counting the rows by hand.
        let held_out = |lo: f64| {
            scored
                .iter()
                .filter(|s| !s.in_sample && s.flags.is_empty() && !s.obs_noise_dominated)
                .filter(|s| s.d2_combined.is_some_and(|d| d >= lo))
                .count() as u32
        };
        let at = 80; // edge(80) = 10^1
        let from_bins: u32 = h.log_counts[at..].iter().sum::<u32>() + h.log_overflow;
        assert_eq!(
            from_bins,
            held_out(10.0_f64.powf(edge(at))),
            "survival read at a bin edge must agree with counting the rows"
        );
        // The χ²₂ 99% point is where the report reads its headline tail; the
        // binning has to resolve it to better than a bin.
        assert!(
            width <= 0.05,
            "{width} dex per bin is too coarse to place the χ²₂ 99% point"
        );
    }

    #[test]
    fn coverage_recovers_from_a_known_covariance() {
        // Residuals drawn from Σ_true = Σ_obs + Σ_pred must recover
        // median(d²)/(2 ln 2) = 1 and the 2-dof coverage targets. Each draw
        // gets its own night so the night statistic is the per-draw d².
        let (s_obs, s_pred) = (0.3_f64, 0.4_f64);
        let sig = (s_obs * s_obs + s_pred * s_pred).sqrt();
        let (ra0, dec0) = (100.0_f64, 20.0_f64);
        let cosd = dec0.to_radians().cos();
        let mut rng = Normal::new(0xC0FFEE);
        let n = 20_000;
        let mut entries = Vec::with_capacity(n);
        let mut draws = Vec::with_capacity(n);
        for i in 0..n {
            let (dx, dy) = (rng.sample() * sig, rng.sample() * sig);
            entries.push(obs(
                ra0 + dx * ARCSEC2DEG / cosd,
                dec0 + dy * ARCSEC2DEG,
                59060 + i as i64,
                s_obs,
            ));
            draws.push((dx, dy));
        }
        let m = manifest(entries);
        let preds: Vec<PredictedObservation> = (0..n as u32)
            .map(|i| {
                prediction(
                    &m,
                    i,
                    ra0,
                    dec0,
                    Some([[s_pred * s_pred, 0.0], [0.0, s_pred * s_pred]]),
                )
            })
            .collect();
        let windows = vec![WalkWindowRecord {
            object: "TestObj".into(),
            tool: "rust".into(),
            config_arm: "default".into(),
            window_index: 0,
            converged: true,
            failure: None,
            n_obs_used: Some(10),
            n_obs_rejected: Some(0),
            n_solve_for: Some(6),
            iterations: Some(3),
            chi2: Some(8.0),
            reduced_chi2: Some(0.9),
            fit_epoch_mjd_tdb: Some(59050.0),
            warm_start: false,
            seed_source: "own_iod".into(),
            covariance_trust: None,
            fit_time_ms: 1.0,
            predict_time_ms: Some(1.0),
            captured: None,
        }];
        let (scored, sp) = score_all(&m, &preds, &windows, &opts()).unwrap();
        assert_eq!(scored.len(), n);
        assert!(
            scored.iter().all(|s| s.flags.is_empty()),
            "no flags expected"
        );
        let agg = aggregate(&m, &scored, &windows, &sp);
        assert_eq!(agg.cells.len(), 1, "one (arc, dt) cell expected");
        let c = &agg.cells[0];
        let med = c.med_d2_norm.unwrap();
        assert!(
            (med - 1.0).abs() < 0.03,
            "median(d²)/(2 ln 2) = {med}, want 1±0.03"
        );
        let c1 = c.cov_1s.unwrap();
        let c2 = c.cov_2s.unwrap();
        assert!(
            (c1 - 0.3935).abs() < 0.02,
            "1σ coverage {c1}, want 0.3935±0.02"
        );
        assert!(
            (c2 - 0.8647).abs() < 0.02,
            "2σ coverage {c2}, want 0.8647±0.02"
        );
        assert_eq!(c.n_objects, 1);
        assert!((c.delivered_fraction - 1.0).abs() < 1e-12);
        // The χ²₂ p-value of the median-normalized statistic is ~0.5 at the
        // median draw — spot-check chi2_sf wiring.
        let p = hyperjet::statistics::chi2_sf(2.0 * std::f64::consts::LN_2, 2);
        assert!((p - 0.5).abs() < 1e-12);
    }

    #[test]
    fn native_ra_contraction_matches_great_circle() {
        let dec = 60.0_f64;
        let c = dec.to_radians().cos();
        let gc = [[0.04, 0.01], [0.01, 0.09]];
        let native = [[0.04 / (c * c), 0.01 / c], [0.01 / c, 0.09]];
        let a = normalize_pred_cov(&gc, "arcsec2", "great_circle", dec).unwrap();
        let b = normalize_pred_cov(&native, "arcsec2", "native_ra", dec).unwrap();
        for i in 0..2 {
            for j in 0..2 {
                assert!((a[i][j] - b[i][j]).abs() < 1e-15, "[{i}][{j}]");
            }
        }
    }

    #[test]
    fn unit_normalization_is_consistent() {
        let arcsec2 = [[0.25, 0.05], [0.05, 0.16]];
        let deg2 = arcsec2.map(|r| r.map(|v| v / (3600.0 * 3600.0)));
        let rad2 = arcsec2.map(|r| r.map(|v| v / (RAD2ARCSEC * RAD2ARCSEC)));
        let a = normalize_pred_cov(&arcsec2, "arcsec2", "great_circle", 0.0).unwrap();
        let b = normalize_pred_cov(&deg2, "deg2", "great_circle", 0.0).unwrap();
        let c = normalize_pred_cov(&rad2, "rad2", "great_circle", 0.0).unwrap();
        for i in 0..2 {
            for j in 0..2 {
                assert!((a[i][j] - b[i][j]).abs() < 1e-12);
                assert!((a[i][j] - c[i][j]).abs() < 1e-12);
            }
        }
        assert!(normalize_pred_cov(&arcsec2, "furlong2", "great_circle", 0.0).is_err());
        assert!(normalize_pred_cov(&arcsec2, "arcsec2", "sideways", 0.0).is_err());
    }

    #[test]
    fn gnomonic_pins_at_high_declination() {
        let (ra0, dec0) = (100.0, 60.0_f64);
        let cosd = dec0.to_radians().cos();
        // +1″ of great-circle RA offset = 1/cos δ arcsec of RA coordinate.
        let (e, n) = gnomonic_arcsec(ra0, dec0, ra0 + ARCSEC2DEG / cosd, dec0).unwrap();
        assert!((e - 1.0).abs() < 1e-6, "east {e}");
        // A constant-declination offset is not a great circle: the exact
        // projection carries the curvature term \( \eta = \xi^2 \tan\delta/2 \)
        // (ξ, η in radians) — for ξ = 1″ at δ = 60° that is 4.2e-6″. Its
        // presence is evidence the projection is exact rather than flat-sky.
        let xi_rad = 1.0 / RAD2ARCSEC;
        let expected_north = xi_rad * xi_rad * dec0.to_radians().tan() / 2.0 * RAD2ARCSEC;
        assert!(
            (n - expected_north).abs() < 1e-8,
            "north {n} vs {expected_north}"
        );
        let (e, n) = gnomonic_arcsec(ra0, dec0, ra0, dec0 + ARCSEC2DEG).unwrap();
        assert!(e.abs() < 1e-6, "east {e}");
        assert!((n - 1.0).abs() < 1e-6, "north {n}");
    }

    #[test]
    fn large_separation_flags_and_withholds_d2() {
        let entries = vec![obs(100.0, 20.0, 59060, 0.3)];
        let m = manifest(entries);
        // Prediction 2° away from the observation.
        let p = prediction(&m, 0, 102.0, 20.0, Some([[0.01, 0.0], [0.0, 0.01]]));
        let (scored, _) = score_all(&m, &[p], &[], &opts()).unwrap();
        let s = &scored[0];
        assert!(s.flags.iter().any(|f| f == "d2_invalid_large_separation"));
        assert!(s.d2_combined.is_none());
        assert!(s.sep_arcsec > 3600.0);
    }

    #[test]
    fn at_ct_rotation_pin() {
        // PA = 90° (motion due East): along-track = +East, cross-track from
        // e_ct = (−cos θ, sin θ) = (0, 1) = +North.
        let (e_at, e_ct) = at_ct_basis(90.0);
        assert!((e_at[0] - 1.0).abs() < 1e-12 && e_at[1].abs() < 1e-12);
        assert!(e_ct[0].abs() < 1e-12 && (e_ct[1] - 1.0).abs() < 1e-12);
        // r = (1″ E, 2″ N) → AT = 1, CT = 2.
        let entries = vec![obs(
            100.0 + ARCSEC2DEG / 20.0_f64.to_radians().cos(),
            20.0 + 2.0 * ARCSEC2DEG,
            59060,
            0.3,
        )];
        let m = manifest(entries);
        let p = prediction(&m, 0, 100.0, 20.0, Some([[0.01, 0.0], [0.0, 0.01]]));
        let (scored, _) = score_all(&m, &[p], &[], &opts()).unwrap();
        let s = &scored[0];
        assert!((s.resid_at_arcsec.unwrap() - 1.0).abs() < 1e-5);
        assert!((s.resid_ct_arcsec.unwrap() - 2.0).abs() < 1e-5);
    }

    #[test]
    fn non_psd_covariance_flags() {
        let entries = vec![obs(100.0, 20.0, 59060, 0.3)];
        let m = manifest(entries);
        let p = prediction(&m, 0, 100.0, 20.0, Some([[1.0, 2.0], [2.0, 1.0]]));
        let (scored, _) = score_all(&m, &[p], &[], &opts()).unwrap();
        let s = &scored[0];
        assert!(s.flags.iter().any(|f| f == "non_psd_covariance"));
        assert!(s.d2_combined.is_none());
        assert!(s.d2_pred_only.is_none());
    }

    #[test]
    fn sigma1d_pa_z1_pin() {
        // σ₁ = 1″ along PA 90° (East); Σ_obs = 1″² isotropic; r = 2″ East:
        // z₁ = 2 / √(1 + 1) = √2.
        let cosd = 20.0_f64.to_radians().cos();
        let entries = vec![obs(100.0 + 2.0 * ARCSEC2DEG / cosd, 20.0, 59060, 1.0)];
        let m = manifest(entries);
        let mut p = prediction(&m, 0, 100.0, 20.0, None);
        p.uncertainty_form = uncertainty_forms::SIGMA1D_PA.into();
        p.sigma1_arcsec = Some(1.0);
        p.sigma1_pa_deg = Some(90.0);
        let (scored, _) = score_all(&m, &[p], &[], &opts()).unwrap();
        let z1 = scored[0].z1_dominant.unwrap();
        assert!((z1 - std::f64::consts::SQRT_2).abs() < 1e-5, "z1 = {z1}");
        assert!(scored[0].d2_combined.is_none());
    }

    #[test]
    fn key_hash_mismatch_is_a_loud_error() {
        let entries = vec![obs(100.0, 20.0, 59060, 0.3)];
        let m = manifest(entries);
        let mut p = prediction(&m, 0, 100.0, 20.0, None);
        p.key_hash ^= 1;
        let err = score_all(&m, &[p], &[], &opts()).unwrap_err();
        assert!(err.contains("key_hash mismatch"), "{err}");
    }

    #[test]
    fn night_mean_uses_shared_pred_covariance() {
        // Two observations, same night, identical residual r = (1″, 0):
        // Σ_night = Σ_pred + Σ_obs/2 = 1 + 0.5 = 1.5 → d² = 1/1.5.
        let cosd = 20.0_f64.to_radians().cos();
        let entries = vec![
            obs(100.0 + ARCSEC2DEG / cosd, 20.0, 59060, 1.0),
            obs(100.0 + ARCSEC2DEG / cosd, 20.0 + 1e-9, 59060, 1.0),
        ];
        let m = manifest(entries);
        let cov = Some([[1.0, 0.0], [0.0, 1.0]]);
        let preds = vec![
            prediction(&m, 0, 100.0, 20.0, cov),
            prediction(&m, 1, 100.0, 20.0, cov),
        ];
        let (scored, sp) = score_all(&m, &preds, &[], &opts()).unwrap();
        let nights = night_stats(&m, &scored, &sp);
        let stats = &nights[&("rust".into(), "default".into(), "TestObj".into())];
        assert_eq!(stats.len(), 1);
        let d2 = stats[0].d2;
        assert!(
            (d2 - 1.0 / 1.5).abs() < 1e-6,
            "night d² = {d2}, want 0.6667"
        );
    }

    #[test]
    fn debias_shifts_the_observed_position() {
        let cosd = 20.0_f64.to_radians().cos();
        // Observation sits 1″ East of the prediction, and the debias table
        // says the catalog bias is exactly that 1″ — debiased residual 0.
        let mut e = obs(100.0 + ARCSEC2DEG / cosd, 20.0, 59060, 0.3);
        e.debias_dra_arcsec = Some(1.0);
        e.debias_ddec_arcsec = Some(0.0);
        let m = manifest(vec![e]);
        let p = prediction(&m, 0, 100.0, 20.0, Some([[0.01, 0.0], [0.0, 0.01]]));
        let (scored, _) = score_all(&m, std::slice::from_ref(&p), &[], &opts()).unwrap();
        assert!(scored[0].debias_applied);
        assert!(scored[0].sep_arcsec < 1e-5, "sep {}", scored[0].sep_arcsec);
        // Raw arm: the same row scores the full 1″.
        let raw = ScoreOptions {
            sigma_table: "pinned".into(),
            apply_debias: false,
        };
        let (scored, _) = score_all(&m, &[p], &[], &raw).unwrap();
        assert!(!scored[0].debias_applied);
        assert!((scored[0].sep_arcsec - 1.0).abs() < 1e-5);
    }

    #[test]
    fn unknown_catalog_without_debias_is_flagged() {
        let mut e = obs(100.0, 20.0, 59060, 0.3);
        e.debias_dra_arcsec = None;
        e.debias_ddec_arcsec = None;
        let m = manifest(vec![e]);
        let p = prediction(&m, 0, 100.0, 20.0, None);
        let (scored, _) = score_all(&m, &[p], &[], &opts()).unwrap();
        assert!(scored[0].flags.iter().any(|f| f == "unk_catalog_no_debias"));
    }

    #[test]
    fn ades_only_table_skips_rows_without_ades_sigma() {
        let mut e = obs(100.0, 20.0, 59060, 0.3);
        e.ades_rms_ra = None;
        e.ades_rms_dec = None;
        let m = manifest(vec![e]);
        let p = prediction(&m, 0, 100.0, 20.0, None);
        let table = ScoreOptions {
            sigma_table: "ades_only".into(),
            apply_debias: true,
        };
        let (scored, _) = score_all(&m, &[p], &[], &table).unwrap();
        assert!(scored.is_empty());
        let bad = ScoreOptions {
            sigma_table: "vibes".into(),
            apply_debias: true,
        };
        let e2 = obs(100.0, 20.0, 59060, 0.3);
        let m2 = manifest(vec![e2]);
        let p2 = prediction(&m2, 0, 100.0, 20.0, None);
        assert!(score_all(&m2, &[p2], &[], &bad).is_err());
    }

    #[test]
    fn dominant_eigenvector_pin() {
        let e = dominant_eigenvector2(&[[4.0, 0.0], [0.0, 1.0]]);
        assert!((e[0].abs() - 1.0).abs() < 1e-12 && e[1].abs() < 1e-12);
        let e = dominant_eigenvector2(&[[2.0, 1.0], [1.0, 2.0]]);
        // Larger eigenvalue 3, eigenvector along (1, 1)/√2.
        assert!((e[0].abs() - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-12);
        assert!((e[0] - e[1]).abs() < 1e-12);
    }

    #[test]
    fn a_miss_beyond_ninety_degrees_is_a_flagged_row_not_an_error() {
        // Observation 120° from the prediction: behind the gnomonic tangent
        // plane. The row scores with flags and no d²; the separation is the
        // exact great-circle angle via the arc-projection fallback.
        let entries = vec![obs(100.0, 0.0, 59_060, 0.3)];
        let m = manifest(entries);
        let p = prediction(&m, 0, 220.0, 0.0, Some([[0.01, 0.0], [0.0, 0.01]]));
        let (scored, _) = score_all(&m, &[p], &[], &opts()).unwrap();
        let s = &scored[0];
        assert!(
            s.flags.iter().any(|f| f == "behind_tangent_plane"),
            "{:?}",
            s.flags
        );
        assert!(s.flags.iter().any(|f| f == "d2_invalid_large_separation"));
        assert!(s.d2_combined.is_none());
        let sep_deg = s.sep_arcsec / 3600.0;
        assert!((sep_deg - 120.0).abs() < 1e-6, "sep {sep_deg}°, want 120°");
    }

    #[test]
    fn obs_noise_dominated_rows_leave_realism_but_keep_accuracy() {
        // σ_obs = 1″ vs σ_pred = 0.1″: the row's d² measures the table, so
        // it is excluded from night statistics and histograms while its
        // separation still feeds accuracy.
        let entries = vec![obs(100.0, 20.0, 59_060, 1.0)];
        let m = manifest(entries);
        let p = prediction(&m, 0, 100.0, 20.0, Some([[0.01, 0.0], [0.0, 0.01]]));
        let (scored, sp) = score_all(&m, &[p], &[], &opts()).unwrap();
        assert!(scored[0].obs_noise_dominated);
        assert!(
            scored[0].d2_combined.is_some(),
            "d² still computed on the row"
        );
        let nights = night_stats(&m, &scored, &sp);
        assert!(nights.values().all(|v| v.is_empty()) || nights.is_empty());
        let agg = aggregate(&m, &scored, &[], &sp);
        assert!(agg.d2_histograms.iter().all(|h| h.n == 0) || agg.d2_histograms.is_empty());
        // Reverse dominance: σ_pred = 2″ ≫ σ_obs → included.
        let entries = vec![obs(100.0, 20.0, 59_060, 0.3)];
        let m2 = manifest(entries);
        let p2 = prediction(&m2, 0, 100.0, 20.0, Some([[4.0, 0.0], [0.0, 4.0]]));
        let (scored2, _) = score_all(&m2, &[p2], &[], &opts()).unwrap();
        assert!(!scored2[0].obs_noise_dominated);
    }
}
