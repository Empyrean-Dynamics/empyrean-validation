//! empyrean validation runner — CLI channel.
//!
//! A standalone CLI binary that exercises the empyrean public API exactly
//! how a user would invoke it from a shell script: take orbit + target
//! as args, propagate / generate ephemeris / determine an orbit, print
//! the result to stdout, exit.
//!
//! Three modes (selected via `--mode`):
//! - `prop`: Cartesian state propagation. Output: `x y z vx vy vz time_ms`.
//! - `eph`: topocentric ephemeris. Output: `ra_deg dec_deg rho_au lt_d time_ms`.
//! - `od` : orbit determination from an ADES file. Output:
//!   `x y z vx vy vz iterations time_ms`.
//!
//! Unlike the rust / python / c channels (which all run inside a long-
//! lived process with warm caches), this binary is fork-exec'd once
//! per row by drive.py. The reported `time_ms` includes only the
//! relevant API call — not process startup.
//!
//! # Per-method uncertainty axis (prop mode)
//!
//! The plan row's `propagation_uncertainty` method is threaded into the
//! propagation config exactly as the rust channel's `build_uncertainty_axes`
//! does: `--uncertainty-method <tag>` in one-shot mode, or an **optional**
//! 19th token after the 18 `prop` fields in daemon mode. The tag is one of the
//! schema's `uncertainty_modes` spellings (`none`,
//! `first_order`, `second_order`, `auto`,
//! `sigma_point`, `monte_carlo`,
//! `gaussian_mixture`); Monte Carlo carries the suite-wide
//! `MONTE_CARLO_SAMPLE_COUNT` / `MONTE_CARLO_SEED`, and an unrecognized tag is
//! refused by name (`fail unknown_uncertainty_method:<tag>`), never
//! substituted. Every tag but `none` attaches the same synthetic
//! covariance the rust channel uses, which is what makes the propagator
//! dispatch to Jet1 / STM integration.
//!
//! When a method is requested the prop output line carries the delivered 0.11
//! products after `time_ms`, as whitespace-free `key=value` tokens:
//! `resolved_method` (the delivered covariance kind's tag, never the request),
//! `cov_kind` (the wire discriminant), `cov_joint_width`, `cov_tri` (the packed
//! lower triangle, comma-separated), `orbit_delivered` / `orbit_status` (off
//! `outcomes[0]`), and the six `mix_*` tallies over the retained mixture
//! components. Absent scalars render `na`. **Without** a method the line stays
//! byte-identical to before (`x y z vx vy vz time_ms`, or `ok …` in daemon
//! mode), so the existing driver and the first-order goldens are unchanged.
//!
//! Gaps at this pin, named rather than back-filled:
//! - **OD method axis** — `ODConfig` carries no `uncertainty_method` at this
//!   distribution revision, so OD fit rows run method-free; the shared
//!   [`empyrean_validation::schema::OD_METHOD_AXIS_NOT_PRODUCED`] note is the
//!   carrier (recorded by the driver), and the per-method OD transport rows
//!   are `not produced` here, exactly as the rust and core channels.
//! - **Ephemeris method axis** — the cli ephemeris one-shot stays
//!   covariance-free first order at this pin (the per-method ephemeris leg is a
//!   follow-up), matching the rust channel's `None` ephemeris products.
//! - **Driver wiring** — this commit extends only the cli runner binary
//!   (`runners/cli/src`); carrying the emitted products into the schema's
//!   per-method JSON fields is a `drive.py` follow-up (out of this commit's
//!   scope, which is the `src` runner).

use clap::{Parser, ValueEnum};
use std::time::Instant;

use empyrean::propagate::{ComponentStatus, MixtureComponent};
use empyrean::{
    Context, CoordinateState, CovarianceKind, EphemerisConfig, Epoch, ForceModelTier, Frame,
    ODConfig, Orbit, OrbitOutcome, Origin, PropagationConfig, Representation, SolveForParams,
    UncertaintyMethod,
};
use empyrean_validation::schema::uncertainty_modes as um;

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum Mode {
    /// Cartesian propagation.
    Prop,
    /// Topocentric ephemeris.
    Eph,
    /// Orbit determination from an ADES file.
    Od,
}

#[derive(Parser, Debug)]
#[command(
    name = "empyrean-cli-runner",
    about = "Single-row prop / ephemeris / OD via the empyrean CLI distribution channel."
)]
struct Cli {
    /// Daemon mode: load context once, then read one row per stdin line
    /// (mirrors the C runner's protocol). Prints `ready` on stderr after
    /// context init; for each input line writes `ok ...` or `fail ...`
    /// to stdout. Use this from the validation driver to amortize
    /// `Context::from_data_dir` across thousands of rows.
    #[arg(long, default_value_t = false)]
    daemon: bool,

    /// Operation. Determines which other args are required.
    #[arg(long, value_enum, default_value_t = Mode::Prop)]
    mode: Mode,

    // ── prop / eph: IC ────────────────────────────────────────
    /// Initial-condition epoch (MJD TDB). prop / eph.
    #[arg(long, required_if_eq_any([("mode", "prop"), ("mode", "eph")]))]
    epoch: Option<f64>,
    /// IC Cartesian position (AU, ICRF, SSB-centered): "x,y,z". prop / eph.
    #[arg(long, value_parser = parse_triple)]
    pos: Option<[f64; 3]>,
    /// IC Cartesian velocity (AU/day, ICRF, SSB-centered): "vx,vy,vz". prop / eph.
    #[arg(long, value_parser = parse_triple)]
    vel: Option<[f64; 3]>,
    /// Marsden A1 (radial), AU/day². Default 0.
    #[arg(long, default_value_t = 0.0)]
    a1: f64,
    /// Marsden A2 (transverse).
    #[arg(long, default_value_t = 0.0)]
    a2: f64,
    /// Marsden A3 (normal).
    #[arg(long, default_value_t = 0.0)]
    a3: f64,
    /// g(r) parameters: "alpha,r0,m,n,k". All-zeros → inverse_square.
    #[arg(long, value_parser = parse_quintuple, default_value = "0,0,0,0,0")]
    g: [f64; 5],
    /// Per-method uncertainty axis (prop mode): a `propagation_uncertainty`
    /// plan tag — `none`, `first_order`,
    /// `second_order`, `auto`, `sigma_point`,
    /// `monte_carlo`, or `gaussian_mixture`. When set,
    /// the synthetic covariance is attached (all tags except `none`),
    /// the named rung is requested (MonteCarlo at the suite-wide N and seed),
    /// and the delivered 0.11 per-method products are appended to the prop
    /// output line. An unrecognized tag is refused by name — never silently
    /// substituted. Omitted → the covariance-free first-order path, with the
    /// output byte-identical to before.
    #[arg(long)]
    uncertainty_method: Option<String>,

    // ── shared ────────────────────────────────────────────────
    /// Force-model tier: 0=Approximate, 1=Basic, 2=Standard.
    #[arg(long, default_value_t = 2)]
    force_model: i32,
    /// Target epoch (MJD TDB). prop / eph (output epoch for OD via output_epoch=Epoch).
    #[arg(long)]
    target: Option<f64>,
    /// Empyrean data directory. Default: ~/.empyrean/data/.
    #[arg(long)]
    data_dir: Option<std::path::PathBuf>,

    // ── eph only ──────────────────────────────────────────────
    /// MPC observer code (eph mode only).
    #[arg(long, required_if_eq("mode", "eph"))]
    observer: Option<String>,

    // ── od only ───────────────────────────────────────────────
    /// ADES PSV file (od mode only).
    #[arg(long, required_if_eq("mode", "od"))]
    ades: Option<std::path::PathBuf>,
    /// Maximum DC iterations (od mode).
    #[arg(long, default_value_t = 20)]
    max_iterations: u32,
}

fn parse_triple(s: &str) -> Result<[f64; 3], String> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 3 {
        return Err(format!(
            "expected 3 comma-separated floats, got {}",
            parts.len()
        ));
    }
    let v: Result<Vec<f64>, _> = parts.iter().map(|p| p.parse::<f64>()).collect();
    let v = v.map_err(|e| e.to_string())?;
    Ok([v[0], v[1], v[2]])
}

fn parse_quintuple(s: &str) -> Result<[f64; 5], String> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 5 {
        return Err(format!(
            "expected 5 comma-separated floats, got {}",
            parts.len()
        ));
    }
    let v: Result<Vec<f64>, _> = parts.iter().map(|p| p.parse::<f64>()).collect();
    let v = v.map_err(|e| e.to_string())?;
    Ok([v[0], v[1], v[2], v[3], v[4]])
}

// ── Per-method uncertainty axis ─────────────────────────────────────
//
// The cli channel threads the plan row's `propagation_uncertainty` method
// into the propagation config exactly as the rust channel's
// `build_uncertainty_axes` does, reads the delivered 0.11 products off the
// wrapper (never recomputed), and emits them beside the state so the driver
// can carry them into the schema's per-method fields. The method tags, the
// Monte-Carlo sample count and seed, and the wire covariance-kind values all
// come from `empyrean_validation::schema` so this channel requests the same
// surfaces and records the same strings the rust and core channels do.

/// The suite's synthetic 6×6 Cartesian covariance — 1 km position σ, 1 mm/s
/// velocity σ, uncorrelated — byte-identical to the rust channel's
/// `build_uncertainty_axes` covariance, so a same-method cross-channel compare
/// sees the same input uncertainty. Units are AU and AU/day.
fn synthetic_covariance() -> [[f64; 6]; 6] {
    let pos_var_au = (1.0 / 149_597_870.700_f64).powi(2);
    let vel_var_au_d = (1e-6 / 149_597_870.700_f64 * 86_400.0).powi(2);
    let mut c = [[0.0_f64; 6]; 6];
    c[0][0] = pos_var_au;
    c[1][1] = pos_var_au;
    c[2][2] = pos_var_au;
    c[3][3] = vel_var_au_d;
    c[4][4] = vel_var_au_d;
    c[5][5] = vel_var_au_d;
    c
}

/// Map a plan `propagation_uncertainty` composite tag to
/// `(attach_covariance, method)`.
///
/// The engine method is the tag's method **prefix** ([`um::method_of`]) — the
/// detection/timing arm is irrelevant to which rung the engine runs, so the
/// cli channel keys the rung on the prefix alone. `none` is the covariance-free
/// path — first order, no covariance attached. Every other method attaches the
/// synthetic covariance and runs its named rung. `monte_carlo` carries the
/// suite-wide sample count and seed from the schema — never the engine's
/// per-call convenience seed — so a seeded Monte-Carlo row is a cross-channel
/// bit check. A tag outside the vocabulary yields `None` so the caller refuses
/// it by name rather than silently substituting a method (the
/// no-silent-substitution invariant).
fn method_for_tag(tag: &str) -> Option<(bool, UncertaintyMethod)> {
    let m = match um::method_of(tag)? {
        um::NONE => (false, UncertaintyMethod::FirstOrder),
        um::FIRST_ORDER => (true, UncertaintyMethod::FirstOrder),
        um::SECOND_ORDER => (true, UncertaintyMethod::SecondOrder),
        um::AUTO => (true, UncertaintyMethod::auto()),
        um::SIGMA_POINT => (true, UncertaintyMethod::sigma_point()),
        um::MONTE_CARLO => (
            true,
            UncertaintyMethod::MonteCarlo {
                n_samples: um::MONTE_CARLO_SAMPLE_COUNT as usize,
                seed: Some(um::MONTE_CARLO_SEED),
            },
        ),
        um::GAUSSIAN_MIXTURE => (true, UncertaintyMethod::gaussian_mixture()),
        _ => return None,
    };
    Some(m)
}

/// The C-ABI wire discriminant for a delivered covariance kind — the `cov_kind`
/// the schema carries (linear 0, second-order 1, mixture 3, monte-carlo 4,
/// sigma-point 5). Mirrors the rust channel's `cov_kind_wire` and the core
/// channel's `kind.wire_discriminant()`; pinned by the unit test so the
/// restated map cannot drift from the `EMPYREAN_COVARIANCE_KIND_*` tags.
fn cov_kind_wire(kind: CovarianceKind) -> u8 {
    match kind {
        CovarianceKind::Linear => 0,
        CovarianceKind::SecondOrder => 1,
        CovarianceKind::Mixture => 3,
        CovarianceKind::MonteCarlo => 4,
        CovarianceKind::SigmaPoint => 5,
    }
}

/// The `resolved_method` tag a propagation row reports: the tag of the
/// covariance **kind the engine delivered**, never the request — so a silent
/// substitution under an explicit method is caught by the cross-channel
/// compare. Mirrors the rust channel's `resolved_method_for`.
fn resolved_method_tag(kind: CovarianceKind) -> &'static str {
    match kind {
        CovarianceKind::Linear => um::FIRST_ORDER,
        CovarianceKind::SecondOrder => um::SECOND_ORDER,
        CovarianceKind::Mixture => um::GAUSSIAN_MIXTURE,
        CovarianceKind::MonteCarlo => um::MONTE_CARLO,
        CovarianceKind::SigmaPoint => um::SIGMA_POINT,
    }
}

/// Name an `EMPYREAN_PROPAGATE_FAILURE_*` classification code. Mirrors the rust
/// channel's `propagate_failure_variant`; an unrecognized code falls back to
/// the engine's own message so no failure is ever a bare number.
fn propagate_failure_variant(code: i32, message: &str) -> String {
    match code {
        1 => "integration".to_string(),
        2 => "kepler_dt_backprop".to_string(),
        3 => "transform".to_string(),
        4 => "covariance_input".to_string(),
        5 => "sigma_point".to_string(),
        6 => "sampled_parameter".to_string(),
        7 => "ensemble_member".to_string(),
        8 => "output_assembly".to_string(),
        99 => "other".to_string(),
        other => format!("code_{other}({message})"),
    }
}

/// Per-orbit delivery outcome read off `outcomes[0]` — the sole delivery
/// discriminator, never a row count. Mirrors the rust channel's
/// `orbit_outcome_channel`. Returns `(orbit_delivered, orbit_status)`.
fn outcome_channel(outcome: &OrbitOutcome, withheld: Option<&str>) -> (bool, String) {
    match outcome {
        OrbitOutcome::Delivered { .. } => match withheld {
            Some(reason) => (true, format!("cov_withheld:{reason}")),
            None => (true, "delivered".to_string()),
        },
        OrbitOutcome::Failed { code, message } => (
            false,
            format!("failed:{}", propagate_failure_variant(*code, message)),
        ),
    }
}

/// The six Gaussian-mixture tallies over the engine's RETAINED mixture
/// components (one entry per surviving sub-Gaussian), tallied by
/// `ComponentStatus`. Mirrors the rust channel's `populate_mixture_tallies`;
/// an empty component set yields `None` — never a fabricated zero — so an
/// unsplit (second-order-delivered) row carries no mixture tally. The counts
/// are over retained components (the wrapper surface), matching the rust
/// channel; the core channel reads villeneuve's pre-retention tallies, so a
/// cross-channel difference is a surface difference, not physics.
struct MixTallies {
    total: u32,
    weight: f64,
    failed: u32,
    unresolved: u32,
    curvature: u32,
    sky: u32,
}

fn mixture_tallies(components: &[MixtureComponent]) -> Option<MixTallies> {
    if components.is_empty() {
        return None;
    }
    let (mut failed, mut unresolved, mut curvature, mut sky) = (0u32, 0u32, 0u32, 0u32);
    let mut weight = 0.0;
    for c in components {
        weight += c.weight;
        match c.status {
            ComponentStatus::Resolved => {}
            ComponentStatus::CurvatureRefused { .. } => curvature += 1,
            ComponentStatus::Unresolved => unresolved += 1,
            ComponentStatus::Failed => failed += 1,
            ComponentStatus::SkyLinearizationRefused { .. } => sky += 1,
        }
    }
    Some(MixTallies {
        total: components.len() as u32,
        weight,
        failed,
        unresolved,
        curvature,
        sky,
    })
}

/// The 0.11 per-method products carried on a propagation row, extracted from
/// the wrapper's delivered result (never recomputed). All fields are `None`
/// when the engine delivered no covariance (`none`), except the outcome
/// channel which is always read off `outcomes[0]`.
struct Products {
    resolved_method: Option<&'static str>,
    cov_kind: Option<u8>,
    cov_joint_width: Option<u32>,
    cov_tri: Option<Vec<f64>>,
    orbit_delivered: bool,
    orbit_status: String,
    mix: Option<MixTallies>,
}

impl Products {
    /// Read the per-method products off a delivered propagation result.
    /// `expected_cov` is true when the method attached a covariance, so a
    /// delivered-but-unreadable covariance is reported as `cov_withheld:…`
    /// on the outcome rather than silently dropped.
    fn from_result(result: &empyrean::PropagationResult, expected_cov: bool) -> Self {
        let mut resolved_method = None;
        let mut cov_kind = None;
        let mut cov_joint_width = None;
        let mut cov_tri = None;
        let mut withheld: Option<String> = None;
        match result.covariance_at_cartesian(0, 0) {
            Ok(tc) => {
                resolved_method = Some(resolved_method_tag(tc.kind()));
                cov_kind = Some(cov_kind_wire(tc.joint.kind));
                cov_joint_width = Some(tc.joint.width as u32);
                cov_tri = Some(tc.joint.tri.clone());
            }
            Err(e) => {
                if expected_cov {
                    withheld = Some(e.to_string());
                }
            }
        }
        let (orbit_delivered, orbit_status) = match result.outcomes.first() {
            Some(oc) => outcome_channel(oc, withheld.as_deref()),
            None => (false, "no_outcome".to_string()),
        };
        let mix = result.mixtures.first().and_then(|chain| {
            let comps: Vec<MixtureComponent> = chain.components.iter().flatten().cloned().collect();
            mixture_tallies(&comps)
        });
        Products {
            resolved_method,
            cov_kind,
            cov_joint_width,
            cov_tri,
            orbit_delivered,
            orbit_status,
            mix,
        }
    }

    /// Render the products as whitespace-free `key=value` tokens appended to a
    /// prop output line. Absent scalars render as `na`; the packed triangle is
    /// a comma-separated list of `{:.18e}` doubles (empty list when absent).
    /// Every value is a single token so the whole line stays positionally
    /// splittable by the driver. The schema fields each token feeds:
    /// `resolved_method`, `cov_kind`, `cov_joint_width`, `cov_tri`,
    /// `orbit_delivered`, `orbit_status`, and the six `mix_*`.
    fn render(&self) -> String {
        let na = || "na".to_string();
        let tri = match &self.cov_tri {
            Some(v) => v
                .iter()
                .map(|x| format!("{x:.18e}"))
                .collect::<Vec<_>>()
                .join(","),
            None => na(),
        };
        // The status can carry the engine's free-form withheld/failure text,
        // which may contain spaces; collapse any whitespace to `_` so the
        // token stays single.
        let status: String = self
            .orbit_status
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("_");
        let (mt, mw, mf, mu, mc, ms) = match &self.mix {
            Some(m) => (
                m.total.to_string(),
                format!("{:.18e}", m.weight),
                m.failed.to_string(),
                m.unresolved.to_string(),
                m.curvature.to_string(),
                m.sky.to_string(),
            ),
            None => (na(), na(), na(), na(), na(), na()),
        };
        format!(
            "resolved_method={} cov_kind={} cov_joint_width={} orbit_delivered={} \
             orbit_status={} mix_n_components_total={} mix_weight_delivered={} \
             mix_n_failed={} mix_n_unresolved={} mix_n_curvature_refused={} \
             mix_n_sky_linearization_refused={} cov_tri={}",
            self.resolved_method.unwrap_or("na"),
            self.cov_kind.map(|k| k.to_string()).unwrap_or_else(na),
            self.cov_joint_width
                .map(|w| w.to_string())
                .unwrap_or_else(na),
            if self.orbit_delivered { 1 } else { 0 },
            status,
            mt,
            mw,
            mf,
            mu,
            mc,
            ms,
            tri,
        )
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let ctx = Context::from_data_dir(cli.data_dir.as_deref())?;

    if cli.daemon {
        return run_daemon(&ctx);
    }

    let force = match cli.force_model {
        0 => ForceModelTier::Approximate,
        1 => ForceModelTier::Basic,
        _ => ForceModelTier::Standard,
    };

    match cli.mode {
        Mode::Prop => run_prop(&ctx, &cli, force),
        Mode::Eph => run_eph(&ctx, &cli, force),
        Mode::Od => run_od(&ctx, &cli, force),
    }
}

/// Tier int → enum used by the stdin protocol.
fn tier_from_int(force_model: i32) -> ForceModelTier {
    match force_model {
        0 => ForceModelTier::Approximate,
        1 => ForceModelTier::Basic,
        _ => ForceModelTier::Standard,
    }
}

/// Daemon mode: read one row per stdin line, dispatch by leading mode
/// keyword (`prop` / `eph` / `od`), write a single `ok …` or `fail …`
/// line per row. Mirrors the C runner's protocol exactly so the
/// validation driver can swap binaries without touching its row
/// handling. Context is loaded once and reused; warmup happens on the
/// first prop row.
fn run_daemon(ctx: &Context) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::{BufRead, Write};

    eprintln!("ready");
    let mut warmed_up = false;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let (mode_word, rest) = match trimmed.split_once(char::is_whitespace) {
            Some((m, r)) => (m, r.trim_start()),
            None => (trimmed, ""),
        };
        match mode_word {
            "prop" => match daemon_prop(ctx, rest, &mut warmed_up) {
                Ok(out) => writeln!(stdout, "{out}")?,
                Err(e) => writeln!(stdout, "fail {e}")?,
            },
            "eph" => match daemon_eph(ctx, rest) {
                Ok(out) => writeln!(stdout, "{out}")?,
                Err(e) => writeln!(stdout, "fail {e}")?,
            },
            "od" => match daemon_od(ctx, rest) {
                Ok(out) => writeln!(stdout, "{out}")?,
                Err(e) => writeln!(stdout, "fail {e}")?,
            },
            other => writeln!(stdout, "fail unknown_mode_{other}")?,
        }
        stdout.flush()?;
    }
    Ok(())
}

#[allow(clippy::type_complexity)]
fn parse_prop_args(
    rest: &str,
) -> Result<
    (
        Orbit,
        ForceModelTier,
        Epoch,
        Option<(bool, UncertaintyMethod)>,
    ),
    String,
> {
    // Layout (matches runner.c handle_prop): epoch x y z vx vy vz
    // a1 a2 a3 g0 g1 g2 g3 g4 force_model target non_grav_dt  (18 fields),
    // plus an OPTIONAL 19th token: a `propagation_uncertainty` method tag.
    // The driver's 18-field line leaves the method absent → covariance-free
    // first order, output byte-identical to before. A 19th token requests
    // that method and attaches the synthetic covariance (all tags but
    // `none`); an unrecognized tag is refused by name.
    let mut tokens = rest.split_whitespace();
    let mut f: Vec<f64> = Vec::with_capacity(18);
    for _ in 0..18 {
        match tokens.next() {
            Some(t) => f.push(t.parse::<f64>().map_err(|e| e.to_string())?),
            None => return Err(format!("prop_parse_{}_fields", f.len())),
        }
    }
    let method = match tokens.next() {
        Some(tag) => {
            Some(method_for_tag(tag).ok_or_else(|| format!("unknown_uncertainty_method:{tag}"))?)
        }
        None => None,
    };
    // Any further token is a malformed line — fail loudly, never ignore.
    if tokens.next().is_some() {
        return Err("prop_parse_extra_fields".to_string());
    }
    let attach_cov = method.as_ref().map(|(a, _)| *a).unwrap_or(false);

    let pos = [f[1], f[2], f[3]];
    let vel = [f[4], f[5], f[6]];
    let a1 = f[7];
    let a2 = f[8];
    let a3 = f[9];
    let g = [f[10], f[11], f[12], f[13], f[14]];
    let force_model = f[15] as i32;
    let target = Epoch::from_mjd_tdb(f[16]);
    let non_grav_dt = f[17];

    let state = CoordinateState {
        epoch: Epoch::from_mjd_tdb(f[0]),
        elements: [pos[0], pos[1], pos[2], vel[0], vel[1], vel[2]],
        // Attached only when a covariance-bearing method was requested; the
        // 18-field (no method) line keeps `None`, byte-identical to before.
        covariance: if attach_cov {
            Some(synthetic_covariance())
        } else {
            None
        },
        representation: Representation::Cartesian,
        frame: Frame::ICRF,
        origin: Origin::SSB,
    };
    let mut orbit = Orbit::new(state).with_nongrav(a1, a2, a3);
    if g.iter().any(|v| *v != 0.0) {
        orbit = orbit.with_g_function(g[0], g[1], g[2], g[3], g[4]);
    }
    if non_grav_dt.is_finite() {
        orbit = orbit.with_non_grav_dt(Some(non_grav_dt));
    }
    Ok((orbit, tier_from_int(force_model), target, method))
}

fn daemon_prop(ctx: &Context, rest: &str, warmed_up: &mut bool) -> Result<String, String> {
    let (orbit, force, target, method) = parse_prop_args(rest)?;
    let (attach_cov, umethod) = match &method {
        Some((a, m)) => (*a, m.clone()),
        None => (false, UncertaintyMethod::FirstOrder),
    };
    let cfg = PropagationConfig {
        force_model: force,
        uncertainty_method: umethod,
        frame: Frame::ICRF,
        // Daemon mode also single-threaded: each call is one orbit, so
        // a Rayon pool buys nothing and avoids contention if multiple
        // daemons are run side-by-side.
        num_threads: std::num::NonZeroUsize::new(1),
        ..PropagationConfig::default()
    };

    if !*warmed_up {
        for _ in 0..5 {
            let _ = ctx.propagate(std::slice::from_ref(&orbit), &[target], &cfg);
        }
        *warmed_up = true;
        eprintln!("warmup done");
    }

    let mut best_ms = f64::INFINITY;
    let mut last_pos = [0.0f64; 3];
    let mut last_vel = [0.0f64; 3];
    let mut last_result: Option<empyrean::PropagationResult> = None;
    for _ in 0..3 {
        let t0 = Instant::now();
        let result = ctx
            .propagate(std::slice::from_ref(&orbit), &[target], &cfg)
            .map_err(|e| e.to_string())?;
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        if let Some(s) = result.states.first() {
            last_pos = s.position;
            last_vel = s.velocity;
        }
        if ms < best_ms {
            best_ms = ms;
        }
        last_result = Some(result);
    }
    // No method → byte-identical to before. A method → append the delivered
    // per-method products so the driver can carry them into the schema's
    // per-method fields.
    match method {
        None => Ok(format!(
            "ok {:.18e} {:.18e} {:.18e} {:.18e} {:.18e} {:.18e} {:.6}",
            last_pos[0], last_pos[1], last_pos[2], last_vel[0], last_vel[1], last_vel[2], best_ms,
        )),
        Some(_) => {
            let products = last_result
                .as_ref()
                .map(|r| Products::from_result(r, attach_cov).render())
                .unwrap_or_default();
            Ok(format!(
                "ok {:.18e} {:.18e} {:.18e} {:.18e} {:.18e} {:.18e} {:.6} {}",
                last_pos[0],
                last_pos[1],
                last_pos[2],
                last_vel[0],
                last_vel[1],
                last_vel[2],
                best_ms,
                products,
            ))
        }
    }
}

fn daemon_eph(ctx: &Context, rest: &str) -> Result<String, String> {
    // Same as prop (18 fields) plus a trailing observer code.
    let mut tokens = rest.split_whitespace();
    let mut floats = Vec::with_capacity(18);
    for _ in 0..18 {
        let t = tokens
            .next()
            .ok_or_else(|| format!("eph_parse_{}_fields", floats.len()))?;
        floats.push(t.parse::<f64>().map_err(|e| e.to_string())?);
    }
    let obs_code = tokens
        .next()
        .ok_or_else(|| "eph_parse_missing_observer".to_string())?;
    let pos = [floats[1], floats[2], floats[3]];
    let vel = [floats[4], floats[5], floats[6]];
    let g = [floats[10], floats[11], floats[12], floats[13], floats[14]];
    let force = tier_from_int(floats[15] as i32);
    let target = Epoch::from_mjd_tdb(floats[16]);
    let non_grav_dt = floats[17];
    let state = CoordinateState {
        epoch: Epoch::from_mjd_tdb(floats[0]),
        elements: [pos[0], pos[1], pos[2], vel[0], vel[1], vel[2]],
        covariance: None,
        // 0.11 CoordinateState is state-only (6×6); the state↔parameter
        // border now lives on the engine-side packed joint, not the input
        // state, so there is nothing to carry here.
        representation: Representation::Cartesian,
        frame: Frame::ICRF,
        origin: Origin::SSB,
    };
    let mut orbit = Orbit::new(state).with_nongrav(floats[7], floats[8], floats[9]);
    if g.iter().any(|v| *v != 0.0) {
        orbit = orbit.with_g_function(g[0], g[1], g[2], g[3], g[4]);
    }
    if non_grav_dt.is_finite() {
        orbit = orbit.with_non_grav_dt(Some(non_grav_dt));
    }
    let observers = ctx
        // (ICRF, SSB) is the construction basis: observers come back
        // exactly as built, which is what ephemeris generation requires.
        .get_observers(&[obs_code], &[target], Frame::ICRF, Origin::SSB)
        .map_err(|e| e.to_string())?;
    let mut cfg = EphemerisConfig::with_force_model(force);
    cfg.propagation.num_threads = std::num::NonZeroUsize::new(1);
    let _ = ctx.generate_ephemeris(std::slice::from_ref(&orbit), &observers, &cfg);
    let mut best_ms = f64::INFINITY;
    let mut last_ra = f64::NAN;
    let mut last_dec = f64::NAN;
    let mut last_rho = f64::NAN;
    let mut last_lt = f64::NAN;
    for _ in 0..3 {
        let t0 = Instant::now();
        let entries = ctx
            .generate_ephemeris(std::slice::from_ref(&orbit), &observers, &cfg)
            .map_err(|e| e.to_string())?;
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        if let Some(e) = entries.entries.first() {
            last_ra = e.ra_deg;
            last_dec = e.dec_deg;
            last_rho = e.rho_au;
            last_lt = e.light_time_days;
        }
        if ms < best_ms {
            best_ms = ms;
        }
    }
    Ok(format!(
        "ok {:.18e} {:.18e} {:.18e} {:.18e} {:.6}",
        last_ra, last_dec, last_rho, last_lt, best_ms,
    ))
}

fn daemon_od(ctx: &Context, rest: &str) -> Result<String, String> {
    // Protocol: `od FORCE EXCLUDE_NAIF PATH` where:
    //   - FORCE: integer tier (0/1/2)
    //   - EXCLUDE_NAIF: integer NAIF id of perturber to exclude (0 = none).
    //     Used by SB441-N16 self-perturbers so the body's own gravity does
    //     not act on itself during integration.
    //   - PATH: ADES PSV filename (may contain spaces; trailing field).
    let (force_str, after_force) = rest
        .split_once(char::is_whitespace)
        .ok_or_else(|| "od_parse_force".to_string())?;
    let force_model: i32 = force_str
        .parse()
        .map_err(|_| "od_parse_force".to_string())?;
    let force = tier_from_int(force_model);
    let (exclude_str, path) = after_force
        .trim_start()
        .split_once(char::is_whitespace)
        .ok_or_else(|| "od_parse_exclude".to_string())?;
    let exclude_naif: i32 = exclude_str
        .parse()
        .map_err(|_| "od_parse_exclude".to_string())?;
    let path = path.trim();
    let content = std::fs::read_to_string(path).map_err(|e| format!("od_open_{path}: {e}"))?;
    let observations = ctx.read_ades(&content).map_err(|e| e.to_string())?;
    let excluded_perturbers = if exclude_naif == 0 {
        Vec::new()
    } else {
        match Origin::from_naif_id(exclude_naif) {
            Some(o) => vec![o],
            None => return Err(format!("od_unknown_naif_{exclude_naif}")),
        }
    };
    let cfg = ODConfig {
        force_model: force,
        num_threads: 1,
        excluded_perturbers: excluded_perturbers.clone(),
        ..ODConfig::default()
    };
    let t0 = Instant::now();
    // determine is batch-first at 0.10: observations group by ADES object
    // identifier and each group gets its own entry, so a failed fit rides
    // inside an `Ok` batch. `into_single` refuses anything but exactly one
    // delivered fit, which keeps this per-fixture runner's error string
    // pointing at the real cause instead of at an empty batch.
    let result = ctx
        .determine(&observations, None, &cfg)
        .and_then(|batch| batch.into_single())
        .map_err(|e| e.to_string())?;
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    // `DetermineResult.orbit` is now a re-feedable `Orbit`; take the flat
    // state snapshot for the position/velocity output.
    let s = result.state();

    // Second OD: state + non-grav (9-param) fit on the SAME optical arc.
    // Mirrors the rust channel's non_grav_recovery second pass. The driver
    // pairs this with the per-object SBDB reference (ic_a1/ic_a2/ic_a3) and
    // emits a `non_grav_recovery` row that compares fitted-vs-JPL in σ.
    //
    // σ_a1 = sqrt(C9x9[6][6]), σ_a2 = sqrt(C9x9[7][7]), σ_a3 = sqrt(C9x9[8][8]).
    // The 9×9 covariance is populated ONLY when non-grav was actually solved
    // (`covariance_9x9 == Some`). "Loud failure" rule: if the fit did NOT
    // recover non-grav — 9×9 absent, OR a fitted a-value non-finite — emit
    // the wire sentinel `nan` for that field so the row reads as
    // "non-grav not recovered" (the driver maps `nan` → JSON `null`, never 0).
    let ng_cfg = ODConfig {
        force_model: force,
        num_threads: 1,
        excluded_perturbers,
        solve_for: SolveForParams::StateAndNonGrav,
        ..ODConfig::default()
    };
    // Optical-only fit's combined RMS — the driver writes it to the
    // orbit_determination row so that column is the cli channel's own fit,
    // not the (now-stripped) plan value.
    let od_rms = result.summary.rms_combined_arcsec;

    let (ng_a1, ng_a2, ng_a3, ng_s1, ng_s2, ng_s3, ng_rms, ng_px, ng_py, ng_pz) = match ctx
        .determine(&observations, None, &ng_cfg)
        .and_then(|batch| batch.into_single())
    {
        Ok(ng) => {
            let cov = ng.covariance_9x9;
            // rms + fitted position come from the 9-param fit and are
            // valid even when it fell back to state-only (only the
            // coefficients are then "not recovered"). Capture before the
            // closures borrow `cov`.
            let ng_rms = ng.summary.rms_combined_arcsec;
            let p = ng.state().position;
            let (a1, a2, a3) = (ng.orbit.a1, ng.orbit.a2, ng.orbit.a3);
            // Fitted Marsden coefficients live on the re-feedable orbit.
            let guard_a = |a: f64| if a.is_finite() { a } else { f64::NAN };
            // σ only exists when the 9×9 is present (non-grav solved).
            let sigma = |i: usize| {
                cov.map(|c| c[i][i].sqrt())
                    .filter(|s| s.is_finite())
                    .unwrap_or(f64::NAN)
            };
            if cov.is_some() {
                (
                    guard_a(a1),
                    guard_a(a2),
                    guard_a(a3),
                    sigma(6),
                    sigma(7),
                    sigma(8),
                    ng_rms,
                    p[0],
                    p[1],
                    p[2],
                )
            } else {
                // Non-grav not recovered (engine fell back to 6-param
                // state-only fit): coefficients are "not recovered", but
                // the fit's rms / position are still real.
                (
                    f64::NAN,
                    f64::NAN,
                    f64::NAN,
                    f64::NAN,
                    f64::NAN,
                    f64::NAN,
                    ng_rms,
                    p[0],
                    p[1],
                    p[2],
                )
            }
        }
        // A failed non-grav fit is not fatal to the row — the state-only
        // OD already succeeded; report the non-grav as not recovered.
        Err(_) => (
            f64::NAN,
            f64::NAN,
            f64::NAN,
            f64::NAN,
            f64::NAN,
            f64::NAN,
            f64::NAN,
            f64::NAN,
            f64::NAN,
            f64::NAN,
        ),
    };

    Ok(format!(
        "ok {:.18e} {:.18e} {:.18e} {:.18e} {:.18e} {:.18e} {} {:.6} \
         {:.18e} {:.18e} {:.18e} {:.18e} {:.18e} {:.18e} \
         {:.18e} {:.18e} {:.18e} {:.18e} {:.18e}",
        s.position[0],
        s.position[1],
        s.position[2],
        s.velocity[0],
        s.velocity[1],
        s.velocity[2],
        result.iterations,
        ms,
        ng_a1,
        ng_a2,
        ng_a3,
        ng_s1,
        ng_s2,
        ng_s3,
        od_rms,
        ng_rms,
        ng_px,
        ng_py,
        ng_pz,
    ))
}

fn build_orbit(cli: &Cli, attach_cov: bool) -> Orbit {
    let pos = cli.pos.expect("--pos required for prop/eph");
    let vel = cli.vel.expect("--vel required for prop/eph");
    let state = CoordinateState {
        epoch: Epoch::from_mjd_tdb(cli.epoch.expect("--epoch required for prop/eph")),
        elements: [pos[0], pos[1], pos[2], vel[0], vel[1], vel[2]],
        // The synthetic covariance is attached only when a covariance-bearing
        // uncertainty method was requested; it is what makes the propagator
        // dispatch to Jet1 / STM integration (empyrean is uncertainty-first).
        // `None` leaves the covariance-free f64 path, byte-identical to before.
        covariance: if attach_cov {
            Some(synthetic_covariance())
        } else {
            None
        },
        representation: Representation::Cartesian,
        frame: Frame::ICRF,
        origin: Origin::SSB,
    };
    let mut orbit = Orbit::new(state).with_nongrav(cli.a1, cli.a2, cli.a3);
    if cli.g.iter().any(|v| *v != 0.0) {
        orbit = orbit.with_g_function(cli.g[0], cli.g[1], cli.g[2], cli.g[3], cli.g[4]);
    }
    orbit
}

fn run_prop(
    ctx: &Context,
    cli: &Cli,
    force: ForceModelTier,
) -> Result<(), Box<dyn std::error::Error>> {
    let target = Epoch::from_mjd_tdb(cli.target.expect("--target required for prop"));
    // Resolve the per-method uncertainty axis. Omitted → covariance-free
    // first order (output byte-identical to before); an unrecognized tag is
    // refused by name rather than silently substituted.
    let requested = cli.uncertainty_method.is_some();
    let (attach_cov, method) = match cli.uncertainty_method.as_deref() {
        None => (false, UncertaintyMethod::FirstOrder),
        Some(tag) => {
            method_for_tag(tag).ok_or_else(|| format!("unknown_uncertainty_method:{tag}"))?
        }
    };
    let orbit = build_orbit(cli, attach_cov);
    let cfg = PropagationConfig {
        force_model: force,
        uncertainty_method: method,
        frame: Frame::ICRF,
        // Per-row fork-exec runner: only one orbit per invocation, so
        // a Rayon pool buys nothing and burns thread budget when the
        // driver runs many of these in parallel (the default
        // `num_threads: None` would request all cores per process and
        // hit ulimit -u very quickly).
        num_threads: std::num::NonZeroUsize::new(1),
        ..PropagationConfig::default()
    };

    // Warm-up: first propagation pays one-time costs (lazy SPK segment
    // loads, gravity-field table allocations, jet-buffer allocator
    // pools). The rust + python channels amortize these across many
    // calls in their long-lived processes; for the CLI's per-invocation
    // model we burn 5 untimed runs so the timing reflects steady-state.
    for _ in 0..5 {
        let _ = ctx.propagate(std::slice::from_ref(&orbit), &[target], &cfg);
    }

    // Best-of-3 — keeps timing comparable to rust/python/c channels. The last
    // result is kept so the delivered per-method products can be read off it.
    let mut best_ms = f64::INFINITY;
    let mut last_pos = [0.0f64; 3];
    let mut last_vel = [0.0f64; 3];
    let mut last_result: Option<empyrean::PropagationResult> = None;
    for _ in 0..3 {
        let t0 = Instant::now();
        let result = ctx.propagate(std::slice::from_ref(&orbit), &[target], &cfg)?;
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        if let Some(s) = result.states.first() {
            last_pos = s.position;
            last_vel = s.velocity;
        }
        if ms < best_ms {
            best_ms = ms;
        }
        last_result = Some(result);
    }

    // Output: x y z vx vy vz time_ms. When a method was requested, the
    // delivered per-method products (resolved_method, packed joint, outcome,
    // mixture tallies) are appended as key=value tokens; without a method the
    // line stays byte-identical to before.
    if requested {
        let products = last_result
            .as_ref()
            .map(|r| Products::from_result(r, attach_cov).render())
            .unwrap_or_default();
        println!(
            "{:.18e} {:.18e} {:.18e} {:.18e} {:.18e} {:.18e} {:.6} {}",
            last_pos[0],
            last_pos[1],
            last_pos[2],
            last_vel[0],
            last_vel[1],
            last_vel[2],
            best_ms,
            products,
        );
    } else {
        println!(
            "{:.18e} {:.18e} {:.18e} {:.18e} {:.18e} {:.18e} {:.6}",
            last_pos[0], last_pos[1], last_pos[2], last_vel[0], last_vel[1], last_vel[2], best_ms,
        );
    }
    Ok(())
}

fn run_eph(
    ctx: &Context,
    cli: &Cli,
    force: ForceModelTier,
) -> Result<(), Box<dyn std::error::Error>> {
    // The cli ephemeris one-shot keeps the covariance-free first-order path at
    // this pin; the per-method ephemeris axis is a follow-up (see the module
    // doc). `false` → no covariance attached, output unchanged.
    let orbit = build_orbit(cli, false);
    let target = Epoch::from_mjd_tdb(cli.target.expect("--target required for eph"));
    let obs_code = cli
        .observer
        .as_deref()
        .expect("--observer required for eph");

    // Resolve observer at the target epoch.
    // (ICRF, SSB) is the construction basis; see run_eph_line.
    let observers = ctx.get_observers(&[obs_code], &[target], Frame::ICRF, Origin::SSB)?;

    let mut cfg = EphemerisConfig::with_force_model(force);
    // Per-row fork-exec runner — pin to 1 thread (see run_prop comment).
    cfg.propagation.num_threads = std::num::NonZeroUsize::new(1);

    // Best-of-3 with one warm-up.
    let _ = ctx.generate_ephemeris(std::slice::from_ref(&orbit), &observers, &cfg);
    let mut best_ms = f64::INFINITY;
    let mut last_ra = f64::NAN;
    let mut last_dec = f64::NAN;
    let mut last_rho = f64::NAN;
    let mut last_lt = f64::NAN;
    for _ in 0..3 {
        let t0 = Instant::now();
        let entries = ctx.generate_ephemeris(std::slice::from_ref(&orbit), &observers, &cfg)?;
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        if let Some(e) = entries.entries.first() {
            last_ra = e.ra_deg;
            last_dec = e.dec_deg;
            last_rho = e.rho_au;
            last_lt = e.light_time_days;
        }
        if ms < best_ms {
            best_ms = ms;
        }
    }

    // Output: ra_deg dec_deg rho_au lt_d time_ms
    println!(
        "{:.18e} {:.18e} {:.18e} {:.18e} {:.6}",
        last_ra, last_dec, last_rho, last_lt, best_ms,
    );
    Ok(())
}

fn run_od(
    ctx: &Context,
    cli: &Cli,
    force: ForceModelTier,
) -> Result<(), Box<dyn std::error::Error>> {
    let ades_path = cli.ades.as_deref().expect("--ades required for od");
    // Read PSV content. Wrapper's read_ades passes through to the C ABI
    // which always parses input as content (no path detection at the
    // FFI layer), so we slurp the file here.
    let content = std::fs::read_to_string(ades_path)?;
    let observations = ctx.read_ades(&content)?;
    let cfg = ODConfig {
        force_model: force,
        max_iterations: cli.max_iterations,
        // Per-row fork-exec runner — pin to 1 thread (see run_prop comment).
        num_threads: 1,
        ..ODConfig::default()
    };
    let t0 = Instant::now();
    let result = ctx
        .determine(&observations, None, &cfg)
        .and_then(|batch| batch.into_single())?;
    let ms = t0.elapsed().as_secs_f64() * 1000.0;

    // `DetermineResult.orbit` is now a re-feedable `Orbit`; take the flat
    // state snapshot for the position/velocity output.
    let s = result.state();
    // Output: x y z vx vy vz iterations time_ms
    println!(
        "{:.18e} {:.18e} {:.18e} {:.18e} {:.18e} {:.18e} {} {:.6}",
        s.position[0],
        s.position[1],
        s.position[2],
        s.velocity[0],
        s.velocity[1],
        s.velocity[2],
        result.iterations,
        ms,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Method threading (brief item 1) ──────────────────────────────
    // The plan row's `propagation_uncertainty` tag must select the engine's
    // matching rung. Mutation: a `method_for_tag` that ignored the tag and
    // always returned FirstOrder turns the SecondOrder assertion red.
    #[test]
    fn method_for_tag_selects_the_requested_rung() {
        // The composite tag's method prefix selects the rung; the detection
        // arm is irrelevant to which rung runs.
        assert!(matches!(
            method_for_tag(um::SECOND_ORDER_DETECTION_ON),
            Some((true, UncertaintyMethod::SecondOrder))
        ));
        assert!(matches!(
            method_for_tag(um::FIRST_ORDER_DETECTION_ON),
            Some((true, UncertaintyMethod::FirstOrder))
        ));
        // none is the covariance-free path: first order, no covariance.
        assert!(matches!(
            method_for_tag(um::NONE_DETECTION_ON),
            Some((false, UncertaintyMethod::FirstOrder))
        ));
        // auto / sigma-point / gaussian-mixture attach a covariance and are
        // requestable; their inner shape is the engine's own concern.
        for tag in [
            um::AUTO_DETECTION_ON,
            um::SIGMA_POINT_DETECTION_ON,
            um::GAUSSIAN_MIXTURE_DETECTION_ON,
        ] {
            let (attach, _) = method_for_tag(tag).expect("tag must be requestable");
            assert!(attach, "{tag} must attach a covariance");
        }
        // A bare method (no arm) is NOT a plan tag and is refused — the rung is
        // keyed on a full composite tag, never a lone prefix.
        assert!(method_for_tag(um::SECOND_ORDER).is_none());
    }

    // MonteCarlo must carry the suite-wide sample count and seed from the
    // harness schema — never the engine's per-call convenience seed — so a
    // seeded MC row is a cross-channel bit check. Mutation: dropping the seed
    // (seed: None) turns this red.
    #[test]
    fn monte_carlo_carries_the_suite_sample_count_and_seed() {
        assert!(matches!(
            method_for_tag(um::MONTE_CARLO_DETECTION_ON),
            Some((true, UncertaintyMethod::MonteCarlo { n_samples: 100, seed: Some(s) }))
                if s == um::MONTE_CARLO_SEED
        ));
        assert_eq!(um::MONTE_CARLO_SAMPLE_COUNT, 100);
    }

    // An unrecognized tag is refused by name (None) rather than silently
    // substituted — the no-silent-substitution invariant (brief item 5).
    #[test]
    fn unknown_method_tag_is_refused_by_name() {
        assert!(method_for_tag("not_a_method").is_none());
    }

    // The synthetic covariance must match the rust channel's
    // `build_uncertainty_axes` covariance bit-for-bit (1 km position σ, 1 mm/s
    // velocity σ, uncorrelated), or a same-method cross-channel compare sees a
    // different input. Mutation: any scale change turns this red.
    #[test]
    fn synthetic_covariance_matches_the_rust_channel() {
        let c = synthetic_covariance();
        let pos_var_au = (1.0 / 149_597_870.700_f64).powi(2);
        let vel_var_au_d = (1e-6 / 149_597_870.700_f64 * 86_400.0).powi(2);
        assert_eq!(c[0][0], pos_var_au);
        assert_eq!(c[1][1], pos_var_au);
        assert_eq!(c[2][2], pos_var_au);
        assert_eq!(c[3][3], vel_var_au_d);
        assert_eq!(c[4][4], vel_var_au_d);
        assert_eq!(c[5][5], vel_var_au_d);
        // Uncorrelated: every off-diagonal is exactly zero.
        for (i, row) in c.iter().enumerate() {
            for (j, &v) in row.iter().enumerate() {
                if i != j {
                    assert_eq!(v, 0.0, "off-diagonal ({i},{j}) must be zero");
                }
            }
        }
    }

    // The restated wire covariance-kind map must match the
    // EMPYREAN_COVARIANCE_KIND_* tags (and the rust / core channels):
    // linear 0, second-order 1, mixture 3, monte-carlo 4, sigma-point 5.
    // Mutation: any swapped value turns this red.
    #[test]
    fn cov_kind_wire_matches_the_c_abi_tags() {
        assert_eq!(cov_kind_wire(CovarianceKind::Linear), 0);
        assert_eq!(cov_kind_wire(CovarianceKind::SecondOrder), 1);
        assert_eq!(cov_kind_wire(CovarianceKind::Mixture), 3);
        assert_eq!(cov_kind_wire(CovarianceKind::MonteCarlo), 4);
        assert_eq!(cov_kind_wire(CovarianceKind::SigmaPoint), 5);
    }

    // The reported `resolved_method` is the DELIVERED kind's tag — so a silent
    // substitution under an explicit request is caught by the compare.
    #[test]
    fn resolved_method_tag_names_the_delivered_kind() {
        assert_eq!(
            resolved_method_tag(CovarianceKind::SecondOrder),
            um::SECOND_ORDER
        );
        assert_eq!(resolved_method_tag(CovarianceKind::Linear), um::FIRST_ORDER);
        assert_eq!(
            resolved_method_tag(CovarianceKind::Mixture),
            um::GAUSSIAN_MIXTURE
        );
    }

    // The outcome channel is read off the per-orbit outcome, never a row
    // count: a delivered orbit whose covariance was withheld reads
    // `cov_withheld:<reason>`, and a failed orbit names its failure variant.
    #[test]
    fn outcome_channel_reports_withheld_and_failure_by_name() {
        let delivered = OrbitOutcome::Delivered {
            first_row: 0,
            num_rows: 1,
        };
        assert_eq!(
            outcome_channel(&delivered, None),
            (true, "delivered".to_string())
        );
        assert_eq!(
            outcome_channel(&delivered, Some("no cov")),
            (true, "cov_withheld:no cov".to_string())
        );
        let failed = OrbitOutcome::Failed {
            code: 1,
            message: "boom".to_string(),
        };
        assert_eq!(
            outcome_channel(&failed, None),
            (false, "failed:integration".to_string())
        );
    }

    // The rendered products line carries the method tag and the per-method
    // fields as single tokens; absent scalars render `na`. Mutation: a render
    // that dropped `resolved_method` (or emitted first_order for a second-order
    // row) turns the token assertion red.
    #[test]
    fn products_render_carries_the_method_and_fields() {
        let p = Products {
            resolved_method: Some(um::SECOND_ORDER),
            cov_kind: Some(1),
            cov_joint_width: Some(6),
            cov_tri: Some(vec![1.0, 0.0, 1.0]),
            orbit_delivered: true,
            orbit_status: "delivered".to_string(),
            mix: None,
        };
        let s = p.render();
        assert!(s.contains("resolved_method=second_order"), "{s}");
        assert!(s.contains("cov_kind=1"), "{s}");
        assert!(s.contains("cov_joint_width=6"), "{s}");
        assert!(s.contains("orbit_delivered=1"), "{s}");
        assert!(s.contains("orbit_status=delivered"), "{s}");
        // An unsplit row carries no mixture tally: the six mix_* read `na`.
        assert!(s.contains("mix_n_components_total=na"), "{s}");
        assert!(s.contains("mix_n_sky_linearization_refused=na"), "{s}");
        // Every value is a single whitespace-free token.
        assert!(!s.contains("resolved_method= "), "{s}");
    }

    // An f64 (covariance-free) row renders every covariance field `na` and the
    // orbit_delivered flag from the outcome — never a fabricated covariance.
    #[test]
    fn products_render_f64_row_is_all_na() {
        let p = Products {
            resolved_method: None,
            cov_kind: None,
            cov_joint_width: None,
            cov_tri: None,
            orbit_delivered: true,
            orbit_status: "delivered".to_string(),
            mix: None,
        };
        let s = p.render();
        assert!(s.contains("resolved_method=na"), "{s}");
        assert!(s.contains("cov_kind=na"), "{s}");
        assert!(s.contains("cov_tri=na"), "{s}");
    }
}
