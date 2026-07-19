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
    /// layup per-channel JSON (from `runners/layup/run_layup.py`).
    /// Folds OD-row fields (χ² / reduced-χ² / observation count /
    /// convergence) onto matching rows.
    #[arg(long)]
    layup: Option<PathBuf>,
    /// JPL SBDB cache directory (e.g. `$CACHE_DIR/sbdb`). Reads each OD
    /// object's cached SBDB response and folds JPL's own reported fit
    /// quality (normalized RMS, n_obs_used, radar counts, data-arc,
    /// condition code, provenance) onto its OD rows as the `ref_od_*`
    /// reference — making JPL a full OD tool alongside find_orb / layup.
    #[arg(long)]
    jpl_sbdb_cache: Option<PathBuf>,
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
    if let Some(path) = &args.layup {
        let n = merge_layup(&mut rows, path)?;
        eprintln!("Merged {n} layup rows");
    }
    if let Some(dir) = &args.jpl_sbdb_cache {
        let n = merge_jpl(&mut rows, dir)?;
        eprintln!("Merged {n} JPL SBDB OD-reference rows");
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

/// Fold layup per-channel JSON into the OD rows.
///
/// layup covers orbit determination only (independent MIT-licensed,
/// ASSIST-backed fitter; Smithsonian / CfA). Keyed by object — same as
/// `merge_orbfit` — since OD rows have one fit per object per arc. layup
/// reports a weighted χ² (not an arcsec RMS), so the folded fields are
/// χ² / reduced-χ² / observation count / convergence.
fn merge_layup(
    rows: &mut [ValidationResult],
    path: &std::path::Path,
) -> Result<usize, Box<dyn std::error::Error>> {
    let txt = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let lu: Vec<serde_json::Value> = serde_json::from_str(&txt)?;
    let mut idx: std::collections::HashMap<String, &serde_json::Value> = Default::default();
    for f in &lu {
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
        r.layup_chi2 = f["layup_chi2"].as_f64();
        r.layup_reduced_chi2 = f["layup_reduced_chi2"].as_f64();
        r.layup_n_obs_used = f["layup_n_obs_used"].as_u64().map(|v| v as u32);
        r.layup_converged = f["layup_converged"].as_bool();
        r.layup_time_ms = f["layup_time_ms"].as_f64();
        n += 1;
    }
    Ok(n)
}

/// JPL's own reported orbit-solution quality, parsed from a cached SBDB
/// response. Fields mirror the SBDB `orbit` block; SBDB reports several as
/// JSON strings, so parsing is number-or-string tolerant.
#[derive(Clone)]
struct SbdbOdRef {
    rms: Option<f64>,
    n_obs_used: Option<u32>,
    n_del_obs_used: Option<u32>,
    n_dop_obs_used: Option<u32>,
    data_arc_days: Option<u32>,
    condition_code: Option<u8>,
    soln_date: Option<String>,
    pe_used: Option<String>,
    sb_used: Option<String>,
}

/// Read `{sbdb_cache_dir}/{sbdb_query with spaces→underscores}.json` and
/// extract the JPL fit quality from `response.orbit`. Returns `None` if the
/// cache file is absent or has no orbit block (the object is simply skipped).
fn read_sbdb_od_ref(sbdb_cache_dir: &std::path::Path, sbdb_query: &str) -> Option<SbdbOdRef> {
    let fname = format!("{}.json", sbdb_query.replace(' ', "_"));
    let txt = std::fs::read_to_string(sbdb_cache_dir.join(fname)).ok()?;
    let d: serde_json::Value = serde_json::from_str(&txt).ok()?;
    let o = &d["response"]["orbit"];
    if !o.is_object() {
        return None;
    }
    let f64_of = |v: &serde_json::Value| {
        v.as_f64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
    };
    let u32_of = |v: &serde_json::Value| {
        v.as_u64()
            .map(|x| x as u32)
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
    };
    let u8_of = |v: &serde_json::Value| {
        v.as_u64()
            .map(|x| x as u8)
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
    };
    let str_of = |v: &serde_json::Value| v.as_str().map(|s| s.to_string());
    Some(SbdbOdRef {
        rms: f64_of(&o["rms"]),
        n_obs_used: u32_of(&o["n_obs_used"]),
        n_del_obs_used: u32_of(&o["n_del_obs_used"]),
        n_dop_obs_used: u32_of(&o["n_dop_obs_used"]),
        data_arc_days: u32_of(&o["data_arc"]),
        condition_code: u8_of(&o["condition_code"]),
        soln_date: str_of(&o["soln_date"]),
        pe_used: str_of(&o["pe_used"]),
        sb_used: str_of(&o["sb_used"]),
    })
}

/// Fold JPL's SBDB fit quality onto every OD row as the `ref_od_*` reference.
/// Objects are matched to their SBDB record via the catalog's `sbdb_query`;
/// each cache file is read at most once. `ref_od_reduced_chi2` is `rms²`
/// (the SBDB normalized RMS ≈ √reduced-χ², so this is comparable to layup's
/// reduced χ², modulo the k-dof correction).
fn merge_jpl(
    rows: &mut [ValidationResult],
    sbdb_cache_dir: &std::path::Path,
) -> Result<usize, Box<dyn std::error::Error>> {
    let query_of: std::collections::HashMap<&str, &str> = all_objects()
        .into_iter()
        .map(|o| (o.name, o.sbdb_query))
        .collect();
    let mut cache: std::collections::HashMap<String, Option<SbdbOdRef>> = Default::default();
    let mut n = 0;
    for r in rows.iter_mut() {
        if r.test_type != "orbit_determination" {
            continue;
        }
        let Some(&query) = query_of.get(r.object.as_str()) else {
            continue;
        };
        let od = cache
            .entry(query.to_string())
            .or_insert_with(|| read_sbdb_od_ref(sbdb_cache_dir, query));
        let Some(od) = od else { continue };
        r.ref_od_rms_normalized = od.rms;
        r.ref_od_reduced_chi2 = od.rms.map(|x| x * x);
        r.ref_od_n_obs_used = od.n_obs_used;
        r.ref_od_n_del_obs_used = od.n_del_obs_used;
        r.ref_od_n_dop_obs_used = od.n_dop_obs_used;
        r.ref_od_data_arc_days = od.data_arc_days;
        r.ref_od_condition_code = od.condition_code;
        r.ref_od_soln_date = od.soln_date.clone();
        r.ref_od_pe_used = od.pe_used.clone();
        r.ref_od_sb_used = od.sb_used.clone();
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn od_row(object: &str) -> ValidationResult {
        let mut r = ValidationResult::empty();
        r.object = object.into();
        r.test_type = "orbit_determination".into();
        r
    }

    #[test]
    fn merge_layup_folds_onto_od_rows_only() {
        // A propagation row for the same object must be left untouched: layup
        // is an OD-only reference, keyed by (object, orbit_determination).
        let mut rows = vec![od_row("Eros"), {
            let mut p = ValidationResult::empty();
            p.object = "Eros".into();
            p.test_type = "propagation".into();
            p
        }];

        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(
            f,
            r#"[{{"object":"Eros","test_type":"orbit_determination",
                 "layup_chi2":10.5,"layup_reduced_chi2":1.05,
                 "layup_n_obs_used":512,"layup_converged":true,
                 "layup_time_ms":42.0}}]"#
        )
        .unwrap();

        let n = merge_layup(&mut rows, f.path()).unwrap();
        assert_eq!(n, 1, "exactly the OD row should be merged");

        let od = &rows[0];
        assert_eq!(od.layup_chi2, Some(10.5));
        assert_eq!(od.layup_reduced_chi2, Some(1.05));
        assert_eq!(od.layup_n_obs_used, Some(512));
        assert_eq!(od.layup_converged, Some(true));
        assert_eq!(od.layup_time_ms, Some(42.0));

        // Propagation row for the same object stays clear.
        assert_eq!(rows[1].layup_chi2, None);
    }

    #[test]
    fn merge_layup_skips_objects_without_a_layup_record() {
        // An OD row whose object has no layup fit stays None — no silent
        // fabrication, no cross-object leakage.
        let mut rows = vec![od_row("Bennu")];
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, r#"[{{"object":"Eros","layup_chi2":9.0}}]"#).unwrap();
        let n = merge_layup(&mut rows, f.path()).unwrap();
        assert_eq!(n, 0);
        assert_eq!(rows[0].layup_chi2, None);
    }

    /// Write a mock SBDB cache file under `dir/{query}.json` for `merge_jpl`.
    fn write_sbdb(dir: &std::path::Path, query: &str, orbit_json: &str) {
        std::fs::write(
            dir.join(format!("{}.json", query.replace(' ', "_"))),
            format!(r#"{{"response":{{"orbit":{orbit_json}}}}}"#),
        )
        .unwrap();
    }

    #[test]
    fn merge_jpl_folds_sbdb_fit_quality_onto_od_rows() {
        // SBDB reports several fields as JSON strings (rms, data_arc,
        // condition_code); the merge must parse them, and reduced_chi2 = rms².
        let dir = tempfile::tempdir().unwrap();
        write_sbdb(
            dir.path(),
            "Apophis",
            r#"{"rms":".28","n_obs_used":7370,"n_del_obs_used":20,
                "n_dop_obs_used":30,"data_arc":"6599","condition_code":"0",
                "soln_date":"2024-01-01 00:00:00","pe_used":"DE441","sb_used":"SB441-N16"}"#,
        );
        let mut rows = vec![od_row("Apophis"), {
            let mut p = ValidationResult::empty();
            p.object = "Apophis".into();
            p.test_type = "propagation".into();
            p
        }];
        let n = merge_jpl(&mut rows, dir.path()).unwrap();
        assert_eq!(n, 1, "only the OD row is merged");

        let od = &rows[0];
        assert_eq!(od.ref_od_rms_normalized, Some(0.28));
        assert_eq!(od.ref_od_reduced_chi2, Some(0.28 * 0.28)); // rms²
        assert_eq!(od.ref_od_n_obs_used, Some(7370));
        assert_eq!(od.ref_od_n_del_obs_used, Some(20));
        assert_eq!(od.ref_od_n_dop_obs_used, Some(30));
        assert_eq!(od.ref_od_data_arc_days, Some(6599));
        assert_eq!(od.ref_od_condition_code, Some(0));
        assert_eq!(od.ref_od_pe_used.as_deref(), Some("DE441"));
        // Propagation row for the same object stays clear.
        assert_eq!(rows[1].ref_od_n_obs_used, None);
    }

    #[test]
    fn merge_jpl_handles_space_in_query_and_skips_missing() {
        // "2020 AV2" resolves to cache file "2020_AV2.json"; an object whose
        // cache file is absent is silently skipped (no fabrication).
        let dir = tempfile::tempdir().unwrap();
        write_sbdb(dir.path(), "2020 AV2", r#"{"rms":".5","n_obs_used":402}"#);
        let mut rows = vec![od_row("2020 AV2"), od_row("Bennu")];
        let n = merge_jpl(&mut rows, dir.path()).unwrap();
        assert_eq!(n, 1);
        assert_eq!(rows[0].ref_od_n_obs_used, Some(402));
        assert_eq!(rows[0].ref_od_reduced_chi2, Some(0.25));
        assert_eq!(rows[1].ref_od_n_obs_used, None); // no cache file for Bennu
    }
}
