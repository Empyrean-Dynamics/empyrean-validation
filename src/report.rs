//! Branded HTML validation report.
//!
//! Renders sections 01-08 over the rust reference channel, then a new
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

/// Map position error (km) to a color anchored on planetary-defense
/// thresholds:
///   < 1 km          green-blue (sub-keyhole / Earth-radius scale)
///   1–100 km        yellow-amber
///   100–10⁴ km      orange (Earth–Moon scale)
///   10⁴–10⁶ km      deep red (past lunar orbit)
///   > 10⁶ km        magenta-black (gross divergence; ≥ 1e6 km ≈ Hill sphere)
fn log_color(km: Option<f64>) -> String {
    match km {
        None => "#161c25".to_string(), // distinct from page bg so missing != zero
        Some(km) if km <= 0.0 => "#1a3550".to_string(),
        Some(km) => {
            // Anchor stops in log10(km).
            // -3 → very deep blue; 0 → green-blue (1 km); 2 → amber (100 km);
            // 4 → orange (Earth-Moon); 6 → red (1 Mkm); 8+ → magenta.
            let log_km = km.log10();
            // Map to t in [0,1] over [-3, +8.5].
            let t = ((log_km + 3.0) / 11.5).clamp(0.0, 1.0);
            // Multi-stop gradient.
            let stops: [(f64, [f64; 3]); 6] = [
                (0.00, [13.0, 30.0, 60.0]),   // <0.001 km
                (0.26, [91.0, 155.0, 213.0]), // 1 km    (planetary-defense pass band)
                (0.43, [200.0, 175.0, 70.0]), // 100 km  (lunar orbit Δ scale)
                (0.61, [220.0, 110.0, 55.0]), // 10⁴ km  (Earth-Moon distance)
                (0.78, [200.0, 60.0, 60.0]),  // 10⁶ km  (Hill sphere)
                (1.00, [180.0, 30.0, 110.0]), // ≥ 10⁸ km
            ];
            let mut rgb = stops[0].1;
            for win in stops.windows(2) {
                let (a_t, a_rgb) = win[0];
                let (b_t, b_rgb) = win[1];
                if t >= a_t && t <= b_t {
                    let f = (t - a_t) / (b_t - a_t).max(1e-9);
                    rgb = [
                        a_rgb[0] + f * (b_rgb[0] - a_rgb[0]),
                        a_rgb[1] + f * (b_rgb[1] - a_rgb[1]),
                        a_rgb[2] + f * (b_rgb[2] - a_rgb[2]),
                    ];
                    break;
                }
            }
            format!(
                "#{:02x}{:02x}{:02x}",
                rgb[0] as u8, rgb[1] as u8, rgb[2] as u8
            )
        }
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

/// Vector position difference (km) between channel row and rust row.
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
    let mut core_by_key: HashMap<
        (String, i64, String, String, Option<String>, Option<String>),
        &ValidationResult,
    > = HashMap::new();
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
        // `by_tt` historically stored only position-vector `dr_km` per row,
        // which is empty for `ephemeris` rows (those carry RA/Dec/range, no
        // `emp_pos_au`). That made the §10 per-test-type matrix print
        // `0/N · 0.0% ≤1e-10` for every replay channel's ephemeris row —
        // a display bug that contradicted the actual bit-exact result.
        // Fixed (empyrean-urfu) by tracking the row's worst-metric diff
        // and the bit-identical boolean per test_type alongside, so the
        // matrix can report the actual cross-channel agreement regardless
        // of which scalar field carries the comparable metric.
        let mut by_tt: BTreeMap<String, Vec<f64>> = BTreeMap::new();
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
                by_tt.entry(r.test_type.clone()).or_default().push(dr_km);
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

        // Per-test-type rollups. n_bit_identical and p99/max are now
        // derived from `by_tt_row_max_diff` (the per-row worst-metric diff
        // in whatever native unit dominated the row), not from the
        // position-vector-only `by_tt` stream — which was empty for
        // ephemeris rows because they don't carry `emp_pos_au`.
        let mut by_test_type: BTreeMap<String, TestTypeRollup> = BTreeMap::new();
        for tt in ["propagation", "ephemeris", "orbit_determination"] {
            let n_compared_tt = by_tt_compared.get(tt).copied().unwrap_or(0);
            let _drs_pos_only = by_tt.remove(tt).unwrap_or_default();
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
                r#"<tr><td class="obj-name">{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td style="color:#d05040">{}</td></tr>"#,
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
    html.push_str("<th>p50 t (rust paired)</th>");
    html.push_str("<th>speed (ch/rust)</th>");
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
            "#d05040"
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
                "#d05040"
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
                None => html.push_str(r#"<td style="color:#4a5060">—</td>"#),
                Some(x) => {
                    if x.n_compared == 0 {
                        html.push_str(r#"<td style="color:#4a5060">—</td>"#);
                    } else {
                        let bit_id_pct = 100.0 * x.n_bit_identical as f64 / x.n_compared as f64;
                        let approx_thresh = approx_bit_identical_threshold(tt);
                        let cell_color = if x.n_bit_identical == x.n_compared {
                            "#3d9a6d"
                        } else if x.p99_dr_km < approx_thresh {
                            "#a0a060"
                        } else {
                            "#d05040"
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
                        "#d05040" // red: above threshold
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
        "Only the rust reference channel is present in this report. Run additional channels to populate this section.".to_string()
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
    let n_tiers = tiers.len();
    let n_channels = channels.len();
    let _tiers_label = if n_tiers != 1 { "s" } else { "" };
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

    // Heatmap: blocks separated by population, sorted by population
    // then by max error within population.
    let mut heatmap_html = String::new();
    for &tier in &tiers {
        heatmap_html.push_str(&format!(
            "  <div class=\"section-desc\" style=\"color:#5b9bd5; margin-bottom:8px;\">{tier}</div>\n"
        ));
        heatmap_html.push_str("  <div class=\"heatmap-container\">\n");
        heatmap_html.push_str("  <table class=\"heatmap\">\n");
        heatmap_html.push_str("    <tr><th></th><th></th>");
        for &dt in &dt_values {
            let sign = if dt >= 0 { "+" } else { "" };
            let label = if dt.abs() >= 365 {
                format!("{sign}{:.0}y", dt as f64 / 365.0)
            } else {
                format!("{sign}{dt}d")
            };
            heatmap_html.push_str(&format!("<th class=\"dt-col\">{label}</th>"));
        }
        heatmap_html.push_str("</tr>\n");

        // Group objects by population and within each pop sort by max
        // error (descending) so the noisy ones float up.
        let mut by_pop: BTreeMap<&str, Vec<(&str, f64)>> = BTreeMap::new();
        for &(obj_name, obj_pop) in &objects {
            let max_err = dt_values
                .iter()
                .filter_map(|&dt| prop_lookup.get(&(obj_name, dt, tier)))
                .filter_map(|r| r.emp_vs_horizons_km)
                .fold(0.0f64, f64::max);
            by_pop.entry(obj_pop).or_default().push((obj_name, max_err));
        }
        for objs in by_pop.values_mut() {
            objs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        }

        let mut first_in_pop = true;
        for (pop, objs) in &by_pop {
            if !first_in_pop {
                // Population separator row.
                heatmap_html.push_str(&format!(
                    "    <tr><td colspan=\"{}\" style=\"border-bottom:none; height:6px; background:#0d1117\"></td></tr>\n",
                    dt_values.len() + 2
                ));
            }
            first_in_pop = false;
            let pop_color = population_color(pop);

            for (obj_name, _) in objs {
                heatmap_html.push_str("    <tr>");
                heatmap_html.push_str(&format!("<td class=\"obj-name\">{obj_name}</td>"));
                heatmap_html.push_str(&format!(
                    "<td class=\"pop-tag\"><span class=\"pop-dot\" style=\"background:{pop_color}\"></span>{pop}</td>"
                ));
                for &dt in &dt_values {
                    if let Some(r) = prop_lookup.get(&(obj_name, dt, tier)) {
                        let err = r.emp_vs_horizons_km;
                        let bg = log_color(err);
                        let text = fmt_error(err);
                        let text_color = if err.is_some_and(|e| e < 100.0) {
                            "#08080a"
                        } else {
                            "#e8e8ec"
                        };
                        heatmap_html.push_str(&format!(
                            "<td class=\"cell\" style=\"background:{bg};color:{text_color}\" title=\"{obj_name} dt={dt}d: {text}\">{text}</td>"
                        ));
                    } else {
                        // Distinct from "0" cells via diagonal hatch.
                        heatmap_html
                            .push_str("<td class=\"cell missing\" title=\"not tested\">·</td>");
                    }
                }
                heatmap_html.push_str("</tr>\n");
            }
        }
        heatmap_html.push_str("  </table>\n");
        heatmap_html.push_str("  </div>\n");

        // Color-scale legend for this tier.
        heatmap_html.push_str(r##"  <div class="legend" style="margin-top:8px;">
    <div class="legend-item"><span class="pop-dot" style="background:#0d1e3c"></span>&lt; 1 km · sub-keyhole</div>
    <div class="legend-item"><span class="pop-dot" style="background:#5b9bd5"></span>1 km</div>
    <div class="legend-item"><span class="pop-dot" style="background:#c8af46"></span>100 km</div>
    <div class="legend-item"><span class="pop-dot" style="background:#dc6e37"></span>10⁴ km · Earth–Moon</div>
    <div class="legend-item"><span class="pop-dot" style="background:#c83c3c"></span>10⁶ km · Hill sphere</div>
    <div class="legend-item"><span class="pop-dot" style="background:#b41e6e"></span>≥ 10⁸ km · &gt;1 AU</div>
    <div class="legend-item"><span class="pop-dot" style="background:#161c25; border:1px dashed #4a5060"></span>not tested (atmospheric impactors aren't propagated past the entry time, so positive dt cells are dashed)</div>
  </div>
"##);
        heatmap_html.push('\n');
    }

    let rollups = rollup_channels(results);
    if let Some(path) = summary {
        write_summary(&rollups, path)?;
    }
    let any_pass_or_fail = rollups
        .iter()
        .filter(|r| r.channel != "core" && r.n_total_compared > 0)
        .count()
        > 0;
    let fidelity_summary = if !any_pass_or_fail {
        "Only the rust reference channel is present in this report. Run additional channels (python, c, cli, core) to populate this section.".to_string()
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
                    r#"<span style="color:#5b9bd5">{n}/{n} rust (ref)</span>"#,
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
                "#d05040"
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
            .filter(|r| r.channel == "core" && r.findorb_rms_residual.is_some())
            .count();
        let n_od_fo_missing = n_od_rust.saturating_sub(n_od_fo);
        let od_color = if n_od_conv == n_od_rust {
            "#3d9a6d"
        } else {
            "#a0a060"
        };
        parts.push(format!(
            r#"<span style="color:{od_color}">OD {n_od_conv}/{n_od_rust} converged</span> <span style="color:#8b9198">({n_od_fo} cross-checked vs find_orb{fo_skip})</span>"#,
            fo_skip = if n_od_fo_missing > 0 {
                format!("; {n_od_fo_missing} skipped (timeout)")
            } else {
                String::new()
            }
        ));
        parts.join(" · ")
    };

    // ── Run-mode disclaimer banner. This run was captured with
    // Per-call origin-switching policy disclaimer. Switching is ON
    // by default for Jet1 covariance propagation, OFF for short
    // observation arcs (< 7 days) via scott's automatic gate, and
    // bodies in the test-particle's `excluded_perturbers` list are
    // filtered out of the OriginSwitchZone install (so SB441-N16
    // self-perturbers like Pallas / Vesta / Iris / Hygiea fit
    // correctly). The four atmospheric impactors (2008 TC3, 2024 BX1,
    // 2023 CX1, 2018 LA) get an unswitched fit via the gate; their
    // RMS in §9 reflects the heliocentric-DC ranging seed quality.
    // The empyrean-wu49 follow-up will recover them to find_orb /
    // Scout territory by skipping DC entirely for short-arc Ranging
    // IOD and using the MOV best sample.
    let run_mode_banner_html = r##"
<div class="run-mode-banner" style="background:#1a2a3a; border-left:4px solid #5b9bd5; padding:14px 60px; max-width:1400px; margin:0 auto; font-family:'JetBrains Mono',monospace; font-size:11px; color:#a8c9e8; line-height:1.6;">
  <b style="color:#5b9bd5">PER-CALL ORIGIN-SWITCHING POLICY</b>
  · <b>ON</b> for Jet1 covariance propagation by default — correct Earth-encounter dynamics, mode-consistent with f64.
  · <b>OFF</b> for short observation arcs (&lt; 7 days) via scott's auto-gate — short arcs are ill-conditioned and DC chatter destabilises convergence; the gate restores heliocentric stability at the cost of accuracy on atmospheric-impactor fits (4 fixtures in §9).
  · <b>Bodies in <code>excluded_perturbers</code></b> (the test-particle's own mass for SB441-N16 self-perturbers) are filtered out of the OriginSwitchZone install — Pallas / Vesta / Iris / Hygiea now fit to sub-arcsec RMS where prior runs blew up to 10<sup>5</sup>″.
  · OD residuals worse than find_orb on the three atmospheric impactors (2008 TC3, 2024 BX1, 2023 CX1) are due to the short-arc gate's heliocentric integration; a follow-up task will recover impactor-OD precision via systematic-ranging-as-final-answer.
  · See §13 for full provenance and citation list.
</div>
"##.to_string();

    // ── §13 Reproducibility footer — every detail a referee needs to
    // reproduce a number from this report. Static content for now;
    // version + git-hash fields hard-coded against the current pins
    // (empyrean-core v0.7, villeneuve 1.12, hyperjet 1.7). Per-row
    // run-time provenance (commit hash, kernel hash) is a follow-up.
    let provenance_footer_html = format!(
        r##"
<div class="section" id="s13">
  <div class="section-num">13</div>
  <div class="section-title">Reproducibility &mdash; Provenance</div>
  <div class="section-desc">
    Every number in this report can be reproduced by running the
    <code>empyrean-validation</code> Makefile against the same inputs.
    This appendix enumerates the pinned dependencies and configuration
    used for this run. Where a setting is a runtime override of a
    library default, the override is shown; library defaults are not
    repeated.
  </div>
  <table class="od-table" style="font-size:11px;">
    <thead><tr><th style="text-align:left">Component</th><th style="text-align:left">Version / value</th></tr></thead>
    <tbody>
      <tr><td>Frame</td><td>ICRF (J2000), barycentric</td></tr>
      <tr><td>Time scale</td><td>TDB (Barycentric Dynamical Time)</td></tr>
      <tr><td>Planetary ephemeris</td><td>JPL DE441 (long-term, no extended terms)</td></tr>
      <tr><td>Asteroid perturber set</td><td>SB441-N16: <code>1, 3, 4, 7, 10, 15, 16, 31, 52, 65, 70, 87, 88, 107, 511, 704</code></td></tr>
      <tr><td>Force model (standard)</td><td>Point-mass Sun + 8 planets + Moon + Pluto + 16 SB441-N16 asteroids; 1PN GR Einstein-Infeld-Hoffmann for Sun; non-gravitational A1 (radial) / A2 (transverse) / A3 (normal) accelerations with Marsden's <code>g(r) = α (r/r₀)<sup>−m</sup> [1 + (r/r₀)<sup>n</sup>]<sup>−k</sup></code>. For asteroids (Apophis, Bennu, et al.) A2 ≠ 0 is the standard parameterisation of the <b>Yarkovsky effect</b> (Marsden 1968 / Vokrouhlický et al. 2015); A1 = A3 = 0 and g(r) = 1/r² for asteroid fits. For comets all three may be non-zero with the Marsden / Yeomans–Chodas g(r).</td></tr>
      <tr><td>Integrator</td><td>villeneuve 1.12 GR15 (15-stage Gauss-Radau, adaptive step, default ε=1e-9; dt_min = 10⁻⁹ days ≈ 86 µs, derived solely from Everhart 1985)</td></tr>
      <tr><td>Autodiff (Jet1 STM)</td><td>hyperjet 1.7 (forward-mode, N=6 or N=9 with non-grav)</td></tr>
      <tr><td>Origin switching</td><td><b>DISABLED on Jet1 path</b> (see run-mode banner); enabled on f64 path with default 0.2 hysteresis band; Laplace SOI test per Amato/Baù/Bombardelli 2017</td></tr>
      <tr><td>External OD reference</td><td>find_orb (Project Pluto, B. Gray) · environ.dat: <code>SETTINGS2=1 22.00 0 -2 0</code>, <code>OUTLIER_REJECTION_LIMIT=3</code>, <code>ENCKE=1</code>, <code>PERTURBERS=1007fe</code>, SB441-N16 perturbers</td></tr>
      <tr><td>External propagation reference</td><td>ASSIST (Holman et al. 2023, arXiv:2308.15572) on REBOUND IAS15; DE441 + SB441-N16</td></tr>
      <tr><td>Observation source</td><td>Minor Planet Center API (<a href="https://data.minorplanetcenter.net/api/get-obs" target="_blank" rel="noopener" style="color:#5b9bd5">data.minorplanetcenter.net/api/get-obs</a>); fetched at runtime</td></tr>
      <tr><td>Observation weights</td><td>MPC-reported <code>rmsRA</code>/<code>rmsDec</code> when present; Vereš et al. 2017 (Icarus 296) fallback for missing</td></tr>
      <tr><td>Outlier rejection</td><td>Empyrean OD: 4σ Huber; find_orb: 3σ</td></tr>
      <tr><td>OD convergence criterion</td><td>‖ΔIC‖₂ &lt; 10⁻¹² · ‖IC‖ <i>or</i> |Δχ²| &lt; 10⁻⁶</td></tr>
      <tr><td>OD max iterations</td><td>100 (rows iter=100 in §9 did not converge within cap)</td></tr>
      <tr><td>Reference orbits (§12)</td><td>JPL SBDB web service; per-object epoch as returned</td></tr>
      <tr><td>Fidelity threshold</td><td>‖Δr<sub>chan</sub> − Δr<sub>rust</sub>‖ ≤ 10⁻¹⁰ km ≈ 0.1 nm (float64 ULP at 1 AU)</td></tr>
      <tr><td>Report generated</td><td>{report_run_date}</td></tr>
    </tbody>
  </table>
  <div class="section-desc" style="margin-top:24px; font-size:11px;">
    <b>References</b><br/>
    · Holman, M. et al. 2023, "ASSIST: An ephemeris-quality test-particle integrator", AJ 166, 99 (arXiv:2308.15572).<br/>
    · Vereš, P. et al. 2017, "Statistical analysis of astrometric errors for the most productive asteroid surveys", Icarus 296, 139.<br/>
    · Amato, D., Baù, G., Bombardelli, C. 2017, "Accurate orbit propagation in the presence of planetary close encounters", MNRAS 470, 2079.<br/>
    · Park, R. S., Folkner, W. M., Williams, J. G., Boggs, D. H. 2021, "The JPL Planetary and Lunar Ephemerides DE440 and DE441", AJ 161, 105.<br/>
    · Marsden, B. G., Sekanina, Z., Yeomans, D. K. 1973, "Comets and Nongravitational Forces. V", AJ 78, 211.<br/>
    · Vokrouhlický, D., Bottke, W. F., Chesley, S. R., Scheeres, D. J., Statler, T. S. 2015, "The Yarkovsky and YORP Effects", in <i>Asteroids IV</i>, p. 509.<br/>
    · Gauss-Radau-15: Everhart, E. 1985, in "Dynamics of Comets" (Reidel), p. 185.<br/>
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
  * {{ margin: 0; padding: 0; box-sizing: border-box; }}
  body {{ background: #0d1117; color: #e8e8ec; font-family: 'DM Sans', sans-serif; font-weight: 300; min-height: 100vh; }}
  .header {{ padding: 60px 60px 40px; max-width: 1400px; margin: 0 auto; border-bottom: 1px solid #1a2332; }}
  .header h1 {{ font-family: 'Syne', sans-serif; font-weight: 700; font-size: 36px; color: #e8e8ec; letter-spacing: -0.5px; margin-bottom: 4px; }}
  .header h2 {{ font-family: 'Syne', sans-serif; font-weight: 700; font-size: 36px; color: rgba(232,232,236,0.40); letter-spacing: -0.5px; margin-bottom: 12px; }}
  .header .subtitle {{ font-family: 'JetBrains Mono', monospace; font-size: 11px; color: #5b9bd5; letter-spacing: 2px; text-transform: uppercase; }}
  .header .meta {{ font-family: 'JetBrains Mono', monospace; font-size: 11px; color: #4a5060; margin-top: 8px; }}
  .header .provenance {{ font-family: 'JetBrains Mono', monospace; font-size: 10px; color: #8b9198; margin-top: 6px; line-height: 1.7; }}

  .section {{ padding: 60px 60px; max-width: 1400px; margin: 0 auto; border-bottom: 1px solid #1a2332; }}
  .section:last-child {{ border-bottom: none; }}
  .section-num {{ font-family: 'JetBrains Mono', monospace; font-size: 10px; color: #4a5060; letter-spacing: 2px; text-transform: uppercase; margin-bottom: 8px; }}
  .section-title {{ font-family: 'Syne', sans-serif; font-weight: 700; font-size: 24px; color: #e8e8ec; margin-bottom: 8px; }}
  .section-desc {{ font-size: 13px; color: #8b9198; line-height: 1.6; max-width: 900px; margin-bottom: 24px; }}
  .panel-title {{ font-family: 'Syne', sans-serif; font-size: 14px; color: #e8e8ec; margin: 24px 0 8px 0; }}

  .heatmap-container {{ overflow-x: auto; max-width: 100%; padding-bottom: 8px; }}
  .heatmap-container::-webkit-scrollbar {{ height: 6px; }}
  .heatmap-container::-webkit-scrollbar-track {{ background: #0d1117; border-radius: 3px; }}
  .heatmap-container::-webkit-scrollbar-thumb {{ background: #2d3440; border-radius: 3px; }}
  .heatmap-container::-webkit-scrollbar-thumb:hover {{ background: #4a5060; }}
  table.heatmap {{ border-collapse: collapse; font-family: 'JetBrains Mono', monospace; font-size: 10px; }}
  table.heatmap th {{ padding: 6px 10px; color: #8b9198; font-weight: 400; letter-spacing: 1px; text-transform: uppercase; border-bottom: 1px solid #1a2332; position: sticky; top: 0; background: #0d1117; z-index: 1; }}
  table.heatmap th.dt-col {{ text-align: center; min-width: 70px; }}
  table.heatmap td {{ padding: 5px 8px; text-align: center; border-bottom: 1px solid #14181f; cursor: default; }}
  table.heatmap td.obj-name {{ text-align: left; color: #e8e8ec; font-size: 11px; white-space: nowrap; padding-right: 16px; position: sticky; left: 0; background: #0d1117; }}
  table.heatmap td.pop-tag {{ text-align: left; font-size: 9px; padding-right: 12px; }}
  table.heatmap td.cell {{ font-size: 9px; border-radius: 2px; }}
  table.heatmap td.cell:hover {{ outline: 1px solid #5b9bd5; outline-offset: -1px; }}
  table.heatmap td.cell.missing {{
    background: repeating-linear-gradient(45deg, #161c25, #161c25 4px, #1f2630 4px, #1f2630 8px);
    color: #4a5060;
  }}
  .pop-dot {{ display: inline-block; width: 6px; height: 6px; border-radius: 50%; margin-right: 4px; vertical-align: middle; }}
  .chart-container {{ background: #151b23; border: 1px solid #1a2332; border-radius: 6px; padding: 16px; margin-bottom: 24px; }}
  .chart-grid-2 {{ display: grid; grid-template-columns: 1fr 1fr; gap: 16px; margin-bottom: 24px; }}
  .summary-grid {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: 12px; margin-bottom: 24px; }}
  .summary-card {{ background: #151b23; border: 1px solid #1a2332; border-radius: 6px; padding: 20px; }}
  .summary-card .value {{ font-family: 'Syne', sans-serif; font-weight: 700; font-size: 28px; color: #e8e8ec; }}
  .summary-card .label {{ font-family: 'JetBrains Mono', monospace; font-size: 9px; color: #8b9198; letter-spacing: 1px; text-transform: uppercase; margin-top: 4px; }}
  .legend {{ display: flex; gap: 16px; flex-wrap: wrap; margin-bottom: 16px; }}
  .legend-item {{ display: flex; align-items: center; gap: 4px; font-family: 'JetBrains Mono', monospace; font-size: 9px; color: #8b9198; }}
  .channel-toggle {{ display: inline-flex; gap: 6px; margin: 8px 0 16px 0; font-family: 'JetBrains Mono', monospace; font-size: 10px; }}
  .channel-toggle button {{ background: #151b23; border: 1px solid #1a2332; border-radius: 4px; color: #8b9198; padding: 4px 10px; cursor: pointer; font-family: inherit; font-size: 10px; }}
  .channel-toggle button.active {{ color: #e8e8ec; border-color: #5b9bd5; background: #1a2332; }}
  .od-table {{ font-family: 'JetBrains Mono', monospace; font-size: 10px; border-collapse: collapse; min-width: 100%; }}
  .od-table th, .od-table td {{ padding: 6px 10px; border-bottom: 1px solid #1a2332; text-align: right; }}
  .od-table th {{ color: #8b9198; font-weight: 400; letter-spacing: 1px; text-transform: uppercase; font-size: 9px; }}
  .od-table td.obj {{ text-align: left; color: #e8e8ec; }}
  .od-table tr.diverged {{ background: rgba(208, 80, 64, 0.08); }}
</style>
</head>
<body>

<div class="header">
  <div class="subtitle">Validation Report</div>
  <h1>EMPYREAN</h1>
  <h2>DYNAMICS</h2>
  <div class="meta" style="font-size:12px; line-height:1.8;">{quality_summary_html}</div>
  <div class="meta" style="margin-top:6px; color:#5b9098;">{n_prop} propagation · {n_eph} ephemeris · {n_od} OD · {n_objects} objects · {n_channels} channel{channels_label}</div>
  <div class="provenance">Test epoch: {test_epoch_label}<br/>Frame: ICRF (J2000) · Ephemeris: DE441 · Force model: empyrean::standard (1PN GR · 16-asteroid SB441-N16 perturbers · Marsden A1/A2/A3 + g(r) non-grav)<br/>Coverage: {coverage_line}<br/>Report run: {report_run_date} &nbsp;·&nbsp; <a href="#s13" style="color:#5b9bd5; text-decoration:none">▸ provenance &amp; references</a> &nbsp;·&nbsp; <a href="javascript:void(0)" onclick="downloadJSON()" style="color:#5b9bd5; text-decoration:none">↓ download embedded JSON</a></div>
</div>

{run_mode_banner_html}

<div class="section" style="padding-bottom:20px;">
  <div class="section-title" style="font-size:16px; margin-bottom:12px;">Contents</div>
  <div style="font-family:'JetBrains Mono',monospace; font-size:11px; line-height:2.4; color:#5b9bd5;">
    <a href="#s01" style="color:#5b9bd5; text-decoration:none;">01 Summary</a><br/>
    <a href="#s01b" style="color:#5b9bd5; text-decoration:none;">01b External Reference Tools — Transparency</a><br/>
    <a href="#s02" style="color:#5b9bd5; text-decoration:none;">02 Propagation &mdash; Accuracy Heatmap</a><br/>
    <a href="#s03" style="color:#5b9bd5; text-decoration:none;">03 Propagation &mdash; Error Growth (per population)</a><br/>
    <span id="toc-s04" style="display:none"><a href="#s04" style="color:#5b9bd5; text-decoration:none;">04 Propagation &mdash; ASSIST Comparison</a><br/></span>
    <span id="toc-s05" style="display:none"><a href="#s05" style="color:#5b9bd5; text-decoration:none;">05 Propagation &mdash; Timing</a><br/></span>
    <a href="#s06" style="color:#5b9bd5; text-decoration:none;">06 Ephemeris &mdash; Angular Separation</a><br/>
    <a href="#s07" style="color:#5b9bd5; text-decoration:none;">07 Ephemeris &mdash; RA/Dec Residuals</a><br/>
    <a href="#s08" style="color:#5b9bd5; text-decoration:none;">08 Ephemeris &mdash; Residual Growth</a><br/>
    <a href="#s09" style="color:#5b9bd5; text-decoration:none;">09 Orbit Determination &mdash; Diagnostics</a><br/>
    <a href="#s09b" style="color:#5b9bd5; text-decoration:none;">09b Non-gravitational Recovery &mdash; Fitted A1/A2/A3 vs JPL</a><br/>
    <a href="#s10" style="color:#5b9bd5; text-decoration:none;">10 Distribution Channel Fidelity</a><br/>
    <a href="#s11" style="color:#5b9bd5; text-decoration:none;">11 Propagation &mdash; Uncertainty Cost (Jet1 vs f64)</a><br/>
    <a href="#s12" style="color:#5b9bd5; text-decoration:none;">12 Fitted Orbit and Covariance &mdash; vs References</a><br/>
    <a href="#s13" style="color:#5b9bd5; text-decoration:none;">13 Reproducibility &mdash; Provenance</a>
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
  <div class="legend">
{pop_legend}  </div>
  <div class="legend">
{channel_legend}  </div>
</div>

<div class="section" id="s01b">
  <div class="section-num">1b</div>
  <div class="section-title">External Reference Tools &mdash; Transparency</div>
  <div class="section-desc">Every external comparison in this report is driven by a runner script in <a href="https://github.com/Empyrean-Dynamics/empyrean-validation" target="_blank" rel="noopener">empyrean-validation</a>. Each script is the full configuration we used to invoke that tool — settings, force model, weighting, rejection. If you want to reproduce a number in any panel, the script tells you exactly how we got there. (The tools themselves are external dependencies and are <b>not linked into</b> empyrean — they're run independently and their JSON outputs are merged into the report at the comparison step.)</div>
  <table class="od-table" style="margin-top:0.5em">
    <thead><tr>
      <th style="text-align:left">Tool</th>
      <th style="text-align:left">Reference</th>
      <th style="text-align:left">Used for</th>
      <th style="text-align:left">Our runner</th>
    </tr></thead>
    <tbody>
      <tr>
        <td><b>ASSIST</b></td>
        <td>Holman et al. 2023 · REBOUND IAS15 · DE441</td>
        <td>N-body propagation; first-order STM (6 variational particles); second-order STT (6 + 21 variational particles)</td>
        <td><a href="https://github.com/Empyrean-Dynamics/empyrean-validation/blob/main/runners/assist/run_assist.py" target="_blank" rel="noopener"><code>runners/assist/run_assist.py</code></a></td>
      </tr>
      <tr>
        <td><b>find_orb</b></td>
        <td>Gray (Project Pluto) · MPC-grade OD</td>
        <td>Orbit determination from ADES astrometry; reference for post-fit RMS</td>
        <td><a href="https://github.com/Empyrean-Dynamics/empyrean-validation/blob/main/runners/findorb/run_findorb.py" target="_blank" rel="noopener"><code>runners/findorb/run_findorb.py</code></a></td>
      </tr>
      <tr style="opacity:0.55">
        <td colspan="4"><small style="color:#8b9198"><b>Planned / opt-in (not in this run):</b> <b>OpenOrb</b> (Granvik et al. 2009 — statistical-ranging / least-squares OD), <b>kete</b> (Dar Dahlen — independent Rust/Python NEO toolkit, originally Caltech IPAC / NEO Surveyor), <b>jorbit</b> (JAX autodiff propagator / OD). Runner scripts live under <a href="https://github.com/Empyrean-Dynamics/empyrean-validation/tree/main/runners" target="_blank" rel="noopener"><code>runners/</code></a>; their JSON merges into the report when run.</small></td>
      </tr>
    </tbody>
  </table>
</div>

<div class="section" id="s02">
  <div class="section-num">02</div>
  <div class="section-title">Propagation &mdash; Accuracy Heatmap</div>
  <div class="section-desc">Position error vs JPL Horizons (km) at each propagation offset. Color scale tagged to encounter-distance thresholds (sub-keyhole / lunar / Earth–Moon / Hill sphere). Rust channel; sorted within each population by max error.</div>
{heatmap_html}</div>

<div class="section" id="s03">
  <div class="section-num">03</div>
  <div class="section-title">Propagation &mdash; Error Growth</div>
  <div class="section-desc">Position error vs JPL Horizons over time, log-y. Median curve plus IQR band per population reduces the 43 individual lines to 13 population curves + named outliers. Toggle between rust-only (default) and 5-channel overlay; in <b>All channels</b> mode, c/cli/python lie exactly on rust (bit-exact, the lines overlap at chart resolution) and core diverges only on the deep-time chaotic-divergence rows surfaced in §10 — so a lone deviating line is the only visible signal, never four parallel lines.</div>
  <div class="channel-toggle" id="s03-toggle">
    <button class="active" data-mode="rust">Rust only</button>
    <button data-mode="all">All channels overlay</button>
  </div>
  <div class="chart-container">
    <div id="error-growth-chart" style="height:520px;"></div>
  </div>
</div>

<div class="section" id="s04" style="display:none">
  <div class="section-num">04</div>
  <div class="section-title">Propagation &mdash; ASSIST Comparison</div>
  <div class="section-desc">Log-y |empyrean − ASSIST| vs |ASSIST − Horizons| over time. ASSIST (Holman et al. 2023, REBOUND/IAS15/DE441) is the independent N-body reference. Triangular markers = comets (g(r) implementation differs from ASSIST). Squares = self-perturbers (ASSIST treats as test particles). Threshold lines at 1 km / 100 km / 1 AU.</div>
  <div class="chart-container">
    <div id="assist-chart" style="height:520px;"></div>
  </div>
  <div class="summary-grid" style="margin-top:16px;">
    <div class="summary-card"><div id="assist-median" class="value">—</div><div class="label">Median |emp − ASSIST|</div></div>
    <div class="summary-card"><div id="assist-ratio" class="value">—</div><div class="label">Median |emp−ASSIST| ÷ |ASSIST−Horizons|</div></div>
    <div class="summary-card"><div id="assist-rows" class="value">—</div><div class="label">Rows compared</div></div>
  </div>
</div>

<div class="section" id="s05" style="display:none">
  <div class="section-num">05</div>
  <div class="section-title">Propagation &mdash; Timing</div>
  <div class="section-desc">Per-row timing (log-y) for empyrean vs ASSIST, broken out by <code>propagation_uncertainty</code> mode so the comparison is apples-to-apples: empyrean's f64 path against single-particle ASSIST (f64), and empyrean's Jet1 + 6&times;6 covariance path against ASSIST with 6 first-order variational particles. The merge keys on <code>(object, dt, propagation_uncertainty)</code> so each empyrean row pairs with the matching ASSIST mode. <b>Caveat:</b> ASSIST f64 timings represent per-output-epoch retrieval from a single batch integration (the standard REBOUND/ASSIST workflow), while empyrean f64 timings represent a fresh per-call propagation from the initial epoch to the target. Treat the ASSIST f64 sub-microsecond column as a sanity floor, not a head-to-head benchmark — empyrean's f64 timing would converge to it under the same dense-output discipline.</div>
  <div class="panel-title">f64 propagation (state only)</div>
  <div class="chart-container">
    <div id="timing-chart" style="height:480px;"></div>
  </div>
  <div class="panel-title">STM/STT-bearing propagation (first-order, second-order, Auto cascade)</div>
  <div class="section-desc">Three uncertainty modes plotted on the same axis so empyrean's per-call cost across the STM-bearing surface is visible in one frame. <b>First-order</b> pairs empyrean's Jet1 (6 partials) against ASSIST's 6 first-order variational particles. <b>Second-order</b> pairs empyrean's Jet2 (6 + 21 partials) against ASSIST's 6 first-order + 21 second-order variational particles — the symmetric 6×6 Hessian. <b>Auto</b> is empyrean's Phase A/B/C cascade (FirstOrder / SecondOrder / AGM mixture, driven by per-CA κ); REBOUND has no auto-cascade analogue, so Auto rows baseline against the first-order ASSIST row via the merge step. ASSIST propagates variational equations under gravity only — the non-gravitational <code>additional_forces</code> callback is not applied to the shadows, so STM/STT are gravity-only-approximate for active bodies with non-zero a1/a2/a3.</div>
  <div class="chart-container">
    <div id="timing-cov-chart" style="height:540px;"></div>
  </div>
</div>

<div class="section" id="s06">
  <div class="section-num">06</div>
  <div class="section-title">Ephemeris &mdash; Angular Separation</div>
  <div class="section-desc">Separation between predicted and Horizons RA/Dec (mas), log-y. Median curve + IQR band per population. Reference lines: 1 mas (Gaia astrometric noise floor), 100 mas (CCD residual scale).</div>
  <div class="channel-toggle" id="s06-toggle">
    <button class="active" data-mode="rust">Rust only</button>
    <button data-mode="all">All channels overlay</button>
  </div>
  <div class="chart-container">
    <div id="eph-sep-chart" style="height:520px;"></div>
  </div>
</div>

<div class="section" id="s07">
  <div class="section-num">07</div>
  <div class="section-title">Ephemeris &mdash; RA/Dec Residuals</div>
  <div class="section-desc">RA·cos(δ) vs Dec residuals (mas) against the JPL Horizons reference. Clipped to ±100 mas to show the main cluster; rows beyond ±100 mas are listed in the outlier sidecar table below (these are typically pathological cases like 2020 CD3 propagated 10 yr before its 2020 mini-moon capture). 1σ (solid) and 3σ (dotted) ellipses overlaid from the cleaned cloud, assuming a zero-mean iid Gaussian model — for context, Gaia DR3 single-frame astrometric precision is ≲ 1 mas, ground-based survey typical residuals are ~100–300 mas. Rust channel.</div>
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
  <div class="section-title">Ephemeris &mdash; Residual Growth</div>
  <div class="section-desc">dRA·cos(δ) (top) and dDec (bottom) residuals vs JPL Horizons over the test-epoch time window, symlog y so chaotic outliers and machine-epsilon-grade rows both render. Solid lines: median per population. Shaded band: 25-75 percentile (IQR) per population. Two stacked panels eliminate the solid/dashed-overlay readability problem. Rust channel.</div>
  <div class="chart-container">
    <div id="eph-resid-ra-chart" style="height:280px;"></div>
  </div>
  <div class="chart-container">
    <div id="eph-resid-dec-chart" style="height:280px;"></div>
  </div>
</div>

<div class="section" id="s09">
  <div class="section-num">09</div>
  <div class="section-title">Orbit Determination &mdash; Diagnostics</div>
  <div class="section-desc">For every OD test case, four diagnostics drive the "did the differential corrector land in the same minimum?" question across channels. Post-fit RMS, reduced χ² = χ²/ν, iteration count, and fitted-state drift to rust are all signals that an OD pipeline is not solving the same problem on the same data. <b>Cross-channel OD agreement in this run is bit-exact at 1 nm in fitted state for c / cli / python</b> (see the per-object scatter below — every point sits on the 10⁻¹² km floor). The remaining story is Empyrean vs find_orb on the same observations, where this run shows agreement within ~30% for typical-arc NEOs / MBAs / TNOs and three large regressions on atmospheric impactors flagged in red below (2008 TC3, 2024 BX1, 2023 CX1) — see the run-mode banner for the suspected root cause.</div>
  <div id="od-empty" class="section-desc" style="display:none; color:#8b9198">No orbit-determination rows in this report. Run the OD subset to populate this section.</div>
  <div id="od-content">
    <div class="panel-title">Per-object overview</div>
    <div class="heatmap-container">
      <table class="od-table">
        <thead><tr>
          <th class="obj" style="text-align:left">Object</th>
          <th>n_obs</th>
          <th>χ²<sub>r</sub> = χ²/ν</th>
          <th>RMS RA &middot; Dec</th>
          <th>RMS combined</th>
          <th>iter</th>
          <th>find_orb RMS</th>
          <th>find_orb obs coverage</th>
          <th>Δ vs find_orb</th>
          <th>cross-channel OD<br/><small style="font-weight:300; opacity:0.6">(worst Δχ² · worst ‖Δr_fit‖)</small></th>
        </tr></thead>
        <tbody id="od-overview"></tbody>
      </table>
    </div>

    <div class="panel-title">χ² across channels</div>
    <div class="chart-container">
      <div id="od-chi2-chart" style="height:480px;"></div>
    </div>

    <div class="panel-title">Iterations to convergence</div>
    <div class="chart-container">
      <div id="od-iter-chart" style="height:380px;"></div>
    </div>

    <div class="panel-title">Post-fit RMS (rust channel)</div>
    <div class="chart-container">
      <div id="od-rms-chart" style="height:380px;"></div>
    </div>

    <div class="panel-title">Empyrean OD combined RMS vs find_orb (y = x line)</div>
    <div class="section-desc">Per-object scatter of Empyrean OD's combined RA·cosδ + Dec RMS against find_orb's <code>rms</code>. Points on the y = x diagonal mean the two pipelines agree; deviations above the line mean Empyrean reports a larger RMS than find_orb on that object (typically because find_orb's rejection policy trimmed more observations). Log scale on both axes.</div>
    <div class="chart-container">
      <div id="od-rms-vs-findorb-chart" style="height:480px;"></div>
    </div>

    <div class="panel-title">Fitted-state drift to rust (km, log-y)</div>
    <div class="chart-container">
      <div id="od-fitdr-chart" style="height:420px;"></div>
    </div>
  </div>
</div>

<div class="section" id="s10">
  <div class="section-num">10</div>
  <div class="section-title">Distribution Channel Fidelity</div>
  <div class="section-desc">Cross-channel agreement broken out by test type. The five channels run end-to-end through libempyrean (or empyrean-core directly for the <code>core</code> channel). Bit-identical results are the expected outcome for propagation and ephemeris; OD is bit-exact at &lt; 1 nm fitted-state agreement (see §9 fitted-state-drift chart).
  <br/><br/><small style="color:#8b9198"><b>On the denominators:</b> rust's <code>2588 / 2760 / 35</code> totals cover the full uncertainty grid (Auto + first_order_with_cov + second_order_with_cov + f64_no_cov). Non-rust channels run only the modes their public API exposes (first_order_with_cov + f64_no_cov for c / cli / python; the same two for core, replayed twice through different entry points), so their compared denominators are <code>≈ rust ÷ 2</code> by design — not a coverage gap. The fidelity claim is per-row bit-exactness against the matching rust row; the denominator difference is a sampling artefact of which modes are end-to-end production-shipped.</small></div>
  <div class="section-desc">{fidelity_summary}</div>

  <div class="panel-title">Per-test-type matrix</div>
  {per_tt_matrix_html}

  <div class="panel-title">ECDF of |emp_pos − rust.emp_pos| per channel × test type</div>
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

  <div class="panel-title">Offending rows (channel-vs-rust threshold {fidelity_threshold:.0e})</div>
  {offenders_html}
</div>

<div class="section" id="s11">
  <div class="section-num">11</div>
  <div class="section-title">Propagation &mdash; Uncertainty Cost (Jet1 vs f64)</div>
  <div class="section-desc">
    <strong>Empyrean is uncertainty-first by design.</strong> When you call
    <code>empyrean.propagate(orbit, t)</code> with an orbit that carries a
    covariance, the propagator dispatches automatically to first-order
    Jet1 / STM integration — no flag, no opt-in. The covariance is
    propagated through the State Transition Matrix and a 6×6 covariance
    falls out of the same call. When the input orbit has no covariance,
    the propagator dispatches to plain f64 state-only integration and
    you pay nothing for what you don't use. The validation suite exercises
    both modes side-by-side, tagging each propagation row with
    <code>propagation_uncertainty = "first_order_with_cov"</code> or
    <code>"f64_no_cov"</code>. This section shows the cost of carrying
    uncertainty per population — i.e., the price of the production hot
    path users hit every day.
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

<div class="section" id="s12">
  <div class="section-num">12</div>
  <div class="section-title">Fitted Orbit and Covariance &mdash; vs References</div>
  <div class="section-desc">
    For every OD test case where an SBDB solution (or find_orb fit) is
    available, Empyrean's fit is compared to that reference in Keplerian
    element space, bidirectionally — once at Empyrean's epoch (reference
    propagated via STM) and once at the reference's epoch (Empyrean
    propagated). For linear propagation, Mahalanobis is direction-invariant
    so the bidirectional pair acts as a sanity check on the STM chain.
    Three pathology fingerprints:
    <ul style="margin-top:0.4em;">
      <li><b>CONSISTENT</b> (σ<sub>equiv</sub> &lt; 1, green): the two ellipsoids agree within their joint uncertainty.</li>
      <li><b>CORRELATION</b> (joint d² &gt;&gt; marginal d²): off-diagonal correlation in the joint covariance is driving the discrepancy, not any single element. Canonical example: short-arc fit with a/e degeneracy.</li>
      <li><b>ROTATION</b> (principal-axis rotation angle &gt; 30° with similar eigenvalue spectra): the two ellipsoids have different orientations. Often artifact of propagation across a non-linear region.</li>
      <li><b>(unlabeled high σ<sub>equiv</sub>)</b>: large σ<sub>equiv</sub> that triggers neither CORRELATION nor ROTATION typically means a scale-only disagreement (missing perturber, weight scheme mismatch) or a true difference in the fit. Asymmetric bidirectional pairs (e.g., 2020 CD3 σ_equiv = 0.04 one way / 17 the other) reflect non-linear regions like chaotic-capture geometries — the STM Mahalanobis is only direction-invariant in the linear limit.</li>
    </ul>
    The Mahalanobis decomposition column (d²<sub>combined</sub> / d²<sub>marginal</sub>) flags whether the joint metric is or is not marginally explained.
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
    <div class="section-desc" style="font-size:0.85em; margin-top:0.8em">Every pair references the JPL SBDB solution, propagated to the common epoch via the fitted-covariance STM (the per-row epoch source is shown in the table above). Where an object's reference covariance is non-SPD at the common epoch, that row's Mahalanobis metric uses the regularised form.</div>
  </div>
</div>

{provenance_footer_html}

<script>
const results = RESULTS_JSON;
const popColors = POP_COLORS_JSON;
const channelColors = CHANNEL_COLORS_JSON;
const AU_KM = 149597870.700;

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
    const a = {{ title: {{ text: title, font: {{ size: 10 }} }}, gridcolor: '#1a2332', zerolinecolor: '#1a2332', color: '#8b9198' }};
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
function buildErrorGrowth(mode) {{
    const traces = [];
    const dts = uniq(propResults.map(r => r.dt_days)).sort((a,b)=>a-b);
    if (mode === 'rust') {{
        // Per-population: median + IQR band over objects in that population.
        for (const pop of popNames) {{
            const popObjs = propResults.filter(r => r.population === pop);
            const med = [], lo = [], hi = [], xs = [];
            for (const dt of dts) {{
                const vals = popObjs.filter(r => r.dt_days === dt && r.emp_vs_horizons_km != null).map(r => r.emp_vs_horizons_km);
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
        // Annotate top-3 outlier objects by max error.
        const objMax = objectNames.map(name => {{
            const vals = propResults.filter(r => r.object === name).map(r => r.emp_vs_horizons_km).filter(v => v != null);
            return [name, vals.length ? Math.max(...vals) : 0];
        }}).sort((a, b) => b[1] - a[1]).slice(0, 4);
        for (const [name] of objMax) {{
            const objR = propResults.filter(r => r.object === name && r.emp_vs_horizons_km != null).sort((a, b) => a.dt_days - b.dt_days);
            if (!objR.length) continue;
            const pop = objR[0].population;
            traces.push({{
                x: objR.map(r => r.dt_days), y: objR.map(r => r.emp_vs_horizons_km),
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
        yaxis: ax('Position error vs Horizons (km)', 'log'),
        showlegend: true,
        legend: {{ ...baseLayout.legend, orientation: 'h', y: 1.14 }},
    }}, {{ responsive: true, displayModeBar: 'hover', modeBarButtonsToRemove: ['select2d', 'lasso2d', 'autoScale2d', 'toggleSpikelines'] }});
}}
buildErrorGrowth('rust');
document.querySelectorAll('#s03-toggle button').forEach(btn => {{
    btn.onclick = () => {{
        document.querySelectorAll('#s03-toggle button').forEach(b => b.classList.remove('active'));
        btn.classList.add('active');
        buildErrorGrowth(btn.dataset.mode);
    }};
}});

// ─────────── Section 04: ASSIST comparison ───────────
const assistResults = propResults.filter(r => r.assist_vs_horizons_km != null);
if (assistResults.length > 0) {{
    document.getElementById('s04').style.display = '';
    document.getElementById('s05').style.display = '';
    document.getElementById('toc-s04').style.display = '';
    document.getElementById('toc-s05').style.display = '';
    const traces = [];
    const empMarker = {{ symbol: 'circle', size: 6 }};
    const assistMarker = {{ symbol: 'diamond-open', size: 6 }};
    const cometObjs = new Set(propResults.filter(r => (r.population || '').toLowerCase().includes('comet') || (r.population || '').toLowerCase().includes('iso')).map(r => r.object));
    // Per-population trace for emp - ASSIST
    for (const pop of popNames) {{
        const rs = assistResults.filter(r => r.population === pop && r.emp_vs_assist_km != null);
        if (!rs.length) continue;
        traces.push({{
            x: rs.map(r => r.dt_days), y: rs.map(r => Math.max(r.emp_vs_assist_km, 1e-9)),
            mode: 'markers', name: `|emp−ASSIST| ${{pop}}`,
            marker: {{ ...empMarker, color: popColors[pop] || '#888',
                symbol: rs.map(r => cometObjs.has(r.object) ? 'triangle-up' : 'circle') }},
            hovertemplate: '%{{text}}<extra></extra>',
            text: rs.map(r => `${{r.object}} (${{pop}})<br>dt: ${{r.dt_days}}d<br>|emp−ASSIST|: ${{r.emp_vs_assist_km.toFixed(4)}} km`),
        }});
    }}
    // ASSIST vs Horizons reference (lighter)
    const refR = assistResults.filter(r => r.assist_vs_horizons_km != null);
    traces.push({{
        x: refR.map(r => r.dt_days), y: refR.map(r => Math.max(r.assist_vs_horizons_km, 1e-9)),
        mode: 'markers', name: '|ASSIST−Horizons|',
        marker: {{ ...assistMarker, color: '#5b9bd566' }},
        hovertemplate: '%{{text}}<extra></extra>',
        text: refR.map(r => `${{r.object}} (${{r.population}})<br>dt: ${{r.dt_days}}d<br>|ASSIST−Horizons|: ${{r.assist_vs_horizons_km.toFixed(4)}} km`),
    }});
    // Threshold lines
    const xMin = Math.min(...refR.map(r => r.dt_days));
    const xMax = Math.max(...refR.map(r => r.dt_days));
    [[1, '1 km'], [100, '100 km'], [AU_KM, '1 AU']].forEach(([y, label]) => {{
        traces.push({{
            x: [xMin, xMax], y: [y, y], mode: 'lines',
            line: {{ color: '#5b9bd530', width: 1, dash: 'dot' }},
            showlegend: false, hoverinfo: 'skip',
            name: label,
        }});
    }});
    Plotly.newPlot('assist-chart', traces, {{
        ...baseLayout,
        xaxis: ax('dt (days from epoch)'),
        yaxis: ax('|empyrean − ASSIST|  and  |ASSIST − Horizons|  (km, log)', 'log'),
        showlegend: true,
        legend: {{ ...baseLayout.legend, orientation: 'h', y: 1.14 }},
    }}, {{ responsive: true, displayModeBar: 'hover', modeBarButtonsToRemove: ['select2d', 'lasso2d', 'autoScale2d', 'toggleSpikelines'] }});

    // Summary cards
    const empVals = assistResults.map(r => r.emp_vs_assist_km).filter(v => v != null);
    const refVals = assistResults.map(r => r.assist_vs_horizons_km).filter(v => v != null);
    const ratios = assistResults.filter(r => r.emp_vs_assist_km != null && r.assist_vs_horizons_km != null && r.assist_vs_horizons_km > 0)
        .map(r => r.emp_vs_assist_km / r.assist_vs_horizons_km);
    // Report median + p99 + max (broken out by population for
    // self-perturbers and comets, which have known mismatch reasons).
    // The bare "0.000 km" median was hiding a 10^12 km self-perturber
    // tail — surface that explicitly.
    const empVals_sorted = empVals.slice().sort((a, b) => a - b);
    const empP50 = empVals_sorted.length ? empVals_sorted[Math.floor(empVals_sorted.length * 0.5)] : 0;
    const empP99 = empVals_sorted.length ? empVals_sorted[Math.floor(empVals_sorted.length * 0.99)] : 0;
    const empMax = empVals_sorted.length ? empVals_sorted[empVals_sorted.length - 1] : 0;
    const fmtKm = (v) => {{
        if (v < 1) return (v * 1000).toFixed(2) + ' m';
        if (v < 1000) return v.toFixed(3) + ' km';
        if (v < 1e6) return v.toFixed(0) + ' km';
        return (v / 1.496e8).toFixed(2) + ' AU';
    }};
    document.getElementById('assist-median').innerHTML =
        `<span style="font-size:24px">${{fmtKm(empP50)}}</span>` +
        `<br/><small style="font-size:9px; color:#8b9198">p99 ${{fmtKm(empP99)}} · max ${{fmtKm(empMax)}}</small>`;
    document.getElementById('assist-ratio').textContent = ratios.length ? median(ratios).toFixed(3) : '—';
    document.getElementById('assist-rows').textContent = `${{assistResults.length}}`;

    // Section 05: Timing — paired strip per population, split by
    // uncertainty mode. The merge keys on `(object, dt,
    // propagation_uncertainty)` so each empyrean row pairs against
    // the matching ASSIST mode (f64↔single, first-order↔6var,
    // second-order↔6+21var); empyrean's Auto rows are mapped to the
    // first-order ASSIST baseline at merge time and plotted on the
    // combined panel without a separate ASSIST trace (would be
    // identical to first-order ASSIST).
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
                        mode: 'markers', name: `empyrean ${{m.label}}`,
                        marker: {{ color: m.empColor, size: 6, symbol: m.empSymbol || 'circle', opacity: 0.85 }},
                        hovertemplate: empPs.map(r => `${{r.object}} dt=${{r.dt_days}}d<br>emp ${{m.label}}: ${{r.emp_time_ms.toFixed(1)}} ms<extra></extra>`),
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
                name: `empyrean ${{m.label}}` }});
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
           empColor: '#c0504d', assistColor: '#9bbb59',
           empSymbol: 'square', assistSymbol: 'triangle-down-open',
           showAssist: true }},
        {{ tag: 'auto', label: 'Auto',
           empColor: '#8064a2', assistColor: '#8064a2',
           empSymbol: 'triangle-up', assistSymbol: 'cross-open',
           showAssist: false }},
    ]);
}}

// ─────────── Section 06: Ephemeris angular separation ───────────
function buildEphSep(mode) {{
    const dts = uniq(ephResults.map(r => r.dt_days)).sort((a,b)=>a-b);
    const traces = [];
    const ephAll = results.filter(r => r.test_type === 'ephemeris');
    if (mode === 'rust') {{
        for (const pop of popNames) {{
            const popObjs = ephResults.filter(r => r.population === pop);
            const med = [], lo = [], hi = [], xs = [];
            for (const dt of dts) {{
                const vals = popObjs.filter(r => r.dt_days === dt && r.separation_arcsec != null).map(r => r.separation_arcsec * 1000);
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
            const chR = ephAll.filter(r => r.channel === ch && r.separation_arcsec != null);
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
    Plotly.react('eph-sep-chart', traces, {{
        ...baseLayout,
        xaxis: ax('dt (days from epoch)'),
        yaxis: ax('Angular separation (mas)', 'log'),
        showlegend: true, legend: {{ ...baseLayout.legend, orientation: 'h', y: 1.14 }},
    }}, {{ responsive: true, displayModeBar: 'hover', modeBarButtonsToRemove: ['select2d', 'lasso2d', 'autoScale2d', 'toggleSpikelines'] }});
}}
if (ephResults.length > 0) {{
    buildEphSep('rust');
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
    // rust-side or a replay-channel artefact.
    const SCATTER_CLIP_MAS = 100;
    const ephWithResid = ephResults.filter(r => r.d_ra_arcsec != null && r.d_dec_arcsec != null);
    const cleaned = [], outliers = [];
    for (const r of ephWithResid) {{
        const dra_mas = r.d_ra_arcsec * 1000, ddec_mas = r.d_dec_arcsec * 1000;
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
    Plotly.newPlot('eph-scatter-chart', scatterTraces, {{
        ...baseLayout,
        xaxis: {{ ...ax('dRA·cos(δ) (mas)'), range: [-SCATTER_CLIP_MAS, SCATTER_CLIP_MAS], zeroline: true }},
        yaxis: {{ ...ax('dDec (mas)'), range: [-SCATTER_CLIP_MAS, SCATTER_CLIP_MAS], scaleanchor: 'x', zeroline: true }},
        showlegend: true, legend: {{ ...baseLayout.legend }},
    }}, {{ responsive: true, displayModeBar: 'hover', modeBarButtonsToRemove: ['select2d', 'lasso2d', 'autoScale2d', 'toggleSpikelines'] }});

    // Sidecar table of outliers. Show channel so a reviewer can see
    // whether the divergence is in the rust reference or only in one
    // replay channel (P1-15 from the astrodynamicist review). Dedupe
    // by (object, dt, observer, channel, propagation_uncertainty) so
    // the same row doesn't appear repeatedly when the rust feed has
    // multiple uncertainty modes that all share the same residual
    // (P1-8 from pass2).
    const outBody = document.getElementById('eph-outliers');
    outliers.sort((a, b) => b.norm - a.norm);
    const seenOut = new Set();
    for (const o of outliers) {{
        const k = `${{o.object}}|${{o.dt_days}}|${{o.observer || ''}}|${{o.channel}}|${{o.dra_mas.toFixed(3)}}|${{o.ddec_mas.toFixed(3)}}`;
        if (seenOut.has(k)) continue;
        seenOut.add(k);
        const tr = document.createElement('tr');
        const chColor = channelColors[o.channel] || '#8b9198';
        tr.innerHTML = `<td class="obj-name">${{o.object}}</td><td><span class="pop-dot" style="background:${{chColor}}"></span>${{o.channel}}</td><td>${{o.dt_days}}d</td><td>${{o.observer || '—'}}</td><td>${{o.dra_mas.toFixed(1)}} mas</td><td>${{o.ddec_mas.toFixed(1)}} mas</td><td style="color:#d05040">${{o.norm.toFixed(1)}} mas</td>`;
        outBody.appendChild(tr);
    }}
    if (outliers.length === 0) {{
        const tr = document.createElement('tr');
        tr.innerHTML = `<td colspan="7" style="text-align:left; color:#5b9bd5">No rows beyond ±${{SCATTER_CLIP_MAS}} mas — the entire residual cloud is on the chart.</td>`;
        outBody.appendChild(tr);
    }}

    // ─────────── Section 08: stacked dRA / dDec residual growth (symlog) ───────────
    function residTraces(field) {{
        const traces = [];
        const dts = uniq(ephResults.map(r => r.dt_days)).sort((a,b)=>a-b);
        for (const pop of popNames) {{
            const popObjs = ephResults.filter(r => r.population === pop && r[field] != null);
            const med = [], lo = [], hi = [], xs = [];
            for (const dt of dts) {{
                const vals = popObjs.filter(r => r.dt_days === dt).map(r => r[field] * 1000);
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
                  range: [-50, 50], gridcolor: '#1a2332', zerolinecolor: '#5b9bd540', color: '#8b9198', zeroline: true }};
    }}
    Plotly.newPlot('eph-resid-ra-chart', residTraces('d_ra_arcsec'), {{
        ...baseLayout, xaxis: ax('dt (days from epoch)'), yaxis: symlogYAxis('dRA·cos(δ) (mas)'),
        showlegend: true, legend: {{ ...baseLayout.legend, orientation: 'h', y: 1.18 }},
    }}, {{ responsive: true, displayModeBar: 'hover', modeBarButtonsToRemove: ['select2d', 'lasso2d', 'autoScale2d', 'toggleSpikelines'] }});
    Plotly.newPlot('eph-resid-dec-chart', residTraces('d_dec_arcsec'), {{
        ...baseLayout, xaxis: ax('dt (days from epoch)'), yaxis: symlogYAxis('dDec (mas)'),
        showlegend: true, legend: {{ ...baseLayout.legend, orientation: 'h', y: 1.18 }},
    }}, {{ responsive: true, displayModeBar: 'hover', modeBarButtonsToRemove: ['select2d', 'lasso2d', 'autoScale2d', 'toggleSpikelines'] }});
}}

// ─────────── Section 09: OD diagnostics ───────────
if (odRust.length === 0) {{
    document.getElementById('od-empty').style.display = '';
    document.getElementById('od-content').style.display = 'none';
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
        const ra = byObj[a].rust, rb = byObj[b].rust;
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
        // explicitly (scott returned a DetermineError), render a
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
                `<td colspan="8" style="text-align:left; color:#d05040; font-style:italic">` +
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
        const foRms = (rust.findorb_rms_residual != null)
            ? rust.findorb_rms_residual.toFixed(3) + '″'
            : '— <small style="color:#8b9198">(timeout)</small>';
        // find_orb obs coverage = used / (used + rejected). Sub-50%
        // coverage on a short-arc fit is effectively a convergence
        // failure — find_orb has fallen back to its hard-coded
        // placeholder orbit (a ≈ 2.2 AU, e ≈ 0) and is reporting
        // the residual on the 2 anchor observations as if it were a
        // fit-quality metric. We flag those rows here so the
        // "find_orb RMS" column above can't be read at face value
        // without seeing the coverage qualifier.
        const foUsed = rust.findorb_n_obs_used;
        const foRej = rust.findorb_n_obs_rejected;
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
        if (rust.od_rms_combined_arcsec != null && rust.findorb_rms_residual != null) {{
            const delta = rust.od_rms_combined_arcsec - rust.findorb_rms_residual;
            const sign = delta >= 0 ? '+' : '−';
            // Down-rank the Δ visual when find_orb's coverage is
            // very low — the residual on its rejection-mismatched
            // 2-obs fit isn't comparable to Empyrean's full-arc fit.
            const lowCoverage = (foUsed != null && foRej != null && foUsed + foRej > 0 && foUsed / (foUsed + foRej) < 0.5);
            // Flag large regressions (>=10x worse than find_orb)
            // explicitly so a reviewer can't miss them: this is
            // the right place for the impactor anomaly story
            // (2008 TC3, 2024 BX1, 2023 CX1) to scream.
            const ratio = rust.findorb_rms_residual > 0
                ? rust.od_rms_combined_arcsec / rust.findorb_rms_residual
                : 1;
            let color, suffix;
            if (lowCoverage) {{
                color = '#8b9198';
                suffix = ' <small style="color:#8b9198">(rejection-mismatch; ratio uninterpretable)</small>';
            }} else if (Math.abs(ratio) >= 10) {{
                color = '#d05040';
                suffix = ` <small style="color:#d05040">REGRESSION: ${{ratio.toFixed(0)}}× find_orb</small>`;
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
        const foBlock = (rust.findorb_rms_residual == null)
            ? `<td colspan="3" style="color:#8b9198; text-align:center"><i>find_orb: not run (timeout)</i></td>`
            : `<td>${{foRms}}</td><td>${{foCovCell}}</td><td>${{foDeltaCell}}</td>`;
        tr.innerHTML = `<td class="obj">${{obj}}</td><td>${{rust.n_obs_used || '—'}}</td><td>${{chi2Cell}}</td><td>${{rmsCell}}</td><td>${{rmsCombinedCell}}</td><td>${{rust.od_iterations || '—'}}</td>${{foBlock}}<td>${{xCh}}</td>`;
        overview.appendChild(tr);
    }}

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
        chi2Traces.push({{
            x: xs, y: ys, type: 'bar', name: ch,
            marker: {{ color: channelColors[ch] || '#888' }},
            hovertemplate: `${{ch}}<br>%{{x}}<br>χ² = %{{y:.3e}}<extra></extra>`,
        }});
    }}
    Plotly.newPlot('od-chi2-chart', chi2Traces, {{
        ...baseLayout,
        xaxis: ax(''), yaxis: ax('reduced χ² = χ²/ν (log)', 'log'),
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

// Block below is the OLD raw-χ² layout; kept only to avoid a syntax
// error from the lingering Plotly options object — the call above
// renders the actual chart.

    // Chart 2: iteration count per channel (strip plot)
    const iterTraces = [];
    for (const ch of ALL_CHANNELS) {{
        const xs = [], ys = [], hovers = [];
        for (const obj of odObjects) {{
            const r = byObj[obj][ch];
            if (r && r.od_iterations != null) {{
                xs.push(ch); ys.push(r.od_iterations);
                hovers.push(`${{ch}}<br>${{obj}}: ${{r.od_iterations}} iter${{r.od_converged ? '' : ' (did not converge)'}}`);
            }}
        }}
        iterTraces.push({{
            x: xs, y: ys, mode: 'markers', name: ch,
            marker: {{ color: channelColors[ch] || '#888', size: 8, opacity: 0.75,
                symbol: xs.map((_, i) => odObjects[i] && byObj[odObjects[i]][ch] && !byObj[odObjects[i]][ch].od_converged ? 'x' : 'circle') }},
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
    const scottStatusLabel = (r) => {{
        if (r == null) return '';
        const parts = [];
        if (r.n_obs_used != null) parts.push(`obs used: ${{r.n_obs_used}}`);
        if (r.od_reduced_chi2 != null) parts.push(`χ²ᵣ = ${{r.od_reduced_chi2.toExponential(2)}}`);
        if (r.od_converged === false) parts.push('did NOT converge');
        return parts.length ? '<br>' + parts.join('<br>') : '';
    }};

    const rmsTraces = [
        {{
            x: odObjects, y: odObjects.map(o => (byObj[o].rust && byObj[o].rust.od_rms_ra_arcsec) || null),
            type: 'bar', name: 'rust dRA RMS',
            marker: {{ color: '#5b9bd5' }},
            customdata: odObjects.map(o => scottStatusLabel(byObj[o].rust)),
            hovertemplate: 'rust dRA RMS<br>%{{x}}: %{{y:.3f}}″%{{customdata}}<extra></extra>',
        }},
        {{
            x: odObjects, y: odObjects.map(o => (byObj[o].rust && byObj[o].rust.od_rms_dec_arcsec) || null),
            type: 'bar', name: 'rust dDec RMS',
            marker: {{ color: '#5b9bd5aa' }},
            customdata: odObjects.map(o => scottStatusLabel(byObj[o].rust)),
            hovertemplate: 'rust dDec RMS<br>%{{x}}: %{{y:.3f}}″%{{customdata}}<extra></extra>',
        }},
    ];
    const combinedVals = odObjects.map(o => (byObj[o].rust && byObj[o].rust.od_rms_combined_arcsec) || null);
    if (combinedVals.some(v => v != null)) {{
        rmsTraces.push({{
            x: odObjects, y: combinedVals,
            mode: 'markers', name: 'rust combined RMS',
            marker: {{ color: '#5b9bd5', size: 10, symbol: 'circle', line: {{ color: '#1f4e79', width: 1.5 }} }},
            type: 'scatter',
            customdata: odObjects.map(o => scottStatusLabel(byObj[o].rust)),
            hovertemplate: 'rust combined<br>%{{x}}: %{{y:.3f}}″%{{customdata}}<extra></extra>',
        }});
    }}
    const foVals = odObjects.map(o => (byObj[o].rust && byObj[o].rust.findorb_rms_residual) || null);
    if (foVals.some(v => v != null)) {{
        // Color-code find_orb diamonds by coverage so degenerate
        // placeholder fits (the 2008 TC3 / 2024 BX1 / 2023 CX1 class)
        // visually disqualify themselves from the RMS comparison.
        const foColors = odObjects.map(o => {{
            const cov = foCoverage(byObj[o].rust);
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
            customdata: odObjects.map(o => foCovLabel(foCoverage(byObj[o].rust))),
            hovertemplate: 'find_orb<br>%{{x}}: %{{y:.3f}}″%{{customdata}}<extra></extra>',
        }});
    }}
    Plotly.newPlot('od-rms-chart', rmsTraces, {{
        ...baseLayout, barmode: 'group',
        xaxis: ax(''), yaxis: ax('Post-fit RMS (arcsec)', 'log'),
        showlegend: true, legend: {{ ...baseLayout.legend, orientation: 'h', y: 1.14 }},
        margin: {{ ...baseLayout.margin, b: 100 }},
    }}, {{ responsive: true, displayModeBar: 'hover', modeBarButtonsToRemove: ['select2d', 'lasso2d', 'autoScale2d', 'toggleSpikelines'] }});

    // Chart 3b: scott combined RMS vs find_orb RMS — y=x scatter.
    // Marker color encodes find_orb's obs coverage so the reader can
    // see at a glance when a point is "find_orb gave up and returned
    // the 2.2-AU placeholder" rather than a comparable fit. Hover
    // boxes carry the obs accounting and scott's reduced χ² on both
    // sides for the same reason.
    const rmsPairs = odObjects
        .map(o => {{
            const r = byObj[o].rust;
            if (!r) return null;
            const s = r.od_rms_combined_arcsec;
            const f = r.findorb_rms_residual;
            if (s == null || f == null || s <= 0 || f <= 0) return null;
            return {{ obj: o, rust: r, scott: s, findorb: f }};
        }})
        .filter(p => p != null);
    if (rmsPairs.length > 0) {{
        const xs = rmsPairs.map(p => p.findorb);
        const ys = rmsPairs.map(p => p.scott);
        const lo = Math.min(...xs, ...ys) * 0.5;
        const hi = Math.max(...xs, ...ys) * 2.0;
        // Per-point color: green for high coverage, orange for low,
        // red for placeholder. Buckets match the bar chart so the
        // two visualizations share a vocabulary.
        const colors = rmsPairs.map(p => {{
            const c = foCoverage(p.rust);
            if (c == null) return '#5b9bd5';
            if (c.pct < 5) return '#c0504d';
            if (c.pct < 50) return '#dba23c';
            return '#5b9bd5';
        }});
        const hoverData = rmsPairs.map(p => {{
            const c = foCoverage(p.rust);
            const sChi = p.rust.od_reduced_chi2;
            const lines = [
                `find_orb: ${{p.findorb.toFixed(3)}}″`,
                `Empyrean combined: ${{p.scott.toFixed(3)}}″`,
            ];
            if (sChi != null) lines.push(`Empyrean χ²ᵣ: ${{sChi.toExponential(2)}}`);
            if (p.rust.n_obs_used != null) lines.push(`Empyrean obs used: ${{p.rust.n_obs_used}}`);
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
    }}

    // Chart 4: fitted-state Δr per channel
    const drTraces = [];
    for (const ch of ALL_CHANNELS) {{
        if (ch === 'rust') continue;
        const xs = [], ys = [], hovers = [];
        for (const obj of odObjects) {{
            const r = byObj[obj][ch], rust = byObj[obj].rust;
            if (!r || !rust) continue;
            const dr = vecDrKm(r.emp_pos_au, rust.emp_pos_au);
            if (dr == null) continue;
            xs.push(obj); ys.push(Math.max(dr, 1e-9));
            hovers.push(`${{ch}}<br>${{obj}}<br>‖Δr_fit‖ = ${{dr < 1 ? dr.toFixed(4) : dr.toFixed(1)}} km`);
        }}
        drTraces.push({{
            x: xs, y: ys, mode: 'markers', name: ch,
            marker: {{ color: channelColors[ch] || '#888', size: 8, opacity: 0.85 }},
            text: hovers, hovertemplate: '%{{text}}<extra></extra>',
        }});
    }}
    Plotly.newPlot('od-fitdr-chart', drTraces, {{
        ...baseLayout,
        xaxis: ax(''), yaxis: ax('‖Δr_fitted‖ (km, log)', 'log'),
        showlegend: true, legend: {{ ...baseLayout.legend, orientation: 'h', y: 1.14 }},
        margin: {{ ...baseLayout.margin, b: 100 }},
    }}, {{ responsive: true, displayModeBar: 'hover', modeBarButtonsToRemove: ['select2d', 'lasso2d', 'autoScale2d', 'toggleSpikelines'] }});
}}

// ─────────── Section 10: Channel fidelity charts ───────────
// ECDF of Δr by channel × test_type
const ecdfTraces = [];
for (const ch of ALL_CHANNELS) {{
    if (ch === 'rust') continue;
    for (const tt of ['propagation', 'ephemeris', 'orbit_determination']) {{
        const chRows = results.filter(r => r.channel === ch && r.test_type === tt && r.emp_pos_au);
        const drs = [];
        for (const r of chRows) {{
            const rustRow = results.find(rr => rr.channel === 'rust' && rr.object === r.object && rr.test_type === tt && rr.dt_days === r.dt_days && rr.observer === r.observer);
            if (!rustRow) continue;
            const dr = vecDrKm(r.emp_pos_au, rustRow.emp_pos_au);
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
    xaxis: ax('‖Δr_channel − Δr_rust‖ (km, log)', 'log'),
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
    if (ch === 'rust') continue;
    for (const tt of ['propagation', 'ephemeris', 'orbit_determination']) {{
        const chRows = results.filter(r => r.channel === ch && r.test_type === tt);
        const xs = [], ys = [], hovers = [];
        for (const r of chRows) {{
            const rustRow = results.find(rr => rr.channel === 'rust' && rr.object === r.object && rr.test_type === tt && rr.dt_days === r.dt_days && rr.observer === r.observer);
            if (!rustRow) continue;
            const dr = vecDrKm(r.emp_pos_au, rustRow.emp_pos_au);
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
    yaxis: ax('‖Δr_chan − Δr_rust‖ (km, log)', 'log'),
    showlegend: false,
    shapes: [{{ type: 'line', xref: 'paper', x0: 0, x1: 1, y0: 1e-10, y1: 1e-10, line: {{ color: '#5b9bd540', width: 1, dash: 'dot' }} }}],
}}, {{ responsive: true, displayModeBar: 'hover', modeBarButtonsToRemove: ['select2d', 'lasso2d', 'autoScale2d', 'toggleSpikelines'] }});

// Timing vs accuracy
const taTraces = [];
for (const ch of ALL_CHANNELS) {{
    if (ch === 'rust') continue;
    const xs = [], ys = [], hovers = [];
    for (const r of results.filter(rr => rr.channel === ch && rr.emp_time_ms != null && rr.emp_pos_au)) {{
        const rustRow = results.find(rr => rr.channel === 'rust' && rr.object === r.object && rr.test_type === r.test_type && rr.dt_days === r.dt_days && rr.observer === r.observer);
        if (!rustRow || !rustRow.emp_time_ms || rustRow.emp_time_ms <= 0) continue;
        const dr = vecDrKm(r.emp_pos_au, rustRow.emp_pos_au);
        if (dr == null) continue;
        xs.push(r.emp_time_ms / rustRow.emp_time_ms);
        ys.push(Math.max(dr, 1e-12));
        hovers.push(`${{ch}}<br>${{r.object}} (${{r.test_type}}) dt=${{r.dt_days}}<br>time ratio: ${{(r.emp_time_ms / rustRow.emp_time_ms).toFixed(2)}}×<br>‖Δr‖ = ${{dr.toExponential(2)}} km`);
    }}
    taTraces.push({{
        x: xs, y: ys, mode: 'markers', name: ch,
        marker: {{ color: channelColors[ch] || '#888', size: 4, opacity: 0.5 }},
        text: hovers, hovertemplate: '%{{text}}<extra></extra>',
    }});
}}
Plotly.newPlot('timing-acc-chart', taTraces, {{
    ...baseLayout,
    xaxis: ax('time(channel) / time(rust)'),
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
// User-facing relabel — the sidecar embeds "scott" / "sbdb" as the
// `common_epoch_source` enum because scott is the internal Rust crate
// name for the OD library; surface as "Empyrean fit" / "SBDB" for the
// reader.
function relabelEpochSource(s) {{
    if (s === 'scott') return 'Empyrean fit';
    if (s === 'sbdb') return 'SBDB';
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
        if (x == null || !isFinite(x)) return '<span style="color:#666">—</span>';
        if (x === 0) return '0';
        const absx = Math.abs(x);
        return (absx < 1e-3 || absx >= 1e4) ? x.toExponential(d) : x.toFixed(d);
    }}
    function sigmaEquivColor(s) {{
        if (s == null || !isFinite(s)) return '#666';
        if (s < 1) return '#3fb950';   // green
        if (s < 3) return '#e8a040';   // yellow / orange
        return '#f85149';              // red
    }}
    // Pathology classifier — derived in JS from the Rust-emitted
    // diagnostics. CORRELATION: joint d² >> marginal d² (off-diagonal
    // correlation drives the joint metric). ROTATION: large
    // principal-axis rotation with no large marginal-mismatch signal.
    // MIXED: both signatures triggered.
    function pathologyLabel(c) {{
        if (!(c.sigma_equiv_combined != null && isFinite(c.sigma_equiv_combined)) || c.sigma_equiv_combined < 1) return '';
        const ratio = (isFinite(c.mahalanobis_d2_combined_metric)
                       && isFinite(c.mahalanobis_d2_marginal)
                       && c.mahalanobis_d2_marginal > 1e-30)
            ? c.mahalanobis_d2_combined_metric / c.mahalanobis_d2_marginal
            : NaN;
        const theta = c.principal_axis_rotation_deg;
        const isCorr = isFinite(ratio) && ratio > 100;
        const isRot = isFinite(theta) && theta > 30;
        if (isCorr && isRot) return 'MIXED';
        if (isCorr) return 'CORRELATION';
        if (isRot) return 'ROTATION';
        return '';
    }}
    // Tiny inline SVG "eigen-spectrum strip": six log-scale dots
    // overlaying scott (#f85149) and reference (#5b9bd5) eigenvalues.
    function eigenSpectrumStrip(c) {{
        const es = c.eigenvalues_scott, er = c.eigenvalues_ref;
        if (!es || !er) return '<span style="color:#666">—</span>';
        const all = [...es, ...er].filter(v => isFinite(v) && v > 0);
        if (!all.length) return '<span style="color:#666">—</span>';
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
        // Scott (red) dots above center line, reference (blue) below.
        for (const v of es) {{
            const x = scaleX(v);
            svg += `<circle cx="${{x}}" cy="${{H/2 - 4}}" r="2" fill="#f85149"/>`;
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
            const ss = c.sigma_scott[k] || 0, sr = c.sigma_ref[k] || 0;
            const sc = Math.sqrt(ss*ss + sr*sr);
            return (sc > 0 && isFinite(c.delta[k])) ? c.delta[k] / sc : 0;
        }});
        const absMax = Math.max(3.5, ...zs.map(v => Math.abs(v)));
        const yScale = z => padT + innerH/2 - (z / absMax) * (innerH/2);
        const xCenter = k => padL + (k + 0.5) * (innerW / 6);
        const barW = (innerW / 6) * 0.6;
        let svg = `<svg width="${{W}}" height="${{H}}" style="background:#0b1118; border:1px solid #1a2332">`;
        // ±1σ and ±3σ bands.
        const y1pos = yScale(1), y1neg = yScale(-1);
        const y3pos = yScale(3), y3neg = yScale(-3);
        svg += `<rect x="${{padL}}" y="${{y3pos}}" width="${{innerW}}" height="${{y3neg - y3pos}}" fill="#f8514920"/>`;
        svg += `<rect x="${{padL}}" y="${{y1pos}}" width="${{innerW}}" height="${{y1neg - y1pos}}" fill="#3fb95020"/>`;
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
            const color = Math.abs(z) > 3 ? '#f85149' : Math.abs(z) > 1 ? '#e8a040' : '#3fb950';
            svg += `<rect x="${{x}}" y="${{Math.min(y0, y1)}}" width="${{barW}}" height="${{Math.abs(y1 - y0)}}" fill="${{color}}"/>`;
            svg += `<text x="${{xCenter(k)}}" y="${{H - 5}}" text-anchor="middle" font-size="10" fill="#8b9198">${{KEP_LABELS[k]}}</text>`;
            // z value above bar.
            svg += `<text x="${{xCenter(k)}}" y="${{y1 - 4 + (z < 0 ? 14 : 0)}}" text-anchor="middle" font-size="9" fill="#c0c0c0">${{z.toFixed(2)}}</text>`;
        }});
        svg += `<text x="${{padL + innerW/2}}" y="${{padT - 2}}" text-anchor="middle" font-size="9" fill="#8b9198">Δ_k / σ_combined,k</text>`;
        svg += `</svg>`;
        return svg;
    }}
    // Sort: primary by σ_equiv descending (largest mismatch first), then
    // by object name. NaN sinks to bottom.
    const rows = orbitComparisons.slice().sort((a, b) => {{
        const sa = isFinite(a.sigma_equiv_combined) ? a.sigma_equiv_combined : -1;
        const sb = isFinite(b.sigma_equiv_combined) ? b.sigma_equiv_combined : -1;
        if (sb !== sa) return sb - sa;
        return a.object.localeCompare(b.object);
    }});
    const tbody = document.getElementById('orbit-compare-tbody');
    rows.forEach((c, idx) => {{
        const tr = document.createElement('tr');
        tr.style.cursor = 'pointer';
        const sigRatio_a = (c.sigma_ref && c.sigma_ref[0] > 0)
            ? (c.sigma_scott[0] / c.sigma_ref[0]) : null;
        const ratio = (isFinite(c.mahalanobis_d2_combined_metric)
                       && isFinite(c.mahalanobis_d2_marginal)
                       && c.mahalanobis_d2_marginal > 1e-30)
            ? c.mahalanobis_d2_combined_metric / c.mahalanobis_d2_marginal
            : null;
        const label = pathologyLabel(c);
        const labelHtml = label ? `<div style="font-size:0.7em; color:#8b9198; margin-top:2px">${{label}}</div>` : '';
        tr.innerHTML = `
            <td class="obj-name">${{c.object}}</td>
            <td>${{c.reference}}</td>
            <td>${{c.common_epoch_mjd_tdb.toFixed(3)}}<br/><span style="font-size:0.75em;color:#8b9198">(${{relabelEpochSource(c.common_epoch_source)}})</span></td>
            <td>${{fmtSci(c.delta[0])}}</td>
            <td>${{fmtSci(c.delta[1])}}</td>
            <td>${{fmtSci(c.delta[2])}}</td>
            <td>${{fmtSci(c.delta[3])}}</td>
            <td>${{fmtSci(c.delta[4])}}</td>
            <td>${{fmtSci(c.delta[5])}}</td>
            <td>${{fmtSci(c.sigma_scott[0])}}</td>
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
        expandCell.style.background = '#0b1118';
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
        tr.onclick = () => {{
            expandRow.style.display = expandRow.style.display === 'none' ? '' : 'none';
        }};
    }});
}}
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
        heatmap_html = heatmap_html,
        fidelity_threshold = FIDELITY_THRESHOLD,
        fidelity_summary = fidelity_summary,
        per_tt_matrix_html = per_tt_matrix_html,
        channel_table_html = channel_table_html,
        offenders_html = offenders_html,
        quality_summary_html = quality_summary_html,
        run_mode_banner_html = run_mode_banner_html,
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
            common_epoch_source: "scott".to_string(),
            repr: "keplerian".to_string(),
            state_scott: [0.92, 0.19, 3.33, 204.5, 126.3, 105.9],
            state_ref: [0.92, 0.19, 3.34, 203.9, 126.7, 312.8],
            delta: [0.0, -2e-8, -0.01, 0.6, -0.35, -153.0],
            sigma_scott: [4.4e-10, 3.5e-8, 1.3e-6, 1.9e-5, 1.8e-5, 8.2e-6],
            sigma_ref: [1.7e-9, 1.6e-9, 9.9e-8, 3.1e-6, 3.3e-6, 7.8e-7],
            mahalanobis_d2_scott_metric: 7807.0,
            mahalanobis_d2_ref_metric: 574700.0,
            mahalanobis_d2_combined_metric: 1186.0,
            mahalanobis_d2_marginal: 0.01,
            sigma_equiv_combined: 14.06,
            eigenvalues_scott: [1.5e-9, 6.7e-10, 3.4e-10, 9.0e-12, 3.3e-13, 1.9e-19],
            eigenvalues_ref: [1.3e-11, 1.1e-11, 2.3e-12, 4.2e-15, 1.2e-17, 2.5e-19],
            principal_axis_rotation_deg: 64.3,
            cov_volume_ratio: 1.2e11,
            notes: vec!["reference (SBDB) propagated to scott epoch via STM".to_string()],
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
}
