//! Orbit determination: ADES PSV through adam-core's Rust reader, the shared
//! sigma fill, then `adam_core::run_od` under the public `CORE_OD_V1` profile
//! with ASSIST as the ABI-v2 provider.

use adam_core::coords::observations::TimeColumn;
use adam_core::coords::od::OdConfig;
use adam_core::coords::od::profiles::CORE_OD_V1;
use adam_core::coords::od::{ResourceBindings, ResourceSlot};
use adam_core::coords::translation::normalize_coordinates_to;
use adam_core::coords::{
    AdesObservationBatch, AssociationBatch, CoordinateBatch, CoordinateRepresentation,
    CoordinateValues, CovarianceBatch, CovarianceUnits, DataFrame, Epoch, ObservatoryCode,
    ObserverBatch, OriginArray, OriginId, SourceCatalogBatch, TimeArray, TimeScale,
    ades_angular_covariance_flat,
};
use adam_core::od::{AssociatedObservations, OdBatchResult, OdObservationBatch};
use adam_core::spice::AdamCoreSpiceBackend;
use serde_json::{Map, Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The shared public Vereš et al. (2017) fill, reached through
/// `runners/sigmafill/fill_json.py`: the same `fill_missing` every Python runner
/// imports, so there is one implementation and one set of labels.
#[derive(Debug, Clone)]
pub struct SigmaFill {
    pub python: PathBuf,
    pub script: PathBuf,
}

impl SigmaFill {
    /// Fill the missing sigmas of these observations (arcsec, RA·cos(Dec) for
    /// RA). A reported sigma comes back unchanged; the report carries the
    /// counts by fill level.
    pub fn fill(
        &self,
        stn: &[String],
        astcat: &[Option<String>],
        rms_ra: &[Option<f64>],
        rms_dec: &[Option<f64>],
    ) -> Result<(Vec<f64>, Vec<f64>, Value), String> {
        let request = json!({"stn": stn, "astcat": astcat, "rms_ra": rms_ra, "rms_dec": rms_dec});
        let mut child = Command::new(&self.python)
            .arg("-I")
            .arg(&self.script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                format!(
                    "cannot run {} for the sigma fill: {e}",
                    self.python.display()
                )
            })?;
        child
            .stdin
            .take()
            .ok_or("sigma fill: no stdin")?
            .write_all(request.to_string().as_bytes())
            .map_err(|e| format!("sigma fill: {e}"))?;
        let out = child
            .wait_with_output()
            .map_err(|e| format!("sigma fill: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "sigma fill exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let v: Value = serde_json::from_slice(&out.stdout)
            .map_err(|e| format!("sigma fill wrote unreadable JSON: {e}"))?;
        let col = |k: &str| -> Result<Vec<f64>, String> {
            v[k].as_array()
                .ok_or_else(|| format!("sigma fill response lacks {k}"))?
                .iter()
                .map(|x| {
                    x.as_f64()
                        .ok_or_else(|| format!("sigma fill {k} is not numeric"))
                })
                .collect()
        };
        let (ra, dec) = (col("ra_sigma")?, col("dec_sigma")?);
        if ra.len() != stn.len() || dec.len() != stn.len() {
            return Err("sigma fill returned the wrong number of rows".into());
        }
        Ok((ra, dec, v["report"].clone()))
    }
}

/// Strip the fixed-width padding from every PSV cell, as the Python channel does
/// before its reader (`adam_core_common.normalize_psv`). adam-core's Rust reader
/// trims most cells but drops the last character of `obsTime` unconditionally
/// (expecting the `Z`), so a padded `...Z   ` would fail to parse.
pub fn normalize_psv(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        if line.starts_with('#') || line.starts_with('!') || line.trim().is_empty() {
            out.push_str(line);
        } else {
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            out.push_str(&cells.join("|"));
        }
        out.push('\n');
    }
    out
}

/// The three fixture-name candidates every runner tries: the object name, the
/// name with '/' replaced by '_', then the MPC designation.
pub fn resolve_fixture(
    dir: &Path,
    name: &str,
    mpc: Option<&str>,
) -> (Option<PathBuf>, Vec<String>) {
    let mut cands = vec![
        dir.join(format!("{name}.psv")),
        dir.join(format!("{}.psv", name.replace('/', "_"))),
    ];
    if let Some(m) = mpc {
        cands.push(dir.join(format!("{m}.psv")));
    }
    let found = cands.iter().find(|p| p.is_file()).cloned();
    (
        found,
        cands.iter().map(|p| p.display().to_string()).collect(),
    )
}

/// What the OD input preparation found.
pub struct Prepared {
    pub input: Option<AssociatedObservations>,
    pub n_parsed: usize,
    pub dropped_stations: Vec<String>,
    pub n_dropped: usize,
    pub sigma_report: Value,
}

fn take<T: Clone>(v: &[T], keep: &[usize]) -> Vec<T> {
    keep.iter().map(|&i| v[i].clone()).collect()
}

/// The backend's flat observer states as one `[x, y, z, vx, vy, vz]` row per
/// observation, refused by name unless they are exactly six values for each
/// of the `n` observations (no row is dropped or padded).
fn observer_rows(flat: &[f64], n: usize) -> Result<Vec<[f64; 6]>, String> {
    let (rows, rest) = flat.as_chunks::<6>();
    if !rest.is_empty() || rows.len() != n {
        return Err(format!(
            "observer states: {} values for {n} observers (expected {})",
            flat.len(),
            6 * n
        ));
    }
    Ok(rows.to_vec())
}

/// The `(N, 36)` spherical covariance adam-core's ADES route builds
/// (`ades_angular_covariance_flat`, documented as what
/// `OrbitDeterminationObservations.from_ades` builds): lon sigma
/// rmsRA / cos(dec), lat sigma rmsDec, a null rmsCorr a zero correlation.
pub fn observed_covariance(
    dec_deg: &[f64],
    ra_sigma: &[f64],
    dec_sigma: &[f64],
    corr: &[Option<f64>],
) -> Result<CovarianceBatch, String> {
    let corr: Vec<f64> = corr.iter().map(|c| c.unwrap_or(f64::NAN)).collect();
    let flat = ades_angular_covariance_flat(dec_deg, ra_sigma, dec_sigma, &corr)
        .map_err(|e| e.to_string())?;
    CovarianceBatch::new(
        dec_deg.len(),
        6,
        flat,
        CovarianceUnits::Coordinate(CoordinateRepresentation::Spherical),
    )
    .map_err(|e| e.to_string())
}

/// ADES rows -> the typed OD input, dropping (and counting) stations adam-core
/// cannot place, and filling missing sigma from the shared model.
pub fn prepare(
    object: &str,
    ades: &AdesObservationBatch,
    resolvable: &dyn Fn(&str) -> bool,
    fill: &SigmaFill,
    backend: &AdamCoreSpiceBackend,
) -> Result<Prepared, String> {
    let n_parsed = ades.len();
    let keep: Vec<usize> = (0..n_parsed)
        .filter(|&i| resolvable(&ades.stn[i]))
        .collect();
    let mut dropped_stations: Vec<String> = (0..n_parsed)
        .filter(|i| !keep.contains(i))
        .map(|i| ades.stn[i].clone())
        .collect();
    let n_dropped = dropped_stations.len();
    dropped_stations.sort();
    dropped_stations.dedup();
    let n = keep.len();
    if n == 0 {
        return Ok(Prepared {
            input: None,
            n_parsed,
            dropped_stations,
            n_dropped,
            sigma_report: json!({"model": "Veres2017SigmaFill", "reported": 0, "filled": 0,
                "partially_reported": 0, "filled_by_level": {"station": 0, "catalog": 0,
                "no_catalog": 0, "catalog_unrecognized": 0}, "total": 0}),
        });
    }
    let stn = take(&ades.stn, &keep);
    let astcat: Vec<Option<String>> = take(&ades.ast_cat, &keep)
        .into_iter()
        .map(|a| (!a.is_empty()).then_some(a))
        .collect();
    let rms_ra = take(&ades.rms_ra_cos_dec, &keep);
    let rms_dec = take(&ades.rms_dec, &keep);
    let (ra_sigma, dec_sigma, sigma_report) = fill.fill(&stn, &astcat, &rms_ra, &rms_dec)?;

    let ra = take(&ades.ra, &keep);
    let dec = take(&ades.dec, &keep);
    let corr = take(&ades.rms_corr, &keep);
    let days = take(&ades.obs_time.days, &keep);
    let nanos = take(&ades.obs_time.nanos, &keep);
    let epochs: Vec<Epoch> = days
        .iter()
        .zip(&nanos)
        .map(|(&d, &ns)| Epoch::new(d, ns))
        .collect();
    let times = TimeArray::new(ades.obs_time.scale, epochs).map_err(|e| e.to_string())?;

    let observed = CoordinateBatch::new(
        CoordinateValues::Spherical(
            (0..n)
                .map(|i| [f64::NAN, ra[i], dec[i], f64::NAN, f64::NAN, f64::NAN])
                .collect(),
        ),
        DataFrame::Equatorial,
        OriginArray::new(stn.iter().map(|s| OriginId::from_code(s.clone())).collect()),
        Some(times.clone()),
        Some(observed_covariance(&dec, &ra_sigma, &dec_sigma, &corr)?),
    )
    .map_err(|e| e.to_string())?;

    let mut unique: Vec<String> = stn.clone();
    unique.sort();
    unique.dedup();
    let idx: Vec<usize> = stn
        .iter()
        .map(|s| unique.binary_search(s).expect("code is in its own set"))
        .collect();
    let sun = OriginId::from_code("SUN");
    let flat = backend
        .observer_states_from_codes(&unique, &idx, &times, DataFrame::Ecliptic, &sun)
        .map_err(|e| format!("observer states: {e}"))?;
    let states = observer_rows(&flat, n)?;
    let observers = ObserverBatch::new(
        stn.iter().map(|s| ObservatoryCode(s.clone())).collect(),
        CoordinateBatch::cartesian(
            states,
            DataFrame::Ecliptic,
            OriginArray::repeat(sun, n),
            Some(times),
            None,
        )
        .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;

    let ids: Vec<String> = (0..n).map(|i| format!("{object}#{i}")).collect();
    let time = TimeColumn {
        scale: ades.obs_time.scale,
        days,
        nanos,
        validity: None,
    };
    let null_time = TimeColumn {
        scale: TimeScale::Utc,
        days: vec![0; n],
        nanos: vec![0; n],
        validity: Some(vec![false; n]),
    };
    let none_f = vec![None; n];
    let source = SourceCatalogBatch {
        id: ids.clone(),
        exposure_id: vec![None; n],
        time,
        ra,
        dec,
        ra_sigma: ra_sigma.iter().map(|&s| Some(s)).collect(),
        dec_sigma: dec_sigma.iter().map(|&s| Some(s)).collect(),
        radec_corr: corr,
        mag: none_f.clone(),
        mag_sigma: none_f.clone(),
        fwhm: none_f.clone(),
        a: none_f.clone(),
        a_sigma: none_f.clone(),
        b: none_f.clone(),
        b_sigma: none_f.clone(),
        pa: none_f.clone(),
        pa_sigma: none_f.clone(),
        observatory_code: stn,
        filter: vec![None; n],
        exposure_start_time: null_time,
        exposure_duration: none_f.clone(),
        exposure_seeing: none_f.clone(),
        exposure_depth_5sigma: none_f,
        object_id: vec![Some(object.to_string()); n],
        catalog_id: astcat
            .iter()
            .map(|a| a.clone().unwrap_or_else(|| "UNK".into()))
            .collect(),
        astcat,
        photcat: vec![None; n],
    };
    let associations = AssociationBatch {
        detection_id: ids,
        object_id: vec![Some(object.to_string()); n],
    };
    Ok(Prepared {
        input: Some(AssociatedObservations {
            observations: OdObservationBatch {
                source,
                observed,
                observers,
            },
            associations,
        }),
        n_parsed,
        dropped_stations,
        n_dropped,
        sigma_report,
    })
}

/// `CORE_OD_V1` bound to the registered EFCC18 table (hash and size verified by
/// adam-core). No other resource is bound; no private table exists here.
pub fn core_od_v1(bias_table: &Path) -> Result<OdConfig, String> {
    CORE_OD_V1
        .bind(&ResourceBindings::new().bind(ResourceSlot::Efcc18CatalogBiasTable, bias_table))
        .map_err(|e| e.to_string())
}

fn finite(x: f64) -> Option<Value> {
    x.is_finite().then(|| json!(x))
}

/// The fields of one fit, named as the Python channel names them (with this
/// runner's prefix added by the caller). A run that produced no orbit carries
/// adam-core's own stage, code and message.
pub fn summarize(
    result: &OdBatchResult,
    backend: &AdamCoreSpiceBackend,
) -> Result<Map<String, Value>, String> {
    let mut out = Map::new();
    let Some(fitted) = result.fitted_orbits().first() else {
        let why = match result.failures().first() {
            Some(f) => format!(
                "no orbit [{}/{}]: {}",
                f.stage.as_str(),
                f.code.as_str(),
                f.message
            ),
            None => "no orbit (run_od returned no fitted orbit and no failure detail)".into(),
        };
        out.insert("converged".into(), json!(false));
        out.insert(
            "unsupported".into(),
            json!(why.chars().take(400).collect::<String>()),
        );
        return Ok(out);
    };
    let refined = fitted.refined.as_ref();
    let eval = fitted
        .evaluation
        .as_ref()
        .or(refined.map(|r| &r.fit.evaluation));
    out.insert(
        "converged".into(),
        json!(refined.is_some_and(|r| r.fit.converged) && result.failures().is_empty()),
    );
    if let Some(r) = refined {
        out.insert("status_code".into(), json!(r.fit.status_code.to_string()));
        out.insert("iterations".into(), json!(r.fit.iterations));
    }
    if let Some(e) = eval {
        if let Some(v) = finite(e.orbit_chi2) {
            out.insert("chi2".into(), v);
        }
        if let Some(v) = finite(e.reduced_chi2) {
            out.insert("reduced_chi2".into(), v);
        }
        out.insert("n_obs_used".into(), json!(e.num_obs));
        if let Some(v) = finite(e.arc_length) {
            out.insert("arc_length_days".into(), v);
        }
    }
    out.insert(
        "n_obs_rejected".into(),
        json!(
            result
                .fitted_orbit_members()
                .iter()
                .filter(|m| m.outlier)
                .count()
        ),
    );
    let bary = normalize_coordinates_to(
        &fitted.orbit.coordinates,
        &OriginId::SolarSystemBarycenter,
        DataFrame::Equatorial,
        backend,
    )
    .map_err(|e| format!("fitted state to SSB equatorial: {e}"))?;
    let state = bary
        .values
        .cartesian()
        .ok_or("fitted state is not Cartesian")?[0];
    if state.iter().all(|v| v.is_finite()) {
        out.insert("state_bcart_eq".into(), json!(state));
    }
    if let Some(t) = bary.times.as_ref() {
        let tdb = t.rescale(TimeScale::Tdb).map_err(|e| e.to_string())?;
        if let Some(v) = finite(tdb.epochs[0].mjd()) {
            out.insert("epoch_mjd_tdb".into(), v);
        }
    }
    if let Some(c) = bary.covariance.as_ref() {
        let m = &c.values_row_major[..36];
        if m.iter().all(|v| v.is_finite()) {
            let rows: Vec<Vec<f64>> = (0..6).map(|i| m[i * 6..i * 6 + 6].to_vec()).collect();
            out.insert("cov".into(), json!(rows));
        }
    }
    // A failure beside a retained best state (refinement that did not converge)
    // is a fit that did not converge, as the Python channel reports it
    // (`converged` false with the state's statistics), not an unsupported row;
    // adam-core's stage, code and message ride along as `failure`.
    if !result.failures().is_empty() {
        let f = &result.failures()[0];
        out.insert(
            "failure".into(),
            json!(
                format!(
                    "no converged orbit [{}/{}]: {}",
                    f.stage.as_str(),
                    f.code.as_str(),
                    f.message
                )
                .chars()
                .take(400)
                .collect::<String>()
            ),
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observer_rows_are_six_values_per_observation_or_refused() {
        let flat: Vec<f64> = (0..12).map(f64::from).collect();
        let rows = observer_rows(&flat, 2).unwrap();
        assert_eq!(
            rows,
            vec![
                [0.0, 1.0, 2.0, 3.0, 4.0, 5.0],
                [6.0, 7.0, 8.0, 9.0, 10.0, 11.0]
            ]
        );
        // Two whole rows and a partial one: the partial row is refused, never dropped.
        let long: Vec<f64> = (0..13).map(f64::from).collect();
        let e = observer_rows(&long, 2).unwrap_err();
        assert_eq!(
            e,
            "observer states: 13 values for 2 observers (expected 12)"
        );
        // Whole rows, but not one per observation.
        let e = observer_rows(&flat, 3).unwrap_err();
        assert_eq!(
            e,
            "observer states: 12 values for 3 observers (expected 18)"
        );
    }

    #[test]
    fn the_observed_covariance_is_adam_cores_ades_route() {
        // dec 60 deg: lon sigma = rmsRA / cos(dec) = 2x; null corr -> 0.
        let c = observed_covariance(&[60.0], &[0.36], &[0.72], &[None]).unwrap();
        let v = &c.values_row_major;
        let lon = 0.36 / 3600.0 / 60f64.to_radians().cos();
        let lat = 0.72 / 3600.0;
        assert!((v[7] - lon * lon).abs() < 1e-24, "{}", v[7]);
        assert!((v[14] - lat * lat).abs() < 1e-24);
        assert_eq!((v[8], v[13]), (0.0, 0.0));
        let c = observed_covariance(&[0.0], &[1.0], &[1.0], &[Some(0.5)]).unwrap();
        let s = 1.0 / 3600.0;
        assert!((c.values_row_major[8] - 0.5 * s * s).abs() < 1e-24);
    }

    #[test]
    fn the_psv_normalization_trims_every_cell_and_keeps_headers() {
        let raw = "# version=2017\n! observatory\npermID |obsTime                  |stn \n  433  |2020-01-21T18:03:00Z     | W84\n";
        let n = normalize_psv(raw);
        assert_eq!(
            n,
            "# version=2017\n! observatory\npermID|obsTime|stn\n433|2020-01-21T18:03:00Z|W84\n"
        );
        let (ades, _) = adam_core::coords::ades_string_to_observations(
            "permID|obsTime|ra|dec|stn|mode|astCat\n433|2020-01-21T18:03:00Z|10.0|5.0|W84|CCD|Gaia2\n",
        )
        .unwrap();
        assert_eq!(ades.stn, vec!["W84"]);
        // The padded original is what the reader refuses.
        assert!(adam_core::coords::ades_string_to_observations(
            "permID|obsTime|ra|dec|stn|mode|astCat\n433|2020-01-21T18:03:00Z   |10.0|5.0|W84|CCD|Gaia2\n",
        )
        .is_err());
    }

    #[test]
    fn the_fixture_lookup_tries_the_three_stems_and_names_them() {
        let dir = std::env::temp_dir().join(format!("acr-psv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("103P_Hartley 2.psv"), b"").unwrap();
        let (p, _) = resolve_fixture(&dir, "103P/Hartley 2", None);
        assert!(p.unwrap().ends_with("103P_Hartley 2.psv"));
        let (p, tried) = resolve_fixture(&dir, "Nope", Some("2099 XX"));
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(p.is_none());
        assert_eq!(tried.len(), 3);
        assert!(tried[2].ends_with("2099 XX.psv"));
    }

    fn sigmafill() -> SigmaFill {
        let here = Path::new(env!("CARGO_MANIFEST_DIR"));
        SigmaFill {
            python: PathBuf::from(std::env::var("PYTHON").unwrap_or_else(|_| "python3".into())),
            script: here.join("../sigmafill/fill_json.py"),
        }
    }

    #[test]
    fn the_sigma_fill_is_the_shared_helper_and_keeps_a_reported_sigma() {
        let (ra, dec, report) = sigmafill()
            .fill(
                &["F51".into(), "XXX".into()],
                &[Some("Gaia2".into()), None],
                &[Some(0.12), None],
                &[Some(0.11), Some(0.3)],
            )
            .unwrap();
        assert_eq!((ra[0], dec[0]), (0.12, 0.11), "a reported sigma is kept");
        assert_eq!(dec[1], 0.3, "a reported half is kept");
        assert!(ra[1] > 0.0, "the missing half is filled");
        assert_eq!(report["model"], "Veres2017SigmaFill");
        assert_eq!(
            (report["reported"].as_u64(), report["filled"].as_u64()),
            (Some(1), Some(1))
        );
        assert_eq!(report["filled_by_level"]["no_catalog"].as_u64(), Some(1));
    }

    #[test]
    fn a_failing_sigma_fill_is_an_error_naming_it() {
        let mut f = sigmafill();
        f.script = PathBuf::from("/nonexistent/fill_json.py");
        let err = f
            .fill(&["F51".into()], &[None], &[None], &[None])
            .unwrap_err();
        assert!(err.starts_with("sigma fill exited"), "{err}");
    }
}
