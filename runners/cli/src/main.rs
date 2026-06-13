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

use clap::{Parser, ValueEnum};
use std::time::Instant;

use empyrean::{
    Context, CoordinateState, Epoch, EphemerisConfig, ForceModelTier, Frame, ODConfig, Orbit,
    Origin, PropagationConfig, Representation, UncertaintyMethod,
};

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
        return Err(format!("expected 3 comma-separated floats, got {}", parts.len()));
    }
    let v: Result<Vec<f64>, _> = parts.iter().map(|p| p.parse::<f64>()).collect();
    let v = v.map_err(|e| e.to_string())?;
    Ok([v[0], v[1], v[2]])
}

fn parse_quintuple(s: &str) -> Result<[f64; 5], String> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 5 {
        return Err(format!("expected 5 comma-separated floats, got {}", parts.len()));
    }
    let v: Result<Vec<f64>, _> = parts.iter().map(|p| p.parse::<f64>()).collect();
    let v = v.map_err(|e| e.to_string())?;
    Ok([v[0], v[1], v[2], v[3], v[4]])
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

fn parse_prop_args(rest: &str) -> Result<(Orbit, ForceModelTier, Epoch), String> {
    // Layout (matches runner.c handle_prop): epoch x y z vx vy vz
    // a1 a2 a3 g0 g1 g2 g3 g4 force_model target non_grav_dt  (18 fields)
    let f: Vec<f64> = rest
        .split_whitespace()
        .map(|t| t.parse::<f64>().map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    if f.len() != 18 {
        return Err(format!("prop_parse_{}_fields", f.len()));
    }
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
        covariance: None,
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
    Ok((orbit, tier_from_int(force_model), target))
}

fn daemon_prop(ctx: &Context, rest: &str, warmed_up: &mut bool) -> Result<String, String> {
    let (orbit, force, target) = parse_prop_args(rest)?;
    let cfg = PropagationConfig {
        force_model: force,
        uncertainty_method: UncertaintyMethod::FirstOrder,
        frame: Frame::ICRF,
        // Daemon mode also single-threaded: each call is one orbit, so
        // a Rayon pool buys nothing and avoids contention if multiple
        // daemons are run side-by-side.
        num_threads: std::num::NonZeroUsize::new(1),
        ..PropagationConfig::default()
    };

    if !*warmed_up {
        for _ in 0..5 {
            let _ = ctx.propagate(&[orbit.clone()], &[target], &cfg);
        }
        *warmed_up = true;
        eprintln!("warmup done");
    }

    let mut best_ms = f64::INFINITY;
    let mut last_pos = [0.0f64; 3];
    let mut last_vel = [0.0f64; 3];
    for _ in 0..3 {
        let t0 = Instant::now();
        let result = ctx.propagate(&[orbit.clone()], &[target], &cfg).map_err(|e| e.to_string())?;
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        if let Some(s) = result.states.first() {
            last_pos = s.position;
            last_vel = s.velocity;
        }
        if ms < best_ms {
            best_ms = ms;
        }
    }
    Ok(format!(
        "ok {:.18e} {:.18e} {:.18e} {:.18e} {:.18e} {:.18e} {:.6}",
        last_pos[0], last_pos[1], last_pos[2], last_vel[0], last_vel[1], last_vel[2], best_ms,
    ))
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
    let observers = ctx.get_observers(&[obs_code], &[target]).map_err(|e| e.to_string())?;
    let mut cfg = EphemerisConfig::with_force_model(force);
    cfg.propagation.num_threads = std::num::NonZeroUsize::new(1);
    let _ = ctx.generate_ephemeris(&[orbit.clone()], &observers, &cfg);
    let mut best_ms = f64::INFINITY;
    let mut last_ra = f64::NAN;
    let mut last_dec = f64::NAN;
    let mut last_rho = f64::NAN;
    let mut last_lt = f64::NAN;
    for _ in 0..3 {
        let t0 = Instant::now();
        let entries = ctx.generate_ephemeris(&[orbit.clone()], &observers, &cfg).map_err(|e| e.to_string())?;
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        if let Some(e) = entries.first() {
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
    let force_model: i32 = force_str.parse().map_err(|_| "od_parse_force".to_string())?;
    let force = tier_from_int(force_model);
    let (exclude_str, path) = after_force
        .trim_start()
        .split_once(char::is_whitespace)
        .ok_or_else(|| "od_parse_exclude".to_string())?;
    let exclude_naif: i32 = exclude_str.parse().map_err(|_| "od_parse_exclude".to_string())?;
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
        excluded_perturbers,
        ..ODConfig::default()
    };
    let t0 = Instant::now();
    let result = ctx.determine(&observations, None, &cfg).map_err(|e| e.to_string())?;
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    // `DetermineResult.orbit` is now a re-feedable `Orbit`; take the flat
    // state snapshot for the position/velocity output.
    let s = result.state();
    Ok(format!(
        "ok {:.18e} {:.18e} {:.18e} {:.18e} {:.18e} {:.18e} {} {:.6}",
        s.position[0],
        s.position[1],
        s.position[2],
        s.velocity[0],
        s.velocity[1],
        s.velocity[2],
        result.iterations,
        ms,
    ))
}

fn build_orbit(cli: &Cli) -> Orbit {
    let pos = cli.pos.expect("--pos required for prop/eph");
    let vel = cli.vel.expect("--vel required for prop/eph");
    let state = CoordinateState {
        epoch: Epoch::from_mjd_tdb(cli.epoch.expect("--epoch required for prop/eph")),
        elements: [pos[0], pos[1], pos[2], vel[0], vel[1], vel[2]],
        covariance: None,
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
    let orbit = build_orbit(cli);
    let target = Epoch::from_mjd_tdb(cli.target.expect("--target required for prop"));
    let cfg = PropagationConfig {
        force_model: force,
        uncertainty_method: UncertaintyMethod::FirstOrder,
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
        let _ = ctx.propagate(&[orbit.clone()], &[target], &cfg);
    }

    // Best-of-3 — keeps timing comparable to rust/python/c channels.
    let mut best_ms = f64::INFINITY;
    let mut last_pos = [0.0f64; 3];
    let mut last_vel = [0.0f64; 3];
    for _ in 0..3 {
        let t0 = Instant::now();
        let result = ctx.propagate(&[orbit.clone()], &[target], &cfg)?;
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        if let Some(s) = result.states.first() {
            last_pos = s.position;
            last_vel = s.velocity;
        }
        if ms < best_ms {
            best_ms = ms;
        }
    }

    // Output: x y z vx vy vz time_ms
    println!(
        "{:.18e} {:.18e} {:.18e} {:.18e} {:.18e} {:.18e} {:.6}",
        last_pos[0], last_pos[1], last_pos[2], last_vel[0], last_vel[1], last_vel[2], best_ms,
    );
    Ok(())
}

fn run_eph(
    ctx: &Context,
    cli: &Cli,
    force: ForceModelTier,
) -> Result<(), Box<dyn std::error::Error>> {
    let orbit = build_orbit(cli);
    let target = Epoch::from_mjd_tdb(cli.target.expect("--target required for eph"));
    let obs_code = cli.observer.as_deref().expect("--observer required for eph");

    // Resolve observer at the target epoch.
    let observers = ctx.get_observers(&[obs_code], &[target])?;

    let mut cfg = EphemerisConfig::with_force_model(force);
    // Per-row fork-exec runner — pin to 1 thread (see run_prop comment).
    cfg.propagation.num_threads = std::num::NonZeroUsize::new(1);

    // Best-of-3 with one warm-up.
    let _ = ctx.generate_ephemeris(&[orbit.clone()], &observers, &cfg);
    let mut best_ms = f64::INFINITY;
    let mut last_ra = f64::NAN;
    let mut last_dec = f64::NAN;
    let mut last_rho = f64::NAN;
    let mut last_lt = f64::NAN;
    for _ in 0..3 {
        let t0 = Instant::now();
        let entries = ctx.generate_ephemeris(&[orbit.clone()], &observers, &cfg)?;
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        if let Some(e) = entries.first() {
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
    let result = ctx.determine(&observations, None, &cfg)?;
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
