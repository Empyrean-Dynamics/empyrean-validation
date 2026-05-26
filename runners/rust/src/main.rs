//! `validate` — empyrean wrapper's per-channel validation runner.
//!
//! Runs the propagation / ephemeris / orbit-determination test cases
//! through [`empyrean::Context`] (the safe Rust wrapper over the C
//! ABI — same path the Python / C / CLI channels ride) and emits a
//! per-channel [`ValidationResult`] JSON tagged `channel: "rust"`.
//!
//! Channel-agnostic meta operations (merge-external, report,
//! ci-check) live in the `empyrean-validation` CLI; this binary only
//! does per-row replay.
//!
//! [`ValidationResult`]: empyrean_validation::schema::ValidationResult

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use villeneuve::io::cache::DiskCache;

use empyrean::Context;

mod runner;

#[derive(Parser, Debug)]
#[command(
    name = "validate",
    about = "Run empyrean-wrapper-channel propagation / ephemeris / OD validation against JPL Horizons."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run propagation + ephemeris validation against Horizons.
    Run(RunArgs),
    /// Run orbit-determination validation against MPC PSV fixtures.
    Od(OdArgs),
}

#[derive(Parser, Debug)]
struct RunArgs {
    /// Output JSON path.
    #[arg(short, long, default_value = "results/validation_rust.json")]
    output: PathBuf,
    /// Subset of object names to run (empty = all).
    #[arg(long, value_delimiter = ',')]
    only: Vec<String>,
    /// Force model tiers to evaluate.
    #[arg(long, value_delimiter = ',', default_values_t = ["standard".to_string()])]
    tiers: Vec<String>,
    /// Number of timing runs per propagation.
    #[arg(long, default_value_t = 3)]
    n_timing_runs: usize,
    /// Worker thread count (0 = rayon default).
    #[arg(long)]
    threads: Option<usize>,
    /// Empyrean data directory; defaults to ~/.empyrean/data/.
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Disk-cache directory for SBDB / Horizons queries.
    #[arg(long, default_value = "~/.empyrean/cache")]
    cache_dir: PathBuf,
    /// Skip attaching a synthetic Cartesian covariance to the input
    /// orbits. By default each test orbit carries a typical-NEO
    /// covariance (1 km position σ, 1 mm/s velocity σ) so the
    /// propagator dispatches to its Jet1 STM path — the path real users
    /// hit because empyrean is uncertainty-first by design. Setting this
    /// flag drops covariance and exercises the f64-only path for
    /// like-for-like timing comparisons against external propagators
    /// (kete, ASSIST) that don't propagate uncertainty.
    #[arg(long)]
    no_covariance: bool,
}

#[derive(Parser, Debug)]
struct OdArgs {
    /// Output JSON path.
    #[arg(short, long, default_value = "results/validation_rust_od.json")]
    output: PathBuf,
    /// Subset of object names (empty = all that have a PSV fixture).
    #[arg(long, value_delimiter = ',')]
    only: Vec<String>,
    /// Force model tier.
    #[arg(long, default_value = "standard")]
    tier: String,
    /// Max DC iterations. Default 100 matches scott's library default
    /// so all five validation channels (rust / python / c / cli / core)
    /// use the same iteration cap.
    #[arg(long, default_value_t = 100)]
    max_iterations: u32,
    /// PSV fixtures directory.
    #[arg(long, default_value = "../../fixtures/psv")]
    fixtures_dir: PathBuf,
    /// Empyrean data directory.
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Disk-cache directory for the SBDB orbit-comparison sidecar query.
    #[arg(long, default_value = "~/.empyrean/cache")]
    cache_dir: PathBuf,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.command {
        Command::Run(args) => run(args),
        Command::Od(args) => od(args),
    }
}

fn od(args: OdArgs) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("Loading empyrean context...");
    let ctx = Context::from_data_dir(args.data_dir.as_deref())?;

    let all_objects = empyrean_validation::catalog::all_objects();
    let selected: Vec<&empyrean_validation::catalog::ValidationObject> = if args.only.is_empty() {
        all_objects
    } else {
        all_objects
            .into_iter()
            .filter(|o| args.only.iter().any(|n| n == o.name))
            .collect()
    };
    if selected.is_empty() {
        eprintln!("No matching objects.");
        return Ok(());
    }
    let tier = match args.tier.as_str() {
        "approximate" => empyrean::ForceModelTier::Approximate,
        "basic" => empyrean::ForceModelTier::Basic,
        _ => empyrean::ForceModelTier::Standard,
    };

    let sbdb_cache_dir = expand_tilde(&args.cache_dir).join("sbdb");
    std::fs::create_dir_all(&sbdb_cache_dir)?;
    let runner::OdValidationOutput {
        results,
        captured_orbits,
        orbit_comparisons,
    } = runner::run_od_validation(
        &ctx,
        &selected,
        &args.fixtures_dir,
        args.max_iterations,
        tier,
        Some(&sbdb_cache_dir),
    );

    if let Some(parent) = args.output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(&results)?;
    std::fs::write(&args.output, json)?;
    eprintln!(
        "\nWrote {} OD results to {}",
        results.len(),
        args.output.display()
    );

    // Sidecar 1: per-object fitted + propagated orbit + covariance for
    // the orbit-comparison panel.
    let orbits_path = sidecar_path(&args.output, "_orbits.jsonl");
    write_jsonl(&orbits_path, &captured_orbits)?;
    eprintln!(
        "Wrote {} captured orbits to {}",
        captured_orbits.len(),
        orbits_path.display()
    );

    // Sidecar 2: bidirectional (scott vs SBDB / find_orb) Mahalanobis
    // comparisons in Keplerian element space.
    let cmp_path = sidecar_path(&args.output, "_compare.jsonl");
    write_jsonl(&cmp_path, &orbit_comparisons)?;
    eprintln!(
        "Wrote {} orbit comparisons to {}",
        orbit_comparisons.len(),
        cmp_path.display()
    );

    Ok(())
}

fn sidecar_path(main_output: &std::path::Path, suffix: &str) -> PathBuf {
    let stem = main_output
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("validation");
    let parent = main_output.parent().unwrap_or(std::path::Path::new("."));
    parent.join(format!("{stem}{suffix}"))
}

fn write_jsonl<T: serde::Serialize>(
    path: &std::path::Path,
    rows: &[T],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut s = String::new();
    for row in rows {
        s.push_str(&serde_json::to_string(row)?);
        s.push('\n');
    }
    std::fs::write(path, s)?;
    Ok(())
}

fn run(args: RunArgs) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("Loading empyrean context...");
    let ctx = Context::from_data_dir(args.data_dir.as_deref())?;

    let cache_dir = expand_tilde(&args.cache_dir);
    let mut sbdb_cache = DiskCache::new(cache_dir.join("sbdb"));
    let mut horizons_cache = DiskCache::new(cache_dir.join("horizons"));

    let all_objects = empyrean_validation::catalog::all_objects();
    let selected: Vec<&empyrean_validation::catalog::ValidationObject> = if args.only.is_empty() {
        all_objects
    } else {
        all_objects
            .into_iter()
            .filter(|o| args.only.iter().any(|n| n == o.name))
            .collect()
    };
    if selected.is_empty() {
        eprintln!("No matching objects.");
        return Ok(());
    }

    let config = runner::ValidateConfig {
        tiers: args.tiers,
        n_timing_runs: args.n_timing_runs,
        attach_covariance: !args.no_covariance,
    };

    let results = runner::run_propagation_validation(
        &ctx,
        &selected,
        &config,
        &mut horizons_cache,
        &mut sbdb_cache,
        args.threads,
    );

    if let Some(parent) = args.output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(&results)?;
    std::fs::write(&args.output, json)?;
    eprintln!(
        "\nWrote {} results to {}",
        results.len(),
        args.output.display()
    );
    Ok(())
}

fn expand_tilde(p: &std::path::Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    p.to_path_buf()
}
