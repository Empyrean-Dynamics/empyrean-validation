//! adam-core's released public Rust API as an external reference for the
//! empyrean validation suite: tool id `adam-core-rust`.
//!
//! The same release as the Python channel (`runners/adam-core`), run through
//! its Rust crates instead of its PyPI wheels: `adam_core` 0.8.0 for orbit
//! determination, observers, SPICE and time, and `adam_assist` 0.7.0 for
//! ASSIST N-body propagation. adam-core's own crates carry no N-body
//! integrator (only two-body, which this channel never uses).
//!
//! * propagation: `AssistPropagator::propagate`, plan IC (SSB equatorial, TDB)
//!   to `t_mjd_tdb`, at adam_assist's public default integrator;
//! * ephemeris: `AssistPropagator::generate_ephemeris` with
//!   `EphemerisOptions::default()`, observers from
//!   `AdamCoreSpiceBackend::observer_states_from_codes`;
//! * orbit determination: `adam_core::run_od` under the public `CORE_OD_V1`
//!   profile with ASSIST as the ABI-v2 provider, from ADES PSV through
//!   adam-core's Rust reader, missing sigma filled by the shared public Vereš
//!   2017 model (`runners/sigmafill`).
//!
//! Every file comes from the `adam-core-rust/` folder of the third-party tool
//! set by explicit path. Every row is a plan row plus `adam_core_rust_*` fields.

mod data;
mod od;
mod plan;

use adam_assist::AssistPropagator;
use adam_core::coords::od::profiles::CORE_OD_V1;
use adam_core::coords::propagation::{
    CovariancePropagation, EphemerisOptions, PropagationOptions, PropagationRequest, Propagator,
};
use adam_core::coords::{
    CoordinateBatch, DataFrame, ObservatoryCode, ObserverBatch, OriginArray, OriginId, TimeArray,
    TimeScale, ades_string_to_observations,
};
use adam_core::coords::{SchemaError, TimeScaleProvider};
use adam_core::od::{OdPropagator, ProviderMetadata};
use adam_core::spice::{AdamCoreSpiceBackend, global_backend};
use data::DataDir;
use od::SigmaFill;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The caller-owned time-scale provider adam-core 0.8 asks for in place of the
/// retired `ErfaTimeProvider`: it delegates to `TimeArray::rescale`, which
/// routes every UTC edge through ERFA's absolute two-part Julian date.
struct RescaleTime;

impl TimeScaleProvider for RescaleTime {
    fn rescale(&self, times: &TimeArray, new_scale: TimeScale) -> Result<TimeArray, SchemaError> {
        times.rescale(new_scale)
    }
}

const TOOL: &str = "adam-core-rust";
const PREFIX: &str = "adam_core_rust";
const AU_KM: f64 = 149_597_870.7;
const AXES: [&str; 3] = ["propagation", "ephemeris", "orbit_determination"];

#[derive(Debug, Default)]
struct Args {
    plan: Option<PathBuf>,
    output: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    fixtures_dir: Option<PathBuf>,
    sigmafill: Option<PathBuf>,
    python: PathBuf,
    only: Option<Vec<String>>,
    objects: Option<Vec<String>>,
    timeout_min: f64,
    od_one: Option<String>,
    od_one_mpc: Option<String>,
}

const USAGE: &str = "run-adam-core-rust --plan PLAN.json --output OUT.json --data-dir <third-party>/adam-core-rust \
--fixtures-dir fixtures/psv --sigmafill runners/sigmafill/fill_json.py [--python python3] \
[--only propagation,ephemeris,orbit_determination] [--objects A,B] [--timeout-min 30]";

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut a = Args {
        python: PathBuf::from("python3"),
        timeout_min: 30.0,
        ..Default::default()
    };
    let list = |s: &str| -> Vec<String> {
        s.split(',')
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(String::from)
            .collect()
    };
    let mut it = argv.iter();
    while let Some(flag) = it.next() {
        if flag == "-h" || flag == "--help" {
            return Err(USAGE.into());
        }
        let val = it
            .next()
            .ok_or_else(|| format!("{flag} needs a value\n{USAGE}"))?;
        match flag.as_str() {
            "--plan" | "--input" => a.plan = Some(val.into()),
            "--output" => a.output = Some(val.into()),
            "--data-dir" => a.data_dir = Some(val.into()),
            "--fixtures-dir" => a.fixtures_dir = Some(val.into()),
            "--sigmafill" => a.sigmafill = Some(val.into()),
            "--python" => a.python = val.into(),
            "--only" => {
                let axes = list(val);
                if let Some(bad) = axes.iter().find(|x| !AXES.contains(&x.as_str())) {
                    return Err(format!("--only: unknown axis {bad} (one of {AXES:?})"));
                }
                a.only = Some(axes);
            }
            "--objects" => a.objects = Some(list(val)),
            "--timeout-min" => {
                a.timeout_min = val
                    .parse()
                    .map_err(|_| format!("--timeout-min {val} is not a number"))?
            }
            "--od-one" => a.od_one = Some(val.clone()),
            "--od-one-mpc" => a.od_one_mpc = Some(val.clone()),
            other => return Err(format!("unknown flag {other}\n{USAGE}")),
        }
    }
    Ok(a)
}

fn field(name: &str) -> String {
    format!("{PREFIX}_{name}")
}

/// Insert the runner's `name -> value` fields under the prefix.
fn put(row: &mut Map<String, Value>, fields: Map<String, Value>) {
    for (k, v) in fields {
        row.insert(field(&k), v);
    }
}

fn num(x: f64) -> Option<Value> {
    x.is_finite().then(|| json!(x))
}

fn ms(t0: Instant) -> Value {
    json!(t0.elapsed().as_secs_f64() * 1e3)
}

/// The default kernel set, furnished in `setup_SPICE` order, and the registered
/// observatory table: the pure-Rust equivalent of adam-core's
/// `setup_spice_backend`, from explicit paths.
fn setup_spice(backend: &mut AdamCoreSpiceBackend, data: &DataDir) -> Result<usize, String> {
    for id in adam_core::kernel_data::DEFAULT_SPICE_KERNEL_IDS {
        let p = data.by_id(id);
        backend
            .furnsh(&p)
            .map_err(|e| format!("furnsh {}: {e}", p.display()))?;
    }
    let p = data.by_id("mpc_obscodes");
    let json =
        std::fs::read_to_string(&p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
    backend
        .load_mpc_obscodes(&json)
        .map_err(|e| format!("load_mpc_obscodes {}: {e}", p.display()))
}

/// `name version` of the propagator that fits the object.
fn provider_label(identity: &ProviderMetadata) -> String {
    format!("{} {}", identity.name, identity.version)
}

/// Furnish the default kernel set and the observatory table into the
/// process-global backend under its write guard, which is released on return:
/// adam-core 0.8 takes a shared read guard for every OD call and fails runtime
/// kernel mutation while one is held, so no load may follow the first fit.
fn init_spice(data: &DataDir) -> Result<usize, String> {
    let mut backend = global_backend()
        .write()
        .map_err(|_| "SPICE backend poisoned")?;
    setup_spice(&mut backend, data)
}

/// Can adam-core place this observatory code (a ground site in the registered
/// table, or a SPICE body)? Probed at one epoch, as the Python channel does.
fn resolvable(backend: &AdamCoreSpiceBackend, code: &str) -> bool {
    let Ok(t) = TimeArray::from_mjd(TimeScale::Tdb, &[60000.0]) else {
        return false;
    };
    backend
        .observer_states_from_codes(
            &[code.to_string()],
            &[0],
            &t,
            DataFrame::Ecliptic,
            &OriginId::from_code("SUN"),
        )
        .is_ok_and(|s| s.iter().take(3).all(|v| v.is_finite()))
}

/// The fields of a row adam_assist does not run because the object is one of
/// its own perturbers (decided from `excluded_perturbers_naif`), or None.
fn self_perturber_fields(row: &Value) -> Option<Map<String, Value>> {
    let why = plan::self_perturber_reason(row)?;
    let mut out = Map::new();
    out.insert("force_model".into(), json!(plan::FORCE_MODEL));
    out.insert("unsupported".into(), json!(why));
    Some(out)
}

fn propagation(row: &Value, assist: &AssistPropagator) -> Map<String, Value> {
    if let Some(out) = self_perturber_fields(row) {
        return out;
    }
    let mut out = Map::new();
    out.insert("force_model".into(), json!(plan::FORCE_MODEL));
    let mut run = || -> Result<(), String> {
        let (orbits, ng_reason) = plan::plan_orbit(row)?;
        if let Some(r) = ng_reason {
            out.insert("unsupported".into(), json!(r));
        }
        let t = plan::f64_field(row, "t_mjd_tdb").ok_or("plan row lacks t_mjd_tdb")?;
        let times = TimeArray::from_mjd(TimeScale::Tdb, &[t]).map_err(|e| e.to_string())?;
        let options = PropagationOptions {
            covariance: CovariancePropagation::None,
            ..PropagationOptions::default()
        };
        let request =
            PropagationRequest::new(&orbits, &times, options).map_err(|e| e.to_string())?;
        let t0 = Instant::now();
        let res = assist
            .propagate(&request, &RescaleTime)
            .map_err(|e| e.to_string())?;
        out.insert("time_ms".into(), ms(t0));
        let c = &res.orbits.coordinates;
        if c.frame != DataFrame::Equatorial
            || c.origins.origins[0] != OriginId::SolarSystemBarycenter
        {
            return Err(format!(
                "propagated state came back in {:?} / {:?}, not SSB equatorial",
                c.frame, c.origins.origins[0]
            ));
        }
        let s = c
            .values
            .cartesian()
            .ok_or("propagated state is not Cartesian")?[0];
        if !s[..3].iter().all(|v| v.is_finite()) {
            return Err("non-finite propagated state".into());
        }
        out.insert("pos_au".into(), json!(&s[..3]));
        if let Some(r) = row["ref_pos_au"].as_array() {
            let d: f64 = (0..3)
                .map(|i| (s[i] - r[i].as_f64().unwrap_or(f64::NAN)).powi(2))
                .sum::<f64>()
                .sqrt();
            if let Some(v) = num(d * AU_KM) {
                out.insert("vs_horizons_km".into(), v);
            }
        }
        Ok(())
    };
    if let Err(e) = run() {
        let why = out
            .get("unsupported")
            .and_then(Value::as_str)
            .map(String::from)
            .unwrap_or_else(|| format!("propagation error: {e}"));
        out.insert("unsupported".into(), json!(why));
    }
    out
}

fn angular_sep_arcsec(ra1: f64, dec1: f64, ra2: f64, dec2: f64) -> f64 {
    let (c1, c2, s1, s2) = (dec1.cos(), dec2.cos(), dec1.sin(), dec2.sin());
    let dra = ra2 - ra1;
    let n1 = c2 * dra.sin();
    let n2 = c1 * s2 - s1 * c2 * dra.cos();
    let den = s1 * s2 + c1 * c2 * dra.cos();
    (n1.hypot(n2)).atan2(den).to_degrees() * 3600.0
}

fn ephemeris(
    row: &Value,
    assist: &AssistPropagator,
    backend: &AdamCoreSpiceBackend,
) -> Map<String, Value> {
    if let Some(out) = self_perturber_fields(row) {
        return out;
    }
    let mut out = Map::new();
    out.insert("force_model".into(), json!(plan::FORCE_MODEL));
    let Some(code) = row["observer"].as_str() else {
        return out;
    };
    if !resolvable(backend, code) {
        out.insert("unsupported".into(), json!("space-based observer"));
        return out;
    }
    let mut run = || -> Result<(), String> {
        let (orbits, ng_reason) = plan::plan_orbit(row)?;
        if let Some(r) = ng_reason {
            out.insert("unsupported".into(), json!(r));
        }
        // adam-core's typed ephemeris takes ecliptic Cartesian orbits and observers.
        let orbits = orbits
            .rotate_frame(DataFrame::Ecliptic)
            .map_err(|e| e.to_string())?;
        let t = plan::f64_field(row, "t_mjd_tdb").ok_or("plan row lacks t_mjd_tdb")?;
        let times = TimeArray::from_mjd(TimeScale::Tdb, &[t]).map_err(|e| e.to_string())?;
        let sun = OriginId::from_code("SUN");
        let s = backend
            .observer_states_from_codes(
                &[code.to_string()],
                &[0],
                &times,
                DataFrame::Ecliptic,
                &sun,
            )
            .map_err(|e| format!("observer state: {e}"))?;
        let observers = ObserverBatch::new(
            vec![ObservatoryCode(code.to_string())],
            CoordinateBatch::cartesian(
                vec![[s[0], s[1], s[2], s[3], s[4], s[5]]],
                DataFrame::Ecliptic,
                OriginArray::repeat(sun, 1),
                Some(times),
                None,
            )
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let t0 = Instant::now();
        let res = assist
            .generate_ephemeris(
                &orbits,
                &observers,
                &EphemerisOptions::default(),
                &RescaleTime,
                backend,
            )
            .map_err(|e| e.to_string())?;
        out.insert("time_ms".into(), ms(t0));
        let sph = res
            .ephemeris
            .coordinates
            .values
            .spherical()
            .ok_or("ephemeris is not spherical")?[0];
        let (rho, ra, dec) = (sph[0], sph[1].to_radians(), sph[2].to_radians());
        if !(rho.is_finite() && ra.is_finite() && dec.is_finite()) {
            return Err("non-finite ephemeris".into());
        }
        if let (Some(rr), Some(rd)) = (
            plan::f64_field(row, "ref_ra_rad"),
            plan::f64_field(row, "ref_dec_rad"),
        ) {
            out.insert(
                "separation_arcsec".into(),
                json!(angular_sep_arcsec(ra, dec, rr, rd)),
            );
            out.insert(
                "d_ra_arcsec".into(),
                json!(((ra - rr) * dec.cos()).to_degrees() * 3600.0),
            );
            out.insert(
                "d_dec_arcsec".into(),
                json!((dec - rd).to_degrees() * 3600.0),
            );
        }
        if let Some(rr) = plan::f64_field(row, "ref_rho_au") {
            out.insert("d_rho_km".into(), json!((rho - rr) * AU_KM));
        }
        Ok(())
    };
    if let Err(e) = run() {
        let why = out
            .get("unsupported")
            .and_then(Value::as_str)
            .map(String::from)
            .unwrap_or_else(|| format!("ephemeris error: {e}"));
        out.insert("unsupported".into(), json!(why));
    }
    out
}

/// Fit one object in this process (the `--od-one` child), returning the
/// runner's fields without the prefix.
fn od_one(
    object: &str,
    mpc: Option<&str>,
    args: &Args,
    data: &DataDir,
) -> Result<Map<String, Value>, String> {
    let mut out = Map::new();
    out.insert("force_model".into(), json!(plan::FORCE_MODEL));
    out.insert("profile".into(), json!(CORE_OD_V1.identity().name));
    let fixtures = args
        .fixtures_dir
        .as_ref()
        .ok_or("OD needs --fixtures-dir")?;
    let script = args.sigmafill.as_ref().ok_or("OD needs --sigmafill")?;
    let (psv, tried) = od::resolve_fixture(fixtures, object, mpc);
    let Some(psv) = psv else {
        out.insert(
            "unsupported".into(),
            json!(format!("no PSV fixture (tried {tried:?})")),
        );
        out.insert("converged".into(), json!(false));
        return Ok(out);
    };
    let text =
        std::fs::read_to_string(&psv).map_err(|e| format!("cannot read {}: {e}", psv.display()))?;
    let (ades, _unknown) = match ades_string_to_observations(&od::normalize_psv(&text)) {
        Ok(v) => v,
        Err(e) => {
            out.insert(
                "unsupported".into(),
                json!(format!("adam-core's ADES reader refused the fixture: {e}")),
            );
            out.insert("converged".into(), json!(false));
            return Ok(out);
        }
    };
    let fill = SigmaFill {
        python: args.python.clone(),
        script: script.clone(),
    };
    // Every kernel and site load finishes under the write guard, which is
    // released before the read guard that observer preparation and (inside
    // `run_od`) the fit itself hold.
    init_spice(data)?;
    let prepared = {
        let backend = global_backend()
            .read()
            .map_err(|_| "SPICE backend poisoned")?;
        od::prepare(
            object,
            &ades,
            &|code| resolvable(&backend, code),
            &fill,
            &backend,
        )?
    };
    out.insert("n_obs_parsed".into(), json!(prepared.n_parsed));
    out.insert("n_obs_dropped_spacebased".into(), json!(prepared.n_dropped));
    if prepared.n_dropped > 0 {
        out.insert("dropped_stations".into(), json!(prepared.dropped_stations));
    }
    out.insert("sigma_fill".into(), prepared.sigma_report["model"].clone());
    out.insert("sigma_source".into(), prepared.sigma_report.clone());
    let Some(input) = prepared.input else {
        out.insert(
            "unsupported".into(),
            json!("all observations at space-based stations"),
        );
        out.insert("converged".into(), json!(false));
        return Ok(out);
    };
    let config = od::core_od_v1(&data.path(data::BIAS_TABLE))?;
    let assist = AssistPropagator::from_paths(data.by_id("de440"), data.by_id("sb441_n16"))
        .map_err(|e| e.to_string())?;
    let external = assist.external_propagator().map_err(|e| e.to_string())?;
    let provider = OdPropagator::external(external.external());
    // The provider's provenance-complete identity, read the way adam-core 0.8
    // documents (refused here, by name, if the ABI v2 fingerprints are missing).
    let identity = ProviderMetadata::from_external_propagator(external.external())
        .map_err(|e| e.to_string())?;
    out.insert("propagator".into(), json!(provider_label(&identity)));
    let t0 = Instant::now();
    let result = adam_core::run_od(&input, &config, &provider);
    out.insert("time_ms".into(), ms(t0));
    match result {
        Err(e) => {
            out.insert("converged".into(), json!(false));
            out.insert(
                "unsupported".into(),
                json!(
                    format!("run_od refused: {e}")
                        .chars()
                        .take(400)
                        .collect::<String>()
                ),
            );
        }
        Ok(result) => {
            let backend = global_backend()
                .read()
                .map_err(|_| "SPICE backend poisoned")?;
            out.extend(od::summarize(&result, &backend)?);
        }
    }
    Ok(out)
}

/// Run one object's fit in a child process, so a hung fit is a hard kill at
/// the timeout rather than a stalled leg.
fn od_child(args: &Args, row: &Value) -> Map<String, Value> {
    let object = row["object"].as_str().unwrap_or_default();
    let exe = std::env::current_exe().expect("own executable");
    let mut cmd = Command::new(exe);
    let path = |p: &Option<PathBuf>| p.as_ref().map(|p| p.display().to_string());
    cmd.arg("--od-one").arg(object);
    if let Some(m) = row["mpc_designation"].as_str() {
        cmd.arg("--od-one-mpc").arg(m);
    }
    for (flag, v) in [
        ("--data-dir", path(&args.data_dir)),
        ("--fixtures-dir", path(&args.fixtures_dir)),
        ("--sigmafill", path(&args.sigmafill)),
    ] {
        if let Some(v) = v {
            cmd.arg(flag).arg(v);
        }
    }
    cmd.arg("--python").arg(&args.python);
    cmd.stdout(Stdio::piped()).stderr(Stdio::inherit());
    let mut out = Map::new();
    let t0 = Instant::now();
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            out.insert("converged".into(), json!(false));
            out.insert("unsupported".into(), json!(format!("od subprocess: {e}")));
            return out;
        }
    };
    let mut stdout = child.stdout.take().expect("piped stdout");
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let limit = Duration::from_secs_f64(args.timeout_min * 60.0);
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if t0.elapsed() > limit => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => break None,
        }
    };
    let text = reader.join().unwrap_or_default();
    out.insert("wall_s".into(), json!(t0.elapsed().as_secs_f64()));
    match status {
        None => {
            out.insert("timed_out".into(), json!(true));
            out.insert("converged".into(), json!(false));
            out.insert("force_model".into(), json!(plan::FORCE_MODEL));
            out.insert(
                "unsupported".into(),
                json!(format!("timed out after {} min", args.timeout_min)),
            );
        }
        Some(st) if !st.success() => {
            out.insert("converged".into(), json!(false));
            out.insert(
                "unsupported".into(),
                json!(format!("od subprocess exit {st}")),
            );
        }
        Some(_) => match serde_json::from_str::<Map<String, Value>>(&text) {
            Ok(m) => out.extend(m),
            Err(e) => {
                out.insert("converged".into(), json!(false));
                out.insert(
                    "unsupported".into(),
                    json!(format!("od subprocess wrote unreadable JSON: {e}")),
                );
            }
        },
    }
    out
}

/// The settings record: the crates' own values plus every registered file.
fn settings(
    data: &DataDir,
    assist: &AssistPropagator,
    args: &Args,
) -> Result<BTreeMap<String, String>, String> {
    let id = adam_core::build_identity();
    let b = assist.backend();
    let integ = assist.integrator();
    let show = |v: Option<f64>| v.map_or("unset".to_string(), |x| format!("{x:e}"));
    let profile = CORE_OD_V1.identity();
    let def = CORE_OD_V1.definition_value();
    let eph = EphemerisOptions::default();
    let mut facts = BTreeMap::new();
    let mut put = |k: &str, v: String| {
        facts.insert(k.to_string(), v);
    };
    put(
        "api",
        format!(
            "adam_core {} (source {}) + adam_assist {}: the released crates, public Rust API, no Python",
            id.version, id.commit, b.version
        ),
    );
    put(
        "profile",
        format!(
            "{} v{} (sha256 {})",
            profile.name, profile.version, profile.definition_sha256
        ),
    );
    put("observation_pipeline", def["pipeline"].to_string());
    put("iod", def["iod"].to_string());
    put("refinement", def["refinement"].to_string());
    put(
        "propagator",
        format!(
            "{} {} (ASSIST IAS15), AssistPropagator::from_paths at its public default integrator",
            b.name, b.version
        ),
    );
    put("epsilon", show(integ.epsilon));
    put("min_dt", show(integ.min_dt));
    put("initial_dt", show(integ.initial_dt));
    put(
        "adaptive_mode",
        integ
            .adaptive_mode
            .map_or("unset".to_string(), |m| format!("{m:?}")),
    );
    put(
        "assist_data_fingerprint",
        assist
            .data_fingerprint()
            .map_err(|e| e.to_string())?
            .to_string(),
    );
    put(
        "ephemeris_options",
        format!(
            "EphemerisOptions::default(): lt_tol={:e}, max_iter={}, tol={:e}, stellar_aberration={}, max_lt_iter={}",
            eph.lt_tol, eph.max_iter, eph.tol, eph.stellar_aberration, eph.max_lt_iter
        ),
    );
    if let Some(script) = &args.sigmafill {
        let table = script.with_file_name("veres2017_sigma_table.csv");
        put(
            "sigma_fill",
            format!(
                "Veres2017SigmaFill through runners/sigmafill/fill_json.py (table sha256 {}); CORE_OD_V1 has no fill stage",
                data::sha256_file(&table)?
            ),
        );
    }
    put(
        "observation_covariance",
        "ades_angular_covariance_flat: lon sigma rmsRA/cos(dec), lat sigma rmsDec, null rmsCorr 0"
            .into(),
    );
    put(
        "od_input",
        "ADES PSV through adam_core_rs_coords::ades_string_to_observations".into(),
    );
    put(
        "self_perturbers",
        "a row whose excluded_perturbers_naif names a SB441-N16 body: named unsupported \
         (adam_assist cannot exclude a body)"
            .into(),
    );
    put("non_grav", "A1/A2/A3 + g(r); no Marsden DT term".into());
    put(
        "od_timeout_min",
        format!(
            "{}: each object is fit in its own child process and killed at this wall cap; \
             a capped object is a named row (timed_out, its reason), never dropped",
            args.timeout_min
        ),
    );
    put(
        "space_based_observers",
        "dropped and counted (not a ground site in the registered obscodes table, not a SPICE body)"
            .into(),
    );
    data::settings_record(data, &facts)
}

fn run(args: Args) -> Result<(), String> {
    let data = DataDir::open(args.data_dir.as_deref().ok_or(USAGE)?)?;
    if let Some(object) = &args.od_one {
        let fields = od_one(object, args.od_one_mpc.as_deref(), &args, &data)?;
        println!(
            "{}",
            serde_json::to_string(&fields).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    let plan_path = args.plan.as_ref().ok_or(USAGE)?;
    let output = args.output.as_ref().ok_or(USAGE)?;
    let text = std::fs::read_to_string(plan_path)
        .map_err(|e| format!("cannot read {}: {e}", plan_path.display()))?;
    let mut plan: Vec<Value> = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    if plan.is_empty() {
        return Err("input plan empty".into());
    }
    if let Some(objs) = &args.objects {
        plan = select_objects(plan, objs)?;
    }
    let axes: Vec<String> = args
        .only
        .clone()
        .unwrap_or_else(|| AXES.iter().map(|s| s.to_string()).collect());
    if axes.iter().any(|a| a == "orbit_determination")
        && (args.sigmafill.is_none() || args.fixtures_dir.is_none())
    {
        return Err("orbit determination needs --fixtures-dir and --sigmafill".into());
    }
    let assist = AssistPropagator::from_paths(data.by_id("de440"), data.by_id("sb441_n16"))
        .map_err(|e| e.to_string())?;
    let settings = settings(&data, &assist, &args)?;
    let id = adam_core::build_identity();
    let source_version = format!(
        "adam_core {} + adam_assist {} (Rust API)",
        id.version,
        assist.backend().version
    );
    let timestamp = {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        format!("unix:{secs}")
    };
    let base = |row: &Value| -> Map<String, Value> {
        let mut m = row.as_object().cloned().unwrap_or_default();
        m.insert("channel".into(), json!(TOOL));
        m.insert("timestamp".into(), json!(timestamp));
        m.insert("source_version".into(), json!(source_version));
        m.insert(field("version"), json!(id.version));
        m.insert(field("settings"), json!(settings));
        m
    };
    let mut rows: Vec<Value> = Vec::new();
    if axes.iter().any(|a| a == "propagation") {
        let todo = plan::rows_for(&plan, "propagation");
        for r in &todo {
            let mut m = base(r);
            put(&mut m, propagation(r, &assist));
            rows.push(Value::Object(m));
        }
        eprintln!("propagation: {} rows", todo.len());
    }
    if axes.iter().any(|a| a == "ephemeris") {
        let todo = plan::rows_for(&plan, "ephemeris");
        let n = init_spice(&data)?;
        let backend = global_backend()
            .read()
            .map_err(|_| "SPICE backend poisoned")?;
        eprintln!("SPICE: default kernel set furnished, {n} ground sites");
        for r in &todo {
            let mut m = base(r);
            put(&mut m, ephemeris(r, &assist, &backend));
            rows.push(Value::Object(m));
        }
        eprintln!("ephemeris: {} rows", todo.len());
    }
    if axes.iter().any(|a| a == "orbit_determination") {
        let todo = plan::rows_for(&plan, "orbit_determination");
        eprintln!("orbit_determination: {} objects", todo.len());
        for (i, r) in todo.iter().enumerate() {
            let mut m = base(r);
            // One line before and one after each fit, so a log shows where the
            // time goes and which object a stalled leg was on.
            eprintln!(
                "  [{}/{}] {} ... fitting (cap {} min)",
                i + 1,
                todo.len(),
                r["object"].as_str().unwrap_or("?"),
                args.timeout_min
            );
            let fields = match self_perturber_fields(r) {
                Some(mut f) => {
                    f.insert("converged".into(), json!(false));
                    f.insert("profile".into(), json!(CORE_OD_V1.identity().name));
                    f
                }
                None => od_child(&args, r),
            };
            eprintln!(
                "  {:24} {:8.1}s converged={} rchi2={}{}",
                r["object"].as_str().unwrap_or("?"),
                fields.get("wall_s").and_then(Value::as_f64).unwrap_or(0.0),
                fields.get("converged").unwrap_or(&Value::Null),
                fields.get("reduced_chi2").unwrap_or(&Value::Null),
                if fields.contains_key("timed_out") {
                    "  TIMED OUT"
                } else {
                    ""
                }
            );
            put(&mut m, fields);
            rows.push(Value::Object(m));
        }
    }
    write_rows(&rows, output)?;
    eprintln!("Wrote {} rows to {}", rows.len(), output.display());
    Ok(())
}

/// The plan rows of the named objects (an object leg's `--objects`). A name
/// outside the plan, or an empty list, is refused by name: a leg must never
/// write an empty file for a typo.
fn select_objects(plan: Vec<Value>, objs: &[String]) -> Result<Vec<Value>, String> {
    if objs.is_empty() {
        return Err("--objects names no object".into());
    }
    let known: std::collections::BTreeSet<&str> =
        plan.iter().filter_map(|r| r["object"].as_str()).collect();
    let missing: Vec<&String> = objs
        .iter()
        .filter(|o| !known.contains(o.as_str()))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "--objects names objects outside the plan: {missing:?}"
        ));
    }
    Ok(plan
        .into_iter()
        .filter(|r| {
            r["object"]
                .as_str()
                .is_some_and(|o| objs.iter().any(|x| x == o))
        })
        .collect())
}

/// Write the rows as one JSON array. Values are built finite-only (a
/// non-finite number is never inserted), so the file is valid JSON for the
/// Rust merger.
fn write_rows(rows: &[Value], output: &Path) -> Result<(), String> {
    if let Some(dir) = output.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(rows).map_err(|e| e.to_string())?;
    std::fs::write(output, text).map_err(|e| format!("cannot write {}: {e}", output.display()))
}

fn main() {
    // Guards only: adam-core's resolver is never called (every path is passed
    // explicitly), and these make a stray call fail instead of fetching.
    // SAFETY: set before any thread is started.
    unsafe {
        std::env::set_var("ADAM_CORE_KERNEL_OFFLINE", "1");
        std::env::set_var("ADAM_CORE_KERNEL_PYTHON", "");
    }
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let code = match parse_args(&argv).and_then(run) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("ERROR: {e}");
            1
        }
    };
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn the_flags_parse_and_an_unknown_axis_or_flag_is_refused() {
        let a = parse_args(&argv(
            "--plan p.json --output o.json --data-dir d --only propagation,ephemeris --objects Apophis,Vesta --timeout-min 5",
        ))
        .unwrap();
        assert_eq!(a.only.unwrap(), vec!["propagation", "ephemeris"]);
        assert_eq!(a.objects.unwrap(), vec!["Apophis", "Vesta"]);
        assert_eq!((a.timeout_min, a.python), (5.0, PathBuf::from("python3")));
        assert!(
            parse_args(&argv("--only orbit"))
                .unwrap_err()
                .contains("unknown axis")
        );
        assert!(
            parse_args(&argv("--bogus 1"))
                .unwrap_err()
                .contains("unknown flag")
        );
    }

    #[test]
    fn fields_carry_the_tool_prefix_merge_external_folds() {
        let mut row = Map::new();
        let mut f = Map::new();
        f.insert("unsupported".into(), json!("why"));
        f.insert("vs_horizons_km".into(), json!(0.2));
        put(&mut row, f);
        assert_eq!(row["adam_core_rust_unsupported"], "why");
        assert_eq!(row["adam_core_rust_vs_horizons_km"], 0.2);
        assert_eq!(field("settings"), "adam_core_rust_settings");
    }

    #[test]
    fn a_self_perturber_row_is_named_unsupported_with_no_number() {
        let row = json!({"object": "Vesta", "test_type": "propagation",
                         "excluded_perturbers_naif": [2000004]});
        let out = self_perturber_fields(&row).unwrap();
        assert_eq!(out["unsupported"], plan::SELF_PERTURBER_UNSUPPORTED);
        assert_eq!(out["force_model"], plan::FORCE_MODEL);
        assert_eq!(out.len(), 2, "no position, time or comparison");
        assert!(self_perturber_fields(&json!({"object": "Apophis"})).is_none());
    }

    #[test]
    fn an_object_leg_names_only_plan_objects() {
        let plan = vec![json!({"object": "Apophis"}), json!({"object": "Vesta"})];
        let got = select_objects(plan.clone(), &["Vesta".into()]).unwrap();
        assert_eq!(got, vec![json!({"object": "Vesta"})]);
        let err = select_objects(plan.clone(), &["Vesta".into(), "Nope".into()]).unwrap_err();
        assert!(err.contains("[\"Nope\"]"), "{err}");
        assert!(select_objects(plan, &[]).unwrap_err().contains("no object"));
    }

    #[test]
    fn the_angular_separation_is_vincenty() {
        let a = angular_sep_arcsec(0.0, 0.0, 1e-6, 0.0);
        assert!((a - 1e-6f64.to_degrees() * 3600.0).abs() < 1e-9);
        assert!(angular_sep_arcsec(1.0, 0.5, 1.0, 0.5).abs() < 1e-12);
    }

    #[test]
    fn rows_are_written_as_valid_json() {
        let p = std::env::temp_dir().join(format!("acr-out-{}.json", std::process::id()));
        write_rows(&[json!({"object": "A", "adam_core_rust_time_ms": 1.5})], &p).unwrap();
        let back: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        std::fs::remove_file(&p).unwrap();
        assert_eq!(back[0]["adam_core_rust_time_ms"], 1.5);
        assert_eq!(num(f64::NAN), None);
    }

    #[test]
    fn the_time_provider_rescales_utc_to_tdb_by_the_leap_second_offset() {
        let utc = TimeArray::from_mjd(TimeScale::Utc, &[60000.0]).unwrap();
        let tdb = RescaleTime.rescale(&utc, TimeScale::Tdb).unwrap();
        assert_eq!(tdb.scale, TimeScale::Tdb);
        // TDB - UTC is 69.184 s in 2023 (37 leap seconds + 32.184 s, TDB-TT < 2 ms).
        let dt_s = (tdb.mjd_values()[0] - 60000.0) * 86400.0;
        assert!((dt_s - 69.184).abs() < 2e-3, "{dt_s}");
    }

    #[test]
    fn the_provider_label_is_the_identity_name_then_version() {
        let identity = ProviderMetadata {
            name: "assist-rs".into(),
            version: "0.7.0".into(),
            abi_version: 2,
            identity_validated_by_capsule: true,
            capabilities: 0,
            known_capabilities: 0,
            integration_time_scale: "tdb".into(),
            two_body_dynamics: false,
            configuration_fingerprint: String::new(),
            data_fingerprint: String::new(),
        };
        assert_eq!(provider_label(&identity), "assist-rs 0.7.0");
    }

    #[test]
    fn a_failed_kernel_load_releases_the_backend_write_guard() {
        // adam-core 0.8 fails runtime mutation while a reader is active and
        // OD takes a read guard: a load that left the write guard held (or
        // poisoned) would wedge every later fit. Every required file exists
        // but holds junk, so the first furnsh fails.
        let dir = std::env::temp_dir().join(format!("acr-spice-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for n in data::required_files() {
            std::fs::write(dir.join(n), b"x").unwrap();
        }
        let data = DataDir::open(&dir).unwrap();
        let err = init_spice(&data).unwrap_err();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(err.contains("furnsh"), "{err}");
        assert!(global_backend().try_write().is_ok(), "write guard leaked");
        assert!(global_backend().read().is_ok());
    }
}
