//! Branded HTML validation report.
//!
//! Renders sections 01-08 over the core reference channel, then a new
//! section 09 (OD diagnostics across channels) and a rebuilt section 10
//! (cross-channel fidelity matrix + ECDF + beeswarm + timing-vs-accuracy).
//! All non-rust channel data are embedded so the report can render any
//! channel-comparison view client-side without a separate run.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use crate::catalog::population_color;
use crate::schema::{ValidationResult, test_types};

/// Numerical-fidelity threshold: any metric whose absolute difference
/// against the Rust reference exceeds this is flagged as a failure.
/// Applied directly in whatever unit each metric is in.
pub const FIDELITY_THRESHOLD: f64 = 1e-10;

/// One AU in km, used to convert vector position differences (which are
/// stored in AU) to km for display.
const AU_KM: f64 = 149_597_870.700;

/// The one label every disclosure summary carries, so the affordance reads the
/// same everywhere; the specific method and caveats live in the collapsed body.
const DISCLOSURE_SUMMARY: &str = "Method";

/// The non-grav coefficient axes, stated ONCE on the s09b "Coeff" column header's
/// hover (the per-row cells now read only A1/A2/A3). A1/A2/A3 are the Marsden
/// radial/transverse/normal acceleration components.
const NG_COEFF_DEFS: &str = "A1 radial · A2 transverse · A3 normal";

/// The s09b verdict rule, on the "Result" column header's hover: the per-cell
/// marks (· pass, ✕ fail, hatched n/a) reuse the OD grid's glyph vocabulary, and
/// each cell's own hover restates its word and rule.
const NG_VERDICT_RULE: &str = "· pass (|z| ≤ 3σ on every coefficient) · ✕ fail · hatched where there is no non-zero JPL reference to test";

/// Hatched-state reason: a tool that produced no rows in this run.
const HATCH_NOT_IN_RUN: &str = "not in this run";
/// Hatched-state reason: a tool or axis with no wall clock in this run.
const HATCH_NOT_TIMED: &str = "not timed";
/// Hatched-state reason: a (tool, reference) pair with no stored comparison.
const HATCH_NOT_COMPARED: &str = "not compared";
/// Hatched-state reason: a comparison whose record is missing.
const HATCH_NO_RECORD: &str = "no record";
/// Hatched-state reason: an orbit-determination arc a tool never ran, stated as
/// an understatement (the three-state discipline: never charged as a failure).
const HATCH_NOT_ATTEMPTED: &str = "not attempted";
/// Hatched-state reason: a distribution channel absent from this invocation.
const HATCH_CHANNEL_ABSENT: &str = "channel absent from this invocation";

/// Pixel height shared by the full-width error-growth chart family (the two
/// growth charts and the STM cross-validation chart), so the family cannot drift.
const FULL_CHART_HEIGHT_PX: u32 = 520;

/// Scroll-wrapper class for the narrow, tall timing / cost grids — Part 2 §2.2
/// (T1 wall clock, T2 by population, G1–G3 per object) and the Section 1.3 OD
/// cost pair panels. The `grid-sticky` marker opts the wrapper into the ≥1360 px
/// overflow-drop rule so the column header pins to the viewport while a long body
/// scrolls; the wide fidelity grids keep the plain `grid-scroll` wrapper and
/// scroll horizontally instead. One name, so the rule and its wrappers cannot drift.
const GRID_SCROLL_STICKY: &str = "grid-scroll grid-sticky";

/// The part names, one table the rail and the part heads both read so the two can
/// never disagree; "Wall clock" is spelled without a hyphen everywhere.
const PART_NAME_1: &str = "Part 1 · Reference agreement";
/// Part 2 name, shared by rail and part head.
const PART_NAME_2: &str = "Part 2 · All tools side by side";
/// Part 3 name, shared by rail and part head.
const PART_NAME_3: &str = "Part 3 · Internals";
/// Part 4 name, shared by rail and part head.
const PART_NAME_4: &str = "Part 4 · Covariance realism";
/// Appendix name, shared by rail and part head.
const PART_NAME_APPENDIX: &str = "Appendix";

/// Channel display color from the brand palette.
fn channel_color(channel: &str) -> &'static str {
    match channel {
        "rust" => "#5b9bd5",   // Arctic Blue (primary brand)
        "python" => "#e8a040", // Celestial Amber
        "c" => "#40c0c0",      // Teal
        "cli" => "#a070d0",    // Violet
        "core" => "#d05080",   // Magenta
        _ => "#8b9198",
    }
}

/// Format a position error for display.
fn fmt_error(km: Option<f64>) -> String {
    match km {
        None => "---".to_string(),
        Some(km) => {
            if km < 0.001 {
                format!("{:.1} mm", km * 1e6)
            } else if km < 1.0 {
                format!("{:.1} m", km * 1000.0)
            } else if km < 1000.0 {
                format!("{:.2} km", km)
            } else if km < 1e6 {
                format!("{:.0} km", km)
            } else {
                // Past lunar orbit — show in AU.
                format!("{:.3} AU", km / AU_KM)
            }
        }
    }
}

/// Single micro-sign glyph (U+00B5 MICRO SIGN) for every `µ`-prefixed unit the
/// formatters emit, so `fmt_arcsec`/`fmt_ms`/`fmt_mas` cannot drift onto the
/// look-alike U+03BC GREEK SMALL LETTER MU (the one variant that had crept in).
const MICRO: char = '\u{00B5}';

/// Format an arcsec angular value with an auto-picked unit. Mirrors
/// `fmt_error` for the angular axis: nano-arcsec at the float64-ULP
/// floor, micro-arcsec for sub-mas drift, milli-arcsec for typical
/// ground-based residuals, arcsec for anything larger.
fn fmt_arcsec(v: f64) -> String {
    if v < 1e-6 {
        format!("{:.1} nas", v * 1e9)
    } else if v < 1e-3 {
        format!("{:.1} {MICRO}as", v * 1e6)
    } else if v < 1.0 {
        format!("{:.1} mas", v * 1000.0)
    } else if v < 60.0 {
        format!("{:.2}\"", v)
    } else {
        format!("{:.1}\" (≈{:.1}′)", v, v / 60.0)
    }
}

/// Empyrean Dynamics design tokens, inlined verbatim into every generated
/// report's `<style>` block.
///
/// Vendored as this repo's own asset. The report is a PUBLISHED artifact — it
/// is uploaded to the public GCS bucket — and a published artifact must not
/// carry a pointer to an internal repository. The previous version shipped a
/// CSS comment naming an internal docs path in the `<style>` block of every
/// report the bucket has ever served.
///
/// `include_str!` rather than a duplicated literal, so the values and their
/// provenance note live in exactly one place in this tree.
const BRAND_TOKENS_CSS: &str = include_str!("../assets/empyrean-tokens.css");

/// Test types rolled up per channel — in the summary JSON, the §10 matrix,
/// and the fidelity descriptor chips.
///
/// The OD family (`orbit_determination_radar`, `non_grav_recovery`) used to be
/// missing from this list, so those axes had no `by_test_type` entry anywhere:
/// not in the report, and — the load-bearing consequence — not in
/// `validation_summary.json`, which is what `ci-check` gates on. An axis the
/// gate cannot see is an axis the gate cannot enforce a floor for.
pub const SUMMARY_TEST_TYPES: [&str; 5] = [
    test_types::PROPAGATION,
    test_types::EPHEMERIS,
    test_types::ORBIT_DETERMINATION,
    test_types::ORBIT_DETERMINATION_RADAR,
    test_types::NON_GRAV_RECOVERY,
];

/// Format a duration in milliseconds.
fn fmt_ms(ms: f64) -> String {
    if ms < 0.001 {
        format!("{:.1} {MICRO}s", ms * 1000.0)
    } else if ms < 1.0 {
        format!("{:.2} ms", ms)
    } else if ms < 1000.0 {
        format!("{:.1} ms", ms)
    } else {
        format!("{:.2} s", ms / 1000.0)
    }
}

/// Per-channel summary row used by the channel-fidelity section.
#[derive(Debug, Clone)]
struct ChannelRollup {
    channel: String,
    n_rows: usize,
    /// Per-test-type counts: (n_compared, n_bit_identical, n_offending, p99_dr_km, max_dr_km).
    /// Test types: propagation, ephemeris, orbit_determination.
    by_test_type: BTreeMap<String, TestTypeRollup>,
    /// Largest vector position difference vs rust (km), across any test type.
    max_dr_km: f64,
    max_sep_diff_arcsec: f64,
    max_d_ra_diff_arcsec: f64,
    max_d_dec_diff_arcsec: f64,
    max_d_rho_diff_km: f64,
    max_d_lt_diff_s: f64,
    n_passing: usize,
    n_total_compared: usize,
    p50_time_ms: f64,
    p50_rust_time_ms: f64,
    p50_speed_ratio: f64,
}

#[derive(Debug, Clone, Default)]
struct TestTypeRollup {
    /// Rows of this test type the channel EMITTED, whether or not a
    /// reference row existed to pair them with.
    ///
    /// Distinct from `n_compared` on purpose. A test type only some channels
    /// produce, or whose reference-channel counterpart is absent, has
    /// `n_compared` structurally 0 and cannot carry a row floor. `n_rows` is
    /// what says "the pass ran and produced work", which is exactly the
    /// structural-death class the floors exist to catch.
    n_rows: usize,
    /// Rows of this test type that reported a converged fit.
    ///
    /// `n_rows` alone gates for EXISTENCE, not health: the runners emit a
    /// failure row for every non-convergent, unreadable or empty case
    /// (deliberately — a silent skip is what left the OD channel dead for
    /// months), so a row floor is satisfied by total breakage. Strict
    /// channels get their health check from `passing == total`; a test type
    /// with no reference to be compared against has no strict check to fall
    /// back on, and this is what a floor on such a type must actually test.
    /// Non-OD test types leave it equal to `n_rows` — they carry no
    /// convergence concept.
    n_converged: usize,
    n_compared: usize,
    n_bit_identical: usize,
    p50_dr_km: f64,
    p95_dr_km: f64,
    p99_dr_km: f64,
    max_dr_km: f64,
}

fn percentile(values: &mut [f64], p: f64) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((values.len() - 1) as f64 * p) as usize;
    values[idx]
}

/// Vector position difference (km) between channel row and core row.
fn vec_dr_km(a: &Option<[f64; 3]>, b: &Option<[f64; 3]>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) => {
            let d = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
            Some(d * AU_KM)
        }
        _ => None,
    }
}

/// Build the ChannelRollup for every channel present in `results`,
/// using `core` as the reference.
///
/// `core` (the no-FFI empyrean-core direct binary) is the canonical
/// reference because it exercises empyrean-core's internal API without
/// any cdylib / FFI roundtrip — every other channel (rust wrapper /
/// python wheel / c-ABI / cli) rides libempyrean.dylib and is
/// validated against `core` to confirm the FFI boundary is transparent.
/// Every detection-off arm — f64 and first-order — is a timing probe whose
/// transported state equals its detection-on sibling (bit-identical for
/// `none_detection_off`), so it carries no independent accuracy signal and is
/// kept OUT of the fidelity rollup, the per-channel counts, and the accuracy /
/// error-growth views — surfacing only in the dedicated timing panels.
fn is_timing_only_uncertainty(mode: Option<&str>) -> bool {
    mode.map(|m| m.contains("detection_off")).unwrap_or(false)
}

fn rollup_channels(results: &[ValidationResult]) -> Vec<ChannelRollup> {
    let mut by_channel: BTreeMap<String, Vec<&ValidationResult>> = BTreeMap::new();
    for r in results {
        if is_timing_only_uncertainty(r.propagation_uncertainty.as_deref()) {
            continue;
        }
        by_channel.entry(r.channel.clone()).or_default().push(r);
    }
    let Some(core_rows) = by_channel.get("core") else {
        return Vec::new();
    };
    // Key includes `propagation_uncertainty` so that a row produced under
    // the Jet1 STM path (`first_order`) is matched against the
    // same-mode core baseline rather than the f64 baseline (and vice
    // versa). Without this, the two modes silently overwrite in the
    // hash map and ~50% of the comparable rows hit a Jet1-vs-f64 diff
    // that's small but well above the 1e-10 fidelity threshold.
    // (object, dt_days, force_model, test_type, observer, uncertainty-mode).
    type RowKey = (String, i64, String, String, Option<String>, Option<String>);
    let mut core_by_key: HashMap<RowKey, &ValidationResult> = HashMap::new();
    for r in core_rows {
        core_by_key.insert(
            (
                r.object.clone(),
                r.dt_days as i64,
                r.force_model.clone(),
                r.test_type.clone(),
                r.observer.clone(),
                r.propagation_uncertainty.clone(),
            ),
            r,
        );
    }

    let mut out = Vec::new();
    // Channel order: core first (reference), then the rest alphabetically.
    let mut channel_order: Vec<&String> = by_channel.keys().collect();
    channel_order.sort_by(|a, b| match (a.as_str() == "core", b.as_str() == "core") {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.cmp(b),
    });

    for channel in channel_order {
        let rows = &by_channel[channel];
        let mut max_dr_km = 0.0f64;
        let mut max_sep_diff = 0.0f64;
        let mut max_d_ra = 0.0f64;
        let mut max_d_dec = 0.0f64;
        let mut max_d_rho = 0.0f64;
        let mut max_d_lt = 0.0f64;
        let mut n_passing = 0;
        let mut n_compared = 0;
        let mut channel_times: Vec<f64> = Vec::new();
        let mut core_match_times: Vec<f64> = Vec::new();
        // A position-vector-only `dr_km` stream was historically tracked
        // per row, but it is empty for `ephemeris` rows (those carry
        // RA/Dec/range, no `emp_pos_au`). That made the §10 per-test-type
        // matrix print `0/N · 0.0% ≤1e-10` for every replay channel's
        // ephemeris row — a display bug that contradicted the actual
        // bit-exact result. Fixed (empyrean-urfu) by tracking the row's
        // worst-metric diff and the bit-identical boolean per test_type,
        // so the matrix reports the actual cross-channel agreement
        // regardless of which scalar field carries the comparable metric.
        let mut by_tt_row_max_diff: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        let mut by_tt_bit_identical: BTreeMap<String, usize> = BTreeMap::new();
        let mut by_tt_compared: BTreeMap<String, usize> = BTreeMap::new();
        // Counted BEFORE the reference-pairing filter below, so a test type
        // this channel alone produces still reports a row count.
        let mut by_tt_rows: BTreeMap<String, usize> = BTreeMap::new();
        // Convergence per test type. `od_converged` is None for test types
        // that have no fit (propagation, ephemeris); those count as
        // converged so their rollup reads n_converged == n_rows and a
        // floor on them behaves exactly as before.
        let mut by_tt_converged: BTreeMap<String, usize> = BTreeMap::new();

        for r in rows {
            *by_tt_rows.entry(r.test_type.clone()).or_default() += 1;
            if r.od_converged.unwrap_or(true) {
                *by_tt_converged.entry(r.test_type.clone()).or_default() += 1;
            }
            let key = (
                r.object.clone(),
                r.dt_days as i64,
                r.force_model.clone(),
                r.test_type.clone(),
                r.observer.clone(),
                r.propagation_uncertainty.clone(),
            );
            let Some(&core_r) = core_by_key.get(&key) else {
                continue;
            };
            n_compared += 1;
            *by_tt_compared.entry(r.test_type.clone()).or_default() += 1;
            let mut row_max_diff = 0.0f64;
            // `any_metric_seen` is a separate flag so that rows where every
            // available metric diff is exactly 0.0 (the bit-identical case for
            // replay channels) are still counted: with every diff 0 we never
            // enter the `if d > row_max_diff` branch, yet the row must still
            // count toward the per-test-type matrix rather than be dropped.
            let mut any_metric_seen = false;

            // Vector position diff (the headline cross-channel agreement
            // metric — magnitude in km of channel.emp_pos - rust.emp_pos).
            if let Some(dr_km) = vec_dr_km(&r.emp_pos_au, &core_r.emp_pos_au) {
                any_metric_seen = true;
                max_dr_km = max_dr_km.max(dr_km);
                if dr_km > row_max_diff {
                    row_max_diff = dr_km;
                }
            }
            if let (Some(a), Some(b)) = (r.separation_arcsec, core_r.separation_arcsec) {
                any_metric_seen = true;
                let d = (a - b).abs();
                max_sep_diff = max_sep_diff.max(d);
                if d > row_max_diff {
                    row_max_diff = d;
                }
            }
            if let (Some(a), Some(b)) = (r.d_ra_arcsec, core_r.d_ra_arcsec) {
                any_metric_seen = true;
                let d = (a - b).abs();
                max_d_ra = max_d_ra.max(d);
                if d > row_max_diff {
                    row_max_diff = d;
                }
            }
            if let (Some(a), Some(b)) = (r.d_dec_arcsec, core_r.d_dec_arcsec) {
                any_metric_seen = true;
                let d = (a - b).abs();
                max_d_dec = max_d_dec.max(d);
                if d > row_max_diff {
                    row_max_diff = d;
                }
            }
            if let (Some(a), Some(b)) = (r.d_rho_km, core_r.d_rho_km) {
                any_metric_seen = true;
                let d = (a - b).abs();
                max_d_rho = max_d_rho.max(d);
                if d > row_max_diff {
                    row_max_diff = d;
                }
            }
            if let (Some(a), Some(b)) = (r.d_light_time_s, core_r.d_light_time_s) {
                any_metric_seen = true;
                let d = (a - b).abs();
                max_d_lt = max_d_lt.max(d);
                if d > row_max_diff {
                    row_max_diff = d;
                }
            }
            if any_metric_seen {
                // Track row-max-diff per test_type, regardless of which
                // metric was max (mixed units across propagation/eph/OD).
                by_tt_row_max_diff
                    .entry(r.test_type.clone())
                    .or_default()
                    .push(row_max_diff);
                if row_max_diff <= FIDELITY_THRESHOLD {
                    n_passing += 1;
                    *by_tt_bit_identical.entry(r.test_type.clone()).or_default() += 1;
                }
            }

            if let (Some(t), Some(rt)) = (r.emp_time_ms, core_r.emp_time_ms)
                && rt > 0.0
            {
                channel_times.push(t);
                core_match_times.push(rt);
            }
        }

        // Per-test-type rollups. n_bit_identical and p99/max are derived
        // from `by_tt_row_max_diff` (the per-row worst-metric diff in
        // whatever native unit dominated the row), so ephemeris rows —
        // which don't carry `emp_pos_au` — still report real agreement.
        let mut by_test_type: BTreeMap<String, TestTypeRollup> = BTreeMap::new();
        for tt in SUMMARY_TEST_TYPES {
            let n_compared_tt = by_tt_compared.get(tt).copied().unwrap_or(0);
            let drs = by_tt_row_max_diff.remove(tt).unwrap_or_default();
            let n_bit_identical = by_tt_bit_identical.get(tt).copied().unwrap_or(0);
            let p50 = if drs.is_empty() {
                0.0
            } else {
                percentile(&mut drs.clone(), 0.5)
            };
            let p95 = if drs.is_empty() {
                0.0
            } else {
                percentile(&mut drs.clone(), 0.95)
            };
            let p99 = if drs.is_empty() {
                0.0
            } else {
                percentile(&mut drs.clone(), 0.99)
            };
            let max_dr = drs.iter().cloned().fold(0.0f64, f64::max);
            by_test_type.insert(
                tt.to_string(),
                TestTypeRollup {
                    n_rows: by_tt_rows.get(tt).copied().unwrap_or(0),
                    n_converged: by_tt_converged.get(tt).copied().unwrap_or(0),
                    n_compared: n_compared_tt,
                    n_bit_identical,
                    p50_dr_km: p50,
                    p95_dr_km: p95,
                    p99_dr_km: p99,
                    max_dr_km: max_dr,
                },
            );
        }

        out.push(ChannelRollup {
            channel: channel.clone(),
            n_rows: rows.len(),
            by_test_type,
            max_dr_km,
            max_sep_diff_arcsec: max_sep_diff,
            max_d_ra_diff_arcsec: max_d_ra,
            max_d_dec_diff_arcsec: max_d_dec,
            max_d_rho_diff_km: max_d_rho,
            max_d_lt_diff_s: max_d_lt,
            n_passing,
            n_total_compared: n_compared,
            p50_time_ms: {
                if channel == "core" {
                    let mut all_rust: Vec<f64> = rows
                        .iter()
                        .filter_map(|r| r.emp_time_ms)
                        .filter(|t| *t > 0.0)
                        .collect();
                    percentile(&mut all_rust, 0.5)
                } else {
                    let mut t = channel_times.clone();
                    percentile(&mut t, 0.5)
                }
            },
            p50_rust_time_ms: {
                let mut t = core_match_times.clone();
                percentile(&mut t, 0.5)
            },
            p50_speed_ratio: {
                if channel == "core" {
                    1.0
                } else {
                    let mut ct = channel_times.clone();
                    let mut rt = core_match_times.clone();
                    let cp = percentile(&mut ct, 0.5);
                    let rp = percentile(&mut rt, 0.5);
                    if rp > 0.0 { cp / rp } else { f64::NAN }
                }
            },
        });
    }
    out
}

fn write_summary(rollups: &[ChannelRollup], path: &Path) -> Result<(), String> {
    let arr: Vec<serde_json::Value> = rollups
        .iter()
        .map(|r| {
            let nan_or = |v: f64| -> serde_json::Value {
                if v.is_nan() {
                    serde_json::Value::Null
                } else {
                    serde_json::json!(v)
                }
            };
            let by_tt: serde_json::Value = r
                .by_test_type
                .iter()
                .map(|(tt, x)| {
                    (
                        tt.clone(),
                        serde_json::json!({
                            "n_rows": x.n_rows,
                            "n_converged": x.n_converged,
                            "n_compared": x.n_compared,
                            "n_bit_identical": x.n_bit_identical,
                            "p50_dr_km": nan_or(x.p50_dr_km),
                            "p95_dr_km": nan_or(x.p95_dr_km),
                            "p99_dr_km": nan_or(x.p99_dr_km),
                            "max_dr_km": nan_or(x.max_dr_km),
                        }),
                    )
                })
                .collect::<serde_json::Map<_, _>>()
                .into();
            serde_json::json!({
                "channel": r.channel,
                "n_rows": r.n_rows,
                "n_passing": r.n_passing,
                "n_total_compared": r.n_total_compared,
                "max_dr_km": r.max_dr_km,
                "max_sep_diff_arcsec": r.max_sep_diff_arcsec,
                "max_d_ra_diff_arcsec": r.max_d_ra_diff_arcsec,
                "max_d_dec_diff_arcsec": r.max_d_dec_diff_arcsec,
                "max_d_rho_diff_km": r.max_d_rho_diff_km,
                "max_d_lt_diff_s": r.max_d_lt_diff_s,
                "p50_time_ms": nan_or(r.p50_time_ms),
                "p50_rust_time_ms": nan_or(r.p50_rust_time_ms),
                "p50_speed_ratio": nan_or(r.p50_speed_ratio),
                "by_test_type": by_tt,
            })
        })
        .collect();
    let summary = serde_json::json!({
        "fidelity_threshold": FIDELITY_THRESHOLD,
        "channels": arr,
    });
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    std::fs::write(
        path,
        serde_json::to_string_pretty(&summary).unwrap_or_default(),
    )
    .map_err(|e| format!("write {}: {e}", path.display()))
}

// ─────────────────────────────────────────────────────────────────────────
// Convergence matrix (§09) — who converged on what, and who was never asked
// ─────────────────────────────────────────────────────────────────────────

/// Escape a string for use inside a double-quoted HTML attribute.
///
/// The matrix puts free-form text — a channel's `notes`, a tool's error
/// string, an object name like `1I/'Oumuamua` — into `title=`, so a stray
/// quote would end the attribute and a stray `<` would open a tag.
fn attr_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Wrap a table-header fragment that carries a mathematical symbol, a variable
/// name or a unit so the header-uppercasing CSS (the `.u` class sets
/// `text-transform: none`) leaves its case intact: `σ_eq` must not become
/// `Σ_EQ`, `dRA·cos(δ)` must not turn declination into a difference, `ms` must
/// not read as `MS`. The one server-side definition; the client script carries a
/// `uhdr` twin for the headers it builds at runtime, and audit check H7 fails on
/// any bare symbol or unit token in a `<th>` outside this span.
fn uhdr(inner: &str) -> String {
    format!("<span class=\"u\">{inner}</span>")
}

/// The one decade ladder: bin 1 (nearest the page background) → bin 7 (runaway),
/// as `(background, ink)` hex pairs per theme. This array is the single source —
/// the ladder CSS is generated from it ([`ladder_css`]) and the WCAG contrast
/// test reads it — so the same literal is never repeated across selectors and the
/// palette cannot regress below AA (4.5:1) in either theme. The ink flip sits
/// where every bin's numeral clears 4.5:1: dark bins 1–4 use light ink, 5–7 dark
/// ink. Dark bin 4 is `#357093` (light-ink contrast 4.62:1) — nudged one shade
/// darker than its former mid-tone, where neither ink cleared AA.
const LADDER_DARK: [(&str, &str); 7] = [
    ("#1b2a38", "#e8eef4"),
    ("#22405a", "#e8eef4"),
    ("#2c5a7d", "#e8eef4"),
    ("#357093", "#e8eef4"),
    ("#5396c0", "#0b1620"),
    ("#7fb4d4", "#0b1620"),
    ("#b4d3e6", "#0b1620"),
];

/// The light-theme decade ladder, same shape as [`LADDER_DARK`]. Backgrounds are
/// unchanged from the measured light ramp; only the ink flip moves, to bins 1–5
/// dark ink, 6–7 light ink, so bins 4 and 5 (formerly light ink at 2.08:1 /
/// 3.09:1) now clear AA.
const LADDER_LIGHT: [(&str, &str); 7] = [
    ("#eef4f9", "#0b1620"),
    ("#d3e3f0", "#0b1620"),
    ("#aecde4", "#0b1620"),
    ("#82b0d3", "#0b1620"),
    ("#5590bf", "#0b1620"),
    ("#336fa3", "#eef4f9"),
    ("#1c4d7a", "#eef4f9"),
];

/// Emit the ladder CSS from [`LADDER_DARK`] / [`LADDER_LIGHT`]: per bin, the cell
/// background and the ink shared by the heatmap cell (`.cn`), the Part 2 chip
/// (`.lchip`) and the OD glyph (`.ocg`), for the dark (default) and light
/// (`:root.theme-light`) themes. One generator, so the three surfaces cannot
/// drift and the flip lives in exactly one place.
fn ladder_css() -> String {
    let mut s = String::new();
    for (i, &(bg, ink)) in LADDER_DARK.iter().enumerate() {
        let n = i + 1;
        s.push_str(&format!("  .gb{n} {{ background: {bg}; }}\n"));
        s.push_str(&format!(
            "  .gb{n} .cn, .lchip.gb{n}, .odgrid td.oc.gb{n} .ocg {{ color: {ink}; }}\n"
        ));
    }
    for (i, &(bg, ink)) in LADDER_LIGHT.iter().enumerate() {
        let n = i + 1;
        s.push_str(&format!(
            "  :root.theme-light .gb{n} {{ background: {bg}; }}\n"
        ));
        s.push_str(&format!(
            "  :root.theme-light .gb{n} .cn, :root.theme-light .lchip.gb{n}, :root.theme-light .odgrid td.oc.gb{n} .ocg {{ color: {ink}; }}\n"
        ));
    }
    s
}

/// Does any row in the run carry evidence that this comparator ran at all?
///
/// Mirrors the client-side `TOOLS_PRESENT` probe, and scans every axis
/// rather than just OD: the question is whether the comparator was part of
/// *this invocation*, not whether it fit anything. A comparator whose JSON
/// was never handed to `report` gets no OD panel at all, rather than an empty
/// panel that would imply it was considered and skipped.
fn od_tool_present(key: &str, results: &[ValidationResult]) -> bool {
    results.iter().any(|r| match key {
        "findorb" => {
            r.findorb_rms_residual.is_some()
                || r.findorb_n_obs_used.is_some()
                || r.findorb_vs_horizons_km.is_some()
                || r.findorb_d_ra_arcsec.is_some()
                || r.findorb_time_ms.is_some()
        }
        "oorb" => {
            r.channel == "oorb"
                || r.oorb_vs_horizons_km.is_some()
                || r.oorb_separation_arcsec.is_some()
                || r.oorb_time_ms.is_some()
        }
        "layup" => {
            r.layup_converged.is_some()
                || r.layup_reduced_chi2.is_some()
                || r.layup_chi2.is_some()
                || r.layup_n_obs_used.is_some()
                || r.layup_time_ms.is_some()
        }
        "orbfit" => {
            r.orbfit_rms_arcsec.is_some() || r.orbfit_error.is_some() || r.orbfit_time_ms.is_some()
        }
        "grss" => {
            r.grss_rms_arcsec.is_some()
                || r.grss_error.is_some()
                || r.grss_converged.is_some()
                || r.grss_vs_horizons_km.is_some()
                || r.grss_separation_arcsec.is_some()
                || r.grss_time_ms.is_some()
        }
        "jorbit" => r.jorbit_time_ms.is_some(),
        _ => false,
    })
}

// ═══════════════════════════════════════════════════════════════════
// REDESIGN (empyrean-k2rkx): server-side agreement grids
//
// The Part 1 agreement grids (H1 propagation, H2 ephemeris) and the Part 1
// timing grids (G1..G3) are rendered here, server-side, as one shared grid
// component. Rendering in Rust (rather than an older client-side heatmap
// builder) makes "all fifty objects, one sub-row per present tool"
// a testable invariant of the emitted HTML, keeps the single 45° "not
// compared" hatch to exactly one CSS class, and keeps the grid DOM-stable for
// a print / DOM parse. Colour is carried by seven decade-bin CSS classes
// (`.gb1`..`.gb7`, defined once in the page <style> with the measured dark and
// light ladders of design §3.1); the cell carries no text unless the reader
// turns on "show numbers".
// ═══════════════════════════════════════════════════════════════════

/// The three Part-1 grid axes. Each fixes the decade-bin edges of the one
/// colour ladder so a cell's colour means the same thing everywhere.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GridAxis {
    /// Propagation position offset, km (H1).
    PropKm,
    /// Ephemeris sky separation, milliarcseconds (H2).
    EphMas,
    /// Wall-clock, milliseconds (G1..G3 per-object timing grids).
    TimingMs,
}

impl GridAxis {
    /// Decade bin `1..=7` for a strictly-positive value on this axis, using
    /// the fixed physical decade edges of design §3.1. Bin 1 is below the
    /// measurement floor; bin 7 is the off-scale top decade (the runaway
    /// wedge is drawn there).
    fn bin(self, v: f64) -> u8 {
        let edges: [f64; 6] = match self {
            // km:  1 m,  100 m, 1 km, 100 km, 1e4 km, 1e6 km
            GridAxis::PropKm => [1e-3, 1e-1, 1.0, 1e2, 1e4, 1e6],
            // mas: 1 µas, 10 µas, 100 µas, 1 mas, 10 mas, 100 mas
            GridAxis::EphMas => [1e-3, 1e-2, 1e-1, 1.0, 1e1, 1e2],
            // ms:  100 µs, 1 ms, 10 ms, 100 ms, 1 s, 10 s
            GridAxis::TimingMs => [0.1, 1.0, 10.0, 100.0, 1e3, 1e4],
        };
        let mut b = 1u8;
        for &e in &edges {
            if v >= e {
                b += 1;
            } else {
                break;
            }
        }
        b.min(7)
    }
}

/// Milliarcsecond formatter for the ephemeris grid (mirrors the km / ms
/// formatters `fmt_error` / `fmt_ms`).
fn fmt_mas(mas: f64) -> String {
    if mas < 1e-3 {
        format!("{:.1} nas", mas * 1e6)
    } else if mas < 1.0 {
        format!("{:.1} {MICRO}as", mas * 1e3)
    } else if mas < 1e3 {
        format!("{:.2} mas", mas)
    } else {
        format!("{:.2}\"", mas / 1e3)
    }
}

/// Value formatter for a grid cell's hover / margin number.
fn grid_fmt(axis: GridAxis, v: f64) -> String {
    match axis {
        GridAxis::PropKm => fmt_error(Some(v)),
        GridAxis::EphMas => fmt_mas(v),
        GridAxis::TimingMs => fmt_ms(v),
    }
}

/// One tool's contribution to an agreement grid: its measured value keyed by
/// `(object, dt_days)`, and whether it ran this axis at all.
struct GridTool {
    /// Display label of the tool under test (`empyrean`, `ASSIST`, `find_orb`).
    label: String,
    /// Selector slug of the tool under test (`empyrean`, `assist`, `findorb`,
    /// `oorb`, `grss`) — must equal the value the Tool `<select>` emits, so the
    /// tool-selector's `data-tool` match lands on this panel.
    tool_slug: String,
    /// Reference this panel compares the tool against — its selector slug
    /// (`jpl`, `assist`, `findorb`) and display label. Every panel is a legal
    /// pair (tool vs reference) whose per-row difference is stored in the data;
    /// the tool-selector reveals the panel whose (tool, reference) it matches.
    ref_slug: String,
    ref_label: String,
    /// Did this pair carry any comparison on this axis? An empty pair is not
    /// emitted at all (the selector falls through to the generic hatched line).
    present: bool,
    /// `(object, dt_days_as_i64) -> value on this axis`.
    vals: HashMap<(String, i64), f64>,
    /// `(object, dt) -> Mahalanobis distance` in empyrean's propagated
    /// covariance, for the physical↔sigma toggle. Only the empyrean tool
    /// carries one (it is the only tool that propagates a covariance); empty
    /// otherwise. See the caption's synthetic-input caveat.
    sigma: HashMap<(String, i64), f64>,
}

/// One extra (non-default) pair panel spec: (tool label, tool slug, reference
/// slug, reference label, per-(object, dt) value map). Emitted only when the
/// map is non-empty.
type PanelSpec = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    HashMap<(String, i64), f64>,
);

/// Append a hidden hatched panel for every ordered pair of distinct externals.
/// Both externals carry positions on the axis, so the pair is legal and
/// reachable from the selects, but no shared-reference difference is stored so
/// it cannot be reconstructed (e.g. ASSIST vs find_orb). Selecting it shows the
/// "not compared" line rather than falling through to the generic fallback, so
/// the panel count equals the legal-pair count. `externals` is (tool_slug,
/// tool_label) for the present position-carrying externals on this axis.
fn push_cross_hatched_panels(
    tools: &mut Vec<GridTool>,
    externals: &[(&'static str, &'static str)],
) {
    for &(ta, la) in externals {
        for &(tb, lb) in externals {
            if ta != tb {
                tools.push(GridTool {
                    label: la.to_string(),
                    tool_slug: ta.to_string(),
                    ref_slug: tb.to_string(),
                    ref_label: lb.to_string(),
                    present: false,
                    vals: HashMap::new(),
                    sigma: HashMap::new(),
                });
            }
        }
    }
}

/// 3×3 inverse (row-major), `None` when singular / non-finite.
fn inv3(m: &[[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let (a, b, c) = (m[0][0], m[0][1], m[0][2]);
    let (d, e, f) = (m[1][0], m[1][1], m[1][2]);
    let (g, h, i) = (m[2][0], m[2][1], m[2][2]);
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if !det.is_finite() || det.abs() < 1e-300 {
        return None;
    }
    let iv = 1.0 / det;
    Some([
        [
            (e * i - f * h) * iv,
            (c * h - b * i) * iv,
            (b * f - c * e) * iv,
        ],
        [
            (f * g - d * i) * iv,
            (a * i - c * g) * iv,
            (c * d - a * f) * iv,
        ],
        [
            (d * h - e * g) * iv,
            (b * g - a * h) * iv,
            (a * e - b * d) * iv,
        ],
    ])
}

/// Mahalanobis distance √(δᵀ Σ⁻¹ δ) for a 3-vector.
fn maha3(dr: &[f64; 3], m: &[[f64; 3]; 3]) -> Option<f64> {
    let ci = inv3(m)?;
    let mut s = 0.0;
    for i in 0..3 {
        for j in 0..3 {
            s += dr[i] * ci[i][j] * dr[j];
        }
    }
    Some(s.max(0.0).sqrt())
}

/// Mahalanobis distance for a 2-vector against a 2×2 covariance.
fn maha2(d: &[f64; 2], m: &[[f64; 2]; 2]) -> Option<f64> {
    let det = m[0][0] * m[1][1] - m[0][1] * m[1][0];
    if !det.is_finite() || det.abs() < 1e-300 {
        return None;
    }
    let iv = 1.0 / det;
    let ci = [[m[1][1] * iv, -m[0][1] * iv], [-m[1][0] * iv, m[0][0] * iv]];
    let q = d[0] * (ci[0][0] * d[0] + ci[0][1] * d[1]) + d[1] * (ci[1][0] * d[0] + ci[1][1] * d[1]);
    Some(q.max(0.0).sqrt())
}

/// The objects a marker applies to.
struct GridMarks<'a> {
    /// Objects with a Marsden ΔT and the self-perturbers: the model gap the
    /// ASSIST sub-rows are daggered for.
    model_gap: &'a BTreeSet<&'a str>,
    /// Impactor objects: their forward horizons do not exist (arc ends).
    impactors: &'a BTreeSet<&'a str>,
}

/// Median of a slice of finite values (upper median for even counts, matching
/// the client-side quantile convention the old report used).
fn grid_median(mut xs: Vec<f64>) -> Option<f64> {
    xs.retain(|v| v.is_finite());
    if xs.is_empty() {
        return None;
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(xs[xs.len() / 2])
}

/// Render one cell for `tool` at `(object, dt)`. `is_gap` daggers it, `is_t0`
/// greys it with the spine. `sigma` is the Mahalanobis distance for the
/// physical↔sigma toggle (empyrean cells only); when present it rides in a
/// `data-sbin` attribute and a hidden `.cn-sig` span the sigma view swaps in.
/// Compact, ≤6-character in-cell label for the agreement grids (H1/H2). The
/// cell's colour already carries the decade, so the in-cell number is a
/// two-significant-figure refinement with a terse unit that fits a narrow
/// column; the full-precision value with its full unit rides in the cell's
/// hover title and in the right-margin worst column.
fn grid_compact(axis: GridAxis, v: f64) -> String {
    if v == 0.0 {
        return "0".to_string();
    }
    let a = v.abs();
    match axis {
        GridAxis::PropKm => {
            if a < 1.0 {
                let m = a * 1000.0;
                if m >= 10.0 {
                    format!("{m:.0}m")
                } else {
                    format!("{m:.1}m")
                }
            } else if a < 1e4 {
                if a >= 10.0 {
                    format!("{a:.0}km")
                } else {
                    format!("{a:.1}km")
                }
            } else {
                // km implied by the decade colour; keeps to ≤4 characters.
                format!("{a:.0e}")
            }
        }
        GridAxis::EphMas => {
            if a < 1e-3 {
                format!("{:.0}na", a * 1e6)
            } else if a < 1.0 {
                let u = a * 1e3;
                if u >= 10.0 {
                    format!("{u:.0}µa")
                } else {
                    format!("{u:.1}µa")
                }
            } else if a < 1e3 {
                if a >= 10.0 {
                    format!("{a:.0}ma")
                } else {
                    format!("{a:.1}ma")
                }
            } else {
                format!("{a:.0e}")
            }
        }
        GridAxis::TimingMs => {
            if a < 1.0 {
                let u = a * 1e3;
                if u >= 10.0 {
                    format!("{u:.0}µs")
                } else {
                    format!("{u:.1}µs")
                }
            } else if a < 1e3 {
                if a >= 10.0 {
                    format!("{a:.0}ms")
                } else {
                    format!("{a:.1}ms")
                }
            } else {
                format!("{:.1}s", a / 1e3)
            }
        }
    }
}

fn grid_cell(
    axis: GridAxis,
    v: Option<f64>,
    is_gap: bool,
    is_t0: bool,
    sigma: Option<f64>,
    n_rows: Option<usize>,
) -> String {
    match v {
        None => {
            let t0 = if is_t0 { " gt0" } else { "" };
            // The timing grids' one off-ladder state is an untimed cell; the
            // agreement grids' is a not-compared pair. Same hatch, axis-true hover.
            let reason = match axis {
                GridAxis::TimingMs => HATCH_NOT_TIMED,
                GridAxis::PropKm | GridAxis::EphMas => HATCH_NOT_COMPARED,
            };
            format!("<td class=\"gc hatch{t0}\" title=\"{reason}\"></td>")
        }
        Some(v) => {
            let bin = axis.bin(v);
            let runaway = if bin == 7 { " rw" } else { "" };
            let gap = if is_gap { " mg" } else { "" };
            let t0 = if is_t0 { " gt0" } else { "" };
            let txt = grid_compact(axis, v);
            // Several rows can share one cell (ephemeris observers): the cell is
            // their median and the hover says so, per the no-hidden-data rule.
            let full = match n_rows {
                Some(n) if n > 1 => format!("{} · median of {n} rows", grid_fmt(axis, v)),
                _ => grid_fmt(axis, v),
            };
            let hover = attr_escape(&full);
            let (sattr, ssig) = match sigma {
                Some(s) if s.is_finite() => (
                    format!(" data-sbin=\"{}\"", od_sigma_bin(s)),
                    format!("<span class=\"cn-sig\">{s:.1}σ</span>"),
                ),
                _ => (String::new(), String::new()),
            };
            format!(
                "<td class=\"gc gb{bin}{runaway}{gap}{t0}\"{sattr} title=\"{hover}\"><span class=\"cn\">{txt}</span>{ssig}</td>"
            )
        }
    }
}

/// Order the population blocks and their objects by one tool's worst value on
/// the grid axis (design §3.3): populations by their worst object, objects by
/// their own worst, both descending. `order_vals` is the tool that fixes the
/// order — empyrean vs the default reference — so every pair panel AND the
/// Section-1 cost heatmap reuse this one order and their rows line up.
fn prop_like_ordered_blocks<'a>(
    objects: &[(&'a str, &'a str)],
    dts: &[i64],
    order_vals: &HashMap<(String, i64), f64>,
) -> Vec<(&'a str, Vec<&'a str>)> {
    let mut blocks: BTreeMap<&'a str, Vec<&'a str>> = BTreeMap::new();
    for &(obj, pop) in objects {
        blocks.entry(pop).or_default().push(obj);
    }
    let worst_for = |obj: &str| -> f64 {
        dts.iter()
            .filter_map(|&dt| order_vals.get(&(obj.to_string(), dt)).copied())
            .filter(|v| v.is_finite())
            .fold(f64::NEG_INFINITY, f64::max)
    };
    let block_worst = |objs: &[&str]| -> f64 {
        objs.iter()
            .map(|o| worst_for(o))
            .fold(f64::NEG_INFINITY, f64::max)
    };
    let mut block_order: Vec<(&'a str, Vec<&'a str>)> = blocks.into_iter().collect();
    block_order.sort_by(|a, b| {
        block_worst(&b.1)
            .partial_cmp(&block_worst(&a.1))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for (_, objs) in block_order.iter_mut() {
        objs.sort_by(|a, b| {
            worst_for(b)
                .partial_cmp(&worst_for(a))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
    block_order
}

/// The shared row/column geometry of a prop-like grid: the objects (with
/// population), the horizons, the model-gap objects and the impactor objects.
type PropLikeAxes<'a> = (
    Vec<(&'a str, &'a str)>,
    Vec<i64>,
    BTreeSet<&'a str>,
    BTreeSet<&'a str>,
);

/// The 50 objects (with population), 17 horizons and per-object marks that every
/// prop-like grid shares. Objects and horizons come from the rust propagation
/// rows so an agreement grid and a cost heatmap on any axis carry the same rows
/// and columns.
fn prop_like_axes(results: &[ValidationResult]) -> PropLikeAxes<'_> {
    let mut objects: Vec<(&str, &str)> = Vec::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for r in results {
        if r.channel == "rust" && r.test_type == "propagation" && seen.insert(r.object.as_str()) {
            objects.push((r.object.as_str(), r.population.as_str()));
        }
    }
    objects.sort_by(|a, b| a.1.cmp(b.1).then(a.0.cmp(b.0)));

    let mut dts: Vec<i64> = results
        .iter()
        .filter(|r| r.channel == "rust" && r.test_type == "propagation")
        .map(|r| r.dt_days as i64)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    dts.sort();

    let mut model_gap: BTreeSet<&str> = BTreeSet::new();
    let mut impactors: BTreeSet<&str> = BTreeSet::new();
    for r in results {
        if r.ic_non_grav_dt.is_some() {
            model_gap.insert(r.object.as_str());
        }
        if r.population == "Self-Perturber" {
            model_gap.insert(r.object.as_str());
        }
        if r.population == "Impactor" {
            impactors.insert(r.object.as_str());
        }
    }
    (objects, dts, model_gap, impactors)
}

/// The right-margin summary column a prop-like grid ends each row with: the
/// agreement grids show the row's WORST horizon, the Section-1 cost heatmaps its
/// MEDIAN (the number the old per-object cost table showed, so nothing is lost).
#[derive(Clone, Copy, PartialEq, Eq)]
enum GridTrailing {
    Worst,
    Median,
}

impl GridTrailing {
    fn label(self) -> &'static str {
        match self {
            GridTrailing::Worst => "worst",
            GridTrailing::Median => "median",
        }
    }
    /// Reduce a row's finite horizon values to its trailing number; a
    /// non-finite result renders an empty trailing cell.
    fn reduce(self, vals: Vec<f64>) -> f64 {
        match self {
            GridTrailing::Worst => vals
                .into_iter()
                .filter(|v| v.is_finite())
                .fold(f64::NEG_INFINITY, f64::max),
            GridTrailing::Median => grid_median(vals).unwrap_or(f64::NEG_INFINITY),
        }
    }
}

/// One prop-like grid table (agreement or Section-1 cost), drawn from one tool's
/// values keyed by `(object, dt)`. The agreement grids and the cost heatmaps use
/// this same body so the compact-in-cell / full-on-hover idiom, the population
/// header rows and the impactor arc-ends mark are defined once.
struct GridTable<'a> {
    axis: GridAxis,
    table_id: &'a str,
    dts: &'a [i64],
    vals: &'a HashMap<(String, i64), f64>,
    /// Mahalanobis distance per cell for the physical↔sigma toggle (empyrean
    /// agreement cells only); empty for cost heatmaps.
    sigma: &'a HashMap<(String, i64), f64>,
    /// Rows behind each cell, so a cell pooling several rows (ephemeris
    /// observers) says "median of n rows" on hover; `None` when every cell is
    /// one row.
    counts: Option<&'a HashMap<(String, i64), usize>>,
    marks: &'a GridMarks<'a>,
    /// Mark the ASSIST model gap (agreement panels whose pair includes ASSIST);
    /// never on a cost heatmap, where timing carries no force-model gap.
    show_model_gap: bool,
    trailing: GridTrailing,
    /// Emit the agreement per-column median footers; the cost heatmaps do not.
    footers: bool,
}

/// Render one prop-like grid table body: the scroll container, header, the
/// population header rows and object rows, the trailing column, and the
/// agreement footers when asked. Shared by the agreement grids and the
/// Section-1 cost heatmaps.
fn render_grid_table(t: &GridTable, block_order: &[(&str, Vec<&str>)]) -> String {
    let axis = t.axis;
    let dts = t.dts;
    let n_forward = dts.iter().filter(|&&d| d > 0).count();
    let mut h = String::new();
    h.push_str(&format!(
        "<div class=\"grid-scroll agrid-scroll\"><table class=\"agrid ahgrid\" id=\"{}\"><thead><tr>",
        t.table_id
    ));
    h.push_str("<th class=\"gobj\">object</th>");
    for &dt in dts {
        let cls = if dt == 0 { "gdt gt0h" } else { "gdt" };
        h.push_str(&format!("<th class=\"{cls}\">{}</th>", dt_col_label(dt)));
    }
    h.push_str(&format!(
        "<th class=\"gworst\">{}</th></tr></thead><tbody>",
        t.trailing.label()
    ));

    for (pop, members) in block_order {
        // Population header row — the per-population median at each horizon, in
        // the same colour ramp.
        let pop_gap = t.show_model_gap && members.iter().any(|o| t.marks.model_gap.contains(o));
        h.push_str(&format!(
            "<tr class=\"gpophdr\"><td class=\"gpopname\">{} ({})</td>",
            attr_escape(pop),
            members.len()
        ));
        let mut row_vals: Vec<f64> = Vec::new();
        for &dt in dts {
            let vals: Vec<f64> = members
                .iter()
                .filter_map(|o| t.vals.get(&(o.to_string(), dt)).copied())
                .collect();
            let m = grid_median(vals);
            if let Some(mv) = m {
                row_vals.push(mv);
            }
            h.push_str(&grid_cell(axis, m, pop_gap, dt == 0, None, None));
        }
        h.push_str(&worst_cell(axis, t.trailing.reduce(row_vals)));
        h.push_str("</tr>");

        // One row per object.
        for obj in members {
            let is_impactor = t.marks.impactors.contains(obj);
            let is_gap_obj = t.show_model_gap && t.marks.model_gap.contains(obj);
            h.push_str(&format!(
                "<tr class=\"gobjr\"><td class=\"gobjname2\">{}</td>",
                obj_label(obj, t.marks, t.show_model_gap)
            ));
            let mut arc_done = false;
            let mut obj_vals: Vec<f64> = Vec::new();
            for &dt in dts {
                if is_impactor && dt > 0 {
                    if !arc_done {
                        h.push_str(&format!(
                            "<td class=\"arcends hatch\" colspan=\"{n_forward}\" title=\"arc ends at impact — no forward horizon\"></td>"
                        ));
                        arc_done = true;
                    }
                    continue;
                }
                let v = t.vals.get(&(obj.to_string(), dt)).copied();
                if let Some(x) = v
                    && x.is_finite()
                {
                    obj_vals.push(x);
                }
                let sg = t.sigma.get(&(obj.to_string(), dt)).copied();
                let n = t
                    .counts
                    .and_then(|c| c.get(&(obj.to_string(), dt)).copied());
                h.push_str(&grid_cell(axis, v, is_gap_obj, dt == 0, sg, n));
            }
            h.push_str(&worst_cell(axis, t.trailing.reduce(obj_vals)));
            h.push_str("</tr>");
        }
    }

    if t.footers {
        let obj_names: Vec<&str> = block_order
            .iter()
            .flat_map(|(_, m)| m.iter().copied())
            .collect();
        h.push_str("</tbody><tfoot>");
        footer_row(
            &mut h,
            axis,
            "median / horizon",
            dts,
            t.vals,
            &obj_names,
            None,
        );
        if t.show_model_gap && obj_names.iter().any(|o| t.marks.model_gap.contains(o)) {
            footer_row(
                &mut h,
                axis,
                "excl. model gap",
                dts,
                t.vals,
                &obj_names,
                Some(t.marks.model_gap),
            );
        }
        h.push_str("</tfoot></table></div>");
    } else {
        h.push_str("</tbody></table></div>");
    }
    h
}

/// The shared agreement-grid renderer. Emits one `<table>` per grid, wrapped
/// in a scroll container, preceded by a summary line and a colour key.
fn render_agreement_grid(
    id: &str,
    axis: GridAxis,
    objects: &[(&str, &str)],
    dts: &[i64],
    tools: &[GridTool],
    marks: &GridMarks,
) -> String {
    // empyrean (tools[0]) fixes the population and within-block object order so
    // every per-tool table — and the Section-1 cost heatmap — line up row for
    // row (design §3.3). One ordering function serves both.
    let block_order = prop_like_ordered_blocks(objects, dts, &tools[0].vals);

    // Per-tool summary line (p90 / median / worst / cell count) computed on that
    // tool's own cells, so the visible line always matches the pair the
    // tool-selector shows.
    // Returns the visible summary line (numbers only) and the hover title that
    // carries the "worst …" clause the WORST column and the darkest cell already
    // state in view, so the line itself stays lean.
    let summary_for = |tool: &GridTool| -> (String, String) {
        let mut vals: Vec<f64> = tool
            .vals
            .values()
            .copied()
            .filter(|v| v.is_finite())
            .collect();
        if vals.is_empty() {
            return (
                "No comparable rows on this axis in this run.".to_string(),
                String::new(),
            );
        }
        let p50 = percentile(&mut vals, 0.5);
        let p90 = percentile(&mut vals, 0.9);
        // Tie-break on (object, dt) so the "worst" pick never depends on the
        // map's iteration order — the summary line must be reproducible.
        let (worst_v, worst_obj, worst_dt) = tool.vals.iter().filter(|(_, v)| v.is_finite()).fold(
            (f64::NEG_INFINITY, "", 0i64),
            |acc, ((o, d), &v)| {
                if v > acc.0 || (v == acc.0 && (o.as_str(), *d) < (acc.1, acc.2)) {
                    (v, o.as_str(), *d)
                } else {
                    acc
                }
            },
        );
        let line = format!(
            "p90 {} · median {} · {} cells",
            grid_fmt(axis, p90),
            grid_fmt(axis, p50),
            vals.len(),
        );
        let title = format!(
            "worst {} {} at {}",
            attr_escape(worst_obj),
            grid_fmt(axis, worst_v),
            dt_col_label(worst_dt),
        );
        (line, title)
    };

    let mut h = String::new();
    // Shared decade key, once, outside the panels. Pair-neutral: it never names
    // a tool, so it is legal for every selected pair.
    h.push_str(&grid_key_html(axis));
    // One pair-panel per legal (tool, reference) pair, each computed from the
    // per-row difference stored for that pair. The tool-selector shows exactly
    // one panel at a time; the default (empyrean vs JPL) is visible and the rest
    // are hidden until selected. A pair with no stored comparison on this axis
    // is not emitted, so the selector falls through to the generic "not
    // compared" line at the end. The default table keeps the bare `id` so the
    // σ-view toggle finds its data-sbin cells.
    h.push_str(&format!(
        "<div class=\"pair-panels\" data-section=\"{id}\">"
    ));
    for (ti, tool) in tools.iter().enumerate() {
        let tool_slug = tool.tool_slug.as_str();
        let ref_slug = tool.ref_slug.as_str();
        let table_id = if ti == 0 {
            id.to_string()
        } else {
            format!("{id}-{tool_slug}-{ref_slug}")
        };
        let default_attr = if ti == 0 { " data-default=\"1\"" } else { "" };
        let hidden_attr = if ti == 0 { "" } else { " hidden" };
        h.push_str(&format!(
            "<div class=\"pair-panel\" data-tool=\"{tool_slug}\" data-ref=\"{ref_slug}\"{default_attr}{hidden_attr}>"
        ));
        if !tool.present {
            h.push_str(&format!(
                "<div class=\"pair-notcompared\">{} vs {} is not compared on this axis in this run.</div></div>",
                attr_escape(&tool.label),
                attr_escape(&tool.ref_label)
            ));
            continue;
        }
        let (summary_line, summary_title) = summary_for(tool);
        h.push_str(&format!(
            "<div class=\"grid-summary\" id=\"{table_id}-summary\" title=\"{summary_title}\">{summary_line}</div>"
        ));
        // ASSIST omits force terms (Marsden ΔT, self-perturbers) some objects
        // need, so mark the model gap whenever ASSIST is on either side.
        let tool_is_assist = tool_slug == "assist" || ref_slug == "assist";

        // Pair-scoped model-gap caveat: emitted only inside the panels whose
        // pair includes ASSIST, so Section 1 never names a tool outside the
        // selected pair. The daggered rows and the excl-model-gap footer median
        // below carry the same information per row.
        if tool_is_assist
            && block_order
                .iter()
                .any(|(_, m)| m.iter().any(|o| marks.model_gap.contains(o)))
        {
            h.push_str("<div class=\"pair-caveat\">&dagger; model gap: ASSIST omits a force term (Marsden ΔT, self-perturbers) some objects need; those rows are marked and a second footer median excludes them.</div>");
        }

        h.push_str(&render_grid_table(
            &GridTable {
                axis,
                table_id: &table_id,
                dts,
                vals: &tool.vals,
                sigma: &tool.sigma,
                counts: None,
                marks,
                show_model_gap: tool_is_assist,
                trailing: GridTrailing::Worst,
                footers: true,
            },
            &block_order,
        ));
        h.push_str("</div>");
    }
    // Generic fallback for an illegal or empty pair (non-JPL reference, or the
    // same tool on both sides): the section body collapses to one hatched line,
    // never an empty panel.
    h.push_str("<div class=\"pair-notcompared generic\" hidden>Not compared &mdash; this pair shares no stored comparison on this axis. Choose a tool paired with the reference.</div>");
    h.push_str("</div>");

    h
}

/// One per-column median footer row.
fn footer_row(
    h: &mut String,
    axis: GridAxis,
    label: &str,
    dts: &[i64],
    vals_map: &HashMap<(String, i64), f64>,
    obj_names: &[&str],
    exclude: Option<&BTreeSet<&str>>,
) {
    h.push_str(&format!(
        "<tr class=\"gfoot\"><td class=\"gtool\">{}</td>",
        attr_escape(label)
    ));
    for &dt in dts {
        let vals: Vec<f64> = obj_names
            .iter()
            .filter(|o| exclude.map(|e| !e.contains(**o)).unwrap_or(true))
            .filter_map(|o| vals_map.get(&(o.to_string(), dt)).copied())
            .collect();
        match grid_median(vals) {
            Some(m) => h.push_str(&format!(
                "<td class=\"gfc\" title=\"{}\">{}</td>",
                attr_escape(&grid_fmt(axis, m)),
                grid_fmt(axis, m)
            )),
            None => h.push_str("<td class=\"gfc\">·</td>"),
        }
    }
    h.push_str("<td class=\"gworst\"></td></tr>");
}

/// Right-margin worst cell: a decade-coloured swatch plus the full-precision
/// value in legible page text. (The micro-bar was dropped to reclaim the
/// column width the 17 horizons need to fit without clipping.)
fn worst_cell(axis: GridAxis, worst: f64) -> String {
    if !worst.is_finite() {
        return "<td class=\"gworst\"></td>".to_string();
    }
    let bin = axis.bin(worst);
    format!(
        "<td class=\"gworst\"><span class=\"wdot gb{bin}\"></span><span class=\"wnum\">{}</span></td>",
        grid_fmt(axis, worst)
    )
}

/// Object row label with the model-gap annotation. `show_gap` is true only in a
/// panel whose pair includes ASSIST, so the dagger (and its hover explanation)
/// never renders for a pair that does not involve the tool that carries the gap.
/// Impactor forward cells are the hatched arc-ends state (empty text, the "arc
/// ends at impact — no forward horizon" reason on hover), so they carry no
/// object tag.
fn obj_label(obj: &str, marks: &GridMarks, show_gap: bool) -> String {
    let mut s = attr_escape(obj);
    if show_gap && marks.model_gap.contains(obj) {
        s.push_str(" <span class=\"tag-gap\" title=\"model gap: ASSIST omits a force term this object needs (Marsden delta-T or a self-perturber); the excl. model-gap footer median drops these rows\">&dagger;</span>");
    }
    s
}

/// dt column header label (`-15y`, `-30d`, `t0`, `+1y`, …).
fn dt_col_label(dt: i64) -> String {
    if dt == 0 {
        return "t0".to_string();
    }
    let sign = if dt > 0 { "+" } else { "-" };
    let a = dt.unsigned_abs();
    if a.is_multiple_of(365) {
        format!("{sign}{}y", a / 365)
    } else if a >= 365 {
        format!("{sign}{:.1}y", a as f64 / 365.0)
    } else {
        format!("{sign}{a}d")
    }
}

/// The colour key for an axis: seven decade swatches with only the two end
/// labels shown — each swatch's full bin label and physical anchor ride its
/// hover title — plus the state marks that axis can show (runaway and
/// not-compared on the agreement axes, not-timed on the wall-clock axis). The
/// model-gap caveat is pair-scoped and lives inside the panels that carry the
/// mark, never in this shared key.
fn grid_key_html(axis: GridAxis) -> String {
    // These bin labels are pre-escaped HTML fragments (entities such as `&lt;`,
    // `&gt;`, `µ`) used verbatim in both text content and a `title="…"` hover.
    // Keep them attribute-safe: entity-encoded, never a raw double-quote. They
    // must NOT be passed through `attr_escape` — that would re-escape the `&` of
    // every entity (`&lt;` → `&amp;lt;`) and render the entity literally.
    let bins: [&str; 7] = match axis {
        GridAxis::PropKm => [
            "&lt;1 m",
            "1 m–100 m",
            "100 m–1 km",
            "1–100 km",
            "100 km–1e4 (R⊕)",
            "1e4–1e6 (lunar)",
            "&gt;1e6 (1 AU)",
        ],
        GridAxis::EphMas => [
            "&lt;1 µas",
            "1–10 µas",
            "10–100 µas",
            "100 µas–1 mas (Gaia)",
            "1–10 mas",
            "10–100 mas",
            "&gt;100 mas",
        ],
        GridAxis::TimingMs => [
            "&lt;100 µs",
            "100 µs–1 ms",
            "1–10 ms",
            "10–100 ms",
            "100 ms–1 s",
            "1–10 s",
            "&gt;10 s",
        ],
    };
    // Only the two end labels are shown; every swatch carries its own bin label
    // and physical anchor as a hover title (text-diet rule 2c).
    let (lo_vis, hi_vis) = match axis {
        GridAxis::PropKm => ("&lt;1 m", "&gt;1e6 km"),
        GridAxis::EphMas => ("&lt;1 µas", "&gt;100 mas"),
        GridAxis::TimingMs => ("&lt;100 µs", "&gt;10 s"),
    };
    let mut s = String::from("<div class=\"grid-key\">");
    for (i, b) in bins.iter().enumerate() {
        let vis = if i == 0 {
            lo_vis
        } else if i == bins.len() - 1 {
            hi_vis
        } else {
            ""
        };
        s.push_str(&format!(
            "<span class=\"gk\"><span class=\"gk-sw gb{}\" title=\"{}\"></span>{}</span>",
            i + 1,
            b,
            vis
        ));
    }
    // A wall-clock table has one state outside the ramp, an untimed cell; the
    // agreement grids have two, a runaway and a pair that was not compared. Each
    // mark keeps a short label, its meaning in the hover title.
    match axis {
        GridAxis::TimingMs => {
            s.push_str(&format!("<span class=\"gk\"><span class=\"gk-sw hatch\" title=\"cell has no timing sample\"></span>{HATCH_NOT_TIMED}</span>"));
        }
        GridAxis::PropKm | GridAxis::EphMas => {
            s.push_str("<span class=\"gk\"><span class=\"gk-sw rw\" title=\"off-scale: above the top decade\"></span>runaway</span>");
            s.push_str(&format!("<span class=\"gk\"><span class=\"gk-sw hatch\" title=\"tool has no rows for this pair\"></span>{HATCH_NOT_COMPARED}</span>"));
        }
    }
    s.push_str("</div>");
    s
}

/// The uncertainty arm Section 1 keys empyrean's agreement and cost heatmaps on:
/// the rust channel's first-order (Jet1) arm. Defined once so the agreement
/// values, their row order and the cost wall-clock all read the same rows.
const EMP_SECTION1_ARM: &str = "first_order_detection_on";

/// Whether `r` is an empyrean propagation row Section 1 keys on. Both the
/// propagation agreement values and the propagation cost heatmap select cells
/// through this one predicate, so they can never drift apart.
fn is_emp_prop_section1_row(r: &ValidationResult) -> bool {
    r.channel == "rust"
        && r.test_type == "propagation"
        && r.propagation_uncertainty.as_deref() == Some(EMP_SECTION1_ARM)
}

/// `(object, dt) →` empyrean's propagation position offset vs JPL (km) on the
/// Section-1 rows. Feeds the H1 agreement grid AND fixes the propagation cost
/// heatmap's row order (one definition, so the two heatmaps line up row for row).
fn emp_prop_agreement_vals(results: &[ValidationResult]) -> HashMap<(String, i64), f64> {
    let mut emp: HashMap<(String, i64), f64> = HashMap::new();
    for r in results {
        if is_emp_prop_section1_row(r)
            && let Some(v) = r.emp_vs_horizons_km
        {
            emp.insert((r.object.clone(), r.dt_days as i64), v);
        }
    }
    emp
}

/// `(object, dt) →` empyrean's sky-plane separation vs JPL (mas), meaned over
/// observers, on the Section-1 ephemeris rows. Feeds the H2 agreement grid AND
/// fixes the ephemeris cost heatmap's row order.
fn emp_eph_agreement_vals(results: &[ValidationResult]) -> HashMap<(String, i64), f64> {
    let mut acc: HashMap<(String, i64), (f64, u32)> = HashMap::new();
    for r in results {
        if r.channel == "rust"
            && r.test_type == "ephemeris"
            && r.propagation_uncertainty.as_deref() == Some(EMP_SECTION1_ARM)
            && let Some(v) = r.separation_arcsec
        {
            let e = acc
                .entry((r.object.clone(), r.dt_days as i64))
                .or_insert((0.0, 0));
            e.0 += v;
            e.1 += 1;
        }
    }
    acc.into_iter()
        .filter(|(_, (_, n))| *n > 0)
        .map(|(k, (s, n))| (k, (s / n as f64) * 1e3))
        .collect()
}

/// Build the propagation vs-JPL agreement grid (H1).
fn build_prop_agreement_grid(results: &[ValidationResult]) -> String {
    let emp = emp_prop_agreement_vals(results);
    let mut emp_sigma: HashMap<(String, i64), f64> = HashMap::new();
    for r in results {
        if is_emp_prop_section1_row(r) {
            let key = (r.object.clone(), r.dt_days as i64);
            // Mahalanobis distance of the empyrean−JPL offset in empyrean's
            // propagated position covariance (AU), where it carries one.
            if let (Some(p), Some(rf), Some(c)) = (r.emp_pos_au, r.ref_pos_au, r.emp_pos_cov_au2) {
                let dr = [p[0] - rf[0], p[1] - rf[1], p[2] - rf[2]];
                if let Some(d) = maha3(&dr, &c) {
                    emp_sigma.insert(key, d);
                }
            }
        }
    }
    // Every cross-tool difference is stored on the core rows. Each is a legal
    // pair whose one side is empyrean or JPL (the only pairs reconstructable
    // from the stored differences — no raw ASSIST/find_orb position vectors
    // exist, so e.g. ASSIST-vs-find_orb has no panel and renders hatched).
    let mut emp_assist: HashMap<(String, i64), f64> = HashMap::new();
    let mut emp_findorb: HashMap<(String, i64), f64> = HashMap::new();
    let mut emp_oorb: HashMap<(String, i64), f64> = HashMap::new();
    let mut emp_grss: HashMap<(String, i64), f64> = HashMap::new();
    let mut assist: HashMap<(String, i64), f64> = HashMap::new();
    let mut findorb: HashMap<(String, i64), f64> = HashMap::new();
    let mut oorb: HashMap<(String, i64), f64> = HashMap::new();
    let mut grss: HashMap<(String, i64), f64> = HashMap::new();
    for r in results {
        if r.channel == "core" && r.test_type == "propagation" {
            let key = (r.object.clone(), r.dt_days as i64);
            if let Some(v) = r.emp_vs_assist_km {
                emp_assist.entry(key.clone()).or_insert(v);
            }
            if let Some(v) = r.emp_vs_findorb_km {
                emp_findorb.entry(key.clone()).or_insert(v);
            }
            if let Some(v) = r.emp_vs_oorb_km {
                emp_oorb.entry(key.clone()).or_insert(v);
            }
            if let Some(v) = r.emp_vs_grss_km {
                emp_grss.entry(key.clone()).or_insert(v);
            }
            if let Some(v) = r.assist_vs_horizons_km {
                assist.entry(key.clone()).or_insert(v);
            }
            if let Some(v) = r.findorb_vs_horizons_km {
                findorb.entry(key.clone()).or_insert(v);
            }
            if let Some(v) = r.oorb_vs_horizons_km {
                oorb.entry(key.clone()).or_insert(v);
            }
            if let Some(v) = r.grss_vs_horizons_km {
                grss.entry(key).or_insert(v);
            }
        }
    }
    // Default (empyrean vs JPL) first, then empyrean vs each external, then each
    // external vs JPL. Only pairs that carry data are emitted.
    let mut tools = vec![GridTool {
        label: "empyrean".into(),
        tool_slug: "empyrean".into(),
        ref_slug: "jpl".into(),
        ref_label: "JPL".into(),
        present: true,
        vals: emp,
        sigma: emp_sigma,
    }];
    // Externals carrying a propagation position, from the SINGLE source shared
    // with the Section-1 timing panels (`axis_present_externals`): heatmap pairs
    // and timing pairs cannot drift because both derive from this one call.
    let ext_present: Vec<(&str, &str)> = axis_present_externals(results, "propagation");
    let extra: Vec<PanelSpec> = vec![
        ("empyrean", "empyrean", "assist", "ASSIST", emp_assist),
        ("empyrean", "empyrean", "findorb", "find_orb", emp_findorb),
        ("empyrean", "empyrean", "oorb", "OpenOrb", emp_oorb),
        ("empyrean", "empyrean", "grss", "GRSS", emp_grss),
        ("ASSIST", "assist", "jpl", "JPL", assist),
        ("find_orb", "findorb", "jpl", "JPL", findorb),
        ("OpenOrb", "oorb", "jpl", "JPL", oorb),
        ("GRSS", "grss", "jpl", "JPL", grss),
    ];
    for (label, tool_slug, ref_slug, ref_label, vals) in extra {
        if !vals.is_empty() {
            tools.push(GridTool {
                label: label.into(),
                tool_slug: tool_slug.into(),
                ref_slug: ref_slug.into(),
                ref_label: ref_label.into(),
                present: true,
                vals,
                sigma: HashMap::new(),
            });
        }
    }
    // Legal external-vs-external pairs (e.g. ASSIST vs find_orb): both carry
    // positions but no shared-reference difference is stored, so they render a
    // hatched "not compared" panel.
    push_cross_hatched_panels(&mut tools, &ext_present);
    render_prop_like_grid("h1-prop", GridAxis::PropKm, results, tools)
}

/// Build the ephemeris sky-plane agreement grid (H2). Every panel is a legal
/// pair on the ephemeris axis: empyrean or an external vs JPL (from the stored
/// per-site separation), or empyrean vs an external (the norm of their signed
/// dRA·cosδ / dDec offsets vs Horizons, differenced per site). ASSIST persists
/// no sky-plane offset, so it forms no ephemeris pair.
fn build_eph_agreement_grid(results: &[ValidationResult]) -> String {
    type Acc = HashMap<(String, i64), (f64, u32)>;
    let mut sig_acc: Acc = HashMap::new();
    // external vs JPL (stored per-site separation, arcsec)
    let mut fo_acc: Acc = HashMap::new();
    let mut oorb_acc: Acc = HashMap::new();
    let mut grss_acc: Acc = HashMap::new();
    // empyrean vs external (norm of the signed-offset difference, arcsec)
    let mut emp_fo_acc: Acc = HashMap::new();
    let mut emp_oorb_acc: Acc = HashMap::new();
    let mut emp_grss_acc: Acc = HashMap::new();
    let add = |acc: &mut Acc, key: &(String, i64), v: f64| {
        let e = acc.entry(key.clone()).or_insert((0.0, 0));
        e.0 += v;
        e.1 += 1;
    };
    let cross =
        |dra: Option<f64>, ddec: Option<f64>, xra: Option<f64>, xdec: Option<f64>| -> Option<f64> {
            match (dra, ddec, xra, xdec) {
                (Some(a), Some(b), Some(c), Some(d)) => {
                    Some(((a - c).powi(2) + (b - d).powi(2)).sqrt())
                }
                _ => None,
            }
        };
    for r in results {
        if r.test_type != "ephemeris" {
            continue;
        }
        let key = (r.object.clone(), r.dt_days as i64);
        // empyrean's own agreement values come from the shared Section-1
        // selector; only the sigma toggle is accumulated here.
        if r.channel == "rust"
            && r.propagation_uncertainty.as_deref() == Some(EMP_SECTION1_ARM)
            && let (Some(dra), Some(ddec), Some(c)) =
                (r.d_ra_arcsec, r.d_dec_arcsec, r.emp_radec_cov_arcsec2)
            && let Some(d) = maha2(&[dra, ddec], &c)
        {
            add(&mut sig_acc, &key, d);
        }
        // Cross-tool pairs and external-vs-JPL both read off the core rows,
        // which carry empyrean's own offsets alongside the merged externals'.
        if r.channel == "core" {
            if let Some(v) = r.findorb_separation_arcsec {
                add(&mut fo_acc, &key, v);
            }
            if let Some(v) = r.oorb_separation_arcsec {
                add(&mut oorb_acc, &key, v);
            }
            if let Some(v) = r.grss_separation_arcsec {
                add(&mut grss_acc, &key, v);
            }
            if let Some(v) = cross(
                r.d_ra_arcsec,
                r.d_dec_arcsec,
                r.findorb_d_ra_arcsec,
                r.findorb_d_dec_arcsec,
            ) {
                add(&mut emp_fo_acc, &key, v);
            }
            if let Some(v) = cross(
                r.d_ra_arcsec,
                r.d_dec_arcsec,
                r.oorb_d_ra_arcsec,
                r.oorb_d_dec_arcsec,
            ) {
                add(&mut emp_oorb_acc, &key, v);
            }
            if let Some(v) = cross(
                r.d_ra_arcsec,
                r.d_dec_arcsec,
                r.grss_d_ra_arcsec,
                r.grss_d_dec_arcsec,
            ) {
                add(&mut emp_grss_acc, &key, v);
            }
        }
    }
    let to_mas = |acc: Acc| -> HashMap<(String, i64), f64> {
        acc.into_iter()
            .filter(|(_, (_, n))| *n > 0)
            .map(|(k, (s, n))| (k, (s / n as f64) * 1e3))
            .collect()
    };
    let mean = |acc: Acc| -> HashMap<(String, i64), f64> {
        acc.into_iter()
            .filter(|(_, (_, n))| *n > 0)
            .map(|(k, (s, n))| (k, s / n as f64))
            .collect()
    };
    let mut tools = vec![GridTool {
        label: "empyrean".into(),
        tool_slug: "empyrean".into(),
        ref_slug: "jpl".into(),
        ref_label: "JPL".into(),
        present: true,
        vals: emp_eph_agreement_vals(results),
        sigma: mean(sig_acc),
    }];
    // Externals carrying a sky-plane offset, from the SINGLE source shared with
    // the Section-1 timing panels (`axis_present_externals`) so heatmap pairs and
    // timing pairs cannot drift.
    let ext_present: Vec<(&str, &str)> = axis_present_externals(results, "ephemeris");
    let extra: Vec<PanelSpec> = vec![
        (
            "empyrean",
            "empyrean",
            "findorb",
            "find_orb",
            to_mas(emp_fo_acc),
        ),
        (
            "empyrean",
            "empyrean",
            "oorb",
            "OpenOrb",
            to_mas(emp_oorb_acc),
        ),
        ("empyrean", "empyrean", "grss", "GRSS", to_mas(emp_grss_acc)),
        ("find_orb", "findorb", "jpl", "JPL", to_mas(fo_acc)),
        ("OpenOrb", "oorb", "jpl", "JPL", to_mas(oorb_acc)),
        ("GRSS", "grss", "jpl", "JPL", to_mas(grss_acc)),
    ];
    for (label, tool_slug, ref_slug, ref_label, vals) in extra {
        if !vals.is_empty() {
            tools.push(GridTool {
                label: label.into(),
                tool_slug: tool_slug.into(),
                ref_slug: ref_slug.into(),
                ref_label: ref_label.into(),
                present: true,
                vals,
                sigma: HashMap::new(),
            });
        }
    }
    // Legal external-vs-external sky-plane pairs (both carry offsets, no stored
    // cross difference) render a hatched "not compared" panel.
    push_cross_hatched_panels(&mut tools, &ext_present);
    render_prop_like_grid("h2-eph", GridAxis::EphMas, results, tools)
}

/// Shared setup for the propagation-shaped grids.
fn render_prop_like_grid(
    id: &str,
    axis: GridAxis,
    results: &[ValidationResult],
    tools: Vec<GridTool>,
) -> String {
    let (objects, dts, model_gap, impactors) = prop_like_axes(results);
    let marks = GridMarks {
        model_gap: &model_gap,
        impactors: &impactors,
    };
    render_agreement_grid(id, axis, &objects, &dts, &tools, &marks)
}

/// SBDB radar-observation counts (delay, Doppler) JPL used in its own fit,
/// keyed by object and joined across every channel so a row of any channel sees
/// the counts the suite merged onto the core-channel OD row. An object appears
/// only when JPL used at least one radar observation; for such an object an
/// optical-only fit is not like for like with JPL's radar-informed solution, so
/// its optical fit is not compared with JPL. This one function, driven by the
/// counts and never by a name list, decides the exclusion everywhere it applies.
fn jpl_radar_obs_counts(results: &[ValidationResult]) -> BTreeMap<&str, (u32, u32)> {
    let mut counts: BTreeMap<&str, (u32, u32)> = BTreeMap::new();
    for r in results {
        let del = r.ref_od_n_del_obs_used.unwrap_or(0);
        let dop = r.ref_od_n_dop_obs_used.unwrap_or(0);
        if del > 0 || dop > 0 {
            counts.entry(r.object.as_str()).or_insert((del, dop));
        }
    }
    counts
}

/// The hover a JPL-comparison cell carries when JPL's fit used radar and ours is
/// optical only, so the reason for the hatch is one phrase in one place.
fn jpl_radar_hover(del: u32, dop: u32) -> String {
    format!(
        "JPL's fit used {del} delay and {dop} Doppler radar observations; an optical-only fit is not like for like"
    )
}

// ── H3: orbit-determination closeness grid ──────────────────────────
//
// Shape carries state, fill carries closeness. The closeness metric is
// `sigma_equiv_combined` from the orbit-comparison sidecar — the 6-DOF
// χ-equivalent of the combined Mahalanobis distance in Keplerian element
// space, symmetric in the joint (fit + reference) covariance. The convergence
// discipline of the old matrix is preserved: an unprovable blank resolves to
// "not attempted", never to a failure.

/// Fill bin for a sigma-equivalent distance (design §4.2): ≤1 → D2, 1–3 → D3,
/// 3–10 → D4, 10–100 → D5, >100 → D6 (with the runaway wedge). D1 and D7 are
/// unused here.
fn od_sigma_bin(s: f64) -> u8 {
    if s <= 1.0 {
        2
    } else if s <= 3.0 {
        3
    } else if s <= 10.0 {
        4
    } else if s <= 100.0 {
        5
    } else {
        6
    }
}

/// One OD state+closeness cell for a single tool: converged with a sigma fill,
/// converged with the reference covariance not SPD, converged with no
/// covariance / comparison record, did not converge, or never attempted. The
/// shape carries the convergence state; the fill carries the closeness to the
/// selected reference. `nocov_title` is the hover for the converged-but-no-fill
/// case, which differs by tool (empyrean's sidecar simply carries no record;
/// find_orb publishes no covariance at all).
fn od_state_cell(
    converged: Option<bool>,
    sigma: Option<f64>,
    has_record: bool,
    nocov_title: &str,
) -> String {
    match converged {
        Some(true) => {
            if let Some(s) = sigma {
                let bin = od_sigma_bin(s);
                let rw = if bin == 6 && s > 100.0 { " rw" } else { "" };
                format!(
                    "<td class=\"oc conv gb{bin}{rw}\" title=\"converged · σ_eq {s:.2}\"><span class=\"ocg\">{bin}</span><span class=\"cn\"> {s:.2}σ</span></td>"
                )
            } else if has_record {
                // A record exists but sigma is null: Σ_ref not SPD at the
                // common epoch, so the Cholesky factor does not exist.
                "<td class=\"oc conv undef\" title=\"converged · Σ_ref not SPD at the common epoch — metric undefined, no regularisation\"><span class=\"ocg\">?</span></td>".to_string()
            } else {
                format!(
                    "<td class=\"oc conv nocov\" title=\"{}\"><span class=\"ocg\">·</span></td>",
                    attr_escape(nocov_title)
                )
            }
        }
        Some(false) => {
            "<td class=\"oc fail\" title=\"ran, did not converge\"><span class=\"ocg\">✕</span></td>".to_string()
        }
        None => format!("<td class=\"oc hatch\" title=\"{HATCH_NOT_ATTEMPTED}\"></td>"),
    }
}

/// One arc's orbit-determination outcome for a single tool (optical or radar):
/// the convergence state, the closeness-to-reference sigma (when the tool
/// carries a covariance and a comparison record exists), and the fit
/// diagnostics.
struct OdArcCell {
    converged: Option<bool>,
    sigma: Option<f64>,
    has_record: bool,
    n_obs: Option<u32>,
    chi2: Option<f64>,
}

/// One OD panel: a single tool's fits against a single reference. Every panel
/// carries the identical row structure (all objects, the radar arcs as
/// sub-rows) so the Tool/Reference selector can swap panels row-for-row, exactly
/// as the propagation and ephemeris pair-panels do. Only the selected panel is
/// visible; the cross-fitter comparison lives in Part 2.
struct OdPanel<'a> {
    tool_slug: &'static str,
    tool_label: &'static str,
    tool_color: &'static str,
    ref_slug: &'static str,
    is_default: bool,
    /// Hover for a converged cell that carries no closeness fill.
    nocov_title: &'static str,
    /// Per-object optical arc.
    optical: HashMap<&'a str, OdArcCell>,
    /// Per-object radar arc (only the radar objects).
    radar: HashMap<&'a str, OdArcCell>,
    summary: String,
    /// Hover for the summary line (empty = none); carries the record-level
    /// detail behind an object-level count so the visible line stays terse.
    summary_title: String,
    fails: Vec<String>,
}

/// Render one OD panel's table: the selected tool's convergence + closeness,
/// n_obs, reduced χ², σ_eq and the Δr placeholder, over every object grouped by
/// population, with the radar arcs as sub-rows. The row order is fixed by
/// `block_order`, shared with every other panel.
fn od_panel_table(
    h: &mut String,
    panel: &OdPanel,
    block_order: &[(&str, Vec<&str>)],
    radar_objs: &BTreeSet<&str>,
    radar_counts: &BTreeMap<&str, (u32, u32)>,
) {
    // object / arc + one tool cell + four diagnostics.
    let n_cols = 1 + 1 + 4;
    let fmt_num = |v: Option<f64>| -> String {
        match v {
            Some(x) if x.is_finite() => format!("{x:.3}"),
            _ => "—".to_string(),
        }
    };

    h.push_str("<div class=\"grid-scroll\"><table class=\"agrid odgrid\"><thead><tr>");
    h.push_str("<th class=\"gobj\">object / arc</th>");
    h.push_str(&format!(
        "<th class=\"oc-h\"><span class=\"pop-dot\" style=\"background:{}\"></span>{}</th>",
        panel.tool_color, panel.tool_label
    ));
    h.push_str(&format!(
        "<th class=\"od-d\">{}</th><th class=\"od-d\">{}</th><th class=\"od-d\">{}</th><th class=\"od-d\">{}</th>",
        uhdr("n_obs"),
        uhdr("χ²ᵣ"),
        uhdr("σ_eq"),
        uhdr("Δr @epoch"),
    ));
    h.push_str("</tr></thead><tbody>");

    for (pop, objs) in block_order {
        // Population summary row.
        let attempted = objs.len();
        let converged = objs
            .iter()
            .copied()
            .filter(|o| panel.optical.get(o).and_then(|c| c.converged) == Some(true))
            .count();
        // The population median is over the NON-excluded objects only: a
        // radar-informed JPL object's optical-only σ_eq is not like for like and
        // is hatched in its own cell, so it must not enter the group statistic.
        let pop_sigmas: Vec<f64> = objs
            .iter()
            .copied()
            .filter(|o| !(panel.ref_slug == "jpl" && radar_counts.contains_key(o)))
            .filter_map(|o| panel.optical.get(o).and_then(|c| c.sigma))
            .collect();
        // The median states its own sample size: it can be smaller than the
        // converged count (radar-informed objects, fits with no record).
        let (med_val, n_med) = if pop_sigmas.is_empty() {
            ("—".to_string(), 0usize)
        } else {
            let mut s = pop_sigmas.clone();
            (format!("{:.2}", percentile(&mut s, 0.5)), pop_sigmas.len())
        };
        // Numbers only in view; "converged"/"median" and the sample size ride the
        // row hover (the column sub-header already names them).
        let fill_vis = if n_med > 0 {
            format!("{converged}/{attempted} · σ_eq {med_val} · n {n_med}")
        } else {
            format!("{converged}/{attempted} · σ_eq {med_val}")
        };
        h.push_str(&format!(
            "<tr class=\"gblock\"><td class=\"gblock-l\">{} ({attempted})</td><td class=\"gblock-fill\" colspan=\"{}\" title=\"converged {converged} of {attempted} · median σ_eq over {n_med} object(s)\">{fill_vis}</td></tr>",
            attr_escape(pop),
            n_cols - 1,
        ));

        for obj in objs {
            let oc = panel.optical.get(*obj);
            let conv = oc.and_then(|c| c.converged);
            let name_cls = if conv == Some(false) {
                "gobjname od-fail-name"
            } else {
                "gobjname"
            };
            h.push_str(&format!(
                "<tr class=\"gobjrow\"><td class=\"{name_cls}\" colspan=\"{n_cols}\">{}</td></tr>",
                attr_escape(obj)
            ));

            // Optical arc row.
            h.push_str("<tr class=\"gsub odrow\"><td class=\"gtool\">optical</td>");
            // JPL used radar for this object: an optical-only fit is not like for
            // like with JPL's radar-informed solution, so the JPL-comparison cells
            // (the σ_eq fill cell and every column drawn from the comparison record)
            // are the one hatch with the reason on hover; the own-fit numbers stay.
            let radar_excl = if panel.ref_slug == "jpl" {
                radar_counts.get(*obj).copied()
            } else {
                None
            };
            match oc {
                Some(c) if radar_excl.is_some() => {
                    let (del, dop) = radar_excl.unwrap();
                    let hover = attr_escape(&jpl_radar_hover(del, dop));
                    // The convergence mark stays on the hatch so the own fit's
                    // convergence is not lost; the fill and σ_eq carry no number.
                    let glyph = match c.converged {
                        Some(true) => "·",
                        Some(false) => "✕",
                        None => "",
                    };
                    h.push_str(&format!(
                        "<td class=\"oc hatch\" title=\"{hover}\"><span class=\"ocg\">{glyph}</span></td>"
                    ));
                    let nobs = c.n_obs.map(|n| n.to_string()).unwrap_or_else(|| "—".into());
                    let chi2 = fmt_num(c.chi2);
                    h.push_str(&format!(
                        "<td class=\"od-d\">{nobs}</td><td class=\"od-d\">{chi2}</td><td class=\"od-d hatch\" title=\"{hover}\"></td><td class=\"od-d hatch\" title=\"{hover}\"></td></tr>"
                    ));
                }
                Some(c) => {
                    h.push_str(&od_state_cell(
                        c.converged,
                        c.sigma,
                        c.has_record,
                        panel.nocov_title,
                    ));
                    let nobs = c.n_obs.map(|n| n.to_string()).unwrap_or_else(|| "—".into());
                    let chi2 = fmt_num(c.chi2);
                    let seq = match (c.sigma, c.has_record) {
                        (Some(s), _) => format!("{s:.2}"),
                        (None, true) => "n/d".to_string(),
                        (None, false) => "—".to_string(),
                    };
                    h.push_str(&format!(
                        "<td class=\"od-d\">{nobs}</td><td class=\"od-d\">{chi2}</td><td class=\"od-d\">{seq}</td><td class=\"od-d\" title=\"Cartesian |Δr| at the common epoch is not in the sidecar schema yet\">—</td></tr>"
                    ));
                }
                None => {
                    // This tool ran no optical fit for this object.
                    h.push_str(&od_state_cell(None, None, false, panel.nocov_title));
                    h.push_str("<td class=\"od-d\">—</td><td class=\"od-d\">—</td><td class=\"od-d\">—</td><td class=\"od-d\">—</td></tr>");
                }
            }

            // Radar sub-row — the radar objects only, uniform across panels.
            if radar_objs.contains(*obj) {
                h.push_str("<tr class=\"gsub odrow odradar\"><td class=\"gtool\">+ radar</td>");
                match panel.radar.get(*obj) {
                    Some(c) => {
                        h.push_str(&od_state_cell(c.converged, None, false, panel.nocov_title));
                        let rnobs = c.n_obs.map(|n| n.to_string()).unwrap_or_else(|| "—".into());
                        let rchi2 = fmt_num(c.chi2);
                        h.push_str(&format!(
                            "<td class=\"od-d\">{rnobs}</td><td class=\"od-d\">{rchi2}</td><td class=\"od-d hatch\" colspan=\"2\" title=\"the radar comparison record is not generated yet\"></td></tr>"
                        ));
                    }
                    None => {
                        h.push_str(&od_state_cell(None, None, false, panel.nocov_title));
                        h.push_str("<td class=\"od-d\">—</td><td class=\"od-d\">—</td><td class=\"od-d hatch\" colspan=\"2\" title=\"this tool ran no radar arc in this run\"></td></tr>");
                    }
                }
            }
        }
    }
    h.push_str("</tbody></table></div>");
}

/// Build the H3 orbit-determination panels: one tool's fits against one
/// reference per panel, following the Tool and Reference selects exactly as the
/// propagation (1.1A) and ephemeris (1.2A) pair-panels do. The default panel is
/// empyrean vs JPL; each other legal pair with comparison records is a hidden
/// panel the selector reveals, and a pair with no records shows the generic
/// hatched "not compared" line. The cross-fitter comparison lives in Part 2.
fn build_od_closeness_grid(
    results: &[ValidationResult],
    orbit_comparisons: &[crate::schema::OrbitComparison],
) -> String {
    // sigma_equiv per object, canonical sbdb direction (empyrean's fit at
    // JPL's own published epoch). `Some(None)` records a present-but-non-SPD
    // reference; absent keys have no comparison record at all.
    let mut sigma: HashMap<&str, Option<f64>> = HashMap::new();
    for c in orbit_comparisons {
        if c.common_epoch_source == "sbdb" {
            let v = if c.sigma_equiv_combined.is_finite() {
                Some(c.sigma_equiv_combined)
            } else {
                None
            };
            sigma.insert(c.object.as_str(), v);
        }
    }

    // empyrean OD rows (rust): optical + radar.
    let mut emp_opt: HashMap<&str, &ValidationResult> = HashMap::new();
    let mut emp_radar: HashMap<&str, &ValidationResult> = HashMap::new();
    for r in results {
        if r.channel == "rust" {
            if r.test_type == "orbit_determination" {
                emp_opt.insert(r.object.as_str(), r);
            } else if r.test_type == "orbit_determination_radar" {
                emp_radar.insert(r.object.as_str(), r);
            }
        }
    }
    // find_orb OD data (folded onto core rows).
    let mut fo: HashMap<&str, &ValidationResult> = HashMap::new();
    for r in results {
        if r.channel == "core"
            && r.test_type == "orbit_determination"
            && (r.findorb_rms_residual.is_some() || r.findorb_n_obs_used.is_some())
        {
            fo.insert(r.object.as_str(), r);
        }
    }

    // Object → population blocks. Order objects within a block by empyrean
    // sigma_equiv descending (offender first); non-finite last. This row order
    // is shared by every panel, so the panels the selector swaps line up
    // row-for-row.
    let mut blocks: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for r in results {
        if r.channel == "rust" && r.test_type == "orbit_determination" {
            blocks
                .entry(r.population.as_str())
                .or_default()
                .push(r.object.as_str());
        }
    }
    let sigma_key = |o: &str| -> f64 { sigma.get(o).and_then(|v| *v).unwrap_or(-1.0) };
    let mut block_order: Vec<(&str, Vec<&str>)> = blocks.into_iter().collect();
    block_order.sort_by(|a, b| {
        let am = a.1.iter().map(|o| sigma_key(o)).fold(f64::MIN, f64::max);
        let bm = b.1.iter().map(|o| sigma_key(o)).fold(f64::MIN, f64::max);
        bm.partial_cmp(&am).unwrap_or(std::cmp::Ordering::Equal)
    });
    for (_, objs) in block_order.iter_mut() {
        objs.sort_by(|a, b| {
            sigma_key(b)
                .partial_cmp(&sigma_key(a))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    let radar_objs: BTreeSet<&str> = emp_radar.keys().copied().collect();
    // Objects whose JPL solution used radar: their optical fit is not compared
    // with JPL. One set, computed once, used by the panels and the summary line.
    let radar_counts = jpl_radar_obs_counts(results);

    // ── Panels: one legal (tool, reference) pair each ──
    let mut panels: Vec<OdPanel> = Vec::new();

    // empyrean vs JPL — the default (empyrean is the engine under test).
    {
        let mut optical: HashMap<&str, OdArcCell> = HashMap::new();
        for (&obj, r) in &emp_opt {
            optical.insert(
                obj,
                OdArcCell {
                    converged: r.od_converged,
                    sigma: sigma.get(obj).and_then(|v| *v),
                    has_record: sigma.contains_key(obj),
                    n_obs: r.n_obs_used,
                    chi2: r.od_reduced_chi2,
                },
            );
        }
        let mut radar: HashMap<&str, OdArcCell> = HashMap::new();
        for (&obj, r) in &emp_radar {
            radar.insert(
                obj,
                OdArcCell {
                    converged: r.od_converged,
                    sigma: None,
                    has_record: false,
                    n_obs: r.n_obs_used,
                    chi2: r.od_reduced_chi2,
                },
            );
        }

        // Summary (preserves the counts the method caveat cites: every
        // comparison record, both common-epoch directions).
        let n_arcs = emp_opt.len() + emp_radar.len();
        let n_conv = results
            .iter()
            .filter(|r| {
                r.channel == "rust"
                    && (r.test_type == "orbit_determination"
                        || r.test_type == "orbit_determination_radar")
                    && r.od_converged == Some(true)
            })
            .count();
        // Radar-informed JPL objects are excluded from the comparison statistics
        // (an optical-only fit is not like for like); the excluded count is stated.
        let is_radar =
            |c: &&crate::schema::OrbitComparison| radar_counts.contains_key(c.object.as_str());
        // The exclusion is stated in OBJECTS, drawn from the one exclusion set
        // (`radar_counts`, the JPL-radar objects) so §1.3A and §4.1 report the
        // same count; the per-record tally rides in the summary hover.
        let n_radar_excl_recs = orbit_comparisons.iter().filter(is_radar).count();
        let n_radar_excl_objs = radar_counts.len();
        let n_compared = orbit_comparisons.iter().filter(|c| !is_radar(c)).count();
        let finite: Vec<f64> = orbit_comparisons
            .iter()
            .filter(|c| !is_radar(c))
            .filter(|c| c.sigma_equiv_combined.is_finite())
            .map(|c| c.sigma_equiv_combined)
            .collect();
        let mut finite_sorted = finite.clone();
        let med_sig = if finite_sorted.is_empty() {
            f64::NAN
        } else {
            percentile(&mut finite_sorted, 0.5)
        };
        let within3 = finite.iter().filter(|&&v| v <= 3.0).count();
        // Numbers-only idiom in view (the agreement grids' form); the words that
        // name each number, and the radar-exclusion facts, ride the hover.
        let summary = format!(
            "{n_conv}/{n_arcs} converged · σ_eq {:.2} · n {}/{} · ≤3σ {} · {n_radar_excl_objs} radar excluded",
            med_sig,
            finite.len(),
            n_compared,
            within3,
        );
        let summary_title = format!(
            "Arcs converged of attempted · median σ_eq over finite of compared records · within-3σ count. JPL delay/Doppler radar on {n_radar_excl_objs} objects: optical-only is not like-for-like, kept in the grid marked, out of these stats; {n_radar_excl_recs} records, both epoch sources."
        );

        // Failures list (empyrean rows that did not converge).
        let mut fails: Vec<String> = Vec::new();
        for r in results {
            if r.channel == "rust"
                && (r.test_type == "orbit_determination"
                    || r.test_type == "orbit_determination_radar")
                && r.od_converged == Some(false)
            {
                let short: String = r.notes.trim().chars().take(60).collect();
                fails.push(format!(
                    "<tr><td class=\"od-fail-name\">{}</td><td>empyrean</td><td title=\"{}\">{}</td></tr>",
                    attr_escape(&r.object),
                    attr_escape(&r.notes),
                    attr_escape(&short)
                ));
            }
        }

        panels.push(OdPanel {
            tool_slug: "empyrean",
            tool_label: "empyrean",
            tool_color: "#5b9bd5",
            ref_slug: "jpl",
            is_default: true,
            nocov_title: "converged · no comparison record in this run",
            optical,
            radar,
            summary,
            summary_title,
            fails,
        });
    }

    // find_orb vs JPL — where find_orb fitted. find_orb publishes no covariance,
    // so the closeness fill is absent; the panel carries convergence and n_obs.
    if od_tool_present("findorb", results) {
        let mut optical: HashMap<&str, OdArcCell> = HashMap::new();
        for (&obj, r) in &fo {
            let converged = if r.findorb_rms_residual.is_some() {
                Some(true)
            } else {
                None
            };
            optical.insert(
                obj,
                OdArcCell {
                    converged,
                    sigma: None,
                    has_record: false,
                    n_obs: r.findorb_n_obs_used,
                    chi2: None,
                },
            );
        }
        let fo_att = fo.len();
        let fo_conv = fo
            .values()
            .filter(|r| r.findorb_rms_residual.is_some())
            .count();
        let summary = format!(
            "{fo_conv}/{fo_att} arcs fit · find_orb publishes no covariance, so no closeness metric in this run"
        );
        panels.push(OdPanel {
            tool_slug: "findorb",
            tool_label: "find_orb",
            tool_color: "#d05040",
            ref_slug: "jpl",
            is_default: false,
            nocov_title: "find_orb converged · publishes no covariance in this run",
            optical,
            radar: HashMap::new(),
            summary,
            summary_title: String::new(),
            fails: Vec::new(),
        });
    }

    // ── Render ──
    let mut h = String::new();
    // Colour + state key (shared by every panel), once, outside the panels.
    // One ladder key: only the two end labels are shown, each swatch's band and
    // every state mark's meaning ride the hover title (text-diet rule 2c).
    h.push_str("<div class=\"grid-key\">");
    h.push_str("<span class=\"gk\"><span class=\"gk-sw gb2\" title=\"σ_eq ≤ 1 — agreement\"></span>≤1</span>");
    h.push_str("<span class=\"gk\"><span class=\"gk-sw gb3\" title=\"σ_eq 1–3\"></span></span>");
    h.push_str("<span class=\"gk\"><span class=\"gk-sw gb4\" title=\"σ_eq 3–10\"></span></span>");
    h.push_str("<span class=\"gk\"><span class=\"gk-sw gb5\" title=\"σ_eq 10–100\"></span></span>");
    h.push_str("<span class=\"gk\"><span class=\"gk-sw gb6 rw\" title=\"σ_eq &gt;100 — runaway\"></span>&gt;100</span>");
    h.push_str("<span class=\"gk\" title=\"reference covariance not positive-definite\">?</span>");
    h.push_str("<span class=\"gk\" title=\"converged, no comparison record\">·</span>");
    h.push_str("<span class=\"gk\" title=\"ran, did not converge\">✕</span>");
    h.push_str(&format!("<span class=\"gk\"><span class=\"gk-sw hatch\" title=\"{HATCH_NOT_ATTEMPTED}\"></span>{HATCH_NOT_ATTEMPTED}</span>"));
    h.push_str("</div>");

    h.push_str("<div class=\"pair-panels\" data-section=\"h3-od\">");
    for panel in &panels {
        let default_attr = if panel.is_default {
            " data-default=\"1\""
        } else {
            ""
        };
        let hidden_attr = if panel.is_default { "" } else { " hidden" };
        h.push_str(&format!(
            "<div class=\"pair-panel\" data-tool=\"{}\" data-ref=\"{}\"{default_attr}{hidden_attr}>",
            panel.tool_slug, panel.ref_slug
        ));
        let summary_title_attr = if panel.summary_title.is_empty() {
            String::new()
        } else {
            format!(" title=\"{}\"", attr_escape(&panel.summary_title))
        };
        h.push_str(&format!(
            "<div class=\"grid-summary\"{summary_title_attr}>{}</div>",
            panel.summary
        ));
        od_panel_table(&mut h, panel, &block_order, &radar_objs, &radar_counts);
        if !panel.fails.is_empty() {
            h.push_str(&format!(
                "<div class=\"od-fails\"><div class=\"od-fails-h\" title=\"did not converge on this run\">Did not converge ({})</div><table class=\"od-fails-t\"><tbody>{}</tbody></table></div>",
                panel.fails.len(),
                panel.fails.join("")
            ));
        }
        h.push_str("</div>");
    }
    // A legal pair with no OD comparison record (a non-JPL reference, or a tool
    // that runs no fit) collapses to one hatched line, never an empty panel.
    h.push_str("<div class=\"pair-notcompared generic\" hidden>Not compared &mdash; this tool and reference share no orbit-determination comparison in this run. Choose a fitted tool paired with the reference.</div>");
    h.push_str("</div>");
    h
}

// ── H4: channel fidelity, four states ───────────────────────────────
//
// Every distribution channel (rust, python, cli, c) against empyrean-core,
// per test type. Four states, not three, because a 366 mm reassociation and a
// 216.8 km defect are different findings: EXACT (bitwise), ULP (float
// reassociation, ≤ the fidelity floor), TOL (a real numeric difference below
// physical significance), DIFF (a defect — the offending objects are named).
// The cell takes the WORST state in its population, never the majority.

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum FidState {
    NotRun,
    Exact,
    Ulp,
    Tol,
    Diff,
}

impl FidState {
    fn class(self) -> &'static str {
        match self {
            FidState::NotRun => "hatch",
            FidState::Exact => "st-exact",
            FidState::Ulp => "st-ulp",
            FidState::Tol => "st-tol",
            FidState::Diff => "st-diff",
        }
    }
    fn label(self) -> &'static str {
        match self {
            FidState::NotRun => "not run",
            FidState::Exact => "EXACT",
            FidState::Ulp => "ULP",
            FidState::Tol => "TOL",
            FidState::Diff => "DIFF",
        }
    }
    /// The one-glyph mark the per-object grid prints in a cell, from the shared
    /// `FID_MARKS` table. `NotRun` has no glyph (it is the hatched state).
    fn glyph(self) -> &'static str {
        FID_MARKS
            .iter()
            .find(|m| m.0 == self)
            .map(|m| m.1)
            .unwrap_or("")
    }
    /// The short meaning carried on a per-object cell's and the key's hover,
    /// from the shared `FID_MARKS` table.
    fn meaning(self) -> &'static str {
        FID_MARKS
            .iter()
            .find(|m| m.0 == self)
            .map(|m| m.3)
            .unwrap_or("")
    }
}

/// The four fidelity states as (state, glyph, word, meaning): the ONE definition
/// the per-object grid's cell marks and the grid key both read, so a cell glyph
/// and the key can never drift apart. `NotRun` is not here — it is the page's one
/// hatched state, keyed separately. A glyph (not colour alone) carries each state
/// in the cell; the word and meaning ride the cell and key hovers.
const FID_MARKS: [(FidState, &str, &str, &str); 4] = [
    (
        FidState::Exact,
        "=",
        "EXACT",
        "bitwise identical to empyrean-core",
    ),
    (FidState::Ulp, "≈", "ULP", "within ULP — ≤1e-10 km / 1 µas"),
    (FidState::Tol, "~", "TOL", "real difference, sub-physical"),
    (
        FidState::Diff,
        "≠",
        "DIFF",
        "defect — offending objects named",
    ),
];

/// Classify a channel-vs-core difference given the per-axis ULP (fidelity)
/// floor and physical-significance floor.
///
/// Note on the propagation physical floor. §7.1 of the design tables it as
/// 1 mm, but §7 (and the §7.4 sketch showing rust propagation as ULP/TOL with
/// a 366 mm max, distinct from the 216.8 km python defect) makes clear the
/// four states exist precisely so a sub-metre float-reassociation difference
/// and a kilometre-scale defect do NOT render alike. A 1 mm floor would put
/// both in DIFF and defeat that. So the propagation physical-significance
/// floor is set to 1 km here: a sub-km difference between two builds of the
/// same engine over a multi-year chaotic integration is reassociation (TOL);
/// a kilometre-plus difference is a defect (DIFF). Flagged to the lead as a
/// design-text inconsistency resolved toward the stated intent.
fn fid_classify(diff: f64, ulp: f64, phys: f64) -> FidState {
    if diff == 0.0 {
        FidState::Exact
    } else if diff <= ulp {
        FidState::Ulp
    } else if diff <= phys {
        FidState::Tol
    } else {
        FidState::Diff
    }
}

/// Wrap an angle difference (arcsec) into (-180°, 180°] so a 360° RA wrap does
/// not read as a defect (bead empyrean-fy0po: compare angles modulo a turn).
fn wrap_arcsec(mut d: f64) -> f64 {
    const TURN: f64 = 360.0 * 3600.0;
    while d > TURN / 2.0 {
        d -= TURN;
    }
    while d <= -TURN / 2.0 {
        d += TURN;
    }
    d.abs()
}

/// Aggregate for one (channel, test-type) fidelity cell: worst state, the
/// objects that reached DIFF, compared count, bit-identical count, max diff.
type FidAgg = (FidState, BTreeSet<String>, usize, usize, f64);

/// Build the H4 channel-fidelity grid: the four-state channel × test-type
/// summary and the complete per-object grid beneath it.
fn build_channel_fidelity_grid(results: &[ValidationResult]) -> String {
    // Distribution channels the report validates against core.
    let channels = ["rust", "python", "cli", "c"];
    // (row test_type, header, is_angular, ulp floor, physical floor).
    let test_types = [
        ("propagation", "propagation", false, 1e-10, 1.0),
        ("ephemeris", "ephemeris", true, 1e-6, 1e-3),
        ("orbit_determination", "orbit det.", false, 1e-10, 1.0),
    ];

    // Index core rows by the same key rollup_channels uses.
    type Key = (String, i64, String, String, Option<String>, Option<String>);
    let key_of = |r: &ValidationResult| -> Key {
        (
            r.object.clone(),
            r.dt_days as i64,
            r.force_model.clone(),
            r.test_type.clone(),
            r.observer.clone(),
            r.propagation_uncertainty.clone(),
        )
    };
    let mut core_by_key: HashMap<Key, &ValidationResult> = HashMap::new();
    for r in results {
        if r.channel == "core" && !is_timing_only_uncertainty(r.propagation_uncertainty.as_deref())
        {
            core_by_key.insert(key_of(r), r);
        }
    }

    // Per (channel, test_type): worst state, the objects that hit DIFF, the
    // compared and bit-identical counts, and the max difference.
    // Per (channel, test_type, object): worst state (for the per-object grid).
    let mut cell: HashMap<(&str, &str), FidAgg> = HashMap::new();
    let mut obj_cell: HashMap<(&str, &str, String), FidState> = HashMap::new();
    let mut present: BTreeSet<&str> = BTreeSet::new();
    for r in results {
        if r.channel == "core" || is_timing_only_uncertainty(r.propagation_uncertainty.as_deref()) {
            continue;
        }
        let ch = match channels.iter().find(|&&c| c == r.channel) {
            Some(&c) => c,
            None => continue,
        };
        present.insert(ch);
        let (tt_key, _, is_ang, ulp, phys) =
            match test_types.iter().find(|(k, _, _, _, _)| *k == r.test_type) {
                Some(t) => *t,
                None => continue,
            };
        let Some(core) = core_by_key.get(&key_of(r)) else {
            continue;
        };
        // Channel-vs-core difference for this test type.
        let diff = if is_ang {
            match (
                r.d_ra_arcsec,
                core.d_ra_arcsec,
                r.d_dec_arcsec,
                core.d_dec_arcsec,
            ) {
                (Some(ra), Some(cra), Some(dec), Some(cdec)) => {
                    wrap_arcsec(ra - cra).hypot(dec - cdec)
                }
                _ => continue,
            }
        } else {
            match vec_dr_km(&r.emp_pos_au, &core.emp_pos_au) {
                Some(d) => d,
                None => continue,
            }
        };
        let st = fid_classify(diff, ulp, phys);
        let e = cell
            .entry((ch, tt_key))
            .or_insert((FidState::Exact, BTreeSet::new(), 0, 0, 0.0));
        e.0 = e.0.max(st);
        e.2 += 1; // compared
        if st <= FidState::Ulp {
            e.3 += 1; // bit-identical-ish
        }
        if diff > e.4 {
            e.4 = diff;
        }
        if st == FidState::Diff {
            e.1.insert(r.object.clone());
        }
        let oc = obj_cell
            .entry((ch, tt_key, r.object.clone()))
            .or_insert(FidState::Exact);
        *oc = (*oc).max(st);
    }

    let is_angular_tt = |tt: &str| tt == "ephemeris";
    let fmt_amt = |tt: &str, v: f64| -> String {
        if is_angular_tt(tt) {
            fmt_arcsec(v)
        } else {
            fmt_error(Some(v))
        }
    };

    // ── Summary: channels bit-identical on at least one axis ──────────
    let n_present = channels.iter().filter(|c| present.contains(*c)).count();
    let n_clean = channels
        .iter()
        .filter(|&&c| {
            present.contains(c)
                && test_types
                    .iter()
                    .any(|(k, _, _, _, _)| matches!(cell.get(&(c, *k)).map(|e| e.0), Some(s) if s <= FidState::Ulp))
        })
        .count();
    let mut h = String::new();
    h.push_str(&format!(
        "<div class=\"grid-summary\" title=\"Each counted channel reproduces empyrean-core to ≤ ULP on at least one axis. Every channel runs against empyrean-core, the validation reference; rust also appears in Parts 1–2 against JPL.\">{n_clean} of {n_present} channels ≤ ULP on ≥1 axis</div>"
    ));
    // State key: glyph + word for the four states (the mark the per-object cells
    // print), meaning on the swatch hover; the hatched "not run" stays. Built from
    // the one FID_MARKS table so a cell glyph and this key cannot drift.
    h.push_str("<div class=\"grid-key\">");
    for (st, glyph, word, meaning) in FID_MARKS {
        h.push_str(&format!(
            "<span class=\"gk\"><span class=\"gk-sw {}\" title=\"{meaning}\"></span>{glyph}{word}</span>",
            st.class()
        ));
    }
    h.push_str(&format!("<span class=\"gk\"><span class=\"gk-sw hatch\" title=\"{HATCH_CHANNEL_ABSENT}\"></span>not run</span>"));
    h.push_str("</div>");

    // ── Channel × test-type four-state grid ───────────────────────────
    h.push_str("<div class=\"grid-scroll\"><table class=\"agrid fidgrid\"><thead><tr><th class=\"gobj\">channel</th>");
    for (_, disp, _, _, _) in &test_types {
        h.push_str(&format!("<th class=\"fid-h\">{disp}</th>"));
    }
    h.push_str("</tr></thead><tbody>");
    for &ch in &channels {
        h.push_str(&format!("<tr class=\"gsub\"><td class=\"gtool\">{ch}</td>"));
        if !present.contains(ch) {
            for _ in &test_types {
                h.push_str(&format!(
                    "<td class=\"fc hatch\" title=\"{HATCH_CHANNEL_ABSENT}\"></td>"
                ));
            }
            h.push_str("</tr>");
            continue;
        }
        for (k, _, _, _, _) in &test_types {
            match cell.get(&(ch, *k)) {
                None => h.push_str("<td class=\"fc hatch\" title=\"no compared rows\">—</td>"),
                Some((st, objs, ncmp, nbit, maxv)) => {
                    let named = if *st == FidState::Diff && !objs.is_empty() {
                        let list: Vec<&str> = objs.iter().map(|s| s.as_str()).take(6).collect();
                        format!(
                            "<br/><span class=\"fc-obj\">{}</span>",
                            attr_escape(&list.join(" "))
                        )
                    } else {
                        String::new()
                    };
                    h.push_str(&format!(
                        "<td class=\"fc {}\" title=\"{} of {} rows ≤ ULP · max {}\">{}{}<br/><span class=\"fc-sub\">{}/{} · max {}</span></td>",
                        st.class(),
                        nbit, ncmp,
                        attr_escape(&fmt_amt(k, *maxv)),
                        st.label(),
                        named,
                        nbit, ncmp,
                        attr_escape(&fmt_amt(k, *maxv)),
                    ));
                }
            }
        }
        h.push_str("</tr>");
    }
    h.push_str("</tbody></table></div>");

    // ── Complete per-object grid: 50 objects × (channel × test type) ──
    let mut objects: Vec<(&str, &str)> = Vec::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for r in results {
        if r.channel == "rust" && r.test_type == "propagation" && seen.insert(r.object.as_str()) {
            objects.push((r.object.as_str(), r.population.as_str()));
        }
    }
    objects.sort_by(|a, b| a.1.cmp(b.1).then(a.0.cmp(b.0)));
    let present_ch: Vec<&str> = channels
        .iter()
        .copied()
        .filter(|c| present.contains(c))
        .collect();

    h.push_str(
        "<div class=\"panel-title\" title=\"Complete per-object grid — every channel × test type\">Complete per-object grid</div>",
    );
    h.push_str("<div class=\"grid-scroll\"><table class=\"agrid fidgrid\"><thead><tr><th class=\"gobj\">object</th>");
    for &ch in &present_ch {
        for (_, disp, _, _, _) in &test_types {
            h.push_str(&format!("<th class=\"fo-h\">{ch}<br/>{disp}</th>"));
        }
    }
    h.push_str("</tr></thead><tbody>");
    // Group by population.
    let mut blocks: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for &(o, p) in &objects {
        blocks.entry(p).or_default().push(o);
    }
    let ncols = present_ch.len() * test_types.len() + 1;
    for (pop, objs) in &blocks {
        h.push_str(&format!(
            "<tr class=\"gblock\"><td class=\"gblock-l\">{} ({})</td><td class=\"gblock-fill\" colspan=\"{}\"></td></tr>",
            attr_escape(pop),
            objs.len(),
            ncols - 1
        ));
        for o in objs {
            h.push_str(&format!(
                "<tr class=\"gsub\"><td class=\"gtool\">{}</td>",
                attr_escape(o)
            ));
            for &ch in &present_ch {
                for (k, _, _, _, _) in &test_types {
                    let st = obj_cell
                        .get(&(ch, *k, (*o).to_string()))
                        .copied()
                        .unwrap_or(FidState::NotRun);
                    // One glyph carries the state in the cell (colour is never the
                    // only carrier); the state word and its meaning ride the hover.
                    // NotRun is the page's one hatched state: empty cell, reason on
                    // hover.
                    if st == FidState::NotRun {
                        h.push_str(&format!(
                            "<td class=\"fo hatch\" title=\"{} {} · not run — {HATCH_CHANNEL_ABSENT}\"></td>",
                            ch, k
                        ));
                    } else {
                        h.push_str(&format!(
                            "<td class=\"fo {}\" title=\"{} {} · {} — {}\">{}</td>",
                            st.class(),
                            ch,
                            k,
                            st.label(),
                            st.meaning(),
                            st.glyph(),
                        ));
                    }
                }
            }
            h.push_str("</tr>");
        }
    }
    h.push_str("</tbody></table></div>");
    h
}

// ── Part 2: all tools, side by side ─────────────────────────────────
//
// Every matrix in this part states each tool's OWN measured numbers, coloured
// on the report's one decade ladder (design §3.1). Nothing is combined into a
// ranking and no cell compares two tools: the reader reads each tool's numbers
// within a row. R1 is tool × axis, R2 is tool × population, R3 is tool × object.

/// A per-(object, dt) scalar map — one tool's values on one axis.
type ByObjDt = HashMap<(String, i64), f64>;

/// Per-(object, dt) ephemeris sky separation vs JPL Horizons, in mas, averaged
/// over the observing sites, for empyrean (rust, first-order arm) and find_orb.
/// The H2 grid builder averages the same site separations; this returns just
/// the slice the tool-by-axis matrix needs.
fn eph_vs_jpl_mas(results: &[ValidationResult]) -> (ByObjDt, ByObjDt) {
    let mut emp: HashMap<(String, i64), (f64, u32)> = HashMap::new();
    let mut fo: HashMap<(String, i64), (f64, u32)> = HashMap::new();
    for r in results {
        if r.test_type != "ephemeris" {
            continue;
        }
        let key = (r.object.clone(), r.dt_days as i64);
        if r.channel == "rust"
            && r.propagation_uncertainty.as_deref() == Some("first_order_detection_on")
            && let Some(v) = r.separation_arcsec
        {
            let e = emp.entry(key.clone()).or_insert((0.0, 0));
            e.0 += v;
            e.1 += 1;
        }
        if r.channel == "core"
            && let Some(v) = r.findorb_separation_arcsec
        {
            let e = fo.entry(key).or_insert((0.0, 0));
            e.0 += v;
            e.1 += 1;
        }
    }
    let fin = |m: HashMap<(String, i64), (f64, u32)>| -> ByObjDt {
        m.into_iter()
            .filter(|(_, (_, n))| *n > 0)
            .map(|(k, (s, n))| (k, (s / n as f64) * 1e3))
            .collect()
    };
    (fin(emp), fin(fo))
}

/// A small ladder chip carrying one number on its decade colour (Part 2). Same
/// bin edges and formatters as the grid cells; used where a cell must show two
/// numbers (p50 and p90) side by side.
fn ladder_chip(axis: GridAxis, v: f64) -> String {
    let bin = axis.bin(v);
    let rw = if bin == 7 { " rw" } else { "" };
    format!(
        "<span class=\"lchip gb{bin}{rw}\" title=\"{}\">{}</span>",
        attr_escape(&grid_fmt(axis, v)),
        grid_compact(axis, v)
    )
}

/// One Part-2 ladder cell (R2/R3): the tool's own value on the decade ladder,
/// hatched "not compared" when the tool has no rows, daggered (`mg`) when it is
/// an ASSIST model-gap cell, with an optional object-count line. Mirrors the
/// inner shape of `grid_cell` (the ladder maths live in `GridAxis::bin`); it
/// adds the count line and omits the sigma-toggle attributes Part 2 never uses.
fn part2_cell(axis: GridAxis, v: Option<f64>, is_gap: bool, n: Option<usize>) -> String {
    match v {
        None => format!("<td class=\"gc hatch\" title=\"{HATCH_NOT_COMPARED}\"></td>"),
        Some(v) => {
            let bin = axis.bin(v);
            let rw = if bin == 7 { " rw" } else { "" };
            let gap = if is_gap { " mg" } else { "" };
            let nline = match n {
                Some(k) => format!("<br><span class=\"cn-n\">n {k}</span>"),
                None => String::new(),
            };
            format!(
                "<td class=\"gc gb{bin}{rw}{gap}\" title=\"{}\"><span class=\"cn\">{}{nline}</span></td>",
                attr_escape(&grid_fmt(axis, v)),
                grid_compact(axis, v)
            )
        }
    }
}

/// Build Part 2: three matrices that state every tool's own numbers, coloured
/// on the report's one decade ladder — R1 by axis, R2 by population, R3 by
/// object. No cell ranks the tools or names a winner; the reader compares.
fn build_tool_ranking_html(
    results: &[ValidationResult],
    orbit_comparisons: &[crate::schema::OrbitComparison],
) -> String {
    // Objects whose JPL solution used radar: their optical fit is not compared
    // with JPL, so they are hatched in the by-object view and left out of the
    // orbit-determination statistic. One set, shared with §1.3A and Part 4.
    let radar_counts = jpl_radar_obs_counts(results);

    // Propagation offset vs JPL Horizons per tool, keyed by (object, dt).
    let mut emp: ByObjDt = HashMap::new();
    let mut assist: ByObjDt = HashMap::new();
    let mut findorb: ByObjDt = HashMap::new();
    for r in results {
        let key = (r.object.clone(), r.dt_days as i64);
        if r.channel == "rust"
            && r.test_type == "propagation"
            && r.propagation_uncertainty.as_deref() == Some("first_order_detection_on")
            && let Some(v) = r.emp_vs_horizons_km
        {
            emp.insert(key.clone(), v);
        }
        if r.channel == "core" && r.test_type == "propagation" {
            if let Some(v) = r.assist_vs_horizons_km {
                assist.entry(key.clone()).or_insert(v);
            }
            if let Some(v) = r.findorb_vs_horizons_km {
                findorb.entry(key).or_insert(v);
            }
        }
    }
    // Ephemeris sky separation vs JPL Horizons per tool (mas).
    let (emp_eph, fo_eph) = eph_vs_jpl_mas(results);

    // Model-gap objects (Marsden ΔT comets + self-perturbers): ASSIST lacks a
    // force term they need, so the dagger marks the ASSIST cell for these.
    let mut model_gap: BTreeSet<&str> = BTreeSet::new();
    for r in results {
        if r.ic_non_grav_dt.is_some() || r.population == "Self-Perturber" {
            model_gap.insert(r.object.as_str());
        }
    }

    // Object → population.
    let mut obj2pop: HashMap<&str, &str> = HashMap::new();
    for r in results {
        if r.channel == "rust" && r.test_type == "propagation" {
            obj2pop.insert(r.object.as_str(), r.population.as_str());
        }
    }

    // Per-map percentile / count helpers (each tool over its own rows).
    let pct = |m: &ByObjDt, p: f64| -> Option<f64> {
        let mut v: Vec<f64> = m.values().copied().filter(|x| x.is_finite()).collect();
        if v.is_empty() {
            None
        } else {
            Some(percentile(&mut v, p))
        }
    };
    let pct_excl = |m: &ByObjDt, p: f64| -> Option<f64> {
        let mut v: Vec<f64> = m
            .iter()
            .filter(|((o, _), _)| !model_gap.contains(o.as_str()))
            .map(|(_, &x)| x)
            .filter(|x| x.is_finite())
            .collect();
        if v.is_empty() {
            None
        } else {
            Some(percentile(&mut v, p))
        }
    };
    let n_rows = |m: &ByObjDt| -> usize { m.values().filter(|x| x.is_finite()).count() };
    let obj_worst = |obj: &str| -> f64 {
        emp.iter()
            .filter(|((o, _), _)| o.as_str() == obj)
            .map(|(_, &v)| v)
            .filter(|v| v.is_finite())
            .fold(f64::NEG_INFINITY, f64::max)
    };
    let obj_median = |m: &ByObjDt, obj: &str| -> Option<f64> {
        let mut v: Vec<f64> = m
            .iter()
            .filter(|((o, _), _)| o.as_str() == obj)
            .map(|(_, &x)| x)
            .filter(|x| x.is_finite())
            .collect();
        if v.is_empty() {
            None
        } else {
            Some(percentile(&mut v, 0.5))
        }
    };
    let scoped = |m: &ByObjDt, pop: &str| -> Vec<f64> {
        m.iter()
            .filter(|((o, _), _)| obj2pop.get(o.as_str()).copied() == Some(pop))
            .map(|(_, &v)| v)
            .filter(|v| v.is_finite())
            .collect()
    };
    let scoped_nobj = |m: &ByObjDt, pop: &str| -> usize {
        m.iter()
            .filter(|((o, _), v)| obj2pop.get(o.as_str()).copied() == Some(pop) && v.is_finite())
            .map(|((o, _), _)| o.clone())
            .collect::<BTreeSet<String>>()
            .len()
    };
    let median_of = |xs: Vec<f64>| -> Option<f64> {
        let mut v = xs;
        if v.is_empty() {
            None
        } else {
            Some(percentile(&mut v, 0.5))
        }
    };

    // OD convergence + closeness.
    let od_conv = results
        .iter()
        .filter(|r| {
            r.channel == "rust"
                && (r.test_type == "orbit_determination"
                    || r.test_type == "orbit_determination_radar")
                && r.od_converged == Some(true)
        })
        .count();
    let od_att = results
        .iter()
        .filter(|r| {
            r.channel == "rust"
                && (r.test_type == "orbit_determination"
                    || r.test_type == "orbit_determination_radar")
        })
        .count();
    let mut sig: Vec<f64> = orbit_comparisons
        .iter()
        .filter(|c| c.common_epoch_source == "sbdb" && c.sigma_equiv_combined.is_finite())
        .filter(|c| !radar_counts.contains_key(c.object.as_str()))
        .map(|c| c.sigma_equiv_combined)
        .collect();
    let med_sig = if sig.is_empty() {
        f64::NAN
    } else {
        percentile(&mut sig, 0.5)
    };
    let med_sig_s = if med_sig.is_finite() {
        format!("{med_sig:.2}")
    } else {
        "—".to_string()
    };
    let fo_conv = results
        .iter()
        .filter(|r| {
            r.channel == "core"
                && r.test_type == "orbit_determination"
                && r.findorb_rms_residual.is_some()
        })
        .count();

    // Timing p50 + sample count per tool at its own boundary.
    let p50_n = |f: &dyn Fn(&ValidationResult) -> Option<f64>| -> (Option<f64>, usize) {
        let mut v: Vec<f64> = results
            .iter()
            .filter_map(f)
            .filter(|x| x.is_finite())
            .collect();
        let n = v.len();
        if v.is_empty() {
            (None, 0)
        } else {
            (Some(percentile(&mut v, 0.5)), n)
        }
    };
    let (emp_ms, emp_ms_n) = p50_n(&|r| {
        if r.channel == "rust"
            && r.test_type == "propagation"
            && r.propagation_uncertainty.as_deref() == Some("first_order_detection_on")
        {
            r.emp_time_ms
        } else {
            None
        }
    });
    let (assist_ms, assist_ms_n) = p50_n(&|r| r.assist_time_ms);
    let (fo_ms, fo_ms_n) = p50_n(&|r| r.findorb_time_ms);
    let (kete_ms, kete_ms_n) = p50_n(&|r| r.kete_time_ms);
    let (jorbit_ms, jorbit_ms_n) = p50_n(&|r| r.jorbit_time_ms);

    let km = GridAxis::PropKm;
    let mas = GridAxis::EphMas;
    let tms = GridAxis::TimingMs;
    let chip_pair = |axis: GridAxis, p50: Option<f64>, p90: Option<f64>, n: usize| -> String {
        match (p50, p90) {
            (Some(a), Some(b)) => format!(
                "<td class=\"r1c\"><div class=\"r1line\">{}{}</div><span class=\"cn-n\">n {n}</span></td>",
                ladder_chip(axis, a),
                ladder_chip(axis, b)
            ),
            _ => format!("<td class=\"r1c hatch\" title=\"{HATCH_NOT_COMPARED}\">—</td>"),
        }
    };
    let timing_td = |ms: Option<f64>, n: usize, boundary: &str| -> String {
        match ms {
            Some(v) => format!(
                "<td class=\"r1c\">{}<br/><span class=\"cn-n\">n {n} · {boundary}</span></td>",
                ladder_chip(tms, v)
            ),
            None => "<td class=\"r1c hatch\" title=\"not timed\">—</td>".to_string(),
        }
    };

    let mut h = String::new();

    // ── R1: tool × axis ──
    h.push_str("<div class=\"panel-title\">By tool and axis</div>");
    h.push_str("<div class=\"grid-scroll\"><table class=\"agrid r1grid\"><thead><tr title=\"Each cell is that tool's own number on the column-head axis (prop/eph km·mas, OD fits + median σ_eq, timing ms at its own boundary). One decade ladder; smaller is closer; read each column within itself; n per cell.\">");
    h.push_str(
        "<th class=\"gobj\">tool</th>\
         <th>propagation<br/><span class=\"r1sub\">p50 · p90 · km vs JPL</span></th>\
         <th>ephemeris<br/><span class=\"r1sub\">p50 · p90 · mas vs JPL</span></th>\
         <th>orbit determination<br/><span class=\"r1sub\">converged / attempted · median σ_eq</span></th>\
         <th>timing<br/><span class=\"r1sub\">p50 · ms · own boundary</span></th></tr></thead><tbody>",
    );
    // empyrean.
    h.push_str(&format!(
        "<tr class=\"gsub\"><td class=\"gtool\">empyrean <span class=\"r1ref\">(under test)</span></td>{}{}<td class=\"r1c\">{}/{}<br/><span class=\"cn-n\">σ_eq {}</span></td>{}</tr>",
        chip_pair(km, pct(&emp, 0.5), pct(&emp, 0.9), n_rows(&emp)),
        chip_pair(mas, pct(&emp_eph, 0.5), pct(&emp_eph, 0.9), n_rows(&emp_eph)),
        od_conv,
        od_att,
        med_sig_s,
        timing_td(emp_ms, emp_ms_n, "whole call"),
    ));
    // ASSIST — propagation states its all-rows and excl-gap medians (numbers only).
    let assist_prop = {
        let line = |label: &str, p50: Option<f64>, p90: Option<f64>, n: usize| -> String {
            match (p50, p90) {
                (Some(a), Some(b)) => format!(
                    "<div class=\"r1line\"><span class=\"cn-n r1tag\">{label}</span>{}{}<span class=\"cn-n\">n {n}</span></div>",
                    ladder_chip(km, a),
                    ladder_chip(km, b)
                ),
                _ => format!(
                    "<div class=\"r1line\"><span class=\"cn-n r1tag\">{label}</span><span class=\"cn-n\">—</span></div>"
                ),
            }
        };
        let n_excl = assist
            .iter()
            .filter(|((o, _), v)| !model_gap.contains(o.as_str()) && v.is_finite())
            .count();
        format!(
            "<td class=\"r1c\">{}{}</td>",
            line("all", pct(&assist, 0.5), pct(&assist, 0.9), n_rows(&assist)),
            line(
                "excl. gap",
                pct_excl(&assist, 0.5),
                pct_excl(&assist, 0.9),
                n_excl
            )
        )
    };
    h.push_str(&format!(
        "<tr class=\"gsub\"><td class=\"gtool\">ASSIST</td>{}<td class=\"r1c hatch\" title=\"ASSIST runs no ephemeris axis\"></td><td class=\"r1c hatch\" title=\"ASSIST runs no fit\"></td>{}</tr>",
        assist_prop,
        timing_td(assist_ms, assist_ms_n, "integrate only"),
    ));
    // find_orb.
    h.push_str(&format!(
        "<tr class=\"gsub\"><td class=\"gtool\">find_orb</td>{}{}<td class=\"r1c\">{}<br/><span class=\"cn-n\">σ_eq: {HATCH_NO_RECORD}</span></td>{}</tr>",
        chip_pair(km, pct(&findorb, 0.5), pct(&findorb, 0.9), n_rows(&findorb)),
        chip_pair(mas, pct(&fo_eph, 0.5), pct(&fo_eph, 0.9), n_rows(&fo_eph)),
        fo_conv,
        timing_td(fo_ms, fo_ms_n, "per fit"),
    ));
    // kete / jorbit (timing only).
    for (nm, ms, n) in [
        ("kete", kete_ms, kete_ms_n),
        ("jorbit", jorbit_ms, jorbit_ms_n),
    ] {
        h.push_str(&format!(
            "<tr class=\"gsub\"><td class=\"gtool\">{nm}</td><td class=\"r1c hatch\" title=\"timing only\"></td><td class=\"r1c hatch\" title=\"timing only\"></td><td class=\"r1c hatch\" title=\"runs no fit\"></td>{}</tr>",
            timing_td(ms, n, "whole call"),
        ));
    }
    // JPL reference row.
    h.push_str(&format!(
        "<tr class=\"gsub\"><td class=\"gtool\">JPL Horizons/SBDB</td><td class=\"r1c hatch\" title=\"reference\"></td><td class=\"r1c hatch\" title=\"reference\"></td><td class=\"r1c hatch\" title=\"reference\"></td><td class=\"r1c hatch\" title=\"{HATCH_NOT_TIMED}\"></td></tr>"
    ));
    // Tools with no rows in this run.
    for nm in ["OpenOrb", "GRSS", "OrbFit", "layup"] {
        h.push_str(&format!(
            "<tr class=\"gsub\"><td class=\"gtool\">{nm}</td><td class=\"r1c hatch\" colspan=\"4\" title=\"{HATCH_NOT_IN_RUN}\"></td></tr>"
        ));
    }
    h.push_str("</tbody></table></div>");
    // ── R2: tool × population ──
    let mut pops: Vec<&str> = obj2pop
        .values()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let pop_worst = |pop: &str| -> f64 {
        scoped(&emp, pop)
            .into_iter()
            .fold(f64::NEG_INFINITY, f64::max)
    };
    pops.sort_by(|a, b| {
        pop_worst(b)
            .partial_cmp(&pop_worst(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    h.push_str("<div class=\"panel-title\">By tool and population</div>");
    h.push_str("<div class=\"grid-scroll\"><table class=\"agrid r2grid\"><thead><tr title=\"Each cell: that tool's median km offset from JPL Horizons over a population's objects (n stated); one decade ladder, smaller closer; dagger = a population with an object ASSIST cannot model.\"><th class=\"gobj\">population</th><th>empyrean</th><th>ASSIST</th><th>find_orb</th></tr></thead><tbody>");
    for &pop in &pops {
        let pop_gap = obj2pop
            .iter()
            .any(|(o, p)| *p == pop && model_gap.contains(*o));
        h.push_str(&format!(
            "<tr class=\"gsub\"><td class=\"gtool\">{}</td>{}{}{}</tr>",
            attr_escape(pop),
            part2_cell(
                km,
                median_of(scoped(&emp, pop)),
                false,
                Some(scoped_nobj(&emp, pop))
            ),
            part2_cell(
                km,
                median_of(scoped(&assist, pop)),
                pop_gap,
                Some(scoped_nobj(&assist, pop))
            ),
            part2_cell(
                km,
                median_of(scoped(&findorb, pop)),
                false,
                Some(scoped_nobj(&findorb, pop))
            ),
        ));
    }
    h.push_str("</tbody></table></div>");
    // ── R3: tool × object (grouped by population, heatmap order) ──
    let objects: Vec<(&str, &str)> = obj2pop.iter().map(|(&o, &p)| (o, p)).collect();
    let mut by_pop: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for &(o, p) in &objects {
        by_pop.entry(p).or_default().push(o);
    }
    let mut blocks: Vec<(&str, Vec<&str>)> = by_pop.into_iter().collect();
    for (_, objs) in blocks.iter_mut() {
        objs.sort_by(|a, b| {
            obj_worst(b)
                .partial_cmp(&obj_worst(a))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
    let block_worst = |objs: &[&str]| -> f64 {
        objs.iter()
            .map(|o| obj_worst(o))
            .fold(f64::NEG_INFINITY, f64::max)
    };
    blocks.sort_by(|a, b| {
        block_worst(&b.1)
            .partial_cmp(&block_worst(&a.1))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    h.push_str("<div class=\"panel-title\">By tool and object</div>");
    h.push_str("<div class=\"grid-scroll\"><table class=\"agrid r3grid\"><thead><tr title=\"Each cell: that tool's median km difference from JPL Horizons across the seventeen horizons; smaller closer; one decade ladder; hatched where the tool has no rows; dagger = an object ASSIST cannot model.\"><th class=\"gobj\">object</th><th>empyrean</th><th>ASSIST</th><th>find_orb</th></tr></thead><tbody>");
    for (pop, objs) in &blocks {
        h.push_str(&format!(
            "<tr class=\"gpophdr\"><td class=\"gpopname\" colspan=\"4\">{} ({})</td></tr>",
            attr_escape(pop),
            objs.len()
        ));
        for &obj in objs {
            let is_gap = model_gap.contains(obj);
            h.push_str(&format!(
                "<tr class=\"gobjr\"><td class=\"gobjname2\">{}</td>{}{}{}</tr>",
                attr_escape(obj),
                part2_cell(km, obj_median(&emp, obj), false, None),
                part2_cell(km, obj_median(&assist, obj), is_gap, None),
                part2_cell(km, obj_median(&findorb, obj), false, None),
            ));
        }
    }
    h.push_str("</tbody></table></div>");
    // ── OD closeness by object and fitter (numbers-only) ──
    // The by-object cross-fitter view the one-tool §1.3A grid deliberately
    // omits: every fitter's own convergence, with empyrean's sigma-equivalent
    // distance to JPL. find_orb publishes no covariance, so it carries only its
    // convergence.
    {
        let mut od_sig: HashMap<&str, Option<f64>> = HashMap::new();
        for c in orbit_comparisons {
            if c.common_epoch_source == "sbdb" {
                let v = if c.sigma_equiv_combined.is_finite() {
                    Some(c.sigma_equiv_combined)
                } else {
                    None
                };
                od_sig.insert(c.object.as_str(), v);
            }
        }
        let mut emp_od: HashMap<&str, &ValidationResult> = HashMap::new();
        let mut fo_od: HashMap<&str, &ValidationResult> = HashMap::new();
        for r in results {
            if r.test_type == "orbit_determination" {
                if r.channel == "rust" {
                    emp_od.insert(r.object.as_str(), r);
                } else if r.channel == "core"
                    && (r.findorb_rms_residual.is_some() || r.findorb_n_obs_used.is_some())
                {
                    fo_od.insert(r.object.as_str(), r);
                }
            }
        }
        let emp_cell = |obj: &str| -> String {
            // JPL used radar for this object: the σ_eq comparison is not like for
            // like, so the cell is the one hatch (marked, counted out of the stat),
            // never dropped. The reason rides the hover.
            if let Some((del, dop)) = radar_counts.get(obj) {
                return format!(
                    "<td class=\"r1c hatch\" title=\"{}\"></td>",
                    attr_escape(&jpl_radar_hover(*del, *dop))
                );
            }
            match emp_od.get(obj).and_then(|r| r.od_converged) {
                Some(true) => {
                    if let Some(s) = od_sig.get(obj).and_then(|v| *v) {
                        let bin = od_sigma_bin(s);
                        format!("<td class=\"r1c gb{bin}\">{s:.2}</td>")
                    } else if od_sig.contains_key(obj) {
                        // The OD grid's glyphs: ? for a converged fit whose
                        // metric is undefined, · for one with no record.
                        "<td class=\"r1c\" title=\"converged · Σ_ref not SPD at the common epoch — metric undefined\">?</td>"
                            .to_string()
                    } else {
                        format!("<td class=\"r1c\" title=\"converged · {HATCH_NO_RECORD}\">·</td>")
                    }
                }
                Some(false) => "<td class=\"r1c\" title=\"did not converge\">✕</td>".to_string(),
                None => format!("<td class=\"r1c hatch\" title=\"{HATCH_NOT_ATTEMPTED}\"></td>"),
            }
        };
        let fo_cell = |obj: &str| -> String {
            match fo_od.get(obj).map(|r| r.findorb_rms_residual.is_some()) {
                Some(true) => "<td class=\"r1c\" title=\"converged · find_orb publishes no covariance, so no σ_eq\">·</td>".to_string(),
                _ => format!("<td class=\"r1c hatch\" title=\"{HATCH_NOT_ATTEMPTED}\"></td>"),
            }
        };
        h.push_str("<div class=\"panel-title\" title=\"By object and fitter — orbit determination (σ_eq to JPL)\">By object and fitter</div>");
        h.push_str("<div class=\"grid-scroll\"><table class=\"agrid r3grid\"><thead><tr title=\"Each empyrean cell is that object's σ-equivalent distance to the JPL SBDB solution, on the same σ_eq ladder as §1.3A; smaller is closer. find_orb publishes no covariance, so its cell shows only whether it fit the arc.\"><th class=\"gobj\">object</th><th>empyrean</th><th>find_orb</th></tr></thead><tbody>");
        for (pop, objs) in &blocks {
            h.push_str(&format!(
                "<tr class=\"gpophdr\"><td class=\"gpopname\" colspan=\"3\">{} ({})</td></tr>",
                attr_escape(pop),
                objs.len()
            ));
            for &obj in objs {
                h.push_str(&format!(
                    "<tr class=\"gobjr\"><td class=\"gobjname2\">{}</td>{}{}</tr>",
                    attr_escape(obj),
                    emp_cell(obj),
                    fo_cell(obj),
                ));
            }
        }
        h.push_str("</tbody></table></div>");
    }
    h
}

// ── Part 4: covariance realism ──────────────────────────────────────
//
// Is a stated one sigma really one sigma? The coverage statement is
// computable from today's sidecar: sigma_equiv follows chi(6)/sqrt(6) if both
// covariances are honest and the difference is pure statistical scatter. The
// complete per-object table carries the pathology fingerprint, the reduced-χ²
// pair against JPL, and the sigma ratio.

/// Pathology fingerprint from an OrbitComparison (design §4/§8): CONSISTENT,
/// CORRELATION (joint d² ≫ marginal), ROTATION (principal-axis rotation >30°),
/// or a scale-only disagreement.
fn cov_pathology(c: &crate::schema::OrbitComparison) -> &'static str {
    if !c.sigma_equiv_combined.is_finite() {
        return "no metric (Σ_ref not SPD)";
    }
    if c.sigma_equiv_combined < 1.0 {
        return "CONSISTENT";
    }
    let joint = c.mahalanobis_d2_combined_metric;
    let marg = c.mahalanobis_d2_marginal;
    if joint.is_finite() && marg.is_finite() && marg > 0.0 && joint > 4.0 * marg {
        return "CORRELATION";
    }
    if c.principal_axis_rotation_deg.is_finite() && c.principal_axis_rotation_deg > 30.0 {
        return "ROTATION";
    }
    "scale-only"
}

fn build_covariance_realism_html(
    results: &[ValidationResult],
    orbit_comparisons: &[crate::schema::OrbitComparison],
) -> String {
    // Canonical sbdb-direction records.
    let recs: Vec<&crate::schema::OrbitComparison> = orbit_comparisons
        .iter()
        .filter(|c| c.common_epoch_source == "sbdb")
        .collect();
    // Objects whose JPL solution used radar: their rows stay visible in the
    // per-object table (hatched, reason on hover) but are out of the coverage
    // statistics. One set, shared with §1.3A and Part 2.
    let radar_counts = jpl_radar_obs_counts(results);
    // OD rows for the chi2 pair and arc.
    let mut emp_chi: HashMap<&str, f64> = HashMap::new();
    let mut jpl_chi: HashMap<&str, f64> = HashMap::new();
    let mut arc: HashMap<&str, u32> = HashMap::new();
    for r in results {
        if r.test_type == "orbit_determination" {
            if r.channel == "rust"
                && let Some(v) = r.od_reduced_chi2
            {
                emp_chi.insert(r.object.as_str(), v);
            }
            if r.channel == "core" {
                if let Some(v) = r.ref_od_reduced_chi2 {
                    jpl_chi.insert(r.object.as_str(), v);
                }
                if let Some(v) = r.ref_od_data_arc_days {
                    arc.insert(r.object.as_str(), v);
                }
            }
        }
    }

    // Coverage against chi(6)/sqrt(6): expected median 0.944, fraction ≤1 57.7%.
    // Radar-informed JPL objects are out of the statistics (optical-only is not
    // like for like); the excluded count is stated in the coverage-table header.
    // Stated in OBJECTS from the one exclusion set (matches §1.3A); the SBDB
    // record tally rides in the header hover.
    let n_radar_excl_recs = recs
        .iter()
        .filter(|c| radar_counts.contains_key(c.object.as_str()))
        .count();
    let n_radar_excl_objs = radar_counts.len();
    let finite: Vec<f64> = recs
        .iter()
        .filter(|c| !radar_counts.contains_key(c.object.as_str()))
        .filter(|c| c.sigma_equiv_combined.is_finite())
        .map(|c| c.sigma_equiv_combined)
        .collect();
    let n_finite = finite.len();
    let n_total = recs
        .iter()
        .filter(|c| !radar_counts.contains_key(c.object.as_str()))
        .count();
    let mut fs = finite.clone();
    let med = if fs.is_empty() {
        f64::NAN
    } else {
        percentile(&mut fs, 0.5)
    };
    let le1 = finite.iter().filter(|&&v| v <= 1.0).count();
    let le3 = finite.iter().filter(|&&v| v <= 3.0).count();
    let gt10 = finite.iter().filter(|&&v| v > 10.0).count();
    let pct = |n: usize, d: usize| {
        if d > 0 {
            100.0 * n as f64 / d as f64
        } else {
            f64::NAN
        }
    };

    let mut h = String::new();
    h.push_str(&format!(
        "<div class=\"grid-summary\" title=\"A stated one sigma tested against JPL: empyrean's fitted covariances run about {0:.0}% too tight at the median, with a heavy right tail; a sigma-consistency test is not a coverage test.\">Median σ_eq {med:.2} vs 0.94 expected (6-DOF); ~{0:.0}% too tight.</div>",
        (med / 0.944 - 1.0) * 100.0
    ));

    // Coverage table.
    h.push_str(&format!(
        "<div class=\"grid-scroll\"><table class=\"agrid covtab\"><thead><tr><th class=\"gobj\">statistic</th><th>{}</th><th title=\"{n_radar_excl_recs} SBDB records excluded across the radar objects\">observed · {n_finite} finite of {n_total} · {n_radar_excl_objs} radar objects excluded</th></tr></thead><tbody>",
        uhdr("expected · χ(6)/√6"),
    ));
    h.push_str(&format!("<tr class=\"gsub\"><td class=\"gtool\">median σ_eq</td><td class=\"r1c\">0.944</td><td class=\"r1c\">{med:.2}</td></tr>"));
    h.push_str(&format!("<tr class=\"gsub\"><td class=\"gtool\">fraction ≤ 1σ</td><td class=\"r1c\">57.7%</td><td class=\"r1c\">{:.0}% ({}/{})</td></tr>", pct(le1, n_finite), le1, n_finite));
    h.push_str(&format!("<tr class=\"gsub\"><td class=\"gtool\">fraction ≤ 3σ</td><td class=\"r1c\">~100%</td><td class=\"r1c\">{:.0}% ({}/{})</td></tr>", pct(le3, n_finite), le3, n_finite));
    h.push_str(&format!("<tr class=\"gsub\"><td class=\"gtool\">entries beyond 10σ</td><td class=\"r1c\">~0</td><td class=\"r1c\">{gt10}</td></tr>"));
    h.push_str("</tbody></table></div>");

    // Complete per-object table.
    h.push_str("<div class=\"panel-title\" title=\"Complete per-object table — every object with a comparison record\">Complete per-object table</div>");
    h.push_str(&format!(
        "<div class=\"grid-scroll\"><table class=\"agrid covtab\"><thead><tr><th class=\"gobj\">object<br/><span class=\"cn-n\">empyrean vs JPL</span></th><th>{}</th><th>pathology</th><th>{}</th><th>{}</th><th>arc (days)</th></tr></thead><tbody>",
        uhdr("σ_eq"),
        uhdr("χ²ᵣ emp / JPL"),
        uhdr("σ ratio (a)"),
    ));
    let mut rows: Vec<&&crate::schema::OrbitComparison> = recs.iter().collect();
    rows.sort_by(|a, b| {
        let av = if a.sigma_equiv_combined.is_finite() {
            a.sigma_equiv_combined
        } else {
            -1.0
        };
        let bv = if b.sigma_equiv_combined.is_finite() {
            b.sigma_equiv_combined
        } else {
            -1.0
        };
        bv.partial_cmp(&av).unwrap_or(std::cmp::Ordering::Equal)
    });
    for c in rows {
        let o = c.object.as_str();
        // A radar-informed JPL object stays in the table but its σ_eq is the one
        // hatch (out of the statistics above), reason on hover.
        let seq = if let Some((del, dop)) = radar_counts.get(o) {
            format!(
                "<td class=\"r1c hatch\" title=\"{}\"></td>",
                attr_escape(&jpl_radar_hover(*del, *dop))
            )
        } else if c.sigma_equiv_combined.is_finite() {
            let bin = od_sigma_bin(c.sigma_equiv_combined);
            format!(
                "<td class=\"r1c gb{bin}\">{:.2}</td>",
                c.sigma_equiv_combined
            )
        } else {
            "<td class=\"r1c undef\" title=\"Σ_ref not positive-definite at the common epoch — no regularisation\">n/d</td>".to_string()
        };
        let chi_pair = match (emp_chi.get(o), jpl_chi.get(o)) {
            (Some(e), Some(j)) => format!("{e:.2} / {j:.2}"),
            (Some(e), None) => format!("{e:.2} / —"),
            _ => "—".to_string(),
        };
        // sigma ratio in the a element (sigma_fit[0]/sigma_ref[0]).
        let sr = {
            let f = c.sigma_fit[0];
            let r = c.sigma_ref[0];
            if f.is_finite() && r.is_finite() && r != 0.0 {
                format!("{:.2}", f / r)
            } else {
                "—".to_string()
            }
        };
        let arc_d = arc
            .get(o)
            .map(|v| v.to_string())
            .unwrap_or_else(|| "—".into());
        h.push_str(&format!(
            "<tr class=\"gsub\"><td class=\"gtool\">{}</td>{seq}<td class=\"r1c\">{}</td><td class=\"r1c\">{chi_pair}</td><td class=\"r1c\">{sr}</td><td class=\"r1c\">{arc_d}</td></tr>",
            attr_escape(o),
            cov_pathology(c),
        ));
    }
    h.push_str("</tbody></table></div>");
    // Named not-yet-measured list.
    h.push_str("<div class=\"disclosures\" style=\"margin-top:14px\"><details><summary>");
    h.push_str(DISCLOSURE_SUMMARY);
    h.push_str("</summary><div class=\"disc-body\">Not yet measured: propagated-covariance coverage (are k-sigma ellipsoids at +N years k-sigma in truth); multi-epoch / time-resolved realism; debiased residual statistics per weighting scheme; realism per observation noise model (the per-station Gaussian / Student-t / night-scale mixture of the observation-noise design, not yet in scott); sigma-point and Monte-Carlo cross-check against the first-order covariance; non-linearity diagnostics; the post-selection correction propagated through the metric.</div></details></div>");
    h
}

// ── Timing (wall clock) — per-pair panels, per-object grids, cross-tool ─────
//
// Every timing view sits on the one TimingMs decade ladder. Section 1 shows only
// the selected pair's per-object and per-population wall clock, an untimed side
// drawn hatched with its reason on the header hover; Part 2 carries the
// cross-tool matrices (every tool, every arm) the ruling keeps out of Section 1.
// Boundaries differ by tool — A integrate-only, B whole call, C per fit — each a
// header chip so a column is read on its own terms, never across tools.

/// A per-object timing value getter: returns the row's timing for one column,
/// or `None` when the row does not belong to that column.
type TimingGetter = Box<dyn Fn(&ValidationResult) -> Option<f64>>;

/// One column spec for a Part 2 timing grid: the header label, an optional
/// boundary chip (letter plus its hover text), an optional note chip (short text
/// plus its hover text), and the value getter for that column's cells.
type Part2TimingColumn<'a> = (
    &'a str,
    Option<(char, &'static str)>,
    Option<(&'static str, &'static str)>,
    TimingGetter,
);

/// The measured boundary of a timing column: a one-letter header chip with the
/// plain-words explanation on its hover.
const BOUND_A: (char, &str) = (
    'A',
    "integrate only: the integration call, force model built once outside the timer",
);
const BOUND_B: (char, &str) = (
    'B',
    "whole call: construction, integration, event detection and dense output",
);
const BOUND_C: (char, &str) = ('C', "per fit: one full orbit-determination fit");

/// The visible label a substituted empyrean ephemeris column carries, so the
/// core-for-rust fall-back is never silent.
const CORE_NOTE: (&str, &str) = (
    "core channel",
    "the rust channel carries no wall clock for this arm in this run; the core channel is shown",
);

/// One column of a timing grid.
struct TimingCol {
    /// Header label (a tool name or arm).
    label: String,
    /// Value getter; `None` for a member untimed on this axis (renders fully
    /// hatched, with `hatch_reason` on the header hover).
    getter: Option<TimingGetter>,
    /// Measured-boundary chip (letter, hover); absent on an untimed column.
    boundary: Option<(char, &'static str)>,
    /// A provenance note chip (label, hover) — the ephemeris core-channel
    /// substitution, always visible, never silent.
    note: Option<(&'static str, &'static str)>,
    /// Why an untimed column is hatched, in plain words (header hover).
    hatch_reason: Option<String>,
}

impl TimingCol {
    fn timed(
        label: impl Into<String>,
        getter: TimingGetter,
        boundary: (char, &'static str),
    ) -> Self {
        TimingCol {
            label: label.into(),
            getter: Some(getter),
            boundary: Some(boundary),
            note: None,
            hatch_reason: None,
        }
    }
    fn untimed(label: impl Into<String>, reason: impl Into<String>) -> Self {
        TimingCol {
            label: label.into(),
            getter: None,
            boundary: None,
            note: None,
            hatch_reason: Some(reason.into()),
        }
    }
    fn with_note(mut self, note: (&'static str, &'static str)) -> Self {
        self.note = Some(note);
        self
    }
}

/// The channel empyrean's ephemeris wall-clock is read from, and whether it is
/// the labelled core-channel fall-back. The one place this choice is made: the
/// rust channel is preferred, but this run carries no rust ephemeris timing, so
/// it falls back to core AND says so. Flip the preference here and every
/// ephemeris timing view follows.
fn emp_eph_channel(results: &[ValidationResult]) -> (&'static str, bool) {
    let rust_has = results.iter().any(|r| {
        r.channel == "rust"
            && r.test_type == "ephemeris"
            && r.emp_time_ms.map(|v| v.is_finite()).unwrap_or(false)
    });
    if rust_has {
        ("rust", false)
    } else {
        ("core", true)
    }
}

/// The channel empyrean's orbit-determination wall-clock is read from, and
/// whether it is the labelled core-channel fall-back. The one place this choice
/// is made: the rust channel is preferred, and a run carrying no rust OD timing
/// falls back to core AND says so. Flip the preference here and every OD timing
/// view follows, so no view silently pools the two channels.
fn emp_od_channel(results: &[ValidationResult]) -> (&'static str, bool) {
    let has = |ch: &str| {
        results.iter().any(|r| {
            r.channel == ch
                && r.test_type == "orbit_determination"
                && r.emp_time_ms.map(|v| v.is_finite()).unwrap_or(false)
        })
    };
    if has("rust") {
        ("rust", false)
    } else if has("core") {
        ("core", true)
    } else {
        // No OD wall-clock on either channel: nothing to substitute, so the
        // column renders untimed and claims no core fall-back.
        ("rust", false)
    }
}

// Column getters, shared by the Section-1 pair panels and the Part-2 grids.
fn rust_arm(u: &'static str) -> TimingGetter {
    Box::new(move |r: &ValidationResult| {
        if r.channel == "rust" && r.propagation_uncertainty.as_deref() == Some(u) {
            r.emp_time_ms
        } else {
            None
        }
    })
}
/// empyrean ephemeris wall-clock, read from the resolved channel.
fn emp_eph_getter(channel: &'static str) -> TimingGetter {
    Box::new(move |r: &ValidationResult| {
        if r.channel == channel {
            r.emp_time_ms
        } else {
            None
        }
    })
}
/// empyrean orbit-determination wall-clock, read from the resolved channel.
/// One channel only (never a silent rust+core pool): the caller passes the
/// channel `emp_od_channel` resolved, and a core fall-back is labelled.
fn emp_od_getter(channel: &'static str) -> TimingGetter {
    Box::new(move |r: &ValidationResult| {
        if r.channel == channel {
            r.emp_time_ms
        } else {
            None
        }
    })
}
/// ASSIST integrate-only propagation wall-clock (its default variational-off arm).
fn assist_prop_getter() -> TimingGetter {
    Box::new(|r: &ValidationResult| {
        if r.channel == "assist"
            && r.propagation_uncertainty.as_deref() == Some("assist_default_variational_off")
        {
            r.assist_time_ms
        } else {
            None
        }
    })
}

/// The timing columns for one tool on one axis, restricted to that tool. A member
/// untimed on the axis returns one hatched column with the plain reason.
fn tool_timing_cols(
    slug: &str,
    test_type: &str,
    emp_eph: (&'static str, bool),
    emp_od: (&'static str, bool),
) -> Vec<TimingCol> {
    match (slug, test_type) {
        // Section 1 shows only the first-order (Jet1) arm — the arm the agreement
        // heatmap above keys on; the f64 arm lives in Part 2's wall-clock matrix.
        ("empyrean", "propagation") => vec![TimingCol::timed(
            "empyrean",
            rust_arm(EMP_SECTION1_ARM),
            BOUND_B,
        )],
        ("empyrean", "ephemeris") => {
            let (ch, fallback) = emp_eph;
            let col = TimingCol::timed("empyrean", emp_eph_getter(ch), BOUND_B);
            vec![if fallback {
                col.with_note(CORE_NOTE)
            } else {
                col
            }]
        }
        ("empyrean", _) => {
            let (ch, fallback) = emp_od;
            let col = TimingCol::timed("empyrean", emp_od_getter(ch), BOUND_C);
            vec![if fallback {
                col.with_note(CORE_NOTE)
            } else {
                col
            }]
        }
        ("assist", "propagation") => {
            vec![TimingCol::timed("ASSIST", assist_prop_getter(), BOUND_A)]
        }
        ("assist", _) => vec![TimingCol::untimed(
            "ASSIST",
            "ASSIST: runs propagation only",
        )],
        ("findorb", "orbit_determination") => vec![TimingCol::timed(
            "find_orb",
            Box::new(|r: &ValidationResult| r.findorb_time_ms),
            BOUND_C,
        )],
        ("findorb", _) => vec![TimingCol::untimed(
            "find_orb",
            "find_orb: wall clock measured per OD fit only",
        )],
        ("jpl", _) => vec![TimingCol::untimed(
            "JPL",
            format!("JPL: reference solution, {HATCH_NOT_TIMED}"),
        )],
        (other, _) => vec![TimingCol::untimed(other.to_string(), HATCH_NOT_TIMED)],
    }
}

/// Render one per-object timing grid on the TimingMs ladder. `pop_median` adds a
/// per-population median cell to each population header row (Section 1); the
/// Part-2 grids leave it off.
fn build_timing_grid(
    results: &[ValidationResult],
    id: &str,
    title: &str,
    test_type: &str,
    columns: &[TimingCol],
    pop_median: bool,
    reading: &str,
) -> String {
    let axis = GridAxis::TimingMs;
    let mut objects: Vec<(&str, &str)> = Vec::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for r in results {
        if r.channel == "rust" && r.test_type == "propagation" && seen.insert(r.object.as_str()) {
            objects.push((r.object.as_str(), r.population.as_str()));
        }
    }
    objects.sort_by(|a, b| a.1.cmp(b.1).then(a.0.cmp(b.0)));
    let mut blocks: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for &(o, p) in &objects {
        blocks.entry(p).or_default().push(o);
    }
    let tt_rows: Vec<&ValidationResult> = results
        .iter()
        .filter(|r| r.test_type == test_type)
        .collect();

    // One cell: median of a column's finite values over the objects `want` picks.
    let cell = |col: &TimingCol, want: &dyn Fn(&str) -> bool| -> String {
        match &col.getter {
            None => "<td class=\"gc hatch\" title=\"not timed\"></td>".to_string(),
            Some(getter) => {
                let mut vals: Vec<f64> = tt_rows
                    .iter()
                    .filter(|r| want(r.object.as_str()))
                    .filter_map(|r| getter(r))
                    .filter(|v| v.is_finite())
                    .collect();
                if vals.is_empty() {
                    "<td class=\"gc hatch\" title=\"not timed\"></td>".to_string()
                } else {
                    let m = percentile(&mut vals, 0.5);
                    let bin = axis.bin(m);
                    format!(
                        "<td class=\"gc gb{bin}\" title=\"{}\"><span class=\"cn\">{}</span></td>",
                        attr_escape(&grid_fmt(axis, m)),
                        grid_fmt(axis, m)
                    )
                }
            }
        }
    };

    let mut h = String::new();
    if !title.is_empty() {
        h.push_str(&format!("<div class=\"panel-title\">{title}</div>"));
    }
    let rtr = if reading.is_empty() {
        String::new()
    } else {
        format!(" title=\"{}\"", attr_escape(reading))
    };
    h.push_str(&format!(
        "<div class=\"{GRID_SCROLL_STICKY}\"><table class=\"agrid fidgrid\" id=\"{id}\"><thead><tr{rtr}><th class=\"gobj\">object</th>"
    ));
    for col in columns {
        let mut chips = String::new();
        if let Some((letter, hover)) = col.boundary {
            chips.push_str(&format!(
                "<span class=\"tbchip\" title=\"{}\">{}</span>",
                attr_escape(hover),
                letter
            ));
        }
        if let Some((txt, hover)) = col.note {
            chips.push_str(&format!(
                "<span class=\"tbnote\" title=\"{}\">{}</span>",
                attr_escape(hover),
                txt
            ));
        }
        match &col.hatch_reason {
            Some(reason) => h.push_str(&format!(
                "<th class=\"fo-h hatch\" title=\"{}\">{}</th>",
                attr_escape(reason),
                attr_escape(&col.label)
            )),
            None => h.push_str(&format!(
                "<th class=\"fo-h\">{}{chips}</th>",
                attr_escape(&col.label)
            )),
        }
    }
    h.push_str("</tr></thead><tbody>");
    let ncols = columns.len() + 1;
    for (pop, objs) in &blocks {
        if pop_median {
            h.push_str(&format!(
                "<tr class=\"gblock\"><td class=\"gblock-l\">{} ({})</td>",
                attr_escape(pop),
                objs.len()
            ));
            for col in columns {
                h.push_str(&cell(col, &|o: &str| objs.contains(&o)));
            }
            h.push_str("</tr>");
        } else {
            h.push_str(&format!(
                "<tr class=\"gblock\"><td class=\"gblock-l\">{} ({})</td><td class=\"gblock-fill\" colspan=\"{}\"></td></tr>",
                attr_escape(pop),
                objs.len(),
                ncols - 1
            ));
        }
        for o in objs {
            h.push_str(&format!(
                "<tr class=\"gsub\"><td class=\"gtool\">{}</td>",
                attr_escape(o)
            ));
            for col in columns {
                h.push_str(&cell(col, &|obj: &str| obj == *o));
            }
            h.push_str("</tr>");
        }
    }
    h.push_str("</tbody></table></div>");
    h
}

/// Externals carrying an accuracy comparison on `test_type`, in the heatmap's
/// order. This is the SINGLE source of the legal external set: the propagation
/// and ephemeris agreement grids and the Section-1 timing panels all call it, so
/// the heatmap pairs and the timing pairs cannot drift apart.
fn axis_present_externals(
    results: &[ValidationResult],
    test_type: &str,
) -> Vec<(&'static str, &'static str)> {
    let core = |f: &dyn Fn(&ValidationResult) -> bool| {
        results
            .iter()
            .any(|r| r.channel == "core" && r.test_type == test_type && f(r))
    };
    let mut v = Vec::new();
    match test_type {
        "propagation" => {
            if core(&|r| r.assist_vs_horizons_km.is_some()) {
                v.push(("assist", "ASSIST"));
            }
            if core(&|r| r.findorb_vs_horizons_km.is_some()) {
                v.push(("findorb", "find_orb"));
            }
            if core(&|r| r.oorb_vs_horizons_km.is_some()) {
                v.push(("oorb", "OpenOrb"));
            }
            if core(&|r| r.grss_vs_horizons_km.is_some()) {
                v.push(("grss", "GRSS"));
            }
        }
        "ephemeris" => {
            if core(&|r| r.findorb_separation_arcsec.is_some()) {
                v.push(("findorb", "find_orb"));
            }
            if core(&|r| r.oorb_separation_arcsec.is_some()) {
                v.push(("oorb", "OpenOrb"));
            }
            if core(&|r| r.grss_separation_arcsec.is_some()) {
                v.push(("grss", "GRSS"));
            }
        }
        _ => {}
    }
    v
}

/// The (tool, ref) pairs a timing axis renders, mirroring exactly the pairs its
/// heatmap / OD panels carry so the selects never reach a missing timing panel.
fn timing_pairs(
    results: &[ValidationResult],
    test_type: &str,
) -> Vec<(&'static str, &'static str, &'static str, &'static str)> {
    if test_type == "orbit_determination" {
        let mut v = vec![("empyrean", "empyrean", "jpl", "JPL")];
        if od_tool_present("findorb", results) {
            v.push(("findorb", "find_orb", "jpl", "JPL"));
        }
        return v;
    }
    let externals = axis_present_externals(results, test_type);
    let mut v: Vec<(&'static str, &'static str, &'static str, &'static str)> =
        vec![("empyrean", "empyrean", "jpl", "JPL")];
    for &(s, l) in &externals {
        v.push(("empyrean", "empyrean", s, l));
    }
    for &(s, l) in &externals {
        v.push((s, l, "jpl", "JPL"));
    }
    for &(sa, la) in &externals {
        for &(sb, lb) in &externals {
            if sa != sb {
                v.push((sa, la, sb, lb));
            }
        }
    }
    v
}

/// Section-1 per-pair timing element for one axis: one hidden pair-panel per legal
/// pair (default empyrean/jpl visible), `showPairPanels` driving it with no new
/// client JS. Propagation and ephemeris draw a per-object timing HEATMAP per
/// timed member (same grid as the agreement heatmaps above); orbit
/// determination — a fit has no horizons — stays a per-object table.
fn build_pair_timing_panels(
    results: &[ValidationResult],
    test_type: &str,
    id: &str,
    emp_eph: (&'static str, bool),
    emp_od: (&'static str, bool),
) -> String {
    if test_type != "orbit_determination" {
        return build_cost_heatmap_panels(results, test_type, id, emp_eph, emp_od);
    }
    let pairs = timing_pairs(results, test_type);
    let mut h = String::new();
    h.push_str(&grid_key_html(GridAxis::TimingMs));
    h.push_str(&format!(
        "<div class=\"pair-panels\" data-section=\"{id}\">"
    ));
    for (i, &(ts, _tl, rs, _rl)) in pairs.iter().enumerate() {
        let default_attr = if i == 0 { " data-default=\"1\"" } else { "" };
        let hidden_attr = if i == 0 { "" } else { " hidden" };
        h.push_str(&format!(
            "<div class=\"pair-panel\" data-tool=\"{ts}\" data-ref=\"{rs}\"{default_attr}{hidden_attr}>"
        ));
        let mut cols = tool_timing_cols(ts, test_type, emp_eph, emp_od);
        cols.extend(tool_timing_cols(rs, test_type, emp_eph, emp_od));
        let table_id = format!("{id}-{ts}-{rs}");
        h.push_str(&build_timing_grid(
            results, &table_id, "", test_type, &cols, true, "",
        ));
        h.push_str("</div>");
    }
    h.push_str("<div class=\"pair-notcompared generic\" hidden>Not compared &mdash; no timing for this pair on this axis in this run.</div>");
    h.push_str("</div>");
    h
}

/// One member of a cost pair panel: either a timed heatmap (its label, boundary
/// chip, optional provenance note, per-cell wall-clock and per-cell row count) or
/// an untimed one-line mark carrying the reason on hover.
enum CostMember {
    Timed {
        label: String,
        boundary: (char, &'static str),
        note: Option<(&'static str, &'static str)>,
        vals: HashMap<(String, i64), f64>,
        counts: HashMap<(String, i64), usize>,
    },
    Untimed {
        label: String,
        reason: String,
    },
}

/// A timing heatmap's `(object, dt)` values and the row count behind each cell.
type TimingMap = (HashMap<(String, i64), f64>, HashMap<(String, i64), usize>);

/// Median (and contributing row count) of a timing getter per `(object, dt)`
/// over one test type's rows. Propagation and ASSIST give one row per cell
/// (count 1); ephemeris pools the observers, whose count rides the cell hover.
fn build_timing_map(
    results: &[ValidationResult],
    test_type: &str,
    getter: &TimingGetter,
) -> TimingMap {
    let mut lists: HashMap<(String, i64), Vec<f64>> = HashMap::new();
    for r in results {
        if r.test_type == test_type
            && let Some(v) = getter(r)
            && v.is_finite()
        {
            lists
                .entry((r.object.clone(), r.dt_days as i64))
                .or_default()
                .push(v);
        }
    }
    let mut vals = HashMap::new();
    let mut counts = HashMap::new();
    for (k, xs) in lists {
        let n = xs.len();
        if let Some(m) = grid_median(xs) {
            counts.insert(k.clone(), n);
            vals.insert(k, m);
        }
    }
    (vals, counts)
}

/// The cost-panel member for one tool on one axis, derived from the single
/// source of per-tool timing spec (`tool_timing_cols`) so the boundary, the
/// core-channel note and the untimed reason are never re-encoded. On propagation
/// and ephemeris each tool yields exactly one column.
fn cost_member(
    slug: &str,
    test_type: &str,
    results: &[ValidationResult],
    emp_eph: (&'static str, bool),
    emp_od: (&'static str, bool),
) -> CostMember {
    let col = tool_timing_cols(slug, test_type, emp_eph, emp_od)
        .into_iter()
        .next()
        .expect("tool_timing_cols yields at least one column");
    match col.getter {
        Some(getter) => {
            let (vals, counts) = build_timing_map(results, test_type, &getter);
            CostMember::Timed {
                label: col.label,
                boundary: col.boundary.expect("a timed column carries a boundary"),
                note: col.note,
                vals,
                counts,
            }
        }
        None => CostMember::Untimed {
            label: col.label,
            reason: col
                .hatch_reason
                .expect("an untimed column carries a reason"),
        },
    }
}

/// The tool-name heading above one cost heatmap: the tool label, its
/// timing-boundary chip and, for empyrean's ephemeris core fall-back, the
/// labelled core-channel chip.
fn cost_heatmap_heading(
    label: &str,
    boundary: (char, &'static str),
    note: Option<(&'static str, &'static str)>,
) -> String {
    let (letter, hover) = boundary;
    let mut s = format!(
        "<div class=\"panel-title\">{}<span class=\"tbchip\" title=\"{}\">{}</span>",
        attr_escape(label),
        attr_escape(hover),
        letter
    );
    if let Some((txt, hover)) = note {
        s.push_str(&format!(
            "<span class=\"tbnote\" title=\"{}\">{}</span>",
            attr_escape(hover),
            txt
        ));
    }
    s.push_str("</div>");
    s
}

/// The one-line hatched mark for an untimed cost member: the tool and "not
/// timed" visible, the full reason on hover (marks carry the message).
fn cost_untimed_line(label: &str, reason: &str) -> String {
    format!(
        "<div class=\"pair-notcompared\" title=\"{}\">{} &mdash; not timed</div>",
        attr_escape(reason),
        attr_escape(label)
    )
}

/// The numeric reading line above one cost heatmap, in the agreement grids'
/// idiom (numbers and separators, no sentence): median at epoch, median at the
/// farthest horizon and cell count, with the slowest cell (object and horizon)
/// on the line's hover so the visible line stays inside the word diet.
fn cost_summary_line(
    axis: GridAxis,
    block_order: &[(&str, Vec<&str>)],
    dts: &[i64],
    vals: &HashMap<(String, i64), f64>,
) -> String {
    let objs: Vec<&str> = block_order
        .iter()
        .flat_map(|(_, m)| m.iter().copied())
        .collect();
    let col_median = |dt: i64| -> Option<f64> {
        grid_median(
            objs.iter()
                .filter_map(|o| vals.get(&(o.to_string(), dt)).copied())
                .collect(),
        )
    };
    let col_count = |dt: i64| -> usize {
        objs.iter()
            .filter(|o| {
                vals.get(&(o.to_string(), dt))
                    .map(|v| v.is_finite())
                    .unwrap_or(false)
            })
            .count()
    };
    let epoch = col_median(0);
    // Farthest horizon: the largest |dt|, breaking a tie toward the column with
    // more timed cells (the backward arm survives the impactor truncation).
    let far = dts
        .iter()
        .copied()
        .max_by_key(|&dt| (dt.unsigned_abs(), col_count(dt)));
    // Slowest cell, tie-broken on (object, dt) so the pick never depends on map
    // iteration order.
    let (slow_v, slow_obj, slow_dt) = vals.iter().filter(|(_, v)| v.is_finite()).fold(
        (f64::NEG_INFINITY, "", 0i64),
        |acc, ((o, d), &v)| {
            if v > acc.0 || (v == acc.0 && (o.as_str(), *d) < (acc.1, acc.2)) {
                (v, o.as_str(), *d)
            } else {
                acc
            }
        },
    );
    let n = vals.values().filter(|v| v.is_finite()).count();
    let fmt_opt = |m: Option<f64>| m.map(|v| grid_fmt(axis, v)).unwrap_or_else(|| "·".into());
    let far_str = match far {
        Some(dt) => format!("{} {}", dt_col_label(dt), fmt_opt(col_median(dt))),
        None => "·".to_string(),
    };
    let slow_str = if slow_v.is_finite() {
        format!(
            "slowest {} {} at {}",
            grid_fmt(axis, slow_v),
            attr_escape(slow_obj),
            dt_col_label(slow_dt)
        )
    } else {
        "slowest ·".to_string()
    };
    // The slowest cell is already the darkest cell in the grid below; its value
    // rides the reading line's hover so the visible line stays inside the word
    // diet (marks carry the message, the number is on hover).
    format!(
        "<div class=\"grid-summary\" title=\"{}\">t0 {} · {} · {} cells</div>",
        attr_escape(&slow_str),
        fmt_opt(epoch),
        far_str,
        n
    )
}

/// Section-1 cost heatmaps for propagation / ephemeris: one hidden pair-panel per
/// legal pair (default empyrean/jpl visible), each drawing one timing heatmap per
/// TIMED member (tool then reference) on the same grid as the agreement heatmaps
/// above, plus a one-line hatched mark for an untimed member. Rows and their
/// order come from the agreement heatmap's empyrean values, so the two heatmaps
/// line up row for row.
fn build_cost_heatmap_panels(
    results: &[ValidationResult],
    test_type: &str,
    id: &str,
    emp_eph: (&'static str, bool),
    emp_od: (&'static str, bool),
) -> String {
    let axis = GridAxis::TimingMs;
    let pairs = timing_pairs(results, test_type);
    let (objects, dts, model_gap, impactors) = prop_like_axes(results);
    let marks = GridMarks {
        model_gap: &model_gap,
        impactors: &impactors,
    };
    let order_vals = if test_type == "ephemeris" {
        emp_eph_agreement_vals(results)
    } else {
        emp_prop_agreement_vals(results)
    };
    let block_order = prop_like_ordered_blocks(&objects, &dts, &order_vals);
    let empty_sigma: HashMap<(String, i64), f64> = HashMap::new();

    let mut h = String::new();
    h.push_str(&grid_key_html(axis));
    h.push_str(&format!(
        "<div class=\"pair-panels\" data-section=\"{id}\">"
    ));
    for (i, &(ts, _tl, rs, _rl)) in pairs.iter().enumerate() {
        let default_attr = if i == 0 { " data-default=\"1\"" } else { "" };
        let hidden_attr = if i == 0 { "" } else { " hidden" };
        h.push_str(&format!(
            "<div class=\"pair-panel\" data-tool=\"{ts}\" data-ref=\"{rs}\"{default_attr}{hidden_attr}>"
        ));
        // Tool first, then reference.
        let members = [ts, rs];
        let specs: Vec<CostMember> = members
            .iter()
            .map(|m| cost_member(m, test_type, results, emp_eph, emp_od))
            .collect();
        let any_timed = specs.iter().any(|m| matches!(m, CostMember::Timed { .. }));
        if !any_timed {
            // A pair with no timed member shows the single hatched line.
            if let CostMember::Untimed { label, reason } = &specs[0] {
                h.push_str(&cost_untimed_line(label, reason));
            }
        } else {
            for (mi, spec) in specs.iter().enumerate() {
                match spec {
                    CostMember::Timed {
                        label,
                        boundary,
                        note,
                        vals,
                        counts,
                    } => {
                        h.push_str(&cost_heatmap_heading(label, *boundary, *note));
                        h.push_str(&cost_summary_line(axis, &block_order, &dts, vals));
                        let table_id = format!("{id}-{ts}-{rs}-{}", members[mi]);
                        h.push_str(&render_grid_table(
                            &GridTable {
                                axis,
                                table_id: &table_id,
                                dts: &dts,
                                vals,
                                sigma: &empty_sigma,
                                counts: Some(counts),
                                marks: &marks,
                                show_model_gap: false,
                                trailing: GridTrailing::Median,
                                footers: false,
                            },
                            &block_order,
                        ));
                    }
                    CostMember::Untimed { label, reason } => {
                        h.push_str(&cost_untimed_line(label, reason));
                    }
                }
            }
        }
        h.push_str("</div>");
    }
    h.push_str("<div class=\"pair-notcompared generic\" hidden>Not compared &mdash; no timing for this pair on this axis in this run.</div>");
    h.push_str("</div>");
    h
}

/// p5 / p50 / p95 and sample count of a set of values, on the report's
/// percentile convention.
struct TStat {
    p5: f64,
    p50: f64,
    p95: f64,
    n: usize,
}
fn tstat(mut v: Vec<f64>) -> Option<TStat> {
    v.retain(|x| x.is_finite());
    if v.is_empty() {
        return None;
    }
    let n = v.len();
    Some(TStat {
        p5: percentile(&mut v, 0.05),
        p50: percentile(&mut v, 0.5),
        p95: percentile(&mut v, 0.95),
        n,
    })
}

/// Part-2 T1: every timing arm as a numbers-only matrix (p5 / p50 / p95 / n),
/// grouped by boundary — the dot strip's rows, ported. No ratio column; the
/// reader deduces. JPL is a hatched "not timed" row, an absent tool is hatched.
fn build_part2_timing_t1(results: &[ValidationResult]) -> String {
    // empyrean: rust preferred, core fall-back (labelled).
    let emp = |tt: &str, arm: Option<&'static str>| -> (Option<TStat>, bool) {
        for ch in ["rust", "core"] {
            let vals: Vec<f64> = results
                .iter()
                .filter(|r| {
                    r.channel == ch
                        && r.test_type == tt
                        && arm
                            .map(|a| r.propagation_uncertainty.as_deref() == Some(a))
                            .unwrap_or(true)
                })
                .filter_map(|r| r.emp_time_ms)
                .collect();
            if let Some(s) = tstat(vals) {
                return (Some(s), ch == "core");
            }
        }
        (None, false)
    };
    let ext = |tt: &str, f: &dyn Fn(&ValidationResult) -> Option<f64>| -> Option<TStat> {
        tstat(
            results
                .iter()
                .filter(|r| r.test_type == tt)
                .filter_map(f)
                .collect(),
        )
    };
    let assist = |arm: &'static str| -> Option<TStat> {
        tstat(
            results
                .iter()
                .filter(|r| {
                    r.channel == "assist" && r.propagation_uncertainty.as_deref() == Some(arm)
                })
                .filter_map(|r| r.assist_time_ms)
                .collect(),
        )
    };
    let tms = GridAxis::TimingMs;
    let row = |label: &str, note: Option<(&str, &str)>, st: Option<TStat>| -> String {
        let lbl = match note {
            Some((t, hov)) => format!(
                "{}<span class=\"tbnote\" title=\"{}\">{}</span>",
                attr_escape(label),
                attr_escape(hov),
                t
            ),
            None => attr_escape(label),
        };
        match st {
            Some(s) => format!(
                "<tr class=\"gsub\"><td class=\"gtool\">{lbl}</td><td>{}</td><td>{}</td><td>{}</td><td class=\"cn-n\">{}</td></tr>",
                ladder_chip(tms, s.p5),
                ladder_chip(tms, s.p50),
                ladder_chip(tms, s.p95),
                s.n
            ),
            None => format!(
                "<tr class=\"gsub\"><td class=\"gtool\">{lbl}</td><td class=\"gc hatch\" colspan=\"4\" title=\"{HATCH_NOT_IN_RUN}\"></td></tr>"
            ),
        }
    };
    let group = |title: &str, chip: (char, &str)| -> String {
        format!(
            "<tr class=\"gblock\"><td class=\"gblock-l\">{}<span class=\"tbchip\" title=\"{}\">{}</span></td><td class=\"gblock-fill\" colspan=\"4\"></td></tr>",
            attr_escape(title),
            attr_escape(chip.1),
            chip.0
        )
    };

    let mut h = String::new();
    h.push_str("<div class=\"panel-title\" title=\"Wall clock per row — every arm\">Per row</div>");
    h.push_str(&format!("<div class=\"{GRID_SCROLL_STICKY}\"><table class=\"agrid r1grid\" id=\"t1-speed-matrix\"><thead><tr title=\"Each row is one arm's wall clock (ms): p5, p50, p95 and n rows, grouped by boundary. Read within a boundary; the boundaries differ.\"><th class=\"gobj\">arm</th><th>p5</th><th>p50</th><th>p95</th><th>n</th></tr></thead><tbody>"));
    h.push_str(&group("propagation · integrate only", BOUND_A));
    h.push_str(&row(
        "ASSIST default (f64)",
        None,
        assist("assist_default_variational_off"),
    ));
    h.push_str(&row(
        "ASSIST default (variational)",
        None,
        assist("assist_default_variational_on"),
    ));
    h.push_str(&row(
        "ASSIST layup (f64)",
        None,
        assist("assist_layup_variational_off"),
    ));
    h.push_str(&row(
        "ASSIST asteroid-institute (f64)",
        None,
        assist("assist_asteroid_institute_variational_off"),
    ));
    for (lbl, arm) in [
        (
            "empyrean barebones, ASSIST-default-like (f64)",
            "none_detection_off_assist_default_like",
        ),
        (
            "empyrean barebones, asteroid-institute-like (f64)",
            "none_detection_off_assist_asteroid_institute_like",
        ),
        (
            "empyrean Jet1 barebones, ASSIST-default-like",
            "first_order_detection_off_assist_default_like",
        ),
    ] {
        let (s, core) = emp("propagation", Some(arm));
        h.push_str(&row(lbl, if core { Some(CORE_NOTE) } else { None }, s));
    }
    h.push_str(&group("propagation · whole call", BOUND_B));
    for (lbl, arm) in [
        ("empyrean f64", "none_detection_on"),
        ("empyrean first-order + 6×6", "first_order_detection_on"),
        ("empyrean auto cascade", "auto_detection_on"),
        ("empyrean second-order (Jet2)", "second_order_detection_on"),
        ("empyrean sigma-point", "sigma_point_detection_on"),
        ("empyrean Monte Carlo", "monte_carlo_detection_on"),
    ] {
        let (s, core) = emp("propagation", Some(arm));
        h.push_str(&row(lbl, if core { Some(CORE_NOTE) } else { None }, s));
    }
    h.push_str(&row("kete", None, ext("propagation", &|r| r.kete_time_ms)));
    h.push_str(&row(
        "jorbit",
        None,
        ext("propagation", &|r| r.jorbit_time_ms),
    ));
    h.push_str(&row(
        "OpenOrb",
        None,
        ext("propagation", &|r| r.oorb_time_ms),
    ));
    h.push_str(&row("GRSS", None, ext("propagation", &|r| r.grss_time_ms)));
    h.push_str(&group("ephemeris · whole call", BOUND_B));
    {
        let (s, core) = emp("ephemeris", None);
        h.push_str(&row(
            "empyrean",
            if core { Some(CORE_NOTE) } else { None },
            s,
        ));
    }
    h.push_str(&row("kete", None, ext("ephemeris", &|r| r.kete_time_ms)));
    h.push_str(&row("OpenOrb", None, ext("ephemeris", &|r| r.oorb_time_ms)));
    h.push_str(&row("GRSS", None, ext("ephemeris", &|r| r.grss_time_ms)));
    h.push_str(&row(
        "jorbit",
        None,
        ext("ephemeris", &|r| r.jorbit_time_ms),
    ));
    h.push_str(&group("orbit determination · per fit", BOUND_C));
    {
        let (s, core) = emp("orbit_determination", None);
        h.push_str(&row(
            "empyrean",
            if core { Some(CORE_NOTE) } else { None },
            s,
        ));
    }
    h.push_str(&row(
        "find_orb",
        None,
        ext("orbit_determination", &|r| r.findorb_time_ms),
    ));
    h.push_str(&row(
        "layup",
        None,
        ext("orbit_determination", &|r| r.layup_time_ms),
    ));
    h.push_str(&row(
        "OrbFit",
        None,
        ext("orbit_determination", &|r| r.orbfit_time_ms),
    ));
    h.push_str(&row(
        "GRSS",
        None,
        ext("orbit_determination", &|r| r.grss_time_ms),
    ));
    h.push_str(&format!("<tr class=\"gsub\"><td class=\"gtool\">JPL Horizons/SBDB</td><td class=\"gc hatch\" colspan=\"4\" title=\"{HATCH_NOT_TIMED} — network reference service\">{HATCH_NOT_TIMED}</td></tr>"));
    h.push_str("</tbody></table></div>");
    h
}

/// Part-2 T2: a population × tool median wall-clock matrix per axis, on the
/// TimingMs ladder via `part2_cell`, hatched where untimed.
fn build_part2_timing_t2(
    results: &[ValidationResult],
    emp_eph: (&'static str, bool),
    emp_od: (&'static str, bool),
) -> String {
    let (od_ch, od_fallback) = emp_od;
    let mut pops: Vec<&str> = results
        .iter()
        .filter(|r| r.channel == "rust" && r.test_type == "propagation")
        .map(|r| r.population.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    pops.sort();
    let mut pop_objs: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for r in results {
        if r.channel == "rust" && r.test_type == "propagation" {
            pop_objs
                .entry(r.population.as_str())
                .or_default()
                .insert(r.object.as_str());
        }
    }
    let tms = GridAxis::TimingMs;
    let pop_cell =
        |test_type: &str, pop: &str, getter: &dyn Fn(&ValidationResult) -> Option<f64>| -> String {
            let objs = pop_objs.get(pop);
            let mut vals: Vec<f64> = results
                .iter()
                .filter(|r| r.test_type == test_type)
                .filter(|r| objs.map(|s| s.contains(r.object.as_str())).unwrap_or(false))
                .filter_map(getter)
                .filter(|v| v.is_finite())
                .collect();
            if vals.is_empty() {
                part2_cell(tms, None, false, None)
            } else {
                let n = vals.len();
                let m = percentile(&mut vals, 0.5);
                part2_cell(tms, Some(m), false, Some(n))
            }
        };
    let render = |title: &str,
                  id: &str,
                  test_type: &str,
                  columns: Vec<Part2TimingColumn<'_>>,
                  _reading: &str,
                  reading_title: &str|
     -> String {
        let mut h = String::new();
        h.push_str(&format!(
            "<div class=\"panel-title\">{}</div>",
            attr_escape(title)
        ));
        let rt = attr_escape(reading_title);
        h.push_str(&format!(
            "<div class=\"{GRID_SCROLL_STICKY}\"><table class=\"agrid r2grid\" id=\"{id}\"><thead><tr title=\"{rt}\"><th class=\"gobj\">population</th>"
        ));
        for (label, boundary, note, _) in &columns {
            let mut chips = String::new();
            if let Some((letter, hover)) = boundary {
                chips.push_str(&format!(
                    "<span class=\"tbchip\" title=\"{}\">{}</span>",
                    attr_escape(hover),
                    letter
                ));
            }
            if let Some((txt, hover)) = note {
                chips.push_str(&format!(
                    "<span class=\"tbnote\" title=\"{}\">{}</span>",
                    attr_escape(hover),
                    txt
                ));
            }
            h.push_str(&format!("<th>{}{chips}</th>", attr_escape(label)));
        }
        h.push_str("</tr></thead><tbody>");
        for &pop in &pops {
            h.push_str(&format!(
                "<tr class=\"gsub\"><td class=\"gtool\">{}</td>",
                attr_escape(pop)
            ));
            for (_, _, _, getter) in &columns {
                h.push_str(&pop_cell(test_type, pop, getter.as_ref()));
            }
            h.push_str("</tr>");
        }
        h.push_str("</tbody></table></div>");
        h
    };

    let (eph_ch, eph_fallback) = emp_eph;
    let mut h = String::new();
    h.push_str(&render(
        "By population — propagation",
        "t2-prop-time",
        "propagation",
        vec![
            (
                "empyrean",
                Some(BOUND_B),
                None,
                rust_arm("first_order_detection_on"),
            ),
            ("ASSIST", Some(BOUND_A), None, assist_prop_getter()),
            (
                "kete",
                Some(BOUND_B),
                None,
                Box::new(|r: &ValidationResult| r.kete_time_ms),
            ),
            (
                "jorbit",
                Some(BOUND_B),
                None,
                Box::new(|r: &ValidationResult| r.jorbit_time_ms),
            ),
        ],
        "Median propagation ms per population; read down a column.",
        "Each cell is that tool's median propagation wall clock across a population's objects (ms), over n rows. Read down a column; boundaries differ.",
    ));
    h.push_str(&render(
        "By population — ephemeris",
        "t2-eph-time",
        "ephemeris",
        vec![
            (
                "empyrean",
                Some(BOUND_B),
                if eph_fallback { Some(CORE_NOTE) } else { None },
                emp_eph_getter(eph_ch),
            ),
            (
                "kete",
                Some(BOUND_B),
                None,
                Box::new(|r: &ValidationResult| r.kete_time_ms),
            ),
            (
                "jorbit",
                Some(BOUND_B),
                None,
                Box::new(|r: &ValidationResult| r.jorbit_time_ms),
            ),
        ],
        "Median ephemeris ms per population; read down a column.",
        "Each cell is that tool's median ephemeris wall clock over a population's rows (ms); the empyrean column names its channel.",
    ));
    h.push_str(&render(
        "By population — orbit-determination",
        "t2-od-time",
        "orbit_determination",
        vec![
            (
                "empyrean",
                Some(BOUND_C),
                if od_fallback { Some(CORE_NOTE) } else { None },
                emp_od_getter(od_ch),
            ),
            (
                "find_orb",
                Some(BOUND_C),
                None,
                Box::new(|r: &ValidationResult| r.findorb_time_ms),
            ),
        ],
        "Median per-fit ms per population; read down a column.",
        "Each cell is that tool's median per-fit wall clock across a population's objects (ms), over n rows. Read down a column.",
    ));
    h
}

/// Part-2 T3: the per-object timing grids G1 (propagation), G2 (ephemeris) and
/// G3 (orbit determination), each on the TimingMs ladder with its boundary chips
/// and one reading line. Every tool keeps its place; a tool untimed on an axis is
/// hatched, never dropped.
fn build_timing_grids(
    results: &[ValidationResult],
    emp_eph: (&'static str, bool),
    emp_od: (&'static str, bool),
) -> String {
    let (eph_ch, eph_fallback) = emp_eph;
    let (od_ch, od_fallback) = emp_od;
    let g1: Vec<TimingCol> = vec![
        TimingCol::timed("emp f64", rust_arm("none_detection_on"), BOUND_B),
        TimingCol::timed("emp Jet1", rust_arm("first_order_detection_on"), BOUND_B),
        TimingCol::timed("ASSIST", assist_prop_getter(), BOUND_A),
        TimingCol::timed(
            "kete",
            Box::new(|r: &ValidationResult| r.kete_time_ms),
            BOUND_B,
        ),
        TimingCol::timed(
            "jorbit",
            Box::new(|r: &ValidationResult| r.jorbit_time_ms),
            BOUND_B,
        ),
        TimingCol::untimed("find_orb", "find_orb: wall clock measured per OD fit only"),
    ];
    let emp_g2 = {
        let col = TimingCol::timed("empyrean", emp_eph_getter(eph_ch), BOUND_B);
        if eph_fallback {
            col.with_note(CORE_NOTE)
        } else {
            col
        }
    };
    let g2: Vec<TimingCol> = vec![
        emp_g2,
        TimingCol::timed(
            "kete",
            Box::new(|r: &ValidationResult| r.kete_time_ms),
            BOUND_B,
        ),
        TimingCol::timed(
            "jorbit",
            Box::new(|r: &ValidationResult| r.jorbit_time_ms),
            BOUND_B,
        ),
        TimingCol::untimed("OpenOrb", format!("OpenOrb: {HATCH_NOT_IN_RUN}")),
        TimingCol::untimed("GRSS", format!("GRSS: {HATCH_NOT_IN_RUN}")),
    ];
    let emp_g3 = {
        let col = TimingCol::timed("empyrean", emp_od_getter(od_ch), BOUND_C);
        if od_fallback {
            col.with_note(CORE_NOTE)
        } else {
            col
        }
    };
    let g3: Vec<TimingCol> = vec![
        emp_g3,
        TimingCol::timed(
            "find_orb",
            Box::new(|r: &ValidationResult| r.findorb_time_ms),
            BOUND_C,
        ),
        TimingCol::untimed("layup", format!("layup: {HATCH_NOT_IN_RUN}")),
        TimingCol::untimed("GRSS", format!("GRSS: {HATCH_NOT_IN_RUN}")),
    ];
    let mut h = String::new();
    h.push_str(&build_timing_grid(
        results,
        "g1-prop-time",
        "G1 · per-object propagation",
        "propagation",
        &g1,
        false,
        "Each cell is that arm's median wall clock over an object's rows; the chip marks its boundary. Read down a column, not across.",
    ));
    h.push_str(&build_timing_grid(
        results,
        "g2-eph-time",
        "G2 · per-object ephemeris",
        "ephemeris",
        &g2,
        false,
        "Each cell is that tool's median ephemeris wall clock for an object; the empyrean column names its channel. Read down a column.",
    ));
    h.push_str(&build_timing_grid(
        results,
        "g3-od-time",
        "G3 · per-object orbit-determination",
        "orbit_determination",
        &g3,
        false,
        "Each cell is that tool's median per-fit wall clock for an object on the ms ladder. Read down a column.",
    ));
    h
}

/// Part-2 §2.2: the cross-tool wall-clock matrices — T1 every arm, T2 by
/// population, T3 by object — numbers only, all on the one ladder.
fn build_part2_timing_matrices(
    results: &[ValidationResult],
    emp_eph: (&'static str, bool),
    emp_od: (&'static str, bool),
) -> String {
    let mut h = String::new();
    h.push_str(&build_part2_timing_t1(results));
    h.push_str(&build_part2_timing_t2(results, emp_eph, emp_od));
    h.push_str(&build_timing_grids(results, emp_eph, emp_od));
    h
}

/// One pair chip for a Part 1 section title: the selected comparison pair in the
/// two tools' colours. Server-rendered for the default pair (empyrean vs JPL);
/// its JS twin `updatePairTitles` rewrites every chip on a pair change. The
/// `pair-chip` class scopes its words as a chip (not title words) for the audit,
/// and `data-tool` / `data-ref` let the invariant confirm it names the selection.
/// Colours come from the theme-aware `--tool-<slug>` tokens (AA-safe in both
/// themes); `updatePairTitles` repaints them from the same tokens on a pair change.
fn pair_chip_html() -> &'static str {
    "<span class=\"pair-chip\" data-tool=\"empyrean\" data-ref=\"jpl\">\
<span class=\"pc-t\" style=\"color:var(--tool-empyrean)\">empyrean</span> \
<span class=\"pc-vs\">vs</span> \
<span class=\"pc-r\" style=\"color:var(--tool-jpl)\">JPL</span></span>"
}

/// The per-tool text colours as theme-aware CSS custom properties — one source
/// for both themes. A tool NAME rendered as text (the Part 1 compare-bar selects
/// and the section-title pair chips) reads its `--tool-<slug>` token so it clears
/// WCAG AA (>= 4.5:1) on the card / input surface in BOTH themes; the light values
/// are darkened exactly as `--ed-accent` is. Plotly traces keep the literal hex in
/// the client `TOOLS` map because a chart library cannot resolve a CSS variable.
fn tool_color_tokens_css() -> String {
    // (slug, dark, light): dark clears AA on the dark card/input surface #151b23,
    // light on the light card surface #edf1f6 (the tighter of the two light
    // surfaces). find_orb is nudged brighter in dark so it, too, clears AA as text.
    const TOOL_COLORS: [(&str, &str, &str); 4] = [
        ("empyrean", "#5b9bd5", "#2f6a9e"),
        ("jpl", "#3d9a6d", "#2c7550"),
        ("assist", "#c77dff", "#7c3aed"),
        ("findorb", "#dd6154", "#b83a2c"),
    ];
    let mut dark = String::from("  :root {");
    let mut light = String::from("  :root.theme-light {");
    for (slug, d, l) in TOOL_COLORS {
        dark.push_str(&format!(" --tool-{slug}: {d};"));
        light.push_str(&format!(" --tool-{slug}: {l};"));
    }
    dark.push_str(" }\n");
    light.push_str(" }\n");
    format!("{dark}{light}")
}

/// Generate the interactive HTML validation report.
///
/// `orbit_comparisons` is the optional sidecar from `validate od`'s
/// `_compare.jsonl` output — empty slice when no orbit-comparison data is
/// available (e.g., CI runs that skip the SBDB / find_orb queries).
pub fn generate_report(
    results: &[ValidationResult],
    orbit_comparisons: &[crate::schema::OrbitComparison],
    output: &Path,
    summary: Option<&Path>,
) -> Result<(), String> {
    let core_results: Vec<&ValidationResult> =
        results.iter().filter(|r| r.channel == "core").collect();
    let prop_results: Vec<&ValidationResult> = core_results
        .iter()
        .copied()
        .filter(|r| r.test_type == "propagation")
        .collect();
    let eph_results: Vec<&ValidationResult> = core_results
        .iter()
        .copied()
        .filter(|r| r.test_type == "ephemeris")
        .collect();
    let od_results: Vec<&ValidationResult> = core_results
        .iter()
        .copied()
        .filter(|r| r.test_type == "orbit_determination")
        .collect();

    // Population groupings, kept as (population → [object names]) so the
    // heatmap can emit blocks and per-population sort.
    let mut objects: Vec<(&str, &str)> = Vec::new();
    let mut seen_objects: BTreeSet<&str> = BTreeSet::new();
    for r in &prop_results {
        if seen_objects.insert(r.object.as_str()) {
            objects.push((r.object.as_str(), r.population.as_str()));
        }
    }
    // Sort by population then by object name.
    objects.sort_by(|a, b| a.1.cmp(b.1).then(a.0.cmp(b.0)));

    let mut dt_values: Vec<i64> = prop_results
        .iter()
        .map(|r| r.dt_days as i64)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    dt_values.sort();

    let mut tiers: Vec<&str> = prop_results
        .iter()
        .map(|r| r.force_model.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    tiers.sort();

    let mut populations: Vec<&str> = prop_results
        .iter()
        .map(|r| r.population.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    populations.sort();

    let mut channels: Vec<&str> = results
        .iter()
        .map(|r| r.channel.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    channels.sort_by(|a, b| match (*a == "core", *b == "core") {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.cmp(b),
    });

    // Per-channel coverage counts for the header line.
    let mut channel_coverage: BTreeMap<&str, usize> = BTreeMap::new();
    for r in results {
        *channel_coverage.entry(r.channel.as_str()).or_default() += 1;
    }

    let mut prop_lookup: HashMap<(&str, i64, &str), &ValidationResult> = HashMap::new();
    for r in &prop_results {
        prop_lookup.insert(
            (r.object.as_str(), r.dt_days as i64, r.force_model.as_str()),
            r,
        );
    }

    // BTreeMaps, not HashMaps: their serialization order is stable, so the
    // emitted JS literals are byte-identical across renders of the same input.
    let pop_colors: BTreeMap<&str, &str> = populations
        .iter()
        .map(|&p| (p, population_color(p)))
        .collect();
    let pop_colors_json = serde_json::to_string(&pop_colors).unwrap_or_default();
    let channel_colors_map: BTreeMap<&str, &str> =
        channels.iter().map(|&c| (c, channel_color(c))).collect();
    let channel_colors_json = serde_json::to_string(&channel_colors_map).unwrap_or_default();
    // Serialize the FULL result set. It no longer rides inside the page; it is
    // written to a sibling file the page fetches and verifies at boot. These
    // are the exact bytes the inline embed used to carry.
    let results_json = serde_json::to_string(&results).unwrap_or_default();
    // Embed the orbit-comparison sidecar (Mahalanobis distances etc.)
    // for the "Fitted orbit + covariance vs references" panel.
    let orbit_comparisons_json = serde_json::to_string(&orbit_comparisons).unwrap_or_default();
    // Objects whose JPL solution used radar, computed once here and handed to the
    // client as data so the covariance panels exclude them without re-deriving the
    // rule (which lives only in `jpl_radar_obs_counts`).
    let radar_excluded: Vec<&str> = jpl_radar_obs_counts(results).into_keys().collect();
    let radar_excluded_json = serde_json::to_string(&radar_excluded).unwrap_or_default();

    // Tools present in this run, derived server-side so the Tool / Reference
    // selects can populate and switch the server-rendered pair-panels before the
    // fetched dataset arrives (they never need it). A BTreeSet keeps the emitted
    // literal stable across renders.
    let mut tools_present: BTreeSet<&str> = BTreeSet::new();
    tools_present.insert("empyrean");
    tools_present.insert("jpl");
    for r in results {
        if r.assist_vs_horizons_km.is_some() || r.emp_vs_assist_km.is_some() {
            tools_present.insert("assist");
        }
        if r.oorb_vs_horizons_km.is_some() || r.oorb_separation_arcsec.is_some() {
            tools_present.insert("oorb");
        }
        if r.findorb_rms_residual.is_some()
            || r.findorb_vs_horizons_km.is_some()
            || r.findorb_d_ra_arcsec.is_some()
        {
            tools_present.insert("findorb");
        }
        if r.orbfit_rms_arcsec.is_some() {
            tools_present.insert("orbfit");
        }
        if r.layup_reduced_chi2.is_some() || r.layup_converged.is_some() {
            tools_present.insert("layup");
        }
        if r.grss_vs_horizons_km.is_some()
            || r.grss_separation_arcsec.is_some()
            || r.grss_rms_arcsec.is_some()
        {
            tools_present.insert("grss");
        }
    }
    let tools_present_json = serde_json::to_string(&tools_present).unwrap_or_default();

    // The dataset ships beside the page as "<output stem>.data.json" and the page
    // fetches it — the name derives from the output path so two reports written to
    // one directory never collide. The masthead download link points at it.
    let data_path = dataset_path_for(output);
    let data_file_name = data_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| format!("dataset path has no file name: {}", data_path.display()))?;

    let n_prop = prop_results.len();
    let n_eph = eph_results.len();
    let n_od = od_results.len();
    let n_objects = objects.len();
    let n_populations = populations.len();
    let n_dt = dt_values.len();
    let n_channels = channels.len();

    let report_run_iso = prop_results
        .first()
        .map(|r| r.timestamp.as_str())
        .unwrap_or("");
    let report_run_date = if report_run_iso.len() >= 10 {
        &report_run_iso[..10]
    } else {
        report_run_iso
    };
    let test_epoch = prop_results.first().map(|r| r.epoch_mjd_tdb).unwrap_or(0.0);

    // Test-epoch as a calendar date label (MJD → UTC date, no fractional).
    // MJD 40587 == 1970-01-01 UTC. Converting whole-day MJD only.
    let test_epoch_label = if test_epoch > 0.0 {
        let unix_days = test_epoch - 40587.0;
        let unix_seconds = (unix_days * 86_400.0) as i64;
        // Build a YYYY-MM-DD by hand (no chrono dep here).
        let days = unix_seconds / 86_400;
        let (y, m, d) = ymd_from_unix_days(days);
        format!("MJD {test_epoch:.2} TDB ≈ {y:04}-{m:02}-{d:02} UTC")
    } else {
        "—".to_string()
    };

    let coverage_line = {
        let mut bits: Vec<String> = Vec::new();
        for &ch in &channels {
            let n = channel_coverage.get(ch).copied().unwrap_or(0);
            bits.push(format!("{ch} {n}"));
        }
        bits.join(" · ")
    };

    // Per-population object counts so the legend doubles as a coverage
    // breakdown ("NEO (8)" rather than just "NEO").
    let mut pop_obj_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for &(_, pop) in &objects {
        *pop_obj_counts.entry(pop).or_default() += 1;
    }
    let mut pop_legend = String::new();
    for &pop in &populations {
        let color = population_color(pop);
        let n = pop_obj_counts.get(pop).copied().unwrap_or(0);
        pop_legend.push_str(&format!(
            "    <div class=\"legend-item\"><span class=\"pop-dot\" style=\"background:{color}\"></span>{pop} <span style=\"opacity:0.6\">({n})</span></div>\n"
        ));
    }

    let mut channel_legend = String::new();
    for &ch in &channels {
        let color = channel_color(ch);
        let n = channel_coverage.get(ch).copied().unwrap_or(0);
        channel_legend.push_str(&format!(
            "    <div class=\"legend-item\"><span class=\"pop-dot\" style=\"background:{color}\"></span>{ch} <span style=\"opacity:0.6\">({n})</span></div>\n"
        ));
    }

    let rollups = rollup_channels(results);
    // Hero-strip channel verdict: how many replay channels were compared
    // against core, and how many are bit-identical on every row. Injected
    // into the JS as HERO_CH so the Overview card is data-driven.
    let hero_ch_n = rollups
        .iter()
        .filter(|r| r.channel != "core" && r.n_total_compared > 0)
        .count();
    let hero_ch_pass = rollups
        .iter()
        .filter(|r| {
            r.channel != "core" && r.n_total_compared > 0 && r.n_passing == r.n_total_compared
        })
        .count();
    if let Some(path) = summary {
        write_summary(&rollups, path)?;
    }
    // Redesign (empyrean-k2rkx): server-side Part-1 agreement grids + H3.
    let h1_prop_grid_html = build_prop_agreement_grid(results);
    let h2_eph_grid_html = build_eph_agreement_grid(results);
    let od_closeness_grid_html = build_od_closeness_grid(results, orbit_comparisons);
    let h4_fidelity_grid_html = build_channel_fidelity_grid(results);
    let tool_ranking_html = build_tool_ranking_html(results, orbit_comparisons);
    let covariance_realism_html = build_covariance_realism_html(results, orbit_comparisons);
    // Timing (empyrean-52esa pass 6): the selected pair's per-object / per-population
    // wall clock in Section 1 (one per axis, 1.1C/1.2C/1.3C), all cross-tool
    // wall-clock matrices in Part 2 (§2.2). One place resolves the empyrean
    // ephemeris and orbit-determination channels so a core-for-rust fall-back
    // is always labelled and no view silently pools the two channels.
    let emp_eph = emp_eph_channel(results);
    let emp_od = emp_od_channel(results);
    let t1_prop_timing_html =
        build_pair_timing_panels(results, "propagation", "t1-prop", emp_eph, emp_od);
    let t2_eph_timing_html =
        build_pair_timing_panels(results, "ephemeris", "t2-eph", emp_eph, emp_od);
    let t3_od_timing_html =
        build_pair_timing_panels(results, "orbit_determination", "t3-od", emp_eph, emp_od);
    let part2_timing_html = build_part2_timing_matrices(results, emp_eph, emp_od);

    // ── Hero "quality summary" line — features the cross-channel
    // pass counts and OD convergence rather than raw throughput. Built
    // from the rollup so it's always consistent with the §10 matrix.
    // Legacy masthead verdict strip: computed but no longer displayed (the
    // redesign bans a curated verdict standing in for the data). Kept for now;
    // removed with the rest of the hero/pillars cleanup in the cut-list pass.
    let _quality_summary_html = {
        let mut parts: Vec<String> = Vec::new();
        for r in &rollups {
            if r.channel == "core" {
                parts.push(format!(
                    r#"<span style="color:#5b9bd5">{n}/{n} core (ref)</span>"#,
                    n = r.n_rows
                ));
                continue;
            }
            if r.n_total_compared == 0 {
                continue;
            }
            let color = if r.n_passing == r.n_total_compared {
                "#3d9a6d"
            } else if r.max_dr_km < 1.0 {
                "#a0a060"
            } else {
                "#e06252"
            };
            let tail = if r.n_passing < r.n_total_compared {
                format!(
                    r#" <span style="color:#8b9198">(max Δr {})</span>"#,
                    fmt_error(Some(r.max_dr_km))
                )
            } else {
                String::new()
            };
            parts.push(format!(
                r#"<span style="color:{color}">{p}/{t} {ch}</span>{tail}"#,
                p = r.n_passing,
                t = r.n_total_compared,
                ch = r.channel,
            ));
        }
        let n_od_rust = od_results.iter().filter(|r| r.channel == "core").count();
        let n_od_conv = od_results
            .iter()
            .filter(|r| r.channel == "core" && r.od_converged.unwrap_or(false))
            .count();
        let n_od_fo = od_results
            .iter()
            .filter(|r| {
                r.channel == "core"
                    && (r.findorb_rms_residual.is_some()
                        || r.orbfit_rms_arcsec.is_some()
                        || r.layup_reduced_chi2.is_some()
                        || r.grss_rms_arcsec.is_some()
                        || r.ref_od_reduced_chi2.is_some())
            })
            .count();
        let n_od_fo_missing = n_od_rust.saturating_sub(n_od_fo);
        let od_color = if n_od_conv == n_od_rust {
            "#3d9a6d"
        } else {
            "#a0a060"
        };
        parts.push(format!(
            r#"<span style="color:{od_color}">OD {n_od_conv}/{n_od_rust} converged</span> <span style="color:#8b9198">({n_od_fo} cross-checked vs external OD references{fo_skip})</span>"#,
            fo_skip = if n_od_fo_missing > 0 {
                format!("; {n_od_fo_missing} not cross-checked")
            } else {
                String::new()
            }
        ));
        parts.join(" · ")
    };

    // ── §13 Reproducibility footer — every detail a referee needs to
    // reproduce a number from this report. Static content for now;
    // version + git-hash fields hard-coded against the current pins
    // (empyrean 0.10.0 / empyrean-core v0.10.2 / hyperjet 1.15). Per-row
    // run-time provenance (commit hash, kernel hash) is a follow-up.
    let provenance_footer_html = format!(
        r##"
<div class="section" id="s13" data-page="both">
  <div class="section-num">A.4</div>
  <div class="section-title" title="Reproducibility — provenance for this run: the frame, ephemeris, force model, and external references behind the numbers.">Provenance</div>
  <div class="heatmap-container">
  <table class="od-table" style="font-size:11px;">
    <thead><tr><th style="text-align:left">Component</th><th style="text-align:left">Value</th></tr></thead>
    <tbody>
      <tr><td>Frame</td><td>ICRF (J2000), barycentric</td></tr>
      <tr><td>Time scale</td><td>TDB (Barycentric Dynamical Time)</td></tr>
      <tr><td>Planetary ephemeris</td><td>JPL DE440</td></tr>
      <tr><td>Asteroid perturber set</td><td>SB441-N16: <code>1, 2, 3, 4, 7, 10, 15, 16, 31, 52, 65, 87, 88, 107, 511, 704</code></td></tr>
      <tr><td>Force model (standard)</td><td>Point-mass Sun + 8 planets + Moon + Pluto + 16 SB441-N16 asteroids; 1PN GR Einstein-Infeld-Hoffmann for Sun; non-gravitational A1 (radial) / A2 (transverse) / A3 (normal) accelerations with Marsden's <code>g(r)</code>. For asteroids, A2 ≠ 0 is the standard parameterisation of the <b>Yarkovsky effect</b> (Marsden, Sekanina &amp; Yeomans 1973 / Vokrouhlický et al. 2015).</td></tr>
      <tr><td>Integrator</td><td>GR15 (15-stage Gauss-Radau, adaptive step; derived solely from Everhart 1985)</td></tr>
      <tr><td>Autodiff (Jet1 STM)</td><td>forward-mode, N = 6 (or 9 with non-grav)</td></tr>
      <tr><td>External OD references</td><td>JPL SBDB (reported normalized rms → reduced χ² + n_obs) &middot; layup (Holman, Smithsonian/CfA) &middot; find_orb (Project Pluto, B. Gray) &middot; GRSS (Makadia et al. &mdash; the second radar-capable reference)</td></tr>
      <tr><td>External propagation references</td><td>ASSIST 1.2.3 (Holman et al. 2023) on REBOUND 4.6.0 IAS15, run under three configurations (see the six-arm section &mdash; settings read back after <code>assist.Extras</code> attach): <b>default</b> (mode 1 &ldquo;global&rdquo;, &epsilon; 1e-9, min_dt 0, initial dt 0.001 d), <b>layup</b> (mode 2, else identical), <b>asteroid_institute</b> (mode 1, &epsilon; 1e-6, min_dt 1e-9 d, initial dt 1e-6 d). ASSIST models no Marsden <b>DT</b> (non-gravitational time delay) term, so the five objects whose plan IC carries one &mdash; 67P (&Delta;T +45.69 d), 103P/Hartley 2 (+12.23 d), 46P/Wirtanen (&minus;14.15 d), 2I/Borisov (&minus;65.13 d), 3I/ATLAS (+9.48 d) &mdash; diverge from Horizons under every configuration; empyrean applies &Delta;T and matches Horizons to metres, so those rows are excluded from the ASSIST position medians and marked, never dropped. &middot; OpenOrb (Granvik et al.)</td></tr>
      <tr><td>Observation source</td><td>Minor Planet Center API; fetched at runtime</td></tr>
      <tr><td>Observation weights</td><td>Vereš–Farnocchia–Chesley 2017 (VFC17) per-station RMS floors + nightly deweighting; Eggl–Farnocchia–Chamberlin–Chesley 2020 (EFCC2020) star-catalog debiasing</td></tr>
      <tr><td>Outlier rejection</td><td>empyrean OD: adaptive information-aware χ² rejection (residual statistics per Carpino, Milani &amp; Chesley 2003)</td></tr>
      <tr><td>JPL source of truth</td><td>One JPL solution, two views: <b>Horizons</b> vectors + ephemerides give the propagation / sky-plane truth; <b>SBDB</b> gives the fitted elements, covariance (Fitted Orbit &amp; Covariance panel), and the reported fit quality (normalized rms, n_obs, radar counts, arc, condition code) used as JPL's OD result.</td></tr>
      <tr><td>Cross-channel fidelity</td><td>distribution channels are bit-identical to the reference to ≤ 10⁻¹⁰ km (100 nm — a sub-ULP floor; float64 ULP at 1 AU ≈ 30 µm)</td></tr>
      <tr><td>Coverage</td><td>{coverage_line}</td></tr>
      <tr><td>Test epoch</td><td>{test_epoch_label}</td></tr>
      <tr><td>Report generated</td><td>{report_run_date}</td></tr>
    </tbody>
  </table>
  </div>
  <div class="refs" style="margin-top:24px; font-size:11px;">
    <details><summary class="ref-head"><b>References</b></summary>
    <div class="ref">Holman, M. et al. 2023, "ASSIST: An ephemeris-quality test-particle integrator", PSJ 4(4), 69 (DOI 10.3847/PSJ/acc9a9).</div>
    <div class="ref">Vereš, P. et al. 2017, "Statistical analysis of astrometric errors for the most productive asteroid surveys", Icarus 296, 139.</div>
    <div class="ref">Eggl, S., Farnocchia, D., Chamberlin, A. B., Chesley, S. R. 2020, "Star catalog position and proper motion corrections in asteroid astrometry II: the Gaia era", Icarus 339, 113596.</div>
    <div class="ref">Park, R. S. et al. 2021, "The JPL Planetary and Lunar Ephemerides DE440 and DE441", AJ 161, 105.</div>
    <div class="ref">Marsden, B. G., Sekanina, Z., Yeomans, D. K. 1973, "Comets and Nongravitational Forces. V", AJ 78, 211.</div>
    <div class="ref">Vokrouhlický, D. et al. 2015, "The Yarkovsky and YORP Effects", in <i>Asteroids IV</i>, p. 509.</div>
    <div class="ref">Everhart, E. 1985, in "Dynamics of Comets" (Reidel), p. 185.</div>
    </details>
  </div>
</div>
"##,
        report_run_date = report_run_date,
        coverage_line = coverage_line,
        test_epoch_label = test_epoch_label,
    );

    let html = format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Empyrean Dynamics — Validation Report</title>
<link rel="icon" href="data:,">
<link href="https://fonts.googleapis.com/css2?family=Syne:wght@400;500;600;700;800&family=JetBrains+Mono:wght@300;400;500&family=DM+Sans:wght@300;400;500&display=swap" rel="stylesheet">
<script src="https://cdn.plot.ly/plotly-2.35.0.min.js"></script>
<style>
{brand_tokens_css}
  * {{ margin: 0; padding: 0; box-sizing: border-box; }}
  body {{ background: var(--ed-bg); color: var(--ed-text-primary); font-family: var(--ed-font-body); font-weight: 300; min-height: 100vh; }}
  .header {{ padding: 60px 60px 40px; max-width: 1400px; margin: 0 auto; border-bottom: 1px solid var(--ed-border); }}
  .header h1 {{ font-family: var(--ed-font-display); font-weight: 700; font-size: 36px; color: var(--ed-text-primary); letter-spacing: -0.5px; margin-bottom: 4px; }}
  .header h2 {{ font-family: var(--ed-font-display); font-weight: 700; font-size: 36px; color: rgba(232,232,236,0.40); letter-spacing: -0.5px; margin-bottom: 12px; }}
  .header .subtitle {{ font-family: var(--ed-font-mono); font-size: 11px; color: var(--ed-accent); letter-spacing: 2px; text-transform: uppercase; }}
  .header .meta {{ font-family: var(--ed-font-mono); font-size: 11px; color: var(--ed-text-muted); margin-top: 8px; }}
  .header .provenance {{ font-family: var(--ed-font-mono); font-size: 11px; color: var(--ed-text-secondary); margin-top: 6px; line-height: 1.7; }}

  .section {{ padding: 60px 60px; max-width: 1400px; margin: 0 auto; border-bottom: 1px solid var(--ed-border); }}
  .tool-hidden {{ display: none !important; }}
  .page-hidden {{ display: none !important; }}
  #page-nav {{ max-width: 1400px; margin: 0 auto; padding: 16px 60px 0; display: flex; flex-wrap: wrap; gap: 4px; border-bottom: 1px solid var(--ed-border); }}
  .page-tab {{ background: transparent; color: var(--ed-text-secondary); border: none; border-bottom: 2px solid transparent; padding: 10px 16px; margin-bottom: -1px; cursor: pointer; font-family: var(--ed-font-mono); font-size: 12px; letter-spacing: 0.5px; text-transform: uppercase; }}
  .page-tab:hover {{ color: var(--ed-text-primary); }}
  .page-tab.active {{ color: var(--ed-accent); border-bottom-color: var(--ed-accent); }}
  .section:last-child {{ border-bottom: none; }}
  .section-num {{ font-family: var(--ed-font-mono); font-size: 11px; color: var(--ed-text-muted); letter-spacing: 2px; text-transform: uppercase; margin-bottom: 8px; }}
  .section small {{ font-size: 11px; }}
  .section-title {{ font-family: var(--ed-font-display); font-weight: 700; font-size: 24px; color: var(--ed-text-primary); margin-bottom: 8px; }}
  .section-desc {{ font-size: 13px; color: var(--ed-text-secondary); line-height: 1.6; max-width: 900px; margin-bottom: 24px; }}
  .panel-title {{ font-family: var(--ed-font-display); font-size: 14px; color: var(--ed-text-primary); margin: 24px 0 8px 0; }}

  .heatmap-container {{ overflow-x: auto; max-width: 100%; padding-bottom: 8px; }}
  .heatmap-container::-webkit-scrollbar {{ height: 6px; }}
  .heatmap-container::-webkit-scrollbar-track {{ background: var(--ed-bg); border-radius: 3px; }}
  .heatmap-container::-webkit-scrollbar-thumb {{ background: var(--ed-scrollbar-thumb); border-radius: 3px; }}
  .heatmap-container::-webkit-scrollbar-thumb:hover {{ background: var(--ed-scrollbar-hover); }}
  table.heatmap {{ border-collapse: collapse; font-family: var(--ed-font-mono); font-size: 11px; }}
  table.heatmap th {{ padding: 6px 10px; color: var(--ed-text-secondary); font-weight: 400; letter-spacing: 1px; border-bottom: 1px solid var(--ed-border); position: sticky; top: var(--sticky-offset, 0px); background: var(--ed-bg); z-index: 1; }}
  /* One rule: every server-rendered table header renders uppercase, the
     agreement heatmaps' canonical case (Part 2 matrices and the outliers table
     included). Uppercasing words is right; uppercasing a symbol, a variable
     name or a unit corrupts it (σ_eq→Σ_EQ, dRA·cos(δ)→DRA·COS(Δ), ms→MS), so any
     header fragment carrying one is wrapped in the ONE no-transform span below
     ([`uhdr`] server-side, its `uhdr` twin client-side). */
  table.heatmap th, .od-table th, table.agrid th, table.r1grid th, table.r2grid th, table.r3grid th {{ text-transform: uppercase; }}
  th .u {{ text-transform: none; letter-spacing: normal; }}
  table.heatmap th.dt-col {{ text-align: center; min-width: 70px; }}
  table.heatmap td {{ padding: 5px 8px; text-align: center; border-bottom: 1px solid var(--ed-border-subtle); cursor: default; }}
  table.heatmap td.obj-name {{ text-align: left; color: var(--ed-text-primary); font-size: 11px; white-space: nowrap; padding-right: 16px; position: sticky; left: 0; background: var(--ed-bg); }}
  table.heatmap td.pop-tag {{ text-align: left; font-size: 11px; padding-right: 12px; }}
  table.heatmap td.cell {{ font-size: 11px; border-radius: 2px; }}
  table.heatmap td.cell:hover {{ outline: 1px solid var(--ed-accent); outline-offset: -1px; }}
  table.heatmap td.cell.missing {{
    background: repeating-linear-gradient(45deg, #161c25, #161c25 4px, #1f2630 4px, #1f2630 8px);
    color: var(--ed-text-muted);
  }}
  .pop-dot {{ display: inline-block; width: 6px; height: 6px; border-radius: 50%; margin-right: 4px; vertical-align: middle; }}
  .chart-container {{ background: var(--ed-surface); border: 1px solid var(--ed-border); border-radius: var(--ed-radius-md); padding: 16px; margin-bottom: 24px; }}
  .summary-grid {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: 12px; margin-bottom: 24px; }}
  .summary-card {{ background: var(--ed-surface); border: 1px solid var(--ed-border); border-radius: var(--ed-radius-md); padding: 20px; }}
  .summary-card .value {{ font-family: var(--ed-font-display); font-weight: 700; font-size: 28px; color: var(--ed-text-primary); }}
  .summary-card .label {{ font-family: var(--ed-font-mono); font-size: 11px; color: var(--ed-text-secondary); letter-spacing: 1px; text-transform: uppercase; margin-top: 4px; }}
  .legend {{ display: flex; gap: 16px; flex-wrap: wrap; margin-bottom: 16px; }}
  .legend-item {{ display: flex; align-items: center; gap: 4px; font-family: var(--ed-font-mono); font-size: 11px; color: var(--ed-text-secondary); }}
  .channel-toggle {{ display: inline-flex; flex-wrap: wrap; gap: 6px; margin: 8px 0 16px 0; font-family: var(--ed-font-mono); font-size: 11px; }}
  .channel-toggle button {{ background: var(--ed-surface); border: 1px solid var(--ed-border); border-radius: var(--ed-radius-sm); color: var(--ed-text-secondary); padding: 4px 10px; cursor: pointer; font-family: inherit; font-size: 11px; }}
  .channel-toggle button.active {{ color: var(--ed-text-primary); border-color: var(--ed-accent); background: var(--ed-surface-raised); }}
  .od-table {{ font-family: var(--ed-font-mono); font-size: 11px; border-collapse: collapse; min-width: 100%; }}
  .od-table th, .od-table td {{ padding: 6px 10px; border-bottom: 1px solid var(--ed-border); text-align: right; }}
  .od-table th {{ color: var(--ed-text-secondary); font-weight: 400; letter-spacing: 1px; font-size: 11px; }}
  .od-table td.obj {{ text-align: left; color: var(--ed-text-primary); }}
  .od-table tr.diverged {{ background: rgba(208, 80, 64, 0.08); }}

  /* Convergence matrix — three states, three glyphs, three classes. The
     not-attempted hatch reuses the heatmap's `.cell.missing` fill so "we
     have no number here" looks the same everywhere in the report. */
  table.heatmap td.conv {{ font-size: 13px; line-height: 1.1; cursor: help; }}
  table.heatmap td.conv-ok {{ color: #3d9a6d; }}
  table.heatmap td.conv-fail {{ color: #e06252; background: rgba(208, 80, 64, 0.10); }}
  table.heatmap td.conv-na {{
    color: var(--ed-text-muted);
    background: repeating-linear-gradient(45deg, #161c25, #161c25 4px, #1f2630 4px, #1f2630 8px);
  }}
  table.heatmap tr.conv-footer td {{ border-top: 1px solid var(--ed-border); padding-top: 8px; }}
  .conv-key {{ font-size: 13px; margin-right: 4px; }}
  .conv-key.conv-ok {{ color: #3d9a6d; }}
  .conv-key.conv-fail {{ color: #e06252; }}
  .conv-key.conv-na {{ color: var(--ed-text-muted); }}

  /* Capability pillar band (Overview) */
  .pillar-band {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(250px, 1fr)); gap: 12px; margin-top: 14px; }}
  .pillar {{ background: var(--ed-surface); border: 1px solid var(--ed-border); border-left-width: 3px; border-radius: var(--ed-radius-sm); padding: 14px 16px; display: flex; flex-direction: column; gap: 7px; }}
  .pillar .pt {{ font-family: var(--ed-font-mono); font-size: 11px; letter-spacing: 2px; text-transform: uppercase; color: var(--ed-text-secondary); }}
  .pillar .pc {{ font-family: var(--ed-font-display); font-weight: 700; font-size: 15px; color: var(--ed-text-primary); line-height: 1.35; }}
  .pillar .pp {{ font-family: var(--ed-font-mono); font-size: 11px; color: var(--ed-text-muted); line-height: 1.9; }}
  .pillar .pl {{ font-family: var(--ed-font-mono); font-size: 11px; margin-top: auto; }}
  .pillar .pl a {{ color: var(--ed-accent); text-decoration: none; cursor: pointer; }}
  /* Timing boundary / provenance chips on grid column headers. */
  .tbchip {{ display: inline-block; margin-left: 4px; padding: 0 4px; border-radius: 3px; background: var(--ed-surface); color: var(--ed-text-secondary); font-family: var(--ed-font-mono); font-size: 9px; font-weight: 700; vertical-align: middle; cursor: help; }}
  .tbnote {{ display: inline-block; margin-left: 4px; padding: 0 4px; border-radius: 3px; background: var(--ed-surface); color: var(--ed-text-muted); font-family: var(--ed-font-mono); font-size: 9px; letter-spacing: 0.3px; vertical-align: middle; cursor: help; }}
  /* Keyboard focus visibility for interactive controls (WCAG 2.4.7) */
  .page-tab:focus-visible, .channel-toggle button:focus-visible,
  [role="button"]:focus-visible {{
    outline: 2px solid var(--ed-accent); outline-offset: 2px;
  }}
  /* ── Overview (basic) view ─────────────────────────────────────── */
  /* The Overview tab reuses the comparison page's DOM: prose, section
     numbers, and per-section scorecards hide; one-line captions and the
     hero verdict strip show. */
  .basic-caption {{ display: none; color: var(--ed-text-secondary); font-size: 13px; margin: 2px 0 14px; }}
  body.view-overview .section-desc, body.view-overview .section-num {{ display: none; }}
  body.view-overview .basic-caption {{ display: block; }}
  body.view-overview .header .meta {{ display: none; }}
  .hero-cards {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(185px, 1fr)); gap: 12px; margin: 14px 0 6px; }}
  .hero-card {{ background: var(--ed-surface); border: 1px solid var(--ed-border); border-radius: var(--ed-radius-sm); padding: 16px 18px; }}
  .hero-card .hv {{ font-family: var(--ed-font-display); font-weight: 700; font-size: 30px; color: var(--ed-text-primary); line-height: 1.15; }}
  .hero-card .hl {{ font-family: var(--ed-font-mono); font-size: 11px; color: var(--ed-text-secondary); letter-spacing: 1.5px; text-transform: uppercase; margin-bottom: 6px; }}
  .hero-card .hs {{ font-size: 11px; color: var(--ed-text-muted); margin-top: 6px; }}
  .hero-badge {{ font-size: 13px; font-weight: 700; margin-right: 6px; }}
  .hero-badge.pass {{ color: #3d9a6d; }}
  .hero-badge.warn {{ color: #e8a040; }}
  #hero-title {{ display: flex; justify-content: space-between; align-items: baseline; flex-wrap: wrap; gap: 8px; }}
  #overview-footer {{ color: var(--ed-text-muted); font-size: 12px; }}
  /* Narrow screens: shrink the 60px gutters so content isn't cramped */
  @media (max-width: 640px) {{
    .header {{ padding: 32px 20px 24px; }}
    .section {{ padding: 32px 20px; }}
    #page-nav {{ padding: 12px 20px 0; }}
  }}
  /* ═══════ Redesign ═══════ */
  /* Masthead — no curated verdict strip, just what this run holds. */
  .masthead {{ padding: 48px 60px 22px; max-width: 1600px; margin: 0 auto; border-bottom: 1px solid var(--ed-border); }}
  .masthead .mh-eyebrow {{ font-family: var(--ed-font-mono); font-size: 11px; color: var(--ed-accent); letter-spacing: 2px; text-transform: uppercase; }}
  .masthead h1 {{ font-family: var(--ed-font-display); font-weight: 800; font-size: 34px; letter-spacing: -0.5px; margin: 6px 0 10px; }}
  .masthead h1 .mh-dim {{ color: rgba(232,232,236,0.38); }}
  .mh-sub {{ font-family: var(--ed-font-mono); font-size: 12px; color: var(--ed-text-secondary); line-height: 1.7; max-width: 1000px; }}
  .mh-prov {{ font-family: var(--ed-font-mono); font-size: 11px; color: var(--ed-text-muted); line-height: 1.7; margin-top: 6px; }}
  .mh-link {{ color: var(--ed-accent); text-decoration: none; }}
  .mh-controls {{ display: flex; align-items: center; gap: 12px; flex-wrap: wrap; margin-top: 16px; font-family: var(--ed-font-mono); font-size: 13px; }}
  .shownum {{ display: inline-flex; align-items: center; gap: 6px; color: var(--ed-text-secondary); cursor: pointer; }}
  .mh-toggle {{ background: var(--ed-input-bg); color: var(--ed-text-secondary); border: 1px solid var(--ed-input-border); border-radius: var(--ed-radius-sm); padding: 5px 10px; cursor: pointer; font-family: var(--ed-font-mono); font-size: 12px; }}
  .mh-toggle:hover, .mh-toggle.active {{ color: var(--ed-accent); border-color: var(--ed-accent); }}
  .mh-toggle.active {{ background: var(--ed-surface-raised); }}

  /* Layout — sticky left rail + single-scroll content column. */
  .layout-wrap {{ display: flex; gap: 0; max-width: 1600px; margin: 0 auto; align-items: flex-start; }}
  .rail {{ flex: 0 0 210px; position: sticky; top: 0; align-self: flex-start; max-height: 100vh; overflow-y: auto; padding: 24px 14px 24px 24px; font-family: var(--ed-font-mono); font-size: 11px; border-right: 1px solid var(--ed-border); }}
  .rail .rail-part {{ color: var(--ed-text-muted); letter-spacing: 1px; text-transform: uppercase; font-size: 11px; margin: 16px 0 6px; }}
  .rail .rail-part:first-child {{ margin-top: 0; }}
  .rail a {{ display: block; color: var(--ed-text-secondary); text-decoration: none; padding: 3px 8px; border-left: 2px solid transparent; line-height: 1.5; }}
  .rail a:hover {{ color: var(--ed-text-primary); }}
  .rail a.active {{ color: var(--ed-accent); border-left-color: var(--ed-accent); background: var(--ed-surface); }}
  .content {{ flex: 1 1 auto; min-width: 0; display: flex; flex-direction: column; }}
  .content .section {{ max-width: none; margin: 0; }}
  /* Visual order: parts 1→4 in sequence, the cost section inside Part 1, the
     STM cross-validation and six-arm study in Part 3 / appendix, the run
     summary, external tools and provenance in the appendix, so the page
     opens on Part 1. Reordered by flex `order` so no section div is
     physically moved (and the scroll-spy still tracks). */
  #part1 {{ order: 10; }} #part1-bar {{ order: 11; }} #s02 {{ order: 12; }} #s03 {{ order: 13; }} #s-t1-prop {{ order: 14; }} #s06 {{ order: 15; }} #s06b {{ order: 16; }} #s-t2-eph {{ order: 17; }} #s07 {{ order: 18; }} #s08b {{ order: 19; }} #s-t3-od {{ order: 20; }}
  #part2 {{ order: 21; }} #s-ranking {{ order: 22; }} #s-timing {{ order: 23; }}
  #part3 {{ order: 30; }} #s10 {{ order: 31; }} #s05 {{ order: 32; }} #s09b {{ order: 33; }}
  #part4 {{ order: 40; }} #s12 {{ order: 41; }}
  #appendix {{ order: 49; }} #s01 {{ order: 50; }} #s01b {{ order: 51; }} #s05c {{ order: 52; }} #s13 {{ order: 53; }}
  @media (max-width: 1100px) {{ .rail {{ display: none; }} .masthead {{ padding: 32px 20px 18px; }} }}

  .part-head {{ font-family: var(--ed-font-display); font-weight: 700; font-size: 13px; letter-spacing: 2px; text-transform: uppercase; color: var(--ed-accent); padding: 26px 60px 0; max-width: 1400px; margin: 0 auto; }}

  /* ── Part 1 compare bar (owner pass 18: "make the comparison tool selection
     more obvious for section 1") ─────────────────────────────────────────────
     The Tool / Reference selects live here, a flex-ordered sibling of the Part 1
     sections in the content column, so the reader always sees which pair Part 1
     compares and the bar never overlaps the left rail. position:sticky pins it to
     the viewport top; one passive scroll listener releases it at the Part 2 head
     (.p1b-released) and sets --sticky-offset to its height while pinned so the
     heatmap headers stack directly below it. --bar-h (the bar's height) drives the
     Part 1 sections' scroll-margin so a rail click / deep link clears the bar.
     Both custom properties default here so a jump before the script runs still
     clears the bar. Reads as chrome (page surface + a bottom rule). */
  :root {{ --bar-h: 40px; --sticky-offset: 0px; }}
  /* The bar is a full-bleed opaque strip spanning the whole content column, so it
     masks the entire sticky band (0 .. --sticky-offset) and no heatmap row shows
     above its own column header on either side; the controls sit in a centred
     inner row at the section content width (owner pass 18). Raised surface plus a
     shadow only while pinned read it as an elevated toolbar, not page chrome. */
  .part1-bar {{ position: sticky; top: 0; z-index: 30; background: var(--ed-surface-raised); border-bottom: 1px solid var(--ed-border); }}
  .part1-bar.p1b-released {{ position: static; }}
  .part1-bar.p1b-pinned {{ box-shadow: 0 2px 6px rgba(0, 0, 0, 0.18); }}
  .p1b-inner {{ display: flex; align-items: center; gap: 12px; min-height: 40px; padding: 5px 60px; max-width: 1400px; margin: 0 auto; font-family: var(--ed-font-mono); font-size: 12px; }}
  .part1-bar .p1b-label {{ color: var(--ed-text-muted); letter-spacing: 1px; text-transform: uppercase; font-size: 11px; }}
  .part1-bar .p1b-vs {{ color: var(--ed-text-muted); }}
  .part1-bar select {{ background: var(--ed-input-bg); color: var(--ed-accent); border: 1px solid var(--ed-input-border); border-radius: var(--ed-radius-sm); padding: 6px 12px; font-family: var(--ed-font-mono); font-size: 14px; font-weight: 700; cursor: pointer; }}
  .part1-bar select:focus-visible {{ outline: none; border-color: var(--ed-accent); box-shadow: 0 0 0 2px var(--ed-focus-ring); }}
  @media (max-width: 1100px) {{ .p1b-inner {{ padding-left: 20px; padding-right: 20px; }} }}
  /* A rail click / deep link to a Part 1 section must land below the pinned bar. */
  #s02, #s03, #s-t1-prop, #s06, #s06b, #s-t2-eph, #s07, #s08b, #s-t3-od {{ scroll-margin-top: var(--bar-h); }}
  /* One pair chip per Part 1 section title, naming the selected pair in the two
     tools' colours; the same colour language as the compare-bar selects. Its
     `pair-chip` class scopes its words as a chip (not title words) for the audit. */
  .pair-chip {{ display: inline-flex; align-items: baseline; gap: 5px; margin-left: 10px; padding: 2px 9px; border: 1px solid var(--ed-border); border-radius: 999px; background: var(--ed-surface); font-family: var(--ed-font-mono); font-size: 11px; font-weight: 400; letter-spacing: 0.3px; text-transform: none; vertical-align: middle; }}
  .pair-chip .pc-vs {{ color: var(--ed-text-muted); }}
  /* Per-tool text colours as theme-aware tokens: a tool NAME shown as text (the
     compare-bar selects and the section-title pair chips) reads its --tool-<slug>
     token so it clears WCAG AA (>= 4.5:1) on the card/input surface in BOTH themes.
     The light values are darkened exactly as --ed-accent is. One source, below. */
{tool_tokens_css}

  .section-q {{ font-family: var(--ed-font-body); font-size: 14px; color: var(--ed-text-secondary); line-height: 1.6; max-width: 900px; margin-bottom: 14px; }}
  .disclosures {{ margin-top: 12px; font-family: var(--ed-font-mono); font-size: 11px; }}
  .disclosures summary {{ color: var(--ed-accent); cursor: pointer; }}
  .disclosures .disc-body {{ color: var(--ed-text-secondary); line-height: 1.7; max-width: 900px; margin-top: 8px; }}
  .refs {{ color: var(--ed-text-secondary); line-height: 1.6; max-width: 900px; }}
  .refs .ref-head {{ margin-bottom: 4px; color: var(--ed-accent); cursor: pointer; }}
  .refs .ref {{ margin-top: 3px; }}

  /* Agreement grid (H1/H2, G1..G3) — one shared component. H1/H2 render one
     table PER TOOL (class `ahgrid`), one row per object, with a per-population
     median header row. Every value shows in-cell by default (`.shownums` on the
     body); the checkbox turns them off. Nothing below 11px. */
  .grid-summary {{ font-family: var(--ed-font-mono); font-size: 12px; color: var(--ed-text-primary); margin: 4px 0 10px; }}
  .pair-caveat {{ font-family: var(--ed-font-mono); font-size: 11px; color: var(--ed-text-secondary); margin: 0 0 10px; }}
  .grid-key {{ display: flex; flex-wrap: wrap; gap: 10px 16px; font-family: var(--ed-font-mono); font-size: 11px; color: var(--ed-text-secondary); margin-bottom: 12px; }}
  .grid-key .gk {{ display: inline-flex; align-items: center; gap: 5px; }}
  .grid-key .gk-sw {{ display: inline-block; width: 14px; height: 12px; border-radius: 2px; }}
  .grid-tool-head {{ font-family: var(--ed-font-display); font-weight: 700; font-size: 13px; color: var(--ed-accent); letter-spacing: 0.5px; margin: 20px 0 6px; }}
  .grid-tool-note {{ font-family: var(--ed-font-mono); font-size: 12px; color: var(--ed-text-muted); margin: 2px 0 10px; }}
  .pair-notcompared {{ font-family: var(--ed-font-mono); font-size: 12px; color: var(--ed-text-secondary); padding: 10px 12px; margin: 4px 0 12px; border: 1px solid var(--ed-border); border-radius: var(--ed-radius-sm); background: var(--ed-hatch); }}
  .grid-scroll {{ overflow-x: auto; max-width: 100%; padding-bottom: 8px; }}
  /* H1/H2 tables reclaim the section's side padding so all 17 horizons fit
     inside the content column at 1280/1440; the bottom scrollbar stays visible
     and the fade at the right edge cues any residual overflow. */
  .agrid-scroll {{ margin-left: -32px; margin-right: -32px; padding-left: 32px; scrollbar-color: var(--ed-scrollbar-thumb) var(--ed-surface); }}
  .agrid-scroll::-webkit-scrollbar {{ height: 10px; }}
  .agrid-scroll::-webkit-scrollbar-track {{ background: var(--ed-surface); border-radius: 5px; }}
  .agrid-scroll::-webkit-scrollbar-thumb {{ background: var(--ed-scrollbar-thumb); border-radius: 5px; }}
  .agrid-scroll::-webkit-scrollbar-thumb:hover {{ background: var(--ed-scrollbar-hover); }}
  /* The header row is declared position:sticky, but a scroll wrapper with its own
     overflow captures the sticky context so it never reaches the viewport. At
     widths where the narrow, tall tables fit (≥1360px) drop the wrapper's
     overflow so the header sticks to the viewport top while the body scrolls;
     below that width the wrapper keeps scrolling horizontally. Scoped to the tall
     families: agreement (.agrid-scroll), OD closeness (.odgrid), the Part 2
     per-object heatmaps (.r3grid), and the timing / cost grids (T1/T2/G* and the
     Section 1.3 OD cost pairs) via the .grid-sticky marker. The wide fidelity
     grids keep the plain .grid-scroll wrapper and scroll horizontally. */
  @media (min-width: 1360px) {{
    .agrid-scroll,
    .grid-scroll:has(.odgrid),
    .grid-scroll:has(.r3grid),
    .grid-sticky {{ overflow: visible; }}
  }}
  /* Row + column scanning aids for the heatmap family. The row aid is pure CSS;
     the column aid is toggled by one delegated listener per table (wireHeatmapCrosshair). */
  table.agrid tbody tr:hover td.gobjname2,
  table.agrid tbody tr:hover td.gpopname,
  table.agrid tbody tr:hover td.gtool {{ background: var(--ed-surface); }}
  table.agrid tbody tr:hover td.gc {{ box-shadow: inset 0 1px 0 var(--ed-text-secondary), inset 0 -1px 0 var(--ed-text-secondary); }}
  table.agrid .gc.col-hi, table.agrid .gdt.col-hi {{ box-shadow: inset 1px 0 0 var(--ed-text-secondary), inset -1px 0 0 var(--ed-text-secondary); }}
  table.agrid tbody tr:hover td.gc.col-hi {{ box-shadow: inset 0 1px 0 var(--ed-text-primary), inset 0 -1px 0 var(--ed-text-primary), inset 1px 0 0 var(--ed-text-primary), inset -1px 0 0 var(--ed-text-primary); }}
  table.agrid {{ border-collapse: collapse; font-family: var(--ed-font-mono); font-size: 11px; }}
  /* H1/H2 fixed layout: the object column, the 17 horizon cells and the worst
     column are sized to fit the content width (design §3.3). */
  table.ahgrid {{ table-layout: fixed; width: 100%; }}
  table.ahgrid th.gobj {{ width: 150px; }}
  table.ahgrid th.gdt {{ width: 40px; }}
  table.ahgrid th.gworst {{ width: 76px; }}
  /* The heatmap-family column header pins to the viewport top, offset by the
     Part 1 compare bar while that bar is pinned (--sticky-offset, set by the bar's
     scroll listener; 0 elsewhere) so the header stacks directly below it. This is
     the shared sticky-header rule, live for the families that reach the viewport:
     agreement (h1-prop/h2-eph), OD closeness and the Part 2 grids. table.heatmap th
     carries the same offset for uniformity, but a table wrapped in .heatmap-container
     (e.g. the s07 outliers) scrolls inside that wrapper, so its header pins to the
     wrapper, not below the bar — the offset is inert there. */
  table.agrid th {{ padding: 5px 4px; color: var(--ed-text-secondary); font-weight: 400; letter-spacing: 0.3px; font-size: 11px; border-bottom: 1px solid var(--ed-border); position: sticky; top: var(--sticky-offset, 0px); background: var(--ed-bg); z-index: 2; }}
  table.agrid th.gdt {{ text-align: center; min-width: 40px; }}
  table.agrid th.gt0h {{ color: var(--ed-text-muted); border-left: 3px solid var(--ed-border); }}
  table.agrid th.gobj {{ text-align: left; position: sticky; left: 0; z-index: 3; background: var(--ed-bg); }}
  table.agrid th.gworst {{ position: sticky; right: 0; background: var(--ed-bg); z-index: 3; text-align: left; min-width: 76px; }}
  .agrid td {{ padding: 0; border-bottom: 1px solid var(--ed-border-subtle); }}
  .agrid td.gtool {{ text-align: left; color: var(--ed-text-primary); font-size: 12px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; padding: 4px 12px 4px 16px; position: sticky; left: 0; background: var(--ed-bg); z-index: 1; }}
  .agrid tr.gsummary td.gtool {{ color: var(--ed-text-primary); }}
  /* Per-population median header row (H1/H2). */
  .agrid tr.gpophdr td.gpopname {{ font-family: var(--ed-font-display); font-weight: 700; font-size: 12px; color: var(--ed-text-primary); text-transform: uppercase; letter-spacing: 0.5px; padding: 6px 10px 6px 8px; position: sticky; left: 0; background: var(--ed-bg); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }}
  .agrid tr.gpophdr td {{ border-top: 2px solid var(--ed-border); }}
  .agrid tr.gobjr td.gobjname2 {{ text-align: left; color: var(--ed-text-primary); font-size: 12px; padding: 3px 10px 3px 16px; position: sticky; left: 0; background: var(--ed-bg); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }}
  /* G1..G3 timing grids still use the block/tool row shape. */
  .agrid tr.gblock td.gblock-l {{ font-family: var(--ed-font-display); font-weight: 700; font-size: 12px; color: var(--ed-text-primary); text-transform: uppercase; letter-spacing: 0.5px; padding: 14px 12px 4px 8px; position: sticky; left: 0; background: var(--ed-bg); white-space: nowrap; }}
  .agrid tr.gblock td.gblock-fill {{ background: var(--ed-bg); border-bottom: 1px solid var(--ed-border); }}
  .agrid td.gobjname, .agrid td.gobjname2 {{ max-width: 150px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }}
  .agrid tr.gobjrow td.gobjname {{ text-align: left; color: var(--ed-text-primary); font-size: 12px; padding: 8px 12px 2px 12px; position: sticky; left: 0; background: var(--ed-bg); white-space: nowrap; }}
  .agrid td.gc {{ width: 40px; height: 18px; text-align: center; position: relative; }}
  .agrid td, .agrid th {{ box-sizing: border-box; padding-left: 2px; padding-right: 2px; }}
  .agrid td.gc .cn {{ display: none; font-size: 11px; }}
  /* cn-sig is the per-cell σ span carried by the frozen agreement markup; the σ
     view that would reveal it is disabled (see the masthead control), so it
     stays hidden in every state. If it is ever re-enabled, its ink MUST be driven
     from LADDER_DARK / LADDER_LIGHT alongside .cn (see ladder_css): the fixed
     light ink it inherits today fails contrast on the light-ink-flip bins. */
  .agrid td.gc .cn-sig {{ display: none; font-size: 11px; }}
  body.shownums .agrid td.gc .cn {{ display: inline; }}
  .agrid td.gc.gt0, .agrid th.gt0h {{ }}
  .agrid td.gt0 {{ border-left: 3px solid var(--ed-border); background-image: linear-gradient(rgba(120,128,150,0.14), rgba(120,128,150,0.14)); }}
  .agrid td.gc.rw::after {{ content: ""; position: absolute; top: 0; right: 0; border-width: 0 6px 6px 0; border-style: solid; border-color: transparent var(--ed-text-primary) transparent transparent; }}
  .agrid td.gc.mg::before {{ content: "\2020"; position: absolute; left: 2px; top: 0; font-size: 11px; color: rgba(255,255,255,0.85); }}
  .agrid td.arcends {{ text-align: center; color: var(--ed-text-muted); font-size: 11px; letter-spacing: 1px; background: repeating-linear-gradient(90deg, transparent, transparent 6px, var(--ed-border-subtle) 6px, var(--ed-border-subtle) 7px); border-top: 1px solid var(--ed-border); border-bottom: 1px solid var(--ed-border); }}
  .agrid td.gworst {{ position: sticky; right: 0; background: var(--ed-bg); white-space: nowrap; padding: 2px 8px; z-index: 1; }}
  .agrid td.gworst .wdot {{ display: inline-block; width: 10px; height: 10px; border-radius: 2px; margin-right: 5px; vertical-align: middle; }}
  .agrid td.gworst .wnum {{ font-size: 11px; color: var(--ed-text-primary); }}
  .agrid tr.gfoot td {{ border-top: 1px solid var(--ed-border); }}
  .agrid tr.gfoot td.gtool {{ color: var(--ed-text-secondary); font-size: 11px; white-space: normal; }}
  .agrid td.gfc {{ text-align: center; color: var(--ed-text-primary); font-size: 11px; padding: 4px 2px; }}
  .tag-gap {{ font-size: 11px; color: var(--ed-text-muted); margin-left: 6px; }}
  .tag-gap {{ color: #d8a24a; }}

  /* OD closeness grid (H3) — shape = state, fill = sigma_equiv. */
  table.odgrid th.oc-h {{ text-align: center; min-width: 64px; }}
  table.odgrid th.od-d {{ text-align: right; min-width: 56px; color: var(--ed-text-muted); }}
  .odgrid td.oc {{ width: 64px; text-align: center; position: relative; height: 18px; }}
  /* Glyph size only; its per-bin ink follows the one ladder (see ladder_css). */
  .odgrid td.oc .ocg {{ font-size: 12px; }}
  .odgrid td.oc .cn {{ display: none; }}
  body.shownums .odgrid td.oc .cn {{ display: inline; font-size: 11px; }}
  .odgrid td.oc.nocov {{ background: var(--ed-surface); }}
  .odgrid td.oc.nocov .ocg {{ color: var(--ed-text-muted); }}
  .odgrid td.oc.undef {{ background: rgba(139,145,152,0.14); }}
  .odgrid td.oc.undef .ocg {{ color: var(--ed-text-secondary); }}
  .odgrid td.oc.fail {{ background: rgba(212,85,58,0.14); }}
  .odgrid td.oc.fail .ocg {{ color: #e0654a; font-weight: 700; }}
  .odgrid td.oc.rw::after {{ content: ""; position: absolute; top: 0; right: 0; border-width: 0 6px 6px 0; border-style: solid; border-color: transparent var(--ed-text-primary) transparent transparent; }}
  .odgrid td.oc.jplref {{ color: var(--ed-text-muted); font-size: 11px; background: var(--ed-hatch); }}
  .odgrid td.od-optonly {{ text-align: center; color: var(--ed-text-muted); font-size: 11px; background: var(--ed-hatch); }}
  .odgrid td.od-d {{ text-align: right; color: var(--ed-text-secondary); font-size: 11px; padding: 2px 8px; white-space: nowrap; }}
  .odgrid tr.odradar td {{ opacity: 0.85; }}
  .odgrid tr.odradar td.gtool {{ padding-left: 30px; color: var(--ed-text-muted); }}
  .od-fail-name {{ color: #e0654a; }}
  .od-fails {{ margin-top: 16px; font-family: var(--ed-font-mono); font-size: 12px; }}
  .od-fails-h {{ color: #e0654a; letter-spacing: 1px; text-transform: uppercase; font-size: 12px; margin-bottom: 6px; }}
  .od-fails-t td {{ padding: 3px 14px 3px 0; color: var(--ed-text-secondary); font-size: 11px; }}

  /* H4 channel fidelity — four states. */
  table.fidgrid th.fid-h {{ text-align: center; min-width: 150px; font-size: 12px; }}
  table.fidgrid th.fo-h {{ text-align: center; min-width: 56px; font-size: 11px; line-height: 1.2; }}
  .fidgrid td.fc {{ text-align: center; font-size: 12px; padding: 6px 8px; line-height: 1.4; }}
  .fidgrid td.fc .fc-sub {{ font-size: 11px; opacity: 0.8; }}
  .fidgrid td.fc .fc-obj {{ font-size: 11px; }}
  .fidgrid td.fo {{ text-align: center; font-size: 11px; padding: 3px 2px; }}
  .st-exact {{ background: #1b2a38; color: #cfe0ee; }}
  .st-ulp {{ background: #2c5a7d; color: #eaf2f8; }}
  .st-tol {{ background: #7a5a1e; color: #f4e6c8; }}
  .st-diff {{ background: #7a2a1e; color: #f4d2c8; }}
  .grid-key .gk-sw.st-exact {{ background: #1b2a38; }}
  .grid-key .gk-sw.st-ulp {{ background: #2c5a7d; }}
  .grid-key .gk-sw.st-tol {{ background: #7a5a1e; }}
  .grid-key .gk-sw.st-diff {{ background: #7a2a1e; }}

  /* Part 2 matrices — each states a tool's own numbers on the one decade
     ladder (design §3.1); no cell ranks tools or names a winner. */
  table.r1grid th, table.r2grid th, table.r3grid th {{ text-align: center; min-width: 72px; vertical-align: bottom; font-size: 12px; padding: 5px 6px; font-weight: 400; letter-spacing: 0.3px; }}
  table.r1grid th.gobj, table.r2grid th.gobj, table.r3grid th.gobj {{ text-align: left; width: 200px; }}
  .r1grid td.r1c {{ text-align: center; font-size: 12px; padding: 5px 8px; color: var(--ed-text-primary); vertical-align: middle; }}
  .r1sub {{ font-size: 11px; color: var(--ed-text-muted); font-weight: 400; text-transform: none; letter-spacing: 0; }}
  .r1ref {{ font-size: 11px; color: var(--ed-accent); }}
  .r1line {{ display: flex; align-items: center; gap: 4px; justify-content: center; padding: 1px 0; }}
  .r1tag {{ min-width: 52px; text-align: right; }}
  /* A small ladder chip: one number on its decade colour, used where a cell
     must carry two numbers (p50 and p90) side by side. Same bins and formatters
     as the grid cells. */
  /* .lchip shares the ladder ink with .cn and .ocg — see ladder_css. */
  .lchip {{ display: inline-block; min-width: 30px; padding: 2px 6px; border-radius: 3px; font-family: var(--ed-font-mono); font-size: 11px; line-height: 1.35; text-align: center; }}
  /* Count / secondary line inside a Part 2 cell — inherits the cell's ink. */
  /* Secondary count line: no opacity — it dropped the count below AA on the
     mid-ladder tints; it inherits the cell's (now AA-clearing) ink. */
  .cn-n {{ font-size: 10px; font-weight: 400; }}
  /* Part 2 numbers are the point of these matrices: always shown, regardless of
     the global "show numbers" checkbox. */
  .r2grid td.gc .cn, .r3grid td.gc .cn {{ display: inline !important; }}
  .r2grid td.gc, .r3grid td.gc {{ width: auto; min-width: 66px; height: auto; padding: 4px 6px; text-align: center; }}
  .r2grid td.gtool, .r3grid td.gtool {{ font-size: 12px; }}
  .grid-summary.reading {{ color: var(--ed-text-secondary); font-size: 11px; line-height: 1.6; margin: 6px 0 22px; max-width: 920px; }}

  /* Growth small multiples (§1.1B). */
  .small-multiples {{ display: grid; grid-template-columns: repeat(auto-fill, minmax(320px, 1fr)); gap: 8px; }}
  .sm-panel {{ background: var(--ed-surface); border: 1px solid var(--ed-border); border-radius: var(--ed-radius-sm); min-height: 190px; }}
  @media (max-width: 900px) {{ .small-multiples {{ grid-template-columns: 1fr; }} }}
  .cov-panels {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(360px, 1fr)); gap: 10px; margin-bottom: 16px; }}

  /* The ONE 45° hatch for every unavailable / excluded / not-compared state —
     one definition (a custom property), used everywhere (grids, keys, the
     not-compared strip, the OD optical-only / radar-JPL cells). Dark uses an
     α0.5 stripe over the dark ground (2.36:1). The light ground is near white, so
     that same α0.5 stripe reads at only 1.64:1 — below the 3:1 graphical-object
     floor, risking a "blank cell" read; light theme therefore overrides the token
     with an opaque, darker stripe (#7e858e, ≥3:1 on every light surface), keeping
     one token and one geometry so a marked cell reads as marked in both themes. */
  :root {{ --ed-hatch: repeating-linear-gradient(45deg, transparent, transparent 3px, rgba(139,145,152,0.5) 3px, rgba(139,145,152,0.5) 4.5px); }}
  :root.theme-light {{ --ed-hatch: repeating-linear-gradient(45deg, transparent, transparent 3px, #7e858e 3px, #7e858e 4.5px); }}
  .hatch {{ background: var(--ed-hatch); }}

  /* The one colour ladder — seven decade bins, one hue, monotonic in lightness
     (design §3.1). Backgrounds AND the per-bin ink (cell, chip, OD glyph) for
     both themes are generated from the LADDER_DARK / LADDER_LIGHT single source
     so the flip lives in one place and every bin clears AA in both themes. */
{ladder_css}
  .grid-key .gk-sw.rw {{ background: #22405a; position: relative; }}
  .grid-key .gk-sw.rw::after {{ content: ""; position: absolute; top: 0; right: 0; border-width: 0 5px 5px 0; border-style: solid; border-color: transparent var(--ed-text-primary) transparent transparent; }}

  /* ── Light theme (owner-ruled yes; measured light ladder, design §3.1) ──
     Applied via a `theme-light` class on :root, set from prefers-color-scheme
     on load and flipped by the masthead toggle — one block, no media-query
     duplication. */
  :root.theme-light {{
    --ed-bg: #f6f8fb; --ed-surface: #edf1f6; --ed-surface-raised: #e2e8f0;
    --ed-border: #d3dbe4; --ed-border-subtle: #e7ecf2;
    --ed-text-primary: #101922; --ed-text-secondary: #404b58; --ed-text-muted: #616c77;
    /* Accent and muted are raised to clear AA (4.5:1) as body text on the light
       page: #2f6a9e = 5.37:1, #616c77 = 5.04:1 against --ed-bg #f6f8fb. The dark
       theme's own accent/muted already pass and are unchanged. */
    --ed-accent: #2f6a9e; --ed-accent-hover: #3d7ab8; --ed-accent-pressed: #24537c;
    --ed-input-bg: #ffffff; --ed-input-border: #cdd7e1;
    --ed-scrollbar-thumb: #c3ccd8; --ed-scrollbar-hover: #a8b4c2;
    --ed-chart-grid: rgba(40,80,130,0.14);
  }}
  :root.theme-light .masthead h1 .mh-dim {{ color: rgba(16,25,34,0.42); }}
  /* Light-theme ladder backgrounds and ink are generated with the dark ones (see
     the {{ladder_css}} block above); no per-bin light overrides live here. */
  :root.theme-light .gc.mg::before {{ color: rgba(0,0,0,0.72); }}
  :root.theme-light .st-exact {{ background: #eef4f9; color: #1c4d7a; }}
  :root.theme-light .st-ulp {{ background: #aecde4; color: #12354f; }}
  :root.theme-light .st-tol {{ background: #e6d3a0; color: #5a4410; }}
  :root.theme-light .st-diff {{ background: #e8b3a8; color: #6b241a; }}
  :root.theme-light .grid-key .gk-sw.st-exact {{ background: #eef4f9; }}
  :root.theme-light .grid-key .gk-sw.st-ulp {{ background: #aecde4; }}
  :root.theme-light .grid-key .gk-sw.st-tol {{ background: #e6d3a0; }}
  :root.theme-light .grid-key .gk-sw.st-diff {{ background: #e8b3a8; }}
  :root.theme-light .grid-key .gk-sw.rw {{ background: #d3e3f0; }}
</style>
</head>
<body class="shownums">

<div class="masthead">
  <div class="mh-eyebrow">Validation Report</div>
  <h1>EMPYREAN <span class="mh-dim">DYNAMICS</span></h1>
  <div class="mh-sub" data-requires="empyrean" title="Rust safe-FFI channel under test. The comparison pair is set in the Part 1 compare bar and named on each Part 1 section chip; run counts and configuration are in the appendix run summary and provenance.">Rust channel under test.</div>
  <div class="mh-prov"><a href="#s13" class="mh-link" title="provenance &amp; references">provenance</a> &nbsp;·&nbsp; <a href="{dataset_file_name}" download class="mh-link" title="download dataset JSON">dataset</a></div>
  <div class="mh-controls">
    <label class="shownum"><input type="checkbox" id="show-numbers" checked> show numbers</label>
    <button id="theme-toggle" class="mh-toggle" title="Toggle light / dark theme" aria-label="Toggle light or dark theme">◐ theme</button>
    <button id="sigma-toggle" class="mh-toggle" disabled style="opacity:0.4;cursor:not-allowed" title="σ view unavailable in this build: a coherent Mahalanobis view needs per-object σ at full precision in the key, worst column, summary line and every hover, which the agreement heatmap does not yet carry.">◎ σ view</button>
  </div>
</div>

<div class="layout-wrap">
<nav class="rail" id="rail" aria-label="Contents">
  <div class="rail-part">{part1_name}</div>
  <a data-target="s02" href="#s02">1.1 Propagation</a>
  <a data-target="s03" href="#s03">&nbsp;&nbsp;· error growth</a>
  <a data-target="s-t1-prop" href="#s-t1-prop">&nbsp;&nbsp;· cost</a>
  <a data-target="s06" href="#s06">1.2 Ephemeris</a>
  <a data-target="s06b" href="#s06b">&nbsp;&nbsp;· error growth</a>
  <a data-target="s-t2-eph" href="#s-t2-eph">&nbsp;&nbsp;· cost</a>
  <a data-target="s07" href="#s07">&nbsp;&nbsp;· outliers</a>
  <a data-target="s08b" href="#s08b">1.3 Orbit determination</a>
  <a data-target="s-t3-od" href="#s-t3-od">&nbsp;&nbsp;· cost</a>
  <div class="rail-part">{part2_name}</div>
  <a data-target="s-ranking" href="#s-ranking">2.1 Side by side</a>
  <a data-target="s-timing" href="#s-timing">2.2 Wall clock</a>
  <div class="rail-part">{part3_name}</div>
  <a data-target="s10" href="#s10">3.1 Channel fidelity</a>
  <a data-target="s05" href="#s05">3.2 Derivatives (STM)</a>
  <a data-target="s09b" href="#s09b">3.3 Non-grav recovery</a>
  <div class="rail-part">{part4_name}</div>
  <a data-target="s12" href="#s12">4.1 Covariance realism</a>
  <div class="rail-part">{appendix_name}</div>
  <a data-target="s01" href="#s01">Run summary</a>
  <a data-target="s01b" href="#s01b">External tools</a>
  <a data-target="s05c" href="#s05c">ASSIST configurations</a>
  <a data-target="s13" href="#s13">Provenance</a>
</nav>
<main class="content">

<div class="part-head" id="appendix">{appendix_name}</div>

<div class="section" id="s01">
  <div class="section-num">A.1</div>
  <div class="section-title" title="What this run holds">Run summary</div>
  <div class="summary-grid">
    <div class="summary-card"><div class="value">{n_objects}</div><div class="label">Objects</div></div>
    <div class="summary-card"><div class="value">{n_populations}</div><div class="label">Populations</div></div>
    <div class="summary-card"><div class="value">{n_dt}</div><div class="label">Time Offsets</div></div>
    <div class="summary-card"><div class="value">{n_prop}</div><div class="label">Propagation Cases</div></div>
    <div class="summary-card"><div class="value">{n_eph}</div><div class="label">Ephemeris Cases</div></div>
    <div class="summary-card"><div class="value">{n_od}</div><div class="label">OD Cases</div></div>
    <div class="summary-card"><div class="value">{n_channels}</div><div class="label">Channels</div></div>
  </div>
  <div class="heatmap-container">
  <table class="od-table" style="max-width:720px; margin-bottom:6px;">
    <thead><tr><th style="text-align:left">Component</th><th style="text-align:left">Purpose</th><th>Version</th></tr></thead>
    <tbody>
      <tr><td style="text-align:left">hyperjet</td><td style="text-align:left">Automatic differentiation &mdash; STMs / STTs</td><td>1.15.0</td></tr>
      <tr><td style="text-align:left">empyrean-core</td><td style="text-align:left">Reference channel (<code>validate-core</code>)</td><td>0.10.2</td></tr>
      <tr><td style="text-align:left">empyrean</td><td style="text-align:left">Distribution under test &mdash; Rust wrapper, C ABI, Python wheel, CLI</td><td>0.10.0</td></tr>
      <tr><td style="text-align:left">empyrean-py</td><td style="text-align:left">Python wheel</td><td>0.10.0</td></tr>
      <tr><td style="text-align:left">empyrean-cli</td><td style="text-align:left">Command-line interface</td><td>0.10.0</td></tr>
      <tr><td style="text-align:left">C ABI</td><td style="text-align:left"><code>EMPYREAN_ABI_VERSION</code> &mdash; equality-checked at load</td><td>1000</td></tr>
    </tbody>
  </table>
  </div>
  <div class="legend">
{pop_legend}  </div>
  <div class="legend">
{channel_legend}  </div>
</div>
<div class="section" id="s01b">
  <div class="section-num">A.2</div>
  <div class="section-title" title="External reference tools — transparency">External tools</div>
  <div class="disclosures"><details><summary>{disc_summary}</summary><div class="disc-body">Every external comparison is driven by a runner script in <a href="https://github.com/Empyrean-Dynamics/empyrean-validation" target="_blank" rel="noopener">empyrean-validation</a> holding that tool's full invocation &mdash; settings, force model, weighting, rejection &mdash; so any panel number reproduces. The tools are external dependencies, not linked into empyrean: run independently, their JSON outputs merged at the comparison step.</div></details></div>
  <div class="heatmap-container">
  <table class="od-table" style="margin-top:0.5em">
    <thead><tr>
      <th style="text-align:left">Tool</th>
      <th style="text-align:left">Reference</th>
      <th style="text-align:left">Our runner</th>
    </tr></thead>
    <tbody>
      <tr>
        <td><b title="Used for: propagation & ephemeris truth (Horizons); orbit determination — JPL's reported fit quality (normalized rms → reduced χ², n_obs, radar, arc) + fitted orbit & covariance (SBDB)">JPL</b></td>
        <td>NASA JPL SSD · Horizons + SBDB (one solution)</td>
        <td><code>plan</code> step &mdash; queried, not a runner (SBDB + Horizons disk cache)</td>
      </tr>
      <tr>
        <td><b title="Used for: N-body propagation; first-order STM via 6 variational particles (first order only).">ASSIST</b></td>
        <td>Holman et al. 2023 · REBOUND IAS15 · DE440</td>
        <td><a href="https://github.com/Empyrean-Dynamics/empyrean-validation/blob/main/runners/assist/run_assist.py" target="_blank" rel="noopener"><code>runners/assist/run_assist.py</code></a></td>
      </tr>
      <tr>
        <td><b title="Used for: orbit determination from ADES; reference for reduced χ² (Veres-2017 weighting)">layup</b></td>
        <td>Holman / Smithsonian · MIT · ASSIST-backed</td>
        <td><a href="https://github.com/Empyrean-Dynamics/empyrean-validation/blob/main/runners/layup/run_layup.py" target="_blank" rel="noopener"><code>runners/layup/run_layup.py</code></a></td>
      </tr>
      <tr>
        <td><b title="Used for: orbit determination from ADES astrometry (post-fit RMS reference); its fitted orbit is then propagated by find_orb itself to the plan's epochs for propagation + sky-plane comparison (fit-then-propagate — unlike ASSIST / OpenOrb, which replay the plan's initial conditions, so these diffs include the fit-vs-JPL-orbit difference)">find_orb</b></td>
        <td>Gray (Project Pluto) · MPC-grade OD</td>
        <td><a href="https://github.com/Empyrean-Dynamics/empyrean-validation/blob/main/runners/findorb/run_findorb.py" target="_blank" rel="noopener"><code>runners/findorb/run_findorb.py</code></a></td>
      </tr>
      <tr>
        <td><b title="Used for: propagation, ephemeris and orbit determination — the only external reference covering all three axes, and with find_orb one of only two that ingests radar astrometry (delay + Doppler). Replays the plan's initial conditions (not a fit-then-propagate); its OD is a refit seeded from the plan's IC, holding the plan's Marsden non-grav parameters fixed. Reports the suite's only post-fit radar residuals. GRSS has no Marsden DT (non-grav time delay) term, so the five objects whose plan IC carries one are compared under a different force model — those rows are marked † and carry the reason.">GRSS</b></td>
        <td>Makadia et al. &middot; Gauss-Radau Small-body Simulator (C++ core, Python interface)</td>
        <td><a href="https://github.com/Empyrean-Dynamics/empyrean-validation/blob/main/runners/grss/run_grss.py" target="_blank" rel="noopener"><code>runners/grss/run_grss.py</code></a></td>
      </tr>
      <tr>
        <td><b title="Used for: N-body propagation & ephemeris (Bulirsch–Stoer, planets + Moon + Pluto, relativity on; no asteroid perturbers — BC430 not installed, so km-scale asteroid-perturbation signal remains in its residual). The plan's SSB states are converted to OpenOrb's heliocentric convention on the way in and back on the way out.">OpenOrb</b></td>
        <td>Granvik et al. · University of Helsinki (Fortran)</td>
        <td><a href="https://github.com/Empyrean-Dynamics/empyrean-validation/blob/main/runners/oorb/run_oorb.py" target="_blank" rel="noopener"><code>runners/oorb/run_oorb.py</code></a></td>
      </tr>
      <tr style="opacity:0.55">
        <td colspan="3" title="OrbFit: OrbFit Consortium / IAU MPC (CMC2003 rejection). kete: Dar Dahlen's personal project — independent Rust/Python NEO toolkit, originally Caltech IPAC / NEO Surveyor. jorbit: JAX autodiff propagator / OD. Runner scripts live under runners/; their JSON merges into the report when run."><small style="color:#8b9198"><b>Planned / opt-in:</b> OrbFit · kete · jorbit (not in this run)</small></td>
      </tr>
    </tbody>
  </table>
  </div>
</div>

<div class="part-head" id="part1">{part1_name}</div>
<div class="part1-bar" id="part1-bar">
  <div class="p1b-inner">
  <span class="p1b-label">Compare</span>
  <select id="tool1-select" aria-label="Tool" style="color:var(--tool-empyrean)"></select>
  <span class="p1b-vs">vs</span>
  <select id="tool2-select" aria-label="Reference" style="color:var(--tool-jpl)"></select>
  </div>
</div>
<div class="section" id="s02">
  <div class="section-num">1.1A</div>
  <div class="section-title" id="s02-title" title="Propagation — position agreement">Propagation{pair_chip}</div>
  {h1_prop_grid_html}
  <div class="disclosures"><details><summary>{disc_summary}</summary><div class="disc-body">Each cell is the absolute position offset in km between the selected tool and the reference at that horizon. The decade ladder's colour means the same everywhere and across runs. A cell the tool did not run, or an impactor horizon past impact (arc ends at impact), is hatched with the reason on hover &mdash; never dropped. The runaway wedge marks the off-scale top decade. Per-pair caveats, including any model-gap mark, live in that panel.</div></details></div>
</div>

<div class="section" id="s03" data-view="both">
  <div class="section-num">1.1B</div>
  <div class="section-title" title="Propagation — error growth by population">Error growth{pair_chip}</div>
  <div class="chart-container">
    <div id="growth-chart" style="height:{chart_h}px;"></div>
  </div>
  <div class="disclosures"><details><summary>{disc_summary}</summary><div class="disc-body">One line per population: median |tool &minus; reference| position offset (km) at each horizon, log y, driven by the pair selector. Where the selected tool omits a force term some objects need, a labelled dashed series traces them &mdash; a force-model gap, not an integrator one.</div></details></div>
</div>

<div class="section" id="s-t1-prop" data-view="both">
  <div class="section-num">1.1C</div>
  <div class="section-title" title="Propagation — cost per row">Cost{pair_chip}</div>
  {t1_prop_timing_html}
</div>

<div class="section" id="s06">
  <div class="section-num">1.2A</div>
  <div class="section-title" id="s06-title" title="Ephemeris — sky-plane agreement">Ephemeris{pair_chip}</div>
  {h2_eph_grid_html}
  <div class="disclosures"><details><summary>{disc_summary}</summary><div class="disc-body">Each cell is the mean angular separation (mas) between the selected tool's and the reference's RA/Dec over the five sites. Decade ladder; 1 mas = the Gaia DR3 single-frame floor, at the D4/D5 boundary. A cell the tool did not run is hatched, never dropped. Unlike propagation, the t0 column is not degenerate: topocentric parallax at closest approach leaves a real separation at epoch.</div></details></div>
</div>

<div class="section" id="s06b" data-view="both">
  <div class="section-num">1.2B</div>
  <div class="section-title" title="Ephemeris — error growth by population">Error growth{pair_chip}</div>
  <div class="channel-toggle" id="s06b-toggle">
    <button class="active" data-mode="rust">Selected pair</button>
    <button data-mode="all" data-requires="empyrean">empyrean channels</button>
  </div>
  <div class="chart-container">
    <div id="eph-sep-chart" style="height:{chart_h}px;" title="Separation vs propagation offset, by population"></div>
  </div>
  <div class="disclosures"><details><summary>{disc_summary}</summary><div class="disc-body">One line per population: median |tool &minus; reference| angular separation (mas) at each horizon, log y, with the 25&ndash;75% band, driven by the pair selector. Dotted lines mark the 1 mas Gaia single-frame floor and the 100 mas typical CCD residual scale. With empyrean in the pair, the channels view overlays each distribution channel's own median.</div></details></div>
</div>

<div class="section" id="s07">
  <div class="section-num">1.2D</div>
  <div class="section-title" title="Ephemeris — RA/Dec offset outliers">Outliers{pair_chip}</div>
  <div class="heatmap-container">
    <table class="heatmap" style="min-width:100%" title="Outliers beyond the sky-plane grid"><thead><tr><th style="text-align:left">Object</th><th>Channel</th><th>dt</th><th>Observer</th><th><span class="u">dRA·cos(δ)</span></th><th><span class="u">dDec</span></th><th><span class="u">‖d‖</span></th></tr></thead>
      <tbody id="eph-outliers"></tbody>
    </table>
  </div>
</div>

<div class="section" id="s-t2-eph" data-view="both">
  <div class="section-num">1.2C</div>
  <div class="section-title" title="Ephemeris — cost per row">Cost{pair_chip}</div>
  {t2_eph_timing_html}
</div>

<div class="section" id="s08b">
  <div class="section-num">1.3A</div>
  <div class="section-title" id="s08b-title" title="Orbit determination — convergence &amp; closeness">Orbit determination{pair_chip}</div>
{od_closeness_grid_html}
  <div class="disclosures"><details><summary>{disc_summary}</summary><div class="disc-body">Fill = <code>σ_eq = √(Δᵀ(Σ_fit+Σ_ref)⁻¹Δ/6)</code>: the 6-DOF χ-equivalent Mahalanobis distance in Sun-centred ecliptic J2000 Keplerian elements at a common epoch; the canonical cell propagates the fit to the reference's published epoch. Shape carries convergence; anything not provably an attempt reads "not attempted", never failure. Reference covariance: radar + decades of debiased astrometry; ours optical-only, Vereš-2017 weights. Objects whose JPL solution used radar are not like-for-like — hatched, counted, out of statistics; the 1–3 band (outlier-rejected covariance) is not failure. Nine of 82 lack a finite metric: Σ_ref not positive-definite at epoch (Duende, four self-perturbers; no regularisation). Audited in §4.1.</div></details></div>
</div>

<div class="section" id="s-t3-od" data-view="both">
  <div class="section-num">1.3B</div>
  <div class="section-title" title="Orbit determination — cost per fit">Cost{pair_chip}</div>
  {t3_od_timing_html}
</div>

<div class="section" id="overview-footer" hidden>
  Methods, all six reference tools, per-axis detail and outliers &rarr; <b>Advanced</b> &middot; cross-channel fidelity, timing and covariance &rarr; <b>empyrean internals</b>.
</div>

<div class="part-head" id="part2" title="No figure combines tools and no cell compares two tools; each states a tool's own numbers against JPL.">{part2_name}</div>
<div class="section" id="s-ranking">
  <div class="section-num">2.1</div>
  <div class="section-title" title="All tools, side by side — error growth, orbit determination, speed">Side by side</div>
  {tool_ranking_html}
  <div class="disclosures"><details><summary>{disc_summary}</summary><div class="disc-body">Each matrix states a tool's own numbers against JPL on the one decade ladder. Propagation and ephemeris cells: median and p90 offset from Horizons (km, mas). OD cell: fits converged of attempted, median σ-equivalent distance. Timing: p50 wall clock at its own boundary &mdash; integrate-only (ASSIST), whole-call (tool under test), per-fit (find_orb) &mdash; read down a row. A tool omitting the Marsden &Delta;T term and self-perturber forces the daggered objects need shows a force-model gap there (with-gap and excl-gap medians both). kete and jorbit emit timing only; find_orb is fit-then-propagate; OpenOrb, GRSS, OrbFit, layup have no rows and stay hatched.</div></details></div>
</div>
<div class="section" id="s-timing">
  <div class="section-num">2.2</div>
  <div class="section-title">Wall clock</div>
  {part2_timing_html}
</div>
<div class="part-head" id="part3">{part3_name}</div>
<div class="section" id="s10">
  <div class="section-num">3.1A</div>
  <div class="section-title" title="Channel fidelity — every distribution channel against empyrean-core">Channel fidelity</div>
  {h4_fidelity_grid_html}
  <div class="disclosures"><details><summary>{disc_summary}</summary><div class="disc-body">Each channel row pairs to its core row by (object, dt, force model, test type, observer, uncertainty arm). Prop/OD compared in Cartesian position (km; ULP ≤ 1e-10 km, floor 1 mm); ephemeris as angular separation (arcsec), RA wrapped modulo a turn so a 360° wrap is no defect (floor 1 µas, 1 mas). A cell is its population's worst state, never the majority; DIFF cells name objects. A channel absent this run is hatched "not run" — here <code>c</code>, so the C ABI is unvalidated. rust runs the full uncertainty grid; other channels only public-API arms (~half the counts).</div></details></div>

</div>

<div class="section" id="s05" data-page="empyrean">
  <div class="section-num">3.2A</div>
  <div class="section-title" title="Derivatives — first-order STM cross-validation">Derivatives</div>
  <div class="panel-title">First-order STM cross-validation</div>
  <div class="chart-container">
    <div id="stm-agree-chart" style="height:{chart_h}px;"></div>
  </div>
  <div class="disclosures"><details><summary>{disc_summary}</summary><div class="disc-body">Relative Frobenius difference vs |dt| (log-y): empyrean's Jet1 STM against the corrected ASSIST variational STM over all first-order rows (median solid, 95th-percentile dashed), and against an ASSIST single-particle finite difference on a representative subset (measured step-size floor 1e-8, dotted). The two analytic methods agree to machine precision on the bulk; the tail is the self-perturber rows and deep encounters; the finite difference anchors both to truth at its own floor.</div></details></div>
</div>

<div class="section" id="s09b">
  <div class="section-num">3.3A</div>
  <div class="section-title" title="Non-gravitational recovery — fitted A1/A2/A3 vs JPL">Non-gravitational recovery</div>
  <div id="ng-empty" class="section-desc" style="display:none; color:#8b9198">No non-gravitational-recovery rows in this report. Run the OD subset against objects with a known SBDB non-grav signal to populate this section.</div>
  <div id="ng-content">
    <div class="panel-title" title="Fitted A1/A2/A3 vs JPL SBDB — σ-consistency per channel">Fitted A1/A2/A3 vs JPL SBDB</div>
    <div class="heatmap-container">
      <table class="od-table">
        <thead><tr>
          <th class="obj" style="text-align:left">Object</th>
          <th>Channel</th>
          <th title="{coeff_defs}">Coeff</th>
          <th>Fitted <span class="u">(AU/day²)</span></th>
          <th>JPL SBDB <span class="u">(AU/day²)</span></th>
          <th><span class="u">z = (fit&minus;JPL)/σ</span></th>
          <th title="{verdict_rule}">Result</th>
        </tr></thead>
        <tbody id="ng-overview"></tbody>
      </table>
    </div>
  </div>
  <div class="disclosures"><details><summary>{disc_summary}</summary><div class="disc-body">Objects with a non-zero JPL non-grav signal (Apophis, Bennu, the comets) are re-fit with <code>solve_for = StateAndNonGrav</code>; the fitted Marsden A1/A2/A3 and their 1σ (9×9 covariance diagonal) are compared to JPL. <b>PASS</b> when <code>|z| = |od_a − ic_a| / σ ≤ 3</code> on every coefficient. A <code>None</code> is a loud <b>FAIL</b>, not a blank — the 9×9 covariance was absent (silent fall-back to a state-only fit), the regression this section catches. All four channels (rust/c/cli/python) run side by side, so an FFI drop of the block shows immediately.</div></details></div>
</div>

<div class="part-head" id="part4">{part4_name}</div>
<div class="section" id="s12">
  <div class="section-num">4.1A</div>
  <div class="cov-panels">
    <div class="chart-container"><div id="cov-chi2-panel" style="height:340px;"></div></div>
    <div class="chart-container"><div id="cov-ratio-panel" style="height:340px;"></div></div>
    <div class="chart-container"><div id="cov-cdf-panel" style="height:340px;"></div></div>
  </div>
  {covariance_realism_html}
  <div class="disclosures"><details><summary>{disc_summary}</summary><div class="disc-body">A sigma-consistency test is not a coverage test: it shows consistency with JPL's stated covariance, not that either is correct. Every metric divides by an outlier-rejected formal covariance, the reference too; the measured post-selection effect is ~1.9–2.0× the formal 1σ, so part of the ~1.4 shortfall may be post-selection rather than a tight covariance, and tuning a threshold to a 0.94 median would tune toward an inflated sigma. Both pipelines sit below unity in reduced χ² under conservative Vereš-2017 weights — empyrean's 0.08 on Apophis reproduces the reference pipeline, visible only against JPL's 0.066 on the same object.</div></details></div>

</div>

<div class="section" id="s05c" data-page="empyrean">
  <div class="section-num">A.3</div>
  <div class="section-title" title="ASSIST configurations — six arms">ASSIST configurations</div>
  <div class="panel-title" title="Resolved integrator settings, read back after attach">Resolved integrator settings</div>
  <div class="heatmap-container"><div id="assist-arms-settings"></div></div>
  <div class="disclosures"><details><summary>{disc_summary}</summary><div class="disc-body">The same objects and epochs under three ASSIST integrator configurations, each with the six first-order variational particles off (single-particle f64) and on (6&times;6 STM). <code>assist_default</code> = what <code>assist.Extras</code> attach leaves (IAS15 adaptive mode 1, 1e-9, min_dt 0, initial dt 0.001 d); <code>assist_layup</code> sets adaptive mode 2 (PRS controller) after attach, as Smithsonian/layup does; <code>assist_asteroid_institute</code> = adam-assist defaults (1e-6, min_dt 1e-9 d, mode 1, initial dt 1e-6 d), the config every earlier ASSIST number used. Each arm has a per-row wall clock cap; a row whose adaptive step collapses before the target epoch is a loud FAIL, counted below, never substituted.</div></details></div>
  <div class="panel-title" title="Per-horizon median timing — integrate call only (ms)">Per-horizon median timing</div>
  <div class="heatmap-container"><div id="assist-arms-timing"></div></div>
  <div class="panel-title" title="Position agreement vs empyrean at 3 and 15 years, per configuration">Position agreement vs empyrean</div>
  <div class="heatmap-container"><div id="assist-arms-pos"></div></div>
  <div class="disclosures"><details><summary>{disc_summary}</summary><div class="disc-body">Median |empyrean &minus; ASSIST| in km at |dt| = 3 yr and 15 yr; the propagated position is independent of the variational particles, so this is per configuration. empyrean is its own <code>none_detection_on</code> reference. Marsden &Delta;T objects are excluded from each median and counted separately (see &Delta;T exclusion); the all-rows median is in parentheses.</div></details></div>
  <div class="panel-title" title="STM agreement vs empyrean, per variational arm">STM agreement vs empyrean</div>
  <div class="heatmap-container"><div id="assist-arms-stm"></div></div>
  <div class="disclosures"><details><summary>{disc_summary}</summary><div class="disc-body">Median relative Frobenius difference |emp &minus; ast|/|emp| between empyrean's Jet1 STM and each configuration's ASSIST variational STM, over all horizons.</div></details></div>
  <div class="panel-title">empyrean barebones arms &mdash; ASSIST-matched settings</div>
  <div class="heatmap-container">
  <table class="od-table" style="font-size:11px;">
    <thead><tr><th style="text-align:left">empyrean barebones arm</th><th style="text-align:left">Matched ASSIST config(s)</th><th>epsilon</th><th>initial step <span class="u">(d)</span></th><th>minimum step <span class="u">(d)</span></th><th style="text-align:left">adaptive mode</th></tr></thead>
    <tbody>
      <tr><td style="text-align:left"><code>*_assist_default_like</code></td><td style="text-align:left">assist_default, assist_layup</td><td>1e-9 (matched)</td><td>0.001 (matched)</td><td>0 (matched)</td><td style="text-align:left">no counterpart (GR15; floor tied to &epsilon;)</td></tr>
      <tr><td style="text-align:left"><code>*_assist_asteroid_institute_like</code></td><td style="text-align:left">assist_asteroid_institute</td><td>1e-6 (matched)</td><td>1e-6 (matched)</td><td>1e-9 (matched)</td><td style="text-align:left">no counterpart (GR15; floor tied to &epsilon;)</td></tr>
    </tbody>
  </table>
  </div>
  <div class="disclosures"><details><summary>{disc_summary}</summary><div class="disc-body">empyrean's like-for-like timing arms use a force-model handle assembled once per object outside the timer (only the propagate call timed &mdash; the ASSIST integrate-call analogue), with event detection and dense output off, and villeneuve's integrator knobs matched to an ASSIST configuration. The mapping is exact where a knob exists on both sides, stated where not: villeneuve keeps GR15 (not IAS15); its encounter step-size floor ties to <code>epsilon</code> (&Delta;&theta; &prop; &epsilon;<sup>1/7</sup>), with no separate floor knob or adaptive-mode counterpart, so the layup ASSIST arms (differing from default only in the adaptive criterion) are compared against the default-like empyrean arm.</div></details></div>
</div>

{provenance_footer_html}
</main>
</div>

<script>
// The full result set is fetched from a sibling file, not embedded, so this page
// stays small and loads over HTTP. `results` is null until boot() verifies and
// installs the fetched dataset; DATASET is the inline descriptor boot() checks
// the fetched bytes against (file name, byte length, row count, FNV-1a-32 hash).
let results = null;
const DATASET = DATASET_DESCRIPTOR_JSON;
const popColors = POP_COLORS_JSON;
const channelColors = CHANNEL_COLORS_JSON;
const AU_KM = 149597870.700;
// Objects whose JPL solution carries a Marsden non-gravitational time-delay
// (ΔT / DT) term. empyrean applies it (NonGravParams.dt) and matches Horizons
// to metres; ASSIST models no ΔT term, so its position for exactly these
// objects diverges from Horizons under every configuration — a force-model
// capability gap of ASSIST, not an integrator or tolerance effect. Built from
// the empyrean channels' ic_non_grav_dt; the standalone ASSIST rows carry no
// such field, so panels key ΔT membership off the object name. Filled by boot()
// once the dataset is loaded.
let DT_OBJECTS = new Map();
const isDtObject = (o) => DT_OBJECTS.has(o);
// Channel-fidelity verdict computed by the Rust rollup (bit-identical
// row counts vs core) — the JS can't cheaply recompute it.
const HERO_CH = {{ pass: {hero_ch_pass}, n: {hero_ch_n} }};

// ─────────── tool-pair registry (report generalization) ───────────
// Single source of truth for the tool1-vs-tool2 comparator. Every row carries
// per-tool fields (emp_* = Empyrean, ref_* = JPL Horizons truth, assist_*,
// oorb_*, findorb_*, orbfit_*, layup_*). This registry knows how to read a
// pairwise value for a chosen (tool1, tool2) on a given axis, and which
// pairings are legal — propagation/ephemeris comparisons are stored as
// Horizons-anchored diffs (raw ASSIST/OpenOrb vectors are NOT persisted), so
// only pairs reconstructable from those diffs are offered.
const TOOLS = {{
    empyrean: {{ label: 'empyrean', color: '#5b9bd5', truth: false }},
    jpl:      {{ label: 'JPL', color: '#3d9a6d', truth: true }},
    assist:   {{ label: 'ASSIST', color: '#c77dff', truth: false }},
    oorb:     {{ label: 'OpenOrb', color: '#e8a040', truth: false }},
    findorb:  {{ label: 'find_orb', color: '#d05040', truth: false }},
    orbfit:   {{ label: 'OrbFit', color: '#7dd3c0', truth: false }},
    layup:    {{ label: 'layup', color: '#e07bc0', truth: false }},
    grss:     {{ label: 'GRSS', color: '#8fbf5f', truth: false }},
}};
function toolLabel(t) {{ return (TOOLS[t] && TOOLS[t].label) || t; }}
function toolColor(t) {{ return (TOOLS[t] && TOOLS[t].color) || '#888'; }}
// CSS colour for a tool NAME shown as DOM text (the compare-bar selects and the
// section-title pair chips): the theme-aware --tool-<slug> token, falling back to
// the chart hex for any tool without one. Charts keep toolColor()'s literal hex
// because Plotly cannot resolve a CSS variable.
function toolTextColor(t) {{ return 'var(--tool-' + t + ', ' + toolColor(t) + ')'; }}

// Tools actually present in this run — empyrean + horizons are structural
// (reference + truth); externals appear only if a row carries their fields.
// Computed server-side (same probe) and inlined, so the Tool / Reference selects
// populate and drive the server-rendered pair-panels before the dataset arrives.
const TOOLS_PRESENT = new Set(TOOLS_PRESENT_JSON);

// Per-tool axis coverage. prop_pos: propagation position (km). eph: ephemeris
// separation (arcsec). od_rms / od_chi2 / od_nobs: OD scalars. time: ms.
const TOOL_AXES = {{
    empyrean: new Set(['prop_pos', 'eph', 'od_rms', 'od_chi2', 'od_nobs', 'time']),
    jpl:      new Set(['prop_pos', 'eph', 'od_chi2', 'od_nobs']),
    assist:   new Set(['prop_pos', 'time']),
    oorb:     new Set(['prop_pos', 'eph', 'time']),
    findorb:  new Set(['prop_pos', 'eph', 'od_rms', 'od_nobs']),
    orbfit:   new Set(['od_rms', 'od_nobs']),
    layup:    new Set(['od_chi2', 'od_nobs']),
    // GRSS is the only external reference that covers every axis at once.
    grss:     new Set(['prop_pos', 'eph', 'od_rms', 'od_chi2', 'od_nobs', 'time']),
}};
// Which (tool, axis) actually have data in THIS run — a tool may be
// structurally capable of an axis (TOOL_AXES) yet carry no rows for it (e.g.
// OpenOrb ran propagation but not ephemeris here). Intersecting the two keeps
// the selector from offering pairs that would draw an empty chart.
// Filled by boot() once the dataset is present; hasAxis() reads it and every
// caller of hasAxis runs only from a boot-registered renderer.
let TOOL_AXIS_DATA = {{}};
function computeToolAxisData() {{
    const has = {{}};
    const mark = (t, ax) => {{ (has[t] = has[t] || new Set()).add(ax); }};
    for (const r of results) {{
        if (r.emp_vs_horizons_km != null) {{ mark('empyrean', 'prop_pos'); mark('jpl', 'prop_pos'); }}
        if (r.emp_vs_assist_km != null || r.assist_vs_horizons_km != null) {{ mark('assist', 'prop_pos'); mark('empyrean', 'prop_pos'); mark('jpl', 'prop_pos'); }}
        if (r.emp_vs_oorb_km != null || r.oorb_vs_horizons_km != null) {{ mark('oorb', 'prop_pos'); mark('empyrean', 'prop_pos'); mark('jpl', 'prop_pos'); }}
        if (r.d_ra_arcsec != null) {{ mark('empyrean', 'eph'); mark('jpl', 'eph'); }}
        if (r.oorb_d_ra_arcsec != null) {{ mark('oorb', 'eph'); mark('jpl', 'eph'); }}
        if (r.emp_time_ms != null) mark('empyrean', 'time');
        if (r.assist_time_ms != null) mark('assist', 'time');
        if (r.oorb_time_ms != null) mark('oorb', 'time');
        if (r.od_rms_combined_arcsec != null) mark('empyrean', 'od_rms');
        if (r.od_reduced_chi2 != null) mark('empyrean', 'od_chi2');
        if (r.n_obs_used != null) mark('empyrean', 'od_nobs');
        if (r.findorb_rms_residual != null) {{ mark('findorb', 'od_rms'); mark('findorb', 'od_nobs'); }}
        if (r.orbfit_rms_arcsec != null) {{ mark('orbfit', 'od_rms'); mark('orbfit', 'od_nobs'); }}
        if (r.layup_reduced_chi2 != null) mark('layup', 'od_chi2');
        if (r.layup_n_obs_used != null) mark('layup', 'od_nobs');
        if (r.findorb_vs_horizons_km != null) {{ mark('findorb', 'prop_pos'); mark('jpl', 'prop_pos'); }}
        if (r.emp_vs_findorb_km != null) {{ mark('findorb', 'prop_pos'); mark('empyrean', 'prop_pos'); }}
        if (r.findorb_d_ra_arcsec != null) {{ mark('findorb', 'eph'); mark('jpl', 'eph'); }}
        if (r.ref_od_reduced_chi2 != null) mark('jpl', 'od_chi2');
        if (r.ref_od_n_obs_used != null) mark('jpl', 'od_nobs');
        if (r.grss_vs_horizons_km != null) {{ mark('grss', 'prop_pos'); mark('jpl', 'prop_pos'); }}
        if (r.emp_vs_grss_km != null) {{ mark('grss', 'prop_pos'); mark('empyrean', 'prop_pos'); }}
        if (r.grss_d_ra_arcsec != null) {{ mark('grss', 'eph'); mark('jpl', 'eph'); }}
        if (r.grss_rms_arcsec != null) mark('grss', 'od_rms');
        if (r.grss_reduced_chi2 != null) mark('grss', 'od_chi2');
        if (r.grss_n_obs_used != null) mark('grss', 'od_nobs');
        if (r.grss_time_ms != null) mark('grss', 'time');
    }}
    return has;
}}
function hasAxis(tool, axis) {{
    return !!(TOOL_AXES[tool] && TOOL_AXES[tool].has(axis) && TOOL_AXIS_DATA[tool] && TOOL_AXIS_DATA[tool].has(axis));
}}

// Pairwise propagation position difference (km) on a core prop row. Only the
// Horizons-anchored / Empyrean-anchored diffs exist on disk; any other pair
// (e.g. ASSIST vs OpenOrb) returns null and is gated out by legalPair.
function propPosDiffKm(row, t1, t2) {{
    const F = {{
        'empyrean|jpl': 'emp_vs_horizons_km',
        'assist|empyrean': 'emp_vs_assist_km',
        'empyrean|oorb': 'emp_vs_oorb_km',
        'assist|jpl': 'assist_vs_horizons_km',
        'jpl|oorb': 'oorb_vs_horizons_km',
        'empyrean|findorb': 'emp_vs_findorb_km',
        'findorb|jpl': 'findorb_vs_horizons_km',
        'empyrean|grss': 'emp_vs_grss_km',
        'grss|jpl': 'grss_vs_horizons_km',
    }};
    const f = F[[t1, t2].sort().join('|')];
    return (f && row[f] != null) ? row[f] : null;
}}

// Ephemeris signed offsets (dRA·cosδ, dDec) of a tool vs Horizons (arcsec).
// Horizons is the origin (0,0); Empyrean and OpenOrb store signed offsets, so
// a tool1-vs-tool2 separation is the norm of their difference.
function ephOffsets(row, tool) {{
    if (tool === 'jpl') return [0, 0];
    if (tool === 'empyrean') return (row.d_ra_arcsec != null && row.d_dec_arcsec != null) ? [row.d_ra_arcsec, row.d_dec_arcsec] : null;
    if (tool === 'oorb') return (row.oorb_d_ra_arcsec != null && row.oorb_d_dec_arcsec != null) ? [row.oorb_d_ra_arcsec, row.oorb_d_dec_arcsec] : null;
    if (tool === 'findorb') return (row.findorb_d_ra_arcsec != null && row.findorb_d_dec_arcsec != null) ? [row.findorb_d_ra_arcsec, row.findorb_d_dec_arcsec] : null;
    if (tool === 'grss') return (row.grss_d_ra_arcsec != null && row.grss_d_dec_arcsec != null) ? [row.grss_d_ra_arcsec, row.grss_d_dec_arcsec] : null;
    return null;
}}
function ephSepArcsec(row, t1, t2) {{
    const a = ephOffsets(row, t1), b = ephOffsets(row, t2);
    if (!a || !b) return null;
    const dra = a[0] - b[0], ddec = a[1] - b[1];
    return Math.sqrt(dra * dra + ddec * ddec);
}}

// Per-tool absolute scalars.
function toolTimeMs(row, tool) {{ return ({{ empyrean: row.emp_time_ms, assist: row.assist_time_ms, oorb: row.oorb_time_ms, grss: row.grss_time_ms }})[tool] ?? null; }}
function odRms(row, tool) {{ return ({{ empyrean: row.od_rms_combined_arcsec, findorb: row.findorb_rms_residual, orbfit: row.orbfit_rms_arcsec, grss: row.grss_rms_arcsec }})[tool] ?? null; }}
function odReducedChi2(row, tool) {{ return ({{ empyrean: row.od_reduced_chi2, layup: row.layup_reduced_chi2, jpl: row.ref_od_reduced_chi2, grss: row.grss_reduced_chi2 }})[tool] ?? null; }}
function odNobs(row, tool) {{ return ({{ empyrean: row.n_obs_used, findorb: row.findorb_n_obs_used, orbfit: row.orbfit_n_obs_used, layup: row.layup_n_obs_used, jpl: row.ref_od_n_obs_used, grss: row.grss_n_obs_used }})[tool] ?? null; }}

// ── Mahalanobis (uncertainty-unit) offsets ──────────────────────────────
// Only Empyrean propagates a covariance, so these are defined only for the
// Empyrean-vs-JPL pair: the offset vector normalized by Empyrean's propagated
// covariance, d = sqrt(Δᵀ C⁻¹ Δ). d < 1 means the offset sits inside the 1σ
// uncertainty ellipsoid. (The validation attaches a synthetic typical-NEO
// input covariance, so d is measured against that propagated envelope.)
function inv3(m) {{
    const a=m[0][0],b=m[0][1],c=m[0][2],d=m[1][0],e=m[1][1],f=m[1][2],g=m[2][0],h=m[2][1],i=m[2][2];
    const det = a*(e*i-f*h) - b*(d*i-f*g) + c*(d*h-e*g);
    if (!isFinite(det) || Math.abs(det) < 1e-300) return null;
    const s = 1/det;
    return [[(e*i-f*h)*s,(c*h-b*i)*s,(b*f-c*e)*s],
            [(f*g-d*i)*s,(a*i-c*g)*s,(c*d-a*f)*s],
            [(d*h-e*g)*s,(b*g-a*h)*s,(a*e-b*d)*s]];
}}
function mahalanobisProp(row, t1, t2) {{
    const pair = new Set([t1, t2]);
    if (!(pair.has('empyrean') && pair.has('jpl'))) return null;
    const p = row.emp_pos_au, r = row.ref_pos_au, C = row.emp_pos_cov_au2;
    if (!p || !r || !C) return null;
    const Ci = inv3(C);
    if (!Ci) return null;
    const dv = [p[0]-r[0], p[1]-r[1], p[2]-r[2]];
    let d2 = 0;
    for (let i=0;i<3;i++) for (let j=0;j<3;j++) d2 += dv[i]*Ci[i][j]*dv[j];
    return d2 >= 0 ? Math.sqrt(d2) : null;
}}
function mahalanobisSky(row, t1, t2) {{
    const pair = new Set([t1, t2]);
    if (!(pair.has('empyrean') && pair.has('jpl'))) return null;
    const dra = row.d_ra_arcsec, ddec = row.d_dec_arcsec, C = row.emp_radec_cov_arcsec2;
    if (dra == null || ddec == null || !C) return null;
    const det = C[0][0]*C[1][1] - C[0][1]*C[1][0];
    if (!isFinite(det) || Math.abs(det) < 1e-300) return null;
    const Ci = [[C[1][1]/det, -C[0][1]/det], [-C[1][0]/det, C[0][0]/det]];
    const dd = [dra, ddec];
    let d2 = 0;
    for (let i=0;i<2;i++) for (let j=0;j<2;j++) d2 += dd[i]*Ci[i][j]*dd[j];
    return d2 >= 0 ? Math.sqrt(d2) : null;
}}
// Does the current pair have Mahalanobis-σ data on this axis? Reads the dataset,
// so it is false until boot() installs it (only boot-registered renderers call it).
function sigmaAvailable(axis) {{
    if (!results) return false;
    if (!(new Set([TOOL1, TOOL2]).has('empyrean') && new Set([TOOL1, TOOL2]).has('jpl'))) return false;
    const field = axis === 'prop' ? 'emp_pos_cov_au2' : 'emp_radec_cov_arcsec2';
    return results.some(r => r[field] != null);
}}
function fmtSigma(s) {{
    if (s == null || !isFinite(s)) return '—';
    if (s < 100) return s.toFixed(s < 10 ? 2 : 1) + 'σ';
    return s.toExponential(1) + 'σ';
}}
// Colour by statistical significance: <1σ blue/green (consistent), a few σ
// amber, many σ red/magenta. Log-mapped over [0.1σ, 100σ].
function sigmaColor(s) {{
    if (s == null || !isFinite(s)) return '#161c25';
    const t = Math.max(0, Math.min(1, (Math.log10(Math.max(s, 0.1)) + 1) / 3));
    const stops = [[0.00,[13,30,60]],[0.33,[45,120,90]],[0.55,[200,175,70]],[0.72,[220,110,55]],[0.85,[200,60,60]],[1.00,[180,30,110]]];
    let rgb = stops[0][1];
    for (let k=0;k<stops.length-1;k++) {{ const at=stops[k][0],argb=stops[k][1],bt=stops[k+1][0],brgb=stops[k+1][1]; if (t>=at&&t<=bt) {{ const f=(t-at)/Math.max(bt-at,1e-9); rgb=[argb[0]+f*(brgb[0]-argb[0]),argb[1]+f*(brgb[1]-argb[1]),argb[2]+f*(brgb[2]-argb[2])]; break; }} }}
    const hx = n => Math.max(0,Math.min(255,Math.floor(n))).toString(16).padStart(2,'0');
    return '#'+hx(rgb[0])+hx(rgb[1])+hx(rgb[2]);
}}
let propHeatMode = 'physical', ephHeatMode = 'physical';

// Is (t1, t2) a legal, on-disk-reconstructable comparison on this axis?
function legalPair(t1, t2, axis) {{
    if (t1 === t2) return false;
    if (!hasAxis(t1, axis) || !hasAxis(t2, axis)) return false;
    if (axis === 'prop_pos') {{
        const anchored = t => t === 'empyrean' || t === 'jpl';
        if (!anchored(t1) && !anchored(t2)) return false; // e.g. ASSIST vs OpenOrb
    }}
    if (axis === 'eph') {{
        // Whitelist, not a blacklist: an ephemeris pair is only reconstructable
        // for tools that persist SIGNED (dRA·cosδ, dDec) offsets vs Horizons.
        // A tool storing only a scalar separation has no direction, so its
        // difference against another tool is not defined. `ephOffsets` must
        // know each name listed here.
        const ok = t => t === 'empyrean' || t === 'jpl' || t === 'oorb' || t === 'findorb' || t === 'grss';
        if (!ok(t1) || !ok(t2)) return false;
    }}
    return true;
}}

// Active tool-pair selection (default: Empyrean vs JPL Horizons).
let TOOL1 = 'empyrean', TOOL2 = 'jpl';
function empyreanSelected() {{ return TOOL1 === 'empyrean' || TOOL2 === 'empyrean'; }}

// Panels register a render callback here as they are generalized; each reads
// TOOL1/TOOL2 + legalPair internally and re-renders on every selection change.
const TOOL_RENDERERS = [];
function onToolChange(fn) {{ TOOL_RENDERERS.push(fn); }}

// ─────────── pair-panel visibility (server-rendered heatmaps) ───────────
// Each part-one agreement section server-renders one .pair-panel per tool, all
// compared against JPL. Show exactly the panel matching the selected
// (Tool, Reference) pair; an illegal or empty pair — a non-JPL reference, the
// same tool on both sides, or a tool that did not run the axis — collapses the
// section body to a single hatched "not compared" line, never an empty panel.
function showPairPanels() {{
    document.querySelectorAll('.pair-panels').forEach(group => {{
        let matched = false, generic = null;
        // Iterate direct children only (no :scope selector, for max
        // compatibility): show the panel whose (tool, reference) matches the
        // selection, hide the rest, and reveal the generic hatched line only
        // when nothing matched (a reference that carries no positions).
        Array.prototype.forEach.call(group.children, el => {{
            if (el.classList.contains('pair-panel')) {{
                const match = TOOL1 !== TOOL2 && el.dataset.tool === TOOL1 && el.dataset.ref === TOOL2;
                el.hidden = !match;
                if (match) matched = true;
            }} else if (el.classList.contains('pair-notcompared') && el.classList.contains('generic')) {{
                generic = el;
            }}
        }});
        if (generic) generic.hidden = matched;
    }});
}}
onToolChange(showPairPanels);

// The one pair-update path for the Part 1 section chips (its Rust twin
// pair_chip_html server-renders the default pair). Each chip names the selected
// pair in the two tools' colours; the section-title hovers stay pair-neutral now
// that the chip carries the pair.
function updatePairTitles() {{
    document.querySelectorAll('.pair-chip').forEach(chip => {{
        chip.dataset.tool = TOOL1;
        chip.dataset.ref = TOOL2;
        const t = chip.querySelector('.pc-t'), r = chip.querySelector('.pc-r');
        if (t) {{ t.textContent = toolLabel(TOOL1); t.style.color = toolTextColor(TOOL1); }}
        if (r) {{ r.textContent = toolLabel(TOOL2); r.style.color = toolTextColor(TOOL2); }}
    }});
}}
onToolChange(updatePairTitles);

// Every method disclosure starts closed on load, overriding any state a
// browser may restore from its back/forward cache.
document.querySelectorAll('details').forEach(d => {{ d.open = false; }});

// The option sets differ (Tool offers empyrean; Reference offers jpl/layup),
// so a value legal in one select is not always legal in the other — the source
// of the blanked-select bug. Kept at module scope so the change handlers and the
// invariant can consult them.
let TOOL_OPTS = [], REF_OPTS = [];
function populateToolSelects() {{
    // Tool (the tool under test) leads with empyrean; Reference leads with JPL,
    // the run's reference. Each select offers only the tools present in the run.
    const toolOrder = ['empyrean', 'assist', 'findorb', 'grss', 'oorb', 'orbfit'];
    const refOrder = ['jpl', 'assist', 'findorb', 'grss', 'oorb', 'layup', 'orbfit'];
    TOOL_OPTS = toolOrder.filter(t => TOOLS_PRESENT.has(t));
    REF_OPTS = refOrder.filter(t => TOOLS_PRESENT.has(t));
    const s1 = document.getElementById('tool1-select'), s2 = document.getElementById('tool2-select');
    if (s1) s1.innerHTML = TOOL_OPTS.map(t => `<option value="${{t}}">${{toolLabel(t)}}</option>`).join('');
    if (s2) s2.innerHTML = REF_OPTS.map(t => `<option value="${{t}}">${{toolLabel(t)}}</option>`).join('');
    if (s1) s1.value = TOOL1;
    if (s2) s2.value = TOOL2;
    syncPairOptions();
}}

// An illegal pair is unselectable: in each select the option equal to the other
// select's current value is disabled, so tool === reference can never be chosen.
function syncPairOptions() {{
    const s1 = document.getElementById('tool1-select'), s2 = document.getElementById('tool2-select');
    if (s1) for (const o of s1.options) o.disabled = (o.value === TOOL2);
    if (s2) for (const o of s2.options) o.disabled = (o.value === TOOL1);
}}

// Loud invariant: the two selects, the model pair, and the option sets must
// always agree. A blanked select, an out-of-set value, or tool === reference is
// a bug, not a silent state — surface it in the console rather than let a chart
// draw a pair the tables call "not compared".
function assertPairInvariant(where) {{
    const s1 = document.getElementById('tool1-select'), s2 = document.getElementById('tool2-select');
    const problems = [];
    if (TOOL1 === TOOL2) problems.push('tool === reference (' + TOOL1 + ')');
    if (!TOOL_OPTS.includes(TOOL1)) problems.push('tool "' + TOOL1 + '" is not a Tool option');
    if (!REF_OPTS.includes(TOOL2)) problems.push('reference "' + TOOL2 + '" is not a Reference option');
    if (s1 && s1.value !== TOOL1) problems.push('Tool select shows "' + s1.value + '" but TOOL1="' + TOOL1 + '"');
    if (s2 && s2.value !== TOOL2) problems.push('Reference select shows "' + s2.value + '" but TOOL2="' + TOOL2 + '"');
    // Every Part 1 chip is the reader's on-screen record of the compared pair, so
    // each must name the current selection.
    document.querySelectorAll('.pair-chip').forEach(chip => {{
        if (chip.dataset.tool !== TOOL1 || chip.dataset.ref !== TOOL2)
            problems.push('chip names ' + chip.dataset.tool + '/' + chip.dataset.ref + ' not ' + TOOL1 + '/' + TOOL2);
    }});
    if (problems.length) console.error('[pair invariant] ' + where + ': ' + problems.join('; '));
}}

function applyToolSelection() {{
    // Gate empyrean-only sections (cross-channel fidelity, uncertainty cost,
    // non-grav recovery, fitted covariance) — shown only when Empyrean is one
    // of the two selected tools.
    // Hide via a class (not inline display) so a panel's own data-presence
    // visibility logic is preserved when Empyrean IS selected — we only ever
    // ADD hiding when it is not.
    const emp = empyreanSelected();
    document.querySelectorAll('[data-requires="empyrean"]').forEach(el => {{
        el.classList.toggle('tool-hidden', !emp);
    }});
    // The compare-bar selects wear their tool's colour, so the bar, the section
    // chips and the pair speak one colour language.
    const s1 = document.getElementById('tool1-select'), s2 = document.getElementById('tool2-select');
    if (s1) s1.style.color = toolTextColor(TOOL1);
    if (s2) s2.style.color = toolTextColor(TOOL2);
    for (const fn of TOOL_RENDERERS) {{ try {{ fn(); }} catch (e) {{ console.error('tool renderer failed', e); }} }}
    // The panels and charts have just re-rendered from TOOL1/TOOL2; verify the
    // whole pair state agrees before handing the page back to the reader.
    assertPairInvariant('applyToolSelection');
}}

function wireToolSelector() {{
    populateToolSelects();
    const t1 = document.getElementById('tool1-select'), t2 = document.getElementById('tool2-select');
    // The matching option is disabled (syncPairOptions), so a same-tool pair
    // cannot be picked; the guard below only assigns values legal in the target
    // select, so no auto-swap can ever blank a dropdown.
    if (t1) t1.onchange = () => {{
        TOOL1 = t1.value;
        if (TOOL1 === TOOL2) {{ TOOL2 = REF_OPTS.find(t => t !== TOOL1) || TOOL2; if (t2) t2.value = TOOL2; }}
        syncPairOptions();
        applyToolSelection();
    }};
    if (t2) t2.onchange = () => {{
        TOOL2 = t2.value;
        if (TOOL2 === TOOL1) {{ TOOL1 = TOOL_OPTS.find(t => t !== TOOL2) || TOOL1; if (t1) t1.value = TOOL1; }}
        syncPairOptions();
        applyToolSelection();
    }};
    applyToolSelection();
}}

// ─────────── top-level page switch (Tool Comparison / Empyrean Internals) ──
// Sections carry data-page ('comparison' default, 'empyrean', or 'both'); the
// tool-pair selector belongs to the comparison page. Empyrean-internal panels
// (multichannel fidelity, Jet1-vs-f64 timing/uncertainty, non-grav, covariance)
// live on their own page and are not gated by the tool-pair selection.
// Redesign: one document, one scroll. The Overview / Advanced
// / Internals tabs are gone — every section is always visible and the sticky
// left rail is the contents, driven by a scroll-spy. `showPage` survives as a
// reveal-all + scroll-to shim so the (now-legacy) deep-link and pillar-link
// callers do not throw.
function revealAllSections() {{
    document.querySelectorAll('.section').forEach(s => {{
        s.classList.remove('page-hidden', 'tool-hidden');
        if (s.style && s.style.display === 'none') s.style.display = '';
    }});
    document.body.classList.remove('view-overview');
}}
function showPage(page) {{
    revealAllSections();
    // Legacy callers pass a tab name; treat an element id as a scroll target.
    const el = document.getElementById(page);
    if (el) el.scrollIntoView({{ block: 'start' }});
    if (window.Plotly) {{
        document.querySelectorAll('.js-plotly-plot').forEach(gd => {{
            try {{ Plotly.Plots.resize(gd); }} catch (e) {{}}
        }});
    }}
}}
function wirePageNav() {{
    revealAllSections();
    // Sticky-rail scroll-spy: highlight the rail link whose section is in view.
    const links = Array.from(document.querySelectorAll('#rail a[data-target]'));
    const byId = new Map(links.map(a => [a.getAttribute('data-target'), a]));
    if ('IntersectionObserver' in window && links.length) {{
        const obs = new IntersectionObserver((entries) => {{
            entries.forEach(e => {{
                const a = byId.get(e.target.id);
                if (a && e.isIntersecting) {{
                    links.forEach(l => l.classList.remove('active'));
                    a.classList.add('active');
                }}
            }});
        }}, {{ rootMargin: '-10% 0px -80% 0px', threshold: 0 }});
        document.querySelectorAll('.section[id]').forEach(s => obs.observe(s));
    }}
    links.forEach(a => {{
        a.onclick = (ev) => {{
            const t = document.getElementById(a.getAttribute('data-target'));
            if (t) {{ ev.preventDefault(); t.scrollIntoView({{ block: 'start', behavior: 'smooth' }}); }}
        }};
    }});
    // "Show numbers" toggle reveals the in-cell values on every agreement grid.
    const sn = document.getElementById('show-numbers');
    if (sn) sn.addEventListener('change', () => {{
        document.body.classList.toggle('shownums', sn.checked);
    }});
    // Light / dark theme: initial from prefers-color-scheme, flipped by the
    // toggle (owner-ruled a light theme yes). The charts read the theme
    // variables when drawn and are re-coloured on the flip.
    const root = document.documentElement;
    try {{ if (window.matchMedia && window.matchMedia('(prefers-color-scheme: light)').matches) root.classList.add('theme-light'); }} catch (e) {{}}
    const tt = document.getElementById('theme-toggle');
    if (tt) tt.addEventListener('click', () => {{
        root.classList.toggle('theme-light');
        rethemeCharts();
    }});
    // σ view: DISABLED. A coherent Mahalanobis view has to switch the key, the
    // worst column, the summary line and every hover to σ together with the
    // cells; the agreement heatmap carries only a one-decimal σ span and a bin
    // per cell, not the per-object σ those elements need, and rebuilding it from
    // the dataset is out of scope for this pass. Rather than ship the half-built
    // view the reviewer flagged (nearly every cell reading "0.0σ" in the lowest
    // tint under a metre key), the control stays disabled with the reason on
    // hover; the frozen protected markup (data-sbin / cn-sig) is left untouched.
}}

// The Part 1 compare bar is pinned to the viewport top only while the reader is
// inside Part 1. position:sticky pins it; this one passive scroll listener
// releases it at the Part 2 head (.p1b-released — the sections are flex-ordered
// siblings, so a bare sticky would pin through the whole page) and publishes
// --sticky-offset (the bar's height while pinned, 0 otherwise) so the sticky
// heatmap headers stack directly below it. --bar-h (the bar's height) feeds the
// Part 1 sections' scroll-margin. Runs once on load so a deep link / reload
// mid-page settles into the right pinned/released state with no layout shift.
function wireCompareBar() {{
    const bar = document.getElementById('part1-bar');
    const p1 = document.getElementById('part1');
    const p2 = document.getElementById('part2');
    if (!bar || !p1 || !p2) return;
    const root = document.documentElement;
    let queued = false;
    function measure() {{ root.style.setProperty('--bar-h', bar.offsetHeight + 'px'); }}
    function update() {{
        queued = false;
        const barH = bar.offsetHeight;
        const released = p2.getBoundingClientRect().top <= barH;   // Part 2 head reached the bar
        const pinned = p1.getBoundingClientRect().top <= 0 && !released;
        bar.classList.toggle('p1b-released', released);
        bar.classList.toggle('p1b-pinned', pinned);   // shadow only while floating over content
        root.style.setProperty('--sticky-offset', pinned ? barH + 'px' : '0px');
    }}
    function onScroll() {{ if (!queued) {{ queued = true; requestAnimationFrame(update); }} }}
    measure();
    update();
    window.addEventListener('scroll', onScroll, {{ passive: true }});
    window.addEventListener('resize', () => {{ measure(); update(); }}, {{ passive: true }});
}}

// ─────────── helpers ───────────
// Client twin of the server `uhdr`: wrap a header fragment carrying a symbol,
// a variable or a unit in the one no-transform span so the uppercasing CSS
// leaves its case intact.
function uhdr(s) {{ return '<span class="u">' + s + '</span>'; }}
// Column scanning aid: one delegated mouseover per heatmap table (never a
// per-cell listener) tags every cell in the hovered column with .col-hi; the row
// aid is pure CSS (tr:hover). Applied to the server-rendered agrid tables that
// are in the DOM at load — the tall agreement / cost / OD / Part 2 grids.
function wireHeatmapCrosshair() {{
    document.querySelectorAll('table.agrid').forEach(tbl => {{
        let curCol = -1;
        const clear = () => {{ tbl.querySelectorAll('.col-hi').forEach(c => c.classList.remove('col-hi')); curCol = -1; }};
        tbl.addEventListener('mouseover', e => {{
            const cell = e.target.closest('td, th');
            if (!cell || !tbl.contains(cell)) return;
            const idx = cell.cellIndex;
            if (idx === curCol) return;
            clear();
            curCol = idx;
            if (idx < 0) return;
            tbl.querySelectorAll('tr').forEach(tr => {{ const c = tr.children[idx]; if (c) c.classList.add('col-hi'); }});
        }});
        tbl.addEventListener('mouseleave', clear);
    }});
}}
function pct(arr, p) {{ if (!arr.length) return NaN; const s = arr.slice().sort((a,b)=>a-b); return s[Math.min(s.length-1, Math.floor(s.length*p))]; }}
function median(arr) {{ return pct(arr, 0.5); }}
function vecDrKm(a, b) {{ if (!a || !b) return null; const dx = a[0]-b[0], dy = a[1]-b[1], dz = a[2]-b[2]; return Math.sqrt(dx*dx+dy*dy+dz*dz)*AU_KM; }}
function uniq(arr) {{ return [...new Set(arr)]; }}
function popLabel(p) {{ return p; }}

// Chart colours come from the page's theme variables, read when a chart is
// drawn, so a chart always sits on the page's own surface in either theme.
function themeVar(name) {{
    return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}}
const baseLayout = {{
    get paper_bgcolor() {{ return themeVar('--ed-bg'); }},
    get plot_bgcolor() {{ return themeVar('--ed-bg'); }},
    get font() {{ return {{ family: 'JetBrains Mono', size: 10, color: themeVar('--ed-text-muted') }}; }},
    legend: {{ font: {{ size: 9 }}, bgcolor: 'rgba(0,0,0,0)' }},
    margin: {{ l: 60, r: 20, t: 50, b: 50 }},
    hovermode: 'closest',
}};
// One marker size and one horizontal-legend placement shared by the error-growth
// chart family (both growth charts and the STM chart), so the family cannot drift.
const MARKER = 5;
const LEGEND_H = {{ orientation: 'h', y: 1.14 }};
// Covariance-panel title and legend, each defined once so the three panels
// cannot drift. The title takes a theme-aware token (legible on the light theme
// and re-coloured by rethemeCharts on a flip) in place of the old hardcoded
// ghost-grey, and the legend sits below the plot so it never overprints the
// title in a narrow panel.
function covTitle(text) {{ return {{ text, font: {{ size: 11, color: themeVar('--ed-text-secondary') }}, x: 0.02 }}; }}
const COV_LEGEND = {{ ...baseLayout.legend, orientation: 'h', y: -0.18, yanchor: 'top', x: 0 }};
function ax(title, type) {{
    const a = {{ title: {{ text: title, font: {{ size: 10 }} }}, gridcolor: themeVar('--ed-border'), zerolinecolor: themeVar('--ed-border'), color: themeVar('--ed-text-muted') }};
    if (type) a.type = type;
    return a;
}}
// Re-colour every drawn chart after the theme flips: surfaces, text and every
// axis the chart has (subplots carry xaxis2, yaxis2, ...).
function rethemeCharts() {{
    if (!window.Plotly) return;
    document.querySelectorAll('.js-plotly-plot').forEach(gd => {{
        const upd = {{ paper_bgcolor: themeVar('--ed-bg'), plot_bgcolor: themeVar('--ed-bg'), 'font.color': themeVar('--ed-text-muted'), 'title.font.color': themeVar('--ed-text-secondary') }};
        for (const k of Object.keys(gd.layout || {{}})) {{
            if (!/^[xy]axis\d*$/.test(k)) continue;
            upd[k + '.color'] = themeVar('--ed-text-muted');
            upd[k + '.zerolinecolor'] = themeVar('--ed-border');
            upd[k + '.gridcolor'] = themeVar('--ed-border');
        }}
        try {{ Plotly.relayout(gd, upd); }} catch (e) {{ console.error('retheme failed for', gd.id, e); }}
    }});
}}

// ─────────── boot: everything that reads the fetched dataset ───────────
// Runs ONLY after loadDataset() has fetched, byte-length-verified, FNV-verified,
// parsed and row-count-verified the sibling dataset and installed it in `results`.
// The Tool / Reference selects, showPairPanels and page nav wire up before this
// and never need the dataset; the client charts render here and follow whatever
// pair is selected when boot runs (boot ends by calling applyToolSelection()).
function boot() {{
// Builders render into clean containers. Plotly appends rather than replacing, so
// the "dataset not loaded" placeholders must be removed before anything draws, or
// the placeholder would sit beside the rendered chart.
document.querySelectorAll('.dataset-pending').forEach(el => el.remove());
DT_OBJECTS = (function () {{
    const m = new Map();
    for (const r of results) {{ if (r.ic_non_grav_dt != null && !m.has(r.object)) m.set(r.object, r.ic_non_grav_dt); }}
    return m;
}})();
TOOL_AXIS_DATA = computeToolAxisData();

// ─────────── per-channel and per-test-type slices ───────────
const ALL_CHANNELS = uniq(results.map(r => r.channel));
const rustResults = results.filter(r => r.channel === 'rust');
const propResults = rustResults.filter(r => r.test_type === 'propagation');
const ephResults = rustResults.filter(r => r.test_type === 'ephemeris');
const odResultsAll = results.filter(r => r.test_type === 'orbit_determination' || r.test_type === 'orbit_determination_radar');
const odRust = odResultsAll.filter(r => r.channel === 'rust');
const objectNames = uniq(propResults.map(r => r.object));
const popNames = uniq(propResults.map(r => r.population));

// ─────────── Section 03: Propagation error growth ───────────
// Propagation rows to read the pairwise diff off: core carries every external
// diff (emp_vs_assist_km, oorb_vs_horizons_km, …) AND its own emp_vs_horizons_km,
// so prefer it; fall back to rust when the core channel is absent.
// The `none_detection_off` arms are timing-only: their propagated state is that
// of `none_detection_on` (bit-identical), so they must never enter the accuracy
// / error-growth views, where an extra
// near-duplicate row per (object, dt, tier) would inflate counts without
// adding an independent measurement. They surface only in the timing panels.
const isTimingOnlyMode = m => !!m && m.includes('detection_off');
const propBase = (() => {{
    const c = results.filter(r => r.channel === 'core' && r.test_type === 'propagation'
        && !isTimingOnlyMode(r.propagation_uncertainty));
    return c.length ? c : propResults;
}})();
// ─────────── §1.1B propagation error growth (single per-population chart) ───────────
// One line per population: the median |tool − reference| position offset at
// each horizon, log y, driven by the tool-pair selector above. When the tool
// is ASSIST, a labelled dashed series traces the objects ASSIST cannot model
// (the Marsden-ΔT comets and the self-perturbers), because that gap is a
// force-model difference, not an integrator one.
const GROWTH_GAP_OBJS = new Set([...DT_OBJECTS.keys()]);
for (const r of results) {{ if (r.population === 'Self-Perturber') GROWTH_GAP_OBJS.add(r.object); }}
// ─────────── shared error-growth chart drawer ───────────
// ONE function draws BOTH error-growth charts (propagation and ephemeris) so
// their look cannot drift: per population, a filled 25-75 percent band under a
// median line with markers, log y, fixed population order (popNames), one median
// estimator (median()), one hover shape, one legend placement. Everything
// axis-specific is a parameter of `opts`:
//   base       source rows (core-preferred)
//   pops       population names, in fixed catalogue order
//   valFn      row -> |tool - reference| magnitude for this axis (null to skip)
//   quantity   axis-title quantity ('position' | 'angular sep')
//   unit       'km' | 'mas'
//   fmt        Plotly hover number format for the unit ('.3g' | '.3f')
//   legalKind  legalPair axis key ('prop_pos' | 'eph')
//   noun       illegal-pair message noun ('propagation-position' | 'ephemeris')
//   extra      optional (dts) -> extra traces (the ASSIST model-gap dashed line)
//   refLines   optional [[y, label], ...] dotted reference guides
function drawGrowthChart(elId, opts) {{
    const el = document.getElementById(elId);
    if (!el) return;
    // Draw for any legal pair (both tools carry a stored difference); the same
    // rule the heatmap panels use. An illegal pair shows the not-compared
    // placeholder, pair-neutral (only the selected pair's labels).
    if (!legalPair(TOOL1, TOOL2, opts.legalKind)) {{
        Plotly.react(elId, [], {{
            ...baseLayout,
            xaxis: {{ visible: false }}, yaxis: {{ visible: false }},
            annotations: [{{ text: `<b>${{toolLabel(TOOL1)}} vs ${{toolLabel(TOOL2)}}</b> has no ${{opts.noun}} comparison stored for this pair.`, showarrow: false, font: {{ color: themeVar('--ed-text-muted'), size: 12 }}, x: 0.5, y: 0.5, xref: 'paper', yref: 'paper' }}],
        }}, {{ responsive: true }});
        return;
    }}
    const val = opts.valFn;
    const dts = uniq(opts.base.map(r => r.dt_days)).sort((a, b) => a - b);
    const traces = [];
    for (const pop of opts.pops) {{
        const rows = opts.base.filter(r => r.population === pop);
        // A log axis cannot show an exact zero (tool and reference share the
        // state at the epoch), so the line breaks at a gap and the band is drawn
        // per contiguous segment; no floor value stands in for the zero.
        const xs = [], ys = [], bandX = [], bandY = [];
        let seg = {{ x: [], lo: [], hi: [] }};
        const closeSeg = () => {{
            if (seg.x.length) {{
                if (bandX.length) {{ bandX.push(null); bandY.push(null); }}
                bandX.push(...seg.x, ...seg.x.slice().reverse());
                bandY.push(...seg.hi, ...seg.lo.slice().reverse());
            }}
            seg = {{ x: [], lo: [], hi: [] }};
        }};
        for (const dt of dts) {{
            const vals = rows.filter(r => r.dt_days === dt).map(val).filter(v => v != null && isFinite(v) && v > 0);
            const m = vals.length ? median(vals) : null;
            if (m == null) {{ if (xs.length) {{ xs.push(dt); ys.push(null); }} closeSeg(); continue; }}
            xs.push(dt); ys.push(m);
            seg.x.push(dt); seg.lo.push(pct(vals, 0.25)); seg.hi.push(pct(vals, 0.75));
        }}
        closeSeg();
        if (!ys.some(v => v != null)) continue;
        const color = popColors[pop] || '#888';
        // The population's 25-75 percent band under its median line.
        traces.push({{ x: bandX, y: bandY,
            fill: 'toself', fillcolor: color + '22', line: {{ color: 'rgba(0,0,0,0)' }},
            hoverinfo: 'skip', showlegend: false, legendgroup: pop, name: pop }});
        traces.push({{ x: xs, y: ys, mode: 'lines+markers', line: {{ color, width: 2 }}, marker: {{ size: MARKER }}, name: pop, legendgroup: pop, hovertemplate: `${{pop}} median<br>dt: %{{x}}d<br>%{{y:${{opts.fmt}}}} ${{opts.unit}}<extra></extra>` }});
    }}
    if (opts.extra) {{ for (const t of opts.extra(dts)) traces.push(t); }}
    if (opts.refLines) {{
        const xMin = dts[0], xMax = dts[dts.length - 1];
        for (const [y, label] of opts.refLines) {{
            traces.push({{ x: [xMin, xMax], y: [y, y], mode: 'lines', line: {{ color: '#5b9bd540', width: 1, dash: 'dot' }}, name: label, showlegend: true }});
        }}
    }}
    Plotly.react(elId, traces, {{
        ...baseLayout,
        xaxis: ax('dt (days from epoch)'),
        yaxis: ax(`|${{toolLabel(TOOL1)}} − ${{toolLabel(TOOL2)}}| ${{opts.quantity}} (${{opts.unit}})`, 'log'),
        showlegend: true,
        legend: {{ ...baseLayout.legend, ...LEGEND_H }},
    }}, {{ responsive: true, displayModeBar: 'hover', modeBarButtonsToRemove: ['select2d', 'lasso2d', 'autoScale2d', 'toggleSpikelines'] }});
}}
// Propagation error growth (#growth-chart) through the shared drawer. Its extra
// series is the ASSIST model gap — the objects ASSIST cannot model, drawn only
// when ASSIST is one of the selected tools (a force-model gap, not an integrator
// one). Propagation carries no reference lines.
function redrawGrowthProp() {{
    drawGrowthChart('growth-chart', {{
        base: propBase, pops: popNames,
        valFn: r => propPosDiffKm(r, TOOL1, TOOL2),
        quantity: 'position', unit: 'km', fmt: '.3g',
        legalKind: 'prop_pos', noun: 'propagation-position',
        refLines: null,
        extra: dts => {{
            if (TOOL1 !== 'assist' && TOOL2 !== 'assist') return [];
            const gapRows = propBase.filter(r => GROWTH_GAP_OBJS.has(r.object));
            const xs = [], ys = [];
            for (const dt of dts) {{
                const vals = gapRows.filter(r => r.dt_days === dt).map(r => propPosDiffKm(r, TOOL1, TOOL2)).filter(v => v != null && isFinite(v) && v > 0);
                xs.push(dt); ys.push(vals.length ? median(vals) : null);
            }}
            return ys.some(v => v != null) ? [{{ x: xs, y: ys, mode: 'lines', line: {{ color: '#e8a040', width: 1.5, dash: 'dash' }}, name: 'ASSIST · model gap', hovertemplate: `ASSIST model gap<br>dt: %{{x}}d<br>%{{y:.3g}} km<extra></extra>` }}] : [];
        }},
    }});
}}
onToolChange(redrawGrowthProp);

function fmtErrorKm(km) {{
    if (km == null) return '---';
    if (km < 0.001) return (km * 1e6).toFixed(1) + ' mm';
    if (km < 1.0) return (km * 1000.0).toFixed(1) + ' m';
    if (km < 1000.0) return km.toFixed(2) + ' km';
    if (km < 1e6) return km.toFixed(0) + ' km';
    return (km / AU_KM).toFixed(3) + ' AU';
}}
// ─────────── Section 04: ASSIST comparison ───────────
// The external-tool comparison fields (assist_vs_horizons_km, emp_vs_assist_km,
// assist_time_ms) are merged onto whichever channel is the merge target —
// `core` under WITH_CORE, else `rust` — NOT necessarily the rust rows the prop
// plots key off. So select ASSIST rows by field presence across all channels.
// Exclude the standalone per-arm ASSIST rows (channel="assist") — this panel
// reads the single asteroid_institute value folded onto the merge-target
// (core/rust) rows; the six-arm comparison lives in its own section.
const assistResults = results.filter(r => r.channel !== 'assist' && r.test_type === 'propagation' && r.assist_vs_horizons_km != null);
if (assistResults.length > 0) {{
    // First-order STM cross-validation. Series 1: Empyrean's Jet1 STM vs the
    // corrected ASSIST variational STM over ALL first-order rows — two
    // independent analytic methods — as ‖Φ_emp − Φ_ast‖/‖Φ_emp‖, median (solid)
    // and 95th percentile (dashed) per |dt|. Series 2: Empyrean vs an ASSIST
    // single-particle finite difference on the representative subset, with the
    // measured FD step-size floor (~1e-8) as a dotted line. dt=0 rows (identity
    // vs identity) are dropped so the log axis is not pinned to zero.
    (function buildStmXval() {{
        const el = document.getElementById('stm-agree-chart');
        if (!el) return;
        const relTo = (a, ref) => {{
            let dn = 0, en = 0;
            for (let i = 0; i < 6; i++) for (let j = 0; j < 6; j++) {{
                const d = a[i][j] - ref[i][j];
                dn += d * d; en += ref[i][j] * ref[i][j];
            }}
            return en > 0 ? Math.sqrt(dn) / Math.sqrt(en) : null;
        }};
        const FLOOR = 1e-18, FD_FLOOR = 1e-8;
        const quant = (arr, p) => {{
            if (!arr.length) return null;
            const s = arr.slice().sort((x, y) => x - y);
            return s[Math.min(s.length - 1, Math.floor(s.length * p))];
        }};
        const foRows = results.filter(r => r.channel === 'core'
            && r.test_type === 'propagation'
            && r.propagation_uncertainty === 'first_order_detection_on'
            && Array.isArray(r.emp_stm));
        const traces = [];
        const allDts = new Set();
        // Series 1: Empyrean vs fixed ASSIST variational, all rows carrying both.
        {{
            const byDt = new Map();
            for (const r of foRows) {{
                if (!Array.isArray(r.assist_stm)) continue;
                const adt = Math.abs(r.dt_days);
                if (adt === 0) continue;
                const rel = relTo(r.assist_stm, r.emp_stm);
                if (rel == null || !isFinite(rel)) continue;
                if (!byDt.has(adt)) byDt.set(adt, []);
                byDt.get(adt).push(rel);
            }}
            const dts = Array.from(byDt.keys()).sort((a, b) => a - b);
            dts.forEach(d => allDts.add(d));
            const med = dts.map(d => Math.max(quant(byDt.get(d), 0.5), FLOOR));
            const p95 = dts.map(d => Math.max(quant(byDt.get(d), 0.95), FLOOR));
            const nrows = dts.map(d => byDt.get(d).length);
            traces.push({{ x: dts, y: med, mode: 'lines+markers', name: 'empyrean vs ASSIST variational (median)',
                line: {{ color: '#5b9bd5', width: 2 }}, marker: {{ color: '#5b9bd5', size: MARKER }}, text: nrows,
                hovertemplate: '|dt| %{{x}}d<br>median ‖Φ_emp−Φ_ast‖/‖Φ_emp‖ %{{y:.2e}}<br>%{{text}} rows<extra></extra>' }});
            traces.push({{ x: dts, y: p95, mode: 'lines', name: 'empyrean vs ASSIST variational (p95)',
                line: {{ color: '#5b9bd5', width: 1.5, dash: 'dash' }},
                hovertemplate: '|dt| %{{x}}d<br>p95 %{{y:.2e}}<extra></extra>' }});
        }}
        // Series 2: Empyrean vs finite difference on the representative subset.
        {{
            const byDt = new Map();
            for (const r of foRows) {{
                if (!Array.isArray(r.fd_stm)) continue;
                const adt = Math.abs(r.dt_days);
                if (adt === 0) continue;
                const rel = relTo(r.emp_stm, r.fd_stm);
                if (rel == null || !isFinite(rel)) continue;
                if (!byDt.has(adt)) byDt.set(adt, []);
                byDt.get(adt).push(rel);
            }}
            const dts = Array.from(byDt.keys()).sort((a, b) => a - b);
            dts.forEach(d => allDts.add(d));
            const med = dts.map(d => Math.max(quant(byDt.get(d), 0.5), FLOOR));
            const nrows = dts.map(d => byDt.get(d).length);
            if (dts.length) traces.push({{ x: dts, y: med, mode: 'lines+markers', name: 'empyrean vs finite difference (subset)',
                line: {{ color: '#e8a040', width: 2 }}, marker: {{ color: '#e8a040', size: MARKER, symbol: 'diamond' }}, text: nrows,
                hovertemplate: '|dt| %{{x}}d<br>median ‖Φ_emp−Φ_FD‖/‖Φ_FD‖ %{{y:.2e}}<br>%{{text}} bodies<extra></extra>' }});
        }}
        if (!traces.length) {{
            Plotly.react(el, [], {{ ...baseLayout, xaxis: {{ visible: false }}, yaxis: {{ visible: false }},
                annotations: [{{ text: 'No STM cross-validation rows.', showarrow: false, font: {{ color: '#8b9198', size: 12 }}, x: 0.5, y: 0.5, xref: 'paper', yref: 'paper' }}] }}, {{ responsive: true }});
            return;
        }}
        const dtsAll = Array.from(allDts).sort((a, b) => a - b);
        traces.push({{ x: dtsAll, y: dtsAll.map(() => FD_FLOOR), mode: 'lines', name: 'finite-difference floor (~1e-8)',
            line: {{ color: '#8b9198', width: 1.5, dash: 'dot' }}, hoverinfo: 'skip' }});
        Plotly.react(el, traces, {{
            ...baseLayout,
            xaxis: ax('|dt| (days from epoch)'),
            yaxis: ax('rel. Frobenius difference (log)', 'log'),
            showlegend: true,
            legend: {{ ...baseLayout.legend, ...LEGEND_H }},
        }}, {{ responsive: true, displayModeBar: 'hover', modeBarButtonsToRemove: ['select2d', 'lasso2d', 'autoScale2d', 'toggleSpikelines'] }});
    }})();
}}

// ─────────── Section 01c: ASSIST six-arm configuration comparison ───────────
// Reads the standalone per-arm ASSIST rows (channel="assist") appended by
// merge-external. Three configurations × variational off/on; per-horizon median
// timing, per-configuration position agreement vs Empyrean at 15 yr, and
// per-variational-arm STM agreement. Capped (FAIL) rows carry a null
// assist_time_ms and are reported as a count, never a substituted number.
(function buildAssistArms() {{
    const arows = results.filter(r => r.channel === 'assist' && r.test_type === 'propagation');
    if (!arows.length) return;
    const s = document.getElementById('s05c');
    if (s) s.style.display = '';
    const byArm = {{}};
    for (const r of arows) {{ (byArm[r.propagation_uncertainty] = byArm[r.propagation_uncertainty] || []).push(r); }}
    const med = xs => {{ const v = xs.filter(x => x != null && isFinite(x)).sort((a, b) => a - b); return v.length ? v[Math.floor((v.length - 1) / 2)] : null; }};
    const configs = [
        ['assist_default', 'assist_default_variational_off', 'assist_default_variational_on', 'ASSIST default (mode 1, ε=1e-9)'],
        ['assist_layup', 'assist_layup_variational_off', 'assist_layup_variational_on', 'ASSIST layup (mode 2, ε=1e-9)'],
        ['assist_asteroid_institute', 'assist_asteroid_institute_variational_off', 'assist_asteroid_institute_variational_on', 'ASSIST asteroid-institute (mode 1, ε=1e-6)'],
    ];
    const H = [[365, '1 yr'], [1095, '3 yr'], [5475, '15 yr']];
    const atH = (arm, h) => med((byArm[arm] || []).filter(r => Math.abs(Math.abs(r.dt_days) - h) < 1).map(r => r.assist_time_ms));
    const cappedOf = arm => (byArm[arm] || []).filter(r => r.assist_time_ms == null).length;
    // Timing table.
    let t = '<table class="od-table" style="font-size:11px;"><thead><tr><th style="text-align:left">Arm</th>';
    for (const [, lab] of H) t += `<th>${{lab}}</th>`;
    t += '<th>capped</th></tr></thead><tbody>';
    for (const [, off, on, lab] of configs) {{
        for (const [arm, sub] of [[off, 'f64'], [on, 'variational']]) {{
            t += `<tr><td style="text-align:left">${{lab.split(' (')[0]}} <span style="color:var(--ed-text-muted)">${{sub}}</span></td>`;
            for (const [h] of H) {{ const m = atH(arm, h); t += `<td>${{m == null ? '—' : m.toFixed(2) + ' ms'}}</td>`; }}
            const c = cappedOf(arm); t += `<td>${{c ? '<b style=\"color:#c0504d\">' + c + '</b>' : '0'}}</td></tr>`;
        }}
    }}
    t += '</tbody></table>';
    const te = document.getElementById('assist-arms-timing'); if (te) te.innerHTML = t;
    // Position agreement per configuration at 3 yr and 15 yr (variational-off
    // row). Rows for objects carrying a Marsden ΔT term are excluded from each
    // median — ASSIST models no ΔT, so its position for those objects is not
    // comparable to empyrean's (which applies it) — and counted separately; the
    // all-rows median is shown alongside so the shift is visible, and the
    // excluded rows stay in the data, marked, never dropped.
    const POSH = [[1095, '3 yr'], [5475, '15 yr']];
    let p = '<table class="od-table" style="font-size:11px;"><thead><tr><th style="text-align:left">Configuration</th>';
    for (const [, hl] of POSH) p += `<th>median |emp − ast| @ ${{hl}}<br/><small style="font-weight:300; opacity:0.6">${{uhdr('ΔT')}}-excluded (all rows)</small></th>`;
    p += '<th>rows</th><th>' + uhdr('ΔT excl') + '</th></tr></thead><tbody>';
    let posExcl = 0;
    for (const [, off, , lab] of configs) {{
        p += `<tr><td style="text-align:left">${{lab}}</td>`;
        let kept15 = 0, excl15 = 0;
        for (const [H] of POSH) {{
            const rr = (byArm[off] || []).filter(r => Math.abs(Math.abs(r.dt_days) - H) < 1 && r.emp_vs_assist_km != null);
            const kept = rr.filter(r => !isDtObject(r.object));
            const mk = med(kept.map(r => r.emp_vs_assist_km));
            const ma = med(rr.map(r => r.emp_vs_assist_km));
            if (H === 5475) {{ kept15 = kept.length; excl15 = rr.length - kept.length; }}
            posExcl += rr.length - kept.length;
            p += `<td>${{mk == null ? '—' : fmtErrorKm(mk)}} <small style="opacity:0.6">(${{ma == null ? '—' : fmtErrorKm(ma)}})</small></td>`;
        }}
        p += `<td>${{kept15}}</td><td>${{excl15 ? '<b>' + excl15 + '</b>' : '0'}}</td></tr>`;
    }}
    p += '</tbody></table>';
    if (posExcl > 0) {{
        const dtNames = [...DT_OBJECTS.keys()].join(', ');
        p += `<div class="disclosures" style="margin-top:6px"><details><summary>ΔT exclusion</summary><div class="disc-body" style="font-size:10px">ASSIST does not model the non-gravitational delay ΔT these solutions carry, so its positions are not comparable; empyrean applies it. The ΔT-bearing objects (${{dtNames}}) are excluded from every median above and counted at right (ΔT excl) — kept in the data, marked, never dropped. The all-rows median is in parentheses.</div></details></div>`;
    }}
    const pe = document.getElementById('assist-arms-pos'); if (pe) pe.innerHTML = p;
    // STM agreement per variational arm (relative Frobenius, all horizons).
    const relTo = (a, ref) => {{
        if (!Array.isArray(a) || !Array.isArray(ref)) return null;
        let dn = 0, en = 0;
        for (let i = 0; i < 6; i++) for (let j = 0; j < 6; j++) {{ const d = a[i][j] - ref[i][j]; dn += d * d; en += ref[i][j] * ref[i][j]; }}
        return en > 0 ? Math.sqrt(dn) / Math.sqrt(en) : null;
    }};
    let m2 = '<table class="od-table" style="font-size:11px;"><thead><tr><th style="text-align:left">Configuration (variational)</th><th>' + uhdr('median ‖Φ_emp − Φ_ast‖/‖Φ_emp‖') + '</th><th>rows</th></tr></thead><tbody>';
    for (const [, , on, lab] of configs) {{
        const rels = (byArm[on] || []).filter(r => Math.abs(r.dt_days) > 0 && Array.isArray(r.assist_stm) && Array.isArray(r.emp_stm))
            .map(r => relTo(r.assist_stm, r.emp_stm)).filter(v => v != null && isFinite(v));
        const m = med(rels);
        m2 += `<tr><td style="text-align:left">${{lab}}</td><td>${{m == null ? '—' : m.toExponential(2)}}</td><td>${{rels.length}}</td></tr>`;
    }}
    m2 += '</tbody></table>';
    const me = document.getElementById('assist-arms-stm'); if (me) me.innerHTML = m2;
    // Resolved settings per configuration, from a non-capped row's notes (the
    // runner read them off the simulation AFTER attach + config).
    let ss = '<table class="od-table" style="font-size:11px;"><thead><tr><th style="text-align:left">Configuration</th><th style="text-align:left">Resolved settings (read after attach)</th></tr></thead><tbody>';
    for (const [cfg, off] of configs) {{
        const r = (byArm[off] || []).find(x => x.notes && x.notes.indexOf('config=') >= 0);
        const note = r ? r.notes.slice(r.notes.indexOf('config=')) : '—';
        ss += `<tr><td style="text-align:left">${{cfg}}</td><td style="text-align:left"><code>${{note}}</code></td></tr>`;
    }}
    ss += '</tbody></table>';
    const se = document.getElementById('assist-arms-settings'); if (se) se.innerHTML = ss;
}})();

// ─────────── Section 06: Ephemeris angular separation ───────────
// Ephemeris rows carrying the pairwise offsets: core has Empyrean's d_ra/d_dec
// AND oorb_d_ra/d_dec (both signed vs Horizons), so prefer core; fall back to rust.
// Each object/epoch is observed from several sites (OBSERVER_CODES); collapse
// them to ONE site-mean row per (object, dt, tier) so every heatmap cell /
// growth point is an average over the observing geometries — robust to any
// single site's parallax / light-time handling. n_sites + the site spread
// (sep_min / sep_max) ride along so the hover can surface a divergent site
// rather than let it hide behind the mean (no hidden fallbacks).
const ephBase = (() => {{
    const src = results.filter(r => r.channel === 'core' && r.test_type === 'ephemeris');
    const rows = src.length ? src : ephResults;
    const AVG = ['separation_arcsec', 'd_ra_arcsec', 'd_dec_arcsec',
                 'oorb_separation_arcsec', 'oorb_d_ra_arcsec', 'oorb_d_dec_arcsec',
                 'findorb_separation_arcsec', 'findorb_d_ra_arcsec', 'findorb_d_dec_arcsec'];
    // Key on the uncertainty mode too, so we average over observing SITES
    // only — not across the first_order / none rows that eph
    // is doubled over (they carry the same RA/Dec but must stay distinct rows).
    const groups = new Map();
    for (const r of rows) {{
        const k = r.object + '|' + r.dt_days + '|' + r.force_model + '|' + (r.propagation_uncertainty || '');
        if (!groups.has(k)) groups.set(k, []);
        groups.get(k).push(r);
    }}
    const out = [];
    for (const g of groups.values()) {{
        const base = {{ ...g[0] }};
        for (const f of AVG) {{
            const vals = g.map(r => r[f]).filter(v => v != null);
            base[f] = vals.length ? vals.reduce((a, b) => a + b, 0) / vals.length : null;
        }}
        const seps = g.map(r => r.separation_arcsec).filter(v => v != null);
        base.n_sites = g.length;
        base.sep_min = seps.length ? Math.min(...seps) : null;
        base.sep_max = seps.length ? Math.max(...seps) : null;
        base.observer = g.length > 1 ? (g.length + '-site mean') : g[0].observer;
        // Element-wise mean of the 2×2 RA/Dec covariance over the sites, so the
        // Mahalanobis-σ view uses the mean covariance against the mean offset.
        const covs = g.map(r => r.emp_radec_cov_arcsec2).filter(c => c != null);
        if (covs.length) {{
            base.emp_radec_cov_arcsec2 = [[0, 0], [0, 0]];
            for (const c of covs) for (let i = 0; i < 2; i++) for (let j = 0; j < 2; j++) base.emp_radec_cov_arcsec2[i][j] += c[i][j] / covs.length;
        }}
        out.push(base);
    }}
    return out;
}})();
let s06Mode = 'pair';
// The "empyrean channels" overlay: each distribution channel's own median
// angular separation (a different view from the per-population pair chart the
// shared drawer renders — no per-population band, no reference lines).
function drawEphChannels() {{
    const traces = [];
    const dts = uniq(ephBase.map(r => r.dt_days)).sort((a, b) => a - b);
    for (const ch of ALL_CHANNELS) {{
        const chR = results.filter(r => r.channel === ch && r.test_type === 'ephemeris' && r.separation_arcsec != null);
        const xs = [], ys = [];
        for (const dt of dts) {{
            const vals = chR.filter(r => r.dt_days === dt).map(r => r.separation_arcsec * 1000);
            if (!vals.length) continue;
            xs.push(dt); ys.push(median(vals));
        }}
        const color = channelColors[ch] || '#888';
        traces.push({{ x: xs, y: ys, mode: 'lines+markers',
            line: {{ color: color, width: 2 }}, marker: {{ size: MARKER }}, name: ch,
            hovertemplate: `${{ch}} median<br>dt: %{{x}}d<br>%{{y:.3f}} mas<extra></extra>` }});
    }}
    Plotly.react('eph-sep-chart', traces, {{
        ...baseLayout,
        xaxis: ax('dt (days from epoch)'),
        yaxis: ax('Angular separation (mas)', 'log'),
        showlegend: true, legend: {{ ...baseLayout.legend, ...LEGEND_H }},
    }}, {{ responsive: true, displayModeBar: 'hover', modeBarButtonsToRemove: ['select2d', 'lasso2d', 'autoScale2d', 'toggleSpikelines'] }});
}}
// Ephemeris error growth (#eph-sep-chart). The per-population pair view goes
// through the SAME shared drawer as the propagation chart; the "empyrean
// channels" toggle swaps in the per-channel overlay above.
function redrawEph() {{
    if (s06Mode === 'all') {{ drawEphChannels(); return; }}
    drawGrowthChart('eph-sep-chart', {{
        base: ephBase, pops: popNames,
        valFn: r => {{ const s = ephSepArcsec(r, TOOL1, TOOL2); return s == null ? null : s * 1000; }},
        quantity: 'angular sep', unit: 'mas', fmt: '.3f',
        legalKind: 'eph', noun: 'ephemeris',
        extra: null,
        refLines: [[1, '1 mas (Gaia)'], [100, '100 mas (CCD)']],
    }});
}}
if (ephResults.length > 0) {{
    redrawEph();
    onToolChange(() => {{
        s06Mode = 'pair';
        document.querySelectorAll('#s06b-toggle button').forEach(b => b.classList.toggle('active', b.dataset.mode === 'rust'));
        redrawEph();
    }});
    document.querySelectorAll('#s06b-toggle button').forEach(btn => {{
        btn.onclick = () => {{
            document.querySelectorAll('#s06b-toggle button').forEach(b => b.classList.remove('active'));
            btn.classList.add('active');
            s06Mode = btn.dataset.mode === 'all' ? 'all' : 'pair';
            redrawEph();
        }};
    }});

    // ─────────── Section 07: clipped scatter + outliers ───────────
    // Tightened from ±300 to ±100 mas: the prior clip
    // hid the main residual cluster behind two pathological outliers,
    // and the 1σ/3σ ellipses fell well inside the axes. The outlier
    // sidecar table still lists every row beyond the clip, with the
    // channel column so a reviewer can tell whether the divergence is
    // rust-side or a replay-channel artefact. Now pairwise: dRA/dDec are
    // ephOffsets(TOOL1) − ephOffsets(TOOL2) (arcsec → mas), sourced from
    // ephBase (core-preferred) and gated by legalPair(...,'eph'); the
    // panel re-renders on every tool-pair change.
    const SCATTER_CLIP_MAS = 100;
    function buildEphScatter() {{
    const outBody = document.getElementById('eph-outliers');
    if (!outBody) return;
    if (!legalPair(TOOL1, TOOL2, 'eph')) {{ outBody.innerHTML = ''; return; }}
    const outliers = [];
    for (const r of ephBase) {{
        const o1 = ephOffsets(r, TOOL1), o2 = ephOffsets(r, TOOL2);
        if (!o1 || !o2) continue;
        const dra_mas = (o1[0] - o2[0]) * 1000, ddec_mas = (o1[1] - o2[1]) * 1000;
        const norm = Math.hypot(dra_mas, ddec_mas);
        if (norm > SCATTER_CLIP_MAS) outliers.push({{...r, dra_mas, ddec_mas, norm }});
    }}
    // Table of the rows beyond the grid. The channel column lets a reviewer see
    // whether a divergence is in the rust reference or only in one replay
    // channel; dedupe by (object, dt, observer, channel, residual).
    outBody.innerHTML = '';
    outliers.sort((a, b) => b.norm - a.norm);
    const seenOut = new Set();
    for (const o of outliers) {{
        const k = `${{o.object}}|${{o.dt_days}}|${{o.observer || ''}}|${{o.channel}}|${{o.dra_mas.toFixed(3)}}|${{o.ddec_mas.toFixed(3)}}`;
        if (seenOut.has(k)) continue;
        seenOut.add(k);
        const tr = document.createElement('tr');
        const chColor = channelColors[o.channel] || '#8b9198';
        tr.innerHTML = `<td class="obj-name">${{o.object}}</td><td><span class="pop-dot" style="background:${{chColor}}"></span>${{o.channel}}</td><td>${{o.dt_days}}d</td><td>${{o.observer || '—'}}</td><td>${{o.dra_mas.toFixed(1)}} mas</td><td>${{o.ddec_mas.toFixed(1)}} mas</td><td style="color:#e06252">${{o.norm.toFixed(1)}} mas</td>`;
        outBody.appendChild(tr);
    }}
    if (outliers.length === 0) {{
        const tr = document.createElement('tr');
        tr.innerHTML = `<td colspan="7" style="text-align:left; color:#5b9bd5">No rows beyond ±${{SCATTER_CLIP_MAS}} mas on this pair.</td>`;
        outBody.appendChild(tr);
    }}
    }}
    buildEphScatter();
    onToolChange(() => buildEphScatter());
}}

// ─────────── Section 09b: Non-gravitational recovery ───────────
// Rows tagged `non_grav_recovery` carry the fitted Marsden A1/A2/A3 and
// their 1σ (√ of the 9×9 state+A1/A2/A3 covariance diagonal). The σ are
// `None` (not 0, not NaN) when the StateAndNonGrav fit did not actually
// recover non-grav — the loud-failure case this section exists to catch.
// We render one sub-row per coefficient (A1/A2/A3) whose JPL reference is
// non-zero, per channel, with a |z| ≤ 3 σ-consistency PASS/FAIL.
const ngResults = results.filter(r => r.test_type === 'non_grav_recovery');
if (ngResults.length === 0) {{
    document.getElementById('ng-empty').style.display = '';
    document.getElementById('ng-content').style.display = 'none';
}} else {{
    document.getElementById('ng-empty').style.display = 'none';
    // Group by object, then channel — so every object shows all four
    // distribution channels side by side and a per-channel marshaling
    // drop of the fitted non-grav block is visible as a row of FAILs.
    const byObjNg = {{}};
    for (const r of ngResults) {{
        if (!byObjNg[r.object]) byObjNg[r.object] = {{}};
        byObjNg[r.object][r.channel] = r;
    }}
    const ngObjects = Object.keys(byObjNg).sort((a, b) => a.localeCompare(b));
    // Stable channel order so the four channels always read the same way.
    const NG_CHANNELS = ['rust', 'c', 'cli', 'python'];
    const overview = document.getElementById('ng-overview');
    // Compact scientific formatter for AU/day² values, which run ~1e-14.
    const fmtNg = (v) => (v == null || !isFinite(v)) ? null : v.toExponential(3);
    // Cells read only A1/A2/A3; radial/transverse/normal are defined once on the
    // Coeff column-header hover (NG_COEFF_DEFS), not repeated per row.
    const COEFFS = [
        {{ key: 'a1', label: 'A1' }},
        {{ key: 'a2', label: 'A2' }},
        {{ key: 'a3', label: 'A3' }},
    ];
    for (const obj of ngObjects) {{
        const row = byObjNg[obj];
        // Determine which coefficients have a non-zero JPL reference on
        // ANY channel (the reference is identical across channels, but be
        // defensive). Only those participate in the PASS/FAIL verdict.
        const refRow = NG_CHANNELS.map(ch => row[ch]).find(r => r) || {{}};
        const activeCoeffs = COEFFS.filter(c => {{
            const ic = refRow['ic_' + c.key];
            return ic != null && ic !== 0;
        }});
        // Fallback: if no reference coefficient is non-zero (shouldn't
        // happen for objects the runner tagged non_grav_recovery), still
        // show all three so the row isn't silently empty.
        const coeffs = activeCoeffs.length > 0 ? activeCoeffs : COEFFS;
        // First column spans every (channel × coeff) sub-row for this object.
        const totalSubRows = NG_CHANNELS.length * coeffs.length;
        let firstCellEmitted = false;
        for (let ci = 0; ci < NG_CHANNELS.length; ci++) {{
            const ch = NG_CHANNELS[ci];
            const r = row[ch];
            for (let ki = 0; ki < coeffs.length; ki++) {{
                const c = coeffs[ki];
                const tr = document.createElement('tr');
                // Faint separator between channels for scanability.
                if (ki === 0 && ci > 0) tr.style.borderTop = '1px solid #1a2332';
                let cells = '';
                // Object cell — rowspan across the whole object block.
                if (!firstCellEmitted) {{
                    cells += `<td class="obj" rowspan="${{totalSubRows}}" style="vertical-align:top">${{obj}}</td>`;
                    firstCellEmitted = true;
                }}
                // Channel cell — rowspan across this channel's coefficients.
                if (ki === 0) {{
                    const cColor = channelColors[ch] || '#888';
                    cells += `<td rowspan="${{coeffs.length}}" style="vertical-align:top; text-align:left; color:${{cColor}}">${{ch}}</td>`;
                }}
                cells += `<td style="text-align:left">${{c.label}}</td>`;
                if (!r) {{
                    // Channel produced no non_grav_recovery row at all —
                    // the entire fitted block is missing for this channel.
                    cells += `<td class="hatch" colspan="4" title="no non-grav row emitted for this channel"></td>`;
                    tr.innerHTML = cells;
                    overview.appendChild(tr);
                    continue;
                }}
                const fit = r['od_' + c.key];
                const sig = r['od_' + c.key + '_sigma'];
                const ic = r['ic_' + c.key];
                const jplCell = (ic != null) ? fmtNg(ic) : '—';
                // Loud-failure rule: a None (or non-finite) fit or σ means
                // the StateAndNonGrav fit did not recover non-grav — render
                // it explicitly in red, never as a blank or a zero.
                const fitOk = fit != null && isFinite(fit) && sig != null && isFinite(sig);
                if (!fitOk) {{
                    cells += `<td class="hatch" colspan="3" title="no non-grav covariance — fell back to state-only"></td>`;
                    cells += `<td style="color:#e06252; font-weight:600" title="fail · no non-grav covariance (fell back to state-only)">✕</td>`;
                    tr.innerHTML = cells;
                    overview.appendChild(tr);
                    continue;
                }}
                const fitStr = `${{fmtNg(fit)}} <span style="color:#8b9198">± ${{fmtNg(sig)}}</span>`;
                cells += `<td style="text-align:left">${{fitStr}}</td>`;
                cells += `<td style="text-align:left">${{jplCell}}</td>`;
                // σ-consistency z; only meaningful when JPL reference is
                // non-zero (otherwise there is nothing to be consistent with).
                if (ic == null || ic === 0) {{
                    cells += `<td style="color:#8b9198">—</td>`;
                    // n/a is the page's one hatched state; the word + reason ride the hover.
                    cells += `<td class="hatch" title="n/a · no non-zero JPL reference for this coefficient to test against"></td>`;
                }} else {{
                    const z = (fit - ic) / sig;
                    const zAbs = Math.abs(z);
                    const pass = zAbs <= 3;
                    const zColor = pass ? '#3d9a6d' : '#e06252';
                    cells += `<td style="color:${{zColor}}">${{z >= 0 ? '+' : '−'}}${{zAbs.toFixed(2)}}σ</td>`;
                    // Verdict reuses the OD grid's glyph vocabulary (· pass, ✕ fail);
                    // the word and |z| rule ride the cell hover, colour as before.
                    cells += `<td style="color:${{zColor}}; font-weight:600" title="${{pass ? 'pass · |z| ≤ 3σ' : 'fail · |z| > 3σ'}}">${{pass ? '·' : '✕'}}</td>`;
                }}
                tr.innerHTML = cells;
                overview.appendChild(tr);
            }}
        }}
    }}
}}

// ─────────── Section 12: Fitted orbit + covariance vs references ───────────
const orbitComparisons = ORBIT_COMPARISONS_JSON;
// Objects whose JPL solution used radar, decided server-side; the panels below
// read this set rather than re-deriving the exclusion.
const radarExcluded = new Set(RADAR_EXCLUDED_JSON);

// ─────────── Part 4 covariance realism panels (§8.1) ───────────
// Three plots beside the server-rendered coverage + per-object tables.
function buildCovariancePanels() {{
    if (!window.Plotly || !orbitComparisons || !orbitComparisons.length) return;
    const recs = orbitComparisons.filter(c => c.common_epoch_source === 'sbdb');
    const empChi = {{}}, jplChi = {{}}, arc = {{}}, popOf = {{}};
    for (const r of results) {{
        if (r.test_type === 'orbit_determination') {{
            if (r.channel === 'rust' && r.od_reduced_chi2 != null) empChi[r.object] = r.od_reduced_chi2;
            if (r.channel === 'core') {{ if (r.ref_od_reduced_chi2 != null) jplChi[r.object] = r.ref_od_reduced_chi2; if (r.ref_od_data_arc_days != null) arc[r.object] = r.ref_od_data_arc_days; }}
        }}
        if (!popOf[r.object]) popOf[r.object] = r.population;
    }}
    // Panel 1 — reduced χ², empyrean paired to JPL, per object.
    const objs1 = Object.keys(empChi).filter(o => jplChi[o] != null && !radarExcluded.has(o)).sort((a, b) => empChi[a] - empChi[b]);
    const t1 = [];
    objs1.forEach((o, i) => {{ t1.push({{ x: [i, i], y: [empChi[o], jplChi[o]], mode: 'lines', line: {{ color: '#3a4453', width: 1 }}, hoverinfo: 'skip', showlegend: false }}); }});
    t1.push({{ x: objs1.map((o, i) => i), y: objs1.map(o => empChi[o]), mode: 'markers', marker: {{ color: '#5b9bd5', size: 6 }}, name: 'empyrean', text: objs1, hovertemplate: '%{{text}}<br>empyrean χ²ᵣ %{{y:.3f}}<extra></extra>' }});
    t1.push({{ x: objs1.map((o, i) => i), y: objs1.map(o => jplChi[o]), mode: 'markers', marker: {{ color: '#d05080', size: 6, symbol: 'diamond' }}, name: 'JPL', text: objs1, hovertemplate: '%{{text}}<br>JPL χ²ᵣ %{{y:.3f}}<extra></extra>' }});
    Plotly.react('cov-chi2-panel', t1, {{ ...baseLayout, title: covTitle('1 · reduced χ² — empyrean paired to JPL'), margin: {{ l: 46, r: 8, t: 28, b: 58 }}, xaxis: {{ ...ax('object (sorted by empyrean)'), showticklabels: false }}, yaxis: ax('χ²ᵣ', 'log'), shapes: [{{ type: 'line', x0: -0.5, x1: Math.max(objs1.length - 0.5, 0.5), y0: 1, y1: 1, line: {{ color: '#8b9198', dash: 'dash', width: 1 }} }}], legend: COV_LEGEND }}, {{ responsive: true, displayModeBar: false }});
    // Panel 2 — σ ratio vs observed arc, coloured by population.
    const byPop = {{}};
    for (const c of recs) {{ if (radarExcluded.has(c.object)) continue; const rr = c.sigma_fit[0] / c.sigma_ref[0]; const a = arc[c.object]; if (!isFinite(rr) || a == null) continue; const p = popOf[c.object] || '?'; (byPop[p] = byPop[p] || []).push([a, rr, c.object]); }}
    const t2 = [];
    for (const p in byPop) {{ const pts = byPop[p]; t2.push({{ x: pts.map(q => q[0]), y: pts.map(q => q[1]), mode: 'markers', marker: {{ color: popColors[p] || '#888', size: 7 }}, name: p, text: pts.map(q => q[2]), hovertemplate: '%{{text}} (' + p + ')<br>arc %{{x}} d<br>σ ratio %{{y:.2f}}<extra></extra>' }}); }}
    const allArc = recs.map(c => arc[c.object]).filter(a => a != null);
    const amin = allArc.length ? Math.min(...allArc) : 1, amax = allArc.length ? Math.max(...allArc) : 1e5;
    Plotly.react('cov-ratio-panel', t2, {{ ...baseLayout, title: covTitle('2 · σ_emp(a) / σ_JPL(a) vs observed arc'), margin: {{ l: 46, r: 8, t: 28, b: 36 }}, xaxis: ax('observed arc (days)', 'log'), yaxis: ax('σ ratio (a element)', 'log'), shapes: [{{ type: 'line', x0: amin, x1: amax, y0: 1, y1: 1, line: {{ color: '#8b9198', dash: 'dash', width: 1 }} }}], showlegend: false }}, {{ responsive: true, displayModeBar: false }});
    // Panel 3 — σ_eq empirical CDF vs χ(6)/√6 expected.
    const sig = recs.filter(c => !radarExcluded.has(c.object)).map(c => c.sigma_equiv_combined).filter(v => v != null && isFinite(v)).sort((a, b) => a - b);
    const n = sig.length;
    const obsY = sig.map((v, i) => (i + 1) / n);
    const ex = [], ey = [];
    for (let lt = -1.1; lt <= 3.1; lt += 0.05) {{ const t = Math.pow(10, lt); ex.push(t); ey.push(1 - Math.exp(-3 * t * t) * (1 + 3 * t * t + 4.5 * t * t * t * t)); }}
    Plotly.react('cov-cdf-panel', [
        {{ x: ex, y: ey, mode: 'lines', line: {{ color: '#8b9198', width: 2 }}, name: 'expected χ(6)/√6' }},
        {{ x: sig, y: obsY, mode: 'lines', line: {{ color: '#5b9bd5', width: 2 }}, name: `observed (n=${{n}})` }},
    ], {{ ...baseLayout, title: covTitle('3 · σ_eq empirical CDF vs expected'), margin: {{ l: 46, r: 8, t: 28, b: 64 }}, xaxis: ax('σ_eq', 'log'), yaxis: ax('cumulative fraction'), legend: COV_LEGEND }}, {{ responsive: true, displayModeBar: false }});
}}
try {{ buildCovariancePanels(); }} catch (e) {{ console.error('buildCovariancePanels failed', e); }}
// Charts follow whatever pair is selected at boot time, and every later change.
applyToolSelection();
// Any dataset container a builder left untouched has no data for this run — mark it
// so it reads as "no data", never blank and never the pre-load "dataset not loaded".
markEmptyDatasetContainers();
}} // end boot()

// ─────────── dataset fetch + verification ───────────
// The rows are not in this page; they live in a sibling file the browser fetches.
// Nothing renders from unverified bytes: the page checks byte length, an FNV-1a-32
// hash and the row count against the inline descriptor before booting the charts.
// Containers that need the dataset carry a marked "dataset not loaded" state until
// then; the server-rendered heatmaps, OD grid, timing panels and Part 2 do not.
const DATASET_PENDING_IDS = ['growth-chart', 'eph-sep-chart', 'stm-agree-chart',
    'cov-chi2-panel', 'cov-ratio-panel', 'cov-cdf-panel', 'ng-overview',
    'assist-arms-timing', 'assist-arms-pos', 'assist-arms-stm', 'assist-arms-settings'];
const DATASET_PENDING_TBODY_IDS = ['eph-outliers'];
const DATASET_PENDING_STYLE = 'padding:18px;text-align:center;color:#8b9198;font-size:12px;font-style:italic;border:1px dashed #3a4453;border-radius:4px;background:repeating-linear-gradient(45deg,transparent,transparent 6px,rgba(139,145,152,0.06) 6px,rgba(139,145,152,0.06) 12px);';
function markDatasetPending(label) {{
    for (const id of DATASET_PENDING_IDS) {{
        const el = document.getElementById(id);
        if (el) el.innerHTML = `<div class="dataset-pending" style="${{DATASET_PENDING_STYLE}}">${{label}}</div>`;
    }}
    for (const id of DATASET_PENDING_TBODY_IDS) {{
        const el = document.getElementById(id);
        if (el) el.innerHTML = `<tr><td colspan="99" class="dataset-pending" style="text-align:left;color:#8b9198;font-style:italic;padding:12px">${{label}}</td></tr>`;
    }}
}}
function markEmptyDatasetContainers() {{
    const empty = el => el && el.children.length === 0 && !el.textContent.trim();
    for (const id of DATASET_PENDING_IDS) {{
        const el = document.getElementById(id);
        if (empty(el)) el.innerHTML = `<div class="dataset-pending" style="${{DATASET_PENDING_STYLE}}">no data for this run</div>`;
    }}
    for (const id of DATASET_PENDING_TBODY_IDS) {{
        const el = document.getElementById(id);
        if (el && el.children.length === 0) el.innerHTML = `<tr><td colspan="99" class="dataset-pending" style="text-align:left;color:#8b9198;font-style:italic;padding:12px">no data for this run</td></tr>`;
    }}
}}
function fmtDataBytes(n) {{
    if (n >= 1048576) return (n / 1048576).toFixed(1) + ' MB';
    if (n >= 1024) return (n / 1024).toFixed(1) + ' KB';
    return n + ' B';
}}
function datasetBanner(html, kind) {{
    let b = document.getElementById('dataset-banner');
    if (!b) {{
        b = document.createElement('div');
        b.id = 'dataset-banner';
        b.style.cssText = 'position:sticky;top:0;z-index:9999;padding:10px 16px;font-size:13px;font-family:JetBrains Mono,monospace;line-height:1.4;';
        document.body.insertBefore(b, document.body.firstChild);
    }}
    b.style.background = kind === 'error' ? '#4a1418' : '#12303a';
    b.style.color = kind === 'error' ? '#ffb3b3' : '#bfe3ef';
    b.style.borderBottom = kind === 'error' ? '2px solid #e06252' : '2px solid #3a7d95';
    b.innerHTML = html;
    b.hidden = false;
    return b;
}}
// FNV-1a 32-bit over a Uint8Array, 8 lowercase hex digits (Math.imul keeps the
// multiply in 32 bits). Matches the Rust hash printed in the descriptor.
function fnv1a32(bytes) {{
    let h = 0x811c9dc5;
    for (let i = 0; i < bytes.length; i++) {{ h ^= bytes[i]; h = Math.imul(h, 0x01000193); }}
    return (h >>> 0).toString(16).padStart(8, '0');
}}
function datasetFail(msg) {{
    // Persistent, prominent, and never renders charts from unverified data.
    datasetBanner('<b>Dataset not loaded.</b> ' + msg, 'error');
    markDatasetPending('dataset not loaded');
}}
async function loadDataset() {{
    markDatasetPending('dataset not loaded');
    datasetBanner(`Loading dataset <code>${{DATASET.file}}</code> (${{fmtDataBytes(DATASET.bytes)}})…`, 'status');
    let buf;
    try {{
        const resp = await fetch(DATASET.file);
        if (!resp.ok) {{ datasetFail(`HTTP ${{resp.status}} ${{resp.statusText || ''}} fetching <code>${{DATASET.file}}</code>.`); return; }}
        buf = await resp.arrayBuffer();
    }} catch (e) {{
        datasetFail(`This page fetches <code>${{DATASET.file}}</code>; serve the folder over HTTP — a browser will not fetch it from disk (file://). (${{e}})`);
        return;
    }}
    const bytes = new Uint8Array(buf);
    if (bytes.length !== DATASET.bytes) {{
        datasetFail(`Byte-length mismatch: expected ${{DATASET.bytes}}, got ${{bytes.length}}. The dataset beside this page belongs to a different run.`);
        return;
    }}
    const h = fnv1a32(bytes);
    if (h !== DATASET.hash) {{
        datasetFail(`Hash mismatch: expected ${{DATASET.hash}}, got ${{h}}. The dataset beside this page belongs to a different run.`);
        return;
    }}
    let parsed;
    try {{ parsed = JSON.parse(new TextDecoder('utf-8').decode(bytes)); }}
    catch (e) {{ datasetFail(`The dataset is not valid JSON (${{e}}).`); return; }}
    // Belt-and-suspenders: the byte length and hash checked above already pin the
    // file, so a wrong row count reaches here only on a hash collision at the exact
    // byte length. Kept, and kept loud, because a silent wrong-run swap must never
    // render as if the data belonged to this page.
    if (!Array.isArray(parsed) || parsed.length !== DATASET.rows) {{
        const got = Array.isArray(parsed) ? parsed.length : 'a non-array value';
        datasetFail(`Row-count mismatch: expected ${{DATASET.rows}}, got ${{got}}. The dataset beside this page belongs to a different run.`);
        return;
    }}
    results = parsed;
    try {{
        boot();
        const b = document.getElementById('dataset-banner'); if (b) b.hidden = true;
        // Deep-link settle: the browser's native anchor scroll ran before the
        // dataset rendered, and the now-taller heatmaps/charts pushed the target
        // down. Re-run the scroll once, now the content has its final height, so a
        // reload into a Part 1 section lands on its intended top (scroll-margin
        // clears the bar).
        if (location.hash) {{ const t = document.getElementById(location.hash.slice(1)); if (t) t.scrollIntoView({{ block: 'start' }}); }}
    }}
    catch (e) {{ console.error('boot failed', e); datasetFail(`The dataset verified but the charts failed to render (${{e}}).`); }}
}}

// ─────────── tool-pair selector + page switch: wire + initial state ───────────
// Runs first, after every server-rendered panel is in the DOM. Each is isolated so
// a single wiring failure can't leave the report in a broken half-initialized state
// (page nav must come up even if the selector hiccups). These drive the
// server-rendered pair-panels and need no dataset; loadDataset() boots the charts.
try {{ wireToolSelector(); }} catch (e) {{ console.error('wireToolSelector failed', e); }}
try {{ wirePageNav(); }} catch (e) {{ console.error('wirePageNav failed', e); }}
try {{ wireCompareBar(); }} catch (e) {{ console.error('wireCompareBar failed', e); }}
try {{ wireHeatmapCrosshair(); }} catch (e) {{ console.error('wireHeatmapCrosshair failed', e); }}
loadDataset();
</script>
</body>
</html>"##,
        brand_tokens_css = BRAND_TOKENS_CSS,
        ladder_css = ladder_css(),
        tool_tokens_css = tool_color_tokens_css(),
        disc_summary = DISCLOSURE_SUMMARY,
        coeff_defs = NG_COEFF_DEFS,
        verdict_rule = NG_VERDICT_RULE,
        chart_h = FULL_CHART_HEIGHT_PX,
        pair_chip = pair_chip_html(),
        part1_name = PART_NAME_1,
        part2_name = PART_NAME_2,
        part3_name = PART_NAME_3,
        part4_name = PART_NAME_4,
        appendix_name = PART_NAME_APPENDIX,
        dataset_file_name = data_file_name,
        n_prop = n_prop,
        n_eph = n_eph,
        n_od = n_od,
        n_objects = n_objects,
        n_channels = n_channels,
        n_populations = n_populations,
        n_dt = n_dt,
        pop_legend = pop_legend,
        channel_legend = channel_legend,
        h1_prop_grid_html = h1_prop_grid_html,
        h2_eph_grid_html = h2_eph_grid_html,
        od_closeness_grid_html = od_closeness_grid_html,
        h4_fidelity_grid_html = h4_fidelity_grid_html,
        tool_ranking_html = tool_ranking_html,
        covariance_realism_html = covariance_realism_html,
        t1_prop_timing_html = t1_prop_timing_html,
        t2_eph_timing_html = t2_eph_timing_html,
        t3_od_timing_html = t3_od_timing_html,
        part2_timing_html = part2_timing_html,
        provenance_footer_html = provenance_footer_html,
    );

    // Write the dataset (and fail loudly) BEFORE the HTML, and hand the page a
    // descriptor (name, byte length, row count, FNV-1a-32 hash) so it can verify
    // the file it fetched belongs to this render. `data_path` / `data_file_name`
    // were derived above so the masthead link could reference the file name.
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create output directory: {e}"))?;
    }
    std::fs::write(&data_path, results_json.as_bytes())
        .map_err(|e| format!("Failed to write dataset {}: {e}", data_path.display()))?;
    let dataset_descriptor = serde_json::json!({
        "file": data_file_name,
        "bytes": results_json.len(),
        "rows": results.len(),
        "hash": fnv1a32_hex(results_json.as_bytes()),
    });
    let dataset_descriptor_json = serde_json::to_string(&dataset_descriptor).unwrap_or_default();

    let html = html.replace("DATASET_DESCRIPTOR_JSON", &dataset_descriptor_json);
    let html = html.replace("TOOLS_PRESENT_JSON", &tools_present_json);
    let html = html.replace("POP_COLORS_JSON", &pop_colors_json);
    let html = html.replace("CHANNEL_COLORS_JSON", &channel_colors_json);
    let html = html.replace("ORBIT_COMPARISONS_JSON", &orbit_comparisons_json);
    let html = html.replace("RADAR_EXCLUDED_JSON", &radar_excluded_json);

    std::fs::write(output, html).map_err(|e| format!("Failed to write report: {e}"))
}

/// The dataset path for a report output: the output with its extension replaced
/// by `.data.json` (`validation_report.html` -> `validation_report.data.json`).
fn dataset_path_for(output: &Path) -> std::path::PathBuf {
    let mut name = output
        .file_stem()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    name.push(".data.json");
    output.with_file_name(name)
}

/// FNV-1a 32-bit hash of `bytes` as 8 lowercase hex digits. Matches the published
/// reference vectors ("" -> 811c9dc5, "a" -> e40c292c, "foobar" -> bf9cf968) and
/// the `Math.imul` loop the page runs to verify the fetched dataset byte-for-byte.
fn fnv1a32_hex(bytes: &[u8]) -> String {
    let mut h: u32 = 0x811c_9dc5;
    for &b in bytes {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    format!("{h:08x}")
}

/// Convert (positive or negative) days-since-Unix-epoch to (Y, M, D)
/// using the standard proleptic Gregorian calendar. Avoids pulling in
/// chrono for one date.
fn ymd_from_unix_days(unix_days: i64) -> (i32, u32, u32) {
    // Algorithm: Howard Hinnant, "civil_from_days".
    // Source: https://howardhinnant.github.io/date_algorithms.html
    let z = unix_days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::ValidationResult;

    fn synthetic_rust_prop_row(object: &str, dt_days: f64) -> ValidationResult {
        let mut r = ValidationResult::empty();
        r.object = object.to_string();
        r.population = "NEO".to_string();
        r.epoch_mjd_tdb = 61000.0;
        r.dt_days = dt_days;
        r.t_mjd_tdb = 61000.0 + dt_days;
        r.force_model = "standard".to_string();
        r.test_type = "propagation".to_string();
        r.channel = "rust".to_string();
        r.emp_pos_au = Some([1.0, 0.0, 0.0]);
        r.emp_time_ms = Some(2.0);
        r.emp_vs_horizons_km = Some(0.001);
        r.ic_pos_au = Some([1.0, 0.0, 0.0]);
        r.ic_vel_au_d = Some([0.0, 0.017, 0.0]);
        r.ref_pos_au = Some([1.0, 0.0, 0.0]);
        r.timestamp = "2026-04-29T00:00:00Z".to_string();
        r
    }

    /// WCAG relative-luminance contrast ratio between two `#rrggbb` colours.
    fn wcag_contrast(a: &str, b: &str) -> f64 {
        fn lin(c: f64) -> f64 {
            let c = c / 255.0;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }
        fn lum(hex: &str) -> f64 {
            let h = hex.trim_start_matches('#');
            let ch = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap() as f64;
            0.2126 * lin(ch(0)) + 0.7152 * lin(ch(2)) + 0.0722 * lin(ch(4))
        }
        let (la, lb) = (lum(a), lum(b));
        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    // Every ladder bin's numeral must clear AA (4.5:1) against its cell in BOTH
    // themes, read from the one LADDER source so the palette cannot regress.
    #[test]
    fn ladder_ink_clears_aa_in_both_themes() {
        // The check must be able to fail: the pre-fix light bin 4 (light ink on
        // #82b0d3) was 2.08:1, and the fn must still flag it.
        assert!(
            wcag_contrast("#82b0d3", "#eef4f9") < 4.5,
            "contrast fn must flag the old light bin-4 failure"
        );
        for (theme, ladder) in [("dark", &LADDER_DARK), ("light", &LADDER_LIGHT)] {
            for (i, &(bg, ink)) in ladder.iter().enumerate() {
                let c = wcag_contrast(bg, ink);
                assert!(
                    c >= 4.5,
                    "{theme} bin {} ({bg} / {ink}) contrast {c:.2} < 4.5",
                    i + 1
                );
            }
        }
    }

    // The one hatched "unavailable / excluded" state must clear the 3:1
    // graphical-object floor so an excluded cell reads as marked, not blank. The
    // light ground is near white, where the shared α0.5 stripe was only 1.64:1, so
    // light theme overrides --ed-hatch with an opaque, darker stripe. The override
    // is opaque, so its rendered colour is the stripe hex — no compositing needed.
    #[test]
    fn light_theme_hatch_stripe_clears_the_graphical_object_floor() {
        let rows = vec![synthetic_rust_prop_row("Apophis", 0.0)];
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&rows, &[], &out, None).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();

        // The light page ground the finding measured against.
        let light_bg = "#f6f8fb";
        // Prove the guard can fail: the pre-fix light hatch (the α0.5 base stripe
        // composited over #f6f8fb ≈ #c1c5ca) sits below the floor.
        assert!(
            wcag_contrast("#c1c5ca", light_bg) < 3.0,
            "the contrast guard must flag the old 1.64:1 light hatch"
        );

        // Light theme must override --ed-hatch with an opaque hex stripe.
        let light_hatch = html
            .lines()
            .find(|l| l.contains(".theme-light") && l.contains("--ed-hatch"))
            .expect("light theme must override --ed-hatch");
        let hash = light_hatch
            .find('#')
            .expect("light hatch stripe must be an opaque hex");
        let stripe = &light_hatch[hash..hash + 7];
        let c = wcag_contrast(stripe, light_bg);
        assert!(
            c >= 3.0,
            "light hatch stripe {stripe} on {light_bg} is {c:.2} < 3.0"
        );
    }

    /// Strip every no-transform header span — `.u` (the symbol wrap) and the
    /// pre-existing Part 2 `.r1sub` subheader, both `text-transform: none` — then
    /// all remaining tags, returning the header text the uppercasing CSS acts on.
    fn th_text_outside_u(inner: &str) -> String {
        let mut s = inner.to_string();
        for open in ["<span class=\"u\">", "<span class=\"r1sub\">"] {
            while let Some(a) = s.find(open) {
                if let Some(rel) = s[a..].find("</span>") {
                    s.replace_range(a..a + rel + "</span>".len(), "");
                } else {
                    break;
                }
            }
        }
        // Drop remaining tags.
        let mut out = String::new();
        let mut depth = 0;
        for c in s.chars() {
            match c {
                '<' => depth += 1,
                '>' => {
                    if depth > 0 {
                        depth -= 1
                    }
                }
                _ if depth == 0 => out.push(c),
                _ => {}
            }
        }
        out
    }

    /// A bare mathematical symbol or unit token that the uppercasing CSS would
    /// corrupt if it sat in a `<th>` outside the no-transform span. Twin of the
    /// audit's H7 token set (kept in sync by hand, like `uhdr`).
    fn header_forbidden_token(text: &str) -> Option<String> {
        for sym in ['σ', 'χ', 'δ', 'Δ', 'µ', 'μ', '‖', '√'] {
            if text.contains(sym) {
                return Some(sym.to_string());
            }
        }
        let is_alpha = |c: char| c.is_ascii_alphabetic();
        for unit in ["km", "mas", "arcsec", "AU", "ms"] {
            let mut from = 0;
            while let Some(rel) = text[from..].find(unit) {
                let start = from + rel;
                let end = start + unit.len();
                let before_ok = start == 0 || !is_alpha(text[..start].chars().next_back().unwrap());
                let after_ok = end == text.len() || !is_alpha(text[end..].chars().next().unwrap());
                if before_ok && after_ok {
                    return Some(unit.to_string());
                }
                from = start + 1;
            }
        }
        if text.contains("(d)") {
            return Some("(d)".to_string());
        }
        None
    }

    // No <th> in a synthetic render may carry a symbol or unit token outside the
    // no-transform span; a bare one would be uppercase-corrupted (σ_eq→Σ_EQ).
    #[test]
    fn no_table_header_leaks_a_bare_symbol_or_unit() {
        let (rows, cmps) = radar_scenario();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&rows, &cmps, &out, None).unwrap();
        let raw = std::fs::read_to_string(&out).unwrap();
        // Excise <script>/<style> so `<th>` literals in the client JS source
        // (headers the browser builds at runtime) are not scanned as markup.
        fn excise(src: &str, tag: &str) -> String {
            let (open, close) = (format!("<{tag}"), format!("</{tag}>"));
            let mut out = String::new();
            let mut cur = src;
            while let Some(a) = cur.find(&open) {
                out.push_str(&cur[..a]);
                match cur[a..].find(&close) {
                    Some(rel) => cur = &cur[a + rel + close.len()..],
                    None => {
                        cur = "";
                        break;
                    }
                }
            }
            out.push_str(cur);
            out
        }
        let html = excise(&excise(&raw, "script"), "style");
        // Prove the scanner can fail: an unwrapped σ_eq header is caught.
        assert_eq!(
            header_forbidden_token(&th_text_outside_u("σ_eq")),
            Some("σ".to_string()),
            "the header scanner must catch a bare symbol"
        );
        let mut from = 0;
        let mut checked = 0;
        while let Some(rel) = html[from..].find("<th") {
            let open = from + rel;
            let gt = match html[open..].find('>') {
                Some(g) => open + g + 1,
                None => break,
            };
            let close = match html[gt..].find("</th>") {
                Some(c) => gt + c,
                None => break,
            };
            let text = th_text_outside_u(&html[gt..close]);
            if let Some(tok) = header_forbidden_token(&text) {
                panic!("<th> leaks bare token \"{tok}\" outside the .u span: \"{text}\"");
            }
            checked += 1;
            from = close + "</th>".len();
        }
        assert!(checked > 10, "expected to scan many headers, saw {checked}");
    }

    #[test]
    fn generate_report_produces_valid_html() {
        let rows = vec![
            synthetic_rust_prop_row("Apophis", 0.0),
            synthetic_rust_prop_row("Apophis", 30.0),
            synthetic_rust_prop_row("Bennu", 0.0),
        ];
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        let result = generate_report(&rows, &[], &out, None);
        assert!(result.is_ok(), "generate_report failed: {result:?}");
        let html = std::fs::read_to_string(&out).unwrap();
        assert!(html.contains("EMPYREAN"), "missing brand title");
        assert!(html.contains("DYNAMICS"), "missing brand title");
        assert!(html.contains("Apophis"), "missing per-object data");
        assert!(html.contains("Bennu"), "missing per-object data");
        assert!(html.contains("Plotly"), "missing chart library");
    }

    #[test]
    fn generate_report_writes_summary_when_path_given() {
        let rows = vec![synthetic_rust_prop_row("Apophis", 0.0)];
        let dir = tempfile::tempdir().unwrap();
        let html = dir.path().join("report.html");
        let summary = dir.path().join("summary.json");
        let result = generate_report(&rows, &[], &html, Some(&summary));
        assert!(result.is_ok());
        let summary_json = std::fs::read_to_string(&summary).unwrap();
        let v: serde_json::Value = serde_json::from_str(&summary_json).unwrap();
        assert_eq!(
            v["fidelity_threshold"], FIDELITY_THRESHOLD,
            "summary should record the threshold the run was gated against",
        );
    }

    #[test]
    fn generate_report_renders_orbit_compare_section_when_present() {
        use crate::schema::OrbitComparison;
        let rows = vec![synthetic_rust_prop_row("Apophis", 0.0)];
        let comps = vec![OrbitComparison {
            object: "Apophis".to_string(),
            reference: "sbdb".to_string(),
            common_epoch_mjd_tdb: 53371.835,
            common_epoch_source: "fit".to_string(),
            repr: "keplerian".to_string(),
            state_fit: [0.92, 0.19, 3.33, 204.5, 126.3, 105.9],
            state_ref: [0.92, 0.19, 3.34, 203.9, 126.7, 312.8],
            delta: [0.0, -2e-8, -0.01, 0.6, -0.35, -153.0],
            sigma_fit: [4.4e-10, 3.5e-8, 1.3e-6, 1.9e-5, 1.8e-5, 8.2e-6],
            sigma_ref: [1.7e-9, 1.6e-9, 9.9e-8, 3.1e-6, 3.3e-6, 7.8e-7],
            mahalanobis_d2_fit_metric: 7807.0,
            mahalanobis_d2_ref_metric: 574700.0,
            mahalanobis_d2_combined_metric: 1186.0,
            mahalanobis_d2_marginal: 0.01,
            sigma_equiv_combined: 14.06,
            eigenvalues_fit: [1.5e-9, 6.7e-10, 3.4e-10, 9.0e-12, 3.3e-13, 1.9e-19],
            eigenvalues_ref: [1.3e-11, 1.1e-11, 2.3e-12, 4.2e-15, 1.2e-17, 2.5e-19],
            principal_axis_rotation_deg: 64.3,
            cov_volume_ratio: 1.2e11,
            notes: vec!["reference (SBDB) propagated to fit epoch via STM".to_string()],
        }];
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&rows, &comps, &out, None).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        assert!(
            html.contains("Covariance realism"),
            "missing Part 4 section title"
        );
        assert!(
            html.contains(r#""sigma_equiv_combined":14.06"#),
            "comparison data not embedded"
        );
        assert!(
            html.contains(r#""mahalanobis_d2_marginal":0.01"#),
            "marginal d² not embedded"
        );
        assert!(
            html.contains(r#""principal_axis_rotation_deg":64.3"#),
            "principal-axis rotation angle not embedded"
        );
    }

    // ---- Text-diet (H5) invariants ----------------------------------------

    /// Visible word count of an HTML fragment: strip tags, drop &nbsp;, and count
    /// whitespace-separated runs — the same token rule the browser measure and the
    /// audit's H5 use (every &mdash; / &middot; is one non-whitespace token).
    fn visible_word_count(fragment: &str) -> usize {
        let mut text = String::new();
        let mut in_tag = false;
        for c in fragment.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => text.push(c),
                _ => {}
            }
        }
        text.replace("&nbsp;", " ").split_whitespace().count()
    }

    /// A section title with its trailing Part 1 pair chip removed. The chip's
    /// words are a chip, not title words (the audit classes `.pair-chip` the same
    /// way), so the 6-word title budget is measured on the title text alone. The
    /// chip is always the last child of the title, so truncating at its start is
    /// exact.
    fn strip_pair_chip(inner: &str) -> &str {
        match inner.find("<span class=\"pair-chip\"") {
            Some(s) => &inner[..s],
            None => inner,
        }
    }

    /// Every `<div class="{class_attr}" ...>inner</div>` in `html`, as
    /// (opening-tag attributes, inner html). The diet elements carry no nested
    /// `<div>`, so scanning to the next `</div>` is exact.
    fn elements_by_class<'a>(html: &'a str, class_attr: &str) -> Vec<(&'a str, &'a str)> {
        let marker = format!("<div class=\"{class_attr}\"");
        let mut out = Vec::new();
        let mut i = 0;
        while let Some(p) = html[i..].find(&marker) {
            let attr_start = i + p + marker.len();
            let gt = html[attr_start..].find('>').expect("unterminated tag") + attr_start;
            let inner_start = gt + 1;
            let end = html[inner_start..].find("</div>").expect("no </div>") + inner_start;
            out.push((&html[attr_start..gt], &html[inner_start..end]));
            i = end + "</div>".len();
        }
        out
    }

    /// The value of a `title="..."` attribute in an opening-tag attribute string.
    fn title_attr(attrs: &str) -> Option<&str> {
        let start = attrs.find("title=\"")? + "title=\"".len();
        let end = attrs[start..].find('"')? + start;
        Some(&attrs[start..end])
    }

    /// Count the decade swatches of a ladder key whose trailing label is visible.
    /// Each swatch is `<span class="gk"><span class="gk-sw gbN" ...></span>LABEL</span>`.
    fn count_visible_swatch_labels(key: &str) -> usize {
        let mut n = 0;
        let mut i = 0;
        while let Some(p) = key[i..].find("class=\"gk-sw gb") {
            let sw = i + p;
            let inner_gt = key[sw..].find('>').unwrap() + sw;
            let inner_close = key[inner_gt..].find("</span>").unwrap() + inner_gt;
            let after_inner = inner_close + "</span>".len();
            let outer_close = key[after_inner..].find("</span>").unwrap() + after_inner;
            if !key[after_inner..outer_close].trim().is_empty() {
                n += 1;
            }
            i = outer_close;
        }
        n
    }

    fn render_synthetic_html() -> String {
        let rows = vec![
            synthetic_rust_prop_row("Apophis", 0.0),
            synthetic_rust_prop_row("Apophis", 30.0),
            synthetic_rust_prop_row("Bennu", 0.0),
            synthetic_rust_prop_row("Bennu", 365.0),
        ];
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&rows, &[], &out, None).unwrap();
        std::fs::read_to_string(&out).unwrap()
    }

    #[test]
    fn text_diet_titles_and_reading_lines_within_budget() {
        // How we know a regression would be caught: the counter flags the pre-diet
        // strings, which exceed the budgets (8+ and 12+ visible tokens).
        assert!(
            visible_word_count(
                "All tools, side by side &mdash; error growth, orbit determination, speed"
            ) > 6,
            "counter must flag a pre-diet section title"
        );
        assert!(
            visible_word_count(
                "Each cell is that tool's own number on the axis in the column head: \
                 propagation and ephemeris are the median and p90 offset from JPL Horizons"
            ) > 12,
            "counter must flag a pre-diet reading line"
        );

        let html = render_synthetic_html();
        for (attrs, inner) in elements_by_class(&html, "section-title") {
            // A Part 1 title's pair chip is a chip, not title words (mirrors the
            // audit's H5 classification); measure the 6-word cap on the title text.
            let w = visible_word_count(strip_pair_chip(inner));
            assert!(
                w <= 6,
                "section-title over 6 words ({w}): {inner:?} [{attrs}]"
            );
        }
        for (attrs, inner) in elements_by_class(&html, "panel-title") {
            let w = visible_word_count(inner);
            assert!(
                w <= 6,
                "panel-title over 6 words ({w}): {inner:?} [{attrs}]"
            );
        }
        // Pass-20 finding: a `grid-summary reading` div with empty content
        // renders at 0 height, so its title hover is unreachable. The per-matrix
        // reading guidance now rides the table header-row hover instead. Any
        // surviving reading div must carry visible content (be hoverable), never
        // hide its guidance in a title on a 0-height element.
        for (attrs, inner) in elements_by_class(&html, "grid-summary reading") {
            assert!(
                visible_word_count(inner) > 0,
                "empty grid-summary reading div (0-height, unreachable hover); \
                 move its guidance to the table header-row title: [{attrs}]"
            );
        }
    }

    #[test]
    fn text_diet_shortened_elements_carry_title() {
        let html = render_synthetic_html();
        // Pass-20 diet: each matrix's reading guidance now rides a hoverable
        // table header-row title (<thead><tr title="...">) instead of a
        // 0-height grid-summary div, so the guidance stays reachable.
        let header_hovers = html.matches("<thead><tr title=\"").count();
        assert!(
            header_hovers >= 3,
            "expected table header-row hovers carrying the moved reading \
             guidance, got {header_hovers}"
        );
        // The shortened headings carry their full form in a non-empty title.
        let mut titled = 0;
        for (attrs, _) in elements_by_class(&html, "section-title")
            .into_iter()
            .chain(elements_by_class(&html, "panel-title"))
        {
            if let Some(t) = title_attr(attrs) {
                assert!(
                    !t.trim().is_empty(),
                    "heading with an empty title attribute"
                );
                titled += 1;
            }
        }
        assert!(
            titled >= 3,
            "expected several headings to carry a hover title, got {titled}"
        );
    }

    #[test]
    fn text_diet_references_and_dt_caveat_collapsed() {
        let html = render_synthetic_html();
        // The provenance reference list moved into a collapsed <details> so its
        // seven citations leave the default view while staying cited.
        let sum = "<details><summary class=\"ref-head\"><b>References</b></summary>";
        let s = html
            .find(sum)
            .expect("references must be wrapped in a collapsed <details> summary");
        let rest = &html[s + sum.len()..];
        let end = rest
            .find("</details>")
            .expect("references <details> must close");
        assert!(
            rest[..end].contains("class=\"ref\""),
            "the citation list must live inside the references disclosure body"
        );
        // Regression guard: no citation may sit before the disclosure (i.e. left
        // visible in the default state).
        assert!(
            !html[..s].contains("class=\"ref\""),
            "no citation may sit outside the collapsed references disclosure"
        );

        // The ASSIST ΔT-exclusion caveat is injected by script; it must be built
        // inside a collapsed disclosure body, never as a free-standing paragraph.
        assert!(
            html.contains("<summary>ΔT exclusion</summary>"),
            "the ΔT-exclusion caveat must sit under a collapsed disclosure"
        );
        let caveat = "ASSIST does not model the non-gravitational delay";
        let c = html.find(caveat).expect("ΔT caveat text must be present");
        assert!(
            html[..c].rfind("disc-body").is_some_and(|d| c - d < 140),
            "the ΔT caveat text must be inside a disclosure body"
        );
    }

    #[test]
    fn ladder_key_shows_two_end_labels_and_titled_swatches() {
        for axis in [GridAxis::PropKm, GridAxis::EphMas, GridAxis::TimingMs] {
            let key = grid_key_html(axis);
            let swatches = key.matches("class=\"gk-sw gb").count();
            assert_eq!(swatches, 7, "ladder key must render seven decade swatches");
            // Every swatch (decade band and state mark) carries a hover title.
            for piece in key.split("class=\"gk-sw").skip(1) {
                let attrs = &piece[..piece.find('>').unwrap()];
                assert!(
                    attrs.contains("title=\""),
                    "swatch without a title in {axis:?}"
                );
            }
            assert_eq!(
                count_visible_swatch_labels(&key),
                2,
                "exactly two visible bin labels expected in {axis:?}"
            );
        }
    }

    fn synthetic_core_od_row(object: &str) -> ValidationResult {
        let mut r = ValidationResult::empty();
        r.object = object.to_string();
        r.population = "NEO".to_string();
        r.epoch_mjd_tdb = 61000.0;
        r.t_mjd_tdb = 61000.0;
        r.force_model = "standard".to_string();
        r.test_type = "orbit_determination".to_string();
        r.channel = "core".to_string();
        r.emp_pos_au = Some([1.0, 0.0, 0.0]);
        r.od_reduced_chi2 = Some(0.5);
        r.od_chi2 = Some(50.0);
        r.n_obs_used = Some(100);
        r.od_converged = Some(true);
        r.od_rms_combined_arcsec = Some(0.3);
        r.findorb_rms_residual = Some(0.35);
        r.layup_reduced_chi2 = Some(0.9);
        r.layup_chi2 = Some(90.0);
        r.layup_n_obs_used = Some(110);
        r.layup_converged = Some(true);
        r.timestamp = "2026-04-29T00:00:00Z".to_string();
        r
    }

    #[test]
    fn report_ships_dataset_as_sibling_file_and_renders_all_sections() {
        // Contract tripwire: the rows no longer ride inside the page. They are
        // written to "<stem>.data.json" byte-for-byte as `serde_json::to_string`
        // (the same bytes the inline embed used to carry), the page holds no
        // inline dataset, and the inline descriptor's byte length, row count and
        // FNV-1a-32 hash match the file. Also asserts every section anchor
        // renders, so a refactor cannot drop a panel unnoticed.
        let rows = vec![
            synthetic_rust_prop_row("Apophis", 0.0),
            synthetic_core_od_row("Apophis"),
        ];
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&rows, &[], &out, None).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();

        // (1) the sibling dataset holds the verbatim serialization.
        let expected = serde_json::to_string(&rows).unwrap();
        let data_path = dir.path().join("report.data.json");
        let data_bytes = std::fs::read(&data_path).unwrap();
        assert_eq!(
            data_bytes,
            expected.as_bytes(),
            "the .data.json bytes must equal serde_json::to_string(&rows)",
        );
        // layup data specifically reaches the file (the fields must survive).
        assert!(
            expected.contains("layup_reduced_chi2"),
            "layup fields absent from the dataset",
        );

        // (2) the page carries no inline dataset, and no distinctive row value.
        assert!(
            !html.contains(&expected),
            "the row array must not be embedded in the page",
        );
        assert!(
            !html.contains(r#""od_reduced_chi2":0.5"#),
            "a distinctive row value leaked into the page",
        );
        assert!(
            !html.contains("const results = ["),
            "the inline dataset array must be gone",
        );

        // (3) the descriptor matches values recomputed from the file.
        let descriptor = serde_json::to_string(&serde_json::json!({
            "file": "report.data.json",
            "bytes": data_bytes.len(),
            "rows": rows.len(),
            "hash": fnv1a32_hex(&data_bytes),
        }))
        .unwrap();
        assert!(
            html.contains(&descriptor),
            "the inline descriptor must match the dataset file's bytes, rows and hash",
        );

        // (4) every kept section anchor is present. The cut sections (s04
        // Tool-vs-Truth, s08 dRA/dDec growth, s09 legacy OD diagnostics, s11
        // uncertainty-cost duplicate) were deleted outright in the cut-list
        // pass, so they are no longer expected.
        for anchor in [
            "id=\"s01\"",
            "id=\"s01b\"",
            "id=\"s02\"",
            "id=\"s03\"",
            "id=\"s05\"",
            "id=\"s06\"",
            "id=\"s06b\"",
            "id=\"s07\"",
            "id=\"s08b\"",
            "id=\"s09b\"",
            "id=\"s10\"",
            "id=\"s12\"",
            "id=\"s13\"",
        ] {
            assert!(html.contains(anchor), "missing section anchor: {anchor}");
        }
        // The cut sections must be gone.
        for gone in ["id=\"s04\"", "id=\"s08\"", "id=\"s09\"", "id=\"s11\""] {
            assert!(!html.contains(gone), "cut section still present: {gone}");
        }

        // (5) the client verifies the fetched dataset with a complete, loud ladder
        // before it renders a single chart: byte length, then FNV-1a-32 hash, then
        // JSON parse, then row count. Each failure surfaces its own visible banner
        // (no silent fallback), so none of these guards may be dropped as
        // "unreachable" — the row-count guard still catches a wrong run that lands
        // on a hash collision at an identical byte length.
        for guard in [
            "Byte-length mismatch: expected",
            "Hash mismatch: expected",
            "Row-count mismatch: expected",
        ] {
            assert!(
                html.contains(guard),
                "dataset-verification guard missing from the client script: {guard}",
            );
        }
    }

    #[test]
    fn fnv1a32_matches_reference_vectors() {
        // The page runs the identical hash (Math.imul loop) to verify the fetched
        // dataset; a mismatch here means every published page would reject its data.
        assert_eq!(fnv1a32_hex(b""), "811c9dc5");
        assert_eq!(fnv1a32_hex(b"a"), "e40c292c");
        assert_eq!(fnv1a32_hex(b"foobar"), "bf9cf968");
    }

    #[test]
    fn dataset_name_derives_from_output_stem() {
        // Two reports in one directory must not collide on their dataset file.
        assert_eq!(
            dataset_path_for(std::path::Path::new("/x/validation_report.html")),
            std::path::PathBuf::from("/x/validation_report.data.json"),
        );
        let rows = vec![synthetic_rust_prop_row("Apophis", 0.0)];
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("report_a.html");
        let b = dir.path().join("report_b.html");
        generate_report(&rows, &[], &a, None).unwrap();
        generate_report(&rows, &[], &b, None).unwrap();
        assert!(dir.path().join("report_a.data.json").exists());
        assert!(dir.path().join("report_b.data.json").exists());
        assert!(
            std::fs::read_to_string(&a)
                .unwrap()
                .contains(r#""file":"report_a.data.json""#)
        );
        assert!(
            std::fs::read_to_string(&b)
                .unwrap()
                .contains(r#""file":"report_b.data.json""#)
        );
    }

    #[test]
    fn generate_report_is_byte_reproducible() {
        // Several populations and channels so the emitted popColors / channelColors
        // literals would expose any HashMap iteration order in the output. This
        // failed before the BTreeMap fix; it must be byte-identical now.
        let mut r1 = synthetic_rust_prop_row("Apophis", 0.0);
        r1.population = "NEO".to_string();
        let mut r2 = synthetic_rust_prop_row("Bennu", 30.0);
        r2.population = "MBA".to_string();
        r2.channel = "core".to_string();
        let mut r3 = synthetic_rust_prop_row("Ceres", 0.0);
        r3.population = "Comet".to_string();
        r3.channel = "python".to_string();
        let rows = vec![r1, r2, r3, synthetic_core_od_row("Apophis")];
        let dir = tempfile::tempdir().unwrap();
        let out1 = dir.path().join("r1.html");
        let out2 = dir.path().join("r2.html");
        generate_report(&rows, &[], &out1, None).unwrap();
        generate_report(&rows, &[], &out2, None).unwrap();
        // The ONLY intended difference is the dataset file name each page cites.
        let s1 = std::fs::read_to_string(&out1)
            .unwrap()
            .replace("r1.data.json", "DATA");
        let s2 = std::fs::read_to_string(&out2)
            .unwrap()
            .replace("r2.data.json", "DATA");
        assert_eq!(s1, s2, "HTML must be byte-reproducible across renders");
        let d1 = std::fs::read(dir.path().join("r1.data.json")).unwrap();
        let d2 = std::fs::read(dir.path().join("r2.data.json")).unwrap();
        assert_eq!(d1, d2, "dataset must be byte-reproducible across renders");
    }

    // ── Redesign (empyrean-k2rkx): Part-1 agreement grid invariants ──

    /// A core propagation row carrying the folded ASSIST / find_orb offsets
    /// (the H1 grid reads its non-empyrean sub-rows from these).
    fn synthetic_core_prop_row(object: &str, pop: &str, dt: f64) -> ValidationResult {
        let mut r = synthetic_rust_prop_row(object, dt);
        r.channel = "core".to_string();
        r.population = pop.to_string();
        r.assist_vs_horizons_km = Some(0.01);
        r.findorb_vs_horizons_km = Some(0.1);
        // A non-JPL-referenced legal pair (empyrean vs ASSIST), so the test can
        // assert an arbitrary legal pair renders its own panel.
        r.emp_vs_assist_km = Some(0.02);
        r
    }

    #[test]
    fn part_one_section_shows_one_heatmap_for_the_selected_pair() {
        // The owner's directive, made a testable invariant of the emitted HTML:
        // each part-one agreement section shows exactly ONE heatmap table — the
        // selected (Tool, Reference) pair — not one table per tool stacked. The
        // default pair (empyrean vs JPL) is the visible panel; every other tool
        // is a hidden panel the tool-selector reveals; an illegal or empty pair
        // falls through to a single hatched "not compared" line. Each visible
        // table carries every object once (one row per object, no tool sub-rows)
        // over every horizon, with a per-population median header row; and the
        // whole page defines the 45° hatch exactly once.
        let objs = [("Apophis", "NEO"), ("Bennu", "NEO"), ("67P", "Comet")];
        let dts = [-30.0_f64, 0.0, 30.0, 365.0];
        let mut rows = Vec::new();
        for (o, p) in objs {
            for dt in dts {
                let mut r = synthetic_rust_prop_row(o, dt);
                r.population = p.to_string();
                r.propagation_uncertainty = Some("first_order_detection_on".to_string());
                rows.push(r);
                rows.push(synthetic_core_prop_row(o, p, dt));
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&rows, &[], &out, None).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();

        // Exactly one hatch definition, everywhere on the page.
        let hatch_defs = html.matches(".hatch {").count();
        assert_eq!(
            hatch_defs, 1,
            "expected exactly one .hatch CSS definition, found {hatch_defs}"
        );

        // Scope to the propagation section (s02 → s03).
        let s02 = html.find("id=\"s02\"").expect("propagation section");
        let s03 = html[s02..]
            .find("id=\"s03\"")
            .map(|e| s02 + e)
            .unwrap_or(html.len());
        let sec = &html[s02..s03];

        // The default pair (empyrean vs JPL) is the single visible panel; the
        // other legal pairs are hidden panels the selector reveals, including a
        // non-JPL-referenced pair (empyrean vs ASSIST) computed from the stored
        // difference — the old pair machinery's behaviour. The generic fallback
        // is hidden until an illegal or empty pair is chosen.
        assert!(
            sec.contains("data-tool=\"empyrean\" data-ref=\"jpl\" data-default=\"1\">"),
            "the default empyrean-vs-JPL panel must be present and visible"
        );
        assert!(
            sec.contains("data-tool=\"assist\" data-ref=\"jpl\" hidden>"),
            "the ASSIST-vs-JPL panel must be present but hidden"
        );
        assert!(
            sec.contains("data-tool=\"findorb\" data-ref=\"jpl\" hidden>"),
            "the find_orb-vs-JPL panel must be present but hidden"
        );
        assert!(
            sec.contains("data-tool=\"empyrean\" data-ref=\"assist\" hidden>"),
            "a non-JPL legal pair (empyrean vs ASSIST) must render its own hidden panel"
        );
        // A legal but non-reconstructable pair (ASSIST vs find_orb: both carry
        // positions, no shared-reference difference stored) renders its own
        // hidden hatched panel, so every legal pair is a panel of its own.
        assert!(
            sec.contains("data-tool=\"assist\" data-ref=\"findorb\" hidden>"),
            "the ASSIST-vs-find_orb legal pair must render its own hidden hatched panel"
        );
        assert!(
            sec.contains("pair-notcompared generic\" hidden>"),
            "the generic not-compared fallback line must be present and hidden"
        );

        // Exactly one panel is visible (the default): every other panel open
        // tag carries the `hidden` attribute.
        let visible_panels = sec
            .split("<div class=\"pair-panel\"")
            .skip(1)
            .filter(|chunk| {
                let tag = &chunk[..chunk.find('>').unwrap_or(chunk.len())];
                !tag.contains(" hidden")
            })
            .count();
        assert_eq!(
            visible_panels, 1,
            "exactly one heatmap panel must be visible per section; found {visible_panels}"
        );

        // The visible empyrean table carries every object exactly once (one row
        // per object), every horizon column, a per-population median header row,
        // and no tool sub-rows.
        let start = sec.find("id=\"h1-prop\"").expect("H1 empyrean grid");
        let end = sec[start..]
            .find("</table>")
            .map(|e| start + e)
            .unwrap_or(sec.len());
        let grid = &sec[start..end];
        let object_rows = grid.matches("class=\"gobjr\"").count();
        assert_eq!(
            object_rows,
            objs.len(),
            "empyrean H1 table must carry all {} objects, one row each; found {object_rows}",
            objs.len()
        );
        let horizon_cols = grid.matches("<th class=\"gdt").count();
        assert_eq!(
            horizon_cols,
            dts.len(),
            "empyrean H1 table must carry every horizon column; found {horizon_cols}"
        );
        assert_eq!(
            grid.matches("class=\"gsub\"").count(),
            0,
            "the tripled tool sub-row layout must be gone from H1"
        );
        assert!(
            grid.contains("class=\"gpophdr\""),
            "the per-population median header row must be present"
        );
        // And the redesigned Part-1 heading is in place (shared part-name table).
        assert!(html.contains(PART_NAME_1), "Part 1 head missing");
    }

    // ── OD closeness grid (H3): three-state discipline, live ─────────
    //
    // The convergence discipline the old convergence matrix carried now lives
    // on the H3 grid itself (shape = state, fill = closeness): converged, did
    // not converge, and not-attempted each render distinctly; an unprovable
    // blank resolves to "not attempted", never to a failure; and a tool is
    // never charged with a failure on data it was never handed.

    /// A bare OD row on one channel, identity fields only. Each test opts
    /// into exactly the fields whose reading it pins.
    fn conv_od_row(object: &str, channel: &str, test_type: &str) -> ValidationResult {
        let mut r = ValidationResult::empty();
        r.object = object.to_string();
        r.population = "NEO".to_string();
        r.epoch_mjd_tdb = 61000.0;
        r.t_mjd_tdb = 61000.0;
        r.force_model = "standard".to_string();
        r.test_type = test_type.to_string();
        r.channel = channel.to_string();
        r.timestamp = "2026-08-04T00:00:00Z".to_string();
        r
    }

    /// An sbdb-direction comparison record with a given (finite or NaN) sigma.
    fn sbdb_cmp(object: &str, sigma: f64) -> crate::schema::OrbitComparison {
        crate::schema::OrbitComparison {
            object: object.to_string(),
            reference: "sbdb".to_string(),
            common_epoch_mjd_tdb: 61000.0,
            common_epoch_source: "sbdb".to_string(),
            repr: "keplerian".to_string(),
            state_fit: [0.0; 6],
            state_ref: [0.0; 6],
            delta: [0.0; 6],
            sigma_fit: [f64::NAN; 6],
            sigma_ref: [f64::NAN; 6],
            mahalanobis_d2_fit_metric: f64::NAN,
            mahalanobis_d2_ref_metric: f64::NAN,
            mahalanobis_d2_combined_metric: f64::NAN,
            mahalanobis_d2_marginal: f64::NAN,
            sigma_equiv_combined: sigma,
            eigenvalues_fit: [f64::NAN; 6],
            eigenvalues_ref: [f64::NAN; 6],
            principal_axis_rotation_deg: f64::NAN,
            cov_volume_ratio: f64::NAN,
            notes: vec![],
        }
    }

    /// The `<tr>` of one arc row for an object, as raw HTML. `kind` is
    /// "optical" or "+ radar". Splitting on the object keeps the assertion
    /// from matching another object's cells.
    fn od_arc_row(html: &str, object: &str, kind: &str) -> String {
        let obj_at = html
            .find(&format!(">{object}"))
            .unwrap_or_else(|| panic!("no grid row for {object:?}"));
        let rest = &html[obj_at..];
        let cell = format!("class=\"gtool\">{kind}</td>");
        let c_at = rest
            .find(&cell)
            .unwrap_or_else(|| panic!("{object:?} has no {kind:?} arc row"));
        let r2 = &rest[c_at..];
        let end = r2.find("</tr>").expect("unterminated arc row");
        r2[..end].to_string()
    }

    #[test]
    fn od_grid_renders_converged_failed_and_not_attempted_distinctly() {
        // Apophis converged with a finite closeness → a filled sigma cell;
        // Bennu did not converge → the ✕ fail cell, and is named in the
        // failures list. The converged-vs-failed split is the panel's point.
        let mut apophis = conv_od_row("Apophis", "rust", "orbit_determination");
        apophis.od_converged = Some(true);
        let mut bennu = conv_od_row("Bennu", "rust", "orbit_determination");
        bennu.od_converged = Some(false);
        bennu.notes = "determine FAIL: differential correction diverged".to_string();

        let html = build_od_closeness_grid(&[apophis, bennu], &[sbdb_cmp("Apophis", 1.5)]);

        let ok = od_arc_row(&html, "Apophis", "optical");
        assert!(ok.contains("oc conv"), "converged cell missing state: {ok}");
        assert!(
            ok.contains("gb3"),
            "σ_eq 1.5 must fill the 1–3 bin (D3): {ok}"
        );

        let fail = od_arc_row(&html, "Bennu", "optical");
        assert!(
            fail.contains("oc fail"),
            "non-converged cell missing state: {fail}"
        );
        assert!(
            fail.contains("✕"),
            "non-converged cell missing ✕ glyph: {fail}"
        );
        assert!(
            html.contains("Did not converge") && html.contains("Bennu"),
            "the failure must be named in the failures list: {html}"
        );
    }

    #[test]
    fn od_grid_never_charges_a_tool_with_an_unprovable_failure() {
        // find_orb ran (it fit Apophis) but folded nothing onto Bennu. The
        // merge does not carry find_orb's failure marker, so "no solution" and
        // "never asked" are the same all-None row — it must read as not
        // attempted, never as a failure.
        let mut apophis = conv_od_row("Apophis", "rust", "orbit_determination");
        apophis.od_converged = Some(true);
        let mut apophis_core = conv_od_row("Apophis", "core", "orbit_determination");
        apophis_core.findorb_rms_residual = Some(0.35);
        apophis_core.findorb_n_obs_used = Some(900);
        let mut bennu = conv_od_row("Bennu", "rust", "orbit_determination");
        bennu.od_converged = Some(true);
        let bennu_core = conv_od_row("Bennu", "core", "orbit_determination");

        let html = build_od_closeness_grid(&[apophis, apophis_core, bennu, bennu_core], &[]);

        // find_orb is its own panel now; scope the assertions to it (the
        // empyrean default panel comes first and carries empyrean's own cells).
        let fo_panel = html
            .split("data-tool=\"findorb\"")
            .nth(1)
            .expect("find_orb panel present");
        let apophis_row = od_arc_row(fo_panel, "Apophis", "optical");
        assert!(
            apophis_row.contains("find_orb converged"),
            "find_orb's fit did not read as converged: {apophis_row}"
        );
        let bennu_row = od_arc_row(fo_panel, "Bennu", "optical");
        assert!(
            bennu_row.contains("title=\"not attempted\""),
            "a find_orb-less row must read as not attempted: {bennu_row}"
        );
        assert!(
            !bennu_row.contains("oc fail"),
            "find_orb was charged with a failure the merged row cannot prove: {bennu_row}"
        );
    }

    #[test]
    fn hatch_vocabulary_is_canonical_across_grids() {
        // One understatement term for an unrun OD arc, one phrase for an absent
        // channel. A near-duplicate ("never attempted" / "channel not run in
        // this invocation") in key or cell is a vocabulary regression.
        let mut apophis = conv_od_row("Apophis", "rust", "orbit_determination");
        apophis.od_converged = Some(true);
        let mut apophis_core = conv_od_row("Apophis", "core", "orbit_determination");
        apophis_core.findorb_rms_residual = Some(0.35);
        apophis_core.findorb_n_obs_used = Some(900);
        let mut bennu = conv_od_row("Bennu", "rust", "orbit_determination");
        bennu.od_converged = Some(true);
        let bennu_core = conv_od_row("Bennu", "core", "orbit_determination");
        let od = build_od_closeness_grid(&[apophis, apophis_core, bennu, bennu_core], &[]);
        assert!(
            !od.contains("never attempted"),
            "OD grid must speak one understatement term: {od}"
        );
        assert!(
            od.contains(HATCH_NOT_ATTEMPTED),
            "OD grid lost the canonical understatement term: {od}"
        );

        let channel = build_channel_fidelity_grid(&[]);
        assert!(
            !channel.contains("channel not run in this invocation"),
            "channel grid must name an absent channel one way: {channel}"
        );
        assert!(
            channel.contains(HATCH_CHANNEL_ABSENT),
            "channel grid lost the canonical absence phrase: {channel}"
        );
    }

    #[test]
    fn od_grid_splits_the_radar_arc_onto_its_own_row() {
        // An object fit on both arcs gets an optical row and a radar sub-row;
        // the radar sub-row carries no closeness number yet (empyrean-pfpb2).
        let mut optical = conv_od_row("Apophis", "rust", "orbit_determination");
        optical.od_converged = Some(true);
        let mut radar = conv_od_row("Apophis", "rust", "orbit_determination_radar");
        radar.od_converged = Some(true);

        let html = build_od_closeness_grid(&[optical, radar], &[sbdb_cmp("Apophis", 2.0)]);
        assert!(
            html.contains("odradar"),
            "radar arc did not get its own sub-row: {html}"
        );
        let radar_row = od_arc_row(&html, "Apophis", "+ radar");
        assert!(
            radar_row.contains("oc conv"),
            "radar fit lost its converged state: {radar_row}"
        );
        assert!(
            radar_row.contains("od-d hatch")
                && radar_row.contains("the radar comparison record is not generated yet"),
            "radar sub-row must mark the missing comparison record on hover: {radar_row}"
        );
    }

    /// Four objects for the radar-informed-JPL rule: Apophis (JPL radar + our own
    /// radar arc), Toutatis (JPL radar, no radar arc of ours), Ceres (no radar),
    /// and Wirtanen (Doppler only). The SBDB counts live on the CORE-channel row
    /// so the by-object join is exercised. Apophis carries two comparison records
    /// (one per common-epoch direction, sbdb + fit); the other objects carry one
    /// each, so the excluded-record count (4) diverges from the excluded-object
    /// count (3, the three radar objects).
    fn radar_scenario() -> (Vec<ValidationResult>, Vec<crate::schema::OrbitComparison>) {
        let mut ap = conv_od_row("Apophis", "rust", "orbit_determination");
        ap.od_converged = Some(true);
        ap.n_obs_used = Some(100);
        let mut ap_radar = conv_od_row("Apophis", "rust", "orbit_determination_radar");
        ap_radar.od_converged = Some(true);
        let mut ap_core = conv_od_row("Apophis", "core", "orbit_determination");
        ap_core.ref_od_n_del_obs_used = Some(20);
        ap_core.ref_od_n_dop_obs_used = Some(30);
        let mut tou = conv_od_row("Toutatis", "rust", "orbit_determination");
        tou.od_converged = Some(true);
        tou.n_obs_used = Some(80);
        let mut tou_core = conv_od_row("Toutatis", "core", "orbit_determination");
        tou_core.ref_od_n_del_obs_used = Some(35);
        tou_core.ref_od_n_dop_obs_used = Some(27);
        let mut ceres = conv_od_row("Ceres", "rust", "orbit_determination");
        ceres.od_converged = Some(true);
        ceres.n_obs_used = Some(200);
        let ceres_core = conv_od_row("Ceres", "core", "orbit_determination");
        let mut wir = conv_od_row("Wirtanen", "rust", "orbit_determination");
        wir.od_converged = Some(true);
        wir.n_obs_used = Some(50);
        let mut wir_core = conv_od_row("Wirtanen", "core", "orbit_determination");
        wir_core.ref_od_n_del_obs_used = Some(0);
        wir_core.ref_od_n_dop_obs_used = Some(1);
        let rows = vec![
            ap, ap_radar, ap_core, tou, tou_core, ceres, ceres_core, wir, wir_core,
        ];
        // Apophis carries a second comparison record at the other common epoch
        // (fit vs sbdb), so excluded records (4) differ from excluded objects (3).
        // The sigma map ingests the sbdb direction only, so closeness is unchanged.
        let mut ap_fit = sbdb_cmp("Apophis", 2.0);
        ap_fit.common_epoch_source = "fit".to_string();
        let cmps = vec![
            sbdb_cmp("Apophis", 2.0),
            ap_fit,
            sbdb_cmp("Toutatis", 4.0),
            sbdb_cmp("Ceres", 1.5),
            sbdb_cmp("Wirtanen", 5.0),
        ];
        (rows, cmps)
    }

    #[test]
    fn od_grid_hatches_the_optical_row_when_jpl_used_radar() {
        let (rows, cmps) = radar_scenario();
        let html = build_od_closeness_grid(&rows, &cmps);

        // The counts are merged onto the CORE row; the hatch must appear on the
        // RUST optical row (the join is by object, so every channel sees them).
        let ap_opt = od_arc_row(&html, "Apophis", "optical");
        assert!(
            ap_opt.contains("oc hatch"),
            "Apophis optical not hatched: {ap_opt}"
        );
        assert!(
            ap_opt.contains("fit used 20 delay and 30 Doppler"),
            "the hatch hover must carry the radar counts: {ap_opt}"
        );
        // Own-fit numbers survive on the hatched row.
        assert!(
            ap_opt.contains(">100</td>"),
            "own-fit n_obs must survive the hatch: {ap_opt}"
        );

        // A JPL-radar object without our own radar arc is hatched all the same.
        let tou_opt = od_arc_row(&html, "Toutatis", "optical");
        assert!(
            tou_opt.contains("oc hatch"),
            "Toutatis optical not hatched: {tou_opt}"
        );
        assert!(
            tou_opt.contains("fit used 35 delay and 27 Doppler"),
            "{tou_opt}"
        );

        // A Doppler-only object (0 delay, 1 Doppler) is excluded too.
        let wir_opt = od_arc_row(&html, "Wirtanen", "optical");
        assert!(
            wir_opt.contains("oc hatch"),
            "Doppler-only object not hatched: {wir_opt}"
        );
        assert!(
            wir_opt.contains("fit used 0 delay and 1 Doppler"),
            "{wir_opt}"
        );

        // The no-radar object keeps its σ_eq closeness fill — never hatched.
        let ceres_opt = od_arc_row(&html, "Ceres", "optical");
        assert!(
            !ceres_opt.contains("oc hatch"),
            "a no-radar object must not be hatched: {ceres_opt}"
        );
        assert!(
            ceres_opt.contains("oc conv"),
            "the no-radar object lost its closeness cell: {ceres_opt}"
        );
    }

    #[test]
    fn od_summary_counts_only_non_radar_objects() {
        let (rows, cmps) = radar_scenario();
        let html = build_od_closeness_grid(&rows, &cmps);
        // The summary div carries the visible text and a record-count title; split
        // the class marker into the open-tag attributes (the title) and the body.
        let (open_tag, body) = html
            .split("class=\"grid-summary\"")
            .nth(1)
            .and_then(|s| s.split_once('>'))
            .unwrap_or(("<no summary>", "<no summary>"));
        let summary = body.split("</div>").next().unwrap_or("<no summary>");
        // Apophis carries two comparison records, the other radar objects one each,
        // so records (4) and objects (3) diverge in the fixture. Only Ceres (no
        // radar) feeds the statistics: σ_eq 1.50, n 1/1, ≤3σ 1. The VISIBLE count
        // is objects (radar_counts.len(), matching §4.1) in the numbers-only idiom;
        // the HOVER title carries the words and the per-record tally.
        assert!(
            summary.contains("σ_eq 1.50 · n 1/1 · ≤3σ 1 · 3 radar excluded"),
            "visible summary must state the excluded OBJECT count (3): {summary}"
        );
        assert!(
            open_tag.contains("3 objects") && open_tag.contains("4 records, both epoch sources"),
            "hover title must state the excluded RECORD count (4): {open_tag}"
        );
        // Lock the divergence: had the visible count regressed to the record tally
        // it would read "4 radar excluded" — objects and records differ here
        // precisely so that regression cannot hide.
        assert!(
            !summary.contains("4 radar excluded"),
            "visible count must come from radar_counts.len(), not the record tally: {summary}"
        );
        // A group row's median states its own sample size, which is the
        // non-radar objects only: Ceres alone, never a radar object beside it.
        assert!(
            html.contains("σ_eq 1.50 · n 1"),
            "group median must state n over non-radar objects"
        );
        for wrong in ["· n 2", "· n 3", "· n 4"] {
            assert!(
                !html.contains(wrong),
                "a radar object entered a group median: {wrong}"
            );
        }
    }

    #[test]
    fn od_grid_is_one_panel_per_tool_following_the_selects() {
        // Part 1 is one tool vs one reference: the default panel is empyrean vs
        // JPL (visible), find_orb vs JPL is a hidden panel the selector reveals,
        // and any other pair falls to the generic hatched "not compared" line.
        // The old fitter-by-object columns (layup / OrbFit / GRSS) are gone from
        // Part 1 — the cross-fitter view lives in Part 2.
        let mut ap = conv_od_row("Apophis", "rust", "orbit_determination");
        ap.od_converged = Some(true);
        let mut ap_r = conv_od_row("Apophis", "rust", "orbit_determination_radar");
        ap_r.od_converged = Some(true);
        let mut bennu = conv_od_row("Bennu", "rust", "orbit_determination");
        bennu.od_converged = Some(true);
        let mut ap_core = conv_od_row("Apophis", "core", "orbit_determination");
        ap_core.findorb_rms_residual = Some(0.3);
        ap_core.findorb_n_obs_used = Some(500);

        let html = build_od_closeness_grid(
            &[ap, ap_r, bennu, ap_core],
            &[sbdb_cmp("Apophis", 1.2), sbdb_cmp("Bennu", 2.0)],
        );

        // One OD pair-panels group.
        assert!(
            html.contains("data-section=\"h3-od\""),
            "the OD section must render a pair-panels group"
        );
        // Default empyrean-vs-JPL panel, visible.
        assert!(
            html.contains("data-tool=\"empyrean\" data-ref=\"jpl\" data-default=\"1\">"),
            "the default panel must be empyrean vs JPL: {html}"
        );
        // find_orb-vs-JPL panel, present but hidden.
        assert!(
            html.contains("data-tool=\"findorb\" data-ref=\"jpl\" hidden>"),
            "find_orb vs JPL must be a hidden panel: {html}"
        );
        // The generic hatched line for a pair with no comparison record.
        assert!(
            html.contains("pair-notcompared generic\" hidden>"),
            "a pair with no records must fall to the generic hatched line"
        );
        // No cross-fitter columns survive in Part 1.
        for gone in [
            ">layup</th>",
            ">OrbFit</th>",
            ">GRSS</th>",
            "not in this run",
        ] {
            assert!(
                !html.contains(gone),
                "the fitter-by-object cross-tool view must be gone from Part 1: found {gone:?}"
            );
        }

        // The visible (empyrean) panel carries every object once as an optical
        // row, with the radar arc as a sub-row — the row structure is uniform
        // per panel.
        let emp_panel = {
            let start = html
                .find("data-tool=\"empyrean\"")
                .expect("empyrean panel present");
            let rest = &html[start..];
            let end = rest.find("data-tool=\"findorb\"").unwrap_or(rest.len());
            &rest[..end]
        };
        assert_eq!(
            emp_panel.matches("class=\"gtool\">optical</td>").count(),
            2,
            "the empyrean panel must have one optical row per object (Apophis, Bennu)"
        );
        assert_eq!(
            emp_panel.matches("class=\"gtool\">+ radar</td>").count(),
            1,
            "the empyrean panel must carry the one radar arc as a sub-row"
        );
    }

    #[test]
    fn od_grid_marks_non_spd_reference_and_no_covariance() {
        // Two flavours of "converged but no metric": Σ_ref not SPD at the
        // common epoch (the metric is undefined — "?"), and no comparison
        // record at all ("·"). Neither is a failure and neither is regularised.
        let mut iris = conv_od_row("Iris", "rust", "orbit_determination");
        iris.od_converged = Some(true);
        let mut apophis = conv_od_row("Apophis", "rust", "orbit_determination");
        apophis.od_converged = Some(true);
        let html = build_od_closeness_grid(
            &[iris, apophis],
            &[sbdb_cmp("Iris", f64::NAN), sbdb_cmp("Apophis", 1.2)],
        );
        let iris_row = od_arc_row(&html, "Iris", "optical");
        assert!(
            iris_row.contains("conv undef") && iris_row.contains("Σ_ref not SPD"),
            "a non-SPD reference must render the undefined-metric state: {iris_row}"
        );
        let apophis_row = od_arc_row(&html, "Apophis", "optical");
        assert!(
            apophis_row.contains("oc conv gb"),
            "Apophis must carry a filled sigma cell: {apophis_row}"
        );
    }

    // ── Timing restructure (empyrean-52esa, pass 6) ──────────────────

    /// A run exercising every timing surface: rust propagation arms, ASSIST
    /// integrate-only timing, find_orb OD timing, kete/jorbit timing rows, and
    /// core-channel ephemeris timing with the external accuracy that makes the
    /// ASSIST/find_orb pairs legal.
    fn timing_fixture() -> Vec<ValidationResult> {
        let mut rows = Vec::new();
        for (o, p) in [("Apophis", "NEO"), ("Bennu", "NEO")] {
            for dt in [0.0_f64, 30.0] {
                for arm in ["none_detection_on", "first_order_detection_on"] {
                    let mut r = synthetic_rust_prop_row(o, dt);
                    r.population = p.to_string();
                    r.propagation_uncertainty = Some(arm.to_string());
                    r.emp_time_ms = Some(3.0);
                    rows.push(r);
                }
                let mut cr = synthetic_core_prop_row(o, p, dt);
                cr.kete_time_ms = Some(4.0);
                cr.jorbit_time_ms = Some(5.0);
                rows.push(cr);
                let mut ar = synthetic_rust_prop_row(o, dt);
                ar.population = p.to_string();
                ar.channel = "assist".to_string();
                ar.propagation_uncertainty = Some("assist_default_variational_off".to_string());
                ar.assist_time_ms = Some(6.0);
                ar.emp_time_ms = None;
                rows.push(ar);
                // rust ephemeris row carries no wall-clock (the eph-timing gap).
                let mut er = synthetic_rust_prop_row(o, dt);
                er.population = p.to_string();
                er.test_type = "ephemeris".to_string();
                er.propagation_uncertainty = Some("first_order_detection_on".to_string());
                er.separation_arcsec = Some(0.001);
                er.emp_time_ms = None;
                rows.push(er);
                let mut cer = synthetic_core_prop_row(o, p, dt);
                cer.test_type = "ephemeris".to_string();
                cer.findorb_separation_arcsec = Some(0.5);
                cer.emp_time_ms = Some(8.0);
                cer.kete_time_ms = Some(4.0);
                cer.jorbit_time_ms = Some(5.0);
                rows.push(cer);
            }
            let mut od = conv_od_row(o, "rust", "orbit_determination");
            od.od_converged = Some(true);
            od.population = p.to_string();
            od.emp_time_ms = Some(50.0);
            rows.push(od);
            let mut odc = conv_od_row(o, "core", "orbit_determination");
            odc.population = p.to_string();
            odc.findorb_rms_residual = Some(0.3);
            odc.findorb_n_obs_used = Some(100);
            odc.findorb_time_ms = Some(200.0);
            rows.push(odc);
        }
        rows
    }

    fn tool_tokens(slug: &str) -> Vec<&'static str> {
        match slug {
            "empyrean" => vec!["empyrean"],
            "assist" => vec!["ASSIST"],
            "findorb" => vec!["find_orb"],
            "jpl" => vec!["JPL"],
            "kete" => vec!["kete"],
            "jorbit" => vec!["jorbit"],
            _ => vec![],
        }
    }

    /// Depth-matched inner HTML of one pre-rendered pair-panel — safe for the
    /// last panel in a container, unlike `panel_slice`, which runs to the next
    /// panel open.
    fn pair_panel_span<'a>(html: &'a str, section: &str, tool: &str, refr: &str) -> &'a str {
        let cont = format!("data-section=\"{section}\"");
        let cstart = html.find(&cont).expect("pair-panels container");
        let region = &html[cstart..];
        let key = format!("data-tool=\"{tool}\" data-ref=\"{refr}\"");
        let kpos = region
            .find(&key)
            .unwrap_or_else(|| panic!("panel {tool}/{refr}"));
        let open = region[..kpos]
            .rfind("<div class=\"pair-panel\"")
            .expect("panel open tag");
        let bytes = region.as_bytes();
        let mut depth = 0i32;
        let mut i = open;
        while i < bytes.len() {
            if bytes[i..].starts_with(b"</div>") {
                depth -= 1;
                i += 6;
                if depth == 0 {
                    return &region[open..i];
                }
            } else if bytes[i..].starts_with(b"<div") {
                depth += 1;
                i += 4;
            } else {
                i += 1;
            }
        }
        &region[open..]
    }

    #[test]
    fn section1_cost_panels_are_pair_only_and_untimed_is_hatched() {
        // Every Section-1 cost pair-panel names only its own two tools. On
        // propagation / ephemeris a timed member draws a heatmap and an untimed
        // member (JPL always; find_orb here) is one hatched line whose reason
        // rides the hover; orbit determination keeps its per-object table. kete
        // and jorbit (timing only, never a pair member) never leak in.
        let rows = timing_fixture();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&rows, &[], &out, None).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();

        let all = ["empyrean", "assist", "findorb", "jpl", "kete", "jorbit"];
        for (axis, id) in [
            ("propagation", "t1-prop"),
            ("ephemeris", "t2-eph"),
            ("orbit_determination", "t3-od"),
        ] {
            let pairs = timing_pairs(&rows, axis);
            assert!(!pairs.is_empty(), "no timing pairs for {axis}");
            for (ts, _tl, rs, _rl) in pairs {
                let panel = pair_panel_span(&html, id, ts, rs);
                for other in all {
                    if other == ts || other == rs {
                        continue;
                    }
                    for tok in tool_tokens(other) {
                        assert!(
                            !panel.contains(tok),
                            "panel {id} {ts}/{rs} leaks tool token {tok:?}"
                        );
                    }
                }
                // Orbit determination keeps its per-object table (a fit has no
                // horizons); propagation / ephemeris draw a heatmap per timed
                // member instead.
                if axis == "orbit_determination" {
                    assert!(
                        panel.contains(&format!("id=\"{id}-{ts}-{rs}\"")),
                        "OD panel {ts}/{rs} must stay a per-object table"
                    );
                    // JPL is a hatched column in the table, its reason on hover.
                    if ts == "jpl" || rs == "jpl" {
                        assert!(
                            panel.contains("JPL: reference solution, not timed"),
                            "OD panel {ts}/{rs} missing the hatched JPL reason"
                        );
                    }
                } else {
                    assert!(
                        !panel.contains(&format!("id=\"{id}-{ts}-{rs}\">")),
                        "cost panel {ts}/{rs} must not draw a two-tool table"
                    );
                    let has_heatmap = panel.contains("class=\"agrid ahgrid\"");
                    // A JPL member alongside a timed member rides its own hatched
                    // "not timed" line, its reason on hover.
                    if (ts == "jpl" || rs == "jpl") && has_heatmap {
                        assert!(
                            panel.contains("JPL: reference solution, not timed"),
                            "cost panel {ts}/{rs} missing the hatched JPL reason"
                        );
                    }
                    // A pair with no timed member collapses to one hatched line.
                    if !has_heatmap {
                        assert!(
                            panel.contains("&mdash; not timed"),
                            "cost panel {ts}/{rs} with no timed member must show a hatched line"
                        );
                    }
                }
            }
        }
        // The default propagation cost panel draws empyrean's own timing heatmap.
        assert!(
            html.contains("id=\"t1-prop-empyrean-jpl-empyrean\""),
            "the default propagation cost panel must draw empyrean's timing heatmap"
        );
    }

    #[test]
    fn cost_heatmap_cells_population_row_and_median_are_correct() {
        // Three NEO objects, three horizons each, distinct per-cell wall clocks
        // so every median is unambiguous. The empyrean propagation cost heatmap
        // must show each (object, horizon) wall clock, a per-population median
        // row, and a trailing MEDIAN equal to the per-object median over
        // horizons (the number the old per-object cost table showed).
        let times = [
            ("A", [1.0_f64, 2.0, 3.0]),
            ("B", [4.0, 5.0, 6.0]),
            ("C", [7.0, 8.0, 9.0]),
        ];
        let dts = [0.0_f64, 30.0, 90.0];
        let mut rows = Vec::new();
        for (obj, ts) in times {
            for (k, &dt) in dts.iter().enumerate() {
                let mut r = synthetic_rust_prop_row(obj, dt);
                r.population = "NEO".to_string();
                r.propagation_uncertainty = Some("first_order_detection_on".to_string());
                r.emp_time_ms = Some(ts[k]);
                rows.push(r);
            }
        }
        let html = build_pair_timing_panels(
            &rows,
            "propagation",
            "t1-prop",
            emp_eph_channel(&rows),
            emp_od_channel(&rows),
        );
        let at = html
            .find("id=\"t1-prop-empyrean-jpl-empyrean\"")
            .expect("empyrean propagation cost heatmap");
        let end = html[at..].find("</table>").map(|e| at + e).unwrap();
        let table = &html[at..end];

        let obj_row = |name: &str| -> String {
            let needle = format!("<td class=\"gobjname2\">{name}</td>");
            let s = table
                .find(&needle)
                .unwrap_or_else(|| panic!("no row for {name}"));
            let e = table[s..].find("</tr>").map(|x| s + x).unwrap();
            table[s..e].to_string()
        };
        // Every (object, horizon) cell shows its own wall clock on hover.
        for (obj, ts) in times {
            let row = obj_row(obj);
            for &v in &ts {
                assert!(
                    row.contains(&format!("title=\"{}\"", fmt_ms(v))),
                    "{obj} row must show cell {v} ms: {row}"
                );
            }
        }
        // Trailing MEDIAN = per-object median over horizons (2, 5, 8 ms).
        for (obj, med) in [("A", 2.0), ("B", 5.0), ("C", 8.0)] {
            let row = obj_row(obj);
            assert!(
                row.contains(&format!("<span class=\"wnum\">{}</span>", fmt_ms(med))),
                "{obj} MEDIAN must be {med} ms: {row}"
            );
        }
        // Population header row: median over the three objects at each horizon
        // (4, 5, 6 ms), then a trailing median of those (5 ms).
        let ps = table
            .find("<td class=\"gpopname\">NEO (3)</td>")
            .expect("NEO population row");
        let pe = table[ps..].find("</tr>").map(|x| ps + x).unwrap();
        let poprow = &table[ps..pe];
        for v in [4.0_f64, 5.0, 6.0] {
            assert!(
                poprow.contains(&format!("title=\"{}\"", fmt_ms(v))),
                "NEO population row must show {v} ms: {poprow}"
            );
        }
        assert!(
            poprow.contains(&format!("<span class=\"wnum\">{}</span>", fmt_ms(5.0))),
            "NEO population MEDIAN must be 5.0 ms: {poprow}"
        );
    }

    #[test]
    fn cost_eph_cell_is_the_median_of_its_observer_rows() {
        // Several observer rows share one ephemeris cell: the cell is their
        // median and the hover says "median of n rows" (nothing hidden).
        let mut rows = vec![synthetic_rust_prop_row("A", 0.0)];
        for v in [2.0_f64, 4.0, 6.0] {
            let mut er = synthetic_rust_prop_row("A", 0.0);
            er.test_type = "ephemeris".to_string();
            er.channel = "core".to_string();
            er.emp_time_ms = Some(v);
            er.separation_arcsec = Some(0.001);
            rows.push(er);
        }
        assert_eq!(emp_eph_channel(&rows), ("core", true));
        let html = build_pair_timing_panels(
            &rows,
            "ephemeris",
            "t2-eph",
            emp_eph_channel(&rows),
            emp_od_channel(&rows),
        );
        // The cell is the median (4 ms) and its hover names the three rows.
        assert!(
            html.contains("title=\"4.0 ms · median of 3 rows\""),
            "the ephemeris cell must be the median of its three observer rows"
        );
    }

    #[test]
    fn cost_pair_with_untimed_reference_is_one_heatmap_and_one_hatched_line() {
        // empyrean is timed on propagation; JPL is never timed. The default
        // panel draws exactly one heatmap (empyrean) plus one hatched "not
        // timed" line (JPL, reason on hover) and names no tool outside the pair.
        let mut rows = Vec::new();
        for dt in [0.0_f64, 30.0] {
            let mut r = synthetic_rust_prop_row("A", dt);
            r.population = "NEO".to_string();
            r.propagation_uncertainty = Some("first_order_detection_on".to_string());
            r.emp_time_ms = Some(2.0);
            rows.push(r);
        }
        let html = build_pair_timing_panels(
            &rows,
            "propagation",
            "t1-prop",
            emp_eph_channel(&rows),
            emp_od_channel(&rows),
        );
        let panel = pair_panel_span(&html, "t1-prop", "empyrean", "jpl");
        assert_eq!(
            panel.matches("class=\"agrid ahgrid\"").count(),
            1,
            "exactly one heatmap in the default panel"
        );
        assert_eq!(
            panel.matches("class=\"pair-notcompared\"").count(),
            1,
            "exactly one hatched line in the default panel"
        );
        assert!(
            panel.contains("JPL: reference solution, not timed"),
            "the JPL hatched line carries its reason on hover"
        );
        for tok in ["ASSIST", "find_orb", "kete", "jorbit"] {
            assert!(!panel.contains(tok), "the default panel leaks {tok}");
        }
    }

    #[test]
    fn orbit_determination_cost_stays_a_per_object_table() {
        // A fit has no horizons, so OD cost keeps its per-object two-tool table
        // and never becomes a timing heatmap.
        let rows = timing_fixture();
        let html = build_pair_timing_panels(
            &rows,
            "orbit_determination",
            "t3-od",
            emp_eph_channel(&rows),
            emp_od_channel(&rows),
        );
        assert!(
            html.contains("id=\"t3-od-empyrean-jpl\">"),
            "OD keeps its per-object table"
        );
        assert!(
            html.contains("agrid fidgrid"),
            "OD cost uses the per-object grid class, not the heatmap"
        );
        assert!(
            !html.contains("t3-od-empyrean-jpl-empyrean"),
            "OD must not draw a per-member timing heatmap"
        );
        assert!(
            !html.contains("class=\"agrid ahgrid\""),
            "OD cost must not use the heatmap table"
        );
    }

    #[test]
    fn run_summary_is_ordered_into_the_appendix_and_the_page_opens_on_part_one() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&timing_fixture(), &[], &out, None).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        let order = |id: &str| -> u32 {
            let rule = format!("#{id} {{ order: ");
            let at = html
                .find(&rule)
                .unwrap_or_else(|| panic!("no flex order rule for #{id}"));
            let rest = &html[at + rule.len()..];
            rest[..rest.find(';').unwrap()].parse().unwrap()
        };
        let first = ["part1", "part2", "part3", "part4", "appendix", "s01", "s13"]
            .into_iter()
            .min_by_key(|id| order(id))
            .unwrap();
        assert_eq!(first, "part1", "the page must open on Part 1");
        assert!(
            order("s01") > order("appendix"),
            "run summary not in appendix"
        );
        assert!(order("s01") < order("s01b") && order("s01b") < order("s13"));
    }

    #[test]
    fn part_two_hatched_cells_carry_their_reason_on_hover_only() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&timing_fixture(), &[], &out, None).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        let start = html.find("id=\"s-ranking\"").expect("Part 2 section");
        let end = html[start..].find("id=\"s-timing\"").unwrap() + start;
        let part2 = &html[start..end];
        let mut hatched = 0;
        for cell in part2.split("<td class=\"r1c hatch\"").skip(1) {
            let (open, rest) = cell.split_once('>').expect("open tag");
            let text = &rest[..rest.find("</td>").expect("cell end")];
            assert!(
                open.contains("title=\""),
                "hatched cell without a reason: {open}"
            );
            assert!(
                !open.contains("title=\"\""),
                "hatched cell with an empty reason"
            );
            assert!(
                text.is_empty(),
                "hatched cell prints its reason as words: {text:?}"
            );
            hatched += 1;
        }
        assert!(
            hatched >= 10,
            "expected the kete, jorbit, JPL and absent-tool hatches, found {hatched}"
        );
        for words in [
            ">timing only<",
            ">reference<",
            ">not in this run<",
            ">runs no fit<",
        ] {
            assert!(
                !part2.contains(words),
                "visible hatch label survived: {words}"
            );
        }
        // The by-object-and-fitter matrix states a fit's state with the OD
        // grid's glyphs, never as a word, and every glyph carries its reason.
        for words in [">fit<", ">conv<", ">n/d<"] {
            assert!(!part2.contains(words), "state written as a word: {words}");
        }
        let fitter = &part2[part2.find("r3grid").expect("by object and fitter")..];
        let mut glyphs = 0;
        for cell in fitter.split("<td class=\"r1c\"").skip(1) {
            let (open, rest) = cell.split_once('>').expect("open tag");
            let text = &rest[..rest.find("</td>").expect("cell end")];
            if ["·", "?", "✕"].contains(&text) {
                assert!(
                    open.contains("title=\""),
                    "state glyph {text} without a reason"
                );
                glyphs += 1;
            }
        }
        assert!(
            glyphs >= 2,
            "expected state glyphs in the fitter matrix, found {glyphs}"
        );
    }

    #[test]
    fn wall_clock_key_marks_untimed_and_the_rail_names_no_ranking() {
        let timing = grid_key_html(GridAxis::TimingMs);
        assert!(timing.contains("not timed"), "timing key lacks not timed");
        for absent in ["not compared", "runaway"] {
            assert!(!timing.contains(absent), "timing key shows {absent}");
        }
        for axis in [GridAxis::PropKm, GridAxis::EphMas] {
            let key = grid_key_html(axis);
            assert!(key.contains("not compared") && key.contains("runaway"));
            assert!(!key.contains("not timed"));
        }

        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&timing_fixture(), &[], &out, None).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        let rail = html
            .find("data-target=\"s-ranking\"")
            .expect("rail entry for section 2.1");
        let entry = &html[rail..rail + html[rail..].find("</a>").unwrap()];
        assert!(entry.ends_with("2.1 Side by side"), "rail entry: {entry}");
    }

    #[test]
    fn grid_cell_hatch_titles_come_from_the_shared_consts() {
        // The hottest cell path speaks the shared hatch phrases, so a future
        // change to a const can never leave a stale literal behind in a data
        // cell. Output is byte-identical while the values agree; this pins them.
        let timed = grid_cell(GridAxis::TimingMs, None, false, false, None, None);
        assert!(
            timed.contains(&format!("title=\"{HATCH_NOT_TIMED}\"")),
            "untimed cell hover must be the shared const: {timed}"
        );
        for axis in [GridAxis::PropKm, GridAxis::EphMas] {
            let cell = grid_cell(axis, None, false, false, None, None);
            assert!(
                cell.contains(&format!("title=\"{HATCH_NOT_COMPARED}\"")),
                "not-compared cell hover must be the shared const: {cell}"
            );
        }
    }

    #[test]
    fn protected_heatmap_only_change_is_the_impactor_arcends_mark() {
        // Owner ruling: the h1-prop / h2-eph agreement heatmaps are protected —
        // every numeric cell, every row and every column stays identical. The one
        // text→mark change this pass is the impactor forward "arc ends at impact"
        // sentence becoming the hatched arc-ends state (empty cell, reason on
        // hover). This renders both protected axes and pins exactly that: the
        // numeric cells survive, and the only arc-ends markup is the hatched mark.
        use std::collections::{BTreeSet, HashMap};
        let objects = [("Apophis", "G1"), ("2020 CD3", "G1")];
        let dts = [-30i64, 0, 30];
        let mut vals: HashMap<(String, i64), f64> = HashMap::new();
        // A non-impactor carries a number at every horizon.
        vals.insert(("Apophis".to_string(), -30), 100.0);
        vals.insert(("Apophis".to_string(), 0), 10.0);
        vals.insert(("Apophis".to_string(), 30), 1000.0);
        // The impactor carries numbers only up to impact (no forward horizon).
        vals.insert(("2020 CD3".to_string(), -30), 5.0);
        vals.insert(("2020 CD3".to_string(), 0), 0.5);
        let model_gap: BTreeSet<&str> = BTreeSet::new();
        let impactors: BTreeSet<&str> = ["2020 CD3"].into_iter().collect();
        let marks = GridMarks {
            model_gap: &model_gap,
            impactors: &impactors,
        };
        for (id, axis) in [("h1-prop", GridAxis::PropKm), ("h2-eph", GridAxis::EphMas)] {
            let tool = GridTool {
                label: "empyrean".to_string(),
                tool_slug: "empyrean".to_string(),
                ref_slug: "jpl".to_string(),
                ref_label: "JPL".to_string(),
                present: true,
                vals: vals.clone(),
                sigma: HashMap::new(),
            };
            let html = render_agreement_grid(id, axis, &objects, &dts, &[tool], &marks);
            // The impactor forward cell is the hatched arc-ends mark, empty text,
            // reason on hover; the colspan equals the forward-horizon count (1).
            assert!(
                html.contains(
                    "<td class=\"arcends hatch\" colspan=\"1\" title=\"arc ends at impact — no forward horizon\"></td>"
                ),
                "{id}: impactor forward cell must be the hatched arc-ends mark: {html}"
            );
            // The old visible sentence must be gone from every cell.
            assert!(
                !html.contains(">arc ends at impact</td>"),
                "{id}: the arc-ends sentence must no longer render in a cell"
            );
            // Every numeric cell survives: the population-median row and the
            // non-impactor each carry three horizons, and the impactor carries its
            // two pre-impact horizons = eight `cn` number spans. The impactor's
            // single forward horizon is the arc-ends mark, never a dropped column.
            assert_eq!(
                html.matches("<span class=\"cn\">").count(),
                8,
                "{id}: every numeric cell must survive: {html}"
            );
            // Rows and columns are intact: one object row per object, one horizon
            // column header per dt.
            assert_eq!(
                html.matches("class=\"gobjr\"").count(),
                2,
                "{id}: both object rows must render"
            );
            assert_eq!(
                html.matches("<th class=\"gdt").count(),
                3,
                "{id}: every horizon column header must render"
            );
        }
    }

    #[test]
    fn report_head_declares_a_favicon() {
        // A declared icon stops the browser requesting /favicon.ico, so the
        // console stays clean on the owner's local server.
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&timing_fixture(), &[], &out, None).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        let head = &html[..html.find("</head>").expect("head present")];
        assert!(
            head.contains("rel=\"icon\""),
            "the page must declare a favicon so no /favicon.ico request is made"
        );
    }

    #[test]
    fn timing_is_relocated_to_part2_and_speed_strip_is_gone() {
        let rows = timing_fixture();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&rows, &[], &out, None).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();

        let p2 = html.find("id=\"part2\"").expect("part2 anchor");
        for id in ["g1-prop-time", "g2-eph-time", "g3-od-time", "s-timing"] {
            let at = html
                .find(&format!("id=\"{id}\""))
                .unwrap_or_else(|| panic!("missing {id}"));
            assert!(at > p2, "{id} must occur after the part2 anchor");
        }
        // No cross-tool (3+ tool) timing table appears in Part 1.
        let before = &html[..p2];
        for grid in ["g1-prop-time", "g2-eph-time", "g3-od-time"] {
            assert!(
                !before.contains(grid),
                "{grid} must not appear before part2"
            );
        }
        // Section-1 per-pair timing still exists (before part2).
        assert!(
            before.contains("data-section=\"t1-prop\""),
            "Section-1 propagation timing panels missing"
        );
        assert!(!html.contains("speed-strip"), "speed-strip must be removed");
        assert!(
            !html.contains("buildSpeedStrip"),
            "buildSpeedStrip must be removed"
        );
    }

    #[test]
    fn ephemeris_timing_core_fallback_is_labelled_rust_is_not() {
        // rust carries no ephemeris wall-clock -> core channel, labelled.
        let mut rust_eph = synthetic_rust_prop_row("Apophis", 0.0);
        rust_eph.test_type = "ephemeris".to_string();
        rust_eph.emp_time_ms = None;
        rust_eph.separation_arcsec = Some(0.001);
        let mut core_eph = synthetic_core_prop_row("Apophis", "NEO", 0.0);
        core_eph.test_type = "ephemeris".to_string();
        core_eph.emp_time_ms = Some(90.0);
        core_eph.findorb_separation_arcsec = Some(0.5);
        let mut rp = synthetic_rust_prop_row("Apophis", 0.0);
        rp.propagation_uncertainty = Some("first_order_detection_on".to_string());
        let rows = vec![rust_eph, core_eph, rp];
        assert_eq!(emp_eph_channel(&rows), ("core", true));
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&rows, &[], &out, None).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        assert!(
            html.contains("core channel</span>"),
            "the core-for-rust ephemeris fallback must render a labelled chip"
        );

        // rust carries ephemeris wall-clock -> rust used, no chip.
        let mut rust_eph2 = synthetic_rust_prop_row("Apophis", 0.0);
        rust_eph2.test_type = "ephemeris".to_string();
        rust_eph2.emp_time_ms = Some(5.0);
        rust_eph2.separation_arcsec = Some(0.001);
        let mut core_eph2 = synthetic_core_prop_row("Apophis", "NEO", 0.0);
        core_eph2.test_type = "ephemeris".to_string();
        core_eph2.emp_time_ms = Some(90.0);
        core_eph2.findorb_separation_arcsec = Some(0.5);
        let mut rp2 = synthetic_rust_prop_row("Apophis", 0.0);
        rp2.propagation_uncertainty = Some("first_order_detection_on".to_string());
        let rows2 = vec![rust_eph2, core_eph2, rp2];
        assert_eq!(emp_eph_channel(&rows2), ("rust", false));
        let g = emp_eph_getter("rust");
        let picked: Vec<f64> = rows2
            .iter()
            .filter(|r| r.test_type == "ephemeris")
            .filter_map(g)
            .collect();
        assert_eq!(picked, vec![5.0], "the rust ephemeris value must be used");
        let dir2 = tempfile::tempdir().unwrap();
        let out2 = dir2.path().join("report.html");
        generate_report(&rows2, &[], &out2, None).unwrap();
        let html2 = std::fs::read_to_string(&out2).unwrap();
        assert!(
            !html2.contains("core channel</span>"),
            "no core-fallback chip when rust carries the wall-clock"
        );
        assert!(
            html2.contains("5.0 ms"),
            "the rust ephemeris wall-clock must render"
        );
    }

    fn timing_row_slice(html: &str, obj: &str) -> String {
        let needle = format!("<td class=\"gtool\">{obj}</td>");
        let at = html
            .find(&needle)
            .unwrap_or_else(|| panic!("no row for {obj}"));
        let end = html[at..].find("</tr>").map(|e| at + e).unwrap();
        html[at..end].to_string()
    }
    fn timing_block_slice(html: &str, pop: &str) -> String {
        let needle = format!("class=\"gblock-l\">{pop} (");
        let at = html
            .find(&needle)
            .unwrap_or_else(|| panic!("no block for {pop}"));
        let end = html[at..].find("</tr>").map(|e| at + e).unwrap();
        html[at..end].to_string()
    }

    #[test]
    fn timing_grid_per_object_and_per_population_medians_are_correct() {
        let mut rows = Vec::new();
        for v in [2.0, 4.0, 6.0] {
            let mut r = synthetic_rust_prop_row("A", 0.0);
            r.population = "NEO".to_string();
            r.propagation_uncertainty = Some("first_order_detection_on".to_string());
            r.emp_time_ms = Some(v);
            rows.push(r);
        }
        for v in [10.0, 20.0] {
            let mut r = synthetic_rust_prop_row("B", 30.0);
            r.population = "NEO".to_string();
            r.propagation_uncertainty = Some("first_order_detection_on".to_string());
            r.emp_time_ms = Some(v);
            rows.push(r);
        }
        let cols = vec![TimingCol::timed(
            "emp Jet1",
            rust_arm("first_order_detection_on"),
            BOUND_B,
        )];
        let html = build_timing_grid(&rows, "tg", "", "propagation", &cols, true, "");

        let a = timing_row_slice(&html, "A");
        assert!(a.contains("4.0 ms"), "A median should be 4.0 ms: {a}");
        assert!(
            !a.contains("2.0 ms") && !a.contains("6.0 ms"),
            "A cell must be the median, not the min/max: {a}"
        );
        let b = timing_row_slice(&html, "B");
        assert!(b.contains("10.0 ms"), "B median should be 10.0 ms: {b}");
        assert!(
            !b.contains("20.0 ms"),
            "B cell must be the median, not the max: {b}"
        );
        let block = timing_block_slice(&html, "NEO");
        assert!(
            block.contains("6.0 ms"),
            "NEO per-population median should be 6.0 ms: {block}"
        );
        assert!(
            !block.contains("2.0 ms") && !block.contains("20.0 ms"),
            "the per-population median must not be the min/max: {block}"
        );
    }

    // The narrow, tall timing / cost grids (T1 wall clock, G* per object, and the
    // Section 1.3 OD cost pairs) opt into the ≥1360px overflow-drop rule via the
    // shared grid-sticky marker so their column header pins while the long body
    // scrolls. The wide fidelity grids share the fidgrid class but must keep
    // horizontal scroll, so they stay on the plain grid-scroll wrapper. Guards
    // both halves: the wrappers carry the marker AND the CSS rule targets it.
    #[test]
    fn timing_grids_opt_into_the_sticky_overflow_rule() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&[synthetic_rust_prop_row("Apophis", 0.0)], &[], &out, None).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();

        // Wrapper side: the T1 wall-clock and a per-object G* grid carry the marker.
        assert!(
            html.contains(
                "grid-scroll grid-sticky\"><table class=\"agrid r1grid\" id=\"t1-speed-matrix\""
            ),
            "T1 wall-clock wrapper must carry the sticky marker"
        );
        assert!(
            html.contains(
                "grid-scroll grid-sticky\"><table class=\"agrid fidgrid\" id=\"g1-prop-time\""
            ),
            "per-object timing grid wrapper must carry the sticky marker"
        );
        // Precision: the wide per-object fidelity grid shares the fidgrid class but
        // must keep the plain wrapper (it scrolls horizontally, never pins).
        assert!(
            html.contains("grid-scroll\"><table class=\"agrid fidgrid\"><thead><tr><th class=\"gobj\">object</th>"),
            "wide fidelity grid must keep the plain grid-scroll wrapper"
        );
        // Rule side: the ≥1360px overflow-drop rule must target the marker, or the
        // wrappers pin nothing.
        assert!(
            html.contains(".grid-sticky { overflow: visible; }"),
            "the min-width:1360px overflow-drop rule must include .grid-sticky"
        );
    }

    #[test]
    fn part2_timing_t1_percentiles_are_correct() {
        // Only kete propagation timing is populated: 1..=10 ms.
        let mut rows = Vec::new();
        for i in 1..=10 {
            let mut r = synthetic_core_prop_row("A", "NEO", i as f64);
            r.kete_time_ms = Some(i as f64);
            r.emp_time_ms = None;
            r.assist_vs_horizons_km = None;
            r.findorb_vs_horizons_km = None;
            rows.push(r);
        }
        let html = build_part2_timing_t1(&rows);
        // p5 = 1.0, p50 = 5.0, p95 = 9.0, n = 10 (report percentile convention).
        assert!(
            html.contains("title=\"1.0 ms\""),
            "kete p5 should be 1.0 ms"
        );
        assert!(
            html.contains("title=\"5.0 ms\""),
            "kete p50 should be 5.0 ms"
        );
        assert!(
            html.contains("title=\"9.0 ms\""),
            "kete p95 should be 9.0 ms"
        );
        assert!(
            !html.contains("title=\"10.0 ms\""),
            "p95 must be the percentile, not the maximum"
        );
        assert!(html.contains(">10</td>"), "kete n should be 10");
    }

    /// Part-2 OD timing reads one resolved channel (rust preferred), never a
    /// silent rust+core pool, and T1's OD median agrees with the G3 per-object
    /// median. The fixture carries rust OD rows (10/20/30 ms) and core OD rows
    /// (400 ms): the rust-only p50 is 20 ms, while a pool would surface 400 ms.
    #[test]
    fn od_timing_uses_one_channel_and_never_pools_rust_and_core() {
        // A rust propagation row makes the object canonical for the per-object
        // grids (their object list is the rust propagation set).
        let mut rows = vec![synthetic_rust_prop_row("Apophis", 0.0)];
        for v in [10.0, 20.0, 30.0] {
            let mut r = synthetic_core_od_row("Apophis");
            r.channel = "rust".to_string();
            r.emp_time_ms = Some(v);
            rows.push(r);
        }
        for _ in 0..7 {
            let mut r = synthetic_core_od_row("Apophis");
            r.emp_time_ms = Some(400.0);
            rows.push(r);
        }
        // rust carries OD timing, so the resolved channel is rust (no fallback).
        assert_eq!(emp_od_channel(&rows), ("rust", false));

        // T1: the empyrean row under the OD boundary group shows the rust-only
        // p50 (20 ms), never the pooled p50 (which would be 400 ms).
        let t1 = build_part2_timing_t1(&rows);
        let od_at = t1
            .find("orbit determination · per fit")
            .expect("T1 OD boundary group");
        let t1_od_row = timing_row_slice(&t1[od_at..], "empyrean");
        assert!(
            t1_od_row.contains("20.0 ms"),
            "T1 OD empyrean p50 must be the rust-only 20.0 ms: {t1_od_row}"
        );
        assert!(
            !t1_od_row.contains("400.0 ms"),
            "T1 OD empyrean must not pool the core channel (400.0 ms): {t1_od_row}"
        );

        // G3: the per-object empyrean cell shows the same rust-only median, so
        // T1 and G3 agree instead of disagreeing across a silent pool.
        let grids = build_timing_grids(&rows, emp_eph_channel(&rows), emp_od_channel(&rows));
        let g3_at = grids.find("id=\"g3-od-time\"").expect("G3 OD grid");
        let g3_row = timing_row_slice(&grids[g3_at..], "Apophis");
        assert!(
            g3_row.contains("20.0 ms"),
            "G3 OD empyrean median must be the rust-only 20.0 ms: {g3_row}"
        );
        assert!(
            !g3_row.contains("400.0 ms"),
            "G3 OD empyrean must not pool the core channel (400.0 ms): {g3_row}"
        );

        // A core-only OD run falls back to core and labels it (never silent).
        let core_only: Vec<_> = (0..3)
            .map(|_| {
                let mut r = synthetic_core_od_row("Apophis");
                r.emp_time_ms = Some(50.0);
                r
            })
            .collect();
        assert_eq!(emp_od_channel(&core_only), ("core", true));
        let t1c = build_part2_timing_t1(&core_only);
        let odc_at = t1c
            .find("orbit determination · per fit")
            .expect("T1 OD boundary group (core-only)");
        assert!(
            t1c[odc_at..].contains("core channel"),
            "a core-only OD run must label the core-channel fall-back in T1"
        );
    }

    // ── pass-7 redesign invariants ───────────────────────────────────────────

    /// Remove every `<tag>…</tag>` span (script/style) so the remaining text is
    /// what a reader sees, not what the client JS holds.
    fn strip_tag_spans(html: &str, tag: &str) -> String {
        let open = format!("<{tag}");
        let close = format!("</{tag}>");
        let mut out = String::with_capacity(html.len());
        let mut rest = html;
        while let Some(s) = rest.find(&open) {
            out.push_str(&rest[..s]);
            let after = &rest[s..];
            match after.find(&close) {
                Some(e) => rest = &after[e + close.len()..],
                None => {
                    rest = "";
                    break;
                }
            }
        }
        out.push_str(rest);
        out
    }

    /// The inner HTML of one pre-rendered pair-panel, bounded by the next
    /// pair-panel open (every panel the tests inspect is followed by another).
    fn panel_slice<'a>(html: &'a str, section: &str, tool: &str, refr: &str) -> &'a str {
        let cont = format!("data-section=\"{section}\"");
        let cstart = html.find(&cont).expect("pair-panels container");
        let region = &html[cstart..];
        let key = format!("data-tool=\"{tool}\" data-ref=\"{refr}\"");
        let pstart = region
            .find(&key)
            .unwrap_or_else(|| panic!("panel {tool}/{refr}"));
        let after = &region[pstart..];
        let end = after
            .find("<div class=\"pair-panel\"")
            .unwrap_or(after.len());
        &after[..end]
    }

    /// Rows that produce the empyrean/jpl, empyrean/assist, assist/jpl,
    /// empyrean/findorb and findorb/jpl propagation pairs, with one comet that
    /// carries a Marsden ΔT (a model-gap object ASSIST cannot model).
    fn pair_test_rows() -> Vec<ValidationResult> {
        let mut rows = Vec::new();
        for (o, p, gap) in [("Apophis", "NEO", false), ("67P", "Comet", true)] {
            for dt in [0.0_f64, 30.0] {
                let mut rr = synthetic_rust_prop_row(o, dt);
                rr.population = p.to_string();
                rr.propagation_uncertainty = Some("first_order_detection_on".to_string());
                if gap {
                    rr.ic_non_grav_dt = Some(1e-9);
                }
                rows.push(rr);
                let mut cr = synthetic_core_prop_row(o, p, dt);
                cr.emp_vs_findorb_km = Some(0.05);
                if gap {
                    cr.ic_non_grav_dt = Some(1e-9);
                }
                rows.push(cr);
            }
        }
        rows
    }

    fn render_rows(rows: &[ValidationResult]) -> String {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(rows, &[], &out, None).unwrap();
        std::fs::read_to_string(&out).unwrap()
    }

    /// Text nodes only: drop every `<…>` tag so attribute values (href URLs,
    /// class names, data-attrs) do not count as visible text.
    fn text_only(s: &str) -> String {
        let mut out = String::new();
        let mut in_tag = false;
        for c in s.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => out.push(c),
                _ => {}
            }
        }
        out
    }

    #[test]
    fn product_name_renders_lowercase_except_organisation_name() {
        let html = render_rows(&pair_test_rows());
        // What a reader sees (client JS and attribute values excluded): the
        // capitalised product name must appear only as the organisation name
        // "Empyrean Dynamics". The org GitHub URLs (github.com/Empyrean-Dynamics)
        // live in href attributes, not text nodes, so they are out of scope.
        let visible = text_only(&strip_tag_spans(&strip_tag_spans(&html, "script"), "style"));
        let mut i = 0;
        let mut org_seen = false;
        while let Some(p) = visible[i..].find("Empyrean") {
            let at = i + p;
            assert!(
                visible[at..].starts_with("Empyrean Dynamics"),
                "capitalised product name in visible text: {:?}",
                &visible[at..(at + 40).min(visible.len())]
            );
            org_seen = true;
            i = at + "Empyrean".len();
        }
        assert!(
            org_seen,
            "the organisation name Empyrean Dynamics should still render"
        );
        // The lowercase product name is the one the tool selector renders.
        assert!(
            html.contains("label: 'empyrean'"),
            "the JS tool label must be lowercase 'empyrean'"
        );
    }

    #[test]
    fn internals_sections_relocated_after_part_one_in_rail_order() {
        let html = render_rows(&pair_test_rows());
        let at = |needle: &str| {
            html.find(needle)
                .unwrap_or_else(|| panic!("missing {needle}"))
        };
        // Anchors preserved (a downstream test and deep-links depend on them).
        let s05 = at("id=\"s05\"");
        let s05c = at("id=\"s05c\"");
        let s09b = at("id=\"s09b\"");
        let s10 = at("id=\"s10\"");
        let part1 = at("id=\"part1\"");
        let part2 = at("id=\"part2\"");
        // The Part 1 block ends at the Part 2 head; all three relocated sections
        // now sit after it, none inside the Part 1 DOM block.
        for (name, off) in [("s05", s05), ("s05c", s05c), ("s09b", s09b)] {
            assert!(
                off > part2,
                "{name} must be after the Part 1 block (part2 head)"
            );
            assert!(
                !(part1 < off && off < part2),
                "{name} must not sit inside the Part 1 DOM block"
            );
        }
        // Rail order in the DOM: 3.1 s10, 3.2 s05, 3.3 s09b, then appendix s05c.
        assert!(s10 < s05, "s10 (3.1) must precede s05 (3.2)");
        assert!(s05 < s09b, "s05 (3.2) must precede s09b (3.3)");
        assert!(
            s09b < s05c,
            "s09b must precede the appendix methodology s05c"
        );
        // The appendix sections must be flex SIBLINGS of the covariance section
        // s12, never DOM descendants of it. A stray unclosed <div> inside the
        // covariance block nests s05c/s13 under s12 so their CSS `order` is
        // ignored and the appendix renders mid-Part-4 — a defect byte order
        // alone cannot see, so this depth-matches the s12 <div> span.
        let s13 = at("id=\"s13\"");
        let s12_open = html
            .find("<div class=\"section\" id=\"s12\"")
            .expect("s12 open tag");
        let hb = html.as_bytes();
        let mut depth = 0i32;
        let mut i = s12_open;
        let mut s12_close = 0usize;
        while i < hb.len() {
            if hb[i..].starts_with(b"</div>") {
                depth -= 1;
                i += 6;
                if depth == 0 {
                    s12_close = i;
                    break;
                }
            } else if hb[i..].starts_with(b"<div") {
                depth += 1;
                i += 4;
            } else {
                i += 1;
            }
        }
        assert!(
            s12_close > s12_open,
            "the s12 covariance <div> never closes"
        );
        assert!(
            !(s12_open < s05c && s05c < s12_close),
            "s05c must be a sibling of s12, not nested inside the covariance section"
        );
        assert!(
            !(s12_open < s13 && s13 < s12_close),
            "s13 must be a sibling of s12, not nested inside the covariance section"
        );
    }

    #[test]
    fn compare_bar_carries_the_only_selects_and_every_part_one_title_a_pair_chip() {
        let html = render_rows(&pair_test_rows());
        let at = |needle: &str| {
            html.find(needle)
                .unwrap_or_else(|| panic!("missing {needle}"))
        };
        let order = |id: &str| -> u32 {
            let rule = format!("#{id} {{ order: ");
            let a = html
                .find(&rule)
                .unwrap_or_else(|| panic!("no flex order rule for #{id}"));
            let rest = &html[a + rule.len()..];
            rest[..rest.find(';').unwrap()].parse().unwrap()
        };

        // Exactly one Tool select and one Reference select, both inside the bar.
        assert_eq!(
            html.matches("id=\"tool1-select\"").count(),
            1,
            "exactly one Tool select on the page"
        );
        assert_eq!(
            html.matches("id=\"tool2-select\"").count(),
            1,
            "exactly one Reference select on the page"
        );
        let bar_open = at("<div class=\"part1-bar\" id=\"part1-bar\">");
        let bar = &html[bar_open..bar_open + html[bar_open..].find("</div>").unwrap()];
        assert!(
            bar.contains("id=\"tool1-select\"") && bar.contains("id=\"tool2-select\""),
            "both selects must live inside the compare bar"
        );
        assert!(
            bar.contains(">Compare<") && bar.contains(">vs<"),
            "the only visible words in the bar are Compare and vs"
        );

        // The bar sits between the Part 1 head and the first Part 1 section, both
        // in flex order and in the DOM (rail order == visual order preserved).
        assert!(
            order("part1") < order("part1-bar") && order("part1-bar") < order("s02"),
            "the bar's flex order sits between #part1 and the first Part 1 section"
        );

        // The masthead carries neither select nor the removed comparing caption.
        let masthead = &html[at("<div class=\"masthead\">")..at("<div class=\"layout-wrap\">")];
        assert!(
            !masthead.contains("tool1-select") && !masthead.contains("tool2-select"),
            "the selects moved out of the masthead — no duplicate"
        );
        assert!(
            !masthead.contains("tool-caption") && !masthead.contains("comparing "),
            "the masthead comparing caption is gone"
        );

        // Exactly one chip per Part 1 section title, none outside Part 1; the
        // server render names the default pair with its data attributes.
        assert!(
            html.contains("<span class=\"pair-chip\" data-tool=\"empyrean\" data-ref=\"jpl\">"),
            "the chip server-renders the default pair with data-tool/data-ref"
        );
        let part1 = at("id=\"part1\"");
        let part2 = at("id=\"part2\"");
        let mut chips = 0;
        for (i, _) in html.match_indices("class=\"pair-chip\"") {
            assert!(
                part1 < i && i < part2,
                "a pair chip fell outside the Part 1 DOM block at byte {i}"
            );
            chips += 1;
        }
        assert_eq!(
            chips, 9,
            "one chip on each of the nine Part 1 section titles, none elsewhere"
        );

        // The shared sticky-header rule threads the --sticky-offset custom
        // property (defaulted for a pre-script jump), and every Part 1 section
        // clears the bar height on a jump.
        let agrid = at("table.agrid th {");
        let rule = &html[agrid..agrid + html[agrid..].find('}').unwrap()];
        assert!(
            rule.contains("top: var(--sticky-offset, 0px)"),
            "the sticky-header rule must reference --sticky-offset"
        );
        assert!(
            html.contains("--sticky-offset: 0px") && html.contains("--bar-h: 40px"),
            "both custom properties default before the script runs"
        );
        assert!(
            html.contains("scroll-margin-top: var(--bar-h)"),
            "Part 1 sections carry a scroll-margin equal to the bar height"
        );

        // The three agreement hovers keep their description but drop the pair
        // (the chip now names it).
        let s02t = at("id=\"s02-title\"");
        let s02_title_attr = &html[s02t..s02t + html[s02t..].find('>').unwrap()];
        assert!(
            s02_title_attr.contains("position agreement")
                && !s02_title_attr.contains("empyrean vs"),
            "the s02 hover keeps its description without the pair: {s02_title_attr}"
        );

        // (pass 18 delta) Finding 1: the outer bar is a full-bleed strip, not a
        // shrink-to-fit centred panel (the max-width + auto margin was the leak),
        // and the controls live in a centred inner row.
        let brule_i = at(".part1-bar {");
        let brule = &html[brule_i..brule_i + html[brule_i..].find('}').unwrap()];
        assert!(
            !brule.contains("max-width") && !brule.contains("margin: 0 auto"),
            "the compare bar must be full-bleed, not a centred shrink-to-fit panel: {brule}"
        );
        assert!(
            html.contains(".p1b-inner {") && html.contains("<div class=\"p1b-inner\">"),
            "the compare controls sit in a centred inner row"
        );
        // Finding 3: raised surface + a shadow only while pinned, toggled by the
        // scroll handler, so the bar reads as an elevated toolbar.
        assert!(
            brule.contains("--ed-surface-raised"),
            "the bar uses the raised-surface token: {brule}"
        );
        assert!(
            html.contains(".part1-bar.p1b-pinned {") && html.contains("box-shadow"),
            "a pinned-only elevation rule exists"
        );
        assert!(
            html.contains("classList.toggle('p1b-pinned', pinned)"),
            "the scroll handler toggles the pinned elevation class"
        );
        // Finding 4: the bar selects use :focus-visible (not the mouse-firing
        // :focus), and the dead #tool-selector rules are gone.
        assert!(
            html.contains(".part1-bar select:focus-visible {")
                && !html.contains(".part1-bar select:focus {"),
            "the bar select focus ring is repointed to :focus-visible"
        );
        assert!(
            !html.contains("#tool-selector"),
            "the dead #tool-selector rules are deleted"
        );
        // Finding 2: tool NAMES render from --tool-<slug> tokens, not inline hex.
        assert!(
            html.contains("<span class=\"pc-t\" style=\"color:var(--tool-empyrean)\">")
                && html.contains("<span class=\"pc-r\" style=\"color:var(--tool-jpl)\">"),
            "the default pair chip colours its tool names from tokens"
        );
        assert!(
            html.contains(
                "id=\"tool1-select\" aria-label=\"Tool\" style=\"color:var(--tool-empyrean)\""
            ) && html.contains(
                "id=\"tool2-select\" aria-label=\"Reference\" style=\"color:var(--tool-jpl)\""
            ),
            "the compare-bar selects colour their tool names from tokens"
        );
        assert!(
            !html.contains("class=\"pc-t\" style=\"color:#")
                && !html.contains("class=\"pc-r\" style=\"color:#"),
            "no chip tool name carries an inline hex colour"
        );
        // Finding 6: loadDataset re-runs the anchor scroll after boot so a deep
        // link settles onto its target once the dataset has rendered.
        assert!(
            html.contains(
                "const t = document.getElementById(location.hash.slice(1)); if (t) t.scrollIntoView"
            ),
            "loadDataset re-runs the deep-link scroll after boot"
        );
    }

    /// sRGB → linear per WCAG 2.x.
    fn srgb_to_linear(c: f64) -> f64 {
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    /// WCAG relative luminance of a `#rrggbb` colour.
    fn rel_luminance(hex: &str) -> f64 {
        let h = hex.trim_start_matches('#');
        let ch = |a, b| u8::from_str_radix(&h[a..b], 16).unwrap() as f64 / 255.0;
        0.2126 * srgb_to_linear(ch(0, 2))
            + 0.7152 * srgb_to_linear(ch(2, 4))
            + 0.0722 * srgb_to_linear(ch(4, 6))
    }

    /// WCAG contrast ratio between two `#rrggbb` colours.
    fn contrast_ratio(a: &str, b: &str) -> f64 {
        let (la, lb) = (rel_luminance(a), rel_luminance(b));
        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// (pass 18 delta) Finding 2: every tool name rendered as text — the compare
    /// bar selects and the section-title pair chips — must clear WCAG AA (4.5:1)
    /// on its surface in BOTH themes. The tokens are the one source; this checks
    /// their measured contrast so a faded colour cannot ship.
    #[test]
    fn tool_text_colours_clear_wcag_aa_in_both_themes() {
        let html = render_rows(&pair_test_rows());
        // Read a `#rrggbb` immediately following `needle`, searching from `from`.
        let hex_after = |from: usize, needle: &str| -> String {
            let i = html[from..]
                .find(needle)
                .map(|x| from + x + needle.len())
                .unwrap_or_else(|| panic!("missing {needle}"));
            assert_eq!(&html[i..i + 1], "#", "expected a hex colour after {needle}");
            html[i..i + 7].to_string()
        };
        // Surfaces the tool text sits on. --ed-surface (the chip / card) is the
        // tighter of chip-vs-input in each theme, so it governs. Dark from the
        // vendored tokens :root (first occurrence); light from the theme-light
        // block, anchored by the light --ed-bg so we read the right one.
        let dark_surface = hex_after(0, "--ed-surface: ");
        let light_anchor = html.find("--ed-bg: #f6f8fb").expect("light theme block");
        let light_surface = hex_after(light_anchor, "--ed-surface: ");
        assert_eq!(dark_surface, "#151b23");
        assert_eq!(light_surface, "#edf1f6");

        let dark_block = html
            .find(":root { --tool-empyrean")
            .expect("dark tool tokens");
        let light_block = html
            .find(":root.theme-light { --tool-empyrean")
            .expect("light tool tokens");
        for slug in ["empyrean", "jpl", "assist", "findorb"] {
            let needle = format!("--tool-{slug}: ");
            let d = hex_after(dark_block, &needle);
            let l = hex_after(light_block, &needle);
            let dr = contrast_ratio(&d, &dark_surface);
            let lr = contrast_ratio(&l, &light_surface);
            assert!(
                dr >= 4.5,
                "{slug} dark {d} on {dark_surface} = {dr:.2}:1 (< 4.5)"
            );
            assert!(
                lr >= 4.5,
                "{slug} light {l} on {light_surface} = {lr:.2}:1 (< 4.5)"
            );
        }
    }

    #[test]
    fn section_one_pair_panels_name_only_their_own_pair() {
        let html = render_rows(&pair_test_rows());
        // Default panel: empyrean vs JPL — no third tool, no model-gap caveat.
        let ej = panel_slice(&html, "h1-prop", "empyrean", "jpl").to_lowercase();
        assert!(
            !ej.contains("assist"),
            "empyrean/jpl panel must not name ASSIST"
        );
        assert!(
            !ej.contains("find_orb"),
            "empyrean/jpl panel must not name find_orb"
        );
        // A panel whose pair includes ASSIST may name ASSIST, never find_orb.
        let ea = panel_slice(&html, "h1-prop", "empyrean", "assist");
        assert!(
            ea.contains("ASSIST"),
            "empyrean/assist panel should name its own tool"
        );
        assert!(
            !ea.contains("find_orb"),
            "empyrean/assist panel must not name find_orb"
        );
        // The static Section-1 disclosure body is pair-neutral (names no tool).
        let s02 = html.find("id=\"s02\"").expect("s02");
        let s03 = html[s02..].find("id=\"s03\"").map(|e| s02 + e).unwrap();
        let sec = &html[s02..s03];
        let db = sec.find("class=\"disc-body\">").expect("s02 disclosure");
        let disc = &sec[db..sec[db..].find("</div>").map(|e| db + e).unwrap()];
        assert!(
            disc.contains("between the selected tool and the reference"),
            "s02 disclosure should be rewritten pair-neutral"
        );
        assert!(
            !disc.contains("ASSIST"),
            "s02 disclosure must not name ASSIST"
        );
        assert!(
            !disc.contains("find_orb"),
            "s02 disclosure must not name find_orb"
        );
    }

    #[test]
    fn model_gap_caveat_shows_only_in_assist_panels() {
        let html = render_rows(&pair_test_rows());
        // ASSIST-bearing panels carry the pair-scoped model-gap caveat…
        assert!(
            panel_slice(&html, "h1-prop", "empyrean", "assist").contains("model gap"),
            "the empyrean/assist panel must carry the model-gap caveat"
        );
        assert!(
            panel_slice(&html, "h1-prop", "assist", "jpl").contains("model gap"),
            "the assist/jpl panel must carry the model-gap caveat"
        );
        // …and the empyrean/jpl panel never does.
        assert!(
            !panel_slice(&html, "h1-prop", "empyrean", "jpl").contains("model gap"),
            "the empyrean/jpl panel must not carry the model-gap caveat"
        );
    }

    #[test]
    fn section_one_growth_fallback_is_pair_neutral() {
        let html = render_rows(&pair_test_rows());
        // The shared error-growth drawer's illegal-pair fallback (the empty-data
        // Plotly.react branch) must not hardcode a tool outside the selected
        // pair: ruling 1 forbids any Section-1 element (chart, annotation, hover)
        // from naming a tool outside the pair. The only tool names it shows come
        // from toolLabel(TOOL1/TOOL2), never a literal reference name.
        let at = html
            .find("Plotly.react(elId, []")
            .expect("shared growth-chart fallback branch");
        let end = html[at..]
            .find("return;")
            .map(|e| at + e)
            .expect("fallback end");
        let fallback = &html[at..end];
        for tool in ["JPL", "Horizons", "ASSIST", "find_orb"] {
            assert!(
                !fallback.contains(tool),
                "growth fallback must not hardcode {tool}: {fallback}"
            );
        }
        assert!(
            fallback.contains("toolLabel(TOOL1)") && fallback.contains("toolLabel(TOOL2)"),
            "growth fallback must label the pair through toolLabel, not literals"
        );
        // The message noun is a per-axis parameter, assembled at draw time; the
        // propagation config supplies the propagation-position axis.
        assert!(
            fallback.contains("${opts.noun} comparison stored for this pair"),
            "fallback message must be assembled from the per-axis noun"
        );
        assert!(
            html.contains("noun: 'propagation-position'"),
            "the propagation growth config must pass the propagation-position noun"
        );
    }

    #[test]
    fn ephemeris_error_growth_is_its_own_section_after_the_heatmap() {
        // A1: the ephemeris error-growth chart is a first-class section (s06b),
        // the sibling of the propagation one (s03): its own anchor, the
        // eph-sep-chart moved into it, a rail entry, and a flex-order slot
        // directly under the 1.2 heatmap and before the ephemeris cost.
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&timing_fixture(), &[], &out, None).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();

        // The eph-sep-chart lives inside the new s06b section, not the s06
        // heatmap section it used to be buried in.
        let s06b = html
            .find("id=\"s06b\"")
            .expect("ephemeris growth section s06b");
        let eph_chart = html.find("id=\"eph-sep-chart\"").expect("eph-sep-chart");
        assert!(eph_chart > s06b, "the eph-sep-chart must sit inside s06b");

        // A rail entry names it (the scroll-spy tracks any rail a[data-target]).
        let rail = html.find("data-target=\"s06b\"").expect("s06b rail entry");
        let entry = &html[rail..rail + html[rail..].find("</a>").unwrap()];
        assert!(entry.contains("error growth"), "s06b rail entry: {entry}");

        // The pair/channels toggle moved into s06b, so its id names that section;
        // the stale s06-toggle carryover from the old section must be gone.
        assert!(
            html.contains("id=\"s06b-toggle\""),
            "the toggle id must name its section (s06b)"
        );
        assert!(
            !html.contains("s06-toggle"),
            "the stale s06-toggle id must be gone"
        );

        // Flex order: heatmap (s06) < error growth (s06b) < cost (s-t2-eph) <
        // outliers (s07); the extra outliers table comes last on the axis.
        let order = |id: &str| -> u32 {
            let rule = format!("#{id} {{ order: ");
            let at = html
                .find(&rule)
                .unwrap_or_else(|| panic!("no order rule for #{id}"));
            let rest = &html[at + rule.len()..];
            rest[..rest.find(';').unwrap()].parse().unwrap()
        };
        assert!(
            order("s06") < order("s06b"),
            "eph growth must follow the heatmap"
        );
        assert!(
            order("s06b") < order("s-t2-eph"),
            "eph growth must precede its cost"
        );
        assert!(
            order("s-t2-eph") < order("s07"),
            "the outliers table comes last on the axis"
        );
    }

    #[test]
    fn part_one_section_letters_are_sequential_per_axis() {
        // A1: within each Part-1 axis the section letters run A, B, C, … with no
        // gaps and the same role at the same letter (heatmap A, error growth B,
        // cost C), except OD, which has no growth chart, so its cost follows the
        // grid directly at B.
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&timing_fixture(), &[], &out, None).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        let num = |sid: &str| -> String {
            let at = html
                .find(&format!("id=\"{sid}\""))
                .unwrap_or_else(|| panic!("no section {sid}"));
            let tag = "section-num\">";
            let n = html[at..].find(tag).map(|e| at + e + tag.len()).unwrap();
            let e = html[n..].find("</div>").map(|e| n + e).unwrap();
            html[n..e].to_string()
        };
        // Propagation axis: A heatmap, B growth, C cost.
        assert_eq!(num("s02"), "1.1A");
        assert_eq!(num("s03"), "1.1B");
        assert_eq!(num("s-t1-prop"), "1.1C");
        // Ephemeris axis: A heatmap, B growth, C cost, D outliers (extra last).
        assert_eq!(num("s06"), "1.2A");
        assert_eq!(num("s06b"), "1.2B");
        assert_eq!(num("s-t2-eph"), "1.2C");
        assert_eq!(num("s07"), "1.2D");
        // OD axis: A grid, B cost (no growth chart between them).
        assert_eq!(num("s08b"), "1.3A");
        assert_eq!(num("s-t3-od"), "1.3B");
    }

    #[test]
    fn both_growth_charts_draw_through_one_shared_function() {
        // A2: exactly one growth-chart drawing function, and both containers are
        // drawn through it, so the two charts cannot drift apart again. The old
        // per-chart drawers and the divergent gMed estimator are gone.
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&timing_fixture(), &[], &out, None).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        assert_eq!(
            html.matches("function drawGrowthChart(").count(),
            1,
            "there must be exactly one shared growth-chart drawing function"
        );
        assert!(
            !html.contains("function buildGrowth("),
            "old buildGrowth must be gone"
        );
        assert!(
            !html.contains("function buildEphSep("),
            "old buildEphSep must be gone"
        );
        assert!(
            !html.contains("function gMed("),
            "the divergent gMed estimator must be gone"
        );
        // Both containers are drawn through the shared function.
        assert!(
            html.contains("drawGrowthChart('growth-chart'"),
            "the propagation chart must draw through the shared function"
        );
        assert!(
            html.contains("drawGrowthChart('eph-sep-chart'"),
            "the ephemeris chart must draw through the shared function"
        );
        // The shared marker size and legend placement are single definitions.
        assert_eq!(
            html.matches("const MARKER = 5;").count(),
            1,
            "one marker-size const for the growth family"
        );
        assert!(
            html.contains("const LEGEND_H ="),
            "one legend-placement const for the growth family"
        );
    }

    #[test]
    fn both_growth_charts_share_one_card_wrapper_and_height() {
        // The two error-growth charts each sit in a .chart-container card (the
        // growth chart is no longer a bare div) and share one inline height (the
        // ephemeris canonical, 520px). The card class is defined exactly once.
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&timing_fixture(), &[], &out, None).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        assert_eq!(
            html.matches(".chart-container {").count(),
            1,
            "the chart-container class must be defined exactly once"
        );
        for id in ["growth-chart", "eph-sep-chart"] {
            let at = html.find(&format!("id=\"{id}\"")).unwrap();
            let before = &html[at.saturating_sub(120)..at];
            assert!(
                before.contains("chart-container"),
                "#{id} must be wrapped in a chart-container card"
            );
            let seg = &html[at..at + 60];
            assert!(
                seg.contains("height:520px"),
                "#{id} must use the 520px growth height: {seg}"
            );
        }
    }

    #[test]
    fn covariance_panel_titles_are_legible_and_theme_aware() {
        // The three covariance panels (#s12) carry in-chart Plotly titles. They
        // used to hardcode a ghost-grey (#cdd3da ≈ 1.5:1 on the light theme) that
        // vanished into the background and smeared under a top legend. The titles
        // must instead route through one shared helper coloured from a theme
        // token, re-theme on a flip, and keep the legend clear of the title band.
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&timing_fixture(), &[], &out, None).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();

        // The invisible ghost-grey literal is gone everywhere.
        assert!(
            !html.contains("#cdd3da"),
            "the ghost-grey panel-title colour must be gone"
        );
        // One shared title helper, drawn by all three panels (no per-panel literal).
        assert_eq!(
            html.matches("function covTitle(").count(),
            1,
            "one shared covariance-panel title helper"
        );
        assert_eq!(
            html.matches("title: covTitle(").count(),
            3,
            "all three covariance panels draw their title through covTitle"
        );
        // The helper colours the title from a theme token, so it is legible on the
        // light theme rather than a hardcoded grey.
        let at = html.find("function covTitle(").unwrap();
        let body = &html[at..at + 160];
        assert!(
            body.contains("themeVar('--ed-text-secondary')"),
            "covTitle must colour from the theme token, not a literal: {body}"
        );
        // rethemeCharts re-colours the title on a theme flip.
        assert!(
            html.contains("'title.font.color': themeVar('--ed-text-secondary')"),
            "rethemeCharts must re-colour panel titles on a theme flip"
        );
        // The legend is placed below the plot (one shared definition) so it cannot
        // overprint the top title band of the two legended panels.
        assert_eq!(
            html.matches("const COV_LEGEND =").count(),
            1,
            "one shared covariance-legend placement"
        );
        assert_eq!(
            html.matches("legend: COV_LEGEND").count(),
            2,
            "the two legended covariance panels share the bottom-placed legend"
        );
    }

    // ── Pass-21 text diet: marks carry the message in s10 and s09b ──────────

    /// Build a channel-fidelity grid where the per-object grid lists one object
    /// with a rust EXACT cell (= core) and a python DIFF cell (0.1 AU off core),
    /// plus the eph/OD cells that no channel ran (hatched). Exercises =, ≠, hatch.
    fn fidelity_fixture() -> Vec<ValidationResult> {
        let mut core = synthetic_rust_prop_row("Apophis", 0.0);
        core.channel = "core".to_string();
        core.emp_pos_au = Some([1.0, 0.0, 0.0]);
        let rust = synthetic_rust_prop_row("Apophis", 0.0); // identical → EXACT
        let mut py = synthetic_rust_prop_row("Apophis", 0.0);
        py.channel = "python".to_string();
        py.emp_pos_au = Some([1.1, 0.0, 0.0]); // 0.1 AU off core → DIFF
        vec![core, rust, py]
    }

    /// The per-object grid slice: everything after the "Complete per-object grid"
    /// panel title (that grid is the last thing the builder emits).
    fn per_object_grid(grid: &str) -> &str {
        let marker = "Complete per-object grid</div>";
        &grid[grid.find(marker).expect("per-object grid present") + marker.len()..]
    }

    #[test]
    fn channel_fidelity_per_object_cells_are_one_glyph_with_the_state_word_on_hover() {
        let grid = build_channel_fidelity_grid(&fidelity_fixture());
        let per = per_object_grid(&grid);
        let glyphs = ["=", "≈", "~", "≠"];
        let words = ["EXACT", "ULP", "TOL", "DIFF"];
        // Able to fail: a bare state word in a per-object cell is the old form.
        for w in words {
            assert!(
                !per.contains(&format!(">{w}</td>")),
                "per-object cell still prints the bare state word {w}"
            );
        }
        // The EXACT cell is the glyph "=", the DIFF cell is "≠".
        assert!(
            per.contains("\">=</td>") && per.contains("\">≠</td>"),
            "the per-object EXACT (=) and DIFF (≠) glyph cells must render: {per}"
        );
        // Every per-object cell shows exactly one glyph (or is the hatched state),
        // and carries its state word in the title.
        let mut saw_cell = false;
        for piece in per.split("<td class=\"fo ").skip(1) {
            saw_cell = true;
            let gt = piece.find('>').expect("cell open tag closes");
            let attrs = &piece[..gt];
            let rest = &piece[gt + 1..];
            let content = &rest[..rest.find("</td>").expect("cell closes")];
            assert!(
                attrs.contains("title=\""),
                "every fo cell carries a title: {attrs}"
            );
            if attrs.starts_with("hatch") {
                assert!(content.is_empty(), "the hatched cell is empty: {content:?}");
                assert!(
                    attrs.contains("not run"),
                    "hatched cell hover names the state: {attrs}"
                );
            } else {
                assert!(
                    glyphs.contains(&content),
                    "a per-object cell must be exactly one glyph, got {content:?}"
                );
                assert!(
                    words.iter().any(|w| attrs.contains(w)),
                    "a glyph cell must carry its state word in the title: {attrs}"
                );
            }
        }
        assert!(saw_cell, "the per-object grid rendered at least one cell");
    }

    #[test]
    fn channel_fidelity_key_names_all_four_glyph_word_pairs() {
        let grid = build_channel_fidelity_grid(&fidelity_fixture());
        // The key is above the per-object grid; check the whole grid string.
        // glyph and word are joined into one token so the key stays within H5's
        // 8-visible-word grid-key cap (the glyph counts as a word on its own).
        for (glyph, word) in [("=", "EXACT"), ("≈", "ULP"), ("~", "TOL"), ("≠", "DIFF")] {
            assert!(
                grid.contains(&format!("></span>{glyph}{word}</span>")),
                "key missing the {glyph}{word} pair"
            );
        }
        assert!(
            grid.contains("></span>not run</span>"),
            "key keeps the hatched not-run"
        );
        // Able to fail: the four-state table drives both cells and key, so a cell
        // glyph that is not in the key would be a drift the FID_MARKS source bans.
        assert_eq!(
            FID_MARKS.len(),
            4,
            "the glyph table defines the four states once"
        );
    }

    #[test]
    fn nongrav_verdict_and_coeff_cells_use_marks_not_words() {
        let html = render_synthetic_html();
        // Coeff definitions stated exactly once, on the column-header hover.
        assert_eq!(
            html.matches("A1 radial · A2 transverse · A3 normal")
                .count(),
            1,
            "the A1/A2/A3 definitions must appear once, on the Coeff header hover"
        );
        assert!(
            html.contains("<th title=\"A1 radial · A2 transverse · A3 normal\">Coeff</th>"),
            "Coeff header must carry the definitions on its hover"
        );
        // Per-row coeff cells read only the label; the sub-labels are gone.
        assert!(
            html.contains("${c.label}</td>"),
            "coeff cell renders only the label"
        );
        for sub in ["sub: 'radial'", "sub: 'transverse'", "sub: 'normal'"] {
            assert!(
                !html.contains(sub),
                "the per-row coeff sub-label {sub} must be gone"
            );
        }
        // Verdict reuses the OD grid's glyph vocabulary; PASS/FAIL words are gone.
        assert!(
            html.contains("pass ? '·' : '✕'"),
            "verdict must render · (pass) / ✕ (fail), the OD grid glyphs"
        );
        for word in ["'PASS'", "'FAIL'", ">FAIL</td>", ">PASS</td>"] {
            assert!(
                !html.contains(word),
                "a verdict word {word} still renders as a cell"
            );
        }
        // The Result header carries the rule on its hover.
        assert!(
            html.contains("Result</th>") && html.contains("✕ fail"),
            "Result header must carry the |z| rule on its hover"
        );
    }

    #[test]
    fn appendix_titles_mirror_the_rail_with_descriptors_on_hover() {
        let html = render_synthetic_html();
        // Visible titles equal the rail labels; the O8 target name "ASSIST
        // configurations" is the one shared appendix name for s05c (title and rail).
        for (title, hover) in [
            ("Run summary", "What this run holds"),
            ("External tools", "External reference tools — transparency"),
            ("Provenance", "Reproducibility — provenance for this run"),
            ("ASSIST configurations", "ASSIST configurations — six arms"),
        ] {
            assert!(
                html.contains(&format!(">{title}</div>")),
                "appendix visible title must be '{title}'"
            );
            assert!(
                html.contains(&format!("title=\"{hover}")),
                "the '{title}' descriptor must move to its title hover"
            );
        }
        // Rail labels for every appendix section match their titles, s05c included.
        for label in [
            "Run summary",
            "External tools",
            "Provenance",
            "ASSIST configurations",
        ] {
            assert!(
                html.contains(&format!(">{label}</a>")),
                "rail label '{label}' present and equal to the title"
            );
        }
        // Able to fail: the old sentence-titles, the deleted s13 caption, and the
        // old prefixed s05c rail label that diverged from the section title.
        for gone in [
            ">What this run holds</div>",
            "External reference tools &mdash; transparency",
            "Reproducibility &mdash; Provenance",
            "&mdash; six arms",
            "Provenance — frame, ephemeris, force model, references",
            "Methodology &middot; ASSIST arms",
        ] {
            assert!(
                !html.contains(gone),
                "old appendix text still present: {gone}"
            );
        }
    }

    #[test]
    fn part_two_hovers_drop_the_ratio_clause() {
        let html = render_synthetic_html();
        // The by-population and by-object matrices render in Part 2.
        assert!(
            html.contains("class=\"agrid r2grid\"") && html.contains("class=\"agrid r3grid\""),
            "the Part 2 by-population and by-object grids must render"
        );
        // Able to fail: the ratio clause instructs a Part-2-forbidden operation.
        assert!(
            !html.contains("dividing"),
            "a Part-2 hover still instructs a ratio"
        );
        assert!(
            !html.contains("gives their factor"),
            "a Part-2 hover still names a factor"
        );
        // The rest of each hover stays.
        assert!(html.contains("one decade ladder, smaller closer; dagger = a population"));
        assert!(
            html.contains("smaller closer; one decade ladder; hatched where the tool has no rows")
        );
    }
}
