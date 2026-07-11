//! `empyrean-validation` CLI — channel-agnostic meta operations.
//!
//! Subcommands:
//!
//! - [`plan`](PlanArgs) — generate a [`ValidationPlan`] JSON using the
//!   shared [`crate::catalog`] and [`crate::plan::build_plan`]. Output
//!   is the canonical fixture every per-channel runner consumes.
//! - [`merge-external`](MergeExternalArgs) — fold ASSIST and find_orb
//!   per-channel JSONs into a base channel JSON (typically the rust
//!   runner's), populating the `assist_*` / `findorb_*` fields on each
//!   row so the report can show 3-way comparisons.
//! - [`report`](ReportArgs) — render an HTML validation report from
//!   one or more channel JSONs (rust must be present as the reference
//!   channel) via [`crate::report::generate_report`].
//! - [`ci-check`](CiCheckArgs) — read the CI summary JSON written by
//!   `report --summary` and exit non-zero if any channel in
//!   `--strict-channels` failed binding fidelity at 1e-10.
//!
//! Per-channel runners (the binaries that actually execute
//! propagation / ephemeris / OD against a specific empyrean
//! distribution layer or external reference) live next to that
//! channel's own crate / package — `empyrean-core/src/bin/validate.rs`,
//! `empyrean-validation/runners/assist/run_assist.py`, etc. This CLI
//! never invokes them; it only handles the channel-agnostic glue.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use empyrean_validation::{
    catalog::{ValidationObject, all_objects, filter_by_name, filter_by_population},
    plan::{PlanConfig, build_plan},
    report::generate_report,
    schema::{OrbitComparison, ValidationResult},
};

#[derive(Parser, Debug)]
#[command(
    name = "empyrean-validation",
    version,
    about = "Cross-channel validation framework — plan generator, external merge, report renderer, CI gate"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Generate a validation plan JSON (test grid + IC + Horizons references).
    Plan(PlanArgs),
    /// Merge ASSIST / find_orb per-channel JSONs into a rust-channel JSON.
    MergeExternal(MergeExternalArgs),
    /// Render an HTML report from one or more channel JSONs.
    Report(ReportArgs),
    /// Read a CI summary JSON and exit non-zero on fidelity failures.
    CiCheck(CiCheckArgs),
}

#[derive(Parser, Debug)]
struct PlanArgs {
    /// Output JSON path.
    #[arg(short, long, default_value = "results/validation_plan.json")]
    output: PathBuf,
    /// Subset of object names to include (case-insensitive). Empty = all.
    #[arg(long, value_delimiter = ',')]
    only: Vec<String>,
    /// Subset of populations to include. Empty = all.
    #[arg(long, value_delimiter = ',')]
    populations: Vec<String>,
    /// Force model tiers to evaluate (one row per tier per object/dt).
    #[arg(long, value_delimiter = ',', default_values_t = ["standard".to_string()])]
    tiers: Vec<String>,
    /// Emit both first-order-with-cov and f64-no-cov rows so the
    /// report can axis on uncertainty method. Set `false` to emit
    /// f64-no-cov only.
    #[arg(long, default_value_t = true)]
    uncertainty_axis: bool,
    /// Disk-cache directory for SBDB / Horizons queries.
    #[arg(long, default_value = "~/.empyrean/cache")]
    cache_dir: PathBuf,
}

#[derive(Parser, Debug)]
struct MergeExternalArgs {
    /// Base channel JSON (usually the rust runner's
    /// `validation_rust.json`) — its `assist_*` / `findorb_*` fields
    /// are populated in place.
    #[arg(short, long)]
    input: PathBuf,
    /// Output JSON path. Defaults to overwriting `--input`.
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// ASSIST per-channel JSON (from `runners/assist/run_assist.py`).
    #[arg(long)]
    assist: Option<PathBuf>,
    /// find_orb per-channel JSON (from `runners/findorb/run_findorb.py`).
    #[arg(long)]
    findorb: Option<PathBuf>,
    /// find_orb radar-augmented JSON (the second pass over `fixtures/psv-radar/`,
    /// run with `--test-type orbit_determination_radar`). Its rows attach to the
    /// `orbit_determination_radar` OD rows.
    #[arg(long)]
    findorb_radar: Option<PathBuf>,
    /// OpenOrb (oorb) per-channel JSON (from `runners/oorb/run_oorb.py`).
    /// Folds propagation + ephemeris fields onto matching rust rows.
    #[arg(long)]
    oorb: Option<PathBuf>,
    /// OrbFit per-channel JSON (from `runners/orbfit/run_orbfit.py`).
    /// Folds OD-row fields (RMS + observation counts) onto matching
    /// rust rows.
    #[arg(long)]
    orbfit: Option<PathBuf>,
}

#[derive(Parser, Debug)]
struct ReportArgs {
    /// One or more JSON files of `ValidationResult` rows. Repeat the
    /// flag or comma-separate. The `rust` channel must be present in
    /// at least one input as the reference.
    #[arg(short, long, value_delimiter = ',', required = true)]
    results: Vec<PathBuf>,
    /// Output HTML path.
    #[arg(short, long, default_value = "results/validation_report.html")]
    output: PathBuf,
    /// Optional CI summary JSON: per-channel pass count + max diffs.
    /// Use `ci-check` to gate a CI workflow on the contents.
    #[arg(long)]
    summary: Option<PathBuf>,
}

#[derive(Parser, Debug)]
struct CiCheckArgs {
    /// CI summary JSON written by `report --summary`.
    #[arg(short, long)]
    summary: PathBuf,
    /// Channels that must pass binding fidelity (1e-10) on every row.
    /// Comma-separated. Channels not in this list are surfaced in the
    /// report but don't fail CI.
    #[arg(long, value_delimiter = ',', default_values_t = ["c".to_string(), "cli".to_string(), "python".to_string()])]
    strict_channels: Vec<String>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.command {
        Command::Plan(args) => plan(args),
        Command::MergeExternal(args) => merge_external(args),
        Command::Report(args) => report(args),
        Command::CiCheck(args) => ci_check(args),
    }
}

fn plan(args: PlanArgs) -> Result<(), Box<dyn std::error::Error>> {
    // Resolve object subset. The catalog filters take `&[&str]`, so
    // borrow the args' Strings into a temporary Vec.
    let pool: Vec<&ValidationObject> = if args.populations.is_empty() {
        all_objects()
    } else {
        let pops: Vec<&str> = args.populations.iter().map(String::as_str).collect();
        filter_by_population(&pops)
    };
    let selected: Vec<&ValidationObject> = if args.only.is_empty() {
        pool
    } else {
        let names: Vec<&str> = args.only.iter().map(String::as_str).collect();
        let by_name = filter_by_name(&names);
        pool.into_iter()
            .filter(|o| by_name.iter().any(|m| m.name == o.name))
            .collect()
    };
    if selected.is_empty() {
        return Err("no matching objects after filters".into());
    }
    eprintln!(
        "Building plan for {} objects, tiers={:?}, uncertainty_axis={}",
        selected.len(),
        args.tiers,
        args.uncertainty_axis
    );

    let cache_dir = expand_tilde(&args.cache_dir);
    let sbdb_cache_dir = cache_dir.join("sbdb");
    let horizons_cache_dir = cache_dir.join("horizons");

    let config = PlanConfig {
        tiers: args.tiers,
        uncertainty_axis: args.uncertainty_axis,
    };

    let plan = build_plan(&selected, &config, &sbdb_cache_dir, &horizons_cache_dir);

    if let Some(parent) = args.output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&args.output, serde_json::to_string_pretty(&plan)?)?;
    eprintln!(
        "Wrote {} plan rows to {}",
        plan.len(),
        args.output.display()
    );
    Ok(())
}

fn merge_external(args: MergeExternalArgs) -> Result<(), Box<dyn std::error::Error>> {
    let raw = std::fs::read_to_string(&args.input)
        .map_err(|e| format!("read {}: {e}", args.input.display()))?;
    let mut rows: Vec<ValidationResult> = serde_json::from_str(&raw)?;

    if let Some(path) = &args.assist {
        let n = merge_assist(&mut rows, path)?;
        eprintln!("Merged {n} ASSIST rows");
    }
    if let Some(path) = &args.findorb {
        let n = merge_findorb(&mut rows, path)?;
        eprintln!("Merged {n} find_orb rows");
    }
    if let Some(path) = &args.findorb_radar {
        let n = merge_findorb(&mut rows, path)?;
        eprintln!("Merged {n} find_orb radar rows");
    }
    if let Some(path) = &args.oorb {
        let n = merge_oorb(&mut rows, path)?;
        eprintln!("Merged {n} OpenOrb rows");
    }
    if let Some(path) = &args.orbfit {
        let n = merge_orbfit(&mut rows, path)?;
        eprintln!("Merged {n} OrbFit rows");
    }

    let out_path = args.output.unwrap_or(args.input);
    std::fs::write(&out_path, serde_json::to_string_pretty(&rows)?)?;
    eprintln!("Wrote merged results to {}", out_path.display());
    Ok(())
}

fn merge_assist(
    rows: &mut [ValidationResult],
    path: &std::path::Path,
) -> Result<usize, Box<dyn std::error::Error>> {
    let txt = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let assist: Vec<serde_json::Value> = serde_json::from_str(&txt)?;
    // Key by (object, dt, propagation_uncertainty) so the f64 ASSIST row
    // attaches only to the empyrean f64_no_cov rust row, and (if the
    // runner emitted it) the STM ASSIST row attaches only to the
    // first_order_with_cov rust row. Without the uncertainty axis in
    // the key, the same ASSIST timing landed on both empyrean modes —
    // making the timing chart compare empyrean's STM-bearing Jet1 path
    // against ASSIST's f64 path on the same axis.
    let mut idx: std::collections::HashMap<(String, i64, Option<String>), &serde_json::Value> =
        Default::default();
    for a in &assist {
        if let (Some(o), Some(d)) = (a["object"].as_str(), a["dt_days"].as_f64()) {
            let unc = a["propagation_uncertainty"].as_str().map(str::to_string);
            idx.insert((o.to_string(), d as i64, unc), a);
        }
    }
    let mut n = 0;
    for r in rows.iter_mut() {
        if r.test_type != "propagation" {
            continue;
        }
        // For empyrean's "auto" rows (UncertaintyMethod::Auto), pair
        // against ASSIST's STM row — the closest analogue REBOUND
        // offers. Auto in well-behaved regimes resolves to FirstOrder
        // (matches STM), and in high-κ regimes escalates to
        // SecondOrder or AGM mixture (no REBOUND analogue at all). STM
        // is the strongest available comparison baseline.
        let assist_uncertainty = match r.propagation_uncertainty.as_deref() {
            Some("auto") => Some("first_order_with_cov".to_string()),
            other => other.map(str::to_string),
        };
        let key = (r.object.clone(), r.dt_days as i64, assist_uncertainty);
        let Some(a) = idx.get(&key) else { continue };
        r.assist_vs_horizons_km = a["assist_vs_horizons_km"].as_f64();
        r.assist_time_ms = a["assist_time_ms"].as_f64();
        if let (Some(emp), Some(arr)) = (&r.emp_pos_au, a["assist_pos_au"].as_array())
            && arr.len() == 3
        {
            let ast = [
                arr[0].as_f64().unwrap_or(0.0),
                arr[1].as_f64().unwrap_or(0.0),
                arr[2].as_f64().unwrap_or(0.0),
            ];
            let dx = emp[0] - ast[0];
            let dy = emp[1] - ast[1];
            let dz = emp[2] - ast[2];
            r.emp_vs_assist_km =
                Some((dx * dx + dy * dy + dz * dz).sqrt() * empyrean_validation::compare::AU_KM);
        }
        if let (Some(emp), Some(ast)) = (r.emp_time_ms, r.assist_time_ms)
            && ast > 0.0
        {
            r.speed_ratio = Some(emp / ast);
        }
        n += 1;
    }
    Ok(n)
}

fn merge_findorb(
    rows: &mut [ValidationResult],
    path: &std::path::Path,
) -> Result<usize, Box<dyn std::error::Error>> {
    let txt = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let fo: Vec<serde_json::Value> = serde_json::from_str(&txt)?;
    // Key on (object, test_type) so the optical find_orb fit attaches to the
    // `orbit_determination` row and the radar-augmented fit (find_orb run over
    // the fixtures/psv-radar/ files, which carry the ADES <radar> table)
    // attaches to the `orbit_determination_radar` row — same object, distinct
    // fit. Older single-pass find_orb files omit `test_type`; default those to
    // the optical OD for backward compatibility.
    let mut idx: std::collections::HashMap<(String, String), &serde_json::Value> =
        Default::default();
    for f in &fo {
        if let Some(o) = f["object"].as_str() {
            let tt = f["test_type"]
                .as_str()
                .unwrap_or("orbit_determination")
                .to_string();
            idx.insert((o.to_string(), tt), f);
        }
    }
    let mut n = 0;
    for r in rows.iter_mut() {
        if r.test_type != "orbit_determination" && r.test_type != "orbit_determination_radar" {
            continue;
        }
        let Some(f) = idx.get(&(r.object.clone(), r.test_type.clone())) else {
            continue;
        };
        r.findorb_rms_residual = f["fo_rms_residual"].as_f64();
        r.findorb_n_obs_used = f["fo_n_obs_used"].as_u64().map(|v| v as u32);
        r.findorb_n_obs_rejected = f["fo_n_obs_rejected"].as_u64().map(|v| v as u32);
        n += 1;
    }
    Ok(n)
}

/// Fold OpenOrb (oorb) per-channel JSON into the rust rows.
///
/// OpenOrb covers propagation + ephemeris (Granvik et al.; not OD —
/// oorb's Ranging / LSL is a multi-stage pipeline that doesn't fit the
/// per-row replay model). Keyed by (object, dt_days) on propagation
/// rows and (object, dt_days, observer) on ephemeris rows.
fn merge_oorb(
    rows: &mut [ValidationResult],
    path: &std::path::Path,
) -> Result<usize, Box<dyn std::error::Error>> {
    let txt = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let oo: Vec<serde_json::Value> = serde_json::from_str(&txt)?;
    let mut prop_idx: std::collections::HashMap<(String, i64), &serde_json::Value> =
        Default::default();
    let mut eph_idx: std::collections::HashMap<(String, i64, String), &serde_json::Value> =
        Default::default();
    for o in &oo {
        let (Some(name), Some(dt)) = (o["object"].as_str(), o["dt_days"].as_f64()) else {
            continue;
        };
        match o["test_type"].as_str() {
            Some("propagation") => {
                prop_idx.insert((name.to_string(), dt as i64), o);
            }
            Some("ephemeris") => {
                if let Some(obs) = o["observer"].as_str() {
                    eph_idx.insert((name.to_string(), dt as i64, obs.to_string()), o);
                }
            }
            _ => {}
        }
    }
    let mut n = 0;
    for r in rows.iter_mut() {
        match r.test_type.as_str() {
            "propagation" => {
                let Some(o) = prop_idx.get(&(r.object.clone(), r.dt_days as i64)) else {
                    continue;
                };
                r.oorb_vs_horizons_km = o["oorb_vs_horizons_km"].as_f64();
                r.oorb_time_ms = o["oorb_time_ms"].as_f64();
                if let (Some(emp), Some(arr)) = (&r.emp_pos_au, o["oorb_pos_au"].as_array())
                    && arr.len() == 3
                {
                    let oo_pos = [
                        arr[0].as_f64().unwrap_or(0.0),
                        arr[1].as_f64().unwrap_or(0.0),
                        arr[2].as_f64().unwrap_or(0.0),
                    ];
                    let dx = emp[0] - oo_pos[0];
                    let dy = emp[1] - oo_pos[1];
                    let dz = emp[2] - oo_pos[2];
                    r.emp_vs_oorb_km = Some(
                        (dx * dx + dy * dy + dz * dz).sqrt() * empyrean_validation::compare::AU_KM,
                    );
                }
                n += 1;
            }
            "ephemeris" => {
                let Some(obs) = r.observer.as_deref() else {
                    continue;
                };
                let key = (r.object.clone(), r.dt_days as i64, obs.to_string());
                let Some(o) = eph_idx.get(&key) else { continue };
                r.oorb_separation_arcsec = o["oorb_separation_arcsec"].as_f64();
                r.oorb_d_ra_arcsec = o["oorb_d_ra_arcsec"].as_f64();
                r.oorb_d_dec_arcsec = o["oorb_d_dec_arcsec"].as_f64();
                r.oorb_d_rho_km = o["oorb_d_rho_km"].as_f64();
                r.oorb_time_ms = o["oorb_time_ms"].as_f64();
                n += 1;
            }
            _ => {}
        }
    }
    Ok(n)
}

/// Fold OrbFit per-channel JSON into the rust rows.
///
/// OrbFit covers orbit determination only (the canonical CMC2003
/// rejection implementation; OrbFit Consortium / IAU MPC). Keyed by
/// object — same as `merge_findorb` — since OD rows have one fit per
/// object per arc.
fn merge_orbfit(
    rows: &mut [ValidationResult],
    path: &std::path::Path,
) -> Result<usize, Box<dyn std::error::Error>> {
    let txt = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let of: Vec<serde_json::Value> = serde_json::from_str(&txt)?;
    let mut idx: std::collections::HashMap<String, &serde_json::Value> = Default::default();
    for f in &of {
        if let Some(o) = f["object"].as_str() {
            idx.insert(o.to_string(), f);
        }
    }
    let mut n = 0;
    for r in rows.iter_mut() {
        if r.test_type != "orbit_determination" {
            continue;
        }
        let Some(f) = idx.get(&r.object) else {
            continue;
        };
        r.orbfit_rms_arcsec = f["orbfit_rms_arcsec"].as_f64();
        r.orbfit_n_obs_used = f["orbfit_n_obs_used"].as_u64().map(|v| v as u32);
        r.orbfit_n_obs_rejected = f["orbfit_n_obs_rejected"].as_u64().map(|v| v as u32);
        r.orbfit_time_ms = f["orbfit_time_ms"].as_f64();
        n += 1;
    }
    Ok(n)
}

fn report(args: ReportArgs) -> Result<(), Box<dyn std::error::Error>> {
    let mut all: Vec<ValidationResult> = Vec::new();
    let mut orbit_comparisons: Vec<OrbitComparison> = Vec::new();
    for path in &args.results {
        let raw =
            std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let rows: Vec<ValidationResult> =
            serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
        eprintln!(
            "Loaded {} rows from {} (channels: {})",
            rows.len(),
            path.display(),
            distinct_channels(&rows).join(", ")
        );
        all.extend(rows);

        // Pick up the orbit-comparison sidecar if it exists. Try
        // the canonical `{stem}_compare.jsonl` first, then fall back
        // to the OD-specific sibling `*_rust_od_compare.jsonl` for the
        // Makefile's merged / unified rust outputs (which inherit the
        // sidecar from the upstream `validate od` step but don't carry
        // it forward in the file name).
        for cmp_path in compare_sidecar_candidates(path) {
            if !cmp_path.exists() {
                continue;
            }
            let raw = std::fs::read_to_string(&cmp_path)
                .map_err(|e| format!("read {}: {e}", cmp_path.display()))?;
            let mut n = 0;
            for line in raw.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let c: OrbitComparison = serde_json::from_str(line)
                    .map_err(|e| format!("parse {}: {e}", cmp_path.display()))?;
                orbit_comparisons.push(c);
                n += 1;
            }
            eprintln!("Loaded {n} orbit comparisons from {}", cmp_path.display(),);
            // First sidecar wins — don't double-load if both candidates exist.
            break;
        }
    }
    if !all.iter().any(|r| r.channel == "rust") {
        return Err(
            "no rows with channel=rust found in inputs; at least one rust JSON is required as the reference"
                .into(),
        );
    }
    generate_report(
        &all,
        &orbit_comparisons,
        &args.output,
        args.summary.as_deref(),
    )
    .map_err(|e| format!("report: {e}"))?;
    eprintln!("Wrote report to {}", args.output.display());
    if let Some(p) = &args.summary {
        eprintln!("Wrote CI summary to {}", p.display());
    }
    Ok(())
}

/// Candidate sidecar paths for the orbit-comparison data associated
/// with a results JSON. The Makefile path is:
///
/// ```text
/// validate od --output validation_rust_od.json       # writes
///   ↳ validation_rust_od_compare.jsonl               # sidecar
/// jq merge → validation_rust.json                    # no sidecar emitted
/// validate merge-external → validation_rust_merged.json  # no sidecar emitted
/// ```
///
/// When the report is invoked on `validation_rust_merged.json` (or the
/// pre-merge `validation_rust.json`), the canonical
/// `{stem}_compare.jsonl` lookup misses the only sidecar that actually
/// exists, leaving §12 of the HTML report empty (empyrean-urfu). Return
/// the canonical candidate first, then the `*_rust_od_compare.jsonl`
/// fallback so report() can pick up the upstream sidecar.
fn compare_sidecar_candidates(main_path: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Some(stem) = main_path.file_stem().and_then(|s| s.to_str()) else {
        return out;
    };
    let Some(parent) = main_path.parent() else {
        return out;
    };
    // 1. Canonical: same stem with `_compare.jsonl` suffix.
    out.push(parent.join(format!("{stem}_compare.jsonl")));
    // 2. If the stem looks like a merged/unified rust output, also try
    //    the OD-specific sidecar (`_od_compare.jsonl`). Covers the
    //    `validation_rust_merged.json` and `validation_rust.json`
    //    cases produced by the Makefile.
    let od_stem = if let Some(base) = stem.strip_suffix("_rust_merged") {
        Some(format!("{base}_rust_od"))
    } else {
        stem.strip_suffix("_rust")
            .map(|base| format!("{base}_rust_od"))
    };
    if let Some(od_stem) = od_stem {
        out.push(parent.join(format!("{od_stem}_compare.jsonl")));
    }
    out
}

fn ci_check(args: CiCheckArgs) -> Result<(), Box<dyn std::error::Error>> {
    let raw = std::fs::read_to_string(&args.summary)
        .map_err(|e| format!("read {}: {e}", args.summary.display()))?;
    let summary: serde_json::Value = serde_json::from_str(&raw)?;
    let channels = summary["channels"]
        .as_array()
        .ok_or("summary missing channels array")?;

    let mut failures = Vec::new();
    for ch in channels {
        let name = ch["channel"].as_str().unwrap_or("?");
        if !args.strict_channels.iter().any(|s| s == name) {
            continue;
        }
        let passing = ch["n_passing"].as_u64().unwrap_or(0);
        let total = ch["n_total_compared"].as_u64().unwrap_or(0);
        if total == 0 {
            failures.push(format!("{name}: no rows compared"));
        } else if passing < total {
            failures.push(format!(
                "{name}: {passing}/{total} rows passed at 1e-10 (expected 100%)"
            ));
        }
    }

    if failures.is_empty() {
        eprintln!(
            "ci-check: all strict channels [{}] passed binding fidelity at 1e-10",
            args.strict_channels.join(", ")
        );
        Ok(())
    } else {
        for f in &failures {
            eprintln!("ci-check FAIL: {f}");
        }
        std::process::exit(1);
    }
}

fn distinct_channels(rows: &[ValidationResult]) -> Vec<String> {
    let mut seen: std::collections::BTreeSet<String> = Default::default();
    for r in rows {
        seen.insert(r.channel.clone());
    }
    seen.into_iter().collect()
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
