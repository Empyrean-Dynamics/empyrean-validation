//! `empyrean-validation` CLI — channel-agnostic meta operations.
//!
//! Subcommands:
//!
//! - [`plan`](PlanArgs) — generate a [`ValidationPlan`] JSON using the
//!   shared [`crate::catalog`] and [`crate::plan::build_plan`]. Output
//!   is the canonical fixture every per-channel runner consumes.
//! - [`strip-plan`](StripPlanArgs) — reduce a channel-result JSON (the
//!   rust reference's) to the canonical plan, keeping only the
//!   plan-contract keys and popping everything else.
//! - [`merge-external`](MergeExternalArgs) — fold ASSIST and find_orb
//!   per-channel JSONs into a base channel JSON (typically the rust
//!   runner's), populating the `assist_*` / `findorb_*` fields on each
//!   row so the report can show 3-way comparisons.
//! - [`report`](ReportArgs) — render an HTML validation report from
//!   one or more channel JSONs (rust must be present as the reference
//!   channel) via [`crate::report::generate_report`].
//! - [`ci-check`](CiCheckArgs) — read the CI summary JSON written by
//!   `report --summary` and exit non-zero if any channel in
//!   `--strict-channels` is absent from the summary, failed binding
//!   fidelity at 1e-10, or fell below a `--min-rows` per-axis floor.
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
    catalog::{
        RADAR_FIXTURE_OBJECTS, ValidationObject, all_objects, filter_by_name, filter_by_population,
    },
    plan::{PlanConfig, build_plan},
    report::{SUMMARY_TEST_TYPES, generate_report},
    schema::{OrbitComparison, ValidationResult, channels, test_types},
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
    /// Strip a channel-result JSON down to the canonical plan contract.
    StripPlan(StripPlanArgs),
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
struct StripPlanArgs {
    /// Channel-result JSON to derive the plan from (the rust reference's
    /// unified `validation_rust.json`).
    #[arg(short, long)]
    input: PathBuf,
    /// Output plan JSON path.
    #[arg(short, long, default_value = "results/validation_plan.json")]
    output: PathBuf,
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
    /// kete per-channel JSON (from `runners/kete/run_kete.py`). Currently
    /// folds per-row wall-clock timing only (`kete_time_ms`) — kete is not
    /// yet a selectable comparison tool in the report registry.
    #[arg(long)]
    kete: Option<PathBuf>,
    /// jorbit per-channel JSON (from `runners/jorbit/run_jorbit.py`).
    /// Folds per-row wall-clock timing only (`jorbit_time_ms`).
    #[arg(long)]
    jorbit: Option<PathBuf>,
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
    ///
    /// A strict channel that is ABSENT from the summary is a failure, not
    /// a skip — a channel that produced no output at all is the most
    /// severe outcome available, and the gate must not read it as silence.
    #[arg(long, value_delimiter = ',', default_values_t = ["c".to_string(), "cli".to_string(), "python".to_string()])]
    strict_channels: Vec<String>,
    /// Minimum compared-row count per test type, as `<test_type>=<count>`
    /// or `<channel>:<test_type>=<count>`. Repeatable and/or comma-separated,
    /// e.g. `--min-rows orbit_determination=50,rust:orbit_determination_radar=5`.
    ///
    /// Unscoped entries are enforced against every strict channel. Aggregate
    /// row counts cannot catch a single dead axis: a run with the full
    /// propagation and ephemeris grid and ZERO orbit-determination rows has
    /// tens of thousands of compared rows and passes an aggregate check
    /// without having tested orbit determination at all. That is precisely
    /// the state this suite shipped in. Floors are per-axis for that reason.
    ///
    /// A `<channel>:` prefix scopes the floor to one named channel, which
    /// need NOT be strict. That exists because an axis can be real and worth
    /// gating while living on only one channel: `orbit_determination_radar`
    /// is produced by the rust runner and by nothing else
    /// (`empyrean_validation::plan::PLAN_RUST_ONLY_TEST_TYPES`,
    /// `empyrean-s1ab`), so its floor belongs on `rust`. A scoped floor on a
    /// channel missing from the summary is a failure, exactly like a strict
    /// channel that produced nothing.
    ///
    /// Scale these when running an object subset; prefer `--catalog-floors`
    /// for a full-catalog run so the numbers cannot drift from the catalog.
    #[arg(long, value_delimiter = ',')]
    min_rows: Vec<String>,
    /// Enforce the per-axis floors implied by the catalog, on top of any
    /// `--min-rows` given. Only valid for a FULL-catalog run.
    ///
    /// The floors are not free-standing numbers, they are facts about the
    /// catalog: one `orbit_determination` row per catalog object, and one
    /// `orbit_determination_radar` row per object with a manifest-pinned radar
    /// fixture. Spelling them as literals in the workflow meant adding a
    /// catalog object silently under-strictened the gate — the floor stayed
    /// at the old count and the new object's absence from the OD axis would
    /// have passed. Derived here instead, from
    /// [`empyrean_validation::catalog`], so the two cannot drift.
    #[arg(long)]
    catalog_floors: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.command {
        Command::Plan(args) => plan(args),
        Command::StripPlan(args) => strip_plan(args),
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

/// Reduce a channel-result JSON to the canonical plan.
///
/// The transformation itself lives in [`empyrean_validation::plan`] — the
/// crate that owns the schema owns the plan contract, so a schema change and
/// a plan change land together instead of drifting apart in a shell one-liner.
/// This replaces an inline Python strip in the Makefile that ran under the
/// empyrean wheel's interpreter, which also means plan generation no longer
/// depends on the wheel venv existing.
fn strip_plan(args: StripPlanArgs) -> Result<(), Box<dyn std::error::Error>> {
    let raw = std::fs::read_to_string(&args.input)
        .map_err(|e| format!("read {}: {e}", args.input.display()))?;
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", args.input.display()))?;
    let (plan, drops) = empyrean_validation::plan::strip_to_plan(&rows)?;

    // Same assertion the Makefile makes on the finished plan, made here at
    // the point the rows are actually discarded, so the message can name the
    // input file that came up empty.
    //
    // Counted by NAME against the OD family, not as "anything that is not
    // propagation or ephemeris". The complement counts a typo'd test type as
    // OD, and it counts any of the narrower recovery axes as if they were the
    // OD axis — a plan of nothing but `non_grav_recovery` rows satisfied the
    // old form while carrying not one differential-correction row of the axis
    // this assertion exists to protect. Both halves are checked: the family
    // must be non-empty, and `orbit_determination` itself must be present.
    let n_od = plan
        .iter()
        .filter(|r| {
            r["test_type"]
                .as_str()
                .is_some_and(test_types::is_orbit_determination)
        })
        .count();
    let n_optical = plan
        .iter()
        .filter(|r| r["test_type"].as_str() == Some(test_types::ORBIT_DETERMINATION))
        .count();
    if n_od > 0 && n_optical == 0 {
        return Err(format!(
            "{} carries {n_od} orbit-determination-family row(s) but ZERO \
             `{}` rows. The other family axes ({}) are checks layered on top of \
             the optical fit, not substitutes for it — a plan with only those \
             deletes the OD axis proper while still looking non-empty.",
            args.input.display(),
            test_types::ORBIT_DETERMINATION,
            test_types::ORBIT_DETERMINATION_FAMILY[1..].join(", "),
        )
        .into());
    }
    if n_od == 0 {
        return Err(format!(
            "{} carries ZERO orbit-determination rows ({} rows in, {} plan rows out). \
             The plan would delete the OD axis from every downstream channel while \
             each one reported success.",
            args.input.display(),
            rows.len(),
            plan.len()
        )
        .into());
    }

    if let Some(parent) = args.output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&args.output, serde_json::to_string_pretty(&plan)?)?;
    eprintln!(
        "Wrote {} plan rows to {} ({} OD; {})",
        plan.len(),
        args.output.display(),
        n_od,
        drops
    );
    // Name the rust-only test types explicitly on every strip. The rows are
    // dropped by policy (empyrean-s1ab), and a policy nobody is reminded of
    // becomes a defect nobody remembers to undo.
    if drops.rust_only_test_type > 0 {
        eprintln!(
            "  NOTE: {} row(s) dropped as rust-only test types [{}] — no replay \
             driver can fit them yet (empyrean-s1ab). They stay gated by a \
             `rust:`-scoped ci-check row floor.",
            drops.rust_only_test_type,
            empyrean_validation::plan::PLAN_RUST_ONLY_TEST_TYPES.join(", ")
        );
    }
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
        let (np, ne) = merge_findorb_ephem(&mut rows, path)?;
        if np + ne > 0 {
            eprintln!("Merged {np} find_orb propagation + {ne} ephemeris reference rows");
        }
    }
    if let Some(path) = &args.findorb_radar {
        let n = merge_findorb(&mut rows, path)?;
        eprintln!("Merged {n} find_orb radar rows");
        // Expected while radar is rust-only: the reference channel is built
        // from the plan, the plan no longer carries radar rows
        // (`PLAN_RUST_ONLY_TEST_TYPES`), so there is nothing for find_orb's
        // radar fits to attach to. Say it out loud — a merge that folds zero
        // rows out of a non-empty input file is otherwise indistinguishable
        // from a merge that folded everything.
        if n == 0 {
            eprintln!(
                "  NOTE: find_orb ran its radar pass but no reference row accepted it. \
                 Radar OD is rust-only today (empyrean-s1ab), so the reference channel \
                 carries no orbit_determination_radar rows to fold onto. find_orb's \
                 radar fits are preserved verbatim in {} — they are simply not shown \
                 as a cross-tool comparison until a replay driver can fit radar.",
                path.display()
            );
        }
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
    if let Some(path) = &args.kete {
        let n = merge_tool_times(&mut rows, path, "kete_time_ms", |r, v| r.kete_time_ms = v)?;
        eprintln!("Merged {n} kete timing rows");
    }
    if let Some(path) = &args.jorbit {
        let n = merge_tool_times(&mut rows, path, "jorbit_time_ms", |r, v| {
            r.jorbit_time_ms = v
        })?;
        eprintln!("Merged {n} jorbit timing rows");
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
        r.findorb_time_ms = f["fo_time_ms"].as_f64();
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
        r.orbfit_error = f["orbfit_error"].as_str().map(String::from);
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

/// Fold one external tool's per-row wall-clock timings onto matching rows,
/// keyed by (object, test_type, dt, observer?). Used for the kete / jorbit
/// runners, whose comparison axes are not yet wired into the report
/// registry — only their timing feeds the performance strip.
fn merge_tool_times(
    rows: &mut [ValidationResult],
    path: &std::path::Path,
    field: &str,
    set: impl Fn(&mut ValidationResult, Option<f64>),
) -> Result<usize, Box<dyn std::error::Error>> {
    let txt = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let ext: Vec<serde_json::Value> = serde_json::from_str(&txt)?;
    let mut idx: std::collections::HashMap<(String, String, i64, String), f64> = Default::default();
    for o in &ext {
        let (Some(name), Some(tt)) = (o["object"].as_str(), o["test_type"].as_str()) else {
            continue;
        };
        let Some(t) = o[field].as_f64() else { continue };
        let dt = o["dt_days"].as_f64().unwrap_or(0.0) as i64;
        let obs = o["observer"].as_str().unwrap_or("").to_string();
        idx.insert((name.to_string(), tt.to_string(), dt, obs), t);
    }
    let mut n = 0;
    for r in rows.iter_mut() {
        let key = (
            r.object.clone(),
            r.test_type.clone(),
            r.dt_days as i64,
            r.observer.clone().unwrap_or_default(),
        );
        if let Some(&t) = idx.get(&key) {
            set(r, Some(t));
            n += 1;
        }
    }
    Ok(n)
}

/// Fold find_orb's ephemeris-stage rows (its own fitted orbit propagated by
/// find_orb to the plan's epochs) onto matching propagation / ephemeris rows.
///
/// Propagation: fo emits GEOCENTRIC equatorial-J2000 geometric vectors
/// (AU / AU/day); convert to SSB with Earth's DE440 state (the wrapper's
/// geocentric "500" observer) and diff against the row's Horizons reference
/// and Empyrean position. Ephemeris: fo emits astrometric RA/Dec + range per
/// site; diff against the row's Horizons reference angles.
///
/// Semantics note: these are fit-then-propagate comparisons — find_orb
/// propagates its own fit, NOT the plan's initial conditions, so the diffs
/// include the fit-vs-JPL-orbit difference (unlike ASSIST / OpenOrb).
fn merge_findorb_ephem(
    rows: &mut [ValidationResult],
    path: &std::path::Path,
) -> Result<(usize, usize), Box<dyn std::error::Error>> {
    let txt = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let fo: Vec<serde_json::Value> = serde_json::from_str(&txt)?;
    let vec3 = |v: &serde_json::Value| -> Option<[f64; 3]> {
        let a = v.as_array()?;
        if a.len() != 3 {
            return None;
        }
        Some([a[0].as_f64()?, a[1].as_f64()?, a[2].as_f64()?])
    };
    let mut prop_idx: std::collections::HashMap<(String, i64), [f64; 3]> = Default::default();
    let mut eph_idx: std::collections::HashMap<(String, i64, String), (f64, f64, f64)> =
        Default::default();
    for o in &fo {
        let (Some(name), Some(dt)) = (o["object"].as_str(), o["dt_days"].as_f64()) else {
            continue;
        };
        match o["test_type"].as_str() {
            Some("propagation") => {
                if let Some(p) = vec3(&o["fo_geo_pos_au"]) {
                    prop_idx.insert((name.to_string(), dt as i64), p);
                }
            }
            Some("ephemeris") => {
                if let (Some(obs), Some(ra), Some(dec), Some(delta)) = (
                    o["observer"].as_str(),
                    o["fo_ra_deg"].as_f64(),
                    o["fo_dec_deg"].as_f64(),
                    o["fo_delta_au"].as_f64(),
                ) {
                    eph_idx.insert(
                        (name.to_string(), dt as i64, obs.to_string()),
                        (ra, dec, delta),
                    );
                }
            }
            _ => {}
        }
    }
    if prop_idx.is_empty() && eph_idx.is_empty() {
        return Ok((0, 0));
    }

    // Earth's SSB position at every matched propagation epoch, batched
    // through the wrapper's geocentric ("500") observer. A failure here is
    // loud: silently skipping the conversion would fabricate a comparison.
    let mut earth_at: std::collections::HashMap<u64, [f64; 3]> = Default::default();
    if !prop_idx.is_empty() {
        let mut epochs: Vec<f64> = rows
            .iter()
            .filter(|r| {
                r.test_type == "propagation"
                    && prop_idx.contains_key(&(r.object.clone(), r.dt_days as i64))
            })
            .map(|r| r.t_mjd_tdb)
            .collect();
        epochs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        epochs.dedup();
        if !epochs.is_empty() {
            let ctx = empyrean::Context::from_data_dir(None)
                .map_err(|e| format!("merge --findorb: engine context for Earth SSB state: {e}"))?;
            let eps: Vec<empyrean::Epoch> = epochs
                .iter()
                .map(|&t| empyrean::Epoch::from_mjd_tdb(t))
                .collect();
            let observers = ctx
                .get_observers(&["500"], &eps)
                .map_err(|e| format!("merge --findorb: Earth (500) observer states: {e}"))?;
            for o in &observers {
                let t = o
                    .epoch
                    .mjd_tdb()
                    .map_err(|e| format!("merge --findorb: observer epoch: {e}"))?;
                earth_at.insert(t.to_bits(), o.position);
            }
        }
    }

    let (mut n_prop, mut n_eph) = (0, 0);
    for r in rows.iter_mut() {
        match r.test_type.as_str() {
            "propagation" => {
                let Some(geo) = prop_idx.get(&(r.object.clone(), r.dt_days as i64)) else {
                    continue;
                };
                let Some(earth) = earth_at.get(&r.t_mjd_tdb.to_bits()) else {
                    continue;
                };
                let fo_ssb = [geo[0] + earth[0], geo[1] + earth[1], geo[2] + earth[2]];
                if let Some(rf) = &r.ref_pos_au {
                    let d = ((fo_ssb[0] - rf[0]).powi(2)
                        + (fo_ssb[1] - rf[1]).powi(2)
                        + (fo_ssb[2] - rf[2]).powi(2))
                    .sqrt();
                    r.findorb_vs_horizons_km = Some(d * empyrean_validation::compare::AU_KM);
                }
                if let Some(emp) = &r.emp_pos_au {
                    let d = ((fo_ssb[0] - emp[0]).powi(2)
                        + (fo_ssb[1] - emp[1]).powi(2)
                        + (fo_ssb[2] - emp[2]).powi(2))
                    .sqrt();
                    r.emp_vs_findorb_km = Some(d * empyrean_validation::compare::AU_KM);
                }
                n_prop += 1;
            }
            "ephemeris" => {
                let Some(obs) = r.observer.as_deref() else {
                    continue;
                };
                let key = (r.object.clone(), r.dt_days as i64, obs.to_string());
                let Some(&(ra_deg, dec_deg, delta_au)) = eph_idx.get(&key) else {
                    continue;
                };
                let (Some(ref_ra), Some(ref_dec)) = (r.ref_ra_rad, r.ref_dec_rad) else {
                    continue;
                };
                let fo_ra = ra_deg.to_radians();
                let fo_dec = dec_deg.to_radians();
                let mut d_ra = (fo_ra - ref_ra).rem_euclid(std::f64::consts::TAU);
                if d_ra > std::f64::consts::PI {
                    d_ra -= std::f64::consts::TAU;
                }
                r.findorb_d_ra_arcsec = Some((d_ra * fo_dec.cos()).to_degrees() * 3600.0);
                r.findorb_d_dec_arcsec = Some((fo_dec - ref_dec).to_degrees() * 3600.0);
                r.findorb_separation_arcsec =
                    Some(empyrean_validation::compare::angular_separation_arcsec(
                        fo_ra, fo_dec, ref_ra, ref_dec,
                    ));
                if let Some(rho) = r.ref_rho_au {
                    r.findorb_d_rho_km =
                        Some((delta_au - rho) * empyrean_validation::compare::AU_KM);
                }
                n_eph += 1;
            }
            _ => {}
        }
    }
    Ok((n_prop, n_eph))
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
    // Every sidecar path that was looked for and not found, so an absence can
    // be reported with the names that were actually tried.
    let mut sidecars_tried: Vec<PathBuf> = Vec::new();
    let mut sidecar_found: Option<PathBuf> = None;
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
                sidecars_tried.push(cmp_path);
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
            if n == 0 {
                eprintln!(
                    "  WARNING: {} is present but empty. §12 of the report will render \
                     nothing. The OD pass ran and wrote a sidecar with no comparisons in \
                     it — check the `validate od` log for fits that produced no orbit.",
                    cmp_path.display()
                );
            }
            sidecar_found = Some(cmp_path);
            // First sidecar wins — don't double-load if both candidates exist.
            break;
        }
    }
    // An OD run with no sidecar anywhere is an error, not a skip.
    //
    // `validate od` always writes `{stem}_compare.jsonl` next to its output,
    // so if the OD channel produced rows the sidecar existed at some point;
    // its absence here means it was not carried across a job boundary. That
    // is what happened in CI: prep's upload step listed only the four channel
    // JSONs, the file stayed in prep's workspace, and the reader below
    // `continue`d past the absence and exited 0 — so §12 was empty in every
    // published report, silently, for reasons no log line ever mentioned.
    // Same silent-empty family as the dead OD channel itself, so it fails the
    // same way: loudly, naming what it looked for.
    if sidecar_found.is_none() && all.iter().any(is_od_row) {
        let od_channels: Vec<String> = {
            let mut c: Vec<String> = all
                .iter()
                .filter(|r| is_od_row(r))
                .map(|r| r.channel.clone())
                .collect();
            c.sort();
            c.dedup();
            c
        };
        return Err(format!(
            "orbit-determination rows are present (channels: {}) but no orbit-comparison \
             sidecar was found. §12 of the report would render empty with no explanation. \
             Looked for: {}. `validate od` writes `<output>_compare.jsonl` beside its \
             output — stage it alongside the channel JSONs (it is part of the \
             prep-plan-and-rust artifact).",
            od_channels.join(", "),
            sidecars_tried
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
        .into());
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

/// Is this an orbit-determination row — i.e. a row produced by a
/// differential-correction fit rather than a propagate / ephemeris call?
fn is_od_row(r: &ValidationResult) -> bool {
    test_types::is_orbit_determination(&r.test_type)
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

/// One `--min-rows` floor: "this test type must have compared at least this
/// many rows".
#[derive(Debug, Clone, PartialEq, Eq)]
struct RowFloor {
    /// `None` → the floor applies to every `--strict-channels` entry.
    /// `Some(c)` → it applies to channel `c` alone, strict or not.
    channel: Option<String>,
    test_type: String,
    floor: u64,
}

impl std::fmt::Display for RowFloor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.channel {
            Some(c) => write!(f, "{c}:{}>={}", self.test_type, self.floor),
            None => write!(f, "{}>={}", self.test_type, self.floor),
        }
    }
}

/// Parse `--min-rows [channel:]test_type=count` entries into [`RowFloor`]s.
///
/// A malformed entry is an error, never a skipped floor: a typo'd floor that
/// silently enforced nothing would reproduce the exact class of defect these
/// floors exist to catch. The channel name on a scoped floor is checked for
/// membership for the same reason — `--min-rows rst:...=5` must read as "you
/// typed it wrong", not as "that channel produced nothing".
fn parse_min_rows(specs: &[String]) -> Result<Vec<RowFloor>, String> {
    specs
        .iter()
        .map(|spec| {
            let (lhs, n) = spec.split_once('=').ok_or_else(|| {
                format!("--min-rows expects [<channel>:]<test_type>=<count>, got {spec:?}")
            })?;
            let (channel, tt) = match lhs.split_once(':') {
                Some((c, t)) => (Some(c.trim()), t.trim()),
                None => (None, lhs.trim()),
            };
            if tt.is_empty() {
                return Err(format!("--min-rows entry {spec:?} has an empty test type"));
            }
            if !test_types::ALL.contains(&tt) {
                return Err(format!(
                    "--min-rows entry {spec:?}: {tt:?} is not a known test type (known: {}). \
                     A floor on a name no channel emits enforces nothing while reading as \
                     'that axis was never exercised' — the exact confusion these floors \
                     exist to remove.",
                    test_types::ALL.join(", ")
                ));
            }
            if !SUMMARY_TEST_TYPES.contains(&tt) {
                return Err(format!(
                    "--min-rows entry {spec:?}: {tt:?} is a real test type but is not rolled \
                     up into validation_summary.json (rolled up: {}), so the gate cannot see \
                     it and the floor could never be satisfied. Add it to \
                     empyrean_validation::report::SUMMARY_TEST_TYPES first.",
                    SUMMARY_TEST_TYPES.join(", ")
                ));
            }
            let channel = match channel {
                None => None,
                Some("") => {
                    return Err(format!("--min-rows entry {spec:?} has an empty channel"));
                }
                Some(c) => {
                    if !KNOWN_CHANNELS.contains(&c) {
                        return Err(format!(
                            "--min-rows entry {spec:?}: {c:?} is not a known channel \
                             (known: {})",
                            KNOWN_CHANNELS.join(", ")
                        ));
                    }
                    Some(c.to_string())
                }
            };
            let floor: u64 = n
                .trim()
                .parse()
                .map_err(|e| format!("--min-rows entry {spec:?}: bad count: {e}"))?;
            Ok(RowFloor {
                channel,
                test_type: tt.to_string(),
                floor,
            })
        })
        .collect()
}

/// The per-axis row floors a full-catalog run must clear, derived from the
/// catalog rather than written out as numbers.
///
/// - `orbit_determination` — one row per catalog object. The rust OD pass
///   emits exactly one, success or failure row, for every object it is given,
///   and every replay channel replays what the plan carries. Unscoped, so it
///   applies to every strict channel.
/// - `orbit_determination_radar` — one row per object with a manifest-pinned radar
///   fixture, scoped to `rust`: those rows are stripped from the plan while
///   radar OD is rust-only (`plan::PLAN_RUST_ONLY_TEST_TYPES`,
///   `empyrean-s1ab`), so `rust` is the only channel that has them.
fn catalog_row_floors() -> Vec<RowFloor> {
    vec![
        RowFloor {
            channel: None,
            test_type: test_types::ORBIT_DETERMINATION.to_string(),
            floor: all_objects().len() as u64,
        },
        RowFloor {
            channel: Some(channels::RUST.to_string()),
            test_type: test_types::ORBIT_DETERMINATION_RADAR.to_string(),
            floor: RADAR_FIXTURE_OBJECTS.len() as u64,
        },
    ]
}

/// Channel names a `--min-rows` floor (or `--strict-channels`) may name.
///
/// The `channels` module constants plus the externally-merged comparators that
/// only ever appear as their own result JSON. Validated against so a typo is
/// "you typed it wrong", not "that channel produced nothing".
const KNOWN_CHANNELS: [&str; 12] = [
    channels::RUST,
    channels::PYTHON,
    channels::C,
    channels::CLI,
    channels::CORE,
    channels::ASSIST,
    channels::FINDORB,
    channels::KETE,
    "oorb",
    "jorbit",
    "layup",
    "orbfit",
];

/// Every reason this summary fails the gate, as human-readable lines.
///
/// Split out from [`ci_check`] so the gate's logic is testable without a
/// process exit — a gate nobody can write a test against is a gate nobody
/// notices has stopped checking anything.
fn ci_check_failures(
    channels: &[serde_json::Value],
    strict_channels: &[String],
    min_rows: &[RowFloor],
    summary_path: &std::path::Path,
) -> Vec<String> {
    let mut failures = Vec::new();
    for want in strict_channels {
        // Absence check FIRST. The previous gate iterated the summary and
        // `continue`d past any channel not in --strict-channels, which meant
        // a strict channel missing from the summary entirely was never
        // examined — the gate passed on a channel that had produced nothing.
        let Some(ch) = channels
            .iter()
            .find(|c| c["channel"].as_str() == Some(want.as_str()))
        else {
            let present: Vec<&str> = channels
                .iter()
                .filter_map(|c| c["channel"].as_str())
                .collect();
            failures.push(format!(
                "{want}: ABSENT from {} — the channel produced no rows at all \
                 (present: [{}])",
                summary_path.display(),
                present.join(", ")
            ));
            continue;
        };

        let passing = ch["n_passing"].as_u64().unwrap_or(0);
        let total = ch["n_total_compared"].as_u64().unwrap_or(0);
        if total == 0 {
            failures.push(format!("{want}: no rows compared"));
        } else if passing < total {
            failures.push(format!(
                "{want}: {passing}/{total} rows passed at 1e-10 (expected 100%)"
            ));
        }

        // Unscoped per-axis floors apply to every strict channel. Scoped
        // floors are handled below, against the one channel they name.
        for f in min_rows.iter().filter(|f| f.channel.is_none()) {
            failures.extend(floor_failures(ch, want, f, true));
        }
    }

    // Channel-scoped floors. The channel need not be strict — `rust` is the
    // reference, never strict, and yet it is the only channel that carries
    // the radar OD axis (empyrean-s1ab). Absence is a failure here for the
    // same reason it is for a strict channel: a floor that evaluates to
    // nothing because its channel vanished is a gate that stopped checking.
    for f in min_rows.iter().filter(|f| f.channel.is_some()) {
        let want = f.channel.as_deref().expect("filtered to Some");
        let Some(ch) = channels
            .iter()
            .find(|c| c["channel"].as_str() == Some(want))
        else {
            let present: Vec<&str> = channels
                .iter()
                .filter_map(|c| c["channel"].as_str())
                .collect();
            failures.push(format!(
                "{want}: ABSENT from {} — a row floor is scoped to it ({f}) but the \
                 channel produced no rows at all (present: [{}])",
                summary_path.display(),
                present.join(", ")
            ));
            continue;
        };
        let strict = strict_channels.iter().any(|s| s == want);
        failures.extend(floor_failures(ch, want, f, strict));
    }
    failures
}

/// Evaluate one row floor against one summary channel entry.
///
/// Two metrics, because a floor has two things to say:
///
/// - `n_rows` — the channel PRODUCED this many rows on this axis. Checked
///   always. This is the structural-death check: an axis nobody ran.
/// - `n_compared` — this many of them paired with a reference row and were
///   actually diffed. Checked only for `--strict-channels`, whose whole job is
///   to match the reference. It is meaningless for a channel that owns an axis
///   alone: `orbit_determination_radar` exists on `rust` and nowhere else
///   (`plan::PLAN_RUST_ONLY_TEST_TYPES`, empyrean-s1ab), so its `n_compared` is
///   structurally 0 and a floor on it would fail forever no matter how much
///   radar work ran.
///
/// `by_test_type` is keyed by test type; a missing key and a zero count are the
/// same failure — the axis was not exercised — and are reported as such rather
/// than skipped.
fn floor_failures(
    ch: &serde_json::Value,
    channel: &str,
    f: &RowFloor,
    strict: bool,
) -> Vec<String> {
    let (tt, floor) = (&f.test_type, f.floor);
    let entry = &ch["by_test_type"][tt];
    if entry.is_null() {
        return vec![format!(
            "{channel}: {tt} is absent from the summary (floor is {floor}) — \
             that axis was never exercised"
        )];
    }
    let mut out = Vec::new();
    // `n_rows` predates nothing — it was added with the scoped floors. Treat a
    // summary that lacks it as a hard error rather than skipping the check: a
    // stale summary silently passing a floor is the defect, not the fix.
    let rows_ok = match entry["n_rows"].as_u64() {
        Some(n) if n >= floor => true,
        Some(n) => {
            out.push(format!(
                "{channel}: {tt} produced {n} rows, floor is {floor}"
            ));
            false
        }
        None => {
            out.push(format!(
                "{channel}: {tt} carries no n_rows count (floor is {floor}) — the summary \
                 predates per-axis row counts; regenerate it with this build of `report`"
            ));
            false
        }
    };
    // A row floor alone gates for EXISTENCE, not health: the runners emit a
    // failure row for every non-convergent or unreadable case, so a floor
    // counting rows is satisfied by total breakage of the axis it protects.
    // Strict channels get their health check from the separate
    // `passing == total` assertion, but a rust-only axis has no reference
    // channel and so no strict check to fall back on — the convergence count
    // is the only thing standing between "the pass ran" and "the pass worked".
    // Only when the row floor PASSED. If it failed, the row shortage already
    // explains the convergence shortage and reporting both says the same thing
    // twice — worse, it would claim the pass ran when it may not have.
    if rows_ok {
        match entry["n_converged"].as_u64() {
            Some(n) if n >= floor => {}
            Some(n) => out.push(format!(
                "{channel}: {tt} produced {} rows but only {n} CONVERGED, floor is {floor} — \
                 the pass ran and emitted failure rows rather than being skipped",
                entry["n_rows"].as_u64().unwrap_or(0)
            )),
            None => out.push(format!(
                "{channel}: {tt} carries no n_converged count (floor is {floor}) — the summary \
                 predates per-axis convergence counts; regenerate it with this build of `report`"
            )),
        }
    }
    if strict {
        match entry["n_compared"].as_u64() {
            Some(n) if n >= floor => {}
            Some(n) => out.push(format!(
                "{channel}: {tt} compared {n} rows, floor is {floor}"
            )),
            None => out.push(format!(
                "{channel}: {tt} carries no n_compared count (floor is {floor})"
            )),
        }
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
    let mut min_rows = parse_min_rows(&args.min_rows)?;
    if args.catalog_floors {
        min_rows.extend(catalog_row_floors());
    }
    let failures = ci_check_failures(channels, &args.strict_channels, &min_rows, &args.summary);

    if failures.is_empty() {
        eprintln!(
            "ci-check: all strict channels [{}] passed binding fidelity at 1e-10",
            args.strict_channels.join(", ")
        );
        if min_rows.is_empty() {
            eprintln!(
                "ci-check: no per-axis row floors were requested \
                 (--min-rows / --catalog-floors)"
            );
        } else {
            eprintln!(
                "ci-check: per-axis row floors met [{}]",
                min_rows
                    .iter()
                    .map(RowFloor::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
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

    // ── ci-check gate ────────────────────────────────────────────────
    //
    // The gate that shipped had two holes, both of which let a channel that
    // computed nothing report success. These tests pin both shut.

    /// A summary channel entry with the given per-axis counts. Rows produced
    /// and rows compared are equal here — the interesting case where they
    /// differ gets its own builder, [`summary_channel_split`].
    fn summary_channel(name: &str, by_tt: &[(&str, u64)]) -> serde_json::Value {
        let split: Vec<(&str, u64, u64)> = by_tt.iter().map(|(tt, n)| (*tt, *n, *n)).collect();
        summary_channel_split(name, &split)
    }

    /// A summary channel entry with per-axis (rows produced, rows compared)
    /// counts that may differ — the shape a rust-only axis takes. Every row
    /// counts as converged; the failure-row case has its own helper below.
    fn summary_channel_split(name: &str, by_tt: &[(&str, u64, u64)]) -> serde_json::Value {
        let split: Vec<(&str, u64, u64, u64)> =
            by_tt.iter().map(|(tt, r, c)| (*tt, *r, *r, *c)).collect();
        summary_channel_split_conv(name, &split)
    }

    /// As above, but with the converged count stated separately, so a test can
    /// build the "the pass ran and emitted nothing but failure rows" shape.
    fn summary_channel_split_conv(
        name: &str,
        by_tt: &[(&str, u64, u64, u64)],
    ) -> serde_json::Value {
        let total: u64 = by_tt.iter().map(|(_, _, _, n)| *n).sum();
        let by_test_type: serde_json::Map<String, serde_json::Value> = by_tt
            .iter()
            .map(|(tt, rows, converged, compared)| {
                (
                    (*tt).to_string(),
                    serde_json::json!({
                        "n_rows": rows,
                        "n_converged": converged,
                        "n_compared": compared,
                    }),
                )
            })
            .collect();
        serde_json::json!({
            "channel": name,
            "n_rows": total,
            "n_passing": total,
            "n_total_compared": total,
            "by_test_type": by_test_type,
        })
    }

    fn strict(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_string()).collect()
    }

    fn check(channels: &[serde_json::Value], names: &[&str], floors: &[&str]) -> Vec<String> {
        let min_rows = parse_min_rows(&strict(floors)).expect("floors parse");
        ci_check_failures(
            channels,
            &strict(names),
            &min_rows,
            std::path::Path::new("results/validation_summary.json"),
        )
    }

    #[test]
    fn ci_check_fails_when_a_strict_channel_is_absent() {
        // Hole 1: the gate iterated the summary's channels and skipped any
        // that were not strict, so a strict channel MISSING from the summary
        // was never examined at all — the most severe outcome available read
        // as silence.
        let channels = vec![summary_channel("python", &[("propagation", 100)])];
        let failures = check(&channels, &["python", "core"], &[]);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].starts_with("core: ABSENT"), "{failures:?}");
        assert!(failures[0].contains("python"), "lists what was present");

        // Present channels are still checked normally.
        assert!(check(&channels, &["python"], &[]).is_empty());
    }

    #[test]
    fn ci_check_fails_on_a_zero_row_od_axis_that_aggregates_green() {
        // Hole 2: the gate asserted aggregate row counts only. This is the
        // exact shape that shipped — a full propagation + ephemeris grid and
        // ZERO orbit-determination rows. 12,194 rows compared, 100% passing,
        // and the OD axis never ran.
        let channels = vec![summary_channel(
            "core",
            &[("propagation", 4854), ("ephemeris", 7340)],
        )];

        // Aggregate-only: green, exactly as before.
        assert!(check(&channels, &["core"], &[]).is_empty());

        // With floors: caught, and the message names the axis.
        let failures = check(
            &channels,
            &["core"],
            &["orbit_determination=50", "orbit_determination_radar=5"],
        );
        assert_eq!(failures.len(), 2, "{failures:?}");
        assert!(
            failures[0].contains("orbit_determination is absent"),
            "{failures:?}"
        );
        assert!(
            failures[1].contains("orbit_determination_radar is absent"),
            "{failures:?}"
        );
    }

    #[test]
    fn ci_check_floor_is_a_floor_not_an_equality() {
        let at_floor = vec![summary_channel("core", &[("orbit_determination", 50)])];
        assert!(check(&at_floor, &["core"], &["orbit_determination=50"]).is_empty());

        let above = vec![summary_channel("core", &[("orbit_determination", 55)])];
        assert!(check(&above, &["core"], &["orbit_determination=50"]).is_empty());

        // One short — a partially-run catalog is a failure, not a pass. A
        // strict channel reports both halves: it produced 49 and compared 49.
        let below = vec![summary_channel("core", &[("orbit_determination", 49)])];
        let failures = check(&below, &["core"], &["orbit_determination=50"]);
        assert_eq!(failures.len(), 2, "{failures:?}");
        assert!(
            failures
                .iter()
                .any(|f| f.contains("produced 49 rows, floor is 50")),
            "{failures:?}"
        );
        assert!(
            failures
                .iter()
                .any(|f| f.contains("compared 49 rows, floor is 50")),
            "{failures:?}"
        );
    }

    #[test]
    fn min_rows_rejects_malformed_specs() {
        // A typo'd floor that silently enforced nothing would reproduce the
        // very defect the floors exist to catch, so parsing is strict.
        assert!(parse_min_rows(&strict(["orbit_determination=50"].as_ref())).is_ok());
        assert!(parse_min_rows(&strict(["orbit_determination"].as_ref())).is_err());
        assert!(parse_min_rows(&strict(["=50"].as_ref())).is_err());
        assert!(parse_min_rows(&strict(["orbit_determination=lots"].as_ref())).is_err());
        // Channel-scoped form.
        assert_eq!(
            parse_min_rows(&strict(["rust:orbit_determination_radar=5"].as_ref())).unwrap(),
            vec![RowFloor {
                channel: Some("rust".into()),
                test_type: "orbit_determination_radar".into(),
                floor: 5,
            }]
        );
        assert!(parse_min_rows(&strict([":orbit_determination=5"].as_ref())).is_err());
        // A channel name nobody emits must read as "you typed it wrong", not
        // as "that channel produced nothing".
        assert!(parse_min_rows(&strict(["rst:orbit_determination=5"].as_ref())).is_err());
    }

    #[test]
    fn min_rows_rejects_unknown_test_types() {
        // Syntax alone is not enough. A typo parsed cleanly and then reported
        // "that axis was never exercised" — indistinguishable from a genuinely
        // dead axis, which is the confusion the floors exist to remove.
        let err = parse_min_rows(&strict(["orbit_determinaton=50"].as_ref()))
            .expect_err("typo must be rejected");
        assert!(err.contains("not a known test type"), "{err}");
        assert!(
            err.contains("orbit_determination"),
            "lists the known set: {err}"
        );

        // A real test type that the summary does not roll up is a different
        // error, because it is a different fix.
        let err = parse_min_rows(&strict(["dt_recovery=1"].as_ref()))
            .expect_err("un-rolled-up axis must be rejected");
        assert!(err.contains("SUMMARY_TEST_TYPES"), "{err}");

        // Everything the summary does roll up parses, scoped or not.
        for tt in SUMMARY_TEST_TYPES {
            parse_min_rows(&strict([format!("{tt}=1").as_str()].as_ref()))
                .unwrap_or_else(|e| panic!("{tt}: {e}"));
            parse_min_rows(&strict([format!("rust:{tt}=1").as_str()].as_ref()))
                .unwrap_or_else(|e| panic!("rust:{tt}: {e}"));
        }
    }

    #[test]
    fn catalog_floors_track_the_catalog() {
        // The 50 and the 5 used to be literals in the workflow, coupled to the
        // catalog by nothing. Derived now, so adding a catalog object raises
        // the floor with it instead of silently under-strictening the gate.
        let floors = catalog_row_floors();
        assert_eq!(
            floors,
            vec![
                RowFloor {
                    channel: None,
                    test_type: "orbit_determination".into(),
                    floor: all_objects().len() as u64,
                },
                RowFloor {
                    channel: Some("rust".into()),
                    test_type: "orbit_determination_radar".into(),
                    floor: RADAR_FIXTURE_OBJECTS.len() as u64,
                },
            ]
        );
        // And they are the numbers the workflow used to spell out, so this is
        // a re-derivation of the same gate rather than a quiet loosening.
        assert_eq!(floors[0].floor, 50);
        assert_eq!(floors[1].floor, 5);
    }

    #[test]
    fn catalog_floors_pass_a_full_catalog_run_and_fail_a_short_one() {
        let n = all_objects().len() as u64;
        let radar = RADAR_FIXTURE_OBJECTS.len() as u64;
        let full = vec![
            summary_channel("core", &[("orbit_determination", n)]),
            summary_channel_split(
                "rust",
                &[
                    ("orbit_determination", n, n),
                    ("orbit_determination_radar", radar, 0),
                ],
            ),
        ];
        let floors = catalog_row_floors();
        assert!(
            ci_check_failures(
                &full,
                &strict(&["core"]),
                &floors,
                std::path::Path::new("results/validation_summary.json"),
            )
            .is_empty()
        );

        // One catalog object missing from the OD axis.
        let short = vec![
            summary_channel("core", &[("orbit_determination", n - 1)]),
            summary_channel_split(
                "rust",
                &[
                    ("orbit_determination", n, n),
                    ("orbit_determination_radar", radar, 0),
                ],
            ),
        ];
        let failures = ci_check_failures(
            &short,
            &strict(&["core"]),
            &floors,
            std::path::Path::new("results/validation_summary.json"),
        );
        assert!(!failures.is_empty(), "a short catalog must fail the gate");
    }

    #[test]
    fn scoped_floors_gate_a_non_strict_channel() {
        // Radar OD is produced only by the rust runner (empyrean-s1ab), and
        // rust is never a strict channel — it IS the reference. A floor that
        // could only be expressed against a strict channel would therefore
        // have to be dropped entirely, silently un-gating the axis. Scoped
        // floors exist so it stays gated where the rows actually are.
        //
        // Note the (rows, compared) split on the rust radar axis: 5 rows
        // produced, 0 compared. That is not a degenerate case, it is THE
        // case — the reference channel carries no radar rows to pair with,
        // so a compared-count floor could never pass. Non-strict channels are
        // floored on rows produced for exactly this reason.
        let channels = vec![
            summary_channel("core", &[("orbit_determination", 50)]),
            summary_channel_split(
                "rust",
                &[
                    ("orbit_determination", 50, 50),
                    ("orbit_determination_radar", 5, 0),
                ],
            ),
        ];
        assert!(
            check(
                &channels,
                &["core"],
                &["orbit_determination=50", "rust:orbit_determination_radar=5"],
            )
            .is_empty()
        );

        // A row floor alone gates for EXISTENCE, not health. The runners emit
        // a failure row for every non-convergent or unreadable case — that is
        // deliberate, since a silent skip is what left this channel dead for
        // months — so an axis that ran and failed COMPLETELY still satisfies a
        // row floor. On a strict channel the separate passing==total assertion
        // catches it; a rust-only axis has no reference and therefore no such
        // check, so without a convergence floor the radar axis would be gated
        // for having been attempted rather than for having worked.
        let all_failed = vec![
            summary_channel("core", &[("orbit_determination", 50)]),
            summary_channel_split_conv(
                "rust",
                &[
                    ("orbit_determination", 50, 50, 50),
                    // 5 rows produced, ZERO converged: every radar fit failed.
                    ("orbit_determination_radar", 5, 0, 0),
                ],
            ),
        ];
        let failures = check(
            &all_failed,
            &["core"],
            &["orbit_determination=50", "rust:orbit_determination_radar=5"],
        );
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(
            failures[0].contains("only 0 CONVERGED"),
            "the convergence shortfall must be named, not the row count: {failures:?}"
        );

        // The scoped floor is enforced against rust ALONE — core carries no
        // radar rows by design and must not be failed for it.
        let short = vec![
            summary_channel("core", &[("orbit_determination", 50)]),
            summary_channel_split(
                "rust",
                &[
                    ("orbit_determination", 50, 50),
                    ("orbit_determination_radar", 4, 0),
                ],
            ),
        ];
        let failures = check(
            &short,
            &["core"],
            &["orbit_determination=50", "rust:orbit_determination_radar=5"],
        );
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(
            failures[0].starts_with("rust: orbit_determination_radar produced 4 rows, floor is 5"),
            "{failures:?}"
        );
    }

    #[test]
    fn strict_channel_floors_check_produced_and_compared() {
        // A strict channel that emitted the rows but paired none of them has
        // not exercised the axis — the reference never saw its work. Rows
        // produced alone is the weaker claim, and for a strict channel the
        // stronger one is available and must be made.
        let channels = vec![summary_channel_split(
            "core",
            &[("orbit_determination", 50, 0)],
        )];
        let failures = check(&channels, &["core"], &["orbit_determination=50"]);
        assert!(
            failures
                .iter()
                .any(|f| f == "core: orbit_determination compared 0 rows, floor is 50"),
            "{failures:?}"
        );
        // …and the rows-produced half stays silent: the channel did emit 50.
        assert!(
            !failures.iter().any(|f| f.contains("produced")),
            "{failures:?}"
        );
    }

    #[test]
    fn scoped_floor_on_an_absent_channel_fails() {
        // Same severity as an absent strict channel: a floor whose channel
        // vanished evaluates to nothing, which is a gate that stopped
        // checking rather than a gate that passed.
        let channels = vec![summary_channel("core", &[("orbit_determination", 50)])];
        let failures = check(&channels, &["core"], &["rust:orbit_determination_radar=5"]);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].starts_with("rust: ABSENT"), "{failures:?}");
        assert!(
            failures[0].contains("rust:orbit_determination_radar>=5"),
            "names the floor that could not be evaluated: {failures:?}"
        );
    }

    // ── orbit-comparison sidecar ─────────────────────────────────────

    /// Write a one-channel results JSON and return its path.
    fn write_results(dir: &std::path::Path, name: &str, rows: &[ValidationResult]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, serde_json::to_string(rows).unwrap()).unwrap();
        p
    }

    fn rust_od_rows() -> Vec<ValidationResult> {
        let mut od = od_row("Eros");
        od.channel = "rust".into();
        let mut prop = ValidationResult::empty();
        prop.object = "Eros".into();
        prop.channel = "rust".into();
        prop.test_type = "propagation".into();
        vec![od, prop]
    }

    #[test]
    fn report_fails_loudly_when_the_od_sidecar_is_absent() {
        // The reader used to `continue` past a missing sidecar and exit 0, so
        // §12 was empty in every published report and nothing said why. If OD
        // rows are present the sidecar existed upstream; its absence here
        // means it was never carried across the job boundary.
        let dir = tempfile::tempdir().unwrap();
        let results = write_results(dir.path(), "validation_rust.json", &rust_od_rows());
        let err = report(ReportArgs {
            results: vec![results],
            output: dir.path().join("report.html"),
            summary: None,
        })
        .expect_err("absent sidecar must fail");
        let msg = err.to_string();
        assert!(msg.contains("no orbit-comparison sidecar"), "{msg}");
        // Names both candidates it looked for, so the fix is obvious.
        assert!(msg.contains("validation_rust_compare.jsonl"), "{msg}");
        assert!(msg.contains("validation_rust_od_compare.jsonl"), "{msg}");
    }

    #[test]
    fn report_accepts_an_od_run_whose_sidecar_is_staged() {
        let dir = tempfile::tempdir().unwrap();
        let results = write_results(dir.path(), "validation_rust.json", &rust_od_rows());
        // Present but empty is a warning, not a failure: `validate od` writes
        // the file unconditionally, and a fit that produced no comparison is a
        // different problem from a file that never travelled.
        std::fs::write(dir.path().join("validation_rust_od_compare.jsonl"), "").unwrap();
        report(ReportArgs {
            results: vec![results],
            output: dir.path().join("report.html"),
            summary: None,
        })
        .expect("staged sidecar");
    }

    #[test]
    fn report_does_not_demand_a_sidecar_without_od_rows() {
        // A propagation/ephemeris-only run never invokes `validate od`, so
        // there is no sidecar to miss and no §12 to fill.
        let dir = tempfile::tempdir().unwrap();
        let mut prop = ValidationResult::empty();
        prop.object = "Eros".into();
        prop.channel = "rust".into();
        prop.test_type = "propagation".into();
        let results = write_results(dir.path(), "validation_rust.json", &[prop]);
        report(ReportArgs {
            results: vec![results],
            output: dir.path().join("report.html"),
            summary: None,
        })
        .expect("prop-only run needs no sidecar");
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

    #[test]
    fn merge_findorb_ephem_folds_sky_plane_offsets() {
        // An ephemeris entry folds signed RA/Dec offsets vs the row's own
        // Horizons reference; a propagation entry with no matching row is
        // ignored (and must NOT force an engine context in CI).
        let mut r = ValidationResult::empty();
        r.object = "Apophis".into();
        r.test_type = "ephemeris".into();
        r.dt_days = 30.0;
        r.observer = Some("W84".into());
        r.ref_ra_rad = Some(100.0_f64.to_radians());
        r.ref_dec_rad = Some(10.0_f64.to_radians());
        r.ref_rho_au = Some(1.5);
        let mut rows = vec![r];

        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(
            f,
            r#"[{{"object":"Apophis","test_type":"ephemeris","dt_days":30.0,
                 "observer":"W84","fo_ra_deg":100.001,"fo_dec_deg":10.0005,
                 "fo_delta_au":1.5001}},
                {{"object":"Nonexistent","test_type":"propagation","dt_days":0.0,
                 "fo_geo_pos_au":[1.0,2.0,3.0],"fo_geo_vel_au_d":[0,0,0]}}]"#
        )
        .unwrap();
        let (np, ne) = merge_findorb_ephem(&mut rows, f.path()).unwrap();
        assert_eq!((np, ne), (0, 1));

        let r = &rows[0];
        // dRA·cosδ = 0.001° · cos(10.0005°) · 3600 ≈ 3.545″
        let d_ra = r.findorb_d_ra_arcsec.unwrap();
        assert!((d_ra - 0.001 * 10.0005_f64.to_radians().cos() * 3600.0).abs() < 1e-6);
        let d_dec = r.findorb_d_dec_arcsec.unwrap();
        assert!((d_dec - 0.0005 * 3600.0).abs() < 1e-6);
        let sep = r.findorb_separation_arcsec.unwrap();
        assert!((sep - (d_ra * d_ra + d_dec * d_dec).sqrt()).abs() < 1e-4);
        let d_rho = r.findorb_d_rho_km.unwrap();
        assert!((d_rho - 0.0001 * empyrean_validation::compare::AU_KM).abs() < 1e-3);
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
