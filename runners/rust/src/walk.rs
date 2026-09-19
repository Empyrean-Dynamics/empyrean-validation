//! Walk-forward covariance-realism runner (`validate walk`).
//!
//! Drives the window manifest through the wrapper channel: per (object,
//! config arm, window) — fit the manifest fit set, predict the held-out
//! targets with the sky-plane covariance, emit sidecar records. Runners
//! emit predictions, never statistics; scoring lives in the harness kernel
//! (`empyrean-validation score-predictions`).
//!
//! Parallelism is two-level by design: Rayon tasks are (object, arm,
//! **window shard**) — object-level alone would pin wall-clock to the
//! deepest object's serial warm-start chain. Within a shard the first fit
//! is cold and later fits warm-start from the previous window's solution
//! (state only — `determine` re-runs selection from a clean slate every
//! call, which is the design's fairness requirement).

use std::path::{Path, PathBuf};
use std::time::Instant;

use empyrean::{Context, EphemerisConfig, Epoch, ODConfig, Observations, Orbit, OutputEpoch};
use empyrean_validation::predict_schema::{
    ObjectWindows, PredictedObservation, WalkWindowRecord, WindowManifest, WindowSpec, profiles,
    seed_sources, uncertainty_forms,
};
use empyrean_validation::schema::{ValidationResult, orbit_sources, test_types};
use rayon::prelude::*;
use serde::Deserialize;

/// Windows per shard: cold-start the first fit of each shard, warm-start
/// within. Shards are the unit of intra-object parallelism.
pub const WINDOWS_PER_SHARD: usize = 100;

/// Sky-covariance block indices into `EphemerisEntry::covariance`, whose
/// row order is (ρ, RA, Dec, ρ̇, RȦ, Deċ). Named because a bare literal is
/// exactly how the range/RA covariance shipped under the RA/Dec name once
/// already.
pub const SKY_COV_ROW_RA: usize = 1;
pub const SKY_COV_ROW_DEC: usize = 2;

/// Nonlinear uncertainty-transport modes for the prediction step. The
/// ephemeris sky covariance is linear/STM regardless of the configured
/// engine method (recon gap G4), so nonlinear transport is done HERE: push
/// variants of the fitted state through the full propagate-and-project map
/// in one batched `generate_ephemeris` call and take the **second moment
/// about the nominal prediction** — the honest error covariance for the
/// point estimate a user would actually publish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UncertaintyMode {
    /// The engine's STM-projected 2×2 (the existing path).
    Linear,
    /// Analytic second-order: one Jet2 ephemeris pass on the nominal
    /// orbit; the wrapper exposes the composed observable Jacobian and
    /// Hessian per row, and the runner closes the Gaussian moments about
    /// the nominal (Isserlis):
    /// \( M_{ab} = J_a\Sigma J_b^\top +
    /// \tfrac14\,\mathrm{tr}(H_a\Sigma)\,\mathrm{tr}(H_b\Sigma) +
    /// \tfrac12\,\mathrm{tr}(H_a\Sigma H_b\Sigma) \)
    /// — the second moment about the published point. State block only
    /// (first six columns), matching the linear mode's truncation.
    SecondOrder,
    /// Cubature (spherical-radial) points: \( 2n = 12 \) variants at
    /// \( x_0 \pm \sqrt{n}\,L e_i \), equal weights (Arasaratnam &
    /// Haykin 2009). Deterministic, ~12× one prediction pass.
    SigmaPoint,
    /// Monte Carlo: [`MC_SAMPLES`] draws from the fitted covariance,
    /// deterministic per (object, window) seed.
    MonteCarlo,
}

impl UncertaintyMode {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "linear" | "first-order" => Ok(Self::Linear),
            "second-order" => Err(format!(
                "uncertainty mode {s:?} is implemented but HELD: the Jet2 sensitivity \
                 Jacobian fails the linear-term self-consistency check against the \
                 engine's own first-order sky covariance (ratios to 0.1x on long-horizon \
                 seed-window rows, which the Isserlis closure cannot produce — both \
                 quadratic terms are non-negative). Held until the second-order sky \
                 transport's sensitivity Jacobian reproduces the delivered covariance \
                 through J·Σ·Jᵀ within one call."
            )),
            "sigma-point" | "sigma-points" => Ok(Self::SigmaPoint),
            "monte-carlo" => Ok(Self::MonteCarlo),
            // AGM's sky-plane covariance is genuinely engine-gated: the
            // rc-line ephemeris path carries no mixture handling at all.
            // Named refusal, never a silent skip.
            "agm" => Err(format!(
                "uncertainty mode {s:?} is engine-gated: the released ephemeris path has \
                 no mixture-component sky covariance"
            )),
            other => Err(format!(
                "unknown uncertainty mode {other:?} (first-order | second-order | \
                 sigma-points | monte-carlo; agm is engine-gated)"
            )),
        }
    }
    /// Suffix appended to the arm name — each mode is its own series,
    /// visualized exactly like another tool.
    pub fn arm_suffix(self) -> Option<&'static str> {
        match self {
            Self::Linear => None,
            Self::SecondOrder => Some("so"),
            Self::SigmaPoint => Some("sp"),
            Self::MonteCarlo => Some("mc"),
        }
    }
}

/// Default Monte-Carlo draw count per window (the grid spec's
/// "Monte Carlo (1000)").
pub const DEFAULT_MC_SAMPLES: usize = 1000;

/// Lower-triangular Cholesky of a 6×6, or `None` when not positive
/// definite — the caller emits `uncertainty_form: none` for the mode
/// rather than repairing the matrix.
fn chol6(m: &[[f64; 6]; 6]) -> Option<[[f64; 6]; 6]> {
    let mut l = [[0.0f64; 6]; 6];
    for i in 0..6 {
        for j in 0..=i {
            let mut sum = m[i][j];
            for (lik, ljk) in l[i].iter().zip(l[j].iter()).take(j) {
                sum -= lik * ljk;
            }
            if i == j {
                if sum <= 0.0 || !sum.is_finite() {
                    return None;
                }
                l[i][j] = sum.sqrt();
            } else {
                l[i][j] = sum / l[j][j];
            }
        }
    }
    Some(l)
}

/// \( \mathrm{tr}(H\Sigma) \) over the 6×6 state block.
fn tr_hs(h: &[[f64; 6]; 6], sig: &[[f64; 6]; 6]) -> f64 {
    let mut t = 0.0;
    for i in 0..6 {
        for j in 0..6 {
            t += h[i][j] * sig[j][i];
        }
    }
    t
}

/// \( \mathrm{tr}(H_a\Sigma H_b\Sigma) \) over the 6×6 state block.
fn tr_hshs(ha: &[[f64; 6]; 6], sig: &[[f64; 6]; 6], hb: &[[f64; 6]; 6]) -> f64 {
    let mut asig = [[0.0f64; 6]; 6];
    let mut bsig = [[0.0f64; 6]; 6];
    for i in 0..6 {
        for j in 0..6 {
            for k in 0..6 {
                asig[i][j] += ha[i][k] * sig[k][j];
                bsig[i][j] += hb[i][k] * sig[k][j];
            }
        }
    }
    let mut t = 0.0;
    for i in 0..6 {
        for j in 0..6 {
            t += asig[i][j] * bsig[j][i];
        }
    }
    t
}

/// Second moment about the nominal prediction for two observable rows:
/// linear term plus the Gaussian (Isserlis) closure of the quadratic term.
fn second_order_moment(
    j_a: &[f64],
    j_b: &[f64],
    h_a: &[[f64; 6]; 6],
    h_b: &[[f64; 6]; 6],
    sig: &[[f64; 6]; 6],
) -> f64 {
    let mut lin = 0.0;
    for (i, ji) in j_a.iter().take(6).enumerate() {
        for (j, jj) in j_b.iter().take(6).enumerate() {
            lin += ji * sig[i][j] * jj;
        }
    }
    lin + 0.25 * tr_hs(h_a, sig) * tr_hs(h_b, sig) + 0.5 * tr_hshs(h_a, sig, h_b)
}

/// State offsets (in the fitted state's own basis) for a mode. Cubature:
/// \( \pm\sqrt{6} \) times each Cholesky column; Monte Carlo:
/// \( Lz \) with standard-normal \( z \) from a seeded LCG +
/// Box–Muller (deterministic — `Date`-free reruns reproduce bit-for-bit).
fn state_offsets(
    mode: UncertaintyMode,
    l: &[[f64; 6]; 6],
    seed: u64,
    mc_samples: usize,
) -> Vec<[f64; 6]> {
    match mode {
        UncertaintyMode::Linear | UncertaintyMode::SecondOrder => Vec::new(),
        UncertaintyMode::SigmaPoint => {
            let s = 6.0f64.sqrt();
            let mut out = Vec::with_capacity(12);
            for col in 0..6 {
                let mut d = [0.0; 6];
                for (dr, lrow) in d.iter_mut().zip(l.iter()) {
                    *dr = s * lrow[col];
                }
                out.push(d);
                out.push(d.map(|v| -v));
            }
            out
        }
        UncertaintyMode::MonteCarlo => {
            let mut state = seed.max(1);
            let mut spare: Option<f64> = None;
            let mut normal = move || {
                if let Some(z) = spare.take() {
                    return z;
                }
                let mut uniform = || {
                    state = state
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    ((state >> 11) as f64) / ((1u64 << 53) as f64)
                };
                let (u1, u2) = (uniform().max(1e-16), uniform());
                let r = (-2.0 * u1.ln()).sqrt();
                let (sn, cs) = (2.0 * std::f64::consts::PI * u2).sin_cos();
                spare = Some(r * sn);
                r * cs
            };
            (0..mc_samples)
                .map(|_| {
                    let z: [f64; 6] = std::array::from_fn(|_| normal());
                    let mut d = [0.0; 6];
                    for (row, dr) in d.iter_mut().enumerate() {
                        for (col, zc) in z.iter().enumerate() {
                            *dr += l[row][col] * zc;
                        }
                    }
                    d
                })
                .collect()
        }
    }
}

/// Gnomonic (east, north) offset of a variant prediction about the nominal
/// prediction, arcsec — the same tangent-plane convention the scoring
/// kernel uses, duplicated here because the kernel keeps its geometry
/// private and the runner must not depend on scoring internals.
fn gnomonic_offset_arcsec(
    nom_ra_deg: f64,
    nom_dec_deg: f64,
    ra_deg: f64,
    dec_deg: f64,
) -> Option<(f64, f64)> {
    const RAD2ARCSEC: f64 = 3600.0 * 180.0 / std::f64::consts::PI;
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

/// Equal-weight second moment about the origin (the nominal prediction) of
/// per-variant tangent-plane offsets. `None` when any variant fell behind
/// the tangent plane — a variant cloud that wraps the sky has no honest
/// 2×2, and the mode row says `none` rather than publishing a fiction.
fn second_moment_arcsec2(offsets: &[Option<(f64, f64)>]) -> Option<[[f64; 2]; 2]> {
    let mut m = [[0.0f64; 2]; 2];
    let n = offsets.len() as f64;
    for o in offsets {
        let (e, no) = (*o)?;
        m[0][0] += e * e / n;
        m[0][1] += e * no / n;
        m[1][1] += no * no / n;
    }
    m[1][0] = m[0][1];
    Some(m)
}

/// One named `ODConfig` variant (a config arm — design §2.7). The fields
/// are the fit-side axes of the config grid; the uncertainty axis rides
/// the prediction step (`--uncertainty-modes`), so a grid of
/// D×R×N fit arms × U modes shares fits across the uncertainty axis.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ArmSpec {
    pub name: String,
    /// Rejection axis: `"adaptive"` (default) | `"cmc2003"` | `"off"`.
    /// (`"cmc2003-recalibrated"` is engine-gated.)
    #[serde(default = "default_rejection")]
    pub rejection: String,
    /// Nightly axis: `"vfc2017"` (default, the production N^¼ layer) |
    /// `"off"`. (`"fullbatch"` — the √N night-counts-as-one convention —
    /// is engine-gated: the wrapper exposes no exponent knob.)
    #[serde(default = "default_nightly")]
    pub nightly: String,
    /// Fit-side catalog-debias axis: `"efcc"` (default) | `"off"`.
    /// The pinned SCORING debias never varies with this axis.
    #[serde(default = "default_debias")]
    pub debias: String,
    /// `"auto"` (engine default) or `"state"` (6-parameter control).
    #[serde(default = "default_solve_for")]
    pub solve_for: String,
}

fn default_rejection() -> String {
    "adaptive".into()
}
fn default_nightly() -> String {
    "vfc2017".into()
}
fn default_debias() -> String {
    "efcc".into()
}
fn default_solve_for() -> String {
    "auto".into()
}

impl ArmSpec {
    /// Is this the canonical default fit config (every axis at its
    /// production value)? Thin gate-visible rows emit for this cell.
    pub fn is_default_config(&self) -> bool {
        self.rejection == "adaptive"
            && self.nightly == "vfc2017"
            && self.debias == "efcc"
            && self.solve_for == "auto"
    }

    /// Validate axis values, refusing engine-gated ones by name.
    pub fn validate(&self) -> Result<(), String> {
        match self.rejection.as_str() {
            "adaptive" | "cmc2003" | "off" => {}
            "cmc2003-recalibrated" => {
                return Err(format!(
                    "arm {:?}: rejection \"cmc2003-recalibrated\" is engine-gated                      (VFC17 recalibration) — it joins the grid                      with the recalibrated release",
                    self.name
                ));
            }
            other => return Err(format!("arm {:?}: unknown rejection {other:?}", self.name)),
        }
        match self.nightly.as_str() {
            "vfc2017" | "off" => {}
            "fullbatch" => {
                return Err(format!(
                    "arm {:?}: nightly \"fullbatch\" is engine-gated — the wrapper's                      NightlyDeweighting exposes no exponent/convention knob                      (an open observation-weighting question)",
                    self.name
                ));
            }
            other => return Err(format!("arm {:?}: unknown nightly {other:?}", self.name)),
        }
        match self.debias.as_str() {
            "efcc" | "off" => {}
            other => return Err(format!("arm {:?}: unknown debias {other:?}", self.name)),
        }
        Ok(())
    }
}

/// Expand the fit-side config grid: every runnable combination of the
/// debias × rejection × nightly axes, canonically named
/// `{debias}.{rejection}.{nightly}` with short codes
/// (efcc|nodeb) · (adap|cmc|norej) · (vfc|nonight).
pub fn grid_arms() -> Vec<ArmSpec> {
    let mut arms = Vec::new();
    for (dcode, debias) in [("efcc", "efcc"), ("nodeb", "off")] {
        for (rcode, rejection) in [("adap", "adaptive"), ("cmc", "cmc2003"), ("norej", "off")] {
            for (ncode, nightly) in [("vfc", "vfc2017"), ("nonight", "off")] {
                arms.push(ArmSpec {
                    name: format!("{dcode}.{rcode}.{ncode}"),
                    rejection: rejection.into(),
                    nightly: nightly.into(),
                    debias: debias.into(),
                    solve_for: "auto".into(),
                });
            }
        }
    }
    arms
}

/// The built-in config set: the primary arm plus the two standing controls
/// (rejection-off pins the post-selection factor; nightly-off separates the
/// de-weighting convention's σ-inflation).
pub fn builtin_arms() -> Vec<ArmSpec> {
    vec![
        ArmSpec {
            name: "default".into(),
            rejection: "adaptive".into(),
            nightly: "vfc2017".into(),
            debias: "efcc".into(),
            solve_for: "auto".into(),
        },
        ArmSpec {
            name: "no-rejection".into(),
            rejection: "off".into(),
            nightly: "vfc2017".into(),
            debias: "efcc".into(),
            solve_for: "auto".into(),
        },
        ArmSpec {
            name: "no-nightly".into(),
            rejection: "adaptive".into(),
            nightly: "off".into(),
            debias: "efcc".into(),
            solve_for: "auto".into(),
        },
    ]
}

/// Windows an arm runs under a selected profile: the primary arm runs the
/// profile as selected; every other arm runs its intersection with `ci`
/// (controls ride the ci subset — design §2.7).
pub fn arm_profile<'a>(arm_name: &str, selected: &'a str) -> Vec<&'a str> {
    if arm_name == "default" {
        vec![selected]
    } else if selected == profiles::LADDER {
        // ladder ⊆ ci, so the intersection is ladder itself.
        vec![selected]
    } else {
        vec![profiles::CI, selected]
    }
}

fn window_in(w: &WindowSpec, wanted: &[&str]) -> bool {
    wanted.iter().all(|p| w.profiles.iter().any(|q| q == p))
}

/// Split window indices into warm-start shards.
pub fn shards(n_windows: usize) -> Vec<std::ops::Range<usize>> {
    (0..n_windows)
        .step_by(WINDOWS_PER_SHARD)
        .map(|s| s..(s + WINDOWS_PER_SHARD).min(n_windows))
        .collect()
}

/// The perturbers an object's fit must exclude: a Self-Perturber (an
/// SB441-N16 body in the catalog) must never be pulled by its own
/// ephemeris. The same rule the OD channel applies
/// (`plan::self_perturber_naif_ids`); resolved once per object, loudly —
/// a manifest object absent from the catalog is a protocol violation.
pub fn excluded_perturbers_for(object: &str) -> Result<Vec<empyrean::Origin>, String> {
    let obj = empyrean_validation::catalog::all_objects()
        .into_iter()
        .find(|o| o.name == object)
        .ok_or_else(|| format!("{object}: manifest object is not in the validation catalog"))?;
    crate::runner::naif_to_origins(&empyrean_validation::plan::self_perturber_naif_ids(obj))
}

fn arm_config(
    arm: &ArmSpec,
    tier: empyrean::ForceModelTier,
    cut_mjd_tdb: f64,
    excluded_perturbers: &[empyrean::Origin],
) -> ODConfig {
    let mut cfg = ODConfig {
        force_model: tier,
        output_epoch: OutputEpoch::Epoch(cut_mjd_tdb),
        num_threads: 1,
        excluded_perturbers: excluded_perturbers.to_vec(),
        ..ODConfig::default()
    };
    match arm.rejection.as_str() {
        "adaptive" => {}
        "cmc2003" => cfg.rejection.kind = empyrean::RejectionKind::CMC2003,
        "off" => cfg.rejection.enabled = false,
        other => unreachable!("unvalidated rejection {other:?}"),
    }
    if arm.nightly == "off" {
        // Assigning the field REPLACES the default [NightlyDeweighting]
        // list — an empty list is exactly the nightly-off arm.
        cfg.weighting.additional_layers = vec![];
    }
    if arm.debias == "off" {
        cfg.debiasing.enabled = false;
    }
    if arm.solve_for == "state" {
        cfg.solve_for = empyrean::SolveForParams::StateOnly;
    }
    cfg
}

/// Per-window products, accumulated per shard and flattened at the end.
struct ShardOutput {
    windows: Vec<WalkWindowRecord>,
    predictions: Vec<PredictedObservation>,
    thin_rows: Vec<ValidationResult>,
}

pub struct WalkArgsResolved<'a> {
    pub manifest: &'a WindowManifest,
    pub profile: String,
    pub arms: Vec<ArmSpec>,
    pub only: Vec<String>,
    pub fixtures_dir: PathBuf,
    pub tier: empyrean::ForceModelTier,
    /// Uncertainty-transport modes for the prediction step. `Linear` is
    /// the base series; each additional mode multiplies the prediction
    /// series (`<arm>+sp`, `<arm>+mc`) without refitting.
    pub modes: Vec<UncertaintyMode>,
    /// Monte-Carlo draws per window for the `monte-carlo` mode.
    pub mc_samples: usize,
}

/// Streaming sidecar sink: each shard task's window records and
/// predictions are appended the moment the shard completes, so a running
/// walk is scoreable mid-flight (`score-predictions` tolerates one torn
/// final line). One formatted `write_all` per shard under the lock —
/// lines from concurrent shards never interleave.
pub struct StreamSink {
    windows: std::sync::Mutex<std::fs::File>,
    predictions: std::sync::Mutex<std::fs::File>,
}

impl StreamSink {
    pub fn create(win_path: &Path, pred_path: &Path) -> Result<Self, String> {
        let mk =
            |p: &Path| std::fs::File::create(p).map_err(|e| format!("create {}: {e}", p.display()));
        Ok(Self {
            windows: std::sync::Mutex::new(mk(win_path)?),
            predictions: std::sync::Mutex::new(mk(pred_path)?),
        })
    }

    fn append<T: serde::Serialize>(
        file: &std::sync::Mutex<std::fs::File>,
        rows: &[T],
    ) -> Result<(), String> {
        use std::io::Write;
        if rows.is_empty() {
            return Ok(());
        }
        let mut buf = String::new();
        for r in rows {
            buf.push_str(
                &serde_json::to_string(r).map_err(|e| format!("serialize sidecar row: {e}"))?,
            );
            buf.push('\n');
        }
        let mut f = file
            .lock()
            .map_err(|_| "sidecar sink lock poisoned".to_string())?;
        f.write_all(buf.as_bytes())
            .map_err(|e| format!("append sidecar: {e}"))
    }

    fn write_shard(&self, s: &ShardOutput) -> Result<(), String> {
        Self::append(&self.windows, &s.windows)?;
        Self::append(&self.predictions, &s.predictions)
    }
}

/// Sidecar row tallies for the end-of-run summary (the rows themselves
/// stream through the [`StreamSink`] and are never buffered whole).
#[derive(Default)]
pub struct WalkCounts {
    pub windows: usize,
    pub predictions: usize,
    pub predictions_with_cov: usize,
}

/// Everything one walk invocation returns in memory.
pub type WalkOutput = (Vec<ValidationResult>, WalkCounts);

/// Run the walk; returns (thin rows, sidecar counts). Sidecar records
/// stream to `sink` as shards complete.
pub fn run_walk(
    ctx: &Context,
    args: &WalkArgsResolved<'_>,
    sink: &StreamSink,
) -> Result<WalkOutput, String> {
    for arm in &args.arms {
        arm.validate()?;
    }
    let selected: Vec<&ObjectWindows> = args
        .manifest
        .objects
        .iter()
        .filter(|o| o.eligible)
        .filter(|o| args.only.is_empty() || args.only.iter().any(|n| n == &o.object))
        .collect();
    if selected.is_empty() {
        return Err("no eligible objects match the selection".into());
    }

    // (object, arm, shard) task list.
    struct Task<'a> {
        obj: &'a ObjectWindows,
        arm: &'a ArmSpec,
        window_idx: Vec<usize>,
        shard: std::ops::Range<usize>,
    }
    let mut tasks: Vec<Task> = Vec::new();
    for obj in &selected {
        for arm in &args.arms {
            let wanted = arm_profile(&arm.name, &args.profile);
            let idx: Vec<usize> = obj
                .windows
                .iter()
                .enumerate()
                .filter(|(_, w)| window_in(w, &wanted))
                .map(|(i, _)| i)
                .collect();
            for shard in shards(idx.len()) {
                tasks.push(Task {
                    obj,
                    arm,
                    window_idx: idx.clone(),
                    shard,
                });
            }
        }
    }
    eprintln!(
        "walk: {} objects × {} arms → {} shard tasks (profile {})",
        selected.len(),
        args.arms.len(),
        tasks.len(),
        args.profile,
    );

    let engine_version = empyrean::version_string().ok();
    // Parse + filter each object's fixture once, up front (serial — cheap
    // relative to fits), so shards share the filtered observation vector.
    let mut per_object: std::collections::HashMap<&str, Vec<empyrean::Observation>> =
        Default::default();
    let mut excluded: std::collections::HashMap<&str, Vec<empyrean::Origin>> = Default::default();
    for obj in &selected {
        per_object.insert(
            obj.object.as_str(),
            load_fit_observations(ctx, obj, &args.fixtures_dir)?,
        );
        let ex = excluded_perturbers_for(&obj.object)?;
        if !ex.is_empty() {
            eprintln!(
                "walk: {} excludes its own perturbation ({ex:?})",
                obj.object
            );
        }
        excluded.insert(obj.object.as_str(), ex);
    }

    // Stream each shard's sidecar rows the moment it completes; only thin
    // rows and tallies ride back through the collect.
    type Slim = (Vec<ValidationResult>, usize, usize, usize);
    let outputs: Vec<Result<Slim, String>> = tasks
        .par_iter()
        .map(|t| {
            let s = run_shard(
                ctx,
                t.obj,
                t.arm,
                &t.window_idx[t.shard.clone()],
                &per_object[t.obj.object.as_str()],
                &excluded[t.obj.object.as_str()],
                args.tier,
                &args.profile,
                &engine_version,
                &args.modes,
                args.mc_samples,
            )?;
            sink.write_shard(&s)?;
            let with_cov = s
                .predictions
                .iter()
                .filter(|p| p.uncertainty_form == uncertainty_forms::RADEC_2X2)
                .count();
            Ok((s.thin_rows, s.windows.len(), s.predictions.len(), with_cov))
        })
        .collect();

    let mut thin_rows = Vec::new();
    let mut counts = WalkCounts::default();
    let mut errors = Vec::new();
    for o in outputs {
        match o {
            Ok((rows, n_win, n_pred, n_cov)) => {
                thin_rows.extend(rows);
                counts.windows += n_win;
                counts.predictions += n_pred;
                counts.predictions_with_cov += n_cov;
            }
            Err(e) => errors.push(e),
        }
    }
    if !errors.is_empty() {
        return Err(format!(
            "{} shard tasks failed; first: {}",
            errors.len(),
            errors[0]
        ));
    }
    Ok((thin_rows, counts))
}

/// Read the fixture and keep exactly the manifest's non-excluded rows,
/// matched by (station, obsTime). Fit-set identity across tools is a
/// protocol invariant — a count mismatch is an error naming the object,
/// never a quiet divergence.
fn load_fit_observations(
    ctx: &Context,
    obj: &ObjectWindows,
    fixtures_dir: &Path,
) -> Result<Vec<empyrean::Observation>, String> {
    let candidates = [
        fixtures_dir.join(format!("{}.psv", obj.object)),
        fixtures_dir.join(format!("{}.psv", obj.object.replace('/', "_"))),
        fixtures_dir.join(format!("{}.psv", obj.mpc_designation)),
    ];
    let path = candidates
        .iter()
        .find(|p| p.exists())
        .ok_or_else(|| format!("{}: no PSV fixture found", obj.object))?;
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let parsed = ctx
        .read_ades(&text)
        .map_err(|e| format!("{}: read_ades: {e}", obj.object))?;
    let keep: std::collections::HashSet<(&str, &str)> = obj
        .observations
        .iter()
        .filter(|o| o.excluded.is_none())
        .map(|o| (o.stn.as_str(), o.obs_time.as_str()))
        .collect();
    let kept: Vec<empyrean::Observation> = parsed
        .iter()
        .filter(|o| keep.contains(&(o.obs_code.as_str(), o.obs_time.as_str())))
        .collect();
    let expected = keep.len();
    // (stn, obsTime) pairs can legitimately repeat (duplicate MPC rows), so
    // compare against the manifest's non-excluded row count, not the set.
    let n_manifest = obj
        .observations
        .iter()
        .filter(|o| o.excluded.is_none())
        .count();
    if kept.len() != n_manifest {
        return Err(format!(
            "{}: fit-set mismatch — manifest lists {n_manifest} non-excluded rows \
             ({expected} distinct (stn, obsTime) pairs), fixture matched {}",
            obj.object,
            kept.len()
        ));
    }
    Ok(kept)
}

#[allow(clippy::too_many_arguments)]
fn run_shard(
    ctx: &Context,
    obj: &ObjectWindows,
    arm: &ArmSpec,
    shard_windows: &[usize],
    fit_pool: &[empyrean::Observation],
    excluded_perturbers: &[empyrean::Origin],
    tier: empyrean::ForceModelTier,
    selected_profile: &str,
    engine_version: &Option<String>,
    modes: &[UncertaintyMode],
    mc_samples: usize,
) -> Result<ShardOutput, String> {
    let mut out = ShardOutput {
        windows: Vec::new(),
        predictions: Vec::new(),
        thin_rows: Vec::new(),
    };
    let mut warm: Option<Orbit> = None;
    let eph_cfg = ephemeris_config(tier, excluded_perturbers);

    for &wi in shard_windows {
        let w = &obj.windows[wi];
        let cfg = arm_config(arm, tier, w.cut_mjd_tdb, excluded_perturbers);

        // The manifest cut instant (TDB) is the slicing authority — never
        // the integer night (TDB−UTC ≈ 69 s at the boundary).
        let cut = Epoch::from_mjd_tdb(w.cut_mjd_tdb);
        let window_obs: Vec<empyrean::Observation> = {
            let all = Observations::from_array(fit_pool)
                .map_err(|e| format!("{}: from_array: {e}", obj.object))?;
            let filtered = all
                .filter_by_epoch(None, Some(cut))
                .map_err(|e| format!("{}: filter_by_epoch: {e}", obj.object))?;
            filtered.iter().collect()
        };
        if window_obs.len() != w.n_obs_fit as usize {
            return Err(format!(
                "{} w{}: fit window has {} rows, manifest says {} — the cut slicing \
                 diverged from the generator",
                obj.object,
                w.index,
                window_obs.len(),
                w.n_obs_fit
            ));
        }
        let observations = Observations::from_array(&window_obs)
            .map_err(|e| format!("{}: from_array(window): {e}", obj.object))?;

        let seeds: Option<Vec<Orbit>> = warm.clone().map(|o| vec![o]);
        let t0 = Instant::now();
        let fit = ctx
            .determine(&observations, seeds.as_deref(), &cfg)
            .and_then(|batch| batch.into_single());
        let fit_ms = t0.elapsed().as_secs_f64() * 1000.0;

        let (record, orbit) = match fit {
            Ok(r) => {
                // The kernel's trust gate compares against the lowercase
                // "trusted" tag; any other value flags the row. Collapse the
                // trusted variant to that tag and keep the full Debug detail
                // for the flagged ones.
                let trust = r.covariance_trust.as_ref().map(|t| match t {
                    empyrean::CovarianceTrust::Trusted => "trusted".to_string(),
                    other => format!("{other:?}"),
                });
                // Capture the fitted orbit + 6×6 in the standard three-view
                // form (the cross-tool comparison and the future
                // common-propagation arm consume these). A capture failure
                // is loud — it means the covariance transform broke.
                let native = crate::runner::propagated_state_to_coord(&r.state());
                let (captured, capture_failure) = match crate::runner::capture_orbit(
                    ctx,
                    &obj.object,
                    orbit_sources::EMPYREAN_OD,
                    engine_version.clone(),
                    &native,
                ) {
                    Ok(c) => (Some(c), None),
                    Err(e) => (None, Some(format!("capture: {e}"))),
                };
                let n_solve_for = if r.covariance_9x9.is_some() { 9 } else { 6 }
                    + u32::from(r.dt_delta.is_some());
                let rec = WalkWindowRecord {
                    object: obj.object.clone(),
                    tool: "rust".into(),
                    config_arm: arm.name.clone(),
                    window_index: w.index,
                    converged: r.converged,
                    failure: (!r.converged).then(|| "did_not_converge".into()),
                    n_obs_used: Some(r.summary.num_selected as u32),
                    n_obs_rejected: Some(r.summary.num_rejected as u32),
                    n_solve_for: Some(n_solve_for),
                    iterations: Some(r.iterations),
                    chi2: Some(r.summary.chi2),
                    reduced_chi2: Some(r.summary.reduced_chi2),
                    fit_epoch_mjd_tdb: Some(w.cut_mjd_tdb),
                    warm_start: warm.is_some(),
                    seed_source: if warm.is_some() {
                        seed_sources::WARM_PREVIOUS.into()
                    } else {
                        seed_sources::OWN_IOD.into()
                    },
                    covariance_trust: trust,
                    fit_time_ms: fit_ms,
                    predict_time_ms: None,
                    captured,
                };
                let mut rec = rec;
                if let Some(f) = capture_failure {
                    rec.failure = Some(f);
                }
                (rec, Some(r.orbit))
            }
            Err(e) => {
                let rec = WalkWindowRecord {
                    object: obj.object.clone(),
                    tool: "rust".into(),
                    config_arm: arm.name.clone(),
                    window_index: w.index,
                    converged: false,
                    failure: Some(format!("determine: {e}")),
                    n_obs_used: None,
                    n_obs_rejected: None,
                    n_solve_for: None,
                    iterations: None,
                    chi2: None,
                    reduced_chi2: None,
                    fit_epoch_mjd_tdb: Some(w.cut_mjd_tdb),
                    warm_start: warm.is_some(),
                    seed_source: if warm.is_some() {
                        seed_sources::WARM_PREVIOUS.into()
                    } else {
                        seed_sources::OWN_IOD.into()
                    },
                    covariance_trust: None,
                    fit_time_ms: fit_ms,
                    predict_time_ms: None,
                    captured: None,
                };
                (rec, None)
            }
        };

        let mut record = record;
        if let Some(orbit) = &orbit {
            let tp = Instant::now();
            match predict_window(ctx, obj, w, orbit, arm, modes, mc_samples, &eph_cfg) {
                Ok(preds) => {
                    record.predict_time_ms = Some(tp.elapsed().as_secs_f64() * 1000.0);
                    out.predictions.extend(preds);
                }
                Err(e) => {
                    // A fit whose predictions failed is a window-level
                    // failure: keep the fit facts, name the cause.
                    record.failure = Some(format!("predict: {e}"));
                }
            }
        }

        // Thin rows: ci-profile windows of the primary arm only (report
        // page budget; everything else lives in the sidecars).
        if arm.is_default_config()
            && w.profiles.iter().any(|p| p == profiles::CI)
            && window_in(w, &[selected_profile])
        {
            out.thin_rows.push(thin_row(obj, w, &record));
        }
        // Every non-linear mode is its own series: duplicate the window
        // record under the suffixed arm so per-series attempted/failed
        // counts (and delivered fractions) stay honest.
        for mode in modes {
            if let Some(suffix) = mode.arm_suffix() {
                let mut clone = record.clone();
                clone.config_arm = format!("{}+{}", arm.name, suffix);
                out.windows.push(clone);
            }
        }
        // Warm-start carries the state only, and only from a converged fit.
        warm = if record.converged { orbit } else { None };
        out.windows.push(record);
    }
    eprintln!(
        "  {} [{}] windows {}..{}: {} fit, {} failed",
        obj.object,
        arm.name,
        shard_windows.first().copied().unwrap_or(0),
        shard_windows.last().copied().unwrap_or(0),
        out.windows.iter().filter(|r| r.converged).count(),
        out.windows.iter().filter(|r| !r.converged).count(),
    );
    Ok(out)
}

/// The ephemeris configuration a window's predictions run under: the
/// fit's force-model tier and the object's own-perturbation exclusion —
/// a Self-Perturber predicted with itself in the force model fails in the
/// light-time iteration (and would be wrong if it did not).
fn ephemeris_config(
    tier: empyrean::ForceModelTier,
    excluded_perturbers: &[empyrean::Origin],
) -> EphemerisConfig {
    EphemerisConfig {
        propagation: empyrean::PropagationConfig {
            force_model: tier,
            excluded_perturbers: excluded_perturbers.to_vec(),
            ..empyrean::PropagationConfig::default()
        },
        ..EphemerisConfig::default()
    }
}

#[allow(clippy::too_many_arguments)]
fn predict_window(
    ctx: &Context,
    obj: &ObjectWindows,
    w: &WindowSpec,
    orbit: &Orbit,
    arm: &ArmSpec,
    modes: &[UncertaintyMode],
    mc_samples: usize,
    eph_cfg: &EphemerisConfig,
) -> Result<Vec<PredictedObservation>, String> {
    let mut targets: Vec<(u32, bool)> = w.targets.iter().map(|t| (t.obs_idx, false)).collect();
    targets.extend(w.in_sample.iter().map(|&i| (i, true)));

    // One Observer per target at the observation's reception time. The
    // wrapper's get_observers is a codes × epochs cross product, so build
    // them one pair at a time and concatenate.
    let mut observers = Vec::with_capacity(targets.len());
    for &(obs_idx, _) in &targets {
        let e = &obj.observations[obs_idx as usize];
        let epoch = Epoch::from_iso_utc(&e.obs_time)
            .map_err(|err| format!("epoch {}: {err}", e.obs_time))?;
        let mut v = ctx
            .get_observers(
                &[e.stn.as_str()],
                &[epoch],
                empyrean::Frame::ICRF,
                empyrean::Origin::SSB,
            )
            .map_err(|err| format!("observer {} @ {}: {err}", e.stn, e.obs_time))?;
        if v.len() != 1 {
            return Err(format!(
                "observer {} @ {}: expected 1 observer, got {}",
                e.stn,
                e.obs_time,
                v.len()
            ));
        }
        observers.push(v.pop().unwrap());
    }

    let eph = ctx
        .generate_ephemeris(std::slice::from_ref(orbit), &observers, eph_cfg)
        .map_err(|e| format!("generate_ephemeris: {e}"))?;
    if eph.entries.len() != targets.len() {
        return Err(format!(
            "generate_ephemeris returned {} entries for {} observers",
            eph.entries.len(),
            targets.len()
        ));
    }

    let mut out = Vec::with_capacity(targets.len());
    for ((obs_idx, in_sample), entry) in targets.into_iter().zip(eph.entries.iter()) {
        let e = &obj.observations[obs_idx as usize];
        let cov = entry.covariance.map(|c| {
            [
                [
                    c[SKY_COV_ROW_RA][SKY_COV_ROW_RA],
                    c[SKY_COV_ROW_RA][SKY_COV_ROW_DEC],
                ],
                [
                    c[SKY_COV_ROW_DEC][SKY_COV_ROW_RA],
                    c[SKY_COV_ROW_DEC][SKY_COV_ROW_DEC],
                ],
            ]
        });
        out.push(PredictedObservation {
            object: obj.object.clone(),
            tool: "rust".into(),
            config_arm: arm.name.clone(),
            window_index: w.index,
            obs_idx,
            key_hash: e.key_hash,
            in_sample,
            ra_deg: entry.ra_deg,
            dec_deg: entry.dec_deg,
            epoch_scale_used: "utc".into(),
            uncertainty_form: if cov.is_some() {
                uncertainty_forms::RADEC_2X2.into()
            } else {
                uncertainty_forms::NONE.into()
            },
            cov_radec: cov,
            cov_units: cov.map(|_| "deg2".into()),
            cov_basis: cov.map(|_| "native_ra".into()),
            sigma1_arcsec: None,
            sigma1_pa_deg: None,
            pa_motion_deg: entry
                .position_angle_deg
                .is_finite()
                .then_some(entry.position_angle_deg),
            sky_rate_deg_day: entry
                .sky_rate_deg_day
                .is_finite()
                .then_some(entry.sky_rate_deg_day),
        });
    }

    // ── nonlinear uncertainty-transport series (design: each mode is a
    // first-class series, visualized like another tool) ──────────────────
    for &mode in modes {
        let Some(suffix) = mode.arm_suffix() else {
            continue;
        };
        let arm_name = format!("{}+{}", arm.name, suffix);
        if mode == UncertaintyMode::SecondOrder {
            out.extend(predict_second_order(
                ctx, orbit, &observers, &out, arm, &arm_name, eph_cfg,
            )?);
            continue;
        }
        // Sampling happens in the fitted state's own basis; only the
        // Cartesian representation is sampled in v1 (angle elements wrap).
        // An unsampleable window emits mode rows with `none` — delivered
        // and countable, never silently absent.
        let sampleable = matches!(
            orbit.state.representation,
            empyrean::Representation::Cartesian
        );
        let chol = orbit
            .state
            .covariance
            .filter(|_| sampleable)
            .and_then(|c| chol6(&c));
        let Some(l) = chol else {
            for base in out.clone() {
                out.push(PredictedObservation {
                    config_arm: arm_name.clone(),
                    uncertainty_form: uncertainty_forms::NONE.into(),
                    cov_radec: None,
                    cov_units: None,
                    cov_basis: None,
                    ..base
                });
            }
            continue;
        };
        let seed = {
            // FNV over the object name + window index: deterministic,
            // distinct per (object, window).
            let mut h: u64 = 0xcbf29ce484222325;
            for b in obj.object.as_bytes() {
                h ^= u64::from(*b);
                h = h.wrapping_mul(0x100000001b3);
            }
            h ^ u64::from(w.index)
        };
        let offsets = state_offsets(mode, &l, seed, mc_samples);
        let variants: Vec<Orbit> = offsets
            .iter()
            .map(|d| {
                let mut v = orbit.clone();
                v.state.covariance = None;
                v.state.non_grav_cross = None;
                v.ng_covariance = None;
                v.wide_cross = None;
                for (e, di) in v.state.elements.iter_mut().zip(d.iter()) {
                    *e += di;
                }
                v
            })
            .collect();
        let eph = ctx
            .generate_ephemeris(&variants, &observers, eph_cfg)
            .map_err(|e| format!("generate_ephemeris ({arm_name} variants): {e}"))?;
        let n_targets = observers.len();
        if eph.entries.len() != variants.len() * n_targets {
            return Err(format!(
                "{arm_name}: {} variant entries for {} variants × {} observers",
                eph.entries.len(),
                variants.len(),
                n_targets
            ));
        }
        let nominal: Vec<PredictedObservation> = out
            .iter()
            .filter(|p| p.config_arm == arm.name)
            .cloned()
            .collect();
        for (m, base) in nominal.into_iter().enumerate() {
            let offsets_m: Vec<Option<(f64, f64)>> = (0..variants.len())
                .map(|k| {
                    let e = &eph.entries[k * n_targets + m];
                    gnomonic_offset_arcsec(base.ra_deg, base.dec_deg, e.ra_deg, e.dec_deg)
                })
                .collect();
            let cov = second_moment_arcsec2(&offsets_m);
            out.push(PredictedObservation {
                config_arm: arm_name.clone(),
                uncertainty_form: if cov.is_some() {
                    uncertainty_forms::RADEC_2X2.into()
                } else {
                    uncertainty_forms::NONE.into()
                },
                cov_radec: cov,
                cov_units: cov.map(|_| "arcsec2".into()),
                cov_basis: cov.map(|_| "great_circle".into()),
                ..base
            });
        }
    }
    Ok(out)
}

/// Analytic second-order series: one Jet2 pass on the nominal orbit; the
/// composed observable Jacobian + Hessian close the Gaussian moments about
/// the published point. Rows the pass cannot cover (no Hessian delivered)
/// emit `uncertainty_form: none` — delivered and countable.
fn predict_second_order(
    ctx: &Context,
    orbit: &Orbit,
    observers: &[empyrean::Observer],
    nominal_rows: &[PredictedObservation],
    arm: &ArmSpec,
    arm_name: &str,
    eph_cfg: &EphemerisConfig,
) -> Result<Vec<PredictedObservation>, String> {
    let mut cfg = eph_cfg.clone();
    cfg.propagation.uncertainty_method = empyrean::UncertaintyMethod::SecondOrder;
    let eph = ctx
        .generate_ephemeris(std::slice::from_ref(orbit), observers, &cfg)
        .map_err(|e| format!("generate_ephemeris ({arm_name} Jet2): {e}"))?;
    let sig = orbit.state.covariance.ok_or_else(|| {
        format!("{arm_name}: fitted orbit carries no covariance for the second-order pass")
    })?;
    let nominal: Vec<&PredictedObservation> = nominal_rows
        .iter()
        .filter(|p| p.config_arm == arm.name)
        .collect();
    let mut out = Vec::with_capacity(nominal.len());
    for (m, base) in nominal.into_iter().enumerate() {
        let cov = eph.sensitivity.get(m).and_then(|sens| {
            let n = sens.n_params as usize;
            if n < 6 || sens.jacobian.len() < 6 * n || sens.hessian.len() < 6 * n * n {
                return None;
            }
            let row = |r: usize| &sens.jacobian[r * n..r * n + 6];
            let block = |r: usize| -> [[f64; 6]; 6] {
                std::array::from_fn(|i| std::array::from_fn(|j| sens.hessian[(r * n + i) * n + j]))
            };
            let (ra, dec) = (empyrean::SENSITIVITY_ROW_RA, empyrean::SENSITIVITY_ROW_DEC);
            let (h_ra, h_dec) = (block(ra), block(dec));
            Some([
                [
                    second_order_moment(row(ra), row(ra), &h_ra, &h_ra, &sig),
                    second_order_moment(row(ra), row(dec), &h_ra, &h_dec, &sig),
                ],
                [
                    second_order_moment(row(dec), row(ra), &h_dec, &h_ra, &sig),
                    second_order_moment(row(dec), row(dec), &h_dec, &h_dec, &sig),
                ],
            ])
        });
        out.push(PredictedObservation {
            config_arm: arm_name.to_string(),
            uncertainty_form: if cov.is_some() {
                uncertainty_forms::RADEC_2X2.into()
            } else {
                uncertainty_forms::NONE.into()
            },
            cov_radec: cov,
            cov_units: cov.map(|_| "deg2".into()),
            cov_basis: cov.map(|_| "native_ra".into()),
            ..base.clone()
        });
    }
    Ok(out)
}

/// The gate-visible thin row for one (object, window) of the primary arm.
/// Failure rows keep finite epochs — the epoch fields have no NaN↔null
/// adapter and a NaN would make the JSON unreadable downstream.
fn thin_row(obj: &ObjectWindows, w: &WindowSpec, rec: &WalkWindowRecord) -> ValidationResult {
    let mut row = ValidationResult::empty();
    row.object = obj.object.clone();
    row.population = obj.population.clone();
    row.test_type = test_types::COVARIANCE_REALISM.to_string();
    row.channel = "rust".into();
    // Ordinal, not days — documented on the schema field; the real (arc,
    // dt) axes live in the sidecars.
    row.dt_days = f64::from(w.index);
    row.t_mjd_tdb = w.cut_mjd_tdb;
    row.epoch_mjd_tdb = w.cut_mjd_tdb;
    row.n_obs_used = rec.n_obs_used;
    row.od_converged = Some(rec.converged);
    row.od_iterations = rec.iterations;
    row.od_chi2 = rec.chi2;
    row.od_reduced_chi2 = rec.reduced_chi2;
    row.emp_time_ms = Some(rec.fit_time_ms);
    row.timestamp = chrono::Utc::now().to_rfc3339();
    if let Some(f) = &rec.failure {
        row.notes = f.clone();
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arm_spec_json_and_builtins() {
        let arms: Vec<ArmSpec> = serde_json::from_str(
            r#"[{"name":"x"},{"name":"y","rejection":"off","solve_for":"state"}]"#,
        )
        .unwrap();
        assert_eq!(
            (arms[0].rejection.as_str(), arms[0].nightly.as_str()),
            ("adaptive", "vfc2017")
        );
        assert_eq!(arms[1].rejection, "off");
        assert_eq!(arms[1].solve_for, "state");
        assert!(serde_json::from_str::<Vec<ArmSpec>>(r#"[{"name":"x","surprise":1}]"#).is_err());
        let b = builtin_arms();
        assert_eq!(b.len(), 3);
        assert_eq!(b[1].rejection, "off");
        assert_eq!(b[2].nightly, "off");
        // The runnable fit-side grid: 2 debias × 3 rejection × 2 nightly.
        let g = grid_arms();
        assert_eq!(g.len(), 12);
        assert!(g.iter().all(|a| a.validate().is_ok()));
        let names: std::collections::BTreeSet<&str> = g.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names.len(), 12);
        assert!(names.contains("efcc.adap.vfc") && names.contains("nodeb.norej.nonight"));
        // Gated values refuse by name.
        let gated = ArmSpec {
            name: "g".into(),
            rejection: "cmc2003-recalibrated".into(),
            nightly: "vfc2017".into(),
            debias: "efcc".into(),
            solve_for: "auto".into(),
        };
        assert!(gated.validate().unwrap_err().contains("engine-gated"));
        assert!(
            UncertaintyMode::parse("second-order")
                .unwrap_err()
                .contains("HELD")
        );
        assert_eq!(UncertaintyMode::SecondOrder.arm_suffix(), Some("so"));
        assert!(
            UncertaintyMode::parse("agm")
                .unwrap_err()
                .contains("engine-gated")
        );
    }

    #[test]
    fn shard_chunking() {
        assert_eq!(shards(0).len(), 0);
        assert_eq!(shards(1), vec![0..1]);
        assert_eq!(shards(100), vec![0..100]);
        let s = shards(250);
        assert_eq!(s, vec![0..100, 100..200, 200..250]);
    }

    #[test]
    fn control_arms_ride_ci() {
        assert_eq!(arm_profile("default", "full"), vec!["full"]);
        assert_eq!(arm_profile("no-rejection", "full"), vec!["ci", "full"]);
        assert_eq!(arm_profile("no-rejection", "ladder"), vec!["ladder"]);
    }

    #[test]
    fn stream_sink_appends_whole_lines_per_shard() {
        let dir = std::env::temp_dir().join(format!("covreal_sink_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (wp, pp) = (dir.join("w.jsonl"), dir.join("p.jsonl"));
        let sink = StreamSink::create(&wp, &pp).unwrap();
        // Two successive appends (as two shards would produce) must yield
        // one complete JSON document per line, in append order.
        let rows1 = vec![serde_json::json!({"shard": 1, "row": 0})];
        let rows2 = vec![
            serde_json::json!({"shard": 2, "row": 0}),
            serde_json::json!({"shard": 2, "row": 1}),
        ];
        StreamSink::append(&sink.predictions, &rows1).unwrap();
        StreamSink::append(&sink.predictions, &rows2).unwrap();
        let text = std::fs::read_to_string(&pp).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        for l in &lines {
            serde_json::from_str::<serde_json::Value>(l).unwrap();
        }
        assert!(text.ends_with('\n'));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sky_cov_indices_are_the_radec_block() {
        // Labeled synthetic 6×6: entry (i,j) = 10i + j. The extracted 2×2
        // must be the (RA, Dec) block — rows/cols {1, 2} — not (ρ, RA).
        let c: [[f64; 6]; 6] =
            std::array::from_fn(|i| std::array::from_fn(|j| (10 * i + j) as f64));
        let block = [
            [
                c[SKY_COV_ROW_RA][SKY_COV_ROW_RA],
                c[SKY_COV_ROW_RA][SKY_COV_ROW_DEC],
            ],
            [
                c[SKY_COV_ROW_DEC][SKY_COV_ROW_RA],
                c[SKY_COV_ROW_DEC][SKY_COV_ROW_DEC],
            ],
        ];
        assert_eq!(block, [[11.0, 12.0], [21.0, 22.0]]);
    }

    #[test]
    fn thin_row_failure_keeps_finite_epochs() {
        use empyrean_validation::predict_schema::{ObsEntry, TargetSpec};
        let w = WindowSpec {
            index: 7,
            cut_night: 60_000,
            cut_utc: "2023-02-25T00:00:00.000Z".into(),
            cut_mjd_tt: 60_001.000_8,
            cut_mjd_tdb: 60_001.000_8,
            intra_night: false,
            profiles: vec!["full".into(), "ci".into()],
            n_obs_fit: 12,
            last_fit_obs_idx: 11,
            arc_days: 14.0,
            targets: vec![TargetSpec {
                obs_idx: 12,
                rung: "next".into(),
                horizon_days: 1.0,
            }],
            in_sample: vec![0],
            seed: None,
        };
        let obj = ObjectWindows {
            object: "T".into(),
            mpc_designation: "T".into(),
            population: "NEO".into(),
            class: "NEA".into(),
            eligible: true,
            ineligible_reason: None,
            first_night: 59_990,
            apparition_end_night: 60_010,
            n_obs_total: 13,
            n_excluded_space_based: 0,
            observations: Vec::<ObsEntry>::new(),
            windows: vec![],
        };
        let rec = WalkWindowRecord {
            object: "T".into(),
            tool: "rust".into(),
            config_arm: "default".into(),
            window_index: 7,
            converged: false,
            failure: Some("determine: IOD failed".into()),
            n_obs_used: None,
            n_obs_rejected: None,
            n_solve_for: None,
            iterations: None,
            chi2: None,
            reduced_chi2: None,
            fit_epoch_mjd_tdb: Some(w.cut_mjd_tdb),
            warm_start: false,
            seed_source: "own_iod".into(),
            covariance_trust: None,
            fit_time_ms: 3.0,
            predict_time_ms: None,
            captured: None,
        };
        let row = thin_row(&obj, &w, &rec);
        assert_eq!(row.test_type, "covariance_realism");
        assert_eq!(row.dt_days, 7.0);
        assert!(row.t_mjd_tdb.is_finite() && row.epoch_mjd_tdb.is_finite());
        assert_eq!(row.od_converged, Some(false));
        assert!(row.notes.contains("IOD failed"));
        let json = serde_json::to_string(&row).unwrap();
        let back: ValidationResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back.dt_days, 7.0);
    }

    #[test]
    fn uncertainty_mode_parse_and_suffixes() {
        assert_eq!(
            UncertaintyMode::parse("linear").unwrap(),
            UncertaintyMode::Linear
        );
        assert_eq!(
            UncertaintyMode::parse("sigma-point").unwrap().arm_suffix(),
            Some("sp")
        );
        assert_eq!(
            UncertaintyMode::parse("monte-carlo").unwrap().arm_suffix(),
            Some("mc")
        );
        assert!(UncertaintyMode::parse("vibes").is_err());
        assert_eq!(UncertaintyMode::Linear.arm_suffix(), None);
    }

    #[test]
    fn cubature_offsets_reproduce_the_covariance() {
        // Second moment of the 12 equal-weight cubature deltas must equal
        // the input covariance exactly (the spherical-radial rule is exact
        // for second moments through the identity map).
        let mut cov = [[0.0; 6]; 6];
        for (i, row) in cov.iter_mut().enumerate() {
            row[i] = 1.0 + i as f64;
        }
        cov[0][3] = 0.4;
        cov[3][0] = 0.4;
        let l = chol6(&cov).unwrap();
        let offs = state_offsets(UncertaintyMode::SigmaPoint, &l, 1, DEFAULT_MC_SAMPLES);
        assert_eq!(offs.len(), 12);
        let mut m = [[0.0f64; 6]; 6];
        for d in &offs {
            for i in 0..6 {
                for j in 0..6 {
                    m[i][j] += d[i] * d[j] / offs.len() as f64;
                }
            }
        }
        for i in 0..6 {
            for j in 0..6 {
                assert!((m[i][j] - cov[i][j]).abs() < 1e-12, "[{i}][{j}]");
            }
        }
    }

    #[test]
    fn monte_carlo_offsets_are_deterministic_with_sane_moments() {
        let mut cov = [[0.0; 6]; 6];
        for (i, row) in cov.iter_mut().enumerate() {
            row[i] = 2.0;
        }
        let l = chol6(&cov).unwrap();
        let a = state_offsets(UncertaintyMode::MonteCarlo, &l, 42, 128);
        let b = state_offsets(UncertaintyMode::MonteCarlo, &l, 42, 128);
        assert_eq!(a, b, "same seed must reproduce bit-for-bit");
        let c = state_offsets(UncertaintyMode::MonteCarlo, &l, 43, 128);
        assert_ne!(a, c);
        let var0: f64 = a.iter().map(|d| d[0] * d[0]).sum::<f64>() / a.len() as f64;
        assert!((var0 - 2.0).abs() < 0.8, "var {var0}, want ~2 at n=128");
    }

    #[test]
    fn chol6_rejects_non_psd() {
        let mut bad = [[0.0; 6]; 6];
        bad[0][0] = 1.0;
        bad[1][1] = -1.0;
        assert!(chol6(&bad).is_none());
    }

    #[test]
    fn second_moment_pins_and_refuses_wrapped_clouds() {
        let m = second_moment_arcsec2(&[Some((1.0, 0.0)), Some((-1.0, 0.0))]).unwrap();
        assert!((m[0][0] - 1.0).abs() < 1e-12 && m[1][1].abs() < 1e-12);
        assert!(second_moment_arcsec2(&[Some((1.0, 0.0)), None]).is_none());
    }

    #[test]
    fn gnomonic_offset_pin() {
        // 1″ of great-circle RA offset at δ = 20°.
        let cosd = 20.0_f64.to_radians().cos();
        let (e, n) =
            gnomonic_offset_arcsec(100.0, 20.0, 100.0 + 1.0 / 3600.0 / cosd, 20.0).unwrap();
        assert!((e - 1.0).abs() < 1e-6 && n.abs() < 1e-4);
        assert!(gnomonic_offset_arcsec(100.0, 0.0, 280.5, 0.0).is_none());
    }

    #[test]
    fn second_order_moment_reduces_to_linear_and_pins_quadratic() {
        // H = 0 → the linear quadratic form exactly.
        let sig: [[f64; 6]; 6] = std::array::from_fn(|i| {
            std::array::from_fn(|j| if i == j { (i + 1) as f64 } else { 0.0 })
        });
        let j: Vec<f64> = vec![1.0, 2.0, 0.0, 0.0, 0.0, 0.0];
        let h0 = [[0.0; 6]; 6];
        let lin = second_order_moment(&j, &j, &h0, &h0, &sig);
        assert!((lin - (1.0 * 1.0 + 4.0 * 2.0)).abs() < 1e-12, "{lin}");
        // Pure quadratic pin: J = 0, H = diag(h) → M = ¼(Σ hᵢσᵢ)² + ½Σ hᵢ²σᵢ².
        let mut h = [[0.0; 6]; 6];
        h[0][0] = 2.0;
        h[1][1] = 3.0;
        let z = [0.0; 6];
        let m = second_order_moment(&z, &z, &h, &h, &sig);
        let tr = 2.0 * 1.0 + 3.0 * 2.0;
        let tr2 = (2.0f64 * 1.0).powi(2) + (3.0f64 * 2.0).powi(2);
        assert!((m - (0.25 * tr * tr + 0.5 * tr2)).abs() < 1e-12, "{m}");
    }
}
