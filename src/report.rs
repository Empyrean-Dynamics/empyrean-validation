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
use crate::schema::ValidationResult;

/// Numerical-fidelity threshold: any metric whose absolute difference
/// against the Rust reference exceeds this is flagged as a failure.
/// Applied directly in whatever unit each metric is in.
pub const FIDELITY_THRESHOLD: f64 = 1e-10;

/// One AU in km, used to convert vector position differences (which are
/// stored in AU) to km for display.
const AU_KM: f64 = 149_597_870.700;

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

/// Format an arcsec angular value with an auto-picked unit. Mirrors
/// `fmt_error` for the angular axis: nano-arcsec at the float64-ULP
/// floor, micro-arcsec for sub-mas drift, milli-arcsec for typical
/// ground-based residuals, arcsec for anything larger.
fn fmt_arcsec(v: f64) -> String {
    if v < 1e-6 {
        format!("{:.1} nas", v * 1e9)
    } else if v < 1e-3 {
        format!("{:.1} μas", v * 1e6)
    } else if v < 1.0 {
        format!("{:.1} mas", v * 1000.0)
    } else if v < 60.0 {
        format!("{:.2}\"", v)
    } else {
        format!("{:.1}\" (≈{:.1}′)", v, v / 60.0)
    }
}

/// Format a per-row "worst-metric diff" in the native unit of the
/// dominant metric for a given test_type. Propagation + OD rows carry
/// position-vector diffs in km; ephemeris rows carry the maximum of
/// {separation, ΔRA, ΔDec} in arcsec (with Δρ in km and Δlt in s also
/// possible — see `rollup_channels` for full enumeration). This is
/// used by the §10 per-test-type matrix so that ephemeris doesn't
/// render with bogus "0.0 mm" entries from a position-vector field
/// that was never populated for those rows.
fn fmt_row_max_diff(test_type: &str, v: f64) -> String {
    match test_type {
        "ephemeris" => fmt_arcsec(v),
        _ => fmt_error(Some(v)),
    }
}

/// Threshold in the row's native unit below which we consider the
/// channel "essentially bit-identical" for color-coding (kept distinct
/// from the strict `FIDELITY_THRESHOLD = 1e-10`, which is the actual
/// pass criterion).
fn approx_bit_identical_threshold(test_type: &str) -> f64 {
    match test_type {
        "ephemeris" => 1e-3, // 1 mas — sub-Gaia float64-ULP-ish for angles
        _ => 1e-3,           // 1 mm in km
    }
}

/// Format a numerical diff value in a friendly units string.
fn fmt_diff(v: f64) -> String {
    if v == 0.0 {
        "0".to_string()
    } else if v.abs() < 1e-3 {
        format!("{:.2e}", v)
    } else if v.abs() < 1.0 {
        format!("{:.4}", v)
    } else {
        format!("{:.2}", v)
    }
}

/// Format a duration in milliseconds.
fn fmt_ms(ms: f64) -> String {
    if ms < 0.001 {
        format!("{:.1} µs", ms * 1000.0)
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
    offenders: Vec<Offender>,
}

#[derive(Debug, Clone, Default)]
struct TestTypeRollup {
    n_compared: usize,
    n_bit_identical: usize,
    p50_dr_km: f64,
    p95_dr_km: f64,
    p99_dr_km: f64,
    max_dr_km: f64,
}

/// One row that exceeded the FIDELITY_THRESHOLD on at least one metric.
#[derive(Debug, Clone)]
struct Offender {
    object: String,
    dt_days: f64,
    test_type: String,
    observer: Option<String>,
    worst_metric: String,
    worst_value: f64,
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
fn rollup_channels(results: &[ValidationResult]) -> Vec<ChannelRollup> {
    let mut by_channel: BTreeMap<String, Vec<&ValidationResult>> = BTreeMap::new();
    for r in results {
        by_channel.entry(r.channel.clone()).or_default().push(r);
    }
    let Some(core_rows) = by_channel.get("core") else {
        return Vec::new();
    };
    // Key includes `propagation_uncertainty` so that a row produced under
    // the Jet1 STM path (`first_order_with_cov`) is matched against the
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
        let mut offenders: Vec<Offender> = Vec::new();
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

        for r in rows {
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
            let mut row_worst_metric = "";
            // Separate flag from `row_worst_metric` so that rows where
            // every available metric diff is exactly 0.0 (the
            // bit-identical case for replay channels) are still
            // counted. Previously, when every diff was 0 we never
            // entered the `if d > row_max_diff` branch, so
            // `row_worst_metric` stayed empty and the per-test-type
            // counters silently dropped the row — making the §10
            // matrix display N/0 for c/cli/python instead of N/N.
            let mut any_metric_seen = false;

            // Vector position diff (the headline cross-channel agreement
            // metric — magnitude in km of channel.emp_pos - rust.emp_pos).
            if let Some(dr_km) = vec_dr_km(&r.emp_pos_au, &core_r.emp_pos_au) {
                any_metric_seen = true;
                max_dr_km = max_dr_km.max(dr_km);
                if dr_km > row_max_diff {
                    row_max_diff = dr_km;
                    row_worst_metric = "‖Δr‖ km";
                }
            }
            if let (Some(a), Some(b)) = (r.separation_arcsec, core_r.separation_arcsec) {
                any_metric_seen = true;
                let d = (a - b).abs();
                max_sep_diff = max_sep_diff.max(d);
                if d > row_max_diff {
                    row_max_diff = d;
                    row_worst_metric = "Δsep asec";
                }
            }
            if let (Some(a), Some(b)) = (r.d_ra_arcsec, core_r.d_ra_arcsec) {
                any_metric_seen = true;
                let d = (a - b).abs();
                max_d_ra = max_d_ra.max(d);
                if d > row_max_diff {
                    row_max_diff = d;
                    row_worst_metric = "ΔRA asec";
                }
            }
            if let (Some(a), Some(b)) = (r.d_dec_arcsec, core_r.d_dec_arcsec) {
                any_metric_seen = true;
                let d = (a - b).abs();
                max_d_dec = max_d_dec.max(d);
                if d > row_max_diff {
                    row_max_diff = d;
                    row_worst_metric = "ΔDec asec";
                }
            }
            if let (Some(a), Some(b)) = (r.d_rho_km, core_r.d_rho_km) {
                any_metric_seen = true;
                let d = (a - b).abs();
                max_d_rho = max_d_rho.max(d);
                if d > row_max_diff {
                    row_max_diff = d;
                    row_worst_metric = "Δρ km";
                }
            }
            if let (Some(a), Some(b)) = (r.d_light_time_s, core_r.d_light_time_s) {
                any_metric_seen = true;
                let d = (a - b).abs();
                max_d_lt = max_d_lt.max(d);
                if d > row_max_diff {
                    row_max_diff = d;
                    row_worst_metric = "Δlt s";
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
                } else {
                    // Synthesise a worst_metric label for the offender
                    // entry even when row_max_diff was 0 across the
                    // board (shouldn't reach here in that case, but
                    // belt-and-braces).
                    offenders.push(Offender {
                        object: r.object.clone(),
                        dt_days: r.dt_days,
                        test_type: r.test_type.clone(),
                        observer: r.observer.clone(),
                        worst_metric: if row_worst_metric.is_empty() {
                            "row".to_string()
                        } else {
                            row_worst_metric.to_string()
                        },
                        worst_value: row_max_diff,
                    });
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
        for tt in ["propagation", "ephemeris", "orbit_determination"] {
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
            offenders: {
                let mut o = offenders;
                o.sort_by(|a, b| {
                    b.worst_value
                        .partial_cmp(&a.worst_value)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                o
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

fn build_offenders_html(rollups: &[ChannelRollup]) -> String {
    let mut html = String::new();
    for r in rollups
        .iter()
        .filter(|r| r.channel != "core" && !r.offenders.is_empty())
    {
        // Group offenders by test_type for the by-axis summary.
        let mut by_tt: BTreeMap<&str, usize> = BTreeMap::new();
        for o in &r.offenders {
            *by_tt.entry(o.test_type.as_str()).or_default() += 1;
        }
        let summary: Vec<String> = by_tt.iter().map(|(tt, n)| format!("{n} {tt}")).collect();
        html.push_str(&format!(
            "<div class=\"section-desc\" style=\"color:{}; margin-top:18px; margin-bottom:8px;\"><span class=\"pop-dot\" style=\"background:{}\"></span>{} — {} offending row{} ({}; top 25 shown)</div>",
            channel_color(&r.channel),
            channel_color(&r.channel),
            r.channel,
            r.offenders.len(),
            if r.offenders.len() == 1 { "" } else { "s" },
            summary.join(", "),
        ));
        html.push_str(r#"<div class="heatmap-container"><table class="heatmap" style="min-width:100%"><thead><tr>"#);
        html.push_str(r#"<th style="text-align:left">Object</th><th>dt</th><th>Test</th><th>Observer</th><th>Worst metric</th><th>Δ value</th></tr></thead><tbody>"#);
        for o in r.offenders.iter().take(25) {
            let dt_label = if o.dt_days.abs() >= 365.0 {
                format!("{:+.1}y", o.dt_days / 365.0)
            } else {
                format!("{:+.0}d", o.dt_days)
            };
            html.push_str(&format!(
                r#"<tr><td class="obj-name">{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td style="color:#e06252">{}</td></tr>"#,
                o.object,
                dt_label,
                o.test_type,
                o.observer.as_deref().unwrap_or("—"),
                o.worst_metric,
                fmt_diff(o.worst_value),
            ));
        }
        html.push_str("</tbody></table></div>");
    }
    html
}

/// Top channel-fidelity rollup table — one row per channel, columns are
/// the headline metrics used to gate CI.
fn build_channel_table_html(rollups: &[ChannelRollup]) -> String {
    let mut html = String::new();
    html.push_str(
        r#"<div class="heatmap-container"><table class="heatmap" style="min-width:100%">"#,
    );
    html.push_str("<thead><tr>");
    html.push_str(r#"<th style="text-align:left">Channel</th>"#);
    html.push_str("<th>Rows</th>");
    html.push_str("<th>Compared</th>");
    html.push_str("<th>Pass @ 1e-10</th>");
    html.push_str("<th>Max ‖Δr‖ km</th>");
    html.push_str("<th>Max Δsep (asec)</th>");
    html.push_str("<th>Max ΔRA (asec)</th>");
    html.push_str("<th>Max ΔDec (asec)</th>");
    html.push_str("<th>Max Δρ (km)</th>");
    html.push_str("<th>Max Δlt (s)</th>");
    html.push_str("<th>p50 t</th>");
    html.push_str("<th>p50 t (core paired)</th>");
    html.push_str("<th>speed (ch/core)</th>");
    html.push_str("</tr></thead><tbody>");

    for r in rollups {
        let is_ref = r.channel == "core";
        let color = channel_color(&r.channel);
        // For the reference channel, all "diff vs rust" cells are
        // tautologically zero / undefined. Render "(ref)" or italic
        // greyed text rather than an em-dash trail so the row reads
        // as "this is the reference, comparison is N/A by definition"
        // instead of "data missing" (P2-B from review-pass2).
        let ref_cell = "<i style=\"color:#5b9bd5; opacity:0.7\">(ref)</i>".to_string();
        let pass_text = if is_ref {
            ref_cell.clone()
        } else if r.n_total_compared == 0 {
            "0/0".to_string()
        } else {
            format!("{}/{}", r.n_passing, r.n_total_compared)
        };
        let pass_color = if is_ref || r.n_total_compared == 0 {
            "#8b9198"
        } else if r.n_passing == r.n_total_compared {
            "#3d9a6d"
        } else {
            "#e06252"
        };
        let metric = |v: f64| -> String {
            if is_ref {
                ref_cell.clone()
            } else if r.n_total_compared == 0 {
                "—".to_string()
            } else {
                fmt_diff(v)
            }
        };
        let metric_color = |v: f64| -> &'static str {
            if is_ref || r.n_total_compared == 0 {
                "#8b9198"
            } else if v <= FIDELITY_THRESHOLD {
                "#3d9a6d"
            } else {
                "#e06252"
            }
        };
        let speed_text = if is_ref {
            "1.00x".to_string()
        } else if r.p50_speed_ratio.is_nan() {
            "—".to_string()
        } else {
            format!("{:.2}x", r.p50_speed_ratio)
        };
        let time_text = if r.p50_time_ms.is_nan() {
            "—".to_string()
        } else {
            fmt_ms(r.p50_time_ms)
        };
        let rust_paired_text = if is_ref {
            ref_cell.clone()
        } else if r.p50_rust_time_ms.is_nan() {
            "—".to_string()
        } else {
            fmt_ms(r.p50_rust_time_ms)
        };

        html.push_str("<tr>");
        html.push_str(&format!(
            r#"<td class="obj-name"><span class="pop-dot" style="background:{color}"></span>{ch}</td>"#,
            ch = r.channel
        ));
        html.push_str(&format!("<td>{}</td>", r.n_rows));
        html.push_str(&format!(
            "<td>{}</td>",
            if is_ref {
                "—".to_string()
            } else {
                r.n_total_compared.to_string()
            }
        ));
        html.push_str(&format!(
            r#"<td style="color:{pass_color}">{pass_text}</td>"#
        ));
        html.push_str(&format!(
            r#"<td style="color:{}">{}</td>"#,
            metric_color(r.max_dr_km),
            metric(r.max_dr_km)
        ));
        html.push_str(&format!(
            r#"<td style="color:{}">{}</td>"#,
            metric_color(r.max_sep_diff_arcsec),
            metric(r.max_sep_diff_arcsec)
        ));
        html.push_str(&format!(
            r#"<td style="color:{}">{}</td>"#,
            metric_color(r.max_d_ra_diff_arcsec),
            metric(r.max_d_ra_diff_arcsec)
        ));
        html.push_str(&format!(
            r#"<td style="color:{}">{}</td>"#,
            metric_color(r.max_d_dec_diff_arcsec),
            metric(r.max_d_dec_diff_arcsec)
        ));
        html.push_str(&format!(
            r#"<td style="color:{}">{}</td>"#,
            metric_color(r.max_d_rho_diff_km),
            metric(r.max_d_rho_diff_km)
        ));
        html.push_str(&format!(
            r#"<td style="color:{}">{}</td>"#,
            metric_color(r.max_d_lt_diff_s),
            metric(r.max_d_lt_diff_s)
        ));
        html.push_str(&format!("<td>{time_text}</td>"));
        html.push_str(&format!("<td>{rust_paired_text}</td>"));
        html.push_str(&format!("<td>{speed_text}</td>"));
        html.push_str("</tr>");
    }
    html.push_str("</tbody></table></div>");
    html
}

/// Per-test-type × per-channel matrix: replaces the single "X/Y rows
/// pass" headline. Rows are test types, columns are channels.
fn build_per_test_type_matrix_html(rollups: &[ChannelRollup]) -> String {
    let mut html = String::new();
    html.push_str(r#"<div class="heatmap-container" style="margin-top:20px;"><table class="heatmap" style="min-width:100%">"#);
    html.push_str("<thead><tr>");
    html.push_str(r#"<th style="text-align:left">Test type</th>"#);
    for r in rollups {
        let color = channel_color(&r.channel);
        html.push_str(&format!(
            r#"<th><span class="pop-dot" style="background:{color}"></span>{ch}</th>"#,
            ch = r.channel
        ));
    }
    html.push_str("</tr></thead><tbody>");

    for tt in ["propagation", "ephemeris", "orbit_determination"] {
        html.push_str(&format!(r#"<tr><td class="obj-name">{tt}</td>"#));
        for r in rollups {
            let is_ref = r.channel == "core";
            let row = r.by_test_type.get(tt);
            if is_ref {
                let n = row.map(|x| x.n_compared).unwrap_or(0);
                html.push_str(&format!(r#"<td style="color:#5b9bd5">{n} (ref)</td>"#));
                continue;
            }
            match row {
                None => html.push_str(r#"<td style="color:#778096">—</td>"#),
                Some(x) => {
                    if x.n_compared == 0 {
                        html.push_str(r#"<td style="color:#778096">—</td>"#);
                    } else {
                        let bit_id_pct = 100.0 * x.n_bit_identical as f64 / x.n_compared as f64;
                        let approx_thresh = approx_bit_identical_threshold(tt);
                        let cell_color = if x.n_bit_identical == x.n_compared {
                            "#3d9a6d"
                        } else if x.p99_dr_km < approx_thresh {
                            "#a0a060"
                        } else {
                            "#e06252"
                        };
                        html.push_str(&format!(
                            r#"<td style="color:{cell_color}; line-height:1.4">{}/{}<br/><span style="font-size:8px; opacity:0.75">{:.1}% ≤1e-10 · p99 {} · max {}</span></td>"#,
                            x.n_bit_identical,
                            x.n_compared,
                            bit_id_pct,
                            fmt_row_max_diff(tt, x.p99_dr_km),
                            fmt_row_max_diff(tt, x.max_dr_km),
                        ));
                    }
                }
            }
        }
        html.push_str("</tr>");
    }

    html.push_str("</tbody></table></div>");
    html
}

/// Summary string for the section header, broken out by test type so
/// "1242/1368 pass" doesn't masquerade as universal failure.
fn fidelity_summary_per_tt(rollups: &[ChannelRollup]) -> String {
    let mut bits: Vec<String> = Vec::new();
    for r in rollups.iter().filter(|r| r.channel != "core") {
        for tt in ["propagation", "ephemeris", "orbit_determination"] {
            if let Some(x) = r.by_test_type.get(tt) {
                if x.n_compared == 0 {
                    continue;
                }
                if x.n_bit_identical == x.n_compared {
                    bits.push(format!(
                        "<span style=\"color:{}\">{}</span>·{} {}/{}",
                        channel_color(&r.channel),
                        r.channel,
                        tt,
                        x.n_bit_identical,
                        x.n_compared,
                    ));
                } else {
                    // Apply the SAME color threshold the matrix below
                    // uses (approx_bit_identical_threshold) so the
                    // descriptor chip's severity color matches the
                    // matrix cell color for the same numbers — prior
                    // bug: chip always red, matrix amber for the same
                    // 110.9 m max (P1-B from review-pass2).
                    let chip_color = if x.p99_dr_km < approx_bit_identical_threshold(tt) {
                        "#a0a060" // amber: under threshold, would be "essentially bit-id"
                    } else {
                        "#e06252" // red: above threshold
                    };
                    bits.push(format!(
                        "<span style=\"color:{}\">{}</span>·{} <span style=\"color:{}\">{}/{}</span> (max {})",
                        channel_color(&r.channel),
                        r.channel,
                        tt,
                        chip_color,
                        x.n_bit_identical,
                        x.n_compared,
                        fmt_row_max_diff(tt, x.max_dr_km),
                    ));
                }
            }
        }
    }
    if bits.is_empty() {
        "Only the core reference channel is present in this report. Run additional channels (rust, python, c, cli) to populate this section.".to_string()
    } else {
        bits.join(" &nbsp;·&nbsp; ")
    }
}

/// Generate the interactive HTML validation report.
///
/// `orbit_comparisons` is the optional sidecar from
/// `validate od`'s `_compare.jsonl` output — empty slice when no
/// orbit-comparison data is available (e.g., CI runs that skip the
/// SBDB / find_orb queries).
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

    let pop_colors: HashMap<&str, &str> = populations
        .iter()
        .map(|&p| (p, population_color(p)))
        .collect();
    let pop_colors_json = serde_json::to_string(&pop_colors).unwrap_or_default();
    let channel_colors_map: HashMap<&str, &str> =
        channels.iter().map(|&c| (c, channel_color(c))).collect();
    let channel_colors_json = serde_json::to_string(&channel_colors_map).unwrap_or_default();
    // Embed the FULL result set so client-side code can render any
    // channel-comparison view without re-running the suite.
    let results_json = serde_json::to_string(&results).unwrap_or_default();
    // Embed the orbit-comparison sidecar (Mahalanobis distances etc.)
    // for the "Fitted orbit + covariance vs references" panel.
    let orbit_comparisons_json = serde_json::to_string(&orbit_comparisons).unwrap_or_default();

    let n_prop = prop_results.len();
    let n_eph = eph_results.len();
    let n_od = od_results.len();
    let n_objects = objects.len();
    let n_populations = populations.len();
    let n_dt = dt_values.len();
    let n_channels = channels.len();
    let channels_label = if n_channels != 1 { "s" } else { "" };

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
    let any_pass_or_fail = rollups
        .iter()
        .filter(|r| r.channel != "core" && r.n_total_compared > 0)
        .count()
        > 0;
    let fidelity_summary = if !any_pass_or_fail {
        "Only the core reference channel is present in this report. Run additional channels (rust, python, c, cli) to populate this section.".to_string()
    } else {
        fidelity_summary_per_tt(&rollups)
    };
    let channel_table_html = build_channel_table_html(&rollups);
    let per_tt_matrix_html = build_per_test_type_matrix_html(&rollups);
    let offenders_html = build_offenders_html(&rollups);

    // ── Hero "quality summary" line — features the cross-channel
    // pass counts and OD convergence rather than raw throughput. Built
    // from the rollup so it's always consistent with the §10 matrix.
    let quality_summary_html = {
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
    // (empyrean 0.9.0 / empyrean-core v0.9.2 / hyperjet 1.9). Per-row
    // run-time provenance (commit hash, kernel hash) is a follow-up.
    let provenance_footer_html = format!(
        r##"
<div class="section" id="s13" data-page="both">
  <div class="section-title">Reproducibility &mdash; Provenance</div>
  <div class="section-desc">
    Provenance for this run — the frame, ephemeris, force model, and
    external references behind the numbers in this report.
  </div>
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
      <tr><td>External OD references</td><td>JPL SBDB (reported normalized rms → reduced χ² + n_obs) &middot; layup (Holman, Smithsonian/CfA) &middot; find_orb (Project Pluto, B. Gray)</td></tr>
      <tr><td>External propagation references</td><td>ASSIST (Holman et al. 2023) on REBOUND IAS15 &middot; OpenOrb (Granvik et al.)</td></tr>
      <tr><td>Observation source</td><td>Minor Planet Center API; fetched at runtime</td></tr>
      <tr><td>Observation weights</td><td>Vereš–Farnocchia–Chesley 2017 (VFC17) per-station RMS floors + nightly deweighting; Eggl–Farnocchia–Chamberlin–Chesley 2020 (EFCC2020) star-catalog debiasing</td></tr>
      <tr><td>Outlier rejection</td><td>Empyrean OD: adaptive information-aware χ² rejection (residual statistics per Carpino, Milani &amp; Chesley 2003)</td></tr>
      <tr><td>JPL source of truth</td><td>One JPL solution, two views: <b>Horizons</b> vectors + ephemerides give the propagation / sky-plane truth; <b>SBDB</b> gives the fitted elements, covariance (Fitted Orbit &amp; Covariance panel), and the reported fit quality (normalized rms, n_obs, radar counts, arc, condition code) used as JPL's OD result.</td></tr>
      <tr><td>Cross-channel fidelity</td><td>distribution channels are bit-identical to the reference to ≤ 10⁻¹⁰ km (100 nm — a sub-ULP floor; float64 ULP at 1 AU ≈ 30 µm)</td></tr>
      <tr><td>Report generated</td><td>{report_run_date}</td></tr>
    </tbody>
  </table>
  </div>
  <div class="section-desc" style="margin-top:24px; font-size:11px;">
    <b>References</b><br/>
    · Holman, M. et al. 2023, "ASSIST: An ephemeris-quality test-particle integrator", PSJ 4(4), 69 (DOI 10.3847/PSJ/acc9a9).<br/>
    · Vereš, P. et al. 2017, "Statistical analysis of astrometric errors for the most productive asteroid surveys", Icarus 296, 139.<br/>
    · Eggl, S., Farnocchia, D., Chamberlin, A. B., Chesley, S. R. 2020, "Star catalog position and proper motion corrections in asteroid astrometry II: the Gaia era", Icarus 339, 113596.<br/>
    · Park, R. S. et al. 2021, "The JPL Planetary and Lunar Ephemerides DE440 and DE441", AJ 161, 105.<br/>
    · Marsden, B. G., Sekanina, Z., Yeomans, D. K. 1973, "Comets and Nongravitational Forces. V", AJ 78, 211.<br/>
    · Vokrouhlický, D. et al. 2015, "The Yarkovsky and YORP Effects", in <i>Asteroids IV</i>, p. 509.<br/>
    · Everhart, E. 1985, in "Dynamics of Comets" (Reidel), p. 185.<br/>
  </div>
</div>
"##,
        report_run_date = report_run_date
    );

    let html = format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Empyrean Dynamics — Validation Report</title>
<link href="https://fonts.googleapis.com/css2?family=Syne:wght@400;500;600;700;800&family=JetBrains+Mono:wght@300;400;500&family=DM+Sans:wght@300;400;500&display=swap" rel="stylesheet">
<script src="https://cdn.plot.ly/plotly-2.35.0.min.js"></script>
<style>
  /* Empyrean Dynamics design tokens — Arctic Blue (dark). Mirrors
     lang/brand/empyrean-tokens.css; muted/chart values follow the WCAG-AA
     brand update (muted #778096, chart grid 0.15, chart axis #778096). */
  :root {{
    --ed-bg: #0d1117; --ed-surface: #151b23; --ed-surface-raised: #1a2332;
    --ed-border: #1a2332; --ed-border-subtle: #14181f;
    --ed-text-primary: #e8e8ec; --ed-text-secondary: #8b9198; --ed-text-muted: #778096;
    --ed-accent: #5b9bd5; --ed-accent-hover: #7bb8e8; --ed-accent-pressed: #3d7ab8;
    --ed-accent-subtle: rgba(91, 155, 213, 0.12);
    --ed-success: #3d9a6d; --ed-warning: #c8a040; --ed-warning-text: #c8a040;
    --ed-error: #d05040; --ed-error-text: #e06252; --ed-event: #e8a040;
    --ed-font-display: 'Syne', sans-serif; --ed-font-mono: 'JetBrains Mono', monospace; --ed-font-body: 'DM Sans', sans-serif;
    --ed-radius-sm: 4px; --ed-radius-md: 6px;
    --ed-chart-grid: rgba(91, 155, 213, 0.15); --ed-chart-axis: #778096; --ed-chart-label: #8b9198;
    --ed-scrollbar-thumb: #2d3440; --ed-scrollbar-hover: #4a5060;
    --ed-input-bg: #151b23; --ed-input-border: #1a2332; --ed-focus-ring: rgba(91, 155, 213, 0.5);
  }}
  * {{ margin: 0; padding: 0; box-sizing: border-box; }}
  body {{ background: var(--ed-bg); color: var(--ed-text-primary); font-family: var(--ed-font-body); font-weight: 300; min-height: 100vh; }}
  .header {{ padding: 60px 60px 40px; max-width: 1400px; margin: 0 auto; border-bottom: 1px solid var(--ed-border); }}
  .header h1 {{ font-family: var(--ed-font-display); font-weight: 700; font-size: 36px; color: var(--ed-text-primary); letter-spacing: -0.5px; margin-bottom: 4px; }}
  .header h2 {{ font-family: var(--ed-font-display); font-weight: 700; font-size: 36px; color: rgba(232,232,236,0.40); letter-spacing: -0.5px; margin-bottom: 12px; }}
  .header .subtitle {{ font-family: var(--ed-font-mono); font-size: 11px; color: var(--ed-accent); letter-spacing: 2px; text-transform: uppercase; }}
  .header .meta {{ font-family: var(--ed-font-mono); font-size: 11px; color: var(--ed-text-muted); margin-top: 8px; }}
  .header .provenance {{ font-family: var(--ed-font-mono); font-size: 10px; color: var(--ed-text-secondary); margin-top: 6px; line-height: 1.7; }}

  .section {{ padding: 60px 60px; max-width: 1400px; margin: 0 auto; border-bottom: 1px solid var(--ed-border); }}
  .tool-hidden {{ display: none !important; }}
  .page-hidden {{ display: none !important; }}
  #page-nav {{ max-width: 1400px; margin: 0 auto; padding: 16px 60px 0; display: flex; flex-wrap: wrap; gap: 4px; border-bottom: 1px solid var(--ed-border); }}
  .page-tab {{ background: transparent; color: var(--ed-text-secondary); border: none; border-bottom: 2px solid transparent; padding: 10px 16px; margin-bottom: -1px; cursor: pointer; font-family: var(--ed-font-mono); font-size: 12px; letter-spacing: 0.5px; text-transform: uppercase; }}
  .page-tab:hover {{ color: var(--ed-text-primary); }}
  .page-tab.active {{ color: var(--ed-accent); border-bottom-color: var(--ed-accent); }}
  .section:last-child {{ border-bottom: none; }}
  .section-num {{ font-family: var(--ed-font-mono); font-size: 10px; color: var(--ed-text-muted); letter-spacing: 2px; text-transform: uppercase; margin-bottom: 8px; }}
  .section-title {{ font-family: var(--ed-font-display); font-weight: 700; font-size: 24px; color: var(--ed-text-primary); margin-bottom: 8px; }}
  .section-desc {{ font-size: 13px; color: var(--ed-text-secondary); line-height: 1.6; max-width: 900px; margin-bottom: 24px; }}
  .panel-title {{ font-family: var(--ed-font-display); font-size: 14px; color: var(--ed-text-primary); margin: 24px 0 8px 0; }}

  .heatmap-container {{ overflow-x: auto; max-width: 100%; padding-bottom: 8px; }}
  .heatmap-container::-webkit-scrollbar {{ height: 6px; }}
  .heatmap-container::-webkit-scrollbar-track {{ background: var(--ed-bg); border-radius: 3px; }}
  .heatmap-container::-webkit-scrollbar-thumb {{ background: var(--ed-scrollbar-thumb); border-radius: 3px; }}
  .heatmap-container::-webkit-scrollbar-thumb:hover {{ background: var(--ed-scrollbar-hover); }}
  table.heatmap {{ border-collapse: collapse; font-family: var(--ed-font-mono); font-size: 10px; }}
  table.heatmap th {{ padding: 6px 10px; color: var(--ed-text-secondary); font-weight: 400; letter-spacing: 1px; text-transform: uppercase; border-bottom: 1px solid var(--ed-border); position: sticky; top: 0; background: var(--ed-bg); z-index: 1; }}
  table.heatmap th.dt-col {{ text-align: center; min-width: 70px; }}
  table.heatmap td {{ padding: 5px 8px; text-align: center; border-bottom: 1px solid var(--ed-border-subtle); cursor: default; }}
  table.heatmap td.obj-name {{ text-align: left; color: var(--ed-text-primary); font-size: 11px; white-space: nowrap; padding-right: 16px; position: sticky; left: 0; background: var(--ed-bg); }}
  table.heatmap td.pop-tag {{ text-align: left; font-size: 9px; padding-right: 12px; }}
  table.heatmap td.cell {{ font-size: 9px; border-radius: 2px; }}
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
  .summary-card .label {{ font-family: var(--ed-font-mono); font-size: 9px; color: var(--ed-text-secondary); letter-spacing: 1px; text-transform: uppercase; margin-top: 4px; }}
  .legend {{ display: flex; gap: 16px; flex-wrap: wrap; margin-bottom: 16px; }}
  .legend-item {{ display: flex; align-items: center; gap: 4px; font-family: var(--ed-font-mono); font-size: 9px; color: var(--ed-text-secondary); }}
  .channel-toggle {{ display: inline-flex; flex-wrap: wrap; gap: 6px; margin: 8px 0 16px 0; font-family: var(--ed-font-mono); font-size: 10px; }}
  .channel-toggle button {{ background: var(--ed-surface); border: 1px solid var(--ed-border); border-radius: var(--ed-radius-sm); color: var(--ed-text-secondary); padding: 4px 10px; cursor: pointer; font-family: inherit; font-size: 10px; }}
  .channel-toggle button.active {{ color: var(--ed-text-primary); border-color: var(--ed-accent); background: var(--ed-surface-raised); }}
  .od-table {{ font-family: var(--ed-font-mono); font-size: 10px; border-collapse: collapse; min-width: 100%; }}
  .od-table th, .od-table td {{ padding: 6px 10px; border-bottom: 1px solid var(--ed-border); text-align: right; }}
  .od-table th {{ color: var(--ed-text-secondary); font-weight: 400; letter-spacing: 1px; text-transform: uppercase; font-size: 9px; }}
  .od-table td.obj {{ text-align: left; color: var(--ed-text-primary); }}
  .od-table tr.diverged {{ background: rgba(208, 80, 64, 0.08); }}

  /* Tool-pair selector */
  #tool-selector select {{ background: var(--ed-input-bg); color: var(--ed-accent); border: 1px solid var(--ed-input-border); border-radius: var(--ed-radius-sm); padding: 5px 8px; font-family: var(--ed-font-mono); font-size: 13px; }}
  #tool-selector select:focus {{ outline: none; border-color: var(--ed-accent); box-shadow: 0 0 0 2px var(--ed-focus-ring); }}
  #tool-swap {{ background: var(--ed-input-bg); color: var(--ed-text-secondary); border: 1px solid var(--ed-input-border); border-radius: var(--ed-radius-sm); padding: 5px 9px; cursor: pointer; font-family: var(--ed-font-mono); }}
  #tool-swap:hover {{ color: var(--ed-accent); border-color: var(--ed-accent); }}
  /* Performance strip */
  .speed-group {{ margin: 18px 0 6px; }}
  .speed-group-title {{ font-family: var(--ed-font-mono); font-size: 10px; letter-spacing: 2px; text-transform: uppercase; color: var(--ed-text-secondary); margin-bottom: 8px; }}
  .speed-row {{ display: flex; align-items: center; gap: 10px; margin: 5px 0; }}
  .speed-label {{ font-family: var(--ed-font-mono); font-size: 11px; color: var(--ed-text-primary); flex: 0 0 170px; text-align: right; }}
  .speed-track {{ flex: 1 1 auto; position: relative; height: 20px; background: var(--ed-surface); border-radius: var(--ed-radius-sm); overflow: hidden; }}
  .speed-bar {{ position: absolute; inset: 0 auto 0 0; border-radius: var(--ed-radius-sm); min-width: 2%; }}
  .speed-val {{ font-family: var(--ed-font-mono); font-size: 11px; color: var(--ed-text-primary); flex: 0 0 76px; }}
  .speed-chip {{ font-family: var(--ed-font-mono); font-size: 9px; color: var(--ed-text-muted); flex: 0 0 190px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }}
  @media (max-width: 700px) {{ .speed-chip {{ display: none; }} .speed-label {{ flex-basis: 120px; }} }}
  /* Keyboard focus visibility for interactive controls (WCAG 2.4.7) */
  .page-tab:focus-visible, .channel-toggle button:focus-visible, #tool-swap:focus-visible,
  #tool-selector select:focus-visible, [role="button"]:focus-visible {{
    outline: 2px solid var(--ed-accent); outline-offset: 2px;
  }}
  /* ── Overview (basic) view ─────────────────────────────────────── */
  /* The Overview tab reuses the comparison page's DOM: prose, section
     numbers, and per-section scorecards hide; one-line captions and the
     hero verdict strip show. */
  .basic-caption {{ display: none; color: var(--ed-text-secondary); font-size: 13px; margin: 2px 0 14px; }}
  body.view-overview .section-desc, body.view-overview .section-num {{ display: none; }}
  body.view-overview .basic-caption {{ display: block; }}
  body.view-overview .header .meta, body.view-overview .prov-detail {{ display: none; }}
  body.view-overview #prop-scorecard-pair, body.view-overview #eph-scorecard-pair {{ display: none; }}
  .hero-cards {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(185px, 1fr)); gap: 12px; margin: 14px 0 6px; }}
  .hero-card {{ background: var(--ed-surface); border: 1px solid var(--ed-border); border-radius: var(--ed-radius-sm); padding: 16px 18px; }}
  .hero-card .hv {{ font-family: var(--ed-font-display); font-weight: 700; font-size: 30px; color: var(--ed-text-primary); line-height: 1.15; }}
  .hero-card .hl {{ font-family: var(--ed-font-mono); font-size: 9px; color: var(--ed-text-secondary); letter-spacing: 1.5px; text-transform: uppercase; margin-bottom: 6px; }}
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
</style>
</head>
<body>

<div class="header">
  <div class="subtitle">Validation Report</div>
  <h1>EMPYREAN</h1>
  <h2>DYNAMICS</h2>
  <div class="meta" style="font-size:12px; line-height:1.8;">{quality_summary_html}</div>
  <div class="meta" style="margin-top:6px; color:var(--ed-text-secondary);">{n_prop} propagation · {n_eph} ephemeris · {n_od} OD · {n_objects} objects · {n_channels} channel{channels_label}</div>
  <div class="provenance"><span class="prov-detail">Test epoch: {test_epoch_label}<br/>Frame: ICRF (J2000) · Ephemeris: DE440 · Force model: empyrean::standard (1PN GR · 16-asteroid SB441-N16 perturbers · Marsden A1/A2/A3 + g(r) non-grav)<br/>Coverage: {coverage_line}<br/></span>Report run: {report_run_date} &nbsp;·&nbsp; <a href="#s13" style="color:#5b9bd5; text-decoration:none">▸ provenance &amp; references</a> &nbsp;·&nbsp; <a href="javascript:void(0)" onclick="downloadJSON()" style="color:#5b9bd5; text-decoration:none">↓ download embedded JSON</a></div>
</div>

<div id="page-nav">
  <button data-page="overview" class="page-tab active">Overview</button>
  <button data-page="comparison" class="page-tab">Advanced</button>
  <button data-page="empyrean" class="page-tab">Empyrean Internals</button>
</div>

<div class="section" id="s00-hero" data-view="overview" style="padding-bottom:8px;">
  <div id="hero-strip"></div>
</div>

<div class="section" id="tool-selector" data-view="both" style="padding-bottom:16px;">
  <div class="section-title" style="font-size:16px; margin-bottom:10px;">Comparison</div>
  <div style="display:flex; align-items:center; gap:10px; flex-wrap:wrap; font-family:'JetBrains Mono',monospace; font-size:13px;">
    <select id="tool1-select" aria-label="First tool to compare"></select>
    <span style="color:var(--ed-text-muted)">vs</span>
    <select id="tool2-select" aria-label="Second tool to compare"></select>
    <button id="tool-swap" title="Swap the two tools" aria-label="Swap the two tools">&#8646;</button>
    <span id="tool-caption" style="color:#8b9198; margin-left:6px;" aria-live="polite" aria-atomic="true"></span>
  </div>
  <div id="tool-note" class="section-desc" style="margin-top:8px; color:var(--ed-text-secondary);">Pick any two tools — the accuracy panels below re-render for the selected pair. Empyrean-specific analyses (multichannel fidelity, Jet1-vs-f64 timing &amp; uncertainty cost, non-grav recovery, fitted covariance) live on the <b>Empyrean Internals</b> page. Some pairings are unavailable on an axis — see each panel.</div>
</div>

<div class="section" data-page="comparison" style="padding-bottom:20px;">
  <div class="section-title" style="font-size:16px; margin-bottom:12px;">Contents</div>
  <div style="font-family:var(--ed-font-mono); font-size:11px; line-height:2.4; color:var(--ed-accent);">
    <a href="#s01" style="color:var(--ed-accent); text-decoration:none;">01 Summary</a><br/>
    <a href="#s01b" style="color:var(--ed-accent); text-decoration:none;">02 External Reference Tools — Transparency</a><br/>
    <a href="#s02" style="color:var(--ed-accent); text-decoration:none;">03 Propagation &mdash; Position Agreement</a><br/>
    <a href="#s03" style="color:var(--ed-accent); text-decoration:none;">04 Propagation &mdash; Error Growth (per population)</a><br/>
    <span id="toc-s04" style="display:none"><a href="#s04" style="color:var(--ed-accent); text-decoration:none;">05 Propagation &mdash; Tool vs Truth</a><br/></span>
    <a href="#s06" style="color:var(--ed-accent); text-decoration:none;">06 Ephemeris &mdash; Sky-plane Agreement</a><br/>
    <a href="#s07" style="color:var(--ed-accent); text-decoration:none;">07 Ephemeris &mdash; RA/Dec Offsets</a><br/>
    <a href="#s08" style="color:var(--ed-accent); text-decoration:none;">08 Ephemeris &mdash; Offset Growth</a><br/>
    <a href="#s09" style="color:var(--ed-accent); text-decoration:none;">09 Orbit Determination &mdash; Diagnostics</a><br/>
    <a href="#s13" style="color:var(--ed-accent); text-decoration:none;">Reproducibility &mdash; Provenance</a>
    <div style="margin-top:10px; color:var(--ed-text-muted); font-size:10px;">Empyrean Internals page → 01 Timing (empyrean vs ASSIST) · 02 Non-grav Recovery · 03 Channel Fidelity · 04 Uncertainty Cost (Jet1 vs f64) · 05 Fitted Orbit + Covariance</div>
  </div>
</div>

<div class="section" id="s01">
  <div class="section-num">01</div>
  <div class="section-title">Summary</div>
  <div class="summary-grid">
    <div class="summary-card"><div class="value">{n_objects}</div><div class="label">Objects</div></div>
    <div class="summary-card"><div class="value">{n_populations}</div><div class="label">Populations</div></div>
    <div class="summary-card"><div class="value">{n_dt}</div><div class="label">Time Offsets</div></div>
    <div class="summary-card"><div class="value">{n_prop}</div><div class="label">Propagation Cases</div></div>
    <div class="summary-card"><div class="value">{n_eph}</div><div class="label">Ephemeris Cases</div></div>
    <div class="summary-card"><div class="value">{n_od}</div><div class="label">OD Cases</div></div>
    <div class="summary-card"><div class="value">{n_channels}</div><div class="label">Channels</div></div>
  </div>
  <div class="section-desc" style="margin-top:6px;">Two axes of validation: the <b>distribution channels</b> agree with each other (every channel returns bit-identical numbers from one shared engine — the <b>Empyrean Internals</b> page), and the engine agrees with <b>independent tools</b> (JPL Horizons, ASSIST, OpenOrb, find_orb, layup — pick any pair above). The stack under test:</div>
  <div class="heatmap-container">
  <table class="od-table" style="max-width:720px; margin-bottom:6px;">
    <thead><tr><th style="text-align:left">Component</th><th style="text-align:left">Purpose</th><th>Version</th></tr></thead>
    <tbody>
      <tr><td style="text-align:left">hyperjet</td><td style="text-align:left">Automatic differentiation &mdash; STMs / STTs</td><td>1.9.0</td></tr>
      <tr><td style="text-align:left">empyrean-core</td><td style="text-align:left">Reference channel (<code>validate-core</code>)</td><td>0.9.2</td></tr>
      <tr><td style="text-align:left">empyrean</td><td style="text-align:left">Distribution under test &mdash; Rust wrapper, C ABI, Python wheel, CLI</td><td>0.9.0</td></tr>
      <tr><td style="text-align:left">empyrean-py</td><td style="text-align:left">Python wheel</td><td>0.9.0</td></tr>
      <tr><td style="text-align:left">empyrean-cli</td><td style="text-align:left">Command-line interface</td><td>0.9.0</td></tr>
    </tbody>
  </table>
  </div>
  <div class="legend">
{pop_legend}  </div>
  <div class="legend">
{channel_legend}  </div>
</div>

<div class="section" id="s01b">
  <div class="section-num">02</div>
  <div class="section-title">External Reference Tools &mdash; Transparency</div>
  <div class="section-desc">Every external comparison in this report is driven by a runner script in <a href="https://github.com/Empyrean-Dynamics/empyrean-validation" target="_blank" rel="noopener">empyrean-validation</a>. Each script is the full configuration we used to invoke that tool — settings, force model, weighting, rejection. If you want to reproduce a number in any panel, the script tells you exactly how we got there. (The tools themselves are external dependencies and are <b>not linked into</b> empyrean — they're run independently and their JSON outputs are merged into the report at the comparison step.)</div>
  <div class="heatmap-container">
  <table class="od-table" style="margin-top:0.5em">
    <thead><tr>
      <th style="text-align:left">Tool</th>
      <th style="text-align:left">Reference</th>
      <th style="text-align:left">Used for</th>
      <th style="text-align:left">Our runner</th>
    </tr></thead>
    <tbody>
      <tr>
        <td><b>JPL</b></td>
        <td>NASA JPL SSD · Horizons + SBDB (one solution)</td>
        <td>Propagation &amp; ephemeris truth (Horizons); orbit determination &mdash; JPL's reported fit quality (normalized rms → reduced χ², n_obs, radar, arc) + fitted orbit &amp; covariance (SBDB)</td>
        <td><code>plan</code> step &mdash; queried, not a runner (SBDB + Horizons disk cache)</td>
      </tr>
      <tr>
        <td><b>ASSIST</b></td>
        <td>Holman et al. 2023 · REBOUND IAS15 · DE440</td>
        <td>N-body propagation; first-order STM via 6 variational particles (first order only).</td>
        <td><a href="https://github.com/Empyrean-Dynamics/empyrean-validation/blob/main/runners/assist/run_assist.py" target="_blank" rel="noopener"><code>runners/assist/run_assist.py</code></a></td>
      </tr>
      <tr>
        <td><b>layup</b></td>
        <td>Holman / Smithsonian · MIT · ASSIST-backed</td>
        <td>Orbit determination from ADES; reference for reduced χ² (Veres-2017 weighting)</td>
        <td><a href="https://github.com/Empyrean-Dynamics/empyrean-validation/blob/main/runners/layup/run_layup.py" target="_blank" rel="noopener"><code>runners/layup/run_layup.py</code></a></td>
      </tr>
      <tr>
        <td><b>find_orb</b></td>
        <td>Gray (Project Pluto) · MPC-grade OD</td>
        <td>Orbit determination from ADES astrometry (post-fit RMS reference); its fitted orbit is then propagated by find_orb itself to the plan's epochs for propagation + sky-plane comparison (<b>fit-then-propagate</b> &mdash; unlike ASSIST / OpenOrb, which replay the plan's initial conditions, so these diffs include the fit-vs-JPL-orbit difference)</td>
        <td><a href="https://github.com/Empyrean-Dynamics/empyrean-validation/blob/main/runners/findorb/run_findorb.py" target="_blank" rel="noopener"><code>runners/findorb/run_findorb.py</code></a></td>
      </tr>
      <tr>
        <td><b>OpenOrb</b></td>
        <td>Granvik et al. · University of Helsinki (Fortran)</td>
        <td>N-body propagation &amp; ephemeris (Bulirsch&ndash;Stoer, planets + Moon + Pluto, relativity on; no asteroid perturbers &mdash; BC430 not installed, so km-scale asteroid-perturbation signal remains in its residual). The plan's SSB states are converted to OpenOrb's heliocentric convention on the way in and back on the way out.</td>
        <td><a href="https://github.com/Empyrean-Dynamics/empyrean-validation/blob/main/runners/oorb/run_oorb.py" target="_blank" rel="noopener"><code>runners/oorb/run_oorb.py</code></a></td>
      </tr>
      <tr style="opacity:0.55">
        <td colspan="4"><small style="color:#8b9198"><b>Planned / opt-in (not in this run):</b> <b>OrbFit</b> (OrbFit Consortium / IAU MPC — CMC2003 rejection), <b>kete</b> (Dar Dahlen — independent Rust/Python NEO toolkit, originally Caltech IPAC / NEO Surveyor), <b>jorbit</b> (JAX autodiff propagator / OD). Runner scripts live under <a href="https://github.com/Empyrean-Dynamics/empyrean-validation/tree/main/runners" target="_blank" rel="noopener"><code>runners/</code></a>; their JSON merges into the report when run.</small></td>
      </tr>
    </tbody>
  </table>
  </div>
</div>

<div class="section" id="s02" data-view="both">
  <div class="section-num">03</div>
  <div class="section-title">Propagation &mdash; Position Agreement</div>
  <div class="basic-caption">How closely the two tools' propagated positions agree, per object and time offset.</div>
  <div class="section-desc">How closely the two selected tools' propagated positions agree. The scorecard is the at-a-glance |&Delta;| between them; the heatmap breaks it out per object and propagation offset (color scale tagged to encounter-distance thresholds). Pick the pair at the top of the report.</div>
  <div id="prop-scorecard-pair"></div>
  <div class="channel-toggle" id="prop-heat-toggle" style="display:none">
    <button class="active" data-mode="physical">Physical (km)</button>
    <button data-mode="sigma" title="Offset as a Mahalanobis distance in Empyrean's propagated covariance">Uncertainty (σ)</button>
  </div>
  <div id="heatmap-horizons"></div>
</div>

<div class="section" id="s03" data-view="both">
  <div class="section-num">04</div>
  <div class="section-title">Propagation &mdash; Error Growth</div>
  <div class="basic-caption">Position difference between the two tools, growing with propagation time.</div>
  <div class="section-desc">Pairwise position difference between the two selected tools over time, log-y — median curve plus IQR band per population, with named outliers. The <b>Selected pair</b> view tracks the tool pair chosen above. The <b>Empyrean channels</b> overlay is an Empyrean-internal cross-check that every distribution channel (rust / python / c / cli / core) returns bit-identical numbers; the curves overlap at chart resolution.</div>
  <div class="channel-toggle" id="s03-toggle">
    <button class="active" data-mode="rust">Selected pair</button>
    <button data-mode="all">Empyrean channels</button>
  </div>
  <div class="chart-container">
    <div id="error-growth-chart" style="height:520px;"></div>
  </div>
</div>

<div class="section" id="s04" style="display:none" data-requires="empyrean">
  <div class="section-num">05</div>
  <div class="section-title">Propagation &mdash; Tool vs Truth</div>
  <div class="section-desc">Log-y |tool1 &minus; tool2| vs |tool2 &minus; JPL Horizons| over time — how the two selected tools' disagreement compares to the second tool's own distance from the Horizons truth. Points below the y = x line mean the two tools agree more tightly than either matches Horizons. Triangular markers = comets and ISOs (non-grav model differences between tools); circles = all other (non-cometary) objects. Threshold lines at 1 km / 100 km / 1 AU.</div>
  <div class="chart-container">
    <div id="assist-chart" style="height:520px;"></div>
  </div>
  <div class="summary-grid" style="margin-top:16px;">
    <div class="summary-card"><div id="assist-median" class="value">—</div><div class="label">Median |tool1 &minus; tool2|</div></div>
    <div class="summary-card"><div id="assist-ratio" class="value">—</div><div class="label">Median |t1&minus;t2| &divide; |t2&minus;JPL Horizons|</div></div>
    <div class="summary-card"><div id="assist-rows" class="value">—</div><div class="label">Rows compared</div></div>
  </div>
</div>

<div class="section" id="s05" style="display:none" data-page="empyrean">
  <div class="section-num">01</div>
  <div class="section-title">Propagation &mdash; Timing</div>
  <div class="section-desc">Per-row timing (log-y), Empyrean vs ASSIST, split by <code>propagation_uncertainty</code> mode so it's apples-to-apples: Empyrean's f64 path vs single-particle ASSIST, and Empyrean's Jet1 + 6&times;6 covariance vs ASSIST's 6 first-order variational particles. <b>Caveat:</b> both sides re-integrate fresh from t₀ per call, so this is head-to-head; where ASSIST reads sub-microsecond it's REBOUND's cached ephemeris + IAS15 step reuse across closely spaced epochs, not a different methodology.</div>
  <div class="panel-title">f64 propagation (state only)</div>
  <div class="chart-container">
    <div id="timing-chart" style="height:480px;"></div>
  </div>
  <div class="panel-title">STM/STT-bearing propagation (first-order, second-order, Auto cascade)</div>
  <div class="section-desc">Empyrean's per-call cost across the STM-bearing surface. <b>First-order</b> pairs Jet1 (6 partials) against ASSIST's 6 variational particles — the one like-for-like external comparison, since ASSIST is first-order only. <b>Second-order</b> (Jet2, 6 + 21 partials) and <b>Auto</b> (adaptive) are Empyrean-only.</div>
  <div class="chart-container">
    <div id="timing-cov-chart" style="height:540px;"></div>
  </div>
</div>

<div class="section" id="s06" data-view="both">
  <div class="section-num">06</div>
  <div class="section-title">Ephemeris &mdash; Sky-plane Agreement</div>
  <div class="basic-caption">Predicted sky positions (RA/Dec) — separation vs the &sim;1 mas Gaia astrometric floor.</div>
  <div class="section-desc">Sky-plane (RA/Dec) agreement between the two selected tools' predicted positions, by population. Median and 95th-percentile separation, in milliarcseconds (1 mas &asymp; the Gaia astrometric noise floor). Each cell is the <b>mean over the observing sites</b> (W84, F51, X05, 500, I41) so it isn't hostage to any single site's parallax / light-time handling; hover shows the site count and spread. The chart below shows how that separation grows with propagation offset.</div>
  <div id="eph-scorecard-pair"></div>
  <div class="panel-title">Sky-plane separation per object</div>
  <div class="channel-toggle" id="eph-heat-toggle" style="display:none">
    <button class="active" data-mode="physical">Physical (mas)</button>
    <button data-mode="sigma" title="Offset as a Mahalanobis distance in Empyrean's propagated RA/Dec covariance">Uncertainty (σ)</button>
  </div>
  <div class="section-desc">The selected pair's RA/Dec separation per object and propagation offset (milliarcseconds); color scale anchored to the Gaia 1 mas floor.</div>
  <div id="eph-heatmap-pair"></div>
  <div class="panel-title">Separation vs propagation offset</div>
  <div class="channel-toggle" id="s06-toggle">
    <button class="active" data-mode="rust">Selected pair</button>
    <button data-mode="all">Empyrean channels</button>
  </div>
  <div class="chart-container">
    <div id="eph-sep-chart" style="height:520px;"></div>
  </div>
</div>

<div class="section" id="s-speed" data-view="both">
  <div class="section-title">Performance &mdash; Wall Clock</div>
  <div class="basic-caption">Median wall clock per row, per tool — log scale. The chips explain the floors: in-process libraries sit at ms, subprocess tools pay spawn + ephemeris load, JAX pays per-call JIT.</div>
  <div class="section-desc">Median wall clock per row for every tool with timing data, by axis (log-scaled bars). These are <b>single-particle replay</b> workloads — batch throughput is a different race — and each tool runs at its own accuracy target, so speed alone is not a ranking. Architecture chips mark the structural floors: <b>subprocess</b> tools (OpenOrb, layup, find_orb) pay process spawn + ephemeris load per invocation; <b>JAX</b> (jorbit) pays a per-call JIT/dispatch floor that would amortize in batched use; find_orb's OD bar is <b>per fit</b> (its marginal per-epoch propagation cost is ≈ 0 — the invocation toll includes the full astrometry pipeline). Empyrean shows its full uncertainty ladder — bare f64, the production Jet1 + 6×6 covariance path, adaptive Auto, Jet2 STM+STT, the 120-sample sigma-point transform, and seeded 100-sample Monte Carlo — all through the same <code>propagate()</code> call. Each median covers the rows that tool actually completed — object mixes can differ between tools until a full catalog run.</div>
  <div id="speed-strip"></div>
</div>

<div class="section" id="s07">
  <div class="section-num">07</div>
  <div class="section-title">Ephemeris &mdash; RA/Dec Offsets</div>
  <div class="section-desc">RA·cos(δ) vs Dec offset (mas) between the two selected tools, clipped to ±100 mas (rows beyond are in the outlier table below). 1σ (solid) and 3σ (dotted) ellipses assume a zero-mean iid Gaussian cloud. For scale: Gaia DR3 single-frame precision is ≲ 1 mas; ground-based survey residuals run ~100–300 mas.</div>
  <div class="chart-container">
    <div id="eph-scatter-chart" style="height:560px;"></div>
  </div>
  <div class="panel-title">Outliers excluded from the scatter</div>
  <div class="heatmap-container">
    <table class="heatmap" style="min-width:100%"><thead><tr><th style="text-align:left">Object</th><th>Channel</th><th>dt</th><th>Observer</th><th>dRA·cos(δ)</th><th>dDec</th><th>‖d‖</th></tr></thead>
      <tbody id="eph-outliers"></tbody>
    </table>
  </div>
</div>

<div class="section" id="s08">
  <div class="section-num">08</div>
  <div class="section-title">Ephemeris &mdash; Offset Growth</div>
  <div class="section-desc">dRA·cos(δ) (top) and dDec (bottom) offset between the two selected tools over the test-epoch time window, symlog y so chaotic outliers and machine-epsilon-grade rows both render. Solid lines: median per population. Shaded band: 25-75 percentile (IQR) per population. Two stacked panels eliminate the solid/dashed-overlay readability problem.</div>
  <div class="chart-container">
    <div id="eph-resid-ra-chart" style="height:280px;"></div>
  </div>
  <div class="chart-container">
    <div id="eph-resid-dec-chart" style="height:280px;"></div>
  </div>
</div>

<div class="section" id="s09" data-requires="empyrean">
  <div class="section-num">09</div>
  <div class="section-title">Orbit Determination &mdash; Diagnostics</div>
  <div class="section-desc">Do the fitters land in the same minimum? Post-fit RMS, reduced χ² = χ²/ν, and iteration count for Empyrean's OD (cross-channel bit-identical agreement lives on the Internals page). The external columns follow the second tool selected above — <b>JPL</b> (SBDB reduced χ² ≈ rms² + n_obs), <b>find_orb</b> (post-fit RMS), or <b>layup</b> (reduced χ²) — and are hidden when it carries no OD (e.g. ASSIST). JPL's rms is JPL's own weighting/debiasing over an arc that includes radar; find_orb's low-coverage fallback on short arcs (2 anchor obs) is greyed as &ldquo;rejection-mismatch.&rdquo; Neither is a like-for-like fit against Empyrean's optical-only weights.</div>
  <div id="od-empty" class="section-desc" style="display:none; color:#8b9198">No orbit-determination rows in this report. Run the OD subset to populate this section.</div>
  <div id="od-content">
    <div class="panel-title">Per-object overview</div>
    <div id="od-ext-note" class="section-desc" style="display:none; margin-bottom:12px;">Showing Empyrean's OD only. Select <b>JPL</b>, <b>find_orb</b>, or <b>layup</b> as the second tool (top of the report) to compare Empyrean's fit against an external reference.</div>
    <div class="heatmap-container">
      <table class="od-table">
        <thead><tr>
          <th class="obj" style="text-align:left">Object</th>
          <th>n_obs</th>
          <th>χ²<sub>r</sub> = χ²/ν</th>
          <th>RMS RA &middot; Dec</th>
          <th>RMS combined</th>
          <th>iter</th>
          <th class="od-col-jpl">JPL χ²<sub>r</sub><br/><small style="font-weight:300; opacity:0.6">(≈ rms²)</small></th>
          <th class="od-col-jpl">JPL n_obs<br/><small style="font-weight:300; opacity:0.6">(+radar · arc)</small></th>
          <th class="od-col-layup">layup χ²<sub>r</sub><br/><small style="font-weight:300; opacity:0.6">(Δ vs Empyrean)</small></th>
          <th class="od-col-findorb">find_orb RMS</th>
          <th class="od-col-findorb">find_orb obs coverage</th>
          <th class="od-col-findorb">Δ vs find_orb</th>
          <th>cross-channel OD<br/><small style="font-weight:300; opacity:0.6">(worst Δχ² · worst ‖Δr_fit‖)</small></th>
        </tr></thead>
        <tbody id="od-overview"></tbody>
      </table>
    </div>

    <div class="panel-title">Post-fit RMS (Empyrean)</div>
    <div class="chart-container">
      <div id="od-rms-chart" style="height:380px;"></div>
    </div>

    <div id="od-rms-vs-findorb-panel">
    <div class="panel-title">Empyrean OD combined RMS vs find_orb (y = x line)</div>
    <div class="section-desc">Per-object scatter of Empyrean OD's combined RA·cosδ + Dec RMS against find_orb's <code>rms</code>. Points on the y = x diagonal mean the two pipelines agree; deviations above the line mean Empyrean reports a larger RMS than find_orb on that object (typically because find_orb's rejection policy trimmed more observations). Log scale on both axes.</div>
    <div class="chart-container">
      <div id="od-rms-vs-findorb-chart" style="height:480px;"></div>
    </div>
    </div>
  </div>
</div>

<div class="section" id="overview-footer" data-view="overview" style="padding-top:24px; padding-bottom:32px;">
  Methods, all six reference tools, per-axis detail and outliers &rarr; <b>Advanced</b> &middot; cross-channel fidelity, timing and covariance &rarr; <b>Empyrean Internals</b>.
</div>

<div class="section" id="s09b" data-page="empyrean">
  <div class="section-num">02</div>
  <div class="section-title">Non-gravitational Recovery &mdash; Fitted A1/A2/A3 vs JPL</div>
  <div class="section-desc">Objects with a non-zero JPL non-grav signal (Apophis, Bennu, the comets) are re-fit with <code>solve_for = StateAndNonGrav</code>; the fitted Marsden A1/A2/A3 and their 1σ (from the 9×9 covariance diagonal) are compared to JPL. <b>PASS</b> when <code>|z| = |od_a − ic_a| / σ ≤ 3</code> on every coefficient. A <code>None</code> is a loud <b>FAIL</b>, not a blank — the 9×9 covariance was absent (a silent fall-back to a state-only fit), the exact regression this section catches. All four channels (rust / c / cli / python) run side-by-side so an FFI drop of the non-grav block shows immediately.</div>
  <div id="ng-empty" class="section-desc" style="display:none; color:#8b9198">No non-gravitational-recovery rows in this report. Run the OD subset against objects with a known SBDB non-grav signal to populate this section.</div>
  <div id="ng-content">
    <div class="panel-title">Fitted A1/A2/A3 vs JPL SBDB &mdash; σ-consistency per channel</div>
    <div class="heatmap-container">
      <table class="od-table">
        <thead><tr>
          <th class="obj" style="text-align:left">Object</th>
          <th>Channel</th>
          <th>Coeff</th>
          <th>Fitted (AU/day²)</th>
          <th>JPL SBDB (AU/day²)</th>
          <th>z = (fit&minus;JPL)/σ</th>
          <th>Result</th>
        </tr></thead>
        <tbody id="ng-overview"></tbody>
      </table>
    </div>
  </div>
</div>

<div class="section" id="s10" data-page="empyrean">
  <div class="section-num">03</div>
  <div class="section-title">Distribution Channel Fidelity</div>
  <div class="section-desc">Cross-channel agreement by test type — every channel runs end-to-end through libempyrean (or empyrean-core directly for <code>core</code>). Bit-identical is expected for propagation and ephemeris; OD is bit-identical in fitted state (see the drift chart below).
  <br/><br/><small style="color:#8b9198"><b>Denominators:</b> rust runs the full uncertainty grid (Auto + first/second-order + f64); other channels run only their public-API modes (first_order_with_cov + f64_no_cov), so their compared counts are <code>≈ rust ÷ 2</code> by design, not a coverage gap. Fidelity is per-row bit-exactness against the matching core row.</small></div>
  <div class="section-desc">{fidelity_summary}</div>

  <div class="panel-title">Per-test-type matrix</div>
  {per_tt_matrix_html}

  <div class="panel-title">ECDF of |emp_pos − core.emp_pos| per channel × test type</div>
  <div class="chart-container">
    <div id="ecdf-chart" style="height:440px;"></div>
  </div>

  <div class="panel-title">Per-row Δr by population (beeswarm)</div>
  <div class="chart-container">
    <div id="dev-beeswarm-chart" style="height:480px;"></div>
  </div>

  <div class="panel-title">Timing vs accuracy trade-off</div>
  <div class="chart-container">
    <div id="timing-acc-chart" style="height:420px;"></div>
  </div>

  <div class="panel-title">Top metric details</div>
  {channel_table_html}

  <div class="panel-title">Offending rows (channel-vs-core threshold {fidelity_threshold:.0e})</div>
  {offenders_html}

  <div id="od-xchannel-content">
    <div class="panel-title" style="margin-top:28px;">Cross-channel OD agreement</div>
    <div class="section-desc">The orbit-determination pipeline reached through every distribution channel should land in the same minimum on the same observations &mdash; bit-identical fitted state across rust / c / cli / python is the expected outcome (the per-object drift scatter sits on the 10⁻⁹ km chart floor when they agree). These three views break that down per channel.</div>

    <div class="panel-title">χ² across channels</div>
    <div class="chart-container">
      <div id="od-chi2-chart" style="height:480px;"></div>
    </div>
    <div class="section-desc" style="font-size:0.85em; margin-top:-12px;">Note: the <code>c</code> and <code>cli</code> channels do not yet marshal <code>od_reduced_chi2</code> through the C ABI / CLI, so they have no χ² series and are omitted from this chart.</div>

    <div class="panel-title">Iterations to convergence</div>
    <div class="chart-container">
      <div id="od-iter-chart" style="height:380px;"></div>
    </div>

    <div class="panel-title">Fitted-state drift to core (km, log-y)</div>
    <div class="chart-container">
      <div id="od-fitdr-chart" style="height:420px;"></div>
    </div>
  </div>
</div>

<div class="section" id="s11" data-page="empyrean">
  <div class="section-num">04</div>
  <div class="section-title">Propagation &mdash; Uncertainty Cost (Jet1 vs f64)</div>
  <div class="section-desc">
    <strong>Empyrean is uncertainty-first by design.</strong> A <code>propagate()</code>
    call on an orbit that carries a covariance dispatches automatically to first-order
    Jet1/STM — no flag — and a 6×6 covariance falls out of the same call; with no
    covariance it runs plain f64 and you pay nothing for what you don't use. This panel
    shows the per-population cost of that covariance path
    (<code>first_order_with_cov</code> vs <code>f64_no_cov</code>) — the production hot path.
  </div>
  <div id="unc-empty" class="section-desc" style="display:none; color:#8b9198">No paired Jet1/f64 rows in this report. Re-run rust + core channels to populate this section.</div>

  <div class="panel-title">Per-population timing — Jet1 vs f64 (rust channel)</div>
  <div class="chart-container">
    <div id="unc-pop-chart" style="height:420px;"></div>
  </div>

  <div class="panel-title">Per-row paired scatter — log–log time(Jet1) vs time(f64)</div>
  <div class="chart-container">
    <div id="unc-scatter-chart" style="height:520px;"></div>
  </div>

  <div class="panel-title">Summary</div>
  <div class="heatmap-container">
    <table class="heatmap" style="min-width:100%">
      <thead><tr>
        <th style="text-align:left">Channel</th>
        <th>n paired</th>
        <th>Jet1 p50</th>
        <th>f64 p50</th>
        <th>p50 ratio</th>
        <th>Jet1 p95</th>
        <th>f64 p95</th>
        <th>p95 ratio</th>
      </tr></thead>
      <tbody id="unc-summary"></tbody>
    </table>
  </div>
</div>

<div class="section" id="s12" data-page="empyrean">
  <div class="section-num">05</div>
  <div class="section-title">Fitted Orbit and Covariance &mdash; vs References</div>
  <div class="section-desc">
    Empyrean's fit vs the SBDB (or find_orb) reference in Keplerian element space,
    compared both directions (each propagated to the other's epoch via STM). Rows are
    tagged by a pathology fingerprint:
    <ul style="margin-top:0.4em;">
      <li><b>CONSISTENT</b> (σ<sub>equiv</sub> &lt; 1): ellipsoids agree within their joint uncertainty.</li>
      <li><b>CORRELATION</b> (joint d² &gt;&gt; marginal d²): off-diagonal correlation drives the discrepancy (e.g. short-arc a/e degeneracy).</li>
      <li><b>ROTATION</b> (principal-axis rotation &gt; 30°): ellipsoids oriented differently, often across a non-linear region.</li>
      <li><b>high σ<sub>equiv</sub>, unlabeled</b>: scale-only disagreement (missing perturber, weight mismatch) or a genuine fit difference; directionally asymmetric values flag chaotic-capture geometry.</li>
    </ul>
  </div>
  <div id="oc-empty" class="section-desc" style="display:none; color:#8b9198">No orbit-comparison rows in this report (no SBDB or find_orb sidecar found).</div>
  <div id="oc-content">
    <div class="panel-title">Per-pair comparison</div>
    <div class="section-desc" style="font-size:0.85em">Click a row to expand a per-element z-score breakdown (Δ_k / σ_combined,k). Bands at ±1σ and ±3σ.</div>
    <div class="heatmap-container">
      <table class="od-table">
        <thead><tr>
          <th class="obj" style="text-align:left">Object</th>
          <th>Reference</th>
          <th>Common epoch<br/>(MJD TDB)</th>
          <th>Δa (AU)</th>
          <th>Δe</th>
          <th>Δi (°)</th>
          <th>ΔΩ (°)</th>
          <th>Δω (°)</th>
          <th>ΔM (°)</th>
          <th>σ_emp(a)<br/><small style="font-weight:300; opacity:0.6">marginal, AU</small></th>
          <th>σ_ref(a)<br/><small style="font-weight:300; opacity:0.6">marginal, AU</small></th>
          <th>σ_emp(a)/σ_ref(a)<br/><small style="font-weight:300; opacity:0.6">ratio of marginal a-σ only; not a norm</small></th>
          <th>d²_marginal</th>
          <th>d²_combined</th>
          <th>joint/marg</th>
          <th>σ_equiv</th>
          <th>rot θ (°)</th>
          <th>eig spec (emp vs ref)</th>
        </tr></thead>
        <tbody id="orbit-compare-tbody"></tbody>
      </table>
    </div>
    <div class="section-desc" style="font-size:0.85em; margin-top:0.8em">Every pair references the JPL SBDB solution, propagated to the common epoch via the fitted-covariance STM (the per-row epoch source is shown in the table above). Where an object's reference covariance is non-SPD at the common epoch, the Mahalanobis metric is undefined (the Cholesky factor does not exist), so that row is blanked (—); no regularisation is applied.</div>
  </div>
</div>

{provenance_footer_html}

<script>
const results = RESULTS_JSON;
const popColors = POP_COLORS_JSON;
const channelColors = CHANNEL_COLORS_JSON;
const AU_KM = 149597870.700;
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
    empyrean: {{ label: 'Empyrean', color: '#5b9bd5', truth: false }},
    jpl:      {{ label: 'JPL', color: '#3d9a6d', truth: true }},
    assist:   {{ label: 'ASSIST', color: '#c77dff', truth: false }},
    oorb:     {{ label: 'OpenOrb', color: '#e8a040', truth: false }},
    findorb:  {{ label: 'find_orb', color: '#d05040', truth: false }},
    orbfit:   {{ label: 'OrbFit', color: '#7dd3c0', truth: false }},
    layup:    {{ label: 'layup', color: '#e07bc0', truth: false }},
}};
function toolLabel(t) {{ return (TOOLS[t] && TOOLS[t].label) || t; }}
function toolColor(t) {{ return (TOOLS[t] && TOOLS[t].color) || '#888'; }}

// Tools actually present in this run — empyrean + horizons are structural
// (reference + truth); externals appear only if a row carries their fields.
const TOOLS_PRESENT = (() => {{
    const present = new Set(['empyrean', 'jpl']);
    const probe = {{
        assist:  r => r.assist_vs_horizons_km != null || r.emp_vs_assist_km != null,
        oorb:    r => r.oorb_vs_horizons_km != null || r.oorb_separation_arcsec != null,
        findorb: r => r.findorb_rms_residual != null || r.findorb_vs_horizons_km != null || r.findorb_d_ra_arcsec != null,
        orbfit:  r => r.orbfit_rms_arcsec != null,
        layup:   r => r.layup_reduced_chi2 != null || r.layup_converged != null,
    }};
    for (const r of results) {{
        for (const t in probe) {{ if (probe[t](r)) present.add(t); }}
    }}
    return present;
}})();

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
}};
// Which (tool, axis) actually have data in THIS run — a tool may be
// structurally capable of an axis (TOOL_AXES) yet carry no rows for it (e.g.
// OpenOrb ran propagation but not ephemeris here). Intersecting the two keeps
// the selector from offering pairs that would draw an empty chart.
const TOOL_AXIS_DATA = (() => {{
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
    }}
    return has;
}})();
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
    return null;
}}
function ephSepArcsec(row, t1, t2) {{
    const a = ephOffsets(row, t1), b = ephOffsets(row, t2);
    if (!a || !b) return null;
    const dra = a[0] - b[0], ddec = a[1] - b[1];
    return Math.sqrt(dra * dra + ddec * ddec);
}}

// Per-tool absolute scalars.
function toolTimeMs(row, tool) {{ return ({{ empyrean: row.emp_time_ms, assist: row.assist_time_ms, oorb: row.oorb_time_ms }})[tool] ?? null; }}
function odRms(row, tool) {{ return ({{ empyrean: row.od_rms_combined_arcsec, findorb: row.findorb_rms_residual, orbfit: row.orbfit_rms_arcsec }})[tool] ?? null; }}
function odReducedChi2(row, tool) {{ return ({{ empyrean: row.od_reduced_chi2, layup: row.layup_reduced_chi2, jpl: row.ref_od_reduced_chi2 }})[tool] ?? null; }}
function odNobs(row, tool) {{ return ({{ empyrean: row.n_obs_used, findorb: row.findorb_n_obs_used, orbfit: row.orbfit_n_obs_used, layup: row.layup_n_obs_used, jpl: row.ref_od_n_obs_used }})[tool] ?? null; }}

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
// Does the current pair have Mahalanobis-σ data on this axis?
function sigmaAvailable(axis) {{
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
        const ok = t => t === 'empyrean' || t === 'jpl' || t === 'oorb' || t === 'findorb';
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

function populateToolSelects() {{
    const order = ['empyrean', 'jpl', 'assist', 'layup', 'findorb', 'orbfit', 'oorb'];
    const opts = order.filter(t => TOOLS_PRESENT.has(t));
    for (const id of ['tool1-select', 'tool2-select']) {{
        const sel = document.getElementById(id);
        if (!sel) continue;
        sel.innerHTML = opts.map(t => `<option value="${{t}}">${{toolLabel(t)}}</option>`).join('');
    }}
    const s1 = document.getElementById('tool1-select'), s2 = document.getElementById('tool2-select');
    if (s1) s1.value = TOOL1;
    if (s2) s2.value = TOOL2;
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
    const cap = document.getElementById('tool-caption');
    if (cap) cap.innerHTML = `comparing <b style="color:${{toolColor(TOOL1)}}">${{toolLabel(TOOL1)}}</b> vs <b style="color:${{toolColor(TOOL2)}}">${{toolLabel(TOOL2)}}</b>`;
    for (const fn of TOOL_RENDERERS) {{ try {{ fn(); }} catch (e) {{ console.error('tool renderer failed', e); }} }}
}}

function wireToolSelector() {{
    populateToolSelects();
    const t1 = document.getElementById('tool1-select'), t2 = document.getElementById('tool2-select');
    const swap = document.getElementById('tool-swap');
    // Picking a tool that's already selected on the other side would
    // collapse every panel to a self-pair placeholder — auto-swap the
    // other dropdown to the previous value so t1 and t2 stay distinct.
    if (t1) t1.onchange = () => {{
        const prev = TOOL1; TOOL1 = t1.value;
        if (TOOL1 === TOOL2) {{ TOOL2 = prev; if (t2) t2.value = TOOL2; }}
        applyToolSelection();
    }};
    if (t2) t2.onchange = () => {{
        const prev = TOOL2; TOOL2 = t2.value;
        if (TOOL2 === TOOL1) {{ TOOL1 = prev; if (t1) t1.value = TOOL1; }}
        applyToolSelection();
    }};
    if (swap) swap.onclick = () => {{
        const a = TOOL1; TOOL1 = TOOL2; TOOL2 = a;
        if (t1) t1.value = TOOL1;
        if (t2) t2.value = TOOL2;
        applyToolSelection();
    }};
    applyToolSelection();
}}

// ─────────── top-level page switch (Tool Comparison / Empyrean Internals) ──
// Sections carry data-page ('comparison' default, 'empyrean', or 'both'); the
// tool-pair selector belongs to the comparison page. Empyrean-internal panels
// (multichannel fidelity, Jet1-vs-f64 timing/uncertainty, non-grav, covariance)
// live on their own page and are not gated by the tool-pair selection.
function showPage(page) {{
    // Three views over two DOM pages. 'overview' and 'comparison' (Advanced)
    // share the comparison sections: data-view="overview" shows only on the
    // Overview tab, data-view="both" on Overview AND Advanced, no data-view on
    // Advanced only. body.view-overview swaps prose for one-line captions.
    document.body.classList.toggle('view-overview', page === 'overview');
    document.querySelectorAll('.section').forEach(s => {{
        const dp = s.getAttribute('data-page') || 'comparison';
        const dv = s.getAttribute('data-view');
        let show;
        if (page === 'empyrean') {{
            show = dp === 'empyrean' || dp === 'both';
        }} else if (page === 'comparison') {{
            show = (dp === 'comparison' || dp === 'both') && dv !== 'overview';
        }} else {{
            show = (dp === 'comparison' || dp === 'both') && (dv === 'overview' || dv === 'both');
        }}
        s.classList.toggle('page-hidden', !show);
    }});
    document.querySelectorAll('#page-nav .page-tab').forEach(b => {{ const on = b.dataset.page === page; b.classList.toggle('active', on); b.setAttribute('aria-current', on ? 'page' : 'false'); }});
    // Deep link so a shared URL lands on the right tab.
    const hash = page === 'comparison' ? '#advanced' : page === 'empyrean' ? '#internals' : '';
    try {{ history.replaceState(null, '', hash || location.pathname.split('/').pop()); }} catch (e) {{}}
    // Plotly fixes a chart's width at newPlot time; a chart drawn while its
    // container was hidden (or on a since-resized window) is stale once
    // revealed — re-fit every now-visible chart on each page switch.
    if (window.Plotly) {{
        document.querySelectorAll('.section:not(.page-hidden) .js-plotly-plot').forEach(gd => {{
            try {{ Plotly.Plots.resize(gd); }} catch (e) {{}}
        }});
    }}
    window.scrollTo(0, 0);
}}
function wirePageNav() {{
    document.querySelectorAll('#page-nav .page-tab').forEach(b => {{ b.onclick = () => showPage(b.dataset.page); }});
    const initial = location.hash === '#advanced' ? 'comparison'
        : location.hash === '#internals' ? 'empyrean'
        : 'overview';
    showPage(initial);
}}

function downloadJSON() {{
    const blob = new Blob([JSON.stringify(results, null, 2)], {{type: 'application/json'}});
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url; a.download = 'validation_results.json';
    document.body.appendChild(a); a.click(); document.body.removeChild(a);
    URL.revokeObjectURL(url);
}}

// ─────────── helpers ───────────
function pct(arr, p) {{ if (!arr.length) return NaN; const s = arr.slice().sort((a,b)=>a-b); return s[Math.min(s.length-1, Math.floor(s.length*p))]; }}
function median(arr) {{ return pct(arr, 0.5); }}
function vecDrKm(a, b) {{ if (!a || !b) return null; const dx = a[0]-b[0], dy = a[1]-b[1], dz = a[2]-b[2]; return Math.sqrt(dx*dx+dy*dy+dz*dz)*AU_KM; }}
function uniq(arr) {{ return [...new Set(arr)]; }}
function popLabel(p) {{ return p; }}

const baseLayout = {{
    paper_bgcolor: '#151b23', plot_bgcolor: '#151b23',
    font: {{ family: 'JetBrains Mono', size: 10, color: '#8b9198' }},
    legend: {{ font: {{ size: 9 }}, bgcolor: 'rgba(0,0,0,0)' }},
    margin: {{ l: 60, r: 20, t: 50, b: 50 }},
    hovermode: 'closest',
}};
function ax(title, type) {{
    const a = {{ title: {{ text: title, font: {{ size: 10 }} }}, gridcolor: 'rgba(91, 155, 213, 0.15)', zerolinecolor: '#1a2332', color: '#8b9198' }};
    if (type) a.type = type;
    return a;
}}

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
const propBase = (() => {{
    const c = results.filter(r => r.channel === 'core' && r.test_type === 'propagation');
    return c.length ? c : propResults;
}})();
// §03 sub-mode: anything but 'all' is the selected tool1-vs-tool2 pair view;
// 'all' is Empyrean's cross-channel overlay (an Empyrean-only concept).
let s03Mode = 'pair';
function buildErrorGrowth(mode) {{
    s03Mode = mode || s03Mode;
    const isAll = s03Mode === 'all';
    const traces = [];
    const yLabel = `|${{toolLabel(TOOL1)}} − ${{toolLabel(TOOL2)}}| position (km)`;
    if (!isAll && !legalPair(TOOL1, TOOL2, 'prop_pos')) {{
        Plotly.react('error-growth-chart', [], {{
            ...baseLayout,
            xaxis: {{ visible: false }}, yaxis: {{ visible: false }},
            annotations: [{{ text: `<b>${{toolLabel(TOOL1)}} vs ${{toolLabel(TOOL2)}}</b> has no propagation-position comparison stored.<br>Pick a pair that shares this axis (one side must be Empyrean or JPL Horizons).`, showarrow: false, font: {{ color: '#8b9198', size: 12 }}, x: 0.5, y: 0.5, xref: 'paper', yref: 'paper' }}],
        }}, {{ responsive: true }});
        return;
    }}
    const dts = uniq(propBase.map(r => r.dt_days)).sort((a, b) => a - b);
    const val = r => propPosDiffKm(r, TOOL1, TOOL2);
    if (!isAll) {{
        // Per-population: median + IQR band over objects in that population.
        for (const pop of popNames) {{
            const popObjs = propBase.filter(r => r.population === pop);
            const med = [], lo = [], hi = [], xs = [];
            for (const dt of dts) {{
                const vals = popObjs.filter(r => r.dt_days === dt).map(val).filter(v => v != null);
                if (vals.length === 0) continue;
                xs.push(dt);
                med.push(median(vals));
                lo.push(pct(vals, 0.25));
                hi.push(pct(vals, 0.75));
            }}
            const color = popColors[pop] || '#888';
            traces.push({{
                x: xs.concat(xs.slice().reverse()),
                y: hi.concat(lo.slice().reverse()),
                fill: 'toself', fillcolor: color + '22',
                line: {{ color: 'rgba(0,0,0,0)' }},
                hoverinfo: 'skip', showlegend: false, name: pop,
            }});
            traces.push({{
                x: xs, y: med, mode: 'lines+markers',
                line: {{ color: color, width: 2 }}, marker: {{ size: 5 }},
                name: pop, hovertemplate: `${{pop}} median<br>dt: %{{x}}d<br>%{{y:.4f}} km<extra></extra>`,
            }});
        }}
        // Annotate top-4 outlier objects by max diff.
        const objMax = objectNames.map(name => {{
            const vals = propBase.filter(r => r.object === name).map(val).filter(v => v != null);
            return [name, vals.length ? Math.max(...vals) : 0];
        }}).sort((a, b) => b[1] - a[1]).slice(0, 4);
        for (const [name] of objMax) {{
            const objR = propBase.filter(r => r.object === name && val(r) != null).sort((a, b) => a.dt_days - b.dt_days);
            if (!objR.length) continue;
            const pop = objR[0].population;
            traces.push({{
                x: objR.map(r => r.dt_days), y: objR.map(val),
                mode: 'lines', name: name + ' (outlier)',
                line: {{ color: popColors[pop] || '#888', width: 1, dash: 'dot' }},
                hovertemplate: `${{name}} (${{pop}})<br>dt: %{{x}}d<br>%{{y:.4f}} km<extra></extra>`,
            }});
        }}
    }} else {{
        // 5-channel overlay: each channel's median across all rows at each dt.
        for (const ch of ALL_CHANNELS) {{
            const chR = results.filter(r => r.channel === ch && r.test_type === 'propagation' && r.emp_vs_horizons_km != null);
            const xs = [], ys = [];
            for (const dt of dts) {{
                const vals = chR.filter(r => r.dt_days === dt).map(r => r.emp_vs_horizons_km);
                if (!vals.length) continue;
                xs.push(dt); ys.push(median(vals));
            }}
            const color = channelColors[ch] || '#888';
            traces.push({{
                x: xs, y: ys, mode: 'lines+markers',
                line: {{ color: color, width: 2 }}, marker: {{ size: 5 }},
                name: ch,
                hovertemplate: `${{ch}} median<br>dt: %{{x}}d<br>%{{y:.4f}} km<extra></extra>`,
            }});
        }}
    }}
    Plotly.react('error-growth-chart', traces, {{
        ...baseLayout,
        xaxis: ax('dt (days from epoch)'),
        yaxis: ax(isAll ? 'Position error vs Horizons (km)' : yLabel, 'log'),
        showlegend: true,
        legend: {{ ...baseLayout.legend, orientation: 'h', y: 1.14 }},
    }}, {{ responsive: true, displayModeBar: 'hover', modeBarButtonsToRemove: ['select2d', 'lasso2d', 'autoScale2d', 'toggleSpikelines'] }});
}}
buildErrorGrowth('pair');
// Reset the Empyrean-channels overlay back to the pair view on any pair
// change — the overlay is an Empyrean-internal cross-check and would
// otherwise linger (and be meaningless for a non-Empyrean pair).
onToolChange(() => {{
    s03Mode = 'pair';
    document.querySelectorAll('#s03-toggle button').forEach(b => b.classList.toggle('active', b.dataset.mode === 'rust'));
    buildErrorGrowth('pair');
}});

// ─────────── §02 propagation heatmap + scorecard (registry-driven) ───────────
// Reimplements the log→color mapping in JS (mirroring Rust `fmt_error`) so
// the heatmap can re-render for any legal tool pair.
function logColorKm(km) {{
    if (km == null) return '#161c25';
    if (km <= 0) return '#1a3550';
    const t = Math.max(0, Math.min(1, (Math.log10(km) + 3.0) / 11.5));
    const stops = [[0.00, [13, 30, 60]], [0.26, [91, 155, 213]], [0.43, [200, 175, 70]], [0.61, [220, 110, 55]], [0.78, [200, 60, 60]], [1.00, [180, 30, 110]]];
    let rgb = stops[0][1];
    for (let i = 0; i < stops.length - 1; i++) {{
        const at = stops[i][0], argb = stops[i][1], bt = stops[i + 1][0], brgb = stops[i + 1][1];
        if (t >= at && t <= bt) {{ const f = (t - at) / Math.max(bt - at, 1e-9); rgb = [argb[0] + f * (brgb[0] - argb[0]), argb[1] + f * (brgb[1] - argb[1]), argb[2] + f * (brgb[2] - argb[2])]; break; }}
    }}
    const hx = n => Math.max(0, Math.min(255, Math.floor(n))).toString(16).padStart(2, '0');
    return '#' + hx(rgb[0]) + hx(rgb[1]) + hx(rgb[2]);
}}
function fmtErrorKm(km) {{
    if (km == null) return '---';
    if (km < 0.001) return (km * 1e6).toFixed(1) + ' mm';
    if (km < 1.0) return (km * 1000.0).toFixed(1) + ' m';
    if (km < 1000.0) return km.toFixed(2) + ' km';
    if (km < 1e6) return km.toFixed(0) + ' km';
    return (km / AU_KM).toFixed(3) + ' AU';
}}
function dtLabel(dt) {{
    const sign = dt >= 0 ? '+' : '';
    return Math.abs(dt) >= 365 ? `${{sign}}${{(dt / 365).toFixed(0)}}y` : `${{sign}}${{dt}}d`;
}}
function buildPropHeatmap() {{
    const container = document.getElementById('heatmap-horizons');
    if (!container) return;
    if (!legalPair(TOOL1, TOOL2, 'prop_pos')) {{
        container.innerHTML = `<div class="section-desc" style="color:#8b9198"><b>${{toolLabel(TOOL1)}} vs ${{toolLabel(TOOL2)}}</b> has no propagation-position comparison stored (one side must be Empyrean or JPL Horizons).</div>`;
        return;
    }}
    const sigma = propHeatMode === 'sigma' && sigmaAvailable('prop');
    const tiers = uniq(propBase.map(r => r.force_model)).sort();
    const dts = uniq(propBase.map(r => r.dt_days)).sort((a, b) => a - b);
    const lut = {{}};
    for (const r of propBase) {{ const v = sigma ? mahalanobisProp(r, TOOL1, TOOL2) : propPosDiffKm(r, TOOL1, TOOL2); if (v != null) lut[r.object + '|' + r.dt_days + '|' + r.force_model] = v; }}
    const objs = [], seen = new Set();
    for (const r of propBase) {{ if (!seen.has(r.object)) {{ seen.add(r.object); objs.push([r.object, r.population]); }} }}
    let html = '';
    for (const tier of tiers) {{
        html += '<div class="heatmap-container"><table class="heatmap"><tr><th></th><th></th>';
        for (const dt of dts) html += `<th class="dt-col">${{dtLabel(dt)}}</th>`;
        html += '</tr>';
        const byPop = {{}};
        for (const [name, pop] of objs) {{
            let mx = -Infinity;
            for (const dt of dts) {{ const v = lut[name + '|' + dt + '|' + tier]; if (v != null) mx = Math.max(mx, v); }}
            if (isFinite(mx)) (byPop[pop] = byPop[pop] || []).push([name, mx]);
        }}
        const pops = Object.keys(byPop).sort();
        let firstPop = true;
        for (const pop of pops) {{
            byPop[pop].sort((a, b) => b[1] - a[1]);
            if (!firstPop) html += `<tr><td colspan="${{dts.length + 2}}" style="border-bottom:none; height:6px; background:#0d1117"></td></tr>`;
            firstPop = false;
            const pc = popColors[pop] || '#888';
            for (const [name] of byPop[pop]) {{
                html += `<tr><td class="obj-name">${{name}}</td><td class="pop-tag"><span class="pop-dot" style="background:${{pc}}"></span>${{pop}}</td>`;
                for (const dt of dts) {{
                    const v = lut[name + '|' + dt + '|' + tier];
                    if (v != null) {{
                        const bg = sigma ? sigmaColor(v) : logColorKm(v);
                        const txt = sigma ? fmtSigma(v) : fmtErrorKm(v);
                        const tc = (sigma ? v >= 3 : v >= 100) ? '#e8e8ec' : '#08080a';
                        html += `<td class="cell" style="background:${{bg}};color:${{tc}}" title="${{name}} dt=${{dt}}d: ${{txt}}">${{txt}}</td>`;
                    }} else {{
                        html += '<td class="cell missing" title="not tested / not compared">·</td>';
                    }}
                }}
                html += '</tr>';
            }}
        }}
        html += '</table></div>';
    }}
    if (sigma) {{
        html += `<div class="legend" style="margin-top:8px;">
      <div class="legend-item"><span class="pop-dot" style="background:${{sigmaColor(0.3)}}"></span>&lt; 1σ · inside the 1σ ellipsoid</div>
      <div class="legend-item"><span class="pop-dot" style="background:${{sigmaColor(1)}}"></span>1σ</div>
      <div class="legend-item"><span class="pop-dot" style="background:${{sigmaColor(3)}}"></span>3σ</div>
      <div class="legend-item"><span class="pop-dot" style="background:${{sigmaColor(10)}}"></span>10σ</div>
      <div class="legend-item"><span class="pop-dot" style="background:${{sigmaColor(100)}}"></span>≥ 100σ</div>
      <div class="legend-item" style="color:#8b9198; flex-basis:100%">Mahalanobis distance of the Empyrean&minus;JPL position offset in Empyrean's STM-propagated covariance (synthetic typical-NEO input σ).</div>
    </div>`;
    }} else {{
        html += `<div class="legend" style="margin-top:8px;">
      <div class="legend-item"><span class="pop-dot" style="background:#0d1e3c"></span>&lt; 1 km · sub-keyhole</div>
      <div class="legend-item"><span class="pop-dot" style="background:#5b9bd5"></span>1 km</div>
      <div class="legend-item"><span class="pop-dot" style="background:#c8af46"></span>100 km · lunar orbit</div>
      <div class="legend-item"><span class="pop-dot" style="background:#dc6e37"></span>10⁴ km · GEO / high-Earth-orbit</div>
      <div class="legend-item"><span class="pop-dot" style="background:#c83c3c"></span>10⁶ km · Hill sphere</div>
      <div class="legend-item"><span class="pop-dot" style="background:#b41e6e"></span>≥ 10⁸ km · &gt;1 AU</div>
      <div class="legend-item"><span class="pop-dot" style="background:#161c25; border:1px dashed #778096"></span>not tested / not compared</div>
    </div>`;
    }}
    container.innerHTML = html;
}}
function buildPropScorecard() {{
    const el = document.getElementById('prop-scorecard-pair');
    if (!el) return;
    const vals = legalPair(TOOL1, TOOL2, 'prop_pos') ? propBase.map(r => propPosDiffKm(r, TOOL1, TOOL2)).filter(v => v != null) : [];
    if (!vals.length) {{ el.innerHTML = ''; return; }}
    const s = vals.slice().sort((a, b) => a - b);
    const p = q => s[Math.min(s.length - 1, Math.floor(s.length * q))];
    el.innerHTML = `<div class="summary-grid" style="margin-bottom:10px;">
      <div class="summary-card"><div class="value">${{fmtErrorKm(p(0.5))}}</div><div class="label">Median |Δ|</div></div>
      <div class="summary-card"><div class="value">${{fmtErrorKm(p(0.95))}}</div><div class="label">p95 |Δ|</div></div>
      <div class="summary-card"><div class="value">${{fmtErrorKm(p(1.0))}}</div><div class="label">Max |Δ|</div></div>
      <div class="summary-card"><div class="value">${{vals.length}}</div><div class="label">Cases compared</div></div>
    </div>`;
}}
buildPropHeatmap();
buildPropScorecard();
// Physical / uncertainty-σ toggle — shown only when the pair is Empyrean-vs-JPL
// and a propagated covariance is present (only Empyrean propagates one).
function refreshPropHeatToggle() {{
    const tog = document.getElementById('prop-heat-toggle');
    if (!tog) return;
    const avail = sigmaAvailable('prop');
    tog.style.display = avail ? '' : 'none';
    if (!avail && propHeatMode === 'sigma') {{
        propHeatMode = 'physical';
        tog.querySelectorAll('button').forEach(b => b.classList.toggle('active', b.dataset.mode === 'physical'));
    }}
}}
document.querySelectorAll('#prop-heat-toggle button').forEach(btn => {{
    btn.onclick = () => {{
        document.querySelectorAll('#prop-heat-toggle button').forEach(b => b.classList.remove('active'));
        btn.classList.add('active');
        propHeatMode = btn.dataset.mode;
        buildPropHeatmap();
    }};
}});
refreshPropHeatToggle();
onToolChange(() => {{ refreshPropHeatToggle(); buildPropHeatmap(); buildPropScorecard(); }});
document.querySelectorAll('#s03-toggle button').forEach(btn => {{
    btn.onclick = () => {{
        document.querySelectorAll('#s03-toggle button').forEach(b => b.classList.remove('active'));
        btn.classList.add('active');
        buildErrorGrowth(btn.dataset.mode);
    }};
}});

// ─────────── Section 04: ASSIST comparison ───────────
// The external-tool comparison fields (assist_vs_horizons_km, emp_vs_assist_km,
// assist_time_ms) are merged onto whichever channel is the merge target —
// `core` under WITH_CORE, else `rust` — NOT necessarily the rust rows the prop
// plots key off. So select ASSIST rows by field presence across all channels.
const assistResults = results.filter(r => r.test_type === 'propagation' && r.assist_vs_horizons_km != null);
if (assistResults.length > 0) {{
    document.getElementById('s04').style.display = '';
    document.getElementById('s05').style.display = '';
    // §05 (timing) now lives on the Empyrean Internals page; only §04's TOC
    // entry remains on the comparison page (toc-s05 was removed).
    document.getElementById('toc-s04').style.display = '';
    // Canonical "two tools vs a common truth (Horizons)" panel: plot
    // |TOOL1 − TOOL2| per population against the |TOOL2 − Horizons| reference.
    // Requires a legal prop_pos pair AND a non-Horizons TOOL2 (else the
    // "vs truth" reference axis collapses to |Horizons − Horizons| ≡ 0).
    function buildAssistChart() {{
        if (!(legalPair(TOOL1, TOOL2, 'prop_pos') && TOOL2 !== 'jpl')) {{
            Plotly.react('assist-chart', [], {{
                ...baseLayout,
                xaxis: {{ visible: false }}, yaxis: {{ visible: false }},
                annotations: [{{ text: `<b>${{toolLabel(TOOL1)}} vs ${{toolLabel(TOOL2)}}</b> has no two-tools-vs-Horizons comparison.<br>Pick a pair that shares the propagation-position axis whose second tool is not JPL Horizons.`, showarrow: false, font: {{ color: '#8b9198', size: 12 }}, x: 0.5, y: 0.5, xref: 'paper', yref: 'paper' }}],
            }}, {{ responsive: true }});
            return;
        }}
        const traces = [];
        const pairMarker = {{ symbol: 'circle', size: 6 }};
        const refMarker = {{ symbol: 'diamond-open', size: 6 }};
        const cometObjs = new Set(propResults.filter(r => (r.population || '').toLowerCase().includes('comet') || (r.population || '').toLowerCase().includes('iso')).map(r => r.object));
        // All pair rows across populations — the threshold-line dt fallback
        // when the TOOL2-vs-Horizons reference set is empty (block-scoped `rs`
        // below is not visible where the threshold lines are computed).
        const pairAll = propBase.filter(r => propPosDiffKm(r, TOOL1, TOOL2) != null);
        // Per-population trace for |TOOL1 − TOOL2|
        for (const pop of popNames) {{
            const rs = propBase.filter(r => r.population === pop && propPosDiffKm(r, TOOL1, TOOL2) != null);
            if (!rs.length) continue;
            traces.push({{
                x: rs.map(r => r.dt_days), y: rs.map(r => Math.max(propPosDiffKm(r, TOOL1, TOOL2), 1e-9)),
                mode: 'markers', name: `|${{toolLabel(TOOL1)}}−${{toolLabel(TOOL2)}}| ${{pop}}`,
                marker: {{ ...pairMarker, color: popColors[pop] || '#888',
                    symbol: rs.map(r => cometObjs.has(r.object) ? 'triangle-up' : 'circle') }},
                hovertemplate: '%{{text}}<extra></extra>',
                text: rs.map(r => `${{r.object}} (${{pop}})<br>dt: ${{r.dt_days}}d<br>|${{toolLabel(TOOL1)}}−${{toolLabel(TOOL2)}}|: ${{propPosDiffKm(r, TOOL1, TOOL2).toFixed(4)}} km`),
            }});
        }}
        // TOOL2 vs Horizons reference (lighter) — the common-truth axis.
        const refR = propBase.filter(r => propPosDiffKm(r, TOOL2, 'jpl') != null);
        traces.push({{
            x: refR.map(r => r.dt_days), y: refR.map(r => Math.max(propPosDiffKm(r, TOOL2, 'jpl'), 1e-9)),
            mode: 'markers', name: `|${{toolLabel(TOOL2)}}−${{toolLabel('jpl')}}|`,
            marker: {{ ...refMarker, color: '#5b9bd566' }},
            hovertemplate: '%{{text}}<extra></extra>',
            text: refR.map(r => `${{r.object}} (${{r.population}})<br>dt: ${{r.dt_days}}d<br>|${{toolLabel(TOOL2)}}−${{toolLabel('jpl')}}|: ${{propPosDiffKm(r, TOOL2, 'jpl').toFixed(4)}} km`),
        }});
        // Threshold lines — fall back to the pair rows' dt range when the
        // reference set is empty, so xMin/xMax never collapse to ±Infinity.
        const xr = (refR.length ? refR : pairAll).map(r => r.dt_days);
        const xMin = xr.length ? Math.min(...xr) : 0;
        const xMax = xr.length ? Math.max(...xr) : 0;
        [[1, '1 km'], [100, '100 km'], [AU_KM, '1 AU']].forEach(([y, label]) => {{
            traces.push({{
                x: [xMin, xMax], y: [y, y], mode: 'lines',
                line: {{ color: '#5b9bd530', width: 1, dash: 'dot' }},
                showlegend: false, hoverinfo: 'skip',
                name: label,
            }});
        }});
        Plotly.react('assist-chart', traces, {{
            ...baseLayout,
            xaxis: ax('dt (days from epoch)'),
            yaxis: ax(`|${{toolLabel(TOOL1)}} − ${{toolLabel(TOOL2)}}|  and  |${{toolLabel(TOOL2)}} − ${{toolLabel('jpl')}}|  (km, log)`, 'log'),
            showlegend: true,
            legend: {{ ...baseLayout.legend, orientation: 'h', y: 1.14 }},
        }}, {{ responsive: true, displayModeBar: 'hover', modeBarButtonsToRemove: ['select2d', 'lasso2d', 'autoScale2d', 'toggleSpikelines'] }});
    }}
    buildAssistChart();
    onToolChange(() => buildAssistChart());

    // Summary cards — pair-driven (|tool1 − tool2| conditioned on the second
    // tool's own agreement with Horizons, ≤ 1 km, to exclude shared
    // chaotic-divergence rows where |t1−t2| measures two integrators drifting
    // apart in a chaotic region rather than a tool error). Re-renders on every
    // tool-pair change so it never shows a tool that isn't selected.
    const fmtKm = (v) => {{
        if (v < 1) return (v * 1000).toFixed(2) + ' m';
        if (v < 1000) return v.toFixed(3) + ' km';
        if (v < 1e6) return v.toFixed(0) + ' km';
        return (v / 1.496e8).toFixed(2) + ' AU';
    }};
    function buildAssistCards() {{
        const med = document.getElementById('assist-median');
        const rat = document.getElementById('assist-ratio');
        const rowsEl = document.getElementById('assist-rows');
        if (!med) return;
        const okPair = legalPair(TOOL1, TOOL2, 'prop_pos') && TOOL2 !== 'jpl';
        if (!okPair) {{ med.textContent = '—'; if (rat) rat.textContent = '—'; if (rowsEl) rowsEl.textContent = '—'; return; }}
        const rowSet = propBase.filter(r => propPosDiffKm(r, TOOL1, TOOL2) != null && propPosDiffKm(r, TOOL2, 'jpl') != null);
        const TOL = 1.0;
        const empVals = rowSet.filter(r => propPosDiffKm(r, TOOL2, 'jpl') <= TOL).map(r => propPosDiffKm(r, TOOL1, TOOL2)).sort((a, b) => a - b);
        const ratios = rowSet.filter(r => propPosDiffKm(r, TOOL2, 'jpl') > 0).map(r => propPosDiffKm(r, TOOL1, TOOL2) / propPosDiffKm(r, TOOL2, 'jpl'));
        const q = (a, p) => a.length ? a[Math.floor(a.length * p)] : 0;
        med.innerHTML =
            `<span style="font-size:24px">${{fmtKm(q(empVals, 0.5))}}</span>` +
            `<br/><small style="font-size:9px; color:#8b9198">p99 ${{fmtKm(q(empVals, 0.99))}} · max ${{fmtKm(empVals.length ? empVals[empVals.length - 1] : 0)}}</small>` +
            `<br/><small style="font-size:8px; color:#8b9198">over rows with |${{toolLabel(TOOL2)}}−${{toolLabel('jpl')}}| ≤ 1 km (${{empVals.length}}/${{rowSet.length}}); excludes shared chaotic-divergence rows</small>`;
        if (rat) rat.textContent = ratios.length ? median(ratios).toFixed(3) : '—';
        if (rowsEl) rowsEl.textContent = `${{rowSet.length}}`;
    }}
    buildAssistCards();
    onToolChange(() => buildAssistCards());

    // Section 05: Timing — paired strip per population, split by
    // uncertainty mode. The merge keys on `(object, dt,
    // propagation_uncertainty)` so each empyrean row pairs against
    // the matching ASSIST mode (f64↔single particle, first-order↔6
    // variational particles; ASSIST is first-order only — no
    // second-order counterpart). empyrean's Auto rows map to the
    // first-order ASSIST baseline at merge time and plot on the
    // combined panel without a separate ASSIST trace.
    const renderPanel = (chartId, modes, panelLabel) => {{
        const traces = [];
        for (const m of modes) {{
            const tRows = assistResults.filter(r =>
                r.emp_time_ms != null
                && r.propagation_uncertainty === m.tag
            );
            const empByPop = {{}};
            const assistByPop = {{}};
            for (const pop of popNames) {{
                const ps = tRows.filter(r => r.population === pop);
                if (!ps.length) continue;
                empByPop[pop] = ps.filter(r => r.emp_time_ms != null);
                if (m.showAssist) {{
                    assistByPop[pop] = ps.filter(r => r.assist_time_ms != null);
                }}
            }}
            for (const pop of popNames) {{
                const empPs = empByPop[pop] || [];
                if (empPs.length) {{
                    traces.push({{
                        x: empPs.map(_ => pop),
                        y: empPs.map(r => Math.max(r.emp_time_ms, 1e-3)),
                        mode: 'markers', name: `Empyrean ${{m.label}}`,
                        marker: {{ color: m.empColor, size: 6, symbol: m.empSymbol || 'circle', opacity: 0.85 }},
                        hovertemplate: empPs.map(r => `${{r.object}} dt=${{r.dt_days}}d<br>Empyrean ${{m.label}}: ${{r.emp_time_ms.toFixed(1)}} ms<extra></extra>`),
                        showlegend: false,
                    }});
                }}
                if (m.showAssist) {{
                    const assistPs = assistByPop[pop] || [];
                    if (assistPs.length) {{
                        traces.push({{
                            x: assistPs.map(_ => pop),
                            y: assistPs.map(r => Math.max(r.assist_time_ms, 1e-3)),
                            mode: 'markers', name: `ASSIST ${{m.label}}`,
                            marker: {{ color: m.assistColor, size: 6, symbol: m.assistSymbol || 'diamond-open', opacity: 0.7 }},
                            hovertemplate: assistPs.map(r => `${{r.object}} dt=${{r.dt_days}}d<br>ASSIST ${{m.label}}: ${{r.assist_time_ms.toFixed(1)}} ms<extra></extra>`),
                            showlegend: false,
                        }});
                    }}
                }}
            }}
            // Synthetic legend entries (one per mode × {{emp, assist}}).
            traces.push({{ x:[null], y:[null], mode:'markers',
                marker:{{ color: m.empColor, size:8, symbol: m.empSymbol || 'circle' }},
                name: `Empyrean ${{m.label}}` }});
            if (m.showAssist) {{
                traces.push({{ x:[null], y:[null], mode:'markers',
                    marker:{{ color: m.assistColor, size:8, symbol: m.assistSymbol || 'diamond-open' }},
                    name: `ASSIST ${{m.label}}` }});
            }}
        }}
        Plotly.newPlot(chartId, traces, {{
            ...baseLayout,
            xaxis: {{ ...ax('Population'), categoryorder: 'array', categoryarray: popNames }},
            yaxis: ax('Time (ms, log)', 'log'),
            showlegend: true,
            legend: {{ ...baseLayout.legend, orientation: 'h', y: 1.14 }},
        }}, {{ responsive: true, displayModeBar: 'hover', modeBarButtonsToRemove: ['select2d', 'lasso2d', 'autoScale2d', 'toggleSpikelines'] }});
    }};

    // Panel 1: f64 (single mode, single pair).
    renderPanel('timing-chart', [
        {{ tag: 'f64_no_cov', label: 'f64',
           empColor: '#5b9bd5', assistColor: '#e8a040',
           empSymbol: 'circle', assistSymbol: 'diamond-open',
           showAssist: true }},
    ]);

    // Panel 2: STM/STT-bearing modes — first-order, second-order, Auto.
    // Auto pairs against the same ASSIST first-order STM baseline as
    // the first-order mode via the merge step; we plot only empyrean's
    // Auto here (the ASSIST diamond for Auto would visually overlap
    // first-order ASSIST exactly).
    renderPanel('timing-cov-chart', [
        {{ tag: 'first_order_with_cov', label: 'Jet1 / STM',
           empColor: '#5b9bd5', assistColor: '#e8a040',
           empSymbol: 'circle', assistSymbol: 'diamond-open',
           showAssist: true }},
        {{ tag: 'second_order_with_cov', label: 'Jet2 / STT',
           empColor: '#c0504d',
           empSymbol: 'square',
           showAssist: false }},
        {{ tag: 'auto', label: 'Auto',
           empColor: '#8064a2', assistColor: '#8064a2',
           empSymbol: 'triangle-up', assistSymbol: 'cross-open',
           showAssist: false }},
        {{ tag: 'sigma_point_with_cov', label: 'Sigma-point (120 samples)',
           empColor: '#3d9a6d',
           empSymbol: 'diamond',
           showAssist: false }},
        {{ tag: 'monte_carlo_100_with_cov', label: 'Monte Carlo (100 samples, seeded)',
           empColor: '#e8a040',
           empSymbol: 'x',
           showAssist: false }},
    ]);
}}

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
    // only — not across the first_order_with_cov / f64_no_cov rows that eph
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
function buildEphSep(mode) {{
    s06Mode = mode || s06Mode;
    const isAll = s06Mode === 'all';
    const traces = [];
    const dts = uniq(ephBase.map(r => r.dt_days)).sort((a, b) => a - b);
    if (!isAll && !legalPair(TOOL1, TOOL2, 'eph')) {{
        Plotly.react('eph-sep-chart', [], {{
            ...baseLayout,
            xaxis: {{ visible: false }}, yaxis: {{ visible: false }},
            annotations: [{{ text: `<b>${{toolLabel(TOOL1)}} vs ${{toolLabel(TOOL2)}}</b> has no ephemeris comparison stored.<br>Ephemeris pairs are among Empyrean / OpenOrb / JPL Horizons.`, showarrow: false, font: {{ color: '#8b9198', size: 12 }}, x: 0.5, y: 0.5, xref: 'paper', yref: 'paper' }}],
        }}, {{ responsive: true }});
        return;
    }}
    const val = r => {{ const s = ephSepArcsec(r, TOOL1, TOOL2); return s == null ? null : s * 1000; }};
    if (!isAll) {{
        for (const pop of popNames) {{
            const popObjs = ephBase.filter(r => r.population === pop);
            const med = [], lo = [], hi = [], xs = [];
            for (const dt of dts) {{
                const vals = popObjs.filter(r => r.dt_days === dt).map(val).filter(v => v != null);
                if (!vals.length) continue;
                xs.push(dt); med.push(median(vals)); lo.push(pct(vals, 0.25)); hi.push(pct(vals, 0.75));
            }}
            if (!xs.length) continue;
            const color = popColors[pop] || '#888';
            traces.push({{ x: xs.concat(xs.slice().reverse()), y: hi.concat(lo.slice().reverse()),
                fill: 'toself', fillcolor: color + '22', line: {{ color: 'rgba(0,0,0,0)' }},
                hoverinfo: 'skip', showlegend: false, name: pop }});
            traces.push({{ x: xs, y: med, mode: 'lines+markers',
                line: {{ color: color, width: 2 }}, marker: {{ size: 5 }}, name: pop,
                hovertemplate: `${{pop}} median<br>dt: %{{x}}d<br>%{{y:.3f}} mas<extra></extra>` }});
        }}
        // Reference lines: 1 mas (Gaia floor), 100 mas (CCD residual)
        const xMin = dts[0], xMax = dts[dts.length-1];
        [[1, '1 mas (Gaia)'], [100, '100 mas (CCD)']].forEach(([y, label]) => {{
            traces.push({{ x: [xMin, xMax], y: [y, y], mode: 'lines',
                line: {{ color: '#5b9bd540', width: 1, dash: 'dot' }},
                name: label, showlegend: true }});
        }});
    }} else {{
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
                line: {{ color: color, width: 2 }}, marker: {{ size: 5 }}, name: ch,
                hovertemplate: `${{ch}} median<br>dt: %{{x}}d<br>%{{y:.3f}} mas<extra></extra>` }});
        }}
    }}
    const yl = isAll ? 'Angular separation (mas)' : `|${{toolLabel(TOOL1)}} − ${{toolLabel(TOOL2)}}| angular sep (mas)`;
    Plotly.react('eph-sep-chart', traces, {{
        ...baseLayout,
        xaxis: ax('dt (days from epoch)'),
        yaxis: ax(yl, 'log'),
        showlegend: true, legend: {{ ...baseLayout.legend, orientation: 'h', y: 1.14 }},
    }}, {{ responsive: true, displayModeBar: 'hover', modeBarButtonsToRemove: ['select2d', 'lasso2d', 'autoScale2d', 'toggleSpikelines'] }});
}}
// ─────────── §06 sky-plane heatmap + scorecard (registry-driven) ───────────
// Reimplements the sky-plane log→color mapping and mas formatting in JS so
// the heatmap + scorecard re-render for any legal ephemeris tool pair.
// Same 6 rgb stops as logColorKm, but the
// log axis is anchored to milliarcseconds: t = clamp((log10(mas)+3)/7, 0, 1).
function sepColorMas(mas) {{
    if (mas == null) return '#161c25';
    if (mas <= 0) return '#0d1e3c';
    const t = Math.max(0, Math.min(1, (Math.log10(mas) + 3.0) / 7.0));
    const stops = [[0.00, [13, 30, 60]], [0.26, [91, 155, 213]], [0.43, [200, 175, 70]], [0.61, [220, 110, 55]], [0.78, [200, 60, 60]], [1.00, [180, 30, 110]]];
    let rgb = stops[0][1];
    for (let i = 0; i < stops.length - 1; i++) {{
        const at = stops[i][0], argb = stops[i][1], bt = stops[i + 1][0], brgb = stops[i + 1][1];
        if (t >= at && t <= bt) {{ const f = (t - at) / Math.max(bt - at, 1e-9); rgb = [argb[0] + f * (brgb[0] - argb[0]), argb[1] + f * (brgb[1] - argb[1]), argb[2] + f * (brgb[2] - argb[2])]; break; }}
    }}
    const hx = n => Math.max(0, Math.min(255, Math.floor(n))).toString(16).padStart(2, '0');
    return '#' + hx(rgb[0]) + hx(rgb[1]) + hx(rgb[2]);
}}
function fmtSepMas(mas) {{
    if (mas == null) return '---';
    if (mas <= 0) return '0';
    if (mas < 1.0) return (mas * 1000).toFixed(0) + ' µas';
    if (mas < 1000.0) return mas.toFixed(1) + ' mas';
    return (mas / 1000).toFixed(2) + '"';
}}
function buildEphHeatmap() {{
    const container = document.getElementById('eph-heatmap-pair');
    if (!container) return;
    if (!legalPair(TOOL1, TOOL2, 'eph')) {{
        container.innerHTML = `<div class="section-desc" style="color:#8b9198"><b>${{toolLabel(TOOL1)}} vs ${{toolLabel(TOOL2)}}</b> has no ephemeris comparison stored (pairs are among Empyrean, OpenOrb and JPL Horizons).</div>`;
        return;
    }}
    const tiers = uniq(ephBase.map(r => r.force_model)).sort();
    const dts = uniq(ephBase.map(r => r.dt_days)).sort((a, b) => a - b);
    const sigma = ephHeatMode === 'sigma' && sigmaAvailable('eph');
    const lut = {{}}, spread = {{}};
    for (const r of ephBase) {{
        const k = r.object + '|' + r.dt_days + '|' + r.force_model;
        if (sigma) {{
            const d = mahalanobisSky(r, TOOL1, TOOL2);
            if (d != null) lut[k] = d;
            continue;
        }}
        const s = ephSepArcsec(r, TOOL1, TOOL2);
        if (s == null) continue;
        lut[k] = s * 1000;
        if (r.n_sites > 1) spread[k] = {{ n: r.n_sites, lo: r.sep_min != null ? r.sep_min * 1000 : null, hi: r.sep_max != null ? r.sep_max * 1000 : null }};
    }}
    const objs = [], seen = new Set();
    for (const r of ephBase) {{ if (!seen.has(r.object)) {{ seen.add(r.object); objs.push([r.object, r.population]); }} }}
    let html = '';
    for (const tier of tiers) {{
        html += '<div class="heatmap-container"><table class="heatmap"><tr><th></th><th></th>';
        for (const dt of dts) html += `<th class="dt-col">${{dtLabel(dt)}}</th>`;
        html += '</tr>';
        const byPop = {{}};
        for (const [name, pop] of objs) {{
            let mx = -Infinity;
            for (const dt of dts) {{ const v = lut[name + '|' + dt + '|' + tier]; if (v != null) mx = Math.max(mx, v); }}
            if (isFinite(mx)) (byPop[pop] = byPop[pop] || []).push([name, mx]);
        }}
        const pops = Object.keys(byPop).sort();
        let firstPop = true;
        for (const pop of pops) {{
            byPop[pop].sort((a, b) => b[1] - a[1]);
            if (!firstPop) html += `<tr><td colspan="${{dts.length + 2}}" style="border-bottom:none; height:6px; background:#0d1117"></td></tr>`;
            firstPop = false;
            const pc = popColors[pop] || '#888';
            for (const [name] of byPop[pop]) {{
                html += `<tr><td class="obj-name">${{name}}</td><td class="pop-tag"><span class="pop-dot" style="background:${{pc}}"></span>${{pop}}</td>`;
                for (const dt of dts) {{
                    const v = lut[name + '|' + dt + '|' + tier];
                    if (v != null) {{
                        const bg = sigma ? sigmaColor(v) : sepColorMas(v);
                        const txt = sigma ? fmtSigma(v) : fmtSepMas(v);
                        const tc = (sigma ? v >= 3 : v >= 1.0) ? '#e8e8ec' : '#08080a';
                        const sp = sigma ? null : spread[name + '|' + dt + '|' + tier];
                        const spTxt = sp ? ` · mean of ${{sp.n}} sites${{sp.lo != null ? ' (' + fmtSepMas(sp.lo) + '–' + fmtSepMas(sp.hi) + ')' : ''}}` : '';
                        html += `<td class="cell" style="background:${{bg}};color:${{tc}}" title="${{name}} dt=${{dt}}d: ${{txt}}${{spTxt}}">${{txt}}</td>`;
                    }} else {{
                        html += '<td class="cell missing" title="not tested / not compared">·</td>';
                    }}
                }}
                html += '</tr>';
            }}
        }}
        html += '</table></div>';
    }}
    if (sigma) {{
        html += `<div class="legend" style="margin-top:8px;">
      <div class="legend-item"><span class="pop-dot" style="background:${{sigmaColor(0.3)}}"></span>&lt; 1σ · inside the 1σ ellipse</div>
      <div class="legend-item"><span class="pop-dot" style="background:${{sigmaColor(1)}}"></span>1σ</div>
      <div class="legend-item"><span class="pop-dot" style="background:${{sigmaColor(3)}}"></span>3σ</div>
      <div class="legend-item"><span class="pop-dot" style="background:${{sigmaColor(10)}}"></span>10σ</div>
      <div class="legend-item"><span class="pop-dot" style="background:${{sigmaColor(100)}}"></span>≥ 100σ</div>
      <div class="legend-item" style="color:#8b9198; flex-basis:100%">Mahalanobis distance of the Empyrean&minus;JPL RA/Dec offset in Empyrean's propagated sky-plane covariance (synthetic typical-NEO input σ, averaged over the sites).</div>
    </div>`;
    }} else {{
        html += `<div class="legend" style="margin-top:8px;">
      <div class="legend-item"><span class="pop-dot" style="background:#0d1e3c"></span>&lt; 0.01 mas</div>
      <div class="legend-item"><span class="pop-dot" style="background:#5b9bd5"></span>0.1 mas</div>
      <div class="legend-item"><span class="pop-dot" style="background:#c8af46"></span>1 mas · Gaia floor</div>
      <div class="legend-item"><span class="pop-dot" style="background:#dc6e37"></span>10 mas</div>
      <div class="legend-item"><span class="pop-dot" style="background:#c83c3c"></span>100 mas</div>
      <div class="legend-item"><span class="pop-dot" style="background:#b41e6e"></span>&ge; 1&Prime; (1000 mas)</div>
      <div class="legend-item"><span class="pop-dot" style="background:#161c25; border:1px dashed #778096"></span>not tested / not compared</div>
    </div>`;
    }}
    container.innerHTML = html;
}}
function buildEphScorecard() {{
    const el = document.getElementById('eph-scorecard-pair');
    if (!el) return;
    const vals = legalPair(TOOL1, TOOL2, 'eph') ? ephBase.map(r => {{ const s = ephSepArcsec(r, TOOL1, TOOL2); return s == null ? null : s * 1000; }}).filter(v => v != null) : [];
    if (!vals.length) {{ el.innerHTML = ''; return; }}
    const s = vals.slice().sort((a, b) => a - b);
    const p = q => s[Math.min(s.length - 1, Math.floor(s.length * q))];
    el.innerHTML = `<div class="summary-grid" style="margin-bottom:10px;">
      <div class="summary-card"><div class="value">${{fmtSepMas(p(0.5))}}</div><div class="label">Median sep</div></div>
      <div class="summary-card"><div class="value">${{fmtSepMas(p(0.95))}}</div><div class="label">p95 sep</div></div>
      <div class="summary-card"><div class="value">${{fmtSepMas(p(1.0))}}</div><div class="label">Max sep</div></div>
      <div class="summary-card"><div class="value">${{vals.length}}</div><div class="label">Cases compared</div></div>
    </div>`;
}}

if (ephResults.length > 0) {{
    buildEphSep('pair');
    onToolChange(() => {{
        s06Mode = 'pair';
        document.querySelectorAll('#s06-toggle button').forEach(b => b.classList.toggle('active', b.dataset.mode === 'rust'));
        buildEphSep('pair');
    }});
    buildEphHeatmap();
    buildEphScorecard();
    function refreshEphHeatToggle() {{
        const tog = document.getElementById('eph-heat-toggle');
        if (!tog) return;
        const avail = sigmaAvailable('eph');
        tog.style.display = avail ? '' : 'none';
        if (!avail && ephHeatMode === 'sigma') {{
            ephHeatMode = 'physical';
            tog.querySelectorAll('button').forEach(b => b.classList.toggle('active', b.dataset.mode === 'physical'));
        }}
    }}
    document.querySelectorAll('#eph-heat-toggle button').forEach(btn => {{
        btn.onclick = () => {{
            document.querySelectorAll('#eph-heat-toggle button').forEach(b => b.classList.remove('active'));
            btn.classList.add('active');
            ephHeatMode = btn.dataset.mode;
            buildEphHeatmap();
        }};
    }});
    refreshEphHeatToggle();
    onToolChange(() => {{ refreshEphHeatToggle(); buildEphHeatmap(); buildEphScorecard(); }});
    document.querySelectorAll('#s06-toggle button').forEach(btn => {{
        btn.onclick = () => {{
            document.querySelectorAll('#s06-toggle button').forEach(b => b.classList.remove('active'));
            btn.classList.add('active');
            buildEphSep(btn.dataset.mode);
        }};
    }});

    // ─────────── Section 07: clipped scatter + outliers ───────────
    // Tightened from ±300 to ±100 mas (empyrean-urfu): the prior clip
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
    if (!legalPair(TOOL1, TOOL2, 'eph')) {{
        Plotly.react('eph-scatter-chart', [], {{
            ...baseLayout,
            xaxis: {{ visible: false }}, yaxis: {{ visible: false }},
            annotations: [{{ text: `<b>${{toolLabel(TOOL1)}} vs ${{toolLabel(TOOL2)}}</b> has no ephemeris comparison stored.<br>Ephemeris pairs are among Empyrean / OpenOrb / JPL Horizons.`, showarrow: false, font: {{ color: '#8b9198', size: 12 }}, x: 0.5, y: 0.5, xref: 'paper', yref: 'paper' }}],
        }}, {{ responsive: true }});
        outBody.innerHTML = '';
        return;
    }}
    const cleaned = [], outliers = [];
    for (const r of ephBase) {{
        const o1 = ephOffsets(r, TOOL1), o2 = ephOffsets(r, TOOL2);
        if (!o1 || !o2) continue;
        const dra_mas = (o1[0] - o2[0]) * 1000, ddec_mas = (o1[1] - o2[1]) * 1000;
        const norm = Math.hypot(dra_mas, ddec_mas);
        if (norm <= SCATTER_CLIP_MAS) cleaned.push({{...r, dra_mas, ddec_mas, norm }});
        else outliers.push({{...r, dra_mas, ddec_mas, norm }});
    }}
    const scatterTraces = [];
    for (const pop of popNames) {{
        const rs = cleaned.filter(r => r.population === pop);
        if (!rs.length) continue;
        scatterTraces.push({{
            x: rs.map(r => r.dra_mas), y: rs.map(r => r.ddec_mas),
            name: pop, mode: 'markers',
            marker: {{ color: popColors[pop] || '#888', size: 5, opacity: 0.7 }},
            hovertemplate: '%{{text}}<extra></extra>',
            text: rs.map(r => `${{r.object}} (${{pop}})<br>obs: ${{r.observer}}<br>dt: ${{r.dt_days}}d<br>dRA: ${{r.dra_mas.toFixed(3)}} mas<br>dDec: ${{r.ddec_mas.toFixed(3)}} mas`),
        }});
    }}
    // 1σ and 3σ ellipses on the cleaned cloud (assume zero-mean iid Gaussian).
    if (cleaned.length > 5) {{
        const ras = cleaned.map(r => r.dra_mas), decs = cleaned.map(r => r.ddec_mas);
        const meanRa = ras.reduce((a,b)=>a+b,0)/ras.length;
        const meanDec = decs.reduce((a,b)=>a+b,0)/decs.length;
        const stdRa = Math.sqrt(ras.reduce((s,v)=>s+(v-meanRa)*(v-meanRa),0)/ras.length);
        const stdDec = Math.sqrt(decs.reduce((s,v)=>s+(v-meanDec)*(v-meanDec),0)/decs.length);
        for (const k of [1, 3]) {{
            const t = Array.from({{length: 64}}, (_, i) => 2*Math.PI*i/63);
            scatterTraces.push({{
                x: t.map(a => meanRa + k*stdRa*Math.cos(a)),
                y: t.map(a => meanDec + k*stdDec*Math.sin(a)),
                mode: 'lines',
                line: {{ color: k === 1 ? '#5b9bd5' : '#5b9bd560', width: 1, dash: k === 1 ? 'solid' : 'dot' }},
                name: `${{k}}σ`,
                hoverinfo: 'skip',
            }});
        }}
    }}
    Plotly.react('eph-scatter-chart', scatterTraces, {{
        ...baseLayout,
        xaxis: {{ ...ax(`dRA·cos(δ): ${{toolLabel(TOOL1)}} − ${{toolLabel(TOOL2)}} (mas)`), range: [-SCATTER_CLIP_MAS, SCATTER_CLIP_MAS], zeroline: true }},
        yaxis: {{ ...ax(`dDec: ${{toolLabel(TOOL1)}} − ${{toolLabel(TOOL2)}} (mas)`), range: [-SCATTER_CLIP_MAS, SCATTER_CLIP_MAS], scaleanchor: 'x', zeroline: true }},
        showlegend: true, legend: {{ ...baseLayout.legend }},
    }}, {{ responsive: true, displayModeBar: 'hover', modeBarButtonsToRemove: ['select2d', 'lasso2d', 'autoScale2d', 'toggleSpikelines'] }});

    // Sidecar table of outliers. Show channel so a reviewer can see
    // whether the divergence is in the rust reference or only in one
    // replay channel (P1-15 from the astrodynamicist review). Dedupe
    // by (object, dt, observer, channel, propagation_uncertainty) so
    // the same row doesn't appear repeatedly when the rust feed has
    // multiple uncertainty modes that all share the same residual
    // (P1-8 from pass2).
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
        tr.innerHTML = `<td colspan="7" style="text-align:left; color:#5b9bd5">No rows beyond ±${{SCATTER_CLIP_MAS}} mas — the entire residual cloud is on the chart.</td>`;
        outBody.appendChild(tr);
    }}
    }}
    buildEphScatter();
    onToolChange(() => buildEphScatter());

    // ─────────── Section 08: stacked dRA / dDec residual growth (symlog) ───────────
    function residTraces(idx) {{
        const traces = [];
        const dts = uniq(ephBase.map(r => r.dt_days)).sort((a,b)=>a-b);
        const val = r => {{
            const o1 = ephOffsets(r, TOOL1), o2 = ephOffsets(r, TOOL2);
            if (o1 == null || o2 == null) return null;
            return (o1[idx] - o2[idx]) * 1000;
        }};
        for (const pop of popNames) {{
            const popObjs = ephBase.filter(r => r.population === pop);
            const med = [], lo = [], hi = [], xs = [];
            for (const dt of dts) {{
                const vals = popObjs.filter(r => r.dt_days === dt).map(val).filter(v => v != null);
                if (!vals.length) continue;
                xs.push(dt); med.push(median(vals)); lo.push(pct(vals, 0.25)); hi.push(pct(vals, 0.75));
            }}
            if (!xs.length) continue;
            const color = popColors[pop] || '#888';
            traces.push({{ x: xs.concat(xs.slice().reverse()), y: hi.concat(lo.slice().reverse()),
                fill: 'toself', fillcolor: color + '22', line: {{ color: 'rgba(0,0,0,0)' }},
                hoverinfo: 'skip', showlegend: false }});
            traces.push({{ x: xs, y: med, mode: 'lines+markers',
                line: {{ color: color, width: 2 }}, marker: {{ size: 4 }}, name: pop, showlegend: true,
                hovertemplate: `${{pop}}<br>dt: %{{x}}d<br>median: %{{y:.3f}} mas<extra></extra>` }});
        }}
        return traces;
    }}
    function symlogYAxis(title) {{
        // Plotly doesn't have native symlog. Linear range [-50, 50]
        // mas captures the median behavior of all but the
        // Self-Perturber/2020 CD3 chaotic-divergence rows (which can
        // hit ±1000 mas in IQR). Tightened from [-1000, 1000] (which
        // collapsed every other population into a flat line near zero)
        // per the review-pass2 P1-3 finding. The chaotic outliers
        // remain visible as out-of-band markers at the chart edges,
        // and full-range numbers are in §10's Per-test-type matrix.
        return {{ title: {{ text: title, font: {{ size: 10 }} }}, type: 'linear',
                  range: [-50, 50], gridcolor: 'rgba(91, 155, 213, 0.15)', zerolinecolor: '#5b9bd540', color: '#8b9198', zeroline: true }};
    }}
    function buildEphResidGrowth() {{
        if (!legalPair(TOOL1, TOOL2, 'eph')) {{
            const placeholder = {{
                ...baseLayout,
                xaxis: {{ visible: false }}, yaxis: {{ visible: false }},
                annotations: [{{ text: `<b>${{toolLabel(TOOL1)}} vs ${{toolLabel(TOOL2)}}</b> has no ephemeris comparison stored.<br>Ephemeris pairs are among Empyrean / OpenOrb / JPL Horizons.`, showarrow: false, font: {{ color: '#8b9198', size: 12 }}, x: 0.5, y: 0.5, xref: 'paper', yref: 'paper' }}],
            }};
            Plotly.react('eph-resid-ra-chart', [], placeholder, {{ responsive: true }});
            Plotly.react('eph-resid-dec-chart', [], placeholder, {{ responsive: true }});
            return;
        }}
        Plotly.react('eph-resid-ra-chart', residTraces(0), {{
            ...baseLayout, xaxis: ax('dt (days from epoch)'), yaxis: symlogYAxis(`${{toolLabel(TOOL1)}} − ${{toolLabel(TOOL2)}} dRA·cos(δ) (mas)`),
            showlegend: true, legend: {{ ...baseLayout.legend, orientation: 'h', y: 1.18 }},
        }}, {{ responsive: true, displayModeBar: 'hover', modeBarButtonsToRemove: ['select2d', 'lasso2d', 'autoScale2d', 'toggleSpikelines'] }});
        Plotly.react('eph-resid-dec-chart', residTraces(1), {{
            ...baseLayout, xaxis: ax('dt (days from epoch)'), yaxis: symlogYAxis(`${{toolLabel(TOOL1)}} − ${{toolLabel(TOOL2)}} dDec (mas)`),
            showlegend: true, legend: {{ ...baseLayout.legend, orientation: 'h', y: 1.18 }},
        }}, {{ responsive: true, displayModeBar: 'hover', modeBarButtonsToRemove: ['select2d', 'lasso2d', 'autoScale2d', 'toggleSpikelines'] }});
    }}
    buildEphResidGrowth();
    onToolChange(() => buildEphResidGrowth());
}}

// ─────────── Section 09: OD diagnostics ───────────
if (odRust.length === 0) {{
    document.getElementById('od-empty').style.display = '';
    document.getElementById('od-content').style.display = 'none';
    // The cross-channel OD charts live on the Empyrean Internals page (§10);
    // hide their container too so it doesn't render empty when OD didn't run.
    const xch = document.getElementById('od-xchannel-content');
    if (xch) xch.style.display = 'none';
}} else {{
    // Defensive: belt-and-braces hide the empty-state in the populated
    // branch too — playwright snapshots and some accessibility tools
    // can surface display:none text content as if visible, which made
    // the prior review flag this as an orphan "no data" string sitting
    // next to populated content.
    document.getElementById('od-empty').style.display = 'none';
    // Group rows by (object) across channels.
    const byObj = {{}};
    for (const r of odResultsAll) {{
        // Render the optical+radar OD as a distinct "<obj> +radar" entry so it
        // sits beside the optical-only row with its own find_orb comparison,
        // reusing every downstream table/chart unchanged.
        const key = r.test_type === 'orbit_determination_radar' ? r.object + ' +radar' : r.object;
        if (!byObj[key]) byObj[key] = {{}};
        byObj[key][r.channel] = r;
    }}
    // Pre-sort the overview table by |Δ RMS vs find_orb| descending,
    // with rows that have no find_orb match falling to the bottom. The
    // intent (per the astrodynamicist review): impactor regressions
    // like 2023 CX1 (2200× find_orb) shouldn't be buried in the
    // alphabetical middle of the table.
    const odObjects = Object.keys(byObj).sort((a, b) => {{
        // find_orb (and every external reference) merges onto the core
        // channel — see the Makefile CORE_MERGED input — so the Δ-vs-find_orb
        // sort key must read core, not rust (rust carries no findorb_* fields).
        const ra = byObj[a].core || byObj[a].rust, rb = byObj[b].core || byObj[b].rust;
        const sortKey = (r) => {{
            if (!r || r.findorb_rms_residual == null || r.od_rms_combined_arcsec == null) return -1;
            return Math.abs(r.od_rms_combined_arcsec - r.findorb_rms_residual);
        }};
        const ka = sortKey(ra), kb = sortKey(rb);
        if (ka < 0 && kb < 0) return a.localeCompare(b);
        if (ka < 0) return 1;
        if (kb < 0) return -1;
        return kb - ka;
    }});

    // Per-object summary table — rows already sorted by find_orb Δ desc.
    const overview = document.getElementById('od-overview');
    for (const obj of odObjects) {{
        const row = byObj[obj];
        const rust = row['rust'];
        if (!rust) continue;
        // find_orb comparison fields (findorb_rms_residual / *_n_obs_*) are
        // merged onto the core channel, not rust — read them from `fo`.
        const fo = row['core'] || rust;
        let worstChi2Ratio = 0, worstChi2Channel = '—';
        let worstFitDr = 0, worstFitDrChannel = '—';
        for (const ch of ALL_CHANNELS) {{
            if (ch === 'rust') continue;
            const c = row[ch];
            if (!c) continue;
            if (c.od_chi2 != null && rust.od_chi2 != null && rust.od_chi2 > 0) {{
                const ratio = Math.abs(Math.log10(c.od_chi2 / rust.od_chi2));
                if (ratio > worstChi2Ratio) {{ worstChi2Ratio = ratio; worstChi2Channel = ch; }}
            }}
            const dr = vecDrKm(c.emp_pos_au, rust.emp_pos_au);
            if (dr != null && dr > worstFitDr) {{ worstFitDr = dr; worstFitDrChannel = ch; }}
        }}
        const diverged = worstChi2Ratio > 0.1 || worstFitDr > 1.0;
        const tr = document.createElement('tr');
        if (diverged) tr.classList.add('diverged');
        // empyrean-8l28 + empyrean-ih9f follow-up: when the OD failed
        // explicitly (the OD channel returned a DetermineError), render a
        // visually distinct row with a colspan failure cell rather
        // than a sea of '—' that could be misread as "we just don't
        // have these numbers" instead of "the fit didn't produce
        // them." This makes the explicit-failure rows (Varuna,
        // 2001 QR322, 2024 PT5 in bmpyhksp0) stand out from the
        // converged rows above.
        if (rust.od_converged === false) {{
            tr.style.background = 'rgba(208, 80, 64, 0.10)';
            const notes = rust.notes || '(no notes)';
            tr.innerHTML = `<td class="obj">${{obj}}</td><td>${{rust.n_obs_used || '—'}}</td>` +
                `<td colspan="9" style="text-align:left; color:#e06252; font-style:italic">` +
                `<b>DETERMINE FAILED</b> &nbsp;&middot;&nbsp; ${{notes}}` +
                `</td>`;
            overview.appendChild(tr);
            continue;
        }}
        // Reduced χ² = χ²/ν, where ν = n_obs - 6. Prefer the
        // explicit `od_reduced_chi2` field; fall back to manual
        // computation if missing. Friendlier formatting — scientific
        // notation only past 4 digits; standard decimal for the
        // typical range.
        const fmtChi2 = (v) => (v < 0.01)
            ? v.toExponential(2)
            : (v < 1)
                ? v.toPrecision(2)
                : (v < 10000)
                    ? v.toFixed(v < 10 ? 2 : 0)
                    : v.toExponential(2);
        const reducedChi2 = (rust.od_reduced_chi2 != null)
            ? rust.od_reduced_chi2
            : (rust.od_chi2 != null && rust.n_obs_used && rust.n_obs_used > 6
                ? rust.od_chi2 / (rust.n_obs_used - 6)
                : null);
        const chi2Cell = (reducedChi2 != null) ? fmtChi2(reducedChi2) : '—';
        const rmsCell = (rust.od_rms_ra_arcsec != null && rust.od_rms_dec_arcsec != null)
            ? `${{rust.od_rms_ra_arcsec.toFixed(3)}}″ · ${{rust.od_rms_dec_arcsec.toFixed(3)}}″` : '—';
        const rmsCombinedCell = (rust.od_rms_combined_arcsec != null)
            ? rust.od_rms_combined_arcsec.toFixed(3) + '″' : '—';
        // Annotate missing find_orb rows so the bare em-dash doesn't
        // hide why we don't have a comparison number. The run-time
        // cap in run_findorb.py is 10 min/fixture; large-N_obs cases
        // (Hygiea, Apophis, Eros, etc.) routinely hit it.
        const foRms = (fo.findorb_rms_residual != null)
            ? fo.findorb_rms_residual.toFixed(3) + '″'
            : '— <small style="color:#8b9198">(timeout)</small>';
        // find_orb obs coverage = used / (used + rejected). Sub-50%
        // coverage on a short-arc fit is effectively a convergence
        // failure — find_orb has fallen back to its hard-coded
        // placeholder orbit (a ≈ 2.2 AU, e ≈ 0) and is reporting
        // the residual on the 2 anchor observations as if it were a
        // fit-quality metric. We flag those rows here so the
        // "find_orb RMS" column above can't be read at face value
        // without seeing the coverage qualifier.
        const foUsed = fo.findorb_n_obs_used;
        const foRej = fo.findorb_n_obs_rejected;
        let foCovCell = '—';
        if (foUsed != null && foRej != null) {{
            const total = foUsed + foRej;
            if (total > 0) {{
                const pct = (foUsed / total) * 100;
                let color = '#5b9bd5';      // ≥ 80% green-ish (default)
                let badge = '';
                if (pct < 50) {{
                    color = '#e8a040';
                    badge = pct < 5
                        ? ' <small style="color:#c0504d">(rejection-mismatch)</small>'
                        : ' <small>(low)</small>';
                }}
                foCovCell = `<span style="color:${{color}}">${{foUsed}}/${{total}} <small>(${{pct.toFixed(1)}}%)</small></span>${{badge}}`;
            }}
        }}
        let foDeltaCell = '—';
        if (fo.od_rms_combined_arcsec != null && fo.findorb_rms_residual != null) {{
            const delta = fo.od_rms_combined_arcsec - fo.findorb_rms_residual;
            const sign = delta >= 0 ? '+' : '−';
            // Down-rank the Δ visual when find_orb's coverage is
            // very low — the residual on its rejection-mismatched
            // 2-obs fit isn't comparable to Empyrean's full-arc fit.
            const lowCoverage = (foUsed != null && foRej != null && foUsed + foRej > 0 && foUsed / (foUsed + foRej) < 0.5);
            // Flag large regressions (>=10x worse than find_orb)
            // explicitly so a reviewer can't miss them: this is
            // the right place for the impactor anomaly story
            // (2008 TC3, 2024 BX1, 2023 CX1) to scream.
            const ratio = fo.findorb_rms_residual > 0
                ? fo.od_rms_combined_arcsec / fo.findorb_rms_residual
                : 1;
            let color, suffix;
            if (lowCoverage) {{
                color = '#8b9198';
                suffix = ' <small style="color:#8b9198">(rejection-mismatch; ratio uninterpretable)</small>';
            }} else if (Math.abs(ratio) >= 10) {{
                color = '#e06252';
                suffix = ` <small style="color:#e06252">REGRESSION: ${{ratio.toFixed(0)}}× find_orb</small>`;
            }} else if (Math.abs(delta) > 0.2) {{
                color = '#e8a040';
                suffix = '';
            }} else {{
                color = '#5b9bd5';
                suffix = '';
            }}
            foDeltaCell = `<span style="color:${{color}}">${{sign}}${{Math.abs(delta).toFixed(3)}}″</span>${{suffix}}`;
        }}
        // Combine the two cross-channel OD divergence cells (worst Δχ²
        // ratio + worst ‖Δr_fit‖) into one. When agreement is 1nm-bit-exact
        // across c/cli/python (this run), both subcomponents are 0 and the
        // cell reads "bit-exact 1nm" instead of two em-dashes. When any
        // channel diverges, the cell shows the worst-of-both.
        let xCh = '—';
        if (worstChi2Ratio > 0 || worstFitDr > 0) {{
            const parts = [];
            if (worstChi2Ratio > 0) {{
                parts.push(`<span style="color:${{channelColors[worstChi2Channel] || '#888'}}">${{worstChi2Channel}}</span> χ² 10<sup>${{worstChi2Ratio.toFixed(1)}}</sup>×`);
            }}
            if (worstFitDr > 0) {{
                parts.push(`<span style="color:${{channelColors[worstFitDrChannel] || '#888'}}">${{worstFitDrChannel}}</span> Δr ${{worstFitDr < 1 ? worstFitDr.toFixed(3) : worstFitDr.toFixed(0)}} km`);
            }}
            xCh = parts.join('<br/>');
        }} else {{
            xCh = '<span style="color:#3d9a6d">bit-exact &lt;1nm</span>';
        }}
        // For timeout/missing rows, collapse the three find_orb columns
        // into one span (P1-7 from pass2 review — em-dash trail noise).
        const foBlock = (fo.findorb_rms_residual == null)
            ? `<td class="od-col-findorb" colspan="3" style="color:#8b9198; text-align:center"><i>find_orb: not run (timeout)</i></td>`
            : `<td class="od-col-findorb">${{foRms}}</td><td class="od-col-findorb">${{foCovCell}}</td><td class="od-col-findorb">${{foDeltaCell}}</td>`;
        // layup external OD reference (Smithsonian/CfA, ASSIST-backed,
        // Veres-2017 weighted by default). layup reports a reduced χ², not an
        // arcsec RMS, so it pairs against Empyrean's χ²ᵣ (both ≈1 for a
        // statistically consistent fit). Three states: converged (χ²ᵣ + Δ vs
        // Empyrean), ran-but-not-converged (flag≠0), or not run for this object.
        let layupCell;
        if (fo.layup_reduced_chi2 != null) {{
            const lr = fo.layup_reduced_chi2;
            const d = (reducedChi2 != null) ? (lr - reducedChi2) : null;
            let color = '#5b9bd5';
            if (reducedChi2 != null && reducedChi2 > 0) {{
                const ratio = lr / reducedChi2;
                if (ratio >= 5 || ratio <= 0.2) color = '#e06252';
                else if (ratio >= 2 || ratio <= 0.5) color = '#e8a040';
            }}
            const dTxt = (d != null)
                ? ` <small>(${{d >= 0 ? '+' : '−'}}${{Math.abs(d) < 0.01 ? Math.abs(d).toExponential(1) : Math.abs(d).toFixed(2)}})</small>`
                : '';
            const nTxt = (fo.layup_n_obs_used != null) ? ` title="layup n_obs=${{fo.layup_n_obs_used}}"` : '';
            layupCell = `<span style="color:${{color}}"${{nTxt}}>${{fmtChi2(lr)}}</span>${{dTxt}}`;
        }} else if (fo.layup_converged === false) {{
            layupCell = `<span style="color:#e8a040">non-conv <small>(flag≠0)</small></span>`;
        }} else {{
            layupCell = `<span style="color:#8b9198">— <small>(not run)</small></span>`;
        }}
        // JPL SBDB fit quality — the same JPL solution Horizons propagates.
        // reduced χ² ≈ (SBDB normalized rms)², comparable to Empyrean's χ²ᵣ;
        // n_obs shows the optical count plus radar (+Nr) and arc (Ny) and MPC
        // condition code (U0–9). Not like-for-like: JPL's own weighting /
        // debiasing / rejection, and n_obs includes radar Empyrean's optical
        // arc doesn't fit.
        let jplChi2Cell, jplNobsCell;
        if (fo.ref_od_reduced_chi2 != null) {{
            const jr = fo.ref_od_reduced_chi2;
            const d = (reducedChi2 != null) ? (jr - reducedChi2) : null;
            let color = '#5b9bd5';
            if (reducedChi2 != null && reducedChi2 > 0) {{
                const ratio = jr / reducedChi2;
                if (ratio >= 5 || ratio <= 0.2) color = '#e06252';
                else if (ratio >= 2 || ratio <= 0.5) color = '#e8a040';
            }}
            const dTxt = (d != null)
                ? ` <small>(${{d >= 0 ? '+' : '−'}}${{Math.abs(d) < 0.01 ? Math.abs(d).toExponential(1) : Math.abs(d).toFixed(2)}})</small>`
                : '';
            const rms = fo.ref_od_rms_normalized;
            const prov = `SBDB rms=${{rms != null ? rms.toFixed(3) : '?'}} · soln ${{fo.ref_od_soln_date || '?'}} · ${{fo.ref_od_pe_used || '?'}}/${{fo.ref_od_sb_used || '?'}}`;
            jplChi2Cell = `<span style="color:${{color}}" title="${{prov}}">${{fmtChi2(jr)}}</span>${{dTxt}}`;
        }} else {{
            jplChi2Cell = `<span style="color:#8b9198">—</span>`;
        }}
        if (fo.ref_od_n_obs_used != null) {{
            const radar = (fo.ref_od_n_del_obs_used || 0) + (fo.ref_od_n_dop_obs_used || 0);
            const radarTxt = radar > 0 ? ` <small style="color:#e8a040" title="radar delay+Doppler obs">+${{radar}}r</small>` : '';
            const arcTxt = (fo.ref_od_data_arc_days != null) ? ` <small style="opacity:0.55" title="observed arc">${{(fo.ref_od_data_arc_days / 365.25).toFixed(0)}}y</small>` : '';
            const condTxt = (fo.ref_od_condition_code != null) ? ` <small style="opacity:0.55" title="MPC condition code (0 best – 9 worst)">U${{fo.ref_od_condition_code}}</small>` : '';
            jplNobsCell = `${{fo.ref_od_n_obs_used}}${{radarTxt}}${{arcTxt}}${{condTxt}}`;
        }} else {{
            jplNobsCell = `<span style="color:#8b9198">—</span>`;
        }}
        tr.innerHTML = `<td class="obj">${{obj}}</td><td>${{rust.n_obs_used || '—'}}</td><td>${{chi2Cell}}</td><td>${{rmsCell}}</td><td>${{rmsCombinedCell}}</td><td>${{rust.od_iterations || '—'}}</td><td class="od-col-jpl">${{jplChi2Cell}}</td><td class="od-col-jpl">${{jplNobsCell}}</td><td class="od-col-layup">${{layupCell}}</td>${{foBlock}}<td>${{xCh}}</td>`;
        overview.appendChild(tr);
    }}

    // Selector-aware column emphasis for the OD-external comparison.
    // When TOOL1 is Empyrean and TOOL2 is an external OD tool
    // (findorb / orbfit / layup), highlight that tool's column(s) and
    // de-emphasize the other external columns; when TOOL2 is not an OD
    // tool (e.g. the default Horizons) every column stays neutral. This
    // is emphasis only -- no data is added or removed. Cells and headers
    // carry static od-col-<tool> classes; this pass toggles inline
    // styles on them, so it is robust to the colspan timeout cell and
    // the DETERMINE-FAILED rows (which carry no od-col class).
    function applyOdColumns() {{
        const tbody = document.getElementById('od-overview');
        if (!tbody) return;
        const table = tbody.closest('table');
        if (!table) return;
        const OD_TOOLS = ['findorb', 'orbfit', 'layup', 'jpl'];
        // The external OD comparison follows whichever OD tool is in the
        // selected pair (JPL is the default second tool and carries an OD
        // result from SBDB). If none is (e.g. Empyrean vs ASSIST — ASSIST
        // carries no OD), the external columns are HIDDEN and the table shows
        // Empyrean's own OD quality only, rather than leaking a tool the user
        // didn't select.
        let active = OD_TOOLS.find(t => t === TOOL1 || t === TOOL2) || null;
        // A tool with no column here (e.g. OrbFit, not run) counts as none.
        if (active && table.querySelectorAll('.od-col-' + active).length === 0) active = null;
        for (const tool of OD_TOOLS) {{
            const show = active === tool;
            for (const el of table.querySelectorAll('.od-col-' + tool)) {{
                el.style.display = show ? '' : 'none';
            }}
        }}
        const note = document.getElementById('od-ext-note');
        if (note) note.style.display = active ? 'none' : '';
        // The find_orb-only sub-charts (post-fit RMS overlay + y=x scatter)
        // belong to the find_orb comparison — show them only when find_orb is
        // the selected pair tool.
        const foPanel = document.getElementById('od-rms-vs-findorb-panel');
        if (foPanel) foPanel.style.display = (active === 'findorb') ? '' : 'none';
    }}
    applyOdColumns();
    onToolChange(() => applyOdColumns());

    // Chart 1: chi² per object, paired bars across channels
    // Reduced χ² = χ²/ν (ν = n_obs − 6). Raw χ² penalises large-N
    // fits unfairly; reduced χ² near 1 means the fit residuals match
    // the assumed observation uncertainties; >>1 means residuals are
    // too large for the weights; <<1 (e.g., Apophis at ~0.19, Eros
    // at ~0.22) means residuals are smaller than the weights would
    // predict — usually a sign that obs weights are loose.
    const chi2Traces = [];
    for (const ch of ALL_CHANNELS) {{
        const xs = [], ys = [];
        for (const obj of odObjects) {{
            const r = byObj[obj][ch];
            // Reduced χ² so large-N fits aren't artificially penalised.
            if (r && r.od_reduced_chi2 != null) {{
                xs.push(obj); ys.push(Math.max(r.od_reduced_chi2, 1e-6));
            }} else if (r && r.od_chi2 != null && r.n_obs_used && r.n_obs_used > 6) {{
                xs.push(obj); ys.push(Math.max(r.od_chi2 / (r.n_obs_used - 6), 1e-6));
            }}
        }}
        // Skip channels with no χ² data at all (e.g. c / cli, whose
        // od_reduced_chi2 is null on every row — not yet marshaled
        // through the C ABI / CLI). Emitting an empty named trace just
        // adds a phantom legend entry with nothing plotted.
        if (!xs.length) continue;
        chi2Traces.push({{
            x: xs, y: ys, type: 'bar', name: ch,
            marker: {{ color: channelColors[ch] || '#888' }},
            hovertemplate: `${{ch}}<br>%{{x}}<br>χ² = %{{y:.3e}}<extra></extra>`,
        }});
    }}
    Plotly.newPlot('od-chi2-chart', chi2Traces, {{
        ...baseLayout,
        margin: {{ ...baseLayout.margin, b: 120 }},
        xaxis: {{ ...ax(''), tickangle: -90, tickfont: {{ size: 8 }} }}, yaxis: ax('reduced χ² = χ²/ν (log)', 'log'),
        shapes: [
            {{ type: 'line', xref: 'paper', x0: 0, x1: 1, y0: 1, y1: 1,
              line: {{ color: '#5b9bd540', width: 1, dash: 'dot' }} }},
        ],
        annotations: [
            {{ x: 0.99, y: 1, xref: 'paper', yref: 'y',
              text: 'χ²/ν = 1 (residuals ≈ weights)',
              showarrow: false, font: {{ size: 9, color: '#5b9bd5' }},
              xanchor: 'right' }},
        ],
        showlegend: true, legend: {{ ...baseLayout.legend, orientation: 'h', y: 1.14 }},
    }}, {{ responsive: true, displayModeBar: 'hover', modeBarButtonsToRemove: ['select2d', 'lasso2d', 'autoScale2d', 'toggleSpikelines'] }});

    // Chart 2: iteration count per channel (strip plot)
    const iterTraces = [];
    for (const ch of ALL_CHANNELS) {{
        const xs = [], ys = [], hovers = [], syms = [];
        for (const obj of odObjects) {{
            const r = byObj[obj][ch];
            if (r && r.od_iterations != null) {{
                xs.push(ch); ys.push(r.od_iterations);
                syms.push(r.od_converged ? 'circle' : 'x');
                hovers.push(`${{ch}}<br>${{obj}}: ${{r.od_iterations}} iter${{r.od_converged ? '' : ' (did not converge)'}}`);
            }}
        }}
        iterTraces.push({{
            x: xs, y: ys, mode: 'markers', name: ch,
            marker: {{ color: channelColors[ch] || '#888', size: 8, opacity: 0.75, symbol: syms }},
            text: hovers, hovertemplate: '%{{text}}<extra></extra>',
            showlegend: false,
        }});
    }}
    Plotly.newPlot('od-iter-chart', iterTraces, {{
        ...baseLayout,
        xaxis: {{ ...ax('Channel'), categoryorder: 'array', categoryarray: ALL_CHANNELS }},
        yaxis: ax('iterations (× = did not converge)'),
        showlegend: false,
    }}, {{ responsive: true, displayModeBar: 'hover', modeBarButtonsToRemove: ['select2d', 'lasso2d', 'autoScale2d', 'toggleSpikelines'] }});

    // Chart 3: post-fit RMS per object (rust only — find_orb overlay if present).
    // Hover boxes carry observation accounting on both sides so the
    // reader can see when find_orb has effectively given up: a 0.005″
    // RMS at 0.5% coverage is a degenerate placeholder fit, not a
    // tight fit. Rejecting the majority of observations is in essence
    // a convergence failure even when the tool doesn't admit it.
    const foCoverage = (r) => {{
        if (r == null || r.findorb_n_obs_used == null || r.findorb_n_obs_rejected == null) return null;
        const total = r.findorb_n_obs_used + r.findorb_n_obs_rejected;
        if (total <= 0) return null;
        return {{ used: r.findorb_n_obs_used, total, pct: (r.findorb_n_obs_used / total) * 100 }};
    }};
    const foCovLabel = (c) => c == null ? '' : `<br>obs: ${{c.used}}/${{c.total}} (${{c.pct.toFixed(1)}}%)${{c.pct < 5 ? ' — placeholder' : c.pct < 50 ? ' — low coverage' : ''}}`;
    const fitStatusLabel = (r) => {{
        if (r == null) return '';
        const parts = [];
        if (r.n_obs_used != null) parts.push(`obs used: ${{r.n_obs_used}}`);
        if (r.od_reduced_chi2 != null) parts.push(`χ²ᵣ = ${{r.od_reduced_chi2.toExponential(2)}}`);
        if (r.od_converged === false) parts.push('did NOT converge');
        return parts.length ? '<br>' + parts.join('<br>') : '';
    }};

    // find_orb fields live on the core channel (external-reference merge),
    // so read the find_orb comparison row from core, not rust.
    const foRow = (o) => byObj[o].core || byObj[o].rust;
    function buildOdRmsChart() {{
        const rmsTraces = [
            {{
                x: odObjects, y: odObjects.map(o => (byObj[o].rust && byObj[o].rust.od_rms_ra_arcsec) || null),
                type: 'bar', name: 'Empyrean dRA RMS',
                marker: {{ color: '#5b9bd5' }},
                customdata: odObjects.map(o => fitStatusLabel(byObj[o].rust)),
                hovertemplate: 'Empyrean dRA RMS<br>%{{x}}: %{{y:.3f}}″%{{customdata}}<extra></extra>',
            }},
            {{
                x: odObjects, y: odObjects.map(o => (byObj[o].rust && byObj[o].rust.od_rms_dec_arcsec) || null),
                type: 'bar', name: 'Empyrean dDec RMS',
                marker: {{ color: '#5b9bd5aa' }},
                customdata: odObjects.map(o => fitStatusLabel(byObj[o].rust)),
                hovertemplate: 'Empyrean dDec RMS<br>%{{x}}: %{{y:.3f}}″%{{customdata}}<extra></extra>',
            }},
        ];
        const combinedVals = odObjects.map(o => (byObj[o].rust && byObj[o].rust.od_rms_combined_arcsec) || null);
        if (combinedVals.some(v => v != null)) {{
            rmsTraces.push({{
                x: odObjects, y: combinedVals,
                mode: 'markers', name: 'Empyrean combined RMS',
                marker: {{ color: '#5b9bd5', size: 10, symbol: 'circle', line: {{ color: '#1f4e79', width: 1.5 }} }},
                type: 'scatter',
                customdata: odObjects.map(o => fitStatusLabel(byObj[o].rust)),
                hovertemplate: 'Empyrean combined<br>%{{x}}: %{{y:.3f}}″%{{customdata}}<extra></extra>',
            }});
        }}
        // Overlay find_orb's RMS only when find_orb is the selected pair tool,
        // so an unselected external tool never leaks onto the chart.
        const foVals = odObjects.map(o => (foRow(o) && foRow(o).findorb_rms_residual) || null);
        if (foVals.some(v => v != null) && (TOOL1 === 'findorb' || TOOL2 === 'findorb')) {{
            const foColors = odObjects.map(o => {{
                const cov = foCoverage(foRow(o));
                if (cov == null) return '#e8a040';
                if (cov.pct < 5) return '#c0504d';          // placeholder
                if (cov.pct < 50) return '#dba23c';         // low-coverage
                return '#e8a040';
            }});
            rmsTraces.push({{
                x: odObjects, y: foVals,
                mode: 'markers', name: 'find_orb RMS',
                marker: {{ color: foColors, size: 10, symbol: 'diamond-open', line: {{ width: 2 }} }},
                type: 'scatter',
                customdata: odObjects.map(o => foCovLabel(foCoverage(foRow(o)))),
                hovertemplate: 'find_orb<br>%{{x}}: %{{y:.3f}}″%{{customdata}}<extra></extra>',
            }});
        }}
        Plotly.react('od-rms-chart', rmsTraces, {{
            ...baseLayout, barmode: 'group',
            xaxis: {{ ...ax(''), tickangle: -90, tickfont: {{ size: 8 }} }}, yaxis: ax('Post-fit RMS (arcsec)', 'log'),
            showlegend: true, legend: {{ ...baseLayout.legend, orientation: 'h', y: 1.14 }},
            margin: {{ ...baseLayout.margin, b: 120 }},
        }}, {{ responsive: true, displayModeBar: 'hover', modeBarButtonsToRemove: ['select2d', 'lasso2d', 'autoScale2d', 'toggleSpikelines'] }});
    }}
    buildOdRmsChart();
    onToolChange(() => buildOdRmsChart());

    // Chart 3b: empyrean combined RMS vs find_orb RMS — y=x scatter.
    // Marker color encodes find_orb's obs coverage so the reader can
    // see at a glance when a point is "find_orb gave up and returned
    // the 2.2-AU placeholder" rather than a comparable fit. Hover
    // boxes carry the obs accounting and the fit's reduced χ² on both
    // sides for the same reason.
    const rmsPairs = odObjects
        .map(o => {{
            // find_orb residuals merge onto the core channel; pull the
            // comparison row from core (Empyrean's own RMS is bit-identical
            // across channels, so reading it from core is equivalent).
            const r = byObj[o].core || byObj[o].rust;
            if (!r) return null;
            const s = r.od_rms_combined_arcsec;
            const f = r.findorb_rms_residual;
            if (s == null || f == null || s <= 0 || f <= 0) return null;
            return {{ obj: o, row: r, fit: s, findorb: f }};
        }})
        .filter(p => p != null);
    if (rmsPairs.length > 0) {{
        const xs = rmsPairs.map(p => p.findorb);
        const ys = rmsPairs.map(p => p.fit);
        const lo = Math.min(...xs, ...ys) * 0.5;
        const hi = Math.max(...xs, ...ys) * 2.0;
        // Per-point color: green for high coverage, orange for low,
        // red for placeholder. Buckets match the bar chart so the
        // two visualizations share a vocabulary.
        const colors = rmsPairs.map(p => {{
            const c = foCoverage(p.row);
            if (c == null) return '#5b9bd5';
            if (c.pct < 5) return '#c0504d';
            if (c.pct < 50) return '#dba23c';
            return '#5b9bd5';
        }});
        const hoverData = rmsPairs.map(p => {{
            const c = foCoverage(p.row);
            const sChi = p.row.od_reduced_chi2;
            const lines = [
                `find_orb: ${{p.findorb.toFixed(3)}}″`,
                `Empyrean combined: ${{p.fit.toFixed(3)}}″`,
            ];
            if (sChi != null) lines.push(`Empyrean χ²ᵣ: ${{sChi.toExponential(2)}}`);
            if (p.row.n_obs_used != null) lines.push(`Empyrean obs used: ${{p.row.n_obs_used}}`);
            if (c != null) {{
                const tag = c.pct < 5 ? ' — placeholder' : c.pct < 50 ? ' — low' : '';
                lines.push(`find_orb obs: ${{c.used}}/${{c.total}} (${{c.pct.toFixed(1)}}%)${{tag}}`);
            }}
            return lines.join('<br>');
        }});
        const scatterTraces = [
            {{
                x: [lo, hi], y: [lo, hi],
                mode: 'lines', name: 'y = x',
                line: {{ color: '#888', width: 1, dash: 'dash' }},
                hoverinfo: 'skip',
            }},
            {{
                x: xs, y: ys,
                mode: 'markers+text',
                name: 'Empyrean OD combined RMS',
                marker: {{ color: colors, size: 10, line: {{ color: '#1f4e79', width: 1 }} }},
                text: rmsPairs.map(p => p.obj),
                textposition: 'top center',
                textfont: {{ size: 10 }},
                customdata: hoverData,
                hovertemplate: '<b>%{{text}}</b><br>%{{customdata}}<extra></extra>',
                type: 'scatter',
            }},
        ];
        Plotly.newPlot('od-rms-vs-findorb-chart', scatterTraces, {{
            ...baseLayout,
            xaxis: ax('find_orb RMS (arcsec)', 'log'),
            yaxis: ax('Empyrean OD combined RMS (arcsec)', 'log'),
            showlegend: false,
        }}, {{ responsive: true, displayModeBar: 'hover', modeBarButtonsToRemove: ['select2d', 'lasso2d', 'autoScale2d', 'toggleSpikelines'] }});
    }} else {{
        // No (empyrean_rms, findorb_rms) pairs — find_orb did not
        // converge on enough shared objects this run (typically a
        // find_orb timeout). Replace the would-be-empty 480px Plotly
        // void with an inline placeholder.
        const panel = document.getElementById('od-rms-vs-findorb-panel');
        if (panel) {{
            const chartDiv = document.getElementById('od-rms-vs-findorb-chart');
            if (chartDiv) {{
                chartDiv.style.height = 'auto';
                chartDiv.innerHTML = '<div style="color:#8b9198; font-style:italic; padding:24px 8px;">find_orb did not converge on enough shared objects this run (no paired RMS points).</div>';
            }}
        }}
    }}

    // Chart 4: fitted-state Δr per channel
    const drTraces = [];
    for (const ch of ALL_CHANNELS) {{
        if (ch === 'core') continue;
        const xs = [], ys = [], hovers = [];
        for (const obj of odObjects) {{
            const r = byObj[obj][ch], base = byObj[obj].core;
            if (!r || !base) continue;
            const dr = vecDrKm(r.emp_pos_au, base.emp_pos_au);
            if (dr == null) continue;
            xs.push(obj); ys.push(Math.max(dr, 1e-9));
            hovers.push(`${{ch}}<br>${{obj}}<br>‖Δr_fit‖ = ${{dr < 1 ? dr.toFixed(4) : dr.toFixed(1)}} km`);
        }}
        if (!xs.length) continue;
        drTraces.push({{
            x: xs, y: ys, mode: 'markers', name: ch,
            marker: {{ color: channelColors[ch] || '#888', size: 8, opacity: 0.85 }},
            text: hovers, hovertemplate: '%{{text}}<extra></extra>',
        }});
    }}
    Plotly.newPlot('od-fitdr-chart', drTraces, {{
        ...baseLayout,
        xaxis: {{ ...ax(''), tickangle: -90, tickfont: {{ size: 8 }} }}, yaxis: ax('‖Δr_fitted‖ (km, log)', 'log'),
        showlegend: true, legend: {{ ...baseLayout.legend, orientation: 'h', y: 1.14 }},
        margin: {{ ...baseLayout.margin, b: 120 }},
    }}, {{ responsive: true, displayModeBar: 'hover', modeBarButtonsToRemove: ['select2d', 'lasso2d', 'autoScale2d', 'toggleSpikelines'] }});
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
    const COEFFS = [
        {{ key: 'a1', label: 'A1', sub: 'radial' }},
        {{ key: 'a2', label: 'A2', sub: 'transverse' }},
        {{ key: 'a3', label: 'A3', sub: 'normal' }},
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
                cells += `<td style="text-align:left">${{c.label}} <small style="color:#8b9198">(${{c.sub}})</small></td>`;
                if (!r) {{
                    // Channel produced no non_grav_recovery row at all —
                    // the entire fitted block is missing for this channel.
                    cells += `<td colspan="4" style="text-align:left; color:#e06252; font-style:italic">not recovered (no non-grav row emitted for this channel)</td>`;
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
                    cells += `<td colspan="3" style="text-align:left; color:#e06252; font-style:italic">not recovered (no non-grav covariance — fell back to state-only)</td>`;
                    cells += `<td style="color:#e06252; font-weight:600">FAIL</td>`;
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
                    cells += `<td style="color:#8b9198">n/a</td>`;
                }} else {{
                    const z = (fit - ic) / sig;
                    const zAbs = Math.abs(z);
                    const pass = zAbs <= 3;
                    const zColor = pass ? '#3d9a6d' : '#e06252';
                    cells += `<td style="color:${{zColor}}">${{z >= 0 ? '+' : '−'}}${{zAbs.toFixed(2)}}σ</td>`;
                    cells += `<td style="color:${{zColor}}; font-weight:600">${{pass ? 'PASS' : 'FAIL'}}</td>`;
                }}
                tr.innerHTML = cells;
                overview.appendChild(tr);
            }}
        }}
    }}
}}

// ─────────── Section 10: Channel fidelity charts ───────────
// ECDF of Δr by channel × test_type
const ecdfTraces = [];
for (const ch of ALL_CHANNELS) {{
    if (ch === 'core') continue;
    for (const tt of ['propagation', 'ephemeris', 'orbit_determination']) {{
        const chRows = results.filter(r => r.channel === ch && r.test_type === tt && r.emp_pos_au);
        const drs = [];
        for (const r of chRows) {{
            const coreRow = results.find(rr => rr.channel === 'core' && rr.object === r.object && rr.test_type === tt && rr.dt_days === r.dt_days && rr.observer === r.observer && rr.propagation_uncertainty === r.propagation_uncertainty && rr.force_model === r.force_model);
            if (!coreRow) continue;
            const dr = vecDrKm(r.emp_pos_au, coreRow.emp_pos_au);
            if (dr != null) drs.push(Math.max(dr, 1e-12));
        }}
        if (!drs.length) continue;
        drs.sort((a, b) => a - b);
        const xs = drs;
        const ys = drs.map((_, i) => (i + 1) / drs.length);
        const color = channelColors[ch] || '#888';
        const dash = tt === 'propagation' ? 'solid' : tt === 'ephemeris' ? 'dot' : 'dash';
        ecdfTraces.push({{
            x: xs, y: ys, mode: 'lines', name: `${{ch}} · ${{tt}}`,
            line: {{ color, width: 2, dash }},
            hovertemplate: `${{ch}} · ${{tt}}<br>‖Δr‖ ≤ %{{x:.3e}} km<br>${{(0).toFixed(0)}}%{{y:.1%}}<extra></extra>`,
        }});
    }}
}}
Plotly.newPlot('ecdf-chart', ecdfTraces, {{
    ...baseLayout,
    xaxis: ax('‖Δr_channel − Δr_core‖ (km, log)', 'log'),
    yaxis: ax('CDF'),
    shapes: [
        {{ type: 'line', x0: 1e-10, x1: 1e-10, y0: 0, y1: 1, line: {{ color: '#5b9bd540', width: 1, dash: 'dot' }} }},
    ],
    annotations: [
        {{ x: 1e-10, y: 1.02, xref: 'x', yref: 'paper', text: 'fidelity 1e-10 km', showarrow: false, font: {{ size: 9, color: '#5b9bd5' }} }},
    ],
    showlegend: true, legend: {{ ...baseLayout.legend, orientation: 'h', y: 1.18 }},
}}, {{ responsive: true, displayModeBar: 'hover', modeBarButtonsToRemove: ['select2d', 'lasso2d', 'autoScale2d', 'toggleSpikelines'] }});

// Per-row beeswarm — Δr by channel, faceted by test type via dash/symbol
const beeTraces = [];
for (const ch of ALL_CHANNELS) {{
    if (ch === 'core') continue;
    for (const tt of ['propagation', 'ephemeris', 'orbit_determination']) {{
        const chRows = results.filter(r => r.channel === ch && r.test_type === tt);
        const xs = [], ys = [], hovers = [];
        for (const r of chRows) {{
            const coreRow = results.find(rr => rr.channel === 'core' && rr.object === r.object && rr.test_type === tt && rr.dt_days === r.dt_days && rr.observer === r.observer && rr.propagation_uncertainty === r.propagation_uncertainty && rr.force_model === r.force_model);
            if (!coreRow) continue;
            const dr = vecDrKm(r.emp_pos_au, coreRow.emp_pos_au);
            if (dr == null) continue;
            xs.push(`${{ch}}·${{tt.slice(0, 4)}}`);
            ys.push(Math.max(dr, 1e-12));
            hovers.push(`${{ch}} · ${{tt}}<br>${{r.object}} dt=${{r.dt_days}}d<br>‖Δr‖ = ${{dr.toExponential(2)}} km`);
        }}
        if (!xs.length) continue;
        beeTraces.push({{
            x: xs, y: ys, mode: 'markers', name: `${{ch}}·${{tt}}`,
            marker: {{ color: channelColors[ch] || '#888', size: 4, opacity: 0.5,
                symbol: tt === 'propagation' ? 'circle' : tt === 'ephemeris' ? 'diamond' : 'square' }},
            text: hovers, hovertemplate: '%{{text}}<extra></extra>',
            showlegend: false,
        }});
    }}
}}
Plotly.newPlot('dev-beeswarm-chart', beeTraces, {{
    ...baseLayout,
    xaxis: ax('Channel · test type'),
    yaxis: ax('‖Δr_chan − Δr_core‖ (km, log)', 'log'),
    showlegend: false,
    shapes: [{{ type: 'line', xref: 'paper', x0: 0, x1: 1, y0: 1e-10, y1: 1e-10, line: {{ color: '#5b9bd540', width: 1, dash: 'dot' }} }}],
}}, {{ responsive: true, displayModeBar: 'hover', modeBarButtonsToRemove: ['select2d', 'lasso2d', 'autoScale2d', 'toggleSpikelines'] }});

// Timing vs accuracy
const taTraces = [];
for (const ch of ALL_CHANNELS) {{
    if (ch === 'core') continue;
    const xs = [], ys = [], hovers = [];
    for (const r of results.filter(rr => rr.channel === ch && rr.emp_time_ms != null && rr.emp_pos_au)) {{
        const coreRow = results.find(rr => rr.channel === 'core' && rr.object === r.object && rr.test_type === r.test_type && rr.dt_days === r.dt_days && rr.observer === r.observer && rr.propagation_uncertainty === r.propagation_uncertainty && rr.force_model === r.force_model);
        if (!coreRow || !coreRow.emp_time_ms || coreRow.emp_time_ms <= 0) continue;
        const dr = vecDrKm(r.emp_pos_au, coreRow.emp_pos_au);
        if (dr == null) continue;
        xs.push(r.emp_time_ms / coreRow.emp_time_ms);
        ys.push(Math.max(dr, 1e-12));
        hovers.push(`${{ch}}<br>${{r.object}} (${{r.test_type}}) dt=${{r.dt_days}}<br>time ratio: ${{(r.emp_time_ms / coreRow.emp_time_ms).toFixed(2)}}×<br>‖Δr‖ = ${{dr.toExponential(2)}} km`);
    }}
    taTraces.push({{
        x: xs, y: ys, mode: 'markers', name: ch,
        marker: {{ color: channelColors[ch] || '#888', size: 4, opacity: 0.5 }},
        text: hovers, hovertemplate: '%{{text}}<extra></extra>',
    }});
}}
Plotly.newPlot('timing-acc-chart', taTraces, {{
    ...baseLayout,
    xaxis: ax('time(channel) / time(core)'),
    yaxis: ax('‖Δr‖ (km, log)', 'log'),
    showlegend: true, legend: {{ ...baseLayout.legend, orientation: 'h', y: 1.14 }},
}}, {{ responsive: true, displayModeBar: 'hover', modeBarButtonsToRemove: ['select2d', 'lasso2d', 'autoScale2d', 'toggleSpikelines'] }});

// ─────────── Section 11: Uncertainty cost (Jet1 vs f64) ───────────
// Pair every (object, dt_days, force_model, channel) propagation row's
// Jet1 timing against its f64 sibling and surface the ratio per
// population. Channels that don't yet support covariance on input
// (python / c / cli) emit only f64 rows, so they're skipped here —
// only rust + core have both modes today.
function pairUncertaintyRows(channel) {{
    const propRows = results.filter(r => r.test_type === 'propagation' && r.channel === channel && r.emp_time_ms != null);
    const j1Map = new Map();
    const f6Map = new Map();
    for (const r of propRows) {{
        const k = `${{r.object}}|${{r.dt_days}}|${{r.force_model}}`;
        if (r.propagation_uncertainty === 'first_order_with_cov') j1Map.set(k, r);
        else if (r.propagation_uncertainty === 'f64_no_cov') f6Map.set(k, r);
    }}
    const pairs = [];
    for (const [k, j1] of j1Map) {{
        const f6 = f6Map.get(k);
        if (f6) pairs.push({{ obj: j1.object, dt: j1.dt_days, pop: j1.population, j1_ms: j1.emp_time_ms, f6_ms: f6.emp_time_ms }});
    }}
    return pairs;
}}

const uncRust = pairUncertaintyRows('rust');
const uncCore = pairUncertaintyRows('core');

if (uncRust.length === 0 && uncCore.length === 0) {{
    document.getElementById('unc-empty').style.display = '';
}} else {{
    // Defensive hide (P0-B from review-pass2).
    document.getElementById('unc-empty').style.display = 'none';
    // Per-population paired bar chart (rust channel).
    // empyrean-urfu fix: previously the ratio annotations used data-y
    // coordinates (`y: j1Med[i] * 1.05`) without a `yref` override,
    // which let Plotly's log autorange propagate any NaN/zero in the
    // population medians into a [-1.21, 19.12] log range (≈ 30 billion
    // years at the top of the chart). Filter populations to those
    // with both finite, positive Jet1 and f64 medians, set an explicit
    // y-range anchored to the data, and pin annotations to paper
    // coordinates so autorange ignores their text-Y.
    const popsAll = uniq(uncRust.map(p => p.pop));
    const j1All = popsAll.map(p => median(uncRust.filter(x => x.pop === p).map(x => x.j1_ms)));
    const f6All = popsAll.map(p => median(uncRust.filter(x => x.pop === p).map(x => x.f6_ms)));
    const keepIdx = popsAll.map((_, i) =>
        Number.isFinite(j1All[i]) && Number.isFinite(f6All[i]) && j1All[i] > 0 && f6All[i] > 0
    );
    const popsHere = popsAll.filter((_, i) => keepIdx[i]);
    const j1Med = j1All.filter((_, i) => keepIdx[i]);
    const f6Med = f6All.filter((_, i) => keepIdx[i]);
    const ratios = j1Med.map((v, i) => v / f6Med[i]);
    const allY = [...j1Med, ...f6Med];
    const yLo = Math.max(0.01, Math.min(...allY) * 0.5);
    const yHi = Math.max(...allY) * 2;
    const popBars = [
        {{ x: popsHere, y: f6Med, type: 'bar', name: 'f64 (no covariance)',
            marker: {{ color: '#5b9bd5' }},
            hovertemplate: 'f64 %{{x}}<br>p50 %{{y:.2f}} ms<extra></extra>' }},
        {{ x: popsHere, y: j1Med, type: 'bar', name: 'Jet1 (with covariance)',
            marker: {{ color: '#e8a040' }},
            hovertemplate: 'Jet1 %{{x}}<br>p50 %{{y:.2f}} ms<extra></extra>' }},
    ];
    Plotly.newPlot('unc-pop-chart', popBars, {{
        ...baseLayout, barmode: 'group',
        xaxis: ax('Population'),
        yaxis: {{ ...ax('Median propagation time (ms, log)', 'log'),
            range: [Math.log10(yLo), Math.log10(yHi)] }},
        showlegend: true, legend: {{ ...baseLayout.legend, orientation: 'h', y: 1.14 }},
        annotations: popsHere.map((p, i) => {{
            // Sample count per population — surfaces the n=4 noise
            // floor that makes Self-Perturber's Jet1/f64 = 0.85×
            // physically implausible (per the review-pass2 P1-A).
            const nObjs = uncRust.filter(x => x.pop === p).length;
            const ratioColor = (nObjs < 5 && Math.abs(ratios[i] - 1.0) > 0.15)
                ? '#8b9198'  // grey out tiny-population outliers
                : '#e8a040';
            return {{
                x: p, y: 0.96, xref: 'x', yref: 'paper',
                text: `Jet1/f64 ${{ratios[i].toFixed(2)}}× <sub>n=${{nObjs}}</sub>`,
                showarrow: false, font: {{ size: 9, color: ratioColor }},
            }};
        }}),
        margin: {{ ...baseLayout.margin, b: 80 }},
    }}, {{ responsive: true, displayModeBar: 'hover', modeBarButtonsToRemove: ['select2d', 'lasso2d', 'autoScale2d', 'toggleSpikelines'] }});

    // Per-row paired scatter, rust + core overlaid.
    const scTraces = [];
    for (const [chName, pairs] of [['rust', uncRust], ['core', uncCore]]) {{
        if (!pairs.length) continue;
        scTraces.push({{
            x: pairs.map(p => p.f6_ms),
            y: pairs.map(p => p.j1_ms),
            mode: 'markers',
            name: chName,
            marker: {{ color: channelColors[chName] || '#888', size: 5, opacity: 0.6 }},
            text: pairs.map(p => `${{p.obj}} dt=${{p.dt}}d (${{p.pop}})<br>f64: ${{p.f6_ms.toFixed(2)}} ms<br>Jet1: ${{p.j1_ms.toFixed(2)}} ms<br>ratio: ${{(p.j1_ms / p.f6_ms).toFixed(2)}}×`),
            hovertemplate: '%{{text}}<extra></extra>',
        }});
    }}
    // y = x reference line, plus 1.5× and 2× ratio guides
    const refXs = [0.01, 0.1, 1, 10, 100, 1000, 10000];
    for (const [r, color, label] of [[1, '#5b9bd5', 'y = x (Jet1 = f64)'], [1.5, '#a070d088', '1.5×'], [2, '#d0504088', '2×']]) {{
        scTraces.push({{
            x: refXs, y: refXs.map(x => x * r),
            mode: 'lines',
            line: {{ color, width: 1, dash: r === 1 ? 'solid' : 'dot' }},
            name: label, hoverinfo: 'skip',
        }});
    }}
    Plotly.newPlot('unc-scatter-chart', scTraces, {{
        ...baseLayout,
        xaxis: ax('time without covariance — f64 (ms, log)', 'log'),
        yaxis: ax('time with covariance — Jet1 (ms, log)', 'log'),
        showlegend: true, legend: {{ ...baseLayout.legend, orientation: 'h', y: 1.14 }},
    }}, {{ responsive: true, displayModeBar: 'hover', modeBarButtonsToRemove: ['select2d', 'lasso2d', 'autoScale2d', 'toggleSpikelines'] }});

    // Summary table
    const tbody = document.getElementById('unc-summary');
    function pct(arr, p) {{ if (!arr.length) return NaN; const s = arr.slice().sort((a,b)=>a-b); return s[Math.min(s.length-1, Math.floor(s.length*p))]; }}
    function fmtMs(v) {{ return isFinite(v) ? (v < 1 ? v.toFixed(2) : v.toFixed(1)) + ' ms' : '—'; }}
    function fmtRatio(j, f) {{ return (f > 0) ? (j / f).toFixed(2) + '×' : '—'; }}
    for (const [chName, pairs] of [['rust', uncRust], ['core', uncCore]]) {{
        if (!pairs.length) continue;
        const j1 = pairs.map(p => p.j1_ms), f6 = pairs.map(p => p.f6_ms);
        const j1_50 = pct(j1, 0.5), f6_50 = pct(f6, 0.5);
        const j1_95 = pct(j1, 0.95), f6_95 = pct(f6, 0.95);
        const tr = document.createElement('tr');
        tr.innerHTML = `<td class="obj-name"><span class="pop-dot" style="background:${{channelColors[chName] || '#888'}}"></span>${{chName}}</td>
            <td>${{pairs.length}}</td>
            <td>${{fmtMs(j1_50)}}</td>
            <td>${{fmtMs(f6_50)}}</td>
            <td style="color:#e8a040">${{fmtRatio(j1_50, f6_50)}}</td>
            <td>${{fmtMs(j1_95)}}</td>
            <td>${{fmtMs(f6_95)}}</td>
            <td style="color:#e8a040">${{fmtRatio(j1_95, f6_95)}}</td>`;
        tbody.appendChild(tr);
    }}
}}

// ─────────── Section 12: Fitted orbit + covariance vs references ───────────
const orbitComparisons = ORBIT_COMPARISONS_JSON;
// User-facing relabel — the sidecar embeds "fit" / "sbdb" as the
// `common_epoch_source` enum tags
// name for the OD library; surface as "Empyrean fit" / "SBDB" for the
// reader.
function relabelEpochSource(s) {{
    if (s === 'fit') return 'Empyrean fit';
    if (s === 'sbdb') return 'JPL';
    if (s === 'findorb') return 'find_orb';
    return s;
}}
if (!orbitComparisons.length) {{
    document.getElementById('oc-empty').style.display = '';
    document.getElementById('oc-content').style.display = 'none';
}} else {{
    // Defensive hide (P0-B from review-pass2).
    document.getElementById('oc-empty').style.display = 'none';
    const KEP_LABELS = ['a', 'e', 'i', 'Ω', 'ω', 'M'];
    function fmtSci(x, d=3) {{
        if (x == null || !isFinite(x)) return '<span style="color:#8b9198">—</span>';
        if (x === 0) return '0';
        const absx = Math.abs(x);
        return (absx < 1e-3 || absx >= 1e4) ? x.toExponential(d) : x.toFixed(d);
    }}
    function sigmaEquivColor(s) {{
        if (s == null || !isFinite(s)) return '#8b9198';
        if (s < 1) return '#3d9a6d';   // green
        if (s < 3) return '#e8a040';   // yellow / orange
        return '#e06252';              // red
    }}
    // Pathology classifier — derived in JS from the Rust-emitted
    // diagnostics. CORRELATION: joint d² >> marginal d² (off-diagonal
    // correlation drives the joint metric). ROTATION: large
    // principal-axis rotation with no large marginal-mismatch signal.
    // MIXED: both signatures triggered.
    function pathologyLabel(c) {{
        if (!(c.sigma_equiv_combined != null && isFinite(c.sigma_equiv_combined)) || c.sigma_equiv_combined < 1) return '';
        const ratio = (Number.isFinite(c.mahalanobis_d2_combined_metric)
                       && Number.isFinite(c.mahalanobis_d2_marginal)
                       && c.mahalanobis_d2_marginal > 1e-30)
            ? c.mahalanobis_d2_combined_metric / c.mahalanobis_d2_marginal
            : NaN;
        const theta = c.principal_axis_rotation_deg;
        const isCorr = isFinite(ratio) && ratio > 100;
        const isRot = Number.isFinite(theta) && theta > 30;
        if (isCorr && isRot) return 'MIXED';
        if (isCorr) return 'CORRELATION';
        if (isRot) return 'ROTATION';
        return '';
    }}
    // Tiny inline SVG "eigen-spectrum strip": six log-scale dots
    // overlaying the fit (#d05040) and reference (#5b9bd5) eigenvalues.
    function eigenSpectrumStrip(c) {{
        const es = c.eigenvalues_fit, er = c.eigenvalues_ref;
        if (!es || !er) return '<span style="color:#8b9198">—</span>';
        const all = [...es, ...er].filter(v => isFinite(v) && v > 0);
        if (!all.length) return '<span style="color:#8b9198">—</span>';
        const log = v => (isFinite(v) && v > 0) ? Math.log10(v) : null;
        const lo = Math.min(...all.map(log));
        const hi = Math.max(...all.map(log));
        const span = Math.max(hi - lo, 1.0);
        const W = 140, H = 18, pad = 2;
        const scaleX = v => {{
            const lv = log(v);
            if (lv == null) return pad;
            return pad + (W - 2*pad) * (lv - lo) / span;
        }};
        let svg = `<svg width="${{W}}" height="${{H}}" style="vertical-align:middle">`;
        // Track line.
        svg += `<line x1="${{pad}}" y1="${{H/2}}" x2="${{W-pad}}" y2="${{H/2}}" stroke="#2a3340" stroke-width="1"/>`;
        // Fit (red) dots above center line, reference (blue) below.
        for (const v of es) {{
            const x = scaleX(v);
            svg += `<circle cx="${{x}}" cy="${{H/2 - 4}}" r="2" fill="#d05040"/>`;
        }}
        for (const v of er) {{
            const x = scaleX(v);
            svg += `<circle cx="${{x}}" cy="${{H/2 + 4}}" r="2" fill="#5b9bd5"/>`;
        }}
        svg += `</svg>`;
        return svg;
    }}
    // Expandable per-element z-score bar chart (Δ_k / σ_combined,k).
    function zScoreBars(c) {{
        const W = 360, H = 96, padL = 32, padR = 8, padT = 8, padB = 18;
        const innerW = W - padL - padR, innerH = H - padT - padB;
        const zs = KEP_LABELS.map((_, k) => {{
            const ss = c.sigma_fit[k] || 0, sr = c.sigma_ref[k] || 0;
            const sc = Math.sqrt(ss*ss + sr*sr);
            return (sc > 0 && isFinite(c.delta[k])) ? c.delta[k] / sc : 0;
        }});
        const absMax = Math.max(3.5, ...zs.map(v => Math.abs(v)));
        const yScale = z => padT + innerH/2 - (z / absMax) * (innerH/2);
        const xCenter = k => padL + (k + 0.5) * (innerW / 6);
        const barW = (innerW / 6) * 0.6;
        let svg = `<svg width="${{W}}" height="${{H}}" style="background:#0d1117; border:1px solid #1a2332">`;
        // ±1σ and ±3σ bands.
        const y1pos = yScale(1), y1neg = yScale(-1);
        const y3pos = yScale(3), y3neg = yScale(-3);
        svg += `<rect x="${{padL}}" y="${{y3pos}}" width="${{innerW}}" height="${{y3neg - y3pos}}" fill="#d0504020"/>`;
        svg += `<rect x="${{padL}}" y="${{y1pos}}" width="${{innerW}}" height="${{y1neg - y1pos}}" fill="#3d9a6d20"/>`;
        // Axis ticks at ±1, ±3 (if within range).
        for (const tk of [-3, -1, 0, 1, 3]) {{
            if (Math.abs(tk) > absMax) continue;
            const yy = yScale(tk);
            svg += `<line x1="${{padL}}" y1="${{yy}}" x2="${{W-padR}}" y2="${{yy}}" stroke="#1a2332" stroke-width="1"/>`;
            svg += `<text x="${{padL - 4}}" y="${{yy + 3}}" text-anchor="end" font-size="9" fill="#8b9198">${{tk}}</text>`;
        }}
        // Bars.
        zs.forEach((z, k) => {{
            const x = xCenter(k) - barW/2;
            const y0 = yScale(0);
            const y1 = yScale(z);
            const color = Math.abs(z) > 3 ? '#e06252' : Math.abs(z) > 1 ? '#e8a040' : '#3d9a6d';
            svg += `<rect x="${{x}}" y="${{Math.min(y0, y1)}}" width="${{barW}}" height="${{Math.abs(y1 - y0)}}" fill="${{color}}"/>`;
            svg += `<text x="${{xCenter(k)}}" y="${{H - 5}}" text-anchor="middle" font-size="10" fill="#8b9198">${{KEP_LABELS[k]}}</text>`;
            // z value above bar.
            svg += `<text x="${{xCenter(k)}}" y="${{y1 - 4 + (z < 0 ? 14 : 0)}}" text-anchor="middle" font-size="9" fill="#8b9198">${{z.toFixed(2)}}</text>`;
        }});
        svg += `<text x="${{padL + innerW/2}}" y="${{padT - 2}}" text-anchor="middle" font-size="9" fill="#8b9198">Δ_k / σ_combined,k</text>`;
        svg += `</svg>`;
        return svg;
    }}
    // Sort: primary by σ_equiv descending (largest mismatch first), then
    // by object name. NaN sinks to bottom.
    const rows = orbitComparisons.slice().sort((a, b) => {{
        const sa = Number.isFinite(a.sigma_equiv_combined) ? a.sigma_equiv_combined : -1;
        const sb = Number.isFinite(b.sigma_equiv_combined) ? b.sigma_equiv_combined : -1;
        if (sb !== sa) return sb - sa;
        return a.object.localeCompare(b.object);
    }});
    const tbody = document.getElementById('orbit-compare-tbody');
    rows.forEach((c, idx) => {{
        const tr = document.createElement('tr');
        tr.style.cursor = 'pointer';
        tr.tabIndex = 0;
        tr.setAttribute('role', 'button');
        tr.setAttribute('aria-expanded', 'false');
        tr.setAttribute('aria-label', `Toggle covariance detail for ${{c.object}}`);
        const sigRatio_a = (c.sigma_ref && c.sigma_ref[0] > 0)
            ? (c.sigma_fit[0] / c.sigma_ref[0]) : null;
        const ratio = (Number.isFinite(c.mahalanobis_d2_combined_metric)
                       && Number.isFinite(c.mahalanobis_d2_marginal)
                       && c.mahalanobis_d2_marginal > 1e-30)
            ? c.mahalanobis_d2_combined_metric / c.mahalanobis_d2_marginal
            : null;
        const label = pathologyLabel(c);
        const labelHtml = label ? `<div style="font-size:0.7em; color:#8b9198; margin-top:2px">${{label}}</div>` : '';
        tr.innerHTML = `
            <td class="obj-name">${{c.object}}</td>
            <td>${{c.reference === 'sbdb' ? 'JPL' : c.reference}}</td>
            <td>${{c.common_epoch_mjd_tdb != null ? c.common_epoch_mjd_tdb.toFixed(3) : '&mdash;'}}<br/><span style="font-size:0.75em;color:#8b9198">(${{relabelEpochSource(c.common_epoch_source)}})</span></td>
            <td>${{fmtSci(c.delta[0])}}</td>
            <td>${{fmtSci(c.delta[1])}}</td>
            <td>${{fmtSci(c.delta[2])}}</td>
            <td>${{fmtSci(c.delta[3])}}</td>
            <td>${{fmtSci(c.delta[4])}}</td>
            <td>${{fmtSci(c.delta[5])}}</td>
            <td>${{fmtSci(c.sigma_fit[0])}}</td>
            <td>${{fmtSci(c.sigma_ref[0])}}</td>
            <td>${{fmtSci(sigRatio_a, 2)}}</td>
            <td>${{fmtSci(c.mahalanobis_d2_marginal, 2)}}</td>
            <td>${{fmtSci(c.mahalanobis_d2_combined_metric, 2)}}</td>
            <td>${{fmtSci(ratio, 2)}}</td>
            <td style="color:${{sigmaEquivColor(c.sigma_equiv_combined)}}; font-weight:bold">
              ${{fmtSci(c.sigma_equiv_combined, 2)}}${{labelHtml}}
            </td>
            <td>${{(c.principal_axis_rotation_deg != null && isFinite(c.principal_axis_rotation_deg)) ? c.principal_axis_rotation_deg.toFixed(1) : '—'}}</td>
            <td>${{eigenSpectrumStrip(c)}}</td>`;
        tbody.appendChild(tr);

        // Expandable row with per-element z-score bar chart, hidden by
        // default. Toggled by clicking the parent row.
        const expandRow = document.createElement('tr');
        expandRow.style.display = 'none';
        expandRow.id = `oc-expand-${{idx}}`;
        const expandCell = document.createElement('td');
        expandCell.colSpan = 18;
        expandCell.style.padding = '8px 16px';
        expandCell.style.background = '#0d1117';
        expandCell.innerHTML = `
            <div style="display:flex; gap:24px; align-items:flex-start; flex-wrap:wrap">
                <div>${{zScoreBars(c)}}</div>
                <div style="font-size:0.85em; line-height:1.5em">
                    <b>Pathology</b>: ${{(() => {{
                        if (label) return label;
                        const s = c.sigma_equiv_combined;
                        if (s == null || !isFinite(s)) return 'unknown (σ_equiv missing)';
                        if (s < 1) return `consistent (σ_equiv = ${{s.toFixed(2)}} &lt; 1)`;
                        if (s < 3) return `mild discrepancy (σ_equiv = ${{s.toFixed(2)}}; within 3σ)`;
                        return `${{s.toFixed(1)}}σ discrepancy — neither CORRELATION (joint/marg &gt; 100) nor ROTATION (θ &gt; 30°) triggered; check for missing perturber, weight mismatch, or scale-only disagreement`;
                    }})()}}<br/>
                    <b>d²_marginal</b> = ${{fmtSci(c.mahalanobis_d2_marginal, 2)}}<br/>
                    <b>d²_combined</b> = ${{fmtSci(c.mahalanobis_d2_combined_metric, 2)}}<br/>
                    <b>joint/marg ratio</b> = ${{fmtSci(ratio, 2)}}
                        ${{ratio > 100 ? '<span style="color:#e8a040"> → correlation-driven</span>' : ''}}<br/>
                    <b>principal-axis rotation</b> = ${{(c.principal_axis_rotation_deg != null && isFinite(c.principal_axis_rotation_deg)) ? c.principal_axis_rotation_deg.toFixed(2) + '°' : '—'}}
                        ${{c.principal_axis_rotation_deg > 30 ? '<span style="color:#e8a040"> → ellipsoids rotated</span>' : ''}}<br/>
                    <b>σ_equiv</b> = ${{fmtSci(c.sigma_equiv_combined, 2)}} (6-DOF χ-equivalent)
                </div>
            </div>`;
        expandRow.appendChild(expandCell);
        tbody.appendChild(expandRow);
        const toggleDetail = () => {{
            const hidden = expandRow.style.display === 'none';
            expandRow.style.display = hidden ? '' : 'none';
            tr.setAttribute('aria-expanded', hidden ? 'true' : 'false');
        }};
        tr.onclick = toggleDetail;
        tr.onkeydown = (e) => {{ if (e.key === 'Enter' || e.key === ' ') {{ e.preventDefault(); toggleDetail(); }} }};
    }});
}}

// ─────────── Performance strip (Overview + Advanced) ───────────
// Median wall clock per row per tool, per axis, as log-scaled bars. Absolute
// per-tool numbers — independent of the selected pair, built once.
function buildSpeedStrip() {{
    const el = document.getElementById('speed-strip');
    if (!el) return;
    const fmtT = v => v == null ? '—' : v < 1 ? (v * 1000).toFixed(0) + ' µs' : v < 1000 ? (v < 10 ? v.toFixed(2) : v.toFixed(1)) + ' ms' : (v / 1000).toFixed(1) + ' s';
    const medOf = rows => {{ const v = rows.filter(x => x != null && isFinite(x)); return v.length ? median(v) : null; }};
    // Empyrean timing: rust channel (production wrapper) preferred, core fallback.
    const empT = (tt, extra) => {{
        for (const ch of ['rust', 'core']) {{
            const v = medOf(results.filter(r => r.channel === ch && r.test_type === tt && (!extra || extra(r))).map(r => r.emp_time_ms));
            if (v != null) return v;
        }}
        return null;
    }};
    const extT = (tt, field) => medOf(results.filter(r => r.test_type === tt).map(r => r[field]));
    const CHIP_IN = 'in-process', CHIP_SUB = 'subprocess · spawn + ephem load', CHIP_JAX = 'JAX · per-call JIT floor';
    const KCOL = '#5fb0a5', JCOL = '#9a8cc2';
    const groups = [
        {{ title: 'Propagation — per row', entries: [
            {{ label: 'ASSIST', color: toolColor('assist'), chip: CHIP_IN, v: extT('propagation', 'assist_time_ms') }},
            {{ label: 'kete', color: KCOL, chip: CHIP_IN, v: extT('propagation', 'kete_time_ms') }},
            {{ label: 'Empyrean (f64)', color: toolColor('empyrean'), chip: CHIP_IN, v: empT('propagation', r => r.propagation_uncertainty === 'f64_no_cov') }},
            {{ label: 'Empyrean (+6×6 cov)', color: toolColor('empyrean'), chip: 'in-process · production default', v: empT('propagation', r => r.propagation_uncertainty === 'first_order_with_cov') }},
            {{ label: 'Empyrean (Auto)', color: toolColor('empyrean'), chip: 'in-process · adaptive escalation', v: empT('propagation', r => r.propagation_uncertainty === 'auto') }},
            {{ label: 'Empyrean (Jet2 STT)', color: toolColor('empyrean'), chip: 'in-process · STM + STT, 6+21 partials', v: empT('propagation', r => r.propagation_uncertainty === 'second_order_with_cov') }},
            {{ label: 'Empyrean (σ-point)', color: toolColor('empyrean'), chip: 'in-process · 120 sigma samples', v: empT('propagation', r => r.propagation_uncertainty === 'sigma_point_with_cov') }},
            {{ label: 'Empyrean (MC-100)', color: toolColor('empyrean'), chip: 'in-process · 100 seeded samples', v: empT('propagation', r => r.propagation_uncertainty === 'monte_carlo_100_with_cov') }},
            {{ label: 'OpenOrb', color: toolColor('oorb'), chip: CHIP_SUB, v: extT('propagation', 'oorb_time_ms') }},
            {{ label: 'jorbit', color: JCOL, chip: CHIP_JAX, v: extT('propagation', 'jorbit_time_ms') }},
        ] }},
        {{ title: 'Ephemeris — per row', entries: [
            {{ label: 'kete', color: KCOL, chip: CHIP_IN, v: extT('ephemeris', 'kete_time_ms') }},
            {{ label: 'Empyrean', color: toolColor('empyrean'), chip: CHIP_IN, v: empT('ephemeris', null) }},
            {{ label: 'OpenOrb', color: toolColor('oorb'), chip: CHIP_SUB, v: extT('ephemeris', 'oorb_time_ms') }},
            {{ label: 'jorbit', color: JCOL, chip: CHIP_JAX + ' · Horizons observer query', v: extT('ephemeris', 'jorbit_time_ms') }},
        ] }},
        {{ title: 'Orbit determination — per fit', entries: [
            {{ label: 'Empyrean', color: toolColor('empyrean'), chip: 'in-process · full DC fit', v: empT('orbit_determination', null) }},
            {{ label: 'layup', color: toolColor('layup'), chip: 'cold subprocess · startup-dominated', v: extT('orbit_determination', 'layup_time_ms') }},
            {{ label: 'find_orb', color: toolColor('findorb'), chip: 'subprocess · full astrometry pipeline', v: extT('orbit_determination', 'findorb_time_ms') }},
            {{ label: 'OrbFit', color: toolColor('orbfit'), chip: CHIP_SUB, v: extT('orbit_determination', 'orbfit_time_ms') }},
        ] }},
    ];
    let html = '';
    for (const g of groups) {{
        const live = g.entries.filter(e => e.v != null).sort((a, b) => a.v - b.v);
        if (!live.length) continue;
        const lo = Math.log10(Math.max(live[0].v, 1e-3)) - 0.15;
        const hi = Math.log10(live[live.length - 1].v) + 0.15;
        html += `<div class="speed-group"><div class="speed-group-title">${{g.title}}</div>`;
        for (const e of live) {{
            const w = hi > lo ? Math.max(3, 100 * (Math.log10(Math.max(e.v, 1e-3)) - lo) / (hi - lo)) : 50;
            html += `<div class="speed-row"><div class="speed-label">${{e.label}}</div>` +
                `<div class="speed-track"><div class="speed-bar" style="width:${{w}}%; background:linear-gradient(90deg, ${{e.color}}cc, ${{e.color}}55)"></div></div>` +
                `<div class="speed-val">${{fmtT(e.v)}}</div><div class="speed-chip">${{e.chip}}</div></div>`;
        }}
        html += '</div>';
    }}
    el.innerHTML = html || '<div class="section-desc" style="color:#8b9198">No timing data in this report.</div>';
}}
try {{ buildSpeedStrip(); }} catch (e) {{ console.error('buildSpeedStrip failed', e); }}

// ─────────── Overview hero strip ───────────
// The glanceable verdict for the selected pair: one number per axis, with
// pass/warn badges ONLY for the truth-anchored Empyrean-vs-JPL pair (the
// thresholds are printed on the cards so every badge is auditable).
const HERO_THRESHOLDS = {{ prop_median_km: 1.0, eph_median_mas: 1.0, od_chi2_ratio: 2.0 }};
function buildHero() {{
    const el = document.getElementById('hero-strip');
    if (!el) return;
    const pair = new Set([TOOL1, TOOL2]);
    const isJpl = pair.has('empyrean') && pair.has('jpl');
    const badge = (ok, why) => `<span class="hero-badge ${{ok ? 'pass' : 'warn'}}" title="${{why}}">${{ok ? '✓' : '⚠'}}</span>`;
    // Propagation: median pairwise |Δr| over the full grid.
    const propMed = median(propBase.map(r => propPosDiffKm(r, TOOL1, TOOL2)).filter(v => v != null));
    const propOk = propMed < HERO_THRESHOLDS.prop_median_km;
    const propCard = isFinite(propMed)
        ? `<div class="hero-card"><div class="hl">${{isJpl ? badge(propOk, 'pass: median < ' + HERO_THRESHOLDS.prop_median_km + ' km vs JPL Horizons truth') : ''}}Propagation</div><div class="hv">${{fmtErrorKm(propMed)}}</div><div class="hs">median |Δr| across the grid · threshold ${{HERO_THRESHOLDS.prop_median_km}} km</div></div>`
        : `<div class="hero-card"><div class="hl">Propagation</div><div class="hv">—</div><div class="hs">no shared propagation axis for this pair</div></div>`;
    // Sky-plane: median separation (mas), site-averaged.
    const ephMed = median(ephBase.map(r => {{ const s = ephSepArcsec(r, TOOL1, TOOL2); return s == null ? null : s * 1000; }}).filter(v => v != null));
    const ephOk = ephMed < HERO_THRESHOLDS.eph_median_mas;
    const ephCard = isFinite(ephMed)
        ? `<div class="hero-card"><div class="hl">${{isJpl ? badge(ephOk, 'pass: median < ' + HERO_THRESHOLDS.eph_median_mas + ' mas (≈ Gaia single-frame floor)') : ''}}Sky-plane</div><div class="hv">${{fmtSepMas(ephMed)}}</div><div class="hs">median RA/Dec separation · Gaia floor ≈ 1 mas</div></div>`
        : `<div class="hero-card"><div class="hl">Sky-plane</div><div class="hv">—</div><div class="hs">no shared ephemeris axis for this pair</div></div>`;
    // Orbit fit: reduced-χ² (or arcsec RMS) medians for whichever OD axis the pair shares.
    const odCh = results.some(r => r.channel === 'core') ? 'core' : 'rust';
    const odRows = results.filter(r => r.channel === odCh && r.test_type === 'orbit_determination');
    let odCard = `<div class="hero-card"><div class="hl">Orbit fit</div><div class="hv">—</div><div class="hs">no shared OD axis for this pair</div></div>`;
    if (legalPair(TOOL1, TOOL2, 'od_chi2')) {{
        const a = median(odRows.map(r => odReducedChi2(r, TOOL1)).filter(v => v != null));
        const b = median(odRows.map(r => odReducedChi2(r, TOOL2)).filter(v => v != null));
        if (isFinite(a) && isFinite(b)) {{
            const ratio = a > 0 && b > 0 ? Math.max(a / b, b / a) : Infinity;
            const odOk = ratio <= HERO_THRESHOLDS.od_chi2_ratio;
            odCard = `<div class="hero-card"><div class="hl">${{isJpl ? badge(odOk, 'pass: median χ²ᵣ within ×' + HERO_THRESHOLDS.od_chi2_ratio + ' of the JPL reported fit') : ''}}Orbit fit</div><div class="hv">${{a.toFixed(2)}}</div><div class="hs">median χ²ᵣ · ${{toolLabel(TOOL2)}}: ${{b.toFixed(2)}}</div></div>`;
        }}
    }} else if (legalPair(TOOL1, TOOL2, 'od_rms')) {{
        const a = median(odRows.map(r => odRms(r, TOOL1)).filter(v => v != null));
        const b = median(odRows.map(r => odRms(r, TOOL2)).filter(v => v != null));
        if (isFinite(a) && isFinite(b)) {{
            odCard = `<div class="hero-card"><div class="hl">Orbit fit</div><div class="hv">${{a.toFixed(2)}}″</div><div class="hs">median post-fit RMS · ${{toolLabel(TOOL2)}}: ${{b.toFixed(2)}}″</div></div>`;
        }}
    }}
    // Channels: Rust-computed bit-identical verdict (HERO_CH).
    const chCard = HERO_CH.n > 0
        ? `<div class="hero-card"><div class="hl">${{badge(HERO_CH.pass === HERO_CH.n, 'pass: every replay channel bit-identical to core at ≤ 1e-10 km on every row')}}Channels</div><div class="hv">${{HERO_CH.pass}}/${{HERO_CH.n}}</div><div class="hs">replay channels bit-identical to core (≤ 10⁻¹⁰ km)</div></div>`
        : `<div class="hero-card"><div class="hl">Channels</div><div class="hv">—</div><div class="hs">single channel in this report</div></div>`;
    // Scope.
    const nObj = uniq(results.map(r => r.object)).length;
    const runDate = ((results[0] || {{}}).timestamp || '').slice(0, 10);
    const scopeCard = `<div class="hero-card"><div class="hl">Scope</div><div class="hv">${{nObj}}</div><div class="hs">objects · ${{results.length.toLocaleString()}} rows${{runDate ? ' · run ' + runDate : ''}}</div></div>`;
    el.innerHTML = `
      <div id="hero-title">
        <div style="font-family:var(--ed-font-display); font-weight:700; font-size:22px; color:var(--ed-text-primary)">${{toolLabel(TOOL1)}} <span style="color:var(--ed-text-muted); font-weight:400">vs</span> ${{toolLabel(TOOL2)}}</div>
        <div style="font-family:var(--ed-font-mono); font-size:11px; color:var(--ed-text-muted)">${{isJpl ? 'flagship validation — pass thresholds on each card' : 'exploratory pair — verdicts apply to Empyrean vs JPL only'}}</div>
      </div>
      <div class="hero-cards">${{propCard}}${{ephCard}}${{odCard}}${{chCard}}${{scopeCard}}</div>`;
}}
try {{ buildHero(); }} catch (e) {{ console.error('buildHero failed', e); }}
onToolChange(() => buildHero());

// ─────────── tool-pair selector + page switch: wire + initial state ───────────
// Runs last, after every panel has rendered and registered its renderer. Each
// is isolated so a single wiring failure can't leave the report in a broken
// half-initialized state (page nav must come up even if the selector hiccups).
try {{ wireToolSelector(); }} catch (e) {{ console.error('wireToolSelector failed', e); }}
try {{ wirePageNav(); }} catch (e) {{ console.error('wirePageNav failed', e); }}
</script>
</body>
</html>"##,
        n_prop = n_prop,
        n_eph = n_eph,
        n_od = n_od,
        n_objects = n_objects,
        n_channels = n_channels,
        channels_label = channels_label,
        report_run_date = report_run_date,
        test_epoch_label = test_epoch_label,
        coverage_line = coverage_line,
        n_populations = n_populations,
        n_dt = n_dt,
        pop_legend = pop_legend,
        channel_legend = channel_legend,
        fidelity_threshold = FIDELITY_THRESHOLD,
        fidelity_summary = fidelity_summary,
        per_tt_matrix_html = per_tt_matrix_html,
        channel_table_html = channel_table_html,
        offenders_html = offenders_html,
        quality_summary_html = quality_summary_html,
        provenance_footer_html = provenance_footer_html,
    );

    let html = html.replace("RESULTS_JSON", &results_json);
    let html = html.replace("POP_COLORS_JSON", &pop_colors_json);
    let html = html.replace("CHANNEL_COLORS_JSON", &channel_colors_json);
    let html = html.replace("ORBIT_COMPARISONS_JSON", &orbit_comparisons_json);

    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create output directory: {e}"))?;
    }
    std::fs::write(output, html).map_err(|e| format!("Failed to write report: {e}"))
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
            html.contains("Fitted Orbit and Covariance"),
            "missing section title"
        );
        assert!(
            html.contains(r#""sigma_equiv_combined":14.06"#),
            "comparison data not embedded"
        );
        assert!(html.contains("orbit-compare-tbody"), "table body missing");
        assert!(
            html.contains(r#""mahalanobis_d2_marginal":0.01"#),
            "marginal d² not embedded"
        );
        assert!(
            html.contains(r#""principal_axis_rotation_deg":64.3"#),
            "principal-axis rotation angle not embedded"
        );
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
    fn report_embeds_results_verbatim_and_renders_all_sections() {
        // Contract tripwire (report generalization Phase 0): the embedded
        // RESULTS_JSON must be byte-for-byte `serde_json::to_string(&results)`
        // so generalizing the *renderer* never silently changes the on-disk
        // shape the website + GCS `latest/validation.json` consume. Also
        // asserts every section anchor renders, so a refactor cannot drop a
        // panel unnoticed.
        let rows = vec![
            synthetic_rust_prop_row("Apophis", 0.0),
            synthetic_core_od_row("Apophis"),
        ];
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&rows, &[], &out, None).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();

        // (1) verbatim embed — the exact serialization appears in the page.
        let expected = serde_json::to_string(&rows).unwrap();
        assert!(
            html.contains(&expected),
            "RESULTS_JSON is not the verbatim serde serialization of results",
        );
        // layup data specifically reaches the client (supports the layup
        // report-render work — the fields must survive to the renderer).
        assert!(
            expected.contains("layup_reduced_chi2"),
            "layup fields absent from the embedded results",
        );

        // (2) every section anchor is present.
        for anchor in [
            "id=\"s01\"",
            "id=\"s01b\"",
            "id=\"s02\"",
            "id=\"s03\"",
            "id=\"s04\"",
            "id=\"s05\"",
            "id=\"s06\"",
            "id=\"s07\"",
            "id=\"s08\"",
            "id=\"s09\"",
            "id=\"s09b\"",
            "id=\"s10\"",
            "id=\"s11\"",
            "id=\"s12\"",
            "id=\"s13\"",
        ] {
            assert!(html.contains(anchor), "missing section anchor: {anchor}");
        }
    }
}
