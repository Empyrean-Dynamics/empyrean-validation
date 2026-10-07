//! `validate synthesize` — the engine half of the synthetic
//! (perfect-model) covariance-realism lane.
//!
//! Per catalog object: fit a full-arc truth orbit on the real fixture with
//! the default configuration, generate the truth ephemeris at every
//! ground-based (station, time) of that fixture with the engine's own
//! predictor, hand the positions to the harness's noise model, and write
//! the synthetic PSV. Then regenerate the window manifest against the
//! synthetic fixtures so the walk runner consumes the lane unchanged.
//!
//! The truth ephemeris is produced by exactly the call the walk's
//! prediction step makes (`get_observers` per row, one batched
//! `generate_ephemeris`), so fit-time dynamics, prediction-time dynamics,
//! and truth share one model: the residual left after the fit is the
//! injected noise and nothing else.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use empyrean::{Context, EphemerisConfig, Epoch, ODConfig, Orbit};
use empyrean_validation::catalog::ValidationObject;
use empyrean_validation::synthetic::{
    NoiseModel, ObjectProvenance, PsvTable, SYNTHETIC_AST_CAT, SYNTHETIC_SCHEMA_VERSION,
    SyntheticProvenance, TruthOrbit, noise_self_check, sigma_key, synthesize_table,
    synthetic_snapshot_id,
};
use empyrean_validation::windows::{self, WindowsOptions};
use rayon::prelude::*;

/// Resolved arguments for one synthesis pass.
pub struct SynthArgs {
    /// Real PSV fixture directory (sibling `manifest.json` supplies the
    /// source snapshot id).
    pub fixtures_dir: PathBuf,
    /// Output root; one lane per noise law is written under
    /// `<out_root>/<law tag>/` as `psv/`, `manifest.json`, `windows.json`,
    /// `synthetic.json`. Every law shares the object's truth fit and truth
    /// ephemeris — only the draws differ.
    pub out_root: PathBuf,
    pub models: Vec<NoiseModel>,
    pub seed: u64,
    pub fiducial_sigma_arcsec: f64,
    pub tier: empyrean::ForceModelTier,
    pub tier_name: String,
    pub only: Vec<String>,
    /// EFCC2020 table directory for the regenerated manifest (its
    /// corrections are identically zero on `Gaia2` rows, but the manifest
    /// still records them as computed, never as absent).
    pub debias_dir: PathBuf,
}

fn read_source_snapshot(fixtures_dir: &Path) -> Result<String, String> {
    let path = fixtures_dir
        .parent()
        .ok_or("fixtures dir has no parent")?
        .join("manifest.json");
    let txt = std::fs::read_to_string(&path)
        .map_err(|e| format!("read fixture manifest {}: {e}", path.display()))?;
    let v: serde_json::Value =
        serde_json::from_str(&txt).map_err(|e| format!("parse {}: {e}", path.display()))?;
    v["snapshot_id"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| format!("{}: no snapshot_id field", path.display()))
}

/// The same three-candidate stem convention every runner uses.
fn resolve_fixture(fixtures_dir: &Path, obj: &ValidationObject) -> Result<PathBuf, String> {
    let candidates = [
        fixtures_dir.join(format!("{}.psv", obj.name)),
        fixtures_dir.join(format!("{}.psv", obj.name.replace('/', "_"))),
        fixtures_dir.join(format!("{}.psv", obj.mpc_designation)),
    ];
    candidates
        .iter()
        .find(|p| p.exists())
        .cloned()
        .ok_or_else(|| format!("{}: no fixture found", obj.name))
}

/// One object's truth product: the space-based-free table, the truth
/// positions aligned with its rows, and the provenance so far. `positions`
/// is empty when the truth fit failed — the object's PSV is then written
/// header-only and the manifest marks it ineligible with the failure named
/// here, never silently absent.
struct TruthProduct {
    prov: ObjectProvenance,
    table: PsvTable,
    positions: Vec<(f64, f64)>,
}

/// One object's truth pass: truth fit → truth ephemeris at every row.
fn truth_for_object(
    ctx: &Context,
    obj: &ValidationObject,
    args: &SynthArgs,
) -> Result<TruthProduct, String> {
    let path = resolve_fixture(&args.fixtures_dir, obj)?;
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut table = PsvTable::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let n_rows_source = table.rows.len();
    let n_dropped = table.drop_space_based();

    let mut prov = ObjectProvenance {
        object: obj.name.to_string(),
        mpc_designation: obj.mpc_designation.to_string(),
        population: obj.population.to_string(),
        truth: None,
        failure: None,
        n_rows_source,
        n_rows_space_based_dropped: n_dropped,
        n_rows_observer_unresolvable: 0,
        observer_unresolvable_stations: BTreeMap::new(),
        n_rows_written: 0,
        sigma_sources: BTreeMap::new(),
        sigma_histogram: BTreeMap::new(),
        self_check: None,
        truth_fit_ms: 0.0,
        ephemeris_ms: 0.0,
    };
    let failed = |prov: ObjectProvenance, table: PsvTable| TruthProduct {
        prov,
        table,
        positions: Vec::new(),
    };

    // ── truth fit: the real fixture's ground-based rows, default config ──
    // The engine's own reading of the fixture (not the harness parser)
    // feeds the fit, restricted to the rows the synthetic PSV will carry.
    let parsed = ctx
        .read_ades(&text)
        .map_err(|e| format!("{}: read_ades: {e}", obj.name))?;
    let ground: Vec<empyrean::Observation> = parsed
        .iter()
        .filter(|o| !(o.sys.as_deref().is_some_and(|s| !s.is_empty()) || o.pos1.is_some()))
        .collect();
    if ground.len() != table.rows.len() {
        return Err(format!(
            "{}: engine parsed {} ground-based rows, harness table has {} — the \
             space-based rule diverged between the two parsers",
            obj.name,
            ground.len(),
            table.rows.len()
        ));
    }
    if ground.is_empty() {
        prov.failure = Some("no ground-based observations".into());
        return Ok(failed(prov, table));
    }
    let observations = empyrean::Observations::from_array(&ground)
        .map_err(|e| format!("{}: from_array: {e}", obj.name))?;
    // A Self-Perturber must not be pulled by its own ephemeris — the same
    // exclusion the OD channel and the walk apply.
    let excluded_naif = empyrean_validation::plan::self_perturber_naif_ids(obj);
    let cfg = ODConfig {
        force_model: args.tier,
        num_threads: 1,
        excluded_perturbers: crate::runner::naif_to_origins(&excluded_naif)?,
        ..ODConfig::default()
    };
    let t0 = Instant::now();
    let fit = ctx
        .determine(&observations, None, &cfg)
        .and_then(|b| b.into_single());
    prov.truth_fit_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let fit = match fit {
        Ok(f) if f.converged => f,
        Ok(f) => {
            prov.failure = Some(format!(
                "truth fit did not converge ({} iterations, reduced chi2 {:.3})",
                f.iterations, f.summary.reduced_chi2
            ));
            return Ok(failed(prov, table));
        }
        Err(e) => {
            prov.failure = Some(format!("truth fit: {e}"));
            return Ok(failed(prov, table));
        }
    };
    let truth: Orbit = fit.orbit.clone();
    let n_solve_for =
        if fit.covariance_9x9.is_some() { 9 } else { 6 } + u32::from(fit.dt_delta.is_some());
    prov.truth = Some(TruthOrbit {
        epoch_mjd_tdb: truth
            .state
            .epoch
            .mjd_tdb()
            .map_err(|e| format!("{}: truth epoch: {e}", obj.name))?,
        representation: format!("{:?}", truth.state.representation),
        frame: format!("{:?}", truth.state.frame),
        origin: format!("{:?}", truth.state.origin),
        elements: truth.state.elements,
        a1: truth.a1,
        a2: truth.a2,
        a3: truth.a3,
        ng_alpha: truth.ng_alpha,
        ng_r0: truth.ng_r0,
        ng_m: truth.ng_m,
        ng_n: truth.ng_n,
        ng_k: truth.ng_k,
        non_grav_dt: truth.non_grav_dt,
        excluded_perturbers_naif: excluded_naif.clone(),
        n_solve_for,
        n_obs_used: fit.summary.num_selected as u32,
        n_obs_rejected: fit.summary.num_rejected as u32,
        reduced_chi2: fit.summary.reduced_chi2,
        iterations: fit.iterations,
    });

    // ── truth ephemeris at every (station, time) — the walk's own call ──
    let t1 = Instant::now();
    // Rows whose observer the engine cannot place are dropped and tallied
    // per station — a row with no observer has no truth position, and the
    // manifest generator's space-based rule (sys/pos columns) does not see
    // e.g. WISE rows written without them.
    let mut observers = Vec::with_capacity(table.rows.len());
    let mut keep = Vec::with_capacity(table.rows.len());
    for i in 0..table.rows.len() {
        let (stn, obs_time) = table.row_identity(i);
        let epoch = Epoch::from_iso_utc(obs_time)
            .map_err(|e| format!("{}: epoch {obs_time}: {e}", obj.name))?;
        match ctx.get_observers(
            &[stn],
            &[epoch],
            empyrean::Frame::ICRF,
            empyrean::Origin::SSB,
        ) {
            Ok(mut v) if v.len() == 1 => {
                observers.push(v.pop().unwrap());
                keep.push(i);
            }
            Ok(v) => {
                return Err(format!(
                    "{}: observer {stn} @ {obs_time}: expected 1 observer, got {}",
                    obj.name,
                    v.len()
                ));
            }
            Err(_) => {
                *prov
                    .observer_unresolvable_stations
                    .entry(stn.to_string())
                    .or_insert(0) += 1;
                prov.n_rows_observer_unresolvable += 1;
            }
        }
    }
    if prov.n_rows_observer_unresolvable > 0 {
        let rows: Vec<Vec<String>> = keep.iter().map(|&i| table.rows[i].clone()).collect();
        table.rows = rows;
    }
    if table.rows.is_empty() {
        prov.failure = Some("no rows with a resolvable observer".into());
        return Ok(failed(prov, table));
    }
    let mut nominal = truth.clone();
    // The ephemeris is a point prediction; the truth carries no uncertainty.
    nominal.state.covariance = None;
    nominal.state.non_grav_cross = None;
    nominal.ng_covariance = None;
    nominal.wide_cross = None;
    let eph_cfg = EphemerisConfig {
        propagation: empyrean::PropagationConfig {
            force_model: args.tier,
            excluded_perturbers: cfg.excluded_perturbers.clone(),
            ..empyrean::PropagationConfig::default()
        },
        ..EphemerisConfig::default()
    };
    let eph = ctx
        .generate_ephemeris(std::slice::from_ref(&nominal), &observers, &eph_cfg)
        .map_err(|e| format!("{}: generate_ephemeris: {e}", obj.name))?;
    if eph.entries.len() != observers.len() {
        return Err(format!(
            "{}: generate_ephemeris returned {} entries for {} observers",
            obj.name,
            eph.entries.len(),
            observers.len()
        ));
    }
    prov.ephemeris_ms = t1.elapsed().as_secs_f64() * 1000.0;
    let positions: Vec<(f64, f64)> = eph.entries.iter().map(|e| (e.ra_deg, e.dec_deg)).collect();

    Ok(TruthProduct {
        prov,
        table,
        positions,
    })
}

/// Noise + rewrite for one law: the PSV text and the completed provenance.
fn lane_for_object(
    truth: &TruthProduct,
    model: NoiseModel,
    args: &SynthArgs,
) -> Result<(ObjectProvenance, String), String> {
    let mut prov = truth.prov.clone();
    let mut table = truth.table.clone();
    if truth.positions.is_empty() {
        table.rows.clear();
        return Ok((prov, table.render()));
    }
    let rows = synthesize_table(
        &prov.object,
        &mut table,
        &truth.positions,
        model,
        args.fiducial_sigma_arcsec,
        args.seed,
    )?;
    for r in &rows {
        *prov
            .sigma_sources
            .entry(r.sigma_source.clone())
            .or_insert(0) += 1;
        *prov
            .sigma_histogram
            .entry(sigma_key(r.sigma_arcsec))
            .or_insert(0) += 1;
    }
    prov.n_rows_written = rows.len();
    prov.self_check = Some(noise_self_check(&rows));
    Ok((prov, table.render()))
}

/// Run the lanes: one truth pass per selected object, then per noise law
/// the fixture set, the regenerated manifest, and the provenance. Returns
/// one provenance per law, in `args.models` order.
pub fn run(ctx: &Context, args: &SynthArgs) -> Result<Vec<SyntheticProvenance>, String> {
    if args.models.is_empty() {
        return Err("no noise law given".into());
    }
    let source_snapshot = read_source_snapshot(&args.fixtures_dir)?;
    let objects: Vec<&ValidationObject> = empyrean_validation::catalog::all_objects()
        .into_iter()
        .filter(|o| args.only.is_empty() || args.only.iter().any(|n| n == o.name))
        .collect();
    if objects.is_empty() {
        return Err("no catalog objects match --only".into());
    }
    if !args.only.is_empty() {
        eprintln!(
            "synthesize: --only given — the manifest generator requires every catalog \
             fixture, so unselected objects get header-only PSVs (ineligible)"
        );
    }
    eprintln!(
        "synthesize: {} objects, laws [{}], seed {}, fiducial {}″, ast_cat {} → {}",
        objects.len(),
        args.models
            .iter()
            .map(|m| m.tag())
            .collect::<Vec<_>>()
            .join(", "),
        args.seed,
        args.fiducial_sigma_arcsec,
        SYNTHETIC_AST_CAT,
        args.out_root.display()
    );

    // ── truth pass (parallel over objects; one fit + one ephemeris each) ──
    let results: Vec<Result<TruthProduct, String>> = objects
        .par_iter()
        .map(|obj| {
            let r = truth_for_object(ctx, obj, args);
            match &r {
                Ok(t) => eprintln!(
                    "  {:<16} {} rows ({} space-based dropped, {} observer-unresolvable) — {}",
                    t.prov.object,
                    t.table.rows.len(),
                    t.prov.n_rows_space_based_dropped,
                    t.prov.n_rows_observer_unresolvable,
                    match (&t.prov.truth, &t.prov.failure) {
                        (Some(o), _) => format!(
                            "truth fit ok: {} used / {} rejected, {} params, rchi2 {:.3}, \
                             {:.1}s fit + {:.1}s ephemeris",
                            o.n_obs_used,
                            o.n_obs_rejected,
                            o.n_solve_for,
                            o.reduced_chi2,
                            t.prov.truth_fit_ms / 1000.0,
                            t.prov.ephemeris_ms / 1000.0
                        ),
                        (None, Some(f)) => format!("FAILED: {f}"),
                        (None, None) => "FAILED: unknown".into(),
                    }
                ),
                Err(e) => eprintln!("  ERROR: {e}"),
            }
            r
        })
        .collect();
    let mut truths = Vec::new();
    let mut errors = Vec::new();
    for r in results {
        match r {
            Ok(t) => truths.push(t),
            Err(e) => errors.push(e),
        }
    }
    if !errors.is_empty() {
        return Err(format!(
            "{} objects errored; first: {}",
            errors.len(),
            errors[0]
        ));
    }
    // Unselected catalog objects: header-only tables so the generator's
    // every-fixture invariant holds and they land as ineligible.
    let mut header_only: Vec<(String, String)> = Vec::new();
    for obj in empyrean_validation::catalog::all_objects() {
        if objects.iter().any(|o| o.name == obj.name) {
            continue;
        }
        let src = resolve_fixture(&args.fixtures_dir, obj)?;
        let text =
            std::fs::read_to_string(&src).map_err(|e| format!("read {}: {e}", src.display()))?;
        let mut t = PsvTable::parse(&text).map_err(|e| format!("{}: {e}", src.display()))?;
        t.rows.clear();
        header_only.push((obj.name.to_string(), t.render()));
    }

    // ── one lane per law ──
    let mut out = Vec::new();
    for &model in &args.models {
        let lane_dir = args.out_root.join(model.tag());
        let psv_dir = lane_dir.join("psv");
        std::fs::create_dir_all(&psv_dir)
            .map_err(|e| format!("mkdir {}: {e}", psv_dir.display()))?;
        let snapshot_id = synthetic_snapshot_id(&source_snapshot, model, args.seed);
        let mut provs = Vec::new();
        for t in &truths {
            let (p, text) = lane_for_object(t, model, args)?;
            let path = psv_dir.join(format!("{}.psv", p.object.replace('/', "_")));
            std::fs::write(&path, text).map_err(|e| format!("write {}: {e}", path.display()))?;
            provs.push(p);
        }
        for (name, text) in &header_only {
            std::fs::write(
                psv_dir.join(format!("{}.psv", name.replace('/', "_"))),
                text,
            )
            .map_err(|e| format!("write header-only {name}: {e}"))?;
        }

        // Sibling fixture manifest: the snapshot id the walk cross-checks.
        let fixture_manifest = serde_json::json!({
            "snapshot_id": snapshot_id,
            "source_snapshot_id": source_snapshot,
            "synthetic": true,
            "noise": model,
            "seed": args.seed,
        });
        std::fs::write(
            lane_dir.join("manifest.json"),
            serde_json::to_string_pretty(&fixture_manifest).unwrap(),
        )
        .map_err(|e| format!("write manifest.json: {e}"))?;

        // Window manifest against the synthetic fixtures: identical schedule
        // (times unchanged), σ resolved from the written rmsRA/rmsDec.
        let windows_path = lane_dir.join("windows.json");
        windows::run(
            &psv_dir,
            &windows_path,
            &WindowsOptions {
                bundle_nights: 1,
                debias_dir: Some(args.debias_dir.clone()),
                no_debias: false,
            },
        )?;

        let prov = SyntheticProvenance {
            schema_version: SYNTHETIC_SCHEMA_VERSION,
            snapshot_id,
            source_snapshot_id: source_snapshot.clone(),
            noise: model,
            seed: args.seed,
            fiducial_sigma_arcsec: args.fiducial_sigma_arcsec,
            survey_sigma_table: "vfc17_station_floors".into(),
            ast_cat: SYNTHETIC_AST_CAT.into(),
            engine_version: empyrean::version_string().ok(),
            force_model_tier: args.tier_name.clone(),
            generated_by: format!("{}/{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")),
            objects: provs,
        };
        let prov_path = lane_dir.join("synthetic.json");
        std::fs::write(
            &prov_path,
            serde_json::to_string_pretty(&prov)
                .map_err(|e| format!("serialize provenance: {e}"))?,
        )
        .map_err(|e| format!("write {}: {e}", prov_path.display()))?;

        let n_ok = prov.objects.iter().filter(|o| o.truth.is_some()).count();
        eprintln!(
            "synthesize [{}]: {} / {} truth fits converged; provenance → {}",
            model.tag(),
            n_ok,
            prov.objects.len(),
            prov_path.display()
        );
        for o in prov.objects.iter().filter(|o| o.truth.is_none()) {
            eprintln!(
                "  ineligible: {} — {}",
                o.object,
                o.failure.as_deref().unwrap_or("?")
            );
        }
        out.push(prov);
    }
    Ok(out)
}
