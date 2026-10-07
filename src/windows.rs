//! Window-manifest generator for the covariance-realism family.
//!
//! Reads the pinned fixture snapshot and materializes the single source of
//! truth every runner consumes: eligibility, the bundle=1 schedule with
//! explicit cut instants, profile memberships, prediction targets, the
//! pinned scoring σ, and debias corrections. Developer-only — never on an
//! automated path.
//!
//! Every rule here is tool-agnostic and observational: night counts,
//! observation counts, arc spans. No tool's convergence ever shapes the
//! sample — that would be selection on the test statistic.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crate::catalog;
use crate::debias::DebiasTable;
use crate::predict_schema::{
    MANIFEST_SCHEMA_VERSION, ObjectWindows, ObsEntry, TargetSpec, WindowManifest, WindowSpec,
    classes, obs_key_hash, profiles,
};
use crate::weights::pinned_scoring_sigma;

/// Horizon-ladder rungs, days past the cut.
pub const LADDER_DAYS: [f64; 7] = [3.0, 7.0, 14.0, 30.0, 90.0, 180.0, 365.0];
/// Fractional tolerance for accepting a ladder night.
pub const LADDER_TOLERANCE: f64 = 0.5;
/// Targets below this realized horizon are flagged by the kernel.
pub const MIN_HORIZON_DAYS: f64 = 0.3;
/// Per-target-night observation cap.
pub const MAX_OBS_PER_NIGHT: u32 = 12;
/// Seed window: minimum distinct nights and observations.
pub const SEED_MIN_NIGHTS: usize = 3;
pub const SEED_MIN_OBS: usize = 8;
/// Apparition boundary: first internal night gap larger than this ends the
/// discovery apparition.
pub const APPARITION_GAP_DAYS: i64 = 90;
/// No observation may lie within this many seconds of a cut instant (the
/// TDB−UTC offset is ~69 s; a boundary observation would land on different
/// sides of the cut for TDB- vs UTC-slicing runners).
pub const CUT_CLEARANCE_SECONDS: f64 = 120.0;
/// Intra-night (impactor) schedules: a gap larger than this splits
/// tracklets, and each qualifying boundary becomes a cut.
pub const INTRA_NIGHT_GAP_HOURS: f64 = 1.0;
/// `ci` profile: this many bundle-1 cuts, then geometric spacing.
pub const CI_DENSE_CUTS: usize = 60;
/// `ci` profile: geometric step growth after the dense prefix.
pub const CI_GEOMETRIC_STEP: f64 = 1.3;

/// Generator options, mirrored from the `windows` CLI subcommand.
#[derive(Debug, Clone)]
pub struct WindowsOptions {
    /// Nights per walk step in the base schedule (default 1).
    pub bundle_nights: u32,
    /// EFCC2020 table directory. `None` is only legal together with
    /// `no_debias = true` — absence is a loud choice, never a fallback.
    pub debias_dir: Option<std::path::PathBuf>,
    /// Explicitly generate without debias corrections (every `ObsEntry`
    /// carries `None`; the kernel buckets accordingly).
    pub no_debias: bool,
}

/// Generate the manifest from `fixtures_dir` (+ sibling `manifest.json` for
/// the snapshot id) and write it to `output`.
pub fn run(fixtures_dir: &Path, output: &Path, opts: &WindowsOptions) -> Result<(), String> {
    if opts.bundle_nights == 0 {
        return Err("bundle_nights must be >= 1".into());
    }
    let debias = match (&opts.debias_dir, opts.no_debias) {
        (_, true) => None,
        (Some(dir), false) => Some(DebiasTable::load(dir)?),
        (None, false) => {
            return Err(
                "no --debias-dir given: pass one, or opt out explicitly with --no-debias \
                 (scoring against undebiased positions is a sensitivity arm, not a default)"
                    .into(),
            );
        }
    };

    let snapshot_id = read_snapshot_id(fixtures_dir)?;
    let mut objects = Vec::new();
    for obj in catalog::all_objects() {
        let psv = resolve_fixture(fixtures_dir, obj.name, obj.mpc_designation)?;
        let text =
            std::fs::read_to_string(&psv).map_err(|e| format!("read {}: {e}", psv.display()))?;
        let raw = parse_psv(&text).map_err(|e| format!("{}: {e}", psv.display()))?;
        let ow = build_object(
            obj.name,
            obj.mpc_designation,
            obj.population,
            raw,
            opts,
            &debias,
        )
        .map_err(|e| format!("{}: {e}", obj.name))?;
        eprintln!(
            "windows: {:<16} {} — {} obs, {} windows{}",
            ow.object,
            if ow.eligible {
                "eligible "
            } else {
                "INELIGIBLE"
            },
            ow.n_obs_total,
            ow.windows.len(),
            ow.ineligible_reason
                .as_deref()
                .map(|r| format!(" ({r})"))
                .unwrap_or_default(),
        );
        objects.push(ow);
    }

    let manifest = WindowManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        snapshot_id,
        bundle_nights: opts.bundle_nights,
        generated_by: format!("{}/{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")),
        ladder_days: LADDER_DAYS.to_vec(),
        ladder_tolerance: LADDER_TOLERANCE,
        min_horizon_days: MIN_HORIZON_DAYS,
        max_obs_per_night: MAX_OBS_PER_NIGHT,
        objects,
    };
    let (n_eligible, n_windows, n_targets) = manifest.objects.iter().fold((0, 0, 0), |acc, o| {
        (
            acc.0 + usize::from(o.eligible),
            acc.1 + o.windows.len(),
            acc.2 + o.windows.iter().map(|w| w.targets.len()).sum::<usize>(),
        )
    });
    if let Some(dir) = output.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    }
    std::fs::write(
        output,
        serde_json::to_string(&manifest).map_err(|e| format!("serialize manifest: {e}"))?,
    )
    .map_err(|e| format!("write {}: {e}", output.display()))?;
    eprintln!(
        "windows: wrote {} — {} objects ({} eligible), {} windows, {} targets",
        output.display(),
        manifest.objects.len(),
        n_eligible,
        n_windows,
        n_targets,
    );
    Ok(())
}

fn read_snapshot_id(fixtures_dir: &Path) -> Result<String, String> {
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
fn resolve_fixture(
    fixtures_dir: &Path,
    name: &str,
    mpc_designation: &str,
) -> Result<std::path::PathBuf, String> {
    let candidates = [
        fixtures_dir.join(format!("{name}.psv")),
        fixtures_dir.join(format!("{}.psv", name.replace('/', "_"))),
        fixtures_dir.join(format!("{mpc_designation}.psv")),
    ];
    candidates
        .iter()
        .find(|p| p.exists())
        .cloned()
        .ok_or_else(|| {
            format!(
                "{name}: no fixture found; tried {}",
                candidates
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

// ---------------------------------------------------------------------------
// Time: ISO-8601 UTC → MJD, and UTC → TT via the IERS leap-second table.
// ---------------------------------------------------------------------------

/// `(first MJD UTC of validity, TAI − UTC seconds)` — the IERS leap-second
/// table from the start of the modern (integer-offset) era. Provenance:
/// IERS Bulletin C; no leap second has been announced since 2017-01-01.
const LEAP_SECONDS: [(i64, f64); 28] = [
    (41317, 10.0), // 1972-01-01
    (41499, 11.0), // 1972-07-01
    (41683, 12.0), // 1973-01-01
    (42048, 13.0), // 1974-01-01
    (42413, 14.0), // 1975-01-01
    (42778, 15.0), // 1976-01-01
    (43144, 16.0), // 1977-01-01
    (43509, 17.0), // 1978-01-01
    (43874, 18.0), // 1979-01-01
    (44239, 19.0), // 1980-01-01
    (44786, 20.0), // 1981-07-01
    (45151, 21.0), // 1982-07-01
    (45516, 22.0), // 1983-07-01
    (46247, 23.0), // 1985-07-01
    (47161, 24.0), // 1988-01-01
    (47892, 25.0), // 1990-01-01
    (48257, 26.0), // 1991-01-01
    (48804, 27.0), // 1992-07-01
    (49169, 28.0), // 1993-07-01
    (49534, 29.0), // 1994-07-01
    (50083, 30.0), // 1996-01-01
    (50630, 31.0), // 1997-07-01
    (51179, 32.0), // 1999-01-01
    (53736, 33.0), // 2006-01-01
    (54832, 34.0), // 2009-01-01
    (56109, 35.0), // 2012-07-01
    (57204, 36.0), // 2015-07-01
    (57754, 37.0), // 2017-01-01
];

/// Days from 1970-01-01 for a proleptic-Gregorian civil date
/// (Howard Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of [`days_from_civil`].
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Parse ADES `obsTime` (ISO-8601 UTC, trailing `Z` optional, fractional
/// seconds optional) to MJD UTC.
pub fn iso_to_mjd_utc(s: &str) -> Result<f64, String> {
    let s = s.trim().trim_end_matches('Z');
    let (date, time) = match s.split_once('T') {
        Some((d, t)) => (d, t),
        None => (s, "00:00:00"),
    };
    let mut dp = date.split('-');
    let (y, m, d) = (
        dp.next().and_then(|v| v.parse::<i64>().ok()),
        dp.next().and_then(|v| v.parse::<u32>().ok()),
        dp.next().and_then(|v| v.parse::<u32>().ok()),
    );
    let (Some(y), Some(m), Some(d)) = (y, m, d) else {
        return Err(format!("malformed date in {s:?}"));
    };
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return Err(format!("out-of-range date in {s:?}"));
    }
    let mut tp = time.split(':');
    let (hh, mi, ss) = (
        tp.next().and_then(|v| v.parse::<u32>().ok()),
        tp.next().and_then(|v| v.parse::<u32>().ok()),
        tp.next().map_or(Some(0.0), |v| v.parse::<f64>().ok()),
    );
    let (Some(hh), Some(mi), Some(ss)) = (hh, mi, ss) else {
        return Err(format!("malformed time in {s:?}"));
    };
    if hh > 23 || mi > 59 || !(0.0..61.0).contains(&ss) {
        return Err(format!("out-of-range time in {s:?}"));
    }
    let mjd_day = days_from_civil(y, m, d) + 40_587;
    Ok(mjd_day as f64 + (f64::from(hh) * 3600.0 + f64::from(mi) * 60.0 + ss) / 86_400.0)
}

/// TT − UTC in seconds at a UTC MJD (TAI − UTC from the leap table, plus
/// the fixed TT − TAI = 32.184 s). Errors before the 1972 integer era —
/// the fixtures' earliest rows are 1972.
pub fn tt_minus_utc_seconds(mjd_utc: f64) -> Result<f64, String> {
    let day = mjd_utc.floor() as i64;
    if day < LEAP_SECONDS[0].0 {
        return Err(format!(
            "MJD {day} predates the 1972 leap-second era; the manifest generator does not \
             model rubber-band UTC"
        ));
    }
    let tai_utc = LEAP_SECONDS
        .iter()
        .rev()
        .find(|(from, _)| day >= *from)
        .map(|(_, s)| *s)
        .unwrap();
    Ok(tai_utc + 32.184)
}

/// Format an MJD UTC instant as ISO-8601 with millisecond precision.
fn mjd_utc_to_iso(mjd: f64) -> String {
    let day = mjd.floor() as i64;
    let mut frac = mjd - day as f64;
    // Round to the millisecond first so 23:59:59.9996 does not print as
    // 60 s.
    frac = (frac * 86_400_000.0).round() / 86_400_000.0;
    let (day, frac) = if frac >= 1.0 {
        (day + 1, 0.0)
    } else {
        (day, frac)
    };
    let (y, m, d) = civil_from_days(day - 40_587);
    let total_ms = (frac * 86_400_000.0).round() as i64;
    let (hh, rem) = (total_ms / 3_600_000, total_ms % 3_600_000);
    let (mi, rem) = (rem / 60_000, rem % 60_000);
    let (ss, ms) = (rem / 1000, rem % 1000);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mi:02}:{ss:02}.{ms:03}Z")
}

// ---------------------------------------------------------------------------
// PSV parsing
// ---------------------------------------------------------------------------

struct RawObs {
    stn: String,
    obs_time: String,
    ra_raw: String,
    dec_raw: String,
    ra_deg: f64,
    dec_deg: f64,
    rms_ra: Option<f64>,
    rms_dec: Option<f64>,
    rms_corr: Option<f64>,
    ast_cat: String,
    mode: String,
    space_based: bool,
    mjd_utc: f64,
}

fn parse_psv(text: &str) -> Result<Vec<RawObs>, String> {
    let mut header: Option<Vec<String>> = None;
    let mut out = Vec::new();
    for (lineno, line) in text.lines().enumerate() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split('|').map(str::trim).collect();
        match &header {
            None => {
                if fields.contains(&"obsTime") {
                    header = Some(fields.iter().map(|s| s.to_string()).collect());
                }
                // Pre-header lines (ADES version headers) are skipped; a
                // file with no header at all errors below.
            }
            Some(hdr) => {
                let col = |name: &str| hdr.iter().position(|h| h == name);
                let get = |name: &str| col(name).and_then(|i| fields.get(i)).copied();
                let Some(stn) = get("stn") else {
                    return Err(format!("line {}: no stn field", lineno + 1));
                };
                let Some(obs_time) = get("obsTime") else {
                    return Err(format!("line {}: no obsTime field", lineno + 1));
                };
                let (Some(ra_raw), Some(dec_raw)) = (get("ra"), get("dec")) else {
                    return Err(format!("line {}: no ra/dec fields", lineno + 1));
                };
                let ra_deg: f64 = ra_raw
                    .parse()
                    .map_err(|e| format!("line {}: ra {ra_raw:?}: {e}", lineno + 1))?;
                let dec_deg: f64 = dec_raw
                    .parse()
                    .map_err(|e| format!("line {}: dec {dec_raw:?}: {e}", lineno + 1))?;
                let opt = |name: &str| {
                    get(name)
                        .filter(|v| !v.is_empty())
                        .and_then(|v| v.parse().ok())
                };
                let space_based = get("sys").is_some_and(|v| !v.is_empty())
                    || get("pos1").is_some_and(|v| !v.is_empty());
                let mjd_utc =
                    iso_to_mjd_utc(obs_time).map_err(|e| format!("line {}: {e}", lineno + 1))?;
                out.push(RawObs {
                    stn: stn.to_string(),
                    obs_time: obs_time.to_string(),
                    ra_raw: ra_raw.to_string(),
                    dec_raw: dec_raw.to_string(),
                    ra_deg,
                    dec_deg,
                    rms_ra: opt("rmsRA"),
                    rms_dec: opt("rmsDec"),
                    rms_corr: opt("rmsCorr"),
                    ast_cat: get("astCat")
                        .filter(|v| !v.is_empty())
                        .unwrap_or("UNK")
                        .to_string(),
                    mode: get("mode")
                        .filter(|v| !v.is_empty())
                        .unwrap_or("UNK")
                        .to_string(),
                    space_based,
                    mjd_utc,
                });
            }
        }
    }
    if header.is_none() {
        return Err("no ADES PSV header line (none contains obsTime)".into());
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Per-object assembly
// ---------------------------------------------------------------------------

fn build_object(
    name: &str,
    mpc_designation: &str,
    population: &str,
    mut raw: Vec<RawObs>,
    opts: &WindowsOptions,
    debias: &Option<DebiasTable>,
) -> Result<ObjectWindows, String> {
    raw.sort_by(|a, b| a.mjd_utc.partial_cmp(&b.mjd_utc).unwrap());
    let n_obs_total = raw.len() as u32;

    // Exact-duplicate disambiguation: hash the k-th occurrence with a
    // `#k` suffix on obs_time so content keys stay unique. Duplicate MPC
    // rows are real; a colliding key would silently merge two targets.
    let mut occurrence: HashMap<(String, String, String, String), u32> = HashMap::new();
    let mut observations: Vec<ObsEntry> = Vec::with_capacity(raw.len());
    for r in &raw {
        let occ_key = (
            r.stn.clone(),
            r.obs_time.clone(),
            r.ra_raw.clone(),
            r.dec_raw.clone(),
        );
        let occ = occurrence.entry(occ_key).or_insert(0);
        *occ += 1;
        let hashed_time = if *occ == 1 {
            r.obs_time.clone()
        } else {
            format!("{}#{}", r.obs_time, occ)
        };
        let sigma = pinned_scoring_sigma(&r.stn, r.rms_ra, r.rms_dec, r.rms_corr);
        let (debias_dra, debias_ddec) = match debias {
            Some(table) => match table.correction(r.ra_deg, r.dec_deg, r.mjd_utc, &r.ast_cat) {
                Some(c) => (Some(c.dra_arcsec), Some(c.ddec_arcsec)),
                None => (None, None),
            },
            None => (None, None),
        };
        observations.push(ObsEntry {
            stn: r.stn.clone(),
            obs_time: r.obs_time.clone(),
            key_hash: obs_key_hash(&r.stn, &hashed_time, &r.ra_raw, &r.dec_raw),
            mjd_utc: r.mjd_utc,
            night: r.mjd_utc.floor() as i64,
            ra_deg: r.ra_deg,
            dec_deg: r.dec_deg,
            ast_cat: r.ast_cat.clone(),
            mode: r.mode.clone(),
            sigma_ra_arcsec: sigma.ra_arcsec,
            sigma_dec_arcsec: sigma.dec_arcsec,
            sigma_corr: sigma.corr,
            sigma_source: sigma.source.to_string(),
            ades_rms_ra: r.rms_ra,
            ades_rms_dec: r.rms_dec,
            debias_dra_arcsec: debias_dra,
            debias_ddec_arcsec: debias_ddec,
            excluded: r.space_based.then(|| "space_based".to_string()),
        });
    }
    {
        let mut seen = HashMap::new();
        for (i, o) in observations.iter().enumerate() {
            if let Some(prev) = seen.insert(o.key_hash, i) {
                return Err(format!(
                    "key_hash collision between rows {prev} and {i} ({} {})",
                    o.stn, o.obs_time
                ));
            }
        }
    }

    let included: Vec<usize> = observations
        .iter()
        .enumerate()
        .filter(|(_, o)| o.excluded.is_none())
        .map(|(i, _)| i)
        .collect();
    let n_excluded_space_based = (observations.len() - included.len()) as u32;
    let class = classes::from_population(population).to_string();

    let mut ow = ObjectWindows {
        object: name.to_string(),
        mpc_designation: mpc_designation.to_string(),
        population: population.to_string(),
        class,
        eligible: false,
        ineligible_reason: None,
        first_night: included.first().map_or(0, |&i| observations[i].night),
        apparition_end_night: 0,
        n_obs_total,
        n_excluded_space_based,
        observations,
        windows: Vec::new(),
    };
    if included.is_empty() {
        ow.ineligible_reason = Some("no_usable_observations".into());
        return Ok(ow);
    }

    // Distinct non-excluded nights, ascending, with their obs indices.
    let mut by_night: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
    for &i in &included {
        by_night
            .entry(ow.observations[i].night)
            .or_default()
            .push(i);
    }
    let nights: Vec<i64> = by_night.keys().copied().collect();
    ow.apparition_end_night = {
        let mut end = nights[0];
        for w in nights.windows(2) {
            if w[1] - w[0] > APPARITION_GAP_DAYS {
                break;
            }
            end = w[1];
        }
        end
    };

    if nights.len() < SEED_MIN_NIGHTS {
        return build_intra_night(ow, &included, opts);
    }

    // Seed: the smallest leading prefix of nights with >= SEED_MIN_NIGHTS
    // nights and >= SEED_MIN_OBS observations.
    let mut cum = 0usize;
    let mut seed_idx = None;
    for (idx, night) in nights.iter().enumerate() {
        cum += by_night[night].len();
        if idx + 1 >= SEED_MIN_NIGHTS && cum >= SEED_MIN_OBS {
            seed_idx = Some(idx);
            break;
        }
    }
    let Some(seed_idx) = seed_idx else {
        ow.ineligible_reason = Some("insufficient_nights_or_obs".into());
        return Ok(ow);
    };
    if seed_idx + 1 >= nights.len() {
        ow.ineligible_reason = Some("no_held_out_nights".into());
        return Ok(ow);
    }

    // Night cuts: every bundle-th night from the seed through the
    // second-to-last night; the last cut is always included.
    let bundle = opts.bundle_nights as usize;
    let last_cut_idx = nights.len() - 2;
    let mut cut_idxs: Vec<usize> = (seed_idx..=last_cut_idx).step_by(bundle).collect();
    if *cut_idxs.last().unwrap() != last_cut_idx {
        cut_idxs.push(last_cut_idx);
    }

    let all_times: Vec<f64> = ow.observations.iter().map(|o| o.mjd_utc).collect();
    let mut windows = Vec::with_capacity(cut_idxs.len());
    for (w_index, &night_idx) in cut_idxs.iter().enumerate() {
        let cut_night = nights[night_idx];
        let cut_mjd_utc = nudged_cut(&all_times, (cut_night + 1) as f64)
            .map_err(|e| format!("window {w_index} (night {cut_night}): {e}"))?;
        let spec = build_window_spec(
            &ow.observations,
            &included,
            &by_night,
            &nights,
            w_index as u32,
            cut_night,
            cut_mjd_utc,
            false,
        )?;
        windows.push(spec);
    }
    assign_profiles(&mut windows, &nights, seed_idx, &cut_idxs);
    ow.windows = windows;
    ow.eligible = true;
    Ok(ow)
}

/// Intra-night (impactor / short-arc) schedule: fewer than
/// [`SEED_MIN_NIGHTS`] distinct nights — cuts at tracklet boundaries
/// (inter-observation gaps > [`INTRA_NIGHT_GAP_HOURS`]) with at least
/// [`SEED_MIN_OBS`] observations before them.
fn build_intra_night(
    mut ow: ObjectWindows,
    included: &[usize],
    _opts: &WindowsOptions,
) -> Result<ObjectWindows, String> {
    let gap = INTRA_NIGHT_GAP_HOURS / 24.0;
    let mut cuts: Vec<f64> = Vec::new();
    for pair in included.windows(2) {
        let (a, b) = (&ow.observations[pair[0]], &ow.observations[pair[1]]);
        if b.mjd_utc - a.mjd_utc > gap && pair[0] + 1 >= SEED_MIN_OBS {
            // Midpoint of the gap: maximally clear of both neighbours.
            cuts.push((a.mjd_utc + b.mjd_utc) / 2.0);
        }
    }
    if cuts.is_empty() {
        ow.ineligible_reason = Some("no_intra_night_cut".into());
        return Ok(ow);
    }
    let by_night: BTreeMap<i64, Vec<usize>> = {
        let mut m: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
        for &i in included {
            m.entry(ow.observations[i].night).or_default().push(i);
        }
        m
    };
    let nights: Vec<i64> = by_night.keys().copied().collect();
    let mut windows = Vec::new();
    for (w_index, &cut) in cuts.iter().enumerate() {
        let spec = build_window_spec(
            &ow.observations,
            included,
            &by_night,
            &nights,
            w_index as u32,
            cut.floor() as i64,
            cut,
            true,
        )?;
        windows.push(spec);
    }
    // Few cuts: every profile carries all of them.
    for w in &mut windows {
        w.profiles = vec![
            profiles::FULL.to_string(),
            profiles::CI.to_string(),
            profiles::LADDER.to_string(),
        ];
    }
    ow.windows = windows;
    ow.eligible = true;
    Ok(ow)
}

/// Nudge a nominal cut instant so no observation lies within
/// [`CUT_CLEARANCE_SECONDS`] of it.
///
/// A UTC midnight regularly falls inside a continuous tracking run (one
/// physical night spanning two UTC dates — the fixtures carry gaps as
/// tight as ~80 s across midnight), so the rule is: place the cut at the
/// midpoint of the **nearest gap wide enough for the clearance on both
/// sides**, searching outward from the nominal instant, ties toward later
/// (the physical-night reading: the midnight-straddling continuation stays
/// in the fit). Only a nominal cut with no adequate gap within ±0.5 d is
/// an error — that would mean observations every < 4 minutes for a full
/// day on both sides.
fn nudged_cut(sorted_times: &[f64], nominal: f64) -> Result<f64, String> {
    let clearance = CUT_CLEARANCE_SECONDS / 86_400.0;
    let next_i = sorted_times.partition_point(|t| *t < nominal);
    let prev = next_i.checked_sub(1).map(|i| sorted_times[i]);
    let next = sorted_times.get(next_i).copied();
    let clear = |cut: f64| {
        prev.is_none_or(|p| cut - p >= clearance) && next.is_none_or(|n| n - cut >= clearance)
    };
    if clear(nominal) {
        return Ok(nominal);
    }
    // Candidate cuts: midpoints of every inter-observation gap wide enough
    // for the clearance on both sides, within ±0.5 d of the nominal.
    let lo = sorted_times.partition_point(|t| *t < nominal - 0.5);
    let hi = sorted_times.partition_point(|t| *t < nominal + 0.5);
    let mut best: Option<f64> = None;
    for i in lo.saturating_sub(1)..hi.min(sorted_times.len().saturating_sub(1)) {
        let (a, b) = (sorted_times[i], sorted_times[i + 1]);
        if b - a >= 2.0 * clearance {
            let mid = (a + b) / 2.0;
            let better = match best {
                None => true,
                // Ties toward the later gap: continuous tracking across
                // midnight belongs to the night being fit.
                Some(cur) => {
                    (mid - nominal).abs() < (cur - nominal).abs() - 1e-12
                        || ((mid - nominal).abs() - (cur - nominal).abs()).abs() <= 1e-12
                            && mid > cur
                }
            };
            if better {
                best = Some(mid);
            }
        }
    }
    best.ok_or_else(|| {
        format!(
            "no gap wide enough for a {CUT_CLEARANCE_SECONDS}-second cut clearance within \
             ±0.5 d of MJD {nominal:.6}"
        )
    })
}

/// Cap a night's observation indices at [`MAX_OBS_PER_NIGHT`], evenly
/// spaced in time (the night list is time-ordered).
fn cap_evenly(idxs: &[usize]) -> Vec<usize> {
    let k = MAX_OBS_PER_NIGHT as usize;
    if idxs.len() <= k {
        return idxs.to_vec();
    }
    (0..k)
        .map(|i| idxs[i * (idxs.len() - 1) / (k - 1)])
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn build_window_spec(
    observations: &[ObsEntry],
    included: &[usize],
    by_night: &BTreeMap<i64, Vec<usize>>,
    nights: &[i64],
    index: u32,
    cut_night: i64,
    cut_mjd_utc: f64,
    intra_night: bool,
) -> Result<WindowSpec, String> {
    let fit: Vec<usize> = included
        .iter()
        .copied()
        .filter(|&i| observations[i].mjd_utc < cut_mjd_utc)
        .collect();
    let (Some(&first_fit), Some(&last_fit)) = (fit.first(), fit.last()) else {
        return Err("empty fit window".into());
    };
    let last_fit_mjd = observations[last_fit].mjd_utc;
    let tt_offset = tt_minus_utc_seconds(cut_mjd_utc)? / 86_400.0;

    let mut targets: Vec<TargetSpec> = Vec::new();
    let mut used_nights: Vec<i64> = Vec::new();
    let push_night = |night: i64, rung: &str, targets: &mut Vec<TargetSpec>| {
        for &i in cap_evenly(&by_night[&night]).iter() {
            if observations[i].mjd_utc >= cut_mjd_utc {
                targets.push(TargetSpec {
                    obs_idx: i as u32,
                    rung: rung.to_string(),
                    horizon_days: observations[i].mjd_utc - last_fit_mjd,
                });
            }
        }
    };

    if intra_night {
        // Everything after the cut, capped per night, all rung "next".
        for (&night, idxs) in by_night.iter() {
            if idxs.iter().any(|&i| observations[i].mjd_utc >= cut_mjd_utc) {
                push_night(night, "next", &mut targets);
                used_nights.push(night);
            }
        }
    } else {
        let held_out_nights: Vec<i64> = nights.iter().copied().filter(|&n| n > cut_night).collect();
        if let Some(&next) = held_out_nights.first() {
            push_night(next, "next", &mut targets);
            used_nights.push(next);
        }
        for rung_days in LADDER_DAYS {
            let nominal = cut_night as f64 + rung_days;
            let Some(&best) = held_out_nights.iter().min_by(|a, b| {
                let da = (**a as f64 - nominal).abs();
                let db = (**b as f64 - nominal).abs();
                da.partial_cmp(&db).unwrap()
            }) else {
                continue;
            };
            if used_nights.contains(&best) {
                continue;
            }
            if (best as f64 - nominal).abs() > LADDER_TOLERANCE * rung_days {
                continue;
            }
            push_night(best, &format!("d{}", rung_days as i64), &mut targets);
            used_nights.push(best);
        }
    }

    // In-sample controls: first, middle, last fit observations.
    let mut in_sample = vec![first_fit as u32, fit[fit.len() / 2] as u32, last_fit as u32];
    in_sample.sort_unstable();
    in_sample.dedup();

    Ok(WindowSpec {
        index,
        cut_night,
        cut_utc: mjd_utc_to_iso(cut_mjd_utc),
        cut_mjd_tt: cut_mjd_utc + tt_offset,
        // TDB − TT <= 1.7 ms — far inside the 120-s cut clearance, so the
        // manifest carries TDB = TT by construction (documented
        // approximation; the clearance rule is what makes it safe).
        cut_mjd_tdb: cut_mjd_utc + tt_offset,
        intra_night,
        profiles: vec![profiles::FULL.to_string()],
        n_obs_fit: fit.len() as u32,
        last_fit_obs_idx: last_fit as u32,
        arc_days: last_fit_mjd - observations[first_fit].mjd_utc,
        targets,
        in_sample,
        seed: None,
    })
}

/// Assign `ci` and `ladder` profile membership over the night-cut windows
/// (`full` is already on every window).
fn assign_profiles(
    windows: &mut [WindowSpec],
    nights: &[i64],
    seed_idx: usize,
    cut_idxs: &[usize],
) {
    let n = windows.len();
    if n == 0 {
        return;
    }
    // ci: the first CI_DENSE_CUTS cuts at bundle spacing, then geometric.
    let mut ci: Vec<usize> = Vec::new();
    let mut pos = 0usize;
    let mut step = 1.0f64;
    let mut dense = 0usize;
    while pos < n {
        ci.push(pos);
        dense += 1;
        if dense >= CI_DENSE_CUTS {
            step *= CI_GEOMETRIC_STEP;
        }
        pos += (step.round() as usize).max(1);
    }
    if *ci.last().unwrap() != n - 1 {
        ci.push(n - 1);
    }
    for &i in &ci {
        windows[i].profiles.push(profiles::CI.to_string());
    }
    // ladder: seed + nearest cuts to seed+30 d and seed+180 d + last.
    let seed_night = nights[seed_idx];
    let mut ladder: Vec<usize> = vec![0, n - 1];
    for offset in [30.0, 180.0] {
        let target = seed_night as f64 + offset;
        if let Some((i, _)) = cut_idxs.iter().enumerate().min_by(|(_, a), (_, b)| {
            let da = (nights[**a] as f64 - target).abs();
            let db = (nights[**b] as f64 - target).abs();
            da.partial_cmp(&db).unwrap()
        }) {
            ladder.push(i);
        }
    }
    ladder.sort_unstable();
    ladder.dedup();
    for i in ladder {
        windows[i].profiles.push(profiles::LADDER.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HDR: &str = "permID|provID|trkSub|mode|stn|obsTime|ra|dec|rmsRA|rmsDec|astCat|mag|band";

    fn row(stn: &str, time: &str, ra: f64, dec: f64) -> String {
        format!("X1|||CCD|{stn}|{time}|{ra}|{dec}|0.2|0.2|Gaia2|20.1|G")
    }

    /// A synthetic PSV: `spec` = (night offset from 60600, obs per night).
    fn psv(spec: &[(i64, usize)]) -> String {
        let mut s = String::from(HDR);
        s.push('\n');
        for &(night, count) in spec {
            for k in 0..count {
                let mjd = 60_600 + night;
                let (y, m, d) = civil_from_days(mjd - 40_587);
                s.push_str(&row(
                    "F51",
                    &format!(
                        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:00Z",
                        2 + k / 30,
                        (k * 7) % 60
                    ),
                    150.0 + night as f64 * 0.1 + k as f64 * 1e-4,
                    -5.0 + k as f64 * 1e-5,
                ));
                s.push('\n');
            }
        }
        s
    }

    fn build(spec: &[(i64, usize)]) -> ObjectWindows {
        let raw = parse_psv(&psv(spec)).unwrap();
        build_object(
            "TestObj",
            "T1",
            "NEO",
            raw,
            &WindowsOptions {
                bundle_nights: 1,
                debias_dir: None,
                no_debias: true,
            },
            &None,
        )
        .unwrap()
    }

    #[test]
    fn mjd_conversion_pins() {
        assert_eq!(iso_to_mjd_utc("2024-12-27T00:00:00Z").unwrap(), 60_671.0);
        assert_eq!(iso_to_mjd_utc("2024-12-27").unwrap(), 60_671.0);
        let with_frac = iso_to_mjd_utc("2024-12-27T12:00:00.500Z").unwrap();
        assert!((with_frac - 60_671.500_005_787).abs() < 1e-8);
        assert!(iso_to_mjd_utc("2024-13-01T00:00:00Z").is_err());
        assert_eq!(mjd_utc_to_iso(60_671.0), "2024-12-27T00:00:00.000Z");
        // Round-trip through the civil-date algorithms.
        for mjd in [41_317, 50_000, 60_671, 61_000] {
            let (y, m, d) = civil_from_days(mjd - 40_587);
            assert_eq!(days_from_civil(y, m, d) + 40_587, mjd);
        }
    }

    #[test]
    fn tt_offset_pins() {
        // 2026: TAI−UTC = 37 s → TT−UTC = 69.184 s.
        assert!((tt_minus_utc_seconds(61_000.0).unwrap() - 69.184).abs() < 1e-9);
        // 1972: TAI−UTC = 10 s → 42.184 s.
        assert!((tt_minus_utc_seconds(41_320.0).unwrap() - 42.184).abs() < 1e-9);
        assert!(tt_minus_utc_seconds(41_000.0).is_err());
    }

    #[test]
    fn seed_and_schedule() {
        // 5 nights × 4 obs: seed needs 3 nights / 8 obs → seed at night idx
        // 2 (12 obs); cuts at night indices 2 and 3 (last cut = 2nd-to-last
        // night); every window in `full`.
        let ow = build(&[(0, 4), (1, 4), (2, 4), (5, 4), (9, 4)]);
        assert!(ow.eligible);
        assert_eq!(ow.windows.len(), 2);
        assert_eq!(ow.windows[0].cut_night, 60_602);
        assert_eq!(ow.windows[1].cut_night, 60_605);
        assert_eq!(ow.windows[0].n_obs_fit, 12);
        // Next-night targets exist for both windows.
        assert!(
            ow.windows
                .iter()
                .all(|w| w.targets.iter().any(|t| t.rung == "next"))
        );
        // Horizons measured from the last fit observation are positive.
        assert!(
            ow.windows
                .iter()
                .flat_map(|w| &w.targets)
                .all(|t| t.horizon_days > 0.0)
        );
        // Profile subset property: ladder ⊆ ci ⊆ full (as index sets).
        for w in &ow.windows {
            assert!(w.profiles.contains(&"full".to_string()));
            if w.profiles.contains(&"ladder".to_string()) {
                assert!(w.profiles.contains(&"ci".to_string()) || ow.windows.len() <= 2);
            }
        }
    }

    #[test]
    fn ineligible_reasons() {
        let ow = build(&[(0, 3), (1, 2)]);
        // 2 nights → intra-night path; obs within each night are minutes
        // apart (no >1 h gap before 8 obs) — check what it decided, and
        // that a reason is present when ineligible.
        if !ow.eligible {
            assert!(ow.ineligible_reason.is_some());
        }
        let ow = build(&[(0, 2), (1, 2), (2, 2)]);
        assert!(!ow.eligible);
        assert_eq!(
            ow.ineligible_reason.as_deref(),
            Some("insufficient_nights_or_obs")
        );
        let ow = build(&[(0, 4), (1, 4), (2, 4)]);
        assert!(!ow.eligible);
        assert_eq!(ow.ineligible_reason.as_deref(), Some("no_held_out_nights"));
    }

    #[test]
    fn cut_clearance_nudges() {
        // One observation 30 s after the nominal midnight cut: the cut must
        // move, and no observation may sit within 120 s of the final cut.
        let times = vec![60_602.5, 60_603.000_347, 60_603.6]; // 00:00:30 UTC
        let cut = nudged_cut(&times, 60_603.0).unwrap();
        for t in &times {
            assert!(
                (t - cut).abs() >= CUT_CLEARANCE_SECONDS / 86_400.0,
                "obs {t} within clearance of cut {cut}"
            );
        }
        // A too-tight straddling gap slides the cut to the nearest adequate
        // gap instead of erroring — continuous tracking across midnight is
        // one physical night, and the cut lands at its true boundary.
        let tight = vec![60_602.5, 60_602.999_65, 60_603.000_35, 60_603.6];
        let cut = nudged_cut(&tight, 60_603.0).unwrap();
        for t in &tight {
            assert!((t - cut).abs() >= CUT_CLEARANCE_SECONDS / 86_400.0);
        }
        // Ties resolve toward the later gap (the straddling pair stays in
        // the fit): the chosen cut is after the pair.
        assert!(!(60_602.999_65..=60_603.000_35).contains(&cut));
    }

    #[test]
    fn ladder_tolerance_and_caps() {
        // Dense early arc then nights near +7 and +30; a +90 rung has no
        // night within ±50% and must be absent.
        let ow = build(&[(0, 4), (1, 4), (2, 4), (3, 4), (10, 30), (32, 4)]);
        assert!(ow.eligible);
        let w0 = &ow.windows[0];
        let rungs: Vec<&str> = w0.targets.iter().map(|t| t.rung.as_str()).collect();
        assert!(rungs.contains(&"next"));
        assert!(rungs.contains(&"d7"), "rungs: {rungs:?}");
        assert!(rungs.contains(&"d30"));
        assert!(!rungs.contains(&"d90"));
        // The 30-obs night is capped at 12, evenly spaced.
        let d7_count = w0.targets.iter().filter(|t| t.rung == "d7").count();
        assert!(d7_count <= MAX_OBS_PER_NIGHT as usize);
        // A night is targeted at most once per window.
        let mut nights: Vec<i64> = w0
            .targets
            .iter()
            .map(|t| ow.observations[t.obs_idx as usize].night)
            .collect();
        nights.sort_unstable();
        let unique: std::collections::BTreeSet<i64> = nights.iter().copied().collect();
        // (multiple targets share a night; but distinct rungs never share)
        let mut rung_nights: std::collections::BTreeSet<(String, i64)> = Default::default();
        for t in &w0.targets {
            rung_nights.insert((t.rung.clone(), ow.observations[t.obs_idx as usize].night));
        }
        assert_eq!(
            rung_nights
                .iter()
                .map(|(_, n)| n)
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            unique.len()
        );
    }

    #[test]
    fn space_based_rows_are_excluded_but_kept() {
        let mut s =
            String::from("permID|provID|trkSub|mode|stn|sys|ctr|pos1|pos2|pos3|obsTime|ra|dec\n");
        for (night, stn, sys) in [
            (10, "F51", ""),
            (10, "F51", ""),
            (10, "F51", ""),
            (11, "F51", ""),
            (11, "F51", ""),
            (11, "F51", ""),
            (12, "F51", ""),
            (12, "F51", ""),
            (12, "C57", "ICRF_KM"),
            (14, "F51", ""),
        ] {
            s.push_str(&format!(
                "X1|||CCD|{stn}|{sys}|{}|{}||~|2024-12-{night:02}T0{}:00:00Z|150.0{night}|−5.0\n",
                if sys.is_empty() { "" } else { "399" },
                if sys.is_empty() { "" } else { "1234.0" },
                (night % 8) + 1,
            ));
        }
        let s = s.replace('~', "").replace('−', "-");
        let raw = parse_psv(&s).unwrap();
        assert_eq!(raw.iter().filter(|r| r.space_based).count(), 1);
        let ow = build_object(
            "T",
            "T",
            "NEO",
            raw,
            &WindowsOptions {
                bundle_nights: 1,
                debias_dir: None,
                no_debias: true,
            },
            &None,
        )
        .unwrap();
        assert_eq!(ow.n_excluded_space_based, 1);
        assert_eq!(
            ow.observations
                .iter()
                .filter(|o| o.excluded.as_deref() == Some("space_based"))
                .count(),
            1
        );
        // Excluded rows never appear among targets.
        for w in &ow.windows {
            for t in &w.targets {
                assert!(ow.observations[t.obs_idx as usize].excluded.is_none());
            }
        }
    }

    #[test]
    fn intra_night_impactor_schedule() {
        // One night, three tracklets of 8 obs separated by 2 h gaps →
        // cuts at the two later boundaries (>= 8 obs before each).
        let mut s = String::from(HDR);
        s.push('\n');
        for tracklet in 0..3 {
            for k in 0..8 {
                s.push_str(&row(
                    "F51",
                    &format!("2024-12-10T{:02}:{:02}:00Z", 2 + tracklet * 3, k),
                    150.0 + tracklet as f64 * 0.01 + k as f64 * 1e-4,
                    -5.0,
                ));
                s.push('\n');
            }
        }
        let raw = parse_psv(&s).unwrap();
        let ow = build_object(
            "Imp",
            "I1",
            "Impactor",
            raw,
            &WindowsOptions {
                bundle_nights: 1,
                debias_dir: None,
                no_debias: true,
            },
            &None,
        )
        .unwrap();
        assert!(ow.eligible);
        assert_eq!(ow.windows.len(), 2);
        assert!(ow.windows.iter().all(|w| w.intra_night));
        assert_eq!(ow.windows[0].n_obs_fit, 8);
        assert_eq!(ow.windows[1].n_obs_fit, 16);
        assert!(ow.windows.iter().all(|w| !w.targets.is_empty()));
        // Every profile carries every intra-night window.
        assert!(ow.windows.iter().all(|w| w.profiles.len() == 3));
    }

    #[test]
    fn duplicate_rows_get_distinct_keys() {
        let mut s = String::from(HDR);
        s.push('\n');
        let dup = row("F51", "2024-12-10T02:00:00Z", 150.0, -5.0);
        for _ in 0..3 {
            s.push_str(&dup);
            s.push('\n');
        }
        for night in 11..15 {
            for k in 0..4 {
                s.push_str(&row(
                    "F51",
                    &format!("2024-12-{night}T03:{k:02}:00Z"),
                    150.1,
                    -5.0,
                ));
                s.push('\n');
            }
        }
        let raw = parse_psv(&s).unwrap();
        let ow = build_object(
            "Dup",
            "D1",
            "NEO",
            raw,
            &WindowsOptions {
                bundle_nights: 1,
                debias_dir: None,
                no_debias: true,
            },
            &None,
        )
        .unwrap();
        let mut hashes: Vec<u64> = ow.observations.iter().map(|o| o.key_hash).collect();
        hashes.sort_unstable();
        hashes.dedup();
        assert_eq!(hashes.len(), ow.observations.len());
    }

    #[test]
    fn ci_profile_shape() {
        // 100 consecutive nights: ci = first 60 dense, then geometric ×1.3,
        // always including the last cut; ladder = seed + ~seed+30 + last.
        let spec: Vec<(i64, usize)> = (0..100).map(|n| (n, 4)).collect();
        let ow = build(&spec);
        assert!(ow.eligible);
        let n = ow.windows.len();
        let ci: Vec<u32> = ow
            .windows
            .iter()
            .filter(|w| w.profiles.contains(&"ci".to_string()))
            .map(|w| w.index)
            .collect();
        assert!(ci.len() >= 60, "ci = {}", ci.len());
        assert!(ci.len() < n, "ci must be a strict subset of full");
        // First 60 are consecutive.
        assert!(ci.windows(2).take(59).all(|p| p[1] == p[0] + 1));
        assert_eq!(*ci.last().unwrap(), (n - 1) as u32);
        let ladder: Vec<u32> = ow
            .windows
            .iter()
            .filter(|w| w.profiles.contains(&"ladder".to_string()))
            .map(|w| w.index)
            .collect();
        assert!(ladder.len() <= 4);
        assert!(ladder.contains(&0));
        assert!(ladder.contains(&((n - 1) as u32)));
        // ladder ⊆ ci: seed and last are ci members by construction; the
        // +30/+180 picks are dense-prefix members for this spec.
        for l in &ladder {
            assert!(ci.contains(l), "ladder window {l} not in ci");
        }
    }

    #[test]
    #[ignore = "needs the real fixture snapshot on disk (make fixtures)"]
    fn real_fixture_inventory() {
        let fixtures = Path::new("fixtures/psv");
        let out = std::env::temp_dir().join("covreal_windows_test.json");
        run(
            fixtures,
            &out,
            &WindowsOptions {
                bundle_nights: 1,
                debias_dir: None,
                no_debias: true,
            },
        )
        .unwrap();
        let m: WindowManifest =
            serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        let eligible = m.objects.iter().filter(|o| o.eligible).count();
        let windows: usize = m.objects.iter().map(|o| o.windows.len()).sum();
        eprintln!("real fixtures: {eligible} eligible, {windows} windows");
        assert!(eligible >= 40, "eligible = {eligible}");
        assert!((28_000..=36_000).contains(&windows), "windows = {windows}");
    }
}
