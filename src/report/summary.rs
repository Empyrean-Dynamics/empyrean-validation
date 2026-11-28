//! The summary layer of the validation report: the masthead, the six comparison
//! cards, the known-exceptions strip and the cite block, which sit above the page
//! body.
//!
//! Every number the layer shows lives in one [`SummaryData`], computed from the
//! run's rows by the same functions that compute the panels the cards link to
//! ([`grid_stat`], [`od_panel_set`], [`fidelity_agg`], [`cov_stats`] and the
//! Part 2 timing readers), and serialized once into the page as JSON. The cards are
//! rendered from that struct, so a card and the panel it summarises cannot
//! disagree. The struct is the seed of the data contract later stages extend: its
//! field names are the contract.
//!
//! No card ranks, orders or labels tools by merit: tools appear in registry order,
//! empyrean first, each with its own numbers.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::*;

/// The data contract's version. Bump it when a field changes meaning.
pub const SUMMARY_SCHEMA: u32 = 1;

/// Where a published run's archive lives; the run's timestamp follows.
pub const ARCHIVE_URL_PREFIX: &str = "https://storage.googleapis.com/empyrean-validation/archive/";

/// The archive link of a run published under `timestamp`.
pub fn archive_url(timestamp: &str) -> String {
    format!("{ARCHIVE_URL_PREFIX}{timestamp}/validation_report.html")
}

/// ASSIST's arm the timing card reads: the first-order arm, the one that matches
/// the derivative order of the empyrean arm the card reads ([`EMP_SECTION1_ARM`]).
const ASSIST_TIMING_ARM: &str = "assist_default_variational_on";

/// The timing card's per-row upper percentile (the Part 2 timing matrix prints p5,
/// p50 and p95).
const TIMING_UPPER_PERCENTILE: u32 = 95;

/// The Gaia single-frame astrometric floor, in milliarcseconds, drawn on the
/// ephemeris card (owner, 2026-11-22). There is no JPL floor on propagation.
const GAIA_FLOOR_MAS: f64 = 1.0;

/// The whole summary layer's data.
#[derive(Debug, Clone, Serialize)]
pub struct SummaryData {
    /// [`SUMMARY_SCHEMA`].
    pub schema: u32,
    pub run: RunInfo,
    pub cards: Cards,
    pub exceptions: Vec<ExceptionChip>,
    pub cite: CiteBlock,
}

/// What this run is.
#[derive(Debug, Clone, Serialize)]
pub struct RunInfo {
    pub product: String,
    pub empyrean_version: String,
    /// The archive timestamp the publish step minted, `None` for a render that is
    /// not published (a PR run, act, local).
    pub run_id: Option<String>,
    pub run_date: String,
    pub epoch_mjd_tdb: f64,
    pub n_objects: usize,
    pub n_populations: usize,
    pub n_horizons: usize,
    /// The first and last horizon, in years from the epoch.
    pub horizon_min_years: f64,
    pub horizon_max_years: f64,
}

/// The six cards, in the order the page shows them.
#[derive(Debug, Clone, Serialize)]
pub struct Cards {
    pub propagation: RangeCard,
    pub ephemeris: RangeCard,
    pub orbit_determination: FitCard,
    pub timing: RangeCard,
    pub channel_fidelity: FidelityCard,
    pub uncertainty: UncertaintyCard,
}

/// A card that draws each tool as a range strip on one shared log scale: a median
/// dot and an upper-percentile tick.
#[derive(Debug, Clone, Serialize)]
pub struct RangeCard {
    /// The section anchor the card links to.
    pub anchor: String,
    pub subtitle: String,
    /// The label of the upper statistic (`p90`, `p95`).
    pub upper_label: String,
    /// The shared scale, `None` when no tool has a number.
    pub axis: Option<LogAxis>,
    /// A reference floor drawn as a dashed line, where the axis has one.
    pub floor: Option<Floor>,
    pub tools: Vec<RangeRow>,
    /// A footnote for rows measured on a different boundary, `None` when none.
    pub footnote: Option<String>,
}

/// A shared log10 scale: the decades the strips span.
#[derive(Debug, Clone, Serialize)]
pub struct LogAxis {
    pub lo_decade: i32,
    pub hi_decade: i32,
    pub lo_text: String,
    pub hi_text: String,
}

/// A reference floor on a range card.
#[derive(Debug, Clone, Serialize)]
pub struct Floor {
    pub value: f64,
    pub label: String,
    pub text: String,
}

/// One tool's strip.
#[derive(Debug, Clone, Serialize)]
pub struct RangeRow {
    pub slug: String,
    pub label: String,
    pub median: Option<f64>,
    pub upper: Option<f64>,
    pub median_text: Option<String>,
    pub upper_text: Option<String>,
    /// How many cells (rows) the statistic is over.
    pub n: usize,
    /// Why the tool has no number, or what its number is measured on.
    pub note: Option<String>,
}

/// The orbit-determination card: every tool's fit counts over the same arc set.
#[derive(Debug, Clone, Serialize)]
pub struct FitCard {
    pub anchor: String,
    pub subtitle: String,
    pub tools: Vec<FitRow>,
}

/// One tool's arcs: fit, ran without converging, never attempted.
#[derive(Debug, Clone, Serialize)]
pub struct FitRow {
    pub slug: String,
    pub label: String,
    pub fit: usize,
    pub not_converged: usize,
    pub not_attempted: usize,
    pub total: usize,
}

/// The channel-fidelity card: the 4 x 3 matrix the section-3.1 summary shows.
#[derive(Debug, Clone, Serialize)]
pub struct FidelityCard {
    pub anchor: String,
    pub subtitle: String,
    /// The axes, left to right.
    pub axes: Vec<String>,
    pub channels: Vec<FidelityRow>,
    /// The widest difference on any cell, as the matrix prints it.
    pub max_text: Option<String>,
}

/// One channel's row of the matrix.
#[derive(Debug, Clone, Serialize)]
pub struct FidelityRow {
    pub channel: String,
    /// Whether the run carries any row of this channel.
    pub present: bool,
    pub cells: Vec<Option<FidelityCell>>,
}

/// One channel-by-axis cell.
#[derive(Debug, Clone, Serialize)]
pub struct FidelityCell {
    /// `EXACT`, `ULP`, `TOL` or `DIFF`.
    pub state: String,
    pub n_compared: usize,
    pub n_within_ulp: usize,
    pub max_text: String,
}

/// The uncertainty card: empyrean's σ_eq against JPL at the epoch, with the
/// expected values the covariance-realism table gives.
#[derive(Debug, Clone, Serialize)]
pub struct UncertaintyCard {
    pub anchor: String,
    pub subtitle: String,
    pub n_objects: usize,
    pub n_radar_excluded: usize,
    pub within_1: FractionStat,
    pub within_3: FractionStat,
    pub median: MedianStat,
}

/// An observed fraction against its expected value.
#[derive(Debug, Clone, Serialize)]
pub struct FractionStat {
    pub count: usize,
    pub of: usize,
    /// Percent, `None` when `of` is 0.
    pub percent: Option<f64>,
    pub percent_text: String,
    pub expected_text: String,
    /// The expected value as a percent for drawing its tick.
    pub expected_percent: f64,
}

/// The observed median σ_eq against its expected value.
#[derive(Debug, Clone, Serialize)]
pub struct MedianStat {
    pub observed: Option<f64>,
    pub observed_text: String,
    pub expected_text: String,
}

/// What kind of exception a chip names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExceptionKind {
    /// A fit `ci-check` excuses by name ([`FLOOR_EXCEPTIONS`]), excused.
    FloorExcused,
    /// A floor exception that went unused: the fit converged.
    FloorUnused,
    /// A floor exception keyed to another engine than the row ran on.
    FloorWrongEngine,
    /// Reference radar rows the reference declined.
    ReferenceDeclined,
    /// A comparator that did not attempt some objects on an axis.
    NotAttempted,
    /// find_orb arcs under its observation-coverage floor.
    FindorbCoverageFloor,
    /// layup fits that did not converge, named by its flag.
    LayupNotConverged,
    /// An external tool's radar fit that did not converge.
    ExternalRadarNotConverged,
}

/// One known exception.
#[derive(Debug, Clone, Serialize)]
pub struct ExceptionChip {
    pub kind: ExceptionKind,
    pub tool: Option<String>,
    pub object: Option<String>,
    pub count: Option<usize>,
    pub text: String,
    /// Revealed in place when the chip is opened: one short line per item (empty
    /// for a chip that is only a link).
    pub detail: Vec<String>,
    /// The section anchor that shows it.
    pub anchor: String,
}

/// The cite block.
#[derive(Debug, Clone, Serialize)]
pub struct CiteBlock {
    /// The permanent archive link, `None` for an unpublished render.
    pub archive_url: Option<String>,
    pub engine: Vec<VersionLine>,
    pub tools: Vec<VersionLine>,
    /// The text the copy button copies.
    pub text: String,
}

/// A component and its version.
#[derive(Debug, Clone, Serialize)]
pub struct VersionLine {
    pub name: String,
    pub version: String,
}

/// What the cards need that is not a statistic of the rows' panels: the facts of
/// the run the page states once.
pub struct RunFacts<'a> {
    pub n_objects: usize,
    pub n_populations: usize,
    pub dt_values: &'a [i64],
    pub epoch_mjd_tdb: f64,
    pub run_date: &'a str,
}

/// The registry tools the run carries (JPL excluded): every tool with a row, or
/// handed arcs, in this run.
pub(super) fn tools_in_run(results: &[ValidationResult]) -> BTreeSet<&'static str> {
    let mut present: BTreeSet<&'static str> = BTreeSet::new();
    present.insert("empyrean");
    for spec in crate::tool_registry::externals() {
        if results.iter().any(|r| spec.has_any(r)) || od_tool_present(spec.slug, results) {
            present.insert(spec.slug);
        }
    }
    present
}

/// A strip row from a pair's cells.
fn range_row(spec_slug: &str, label: &str, axis: GridAxis, stat: Option<GridStat>) -> RangeRow {
    match stat {
        Some(s) => RangeRow {
            slug: spec_slug.to_string(),
            label: label.to_string(),
            median: Some(s.p50),
            upper: Some(s.p90),
            median_text: Some(grid_fmt(axis, s.p50)),
            upper_text: Some(grid_fmt(axis, s.p90)),
            n: s.n,
            note: None,
        },
        None => RangeRow {
            slug: spec_slug.to_string(),
            label: label.to_string(),
            median: None,
            upper: None,
            median_text: None,
            upper_text: None,
            n: 0,
            note: None,
        },
    }
}

/// A power of ten as a labelled value on a card's axis: whole numbers with their
/// unit, never a bare exponent.
fn pow10_text(decade: i32) -> String {
    const SUP: [char; 10] = ['⁰', '¹', '²', '³', '⁴', '⁵', '⁶', '⁷', '⁸', '⁹'];
    if (0..=4).contains(&decade) {
        return 10u64.pow(decade as u32).to_string();
    }
    let digits: String = decade
        .unsigned_abs()
        .to_string()
        .chars()
        .filter_map(|c| c.to_digit(10).map(|d| SUP[d as usize]))
        .collect();
    format!("10{}{digits}", if decade < 0 { "⁻" } else { "" })
}

/// The axis end label for `10^decade` of a card's unit: position offsets in
/// metres and kilometres, sky offsets in µas, mas and arcseconds, wall time in
/// µs, ms and seconds.
fn axis_text(axis: GridAxis, decade: i32) -> String {
    match axis {
        GridAxis::PropKm if decade < 0 => format!("{} m", pow10_text(decade + 3)),
        GridAxis::PropKm => format!("{} km", pow10_text(decade)),
        GridAxis::EphMas if decade < 0 => format!("{} {MICRO}as", pow10_text(decade + 3)),
        GridAxis::EphMas if decade < 3 => format!("{} mas", pow10_text(decade)),
        GridAxis::EphMas => format!("{}\u{2033}", pow10_text(decade - 3)),
        GridAxis::TimingMs if decade < 0 => format!("{} {MICRO}s", pow10_text(decade + 3)),
        GridAxis::TimingMs if decade < 3 => format!("{} ms", pow10_text(decade)),
        GridAxis::TimingMs => format!("{} s", pow10_text(decade - 3)),
        GridAxis::SepArcsec => format!("{}\u{2033}", pow10_text(decade)),
    }
}

/// The shared log scale of a card's strips: the decades from the smallest to the
/// largest positive number, widened to at least one decade.
fn log_axis(rows: &[RangeRow], floor: Option<f64>, axis: GridAxis) -> Option<LogAxis> {
    let mut vals: Vec<f64> = rows
        .iter()
        .flat_map(|r| [r.median, r.upper])
        .flatten()
        .filter(|v| v.is_finite() && *v > 0.0)
        .collect();
    vals.extend(floor.filter(|f| *f > 0.0));
    let lo = vals.iter().cloned().fold(f64::INFINITY, f64::min);
    let hi = vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    if !lo.is_finite() || !hi.is_finite() {
        return None;
    }
    let lo_decade = lo.log10().floor() as i32;
    let mut hi_decade = hi.log10().ceil() as i32;
    if hi_decade <= lo_decade {
        hi_decade = lo_decade + 1;
    }
    Some(LogAxis {
        lo_decade,
        hi_decade,
        lo_text: axis_text(axis, lo_decade),
        hi_text: axis_text(axis, hi_decade),
    })
}

/// The propagation or ephemeris card: every tool's cells against JPL, the
/// statistic the 1.1 and 1.2 panels print for that pair.
fn grid_card(
    anchor: &str,
    subtitle: &str,
    axis: GridAxis,
    tools: Vec<GridTool>,
    floor: Option<Floor>,
) -> RangeCard {
    let rows: Vec<RangeRow> = tools
        .iter()
        .filter(|t| t.present && t.ref_slug == "jpl")
        .map(|t| range_row(&t.tool_slug, &t.label, axis, grid_stat(&t.vals)))
        .collect();
    let axis_scale = log_axis(&rows, floor.as_ref().map(|f| f.value), axis);
    RangeCard {
        anchor: anchor.to_string(),
        subtitle: subtitle.to_string(),
        upper_label: "p90".to_string(),
        axis: axis_scale,
        floor,
        tools: rows,
        footnote: None,
    }
}

/// A timing strip from a [`TStat`].
fn timing_row(slug: &str, label: &str, stat: Option<TStat>, note: Option<&str>) -> RangeRow {
    match stat {
        Some(s) => RangeRow {
            slug: slug.to_string(),
            label: label.to_string(),
            median: Some(s.p50),
            upper: Some(s.p95),
            median_text: Some(fmt_ms(s.p50)),
            upper_text: Some(fmt_ms(s.p95)),
            n: s.n,
            note: note.map(str::to_string),
        },
        None => RangeRow {
            slug: slug.to_string(),
            label: label.to_string(),
            median: None,
            upper: None,
            median_text: None,
            upper_text: None,
            n: 0,
            note: Some("not timed on propagation".to_string()),
        },
    }
}

/// The timing card: the wall time of one whole propagation call, median and p95,
/// for every tool the run carries. Each number is the statistic of the Part 2
/// timing matrix's row for that tool; ASSIST is measured on its integrate-only
/// boundary and says so.
fn timing_card(results: &[ValidationResult], present: &BTreeSet<&'static str>) -> RangeCard {
    let mut rows: Vec<RangeRow> = Vec::new();
    for spec in crate::tool_registry::TOOLS.iter().filter(|t| !t.truth) {
        if !present.contains(spec.slug) {
            continue;
        }
        let row = if spec.slug == "empyrean" {
            let (s, _) = emp_timing(results, "propagation", Some(EMP_SECTION1_ARM));
            timing_row(spec.slug, spec.label, s, None)
        } else if spec.slug == "assist" {
            timing_row(
                spec.slug,
                spec.label,
                assist_timing(results, ASSIST_TIMING_ARM),
                Some("integrate only"),
            )
        } else {
            let stat = spec
                .time
                .and_then(|f| ext_timing(results, "propagation", &|r| (f.get)(r)));
            timing_row(spec.slug, spec.label, stat, None)
        };
        rows.push(row);
    }
    let axis = log_axis(&rows, None, GridAxis::TimingMs);
    let footnote = rows
        .iter()
        .any(|r| r.note.as_deref() == Some("integrate only"))
        .then(|| "integrate only: the integration call, no construction or detection".to_string());
    RangeCard {
        anchor: "s-prop-c".to_string(),
        subtitle: format!("propagation, wall time per call, median and p{TIMING_UPPER_PERCENTILE}"),
        upper_label: format!("p{TIMING_UPPER_PERCENTILE}"),
        axis,
        floor: None,
        tools: rows,
        footnote,
    }
}

/// The orbit-determination card, from the panels the 1.3 section prints: one row per
/// tool, counted over every arc the panels list.
fn od_card(panels: &OdPanelSet<'_>) -> FitCard {
    let total = panels.panels.first().map(|p| p.counts.total()).unwrap_or(0);
    FitCard {
        anchor: "s08b".to_string(),
        subtitle: format!("arcs fit of {total}, vs JPL SBDB"),
        tools: panels
            .panels
            .iter()
            .map(|p| FitRow {
                slug: p.tool_slug.to_string(),
                label: p.tool_label.to_string(),
                fit: p.counts.fit,
                not_converged: p.counts.not_converged,
                not_attempted: p.counts.not_attempted,
                total: p.counts.total(),
            })
            .collect(),
    }
}

/// The channel-fidelity card, from the aggregate the 3.1 matrix prints.
fn fidelity_card(results: &[ValidationResult]) -> FidelityCard {
    let agg = fidelity_agg(results);
    let mut widest: Option<(f64, String)> = None;
    let channels: Vec<FidelityRow> = FID_CHANNELS
        .iter()
        .map(|&ch| {
            let present = agg.present.contains(ch);
            let cells = FID_TEST_TYPES
                .iter()
                .map(|&(tt, _, _, _, _)| {
                    if !present {
                        return None;
                    }
                    agg.cell
                        .get(&(ch, tt))
                        .map(|(st, _objs, ncmp, nbit, maxv)| {
                            let text = fid_amount_text(tt, *maxv);
                            if widest.as_ref().is_none_or(|(w, _)| maxv > w) {
                                widest = Some((*maxv, text.clone()));
                            }
                            FidelityCell {
                                state: st.label().to_string(),
                                n_compared: *ncmp,
                                n_within_ulp: *nbit,
                                max_text: text,
                            }
                        })
                })
                .collect();
            FidelityRow {
                channel: ch.to_string(),
                present,
                cells,
            }
        })
        .collect();
    FidelityCard {
        anchor: "s10".to_string(),
        subtitle: "bindings vs core".to_string(),
        axes: FID_TEST_TYPES
            .iter()
            .map(|&(_, disp, _, _, _)| disp.to_string())
            .collect(),
        channels,
        max_text: widest.map(|(_, t)| t),
    }
}

/// The uncertainty card, from the statistics the 4.1 table prints.
fn uncertainty_card(
    results: &[ValidationResult],
    orbit_comparisons: &[crate::schema::OrbitComparison],
) -> UncertaintyCard {
    let recs = sbdb_records(orbit_comparisons);
    let radar_counts = jpl_radar_obs_counts(results);
    let c = cov_stats(&recs, &radar_counts);
    let frac = |count: usize, expected_text: &str, expected_percent: f64| {
        let percent = (c.n_finite > 0).then(|| 100.0 * count as f64 / c.n_finite as f64);
        FractionStat {
            count,
            of: c.n_finite,
            percent,
            percent_text: percent
                .map(|p| format!("{p:.0}%"))
                .unwrap_or_else(|| "n/a".to_string()),
            expected_text: expected_text.to_string(),
            expected_percent,
        }
    };
    let observed = c.med.is_finite().then_some(c.med);
    UncertaintyCard {
        anchor: "s12".to_string(),
        subtitle: format!("σ_eq vs JPL at epoch, {} objects", c.n_finite),
        n_objects: c.n_finite,
        n_radar_excluded: c.n_radar_excl_objs,
        within_1: frac(c.le1, COV_EXPECTED_LE1, 57.7),
        within_3: frac(c.le3, COV_EXPECTED_LE3, 100.0),
        median: MedianStat {
            observed,
            observed_text: format!("{:.2}", c.med),
            expected_text: COV_EXPECTED_MEDIAN.to_string(),
        },
    }
}

/// The objects a panel did not attempt: an object whose optical arc, or its radar
/// arc, has no fit state. Also the count of objects the panel lists.
fn objects_not_attempted<'a>(
    set: &OdPanelSet<'a>,
    panel: &OdPanel<'a>,
) -> (BTreeSet<&'a str>, usize) {
    let mut not_attempted = BTreeSet::new();
    let mut of = 0;
    for (_, objs) in &set.block_order {
        for &o in objs {
            of += 1;
            let optical_na = panel.optical.get(o).and_then(|c| c.converged).is_none();
            let radar_na = set.radar_objs.contains(o)
                && panel.radar.get(o).and_then(|c| c.converged).is_none();
            if optical_na || radar_na {
                not_attempted.insert(o);
            }
        }
    }
    (not_attempted, of)
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The phrase naming the fit a floor exception covers.
fn exception_fit_phrase(test_type: &str) -> String {
    if test_type == crate::schema::test_types::ORBIT_DETERMINATION_RADAR {
        "optical+radar fit".to_string()
    } else {
        format!("{} fit", test_type.replace('_', " "))
    }
}

/// The known exceptions of this run, derived from the rows by the code `ci-check`
/// uses ([`floor_exception_verdicts`] and [`reference_declined_rows`]) and by the
/// functions the OD panels and the convergence matrix use. No log text is read.
fn known_exceptions(
    results: &[ValidationResult],
    od: &OdPanelSet<'_>,
    n_objects: usize,
) -> Vec<ExceptionChip> {
    let mut chips: Vec<ExceptionChip> = Vec::new();

    // The fits ci-check excuses by name, and what it decided on each.
    for (e, verdict) in floor_exception_verdicts(results) {
        let what = format!("{} {}", e.object, exception_fit_phrase(e.test_type));
        let (kind, text) = match verdict {
            FloorExceptionVerdict::Excused => (
                ExceptionKind::FloorExcused,
                format!("{what} excused on {EMPYREAN_VERSION}"),
            ),
            FloorExceptionVerdict::Unused => (
                ExceptionKind::FloorUnused,
                format!("{what} converged, so its floor exception went unused"),
            ),
            FloorExceptionVerdict::WrongEngine { .. } => (
                ExceptionKind::FloorWrongEngine,
                format!("{what} ran on another engine than its floor exception is keyed to"),
            ),
        };
        chips.push(ExceptionChip {
            kind,
            tool: Some("empyrean".to_string()),
            object: Some(e.object.to_string()),
            count: None,
            text,
            detail: Vec::new(),
            anchor: "s08b".to_string(),
        });
    }

    // The reference radar rows the reference declined, as the summary names them.
    let declined = reference_declined_rows(results).len();
    if declined > 0 {
        chips.push(ExceptionChip {
            kind: ExceptionKind::ReferenceDeclined,
            tool: Some("core".to_string()),
            object: None,
            count: Some(declined),
            text: format!(
                "{} declined",
                plural(
                    declined,
                    "core radar reference row",
                    "core radar reference rows"
                )
            ),
            detail: Vec::new(),
            anchor: "s08b".to_string(),
        });
    }

    // Comparators that did not attempt some objects: on the fit axis (every arc,
    // radar included) and where a tool named objects unsupported on propagation or
    // ephemeris. One chip per distinct breakdown; tools with the same breakdown
    // share it.
    type Axes<'x> = Vec<(&'static str, &'static str, BTreeSet<&'x str>)>;
    let mut by_tool: Vec<(&'static str, &'static str, Axes<'_>)> = Vec::new();
    let od_axis = "orbit determination";
    for spec in crate::tool_registry::externals() {
        let mut axes: Axes<'_> = Vec::new();
        if let Some(panel) = od.panels.iter().find(|p| p.tool_slug == spec.slug) {
            let (objs, _) = objects_not_attempted(od, panel);
            if !objs.is_empty() {
                axes.push((od_axis, "s08b", objs));
            }
        }
        for (test_type, axis_name, anchor) in [
            ("propagation", "propagation", "s02"),
            ("ephemeris", "ephemeris", "s06"),
        ] {
            let cells = unsupported_cells(results, test_type);
            if let Some(by_cell) = cells.get(spec.slug) {
                let objs: BTreeSet<&str> = results
                    .iter()
                    .filter(|r| r.test_type == test_type)
                    .filter(|r| by_cell.contains_key(&(r.object.clone(), r.dt_days as i64)))
                    .map(|r| r.object.as_str())
                    .collect();
                if !objs.is_empty() {
                    axes.push((axis_name, anchor, objs));
                }
            }
        }
        if !axes.is_empty() {
            by_tool.push((spec.slug, spec.label, axes));
        }
    }
    let mut groups: Vec<(String, Vec<&str>, usize, &str)> = Vec::new();
    for (_slug, label, axes) in &by_tool {
        let union: BTreeSet<&str> = axes
            .iter()
            .flat_map(|(_, _, o)| o.iter().copied())
            .collect();
        let breakdown = if axes.len() == 1 {
            format!(
                "{} of {n_objects} objects not attempted in {}",
                union.len(),
                axes[0].0
            )
        } else {
            let parts: Vec<String> = axes
                .iter()
                .map(|(name, _, o)| format!("{name} {}", o.len()))
                .collect();
            format!(
                "{} of {n_objects} objects not attempted ({})",
                union.len(),
                parts.join(", ")
            )
        };
        // The section that shows the most of them.
        let anchor = axes
            .iter()
            .max_by_key(|(_, _, o)| o.len())
            .map(|(_, a, _)| *a)
            .unwrap_or("s08b");
        match groups.iter_mut().find(|(b, _, _, _)| *b == breakdown) {
            Some((_, labels, _, _)) => labels.push(label),
            None => groups.push((breakdown, vec![label], union.len(), anchor)),
        }
    }
    if !groups.is_empty() {
        let n_tools: usize = groups.iter().map(|(_, labels, _, _)| labels.len()).sum();
        let anchor = groups[0].3;
        let detail: Vec<String> = groups
            .iter()
            .map(|(breakdown, labels, _, _)| format!("{}: {breakdown}", labels.join(", ")))
            .collect();
        chips.push(ExceptionChip {
            kind: ExceptionKind::NotAttempted,
            tool: None,
            object: None,
            count: Some(n_tools),
            text: format!("Not every tool ran every object ({n_tools} tools)"),
            detail,
            anchor: anchor.to_string(),
        });
    }

    // find_orb arcs under its observation-coverage floor.
    for panel in od.panels.iter().filter(|p| p.tool_slug == "findorb") {
        let n = panel
            .optical
            .values()
            .chain(panel.radar.values())
            .filter(|c| c.under_coverage_floor)
            .count();
        if n > 0 {
            chips.push(ExceptionChip {
                kind: ExceptionKind::FindorbCoverageFloor,
                tool: Some(panel.tool_label.to_string()),
                object: None,
                count: Some(n),
                text: format!(
                    "{}: {} under coverage floor",
                    panel.tool_label,
                    plural(n, "arc", "arcs")
                ),
                detail: Vec::new(),
                anchor: "s08b".to_string(),
            });
        }
    }

    // layup fits that did not converge, named by layup's own flag.
    for panel in od.panels.iter().filter(|p| p.tool_slug == "layup") {
        let failed: Vec<&OdArcCell> = panel
            .optical
            .values()
            .chain(panel.radar.values())
            .filter(|c| c.converged == Some(false))
            .collect();
        if failed.is_empty() {
            continue;
        }
        let mut by_flag: BTreeMap<&str, usize> = BTreeMap::new();
        for c in &failed {
            *by_flag
                .entry(c.flag.as_deref().unwrap_or("no flag recorded"))
                .or_default() += 1;
        }
        chips.push(ExceptionChip {
            kind: ExceptionKind::LayupNotConverged,
            tool: Some(panel.tool_label.to_string()),
            object: None,
            count: Some(failed.len()),
            text: format!(
                "{}: {} did not converge",
                panel.tool_label,
                plural(failed.len(), "arc", "arcs")
            ),
            // layup's own flags, one line each, revealed in place.
            detail: by_flag.iter().map(|(f, n)| format!("{f} {n}")).collect(),
            anchor: "s08b".to_string(),
        });
    }

    // An external tool's radar fit that did not converge, by object.
    for panel in od.panels.iter().filter(|p| p.tool_slug != "empyrean") {
        let mut objs: Vec<&str> = panel
            .radar
            .iter()
            .filter(|(_, c)| c.converged == Some(false))
            .map(|(o, _)| *o)
            .collect();
        objs.sort_unstable();
        for o in objs {
            chips.push(ExceptionChip {
                kind: ExceptionKind::ExternalRadarNotConverged,
                tool: Some(panel.tool_label.to_string()),
                object: Some(o.to_string()),
                count: None,
                text: format!(
                    "{}: {o} optical+radar fit did not converge",
                    panel.tool_label
                ),
                detail: Vec::new(),
                anchor: "s08b".to_string(),
            });
        }
    }
    chips
}

/// A tool's recorded version, as the cite block states it: the version string a
/// runner recorded, less the tool's own name where the runner prefixed it
/// (`find_orb 1de55cc`) and less a trailing parenthetical (`4.5.7 (DE440+...)`),
/// which the Configurations appendix keeps whole.
fn cite_version(spec: &crate::tool_registry::ToolSpec, recorded: &str) -> String {
    let mut v = recorded.trim();
    if let Some(i) = v.find(" (") {
        v = v[..i].trim_end();
    }
    for name in [spec.slug, spec.label] {
        if let Some(rest) = v.strip_prefix(name)
            && rest.starts_with(char::is_whitespace)
        {
            v = rest.trim_start();
        }
    }
    v.to_string()
}

/// The cite block: the archive link (when the run is published), the engine
/// versions the run summary states and every tool version the run recorded.
fn cite_block(results: &[ValidationResult], archive_timestamp: Option<&str>) -> CiteBlock {
    let engine = vec![
        VersionLine {
            name: "empyrean".to_string(),
            version: EMPYREAN_VERSION.to_string(),
        },
        VersionLine {
            name: "empyrean-core".to_string(),
            version: VERSION_EMPYREAN_CORE.to_string(),
        },
        VersionLine {
            name: "hyperjet".to_string(),
            version: VERSION_HYPERJET.to_string(),
        },
    ];
    let mut tools: Vec<VersionLine> = Vec::new();
    for spec in crate::tool_registry::externals() {
        let version = if spec.slug == "assist" {
            assist_version(results)
        } else {
            recorded_tool_version(results, spec.slug)
        };
        if let Some(version) = version {
            tools.push(VersionLine {
                name: spec.label.to_string(),
                version: cite_version(spec, &version),
            });
        }
    }
    let url = archive_timestamp.map(archive_url);
    let mut text = match (&url, archive_timestamp) {
        (Some(u), Some(ts)) => format!("empyrean validation report, run {ts}. {u}"),
        _ => "empyrean validation report, an unpublished render (no archive link).".to_string(),
    };
    let join = |v: &[VersionLine]| {
        v.iter()
            .map(|l| format!("{} {}", l.name, l.version))
            .collect::<Vec<_>>()
            .join(", ")
    };
    text.push_str(&format!("\nEngine: {}.", join(&engine)));
    if !tools.is_empty() {
        text.push_str(&format!(
            "\nTools: {}. Reference: JPL Horizons and SBDB.",
            join(&tools)
        ));
    } else {
        text.push_str("\nReference: JPL Horizons and SBDB.");
    }
    CiteBlock {
        archive_url: url,
        engine,
        tools,
        text,
    }
}

/// Compute the whole summary layer's data from the run's rows.
pub fn build_summary(
    results: &[ValidationResult],
    orbit_comparisons: &[crate::schema::OrbitComparison],
    archive_timestamp: Option<&str>,
    facts: &RunFacts<'_>,
) -> SummaryData {
    let present = tools_in_run(results);
    let od = od_panel_set(results, orbit_comparisons);
    let eph_floor = Floor {
        value: GAIA_FLOOR_MAS,
        label: "Gaia".to_string(),
        text: "Gaia single-frame floor, 1 mas".to_string(),
    };
    let cards = Cards {
        propagation: grid_card(
            "s02",
            "position offset vs JPL Horizons",
            GridAxis::PropKm,
            prop_grid_tools(results),
            None,
        ),
        ephemeris: grid_card(
            "s06",
            "sky offset vs JPL Horizons",
            GridAxis::EphMas,
            eph_grid_tools(results),
            Some(eph_floor),
        ),
        orbit_determination: od_card(&od),
        timing: timing_card(results, &present),
        channel_fidelity: fidelity_card(results),
        uncertainty: uncertainty_card(results, orbit_comparisons),
    };
    let (dt_min, dt_max) = facts
        .dt_values
        .iter()
        .fold((i64::MAX, i64::MIN), |(lo, hi), &d| (lo.min(d), hi.max(d)));
    let (dt_min, dt_max) = if facts.dt_values.is_empty() {
        (0, 0)
    } else {
        (dt_min, dt_max)
    };
    SummaryData {
        schema: SUMMARY_SCHEMA,
        run: RunInfo {
            product: "empyrean validation report".to_string(),
            empyrean_version: EMPYREAN_VERSION.to_string(),
            run_id: archive_timestamp.map(str::to_string),
            run_date: facts.run_date.to_string(),
            epoch_mjd_tdb: facts.epoch_mjd_tdb,
            n_objects: facts.n_objects,
            n_populations: facts.n_populations,
            n_horizons: facts.dt_values.len(),
            horizon_min_years: dt_min as f64 / 365.25,
            horizon_max_years: dt_max as f64 / 365.25,
        },
        cards,
        exceptions: known_exceptions(results, &od, facts.n_objects),
        cite: cite_block(results, archive_timestamp),
    }
}

// ───────────────────────── rendering ─────────────────────────

/// The summary layer's style. It reads the page's own tokens, so both themes
/// follow the theme toggle; no text is under 11 px; numbers are JetBrains Mono,
/// labels DM Sans, titles Syne.
pub const SUMMARY_CSS: &str = r#"
  /* ═══════ Summary layer (masthead, cards, exceptions, cite) ═══════ */
  .vr-wrap { max-width: 1240px; margin: 0 auto; padding: 0 32px; }
  .vr-mast { display: flex; align-items: center; gap: 20px; min-height: 56px; border-bottom: 1px solid var(--ed-border); }
  .vr-mast h1 { font-family: var(--ed-font-display); font-weight: 700; font-size: 18px; line-height: 1; letter-spacing: -0.01em; margin: 0; }
  .vr-mast h1 span { color: var(--ed-text-muted); font-weight: 600; }
  .vr-mast .vr-ver { font-family: var(--ed-font-mono); font-size: 12px; color: var(--ed-text-muted); }
  .vr-mast nav { margin-left: auto; display: flex; gap: 18px; align-items: center; font-family: var(--ed-font-body); font-size: 13px; font-weight: 400; }
  .vr-mast a, .vr-card a, .vr-exc a, .vr-cite a { color: var(--ed-accent); text-decoration: none; }
  .vr-mast a:hover, .vr-card a:hover, .vr-exc a:hover { text-decoration: underline; }
  .vr-btn { border: 1px solid var(--ed-input-border); background: transparent; color: var(--ed-text-secondary); border-radius: 6px; padding: 5px 10px; font-family: var(--ed-font-body); font-size: 12px; cursor: pointer; }
  .vr-btn:hover { border-color: var(--ed-accent); color: var(--ed-accent); }
  .vr-lede { display: flex; align-items: baseline; gap: 16px; flex-wrap: wrap; margin: 20px 0 14px; }
  .vr-lede h2 { font-family: var(--ed-font-display); font-weight: 700; font-size: 26px; line-height: 1.1; letter-spacing: -0.015em; margin: 0; }
  .vr-scope { font-family: var(--ed-font-mono); font-size: 12px; color: var(--ed-text-muted); }
  .vr-scope b { color: var(--ed-text-secondary); font-weight: 500; }
  .vr-cards { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 12px; }
  .vr-card { background: var(--ed-surface); border: 1px solid var(--ed-border-subtle); border-radius: 8px; padding: 16px; display: flex; flex-direction: column; min-width: 0; }
  .vr-card header { display: flex; justify-content: space-between; align-items: baseline; gap: 8px; margin-bottom: 10px; }
  .vr-card h3 { font-family: var(--ed-font-display); font-weight: 600; font-size: 15px; line-height: 1.2; margin: 0; }
  .vr-card .vr-q { font-family: var(--ed-font-body); font-size: 12px; color: var(--ed-text-muted); text-align: right; }
  .vr-strip { font-family: var(--ed-font-body); font-size: 12px; }
  .vr-row { display: grid; grid-template-columns: 100px minmax(0, 1fr) 150px; grid-template-areas: "t k v"; align-items: center; column-gap: 10px; margin-bottom: 5px; }
  .vr-row .vr-t { grid-area: t; }
  .vr-row .vr-v { grid-area: v; }
  .vr-row .vr-track, .vr-row .vr-bar, .vr-row .vr-barwrap { grid-area: k; }
  .vr-strip .vr-t { color: var(--ed-text-secondary); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .vr-strip .vr-t.me { color: var(--ed-text-primary); font-weight: 600; }
  .vr-strip .vr-v { font-family: var(--ed-font-mono); font-variant-numeric: tabular-nums; font-size: 12px; text-align: right; white-space: nowrap; color: var(--ed-text-secondary); }
  .vr-strip .vr-v b { color: var(--ed-text-primary); font-weight: 600; }
  .vr-strip .vr-v.na { color: var(--ed-text-muted); font-family: var(--ed-font-body); }
  .vr-track { position: relative; height: 14px; }
  .vr-track::before { content: ""; position: absolute; left: 0; right: 0; top: 50%; border-top: 1px solid var(--ed-chart-grid); }
  .vr-track .rng { position: absolute; top: 6px; height: 2px; background: var(--ed-text-muted); border-radius: 1px; }
  .vr-track .dot { position: absolute; top: 3px; width: 8px; height: 8px; border-radius: 50%; margin-left: -4px; background: var(--ed-text-muted); box-shadow: 0 0 0 2px var(--ed-surface); }
  .vr-track .tick { position: absolute; top: 2px; height: 10px; width: 2px; margin-left: -1px; background: var(--ed-text-muted); }
  .vr-track .flr { position: absolute; top: 0; bottom: 0; border-left: 1px dashed var(--ed-text-secondary); }
  .vr-track.me .rng, .vr-track.me .dot, .vr-track.me .tick { background: var(--ed-accent); }
  .vr-axis { display: block; margin: 0 160px 0 110px; position: relative; height: 14px; font-family: var(--ed-font-mono); font-size: 11px; color: var(--ed-text-muted); }
  .vr-axis span { position: absolute; top: 1px; white-space: nowrap; }
  .vr-axis span.lo { left: 0; }
  .vr-axis span.hi { right: 0; }
  .vr-axis.fl2 { height: 28px; }
  .vr-scale { display: none; font-family: var(--ed-font-mono); font-size: 11px; color: var(--ed-text-muted); }
  .vr-axis span.fl { top: 15px; transform: translateX(-50%); }
  .vr-card footer { margin-top: auto; padding-top: 10px; display: flex; justify-content: space-between; align-items: baseline; gap: 8px; font-family: var(--ed-font-body); font-size: 12px; color: var(--ed-text-muted); }
  .vr-key { display: inline-flex; gap: 10px; align-items: center; flex-wrap: wrap; font-size: 12px; color: var(--ed-text-muted); }
  .vr-key i { display: inline-block; vertical-align: middle; margin-right: 4px; }
  .vr-key .kd { width: 8px; height: 8px; border-radius: 50%; background: var(--ed-text-muted); }
  .vr-key .kt { width: 2px; height: 10px; background: var(--ed-text-muted); }
  .vr-key .kb { width: 14px; height: 6px; border-radius: 2px; background: var(--ed-text-muted); }
  .vr-key .kn { width: 14px; height: 6px; border-radius: 2px; background: var(--ed-border); }
  .vr-key .kh { width: 14px; height: 6px; border-radius: 2px; background: var(--ed-hatch); border: 1px solid var(--ed-border); }
  .vr-key .ke { width: 2px; height: 12px; background: var(--ed-text-primary); }
  .vr-key .kf { width: 0; height: 10px; border-left: 1px dashed var(--ed-text-secondary); }
  .vr-bar { position: relative; height: 10px; background: var(--ed-surface-raised); border-radius: 3px; overflow: hidden; display: flex; }
  .vr-bar i { display: block; height: 100%; }
  .vr-bar i.fit { background: var(--ed-text-muted); }
  .vr-bar i.nc { background: var(--ed-text-secondary); opacity: 0.55; }
  .vr-bar i.na { background: var(--ed-hatch); }
  .vr-bar.me i.fit { background: var(--ed-accent); }
  .vr-exp { position: absolute; top: -3px; bottom: -3px; width: 2px; background: var(--ed-text-primary); }
  .vr-barwrap { position: relative; }
  .vr-fid { width: 100%; border-collapse: separate; border-spacing: 2px; font-size: 12px; }
  .vr-fid th { font-family: var(--ed-font-body); font-weight: 500; font-size: 11px; color: var(--ed-text-muted); text-align: center; padding: 0 0 2px; text-transform: none; letter-spacing: 0; }
  .vr-fid th:first-child, .vr-fid td:first-child { text-align: left; width: 60px; }
  .vr-fid td.n { font-family: var(--ed-font-body); font-size: 12px; color: var(--ed-text-secondary); padding: 3px 0; }
  .vr-fid td.c { font-family: var(--ed-font-mono); font-size: 12px; text-align: center; padding: 3px 4px; border-radius: 3px; background: var(--ed-surface-raised); color: var(--ed-text-secondary); }
  .vr-fid td.c.nr { background: var(--ed-hatch); }
  .vr-exc { display: flex; flex-wrap: wrap; gap: 6px; align-items: center; margin: 12px 0 0; font-family: var(--ed-font-body); font-size: 12px; }
  .vr-exc .lab { font-size: 11px; font-weight: 500; letter-spacing: 0.08em; text-transform: uppercase; color: var(--ed-text-muted); margin-right: 4px; }
  .vr-chip { border: 1px solid var(--ed-border); border-radius: 999px; padding: 2px 9px; line-height: 1.35; color: var(--ed-text-secondary); background: transparent; }
  .vr-chip:hover { border-color: var(--ed-accent); }
  .vr-chip a { color: inherit; }
  .vr-fold { padding: 0; }
  .vr-fold summary { cursor: pointer; padding: 2px 9px; list-style: none; }
  .vr-fold summary::-webkit-details-marker { display: none; }
  .vr-fold summary::after { content: " +"; color: var(--ed-text-muted); }
  .vr-fold[open] { flex-basis: 100%; border-radius: 8px; }
  .vr-fold[open] summary::after { content: " \2212"; }
  .vr-fold .vr-detail { display: flex; flex-direction: column; gap: 2px; padding: 2px 9px 8px; }
  .vr-fold .vr-line { color: var(--ed-text-secondary); }
  .vr-chip .nm { font-weight: 600; color: var(--ed-text-primary); }
  .vr-foot { padding: 0 60px 24px; }
  .vr-cite a, .vr-cite pre { overflow-wrap: anywhere; word-break: break-word; }
  .vr-cite { margin: 22px 0 8px; padding: 14px 16px; border: 1px solid var(--ed-border-subtle); border-radius: 8px; background: var(--ed-surface); }
  .vr-cite h3 { font-family: var(--ed-font-display); font-weight: 600; font-size: 15px; margin: 0 0 8px; }
  .vr-cite pre { margin: 0 0 10px; white-space: pre-wrap; word-break: break-word; font-family: var(--ed-font-mono); font-size: 12px; line-height: 1.5; color: var(--ed-text-secondary); }
  .vr-layer { padding-bottom: 8px; }
  @media (max-width: 1100px) { .vr-cards { grid-template-columns: repeat(2, minmax(0, 1fr)); } }
  @media (max-width: 700px) {
    .vr-wrap { padding: 0 16px; }
    .vr-foot { padding: 0 16px 24px; }
    .vr-mast { gap: 10px; min-height: 48px; flex-wrap: wrap; padding-top: 8px; padding-bottom: 8px; }
    .vr-mast h1 { font-size: 15px; }
    .vr-mast .vr-ver { display: block; width: 100%; }
    .vr-mast nav { gap: 12px; flex-wrap: wrap; margin-left: 0; width: 100%; }
    .vr-lede h2 { font-size: 21px; }
    .vr-cards { grid-template-columns: minmax(0, 1fr); }
    /* One tool per two lines: the name and its numbers, then its strip across the card. */
    .vr-row { grid-template-columns: minmax(0, 1fr) auto; grid-template-areas: "t v" "k k"; row-gap: 2px; margin-bottom: 7px; }
    .vr-axis { display: none; }
    .vr-scale { display: block; }
  }
"#;

/// A value in the plain text of a tool name: escaped for an HTML text node.
fn esc(s: &str) -> String {
    attr_escape(s)
}

/// The position of `v` on a card's log axis, as a percentage.
fn log_pos(axis: &LogAxis, v: f64) -> f64 {
    let span = f64::from(axis.hi_decade - axis.lo_decade).max(1.0);
    let x = if v > 0.0 {
        (v.log10() - f64::from(axis.lo_decade)) / span
    } else {
        0.0
    };
    100.0 * x.clamp(0.0, 1.0)
}

/// One range card's HTML.
fn range_card_html(title: &str, card: &RangeCard, link_text: &str) -> String {
    let mut h = String::new();
    h.push_str(&format!(
        "<article class=\"vr-card\" data-card=\"{}\"><header><h3>{}</h3><span class=\"vr-q\">{}</span></header><div class=\"vr-strip\">",
        esc(&card.anchor),
        esc(title),
        esc(&card.subtitle)
    ));
    for t in &card.tools {
        let me = if t.slug == "empyrean" { " me" } else { "" };
        let star = if t.note.as_deref() == Some("integrate only") {
            "*"
        } else {
            ""
        };
        let title_attr = t
            .note
            .as_deref()
            .map(|n| format!(" title=\"{}\"", esc(n)))
            .unwrap_or_default();
        h.push_str(&format!(
            "<div class=\"vr-row\"><span class=\"vr-t{me}\"{title_attr}>{}{star}</span>",
            esc(&t.label)
        ));
        match (&card.axis, t.median, t.upper) {
            (Some(axis), Some(m), Some(u)) => {
                let (pm, pu) = (log_pos(axis, m), log_pos(axis, u));
                let (lo, hi) = (pm.min(pu), pm.max(pu));
                let floor = card
                    .floor
                    .as_ref()
                    .map(|f| {
                        format!(
                            "<span class=\"flr\" style=\"left:{:.2}%\"></span>",
                            log_pos(axis, f.value)
                        )
                    })
                    .unwrap_or_default();
                h.push_str(&format!(
                    "<span class=\"vr-track{me}\">{floor}<span class=\"rng\" style=\"left:{lo:.2}%;width:{:.2}%\"></span><span class=\"dot\" style=\"left:{pm:.2}%\"></span><span class=\"tick\" style=\"left:{pu:.2}%\"></span></span>",
                    hi - lo
                ));
                h.push_str(&format!(
                    "<span class=\"vr-v\"><b>{}</b> · {}</span>",
                    esc(t.median_text.as_deref().unwrap_or("")),
                    esc(t.upper_text.as_deref().unwrap_or(""))
                ));
            }
            _ => {
                h.push_str("<span class=\"vr-track\"></span>");
                h.push_str(&format!(
                    "<span class=\"vr-v na\">{}</span>",
                    esc(t.note.as_deref().unwrap_or("not run"))
                ));
            }
        }
        h.push_str("</div>");
    }
    if let Some(axis) = &card.axis {
        let floor = card
            .floor
            .as_ref()
            .map(|f| {
                format!(
                    "<span class=\"fl\" style=\"left:{:.2}%\" title=\"{}\">{}</span>",
                    log_pos(axis, f.value),
                    esc(&f.text),
                    esc(&f.label)
                )
            })
            .unwrap_or_default();
        h.push_str(&format!(
            "<span class=\"vr-axis{}\"><span class=\"lo\">{}</span>{floor}<span class=\"hi\">{}</span></span><span class=\"vr-scale\">log scale, {} to {}</span>",
            if card.floor.is_some() { " fl2" } else { "" },
            esc(&axis.lo_text),
            esc(&axis.hi_text),
            esc(&axis.lo_text),
            esc(&axis.hi_text)
        ));
    }
    h.push_str("</div>");
    if let Some(note) = &card.footnote {
        h.push_str(&format!(
            "<div class=\"vr-q\" style=\"text-align:left;margin-top:6px\">* {}</div>",
            esc(note)
        ));
    }
    let upper = esc(&card.upper_label);
    let floor_key = card
        .floor
        .as_ref()
        .map(|f| format!("<span><i class=\"kf\"></i>{}</span>", esc(&f.text)))
        .unwrap_or_default();
    h.push_str(&format!(
        "<footer><span class=\"vr-key\"><span><i class=\"kd\"></i>median</span><span><i class=\"kt\"></i>{upper}</span>{floor_key}</span><a href=\"#{}\">{}</a></footer></article>",
        esc(&card.anchor),
        esc(link_text)
    ));
    h
}

/// The orbit-determination card's HTML.
fn fit_card_html(card: &FitCard) -> String {
    let mut h = format!(
        "<article class=\"vr-card\" data-card=\"{}\"><header><h3>Orbit determination</h3><span class=\"vr-q\">{}</span></header><div class=\"vr-strip\">",
        esc(&card.anchor),
        esc(&card.subtitle)
    );
    for t in &card.tools {
        let me = if t.slug == "empyrean" { " me" } else { "" };
        let w = |n: usize| {
            if t.total == 0 {
                0.0
            } else {
                100.0 * n as f64 / t.total as f64
            }
        };
        h.push_str(&format!(
            "<div class=\"vr-row\"><span class=\"vr-t{me}\">{}</span><span class=\"vr-bar{me}\" role=\"img\" aria-label=\"{} fit, {} did not converge, {} not attempted\"><i class=\"fit\" style=\"width:{:.2}%\"></i><i class=\"nc\" style=\"width:{:.2}%\"></i><i class=\"na\" style=\"width:{:.2}%\"></i></span><span class=\"vr-v\"><b>{}</b> · {} · {}</span></div>",
            esc(&t.label),
            t.fit,
            t.not_converged,
            t.not_attempted,
            w(t.fit),
            w(t.not_converged),
            w(t.not_attempted),
            t.fit,
            t.not_converged,
            t.not_attempted
        ));
    }
    h.push_str(&format!(
        "</div><footer><span class=\"vr-key\"><span><i class=\"kb\"></i>fit</span><span><i class=\"kn\"></i>did not converge</span><span><i class=\"kh\"></i>not attempted</span></span><a href=\"#{}\">Details</a></footer></article>",
        esc(&card.anchor)
    ));
    h
}

/// The channel-fidelity card's HTML.
fn fidelity_card_html(card: &FidelityCard) -> String {
    let mut h = format!(
        "<article class=\"vr-card\" data-card=\"{}\"><header><h3>Channel fidelity</h3><span class=\"vr-q\">{}</span></header><table class=\"vr-fid\"><thead><tr><th></th>",
        esc(&card.anchor),
        esc(&card.subtitle)
    );
    for a in &card.axes {
        h.push_str(&format!("<th>{}</th>", esc(a)));
    }
    h.push_str("</tr></thead><tbody>");
    for ch in &card.channels {
        h.push_str(&format!("<tr><td class=\"n\">{}</td>", esc(&ch.channel)));
        for cell in &ch.cells {
            match cell {
                None if !ch.present => {
                    h.push_str("<td class=\"c nr\" title=\"channel absent from this run\"></td>")
                }
                None => h.push_str("<td class=\"c\" title=\"no compared rows\">n/a</td>"),
                Some(c) => {
                    let (class, text) = match c.state.as_str() {
                        "EXACT" => ("st-exact", "exact".to_string()),
                        "ULP" => ("st-ulp", "≤ ULP".to_string()),
                        "TOL" => ("st-tol", c.max_text.clone()),
                        _ => ("st-diff", c.max_text.clone()),
                    };
                    h.push_str(&format!(
                        "<td class=\"c {class}\" title=\"{} of {} rows ≤ ULP, max {}\">{}</td>",
                        c.n_within_ulp,
                        c.n_compared,
                        esc(&c.max_text),
                        esc(&text)
                    ));
                }
            }
        }
        h.push_str("</tr>");
    }
    h.push_str("</tbody></table>");
    let max = card
        .max_text
        .as_deref()
        .map(|t| format!("max difference per cell, widest {}", esc(t)))
        .unwrap_or_else(|| "max difference per cell".to_string());
    h.push_str(&format!(
        "<footer><span class=\"vr-key\">{max}</span><a href=\"#{}\">Details</a></footer></article>",
        esc(&card.anchor)
    ));
    h
}

/// The uncertainty card's HTML.
fn uncertainty_card_html(card: &UncertaintyCard) -> String {
    let mut h = format!(
        "<article class=\"vr-card\" data-card=\"{}\"><header><h3>Uncertainty</h3><span class=\"vr-q\">{}</span></header><div class=\"vr-strip\">",
        esc(&card.anchor),
        esc(&card.subtitle)
    );
    for (label, f) in [("within 1σ", &card.within_1), ("within 3σ", &card.within_3)] {
        let obs = f.percent.unwrap_or(0.0);
        h.push_str(&format!(
            "<div class=\"vr-row\"><span class=\"vr-t me\">{label}</span><span class=\"vr-barwrap\"><span class=\"vr-bar me\"><i class=\"fit\" style=\"width:{obs:.2}%\"></i></span><span class=\"vr-exp\" style=\"left:{:.2}%\"></span></span><span class=\"vr-v\"><b>{}</b> · {}</span></div>",
            f.expected_percent,
            esc(&f.percent_text),
            esc(&f.expected_text)
        ));
    }
    h.push_str(&format!(
        "<div class=\"vr-row\"><span class=\"vr-t me\">median σ_eq</span><span class=\"vr-track me\"></span><span class=\"vr-v\"><b>{}</b> · {}</span></div>",
        esc(&card.median.observed_text),
        esc(&card.median.expected_text)
    ));
    h.push_str(&format!(
        "</div><footer><span class=\"vr-key\"><span><i class=\"kb\"></i>observed</span><span><i class=\"ke\"></i>expected, χ(6)/√6</span></span><a href=\"#{}\">Details</a></footer></article>",
        esc(&card.anchor)
    ));
    h
}

/// A calendar-free label for a horizon in years: `−15 y`, `+15 y`.
fn years_label(y: f64) -> String {
    let r = y.round() as i64;
    if r < 0 {
        format!("\u{2212}{} y", -r)
    } else {
        format!("+{r} y")
    }
}

/// The masthead: one row with the product name, the version, the run id and the
/// links.
pub fn masthead_html(data: &SummaryData, dataset_href: &str) -> String {
    let run = match &data.run.run_id {
        Some(id) => format!("run {}", esc(id)),
        None => "unpublished render".to_string(),
    };
    format!(
        "<div class=\"masthead\"><div class=\"vr-wrap vr-mast\">\
<h1>empyrean <span>validation report</span></h1>\
<span class=\"vr-ver\">{} · {run}</span>\
<nav><a href=\"#vr-cite\">Cite this run</a><a href=\"{}\" download>Dataset</a><a href=\"#s05c\">Configurations</a>\
<button id=\"theme-toggle\" class=\"vr-btn\" title=\"Toggle light or dark theme\" aria-label=\"Toggle light or dark theme\">Theme</button></nav>\
</div></div>",
        esc(&data.run.empyrean_version),
        esc(dataset_href)
    )
}

/// The summary layer's body: the lede, the six cards, the exceptions strip and the
/// cite block, then the data the cards were rendered from.
pub fn layer_html(data: &SummaryData) -> String {
    let run = &data.run;
    let mut h = String::new();
    h.push_str("<section class=\"vr-layer\" id=\"vr-summary\"><div class=\"vr-wrap\">");
    h.push_str(&format!(
        "<div class=\"vr-lede\"><h2>How empyrean compares</h2><span class=\"vr-scope\"><b>{}</b> objects · <b>{}</b> populations · <b>{}</b> horizons, {} to {} · epoch MJD <b>{:.1}</b> TDB</span></div>",
        run.n_objects,
        run.n_populations,
        run.n_horizons,
        years_label(run.horizon_min_years),
        years_label(run.horizon_max_years),
        run.epoch_mjd_tdb
    ));
    h.push_str("<div class=\"vr-cards\">");
    h.push_str(&range_card_html(
        "Propagation",
        &data.cards.propagation,
        "Details",
    ));
    h.push_str(&range_card_html(
        "Ephemeris",
        &data.cards.ephemeris,
        "Details",
    ));
    h.push_str(&fit_card_html(&data.cards.orbit_determination));
    h.push_str(&range_card_html("Timing", &data.cards.timing, "Details"));
    h.push_str(&fidelity_card_html(&data.cards.channel_fidelity));
    h.push_str(&uncertainty_card_html(&data.cards.uncertainty));
    h.push_str("</div>");

    if !data.exceptions.is_empty() {
        h.push_str("<div class=\"vr-exc\" id=\"vr-exceptions\"><span class=\"lab\">Known exceptions</span>");
        for c in &data.exceptions {
            let kind = serde_json::to_value(c.kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            if c.detail.is_empty() {
                h.push_str(&format!(
                    "<span class=\"vr-chip\" data-kind=\"{kind}\"><a href=\"#{}\">{}</a></span>",
                    esc(&c.anchor),
                    esc(&c.text)
                ));
            } else {
                // Folded: a <details> chip, closed by default, keyboard reachable.
                let lines: String = c
                    .detail
                    .iter()
                    .map(|l| {
                        format!(
                            "<a class=\"vr-line\" href=\"#{}\">{}</a>",
                            esc(&c.anchor),
                            esc(l)
                        )
                    })
                    .collect();
                h.push_str(&format!(
                    "<details class=\"vr-chip vr-fold\" data-kind=\"{kind}\"><summary>{}</summary><div class=\"vr-detail\">{lines}</div></details>",
                    esc(&c.text)
                ));
            }
        }
        h.push_str("</div>");
    }

    h.push_str("</div></section>");

    // The data the cards were rendered from, once. A `</` inside a string cannot
    // end the script element.
    let json = serde_json::to_string(data).unwrap_or_else(|_| "null".to_string());
    h.push_str(&format!(
        "<script id=\"summary-data\" type=\"application/json\">{}</script>",
        json.replace("</", "<\\/")
    ));
    h
}

/// The cite block, at the foot of the page (the masthead's "Cite this run" link
/// jumps to it): the citation text, the permanent archive link when the run is
/// published and a copy button.
pub fn cite_html(data: &SummaryData) -> String {
    let mut h = String::new();
    h.push_str("<div class=\"vr-foot\">");
    h.push_str("<div class=\"vr-cite\" id=\"vr-cite\"><h3>Cite this run</h3>");
    match &data.cite.archive_url {
        Some(u) => h.push_str(&format!(
            "<pre id=\"vr-cite-text\">{}</pre><p class=\"vr-q\" style=\"text-align:left;margin:0 0 10px\">Permanent link: <a href=\"{}\">{}</a></p>",
            esc(&data.cite.text),
            esc(u),
            esc(u)
        )),
        None => h.push_str(&format!(
            "<pre id=\"vr-cite-text\">{}</pre><p class=\"vr-q\" style=\"text-align:left;margin:0 0 10px\">This is an unpublished render: it has no permanent archive link.</p>",
            esc(&data.cite.text)
        )),
    }
    h.push_str(
        "<button class=\"vr-btn\" id=\"vr-cite-copy\" type=\"button\">Copy citation</button></div></div>",
    );
    h.push_str(COPY_SCRIPT);
    h
}

/// The cite block's copy button: its own small script, so it works whatever the
/// page's main script does.
const COPY_SCRIPT: &str = r#"<script>
(function () {
  var b = document.getElementById('vr-cite-copy');
  var t = document.getElementById('vr-cite-text');
  if (!b || !t) return;
  b.addEventListener('click', function () {
    var text = t.textContent || '';
    var done = function () { b.textContent = 'Copied'; setTimeout(function () { b.textContent = 'Copy citation'; }, 1600); };
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(text).then(done, function () { b.textContent = 'Copy failed'; });
    } else {
      var ta = document.createElement('textarea'); ta.value = text; document.body.appendChild(ta); ta.select();
      try { document.execCommand('copy'); done(); } catch (e) { b.textContent = 'Copy failed'; }
      document.body.removeChild(ta);
    }
  });
})();
</script>"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{OrbitComparison, ValidationResult};

    const OBJECTS: [(&str, &str); 4] = [
        ("Apophis", "NEO"),
        ("Bennu", "NEO"),
        ("Eros", "NEO"),
        ("Vesta", "MainBelt"),
    ];
    const DTS: [f64; 4] = [-365.0, 0.0, 365.0, 3650.0];

    fn base(obj: &str, pop: &str, ch: &str, tt: &str, dt: f64) -> ValidationResult {
        let mut r = ValidationResult::empty();
        r.object = obj.to_string();
        r.population = pop.to_string();
        r.channel = ch.to_string();
        r.test_type = tt.to_string();
        r.dt_days = dt;
        r.epoch_mjd_tdb = 61200.0;
        r.t_mjd_tdb = 61200.0 + dt;
        r.force_model = "standard".to_string();
        r.timestamp = "2026-11-21T00:00:00Z".to_string();
        r
    }

    /// A deterministic, distinct value for each (object, dt) cell.
    fn cell(oi: usize, di: usize, scale: f64) -> f64 {
        scale * (1.0 + oi as f64 * 0.7 + di as f64 * 1.9)
    }

    /// The engine string the Apophis floor exception is keyed to.
    fn rc1_engine() -> &'static str {
        FLOOR_EXCEPTIONS[0].engine
    }

    /// A small run that carries every axis of the summary layer: propagation and
    /// ephemeris (empyrean, ASSIST, find_orb, GRSS, kete), orbit determination
    /// over optical and radar arcs (empyrean, find_orb, layup, GRSS), the four
    /// channels, a declined reference radar row and the Apophis floor exception.
    fn fixture() -> (Vec<ValidationResult>, Vec<OrbitComparison>) {
        let mut rows: Vec<ValidationResult> = Vec::new();
        for (oi, (obj, pop)) in OBJECTS.iter().enumerate() {
            for (di, &dt) in DTS.iter().enumerate() {
                // Propagation: the rust rows the section reads, the core rows the
                // externals fold onto, the ASSIST arm rows, a TOL python row.
                let mut rust = base(obj, pop, "rust", "propagation", dt);
                rust.propagation_uncertainty = Some(EMP_SECTION1_ARM.to_string());
                rust.emp_pos_au = Some([1.0, 0.0, 0.0]);
                rust.emp_vs_horizons_km = Some(cell(oi, di, 0.004));
                rust.emp_time_ms = Some(cell(oi, di, 1.5));
                rows.push(rust);
                let mut core = base(obj, pop, "core", "propagation", dt);
                core.propagation_uncertainty = Some(EMP_SECTION1_ARM.to_string());
                core.emp_pos_au = Some([1.0, 0.0, 0.0]);
                core.emp_vs_horizons_km = Some(cell(oi, di, 0.004));
                core.assist_vs_horizons_km = Some(cell(oi, di, 0.003));
                core.findorb_vs_horizons_km = Some(cell(oi, di, 0.9));
                core.grss_vs_horizons_km = Some(cell(oi, di, 0.02));
                core.kete_time_ms = Some(cell(oi, di, 0.4));
                core.grss_time_ms = Some(cell(oi, di, 3.0));
                rows.push(core);
                let mut py = base(obj, pop, "python", "propagation", dt);
                py.propagation_uncertainty = Some(EMP_SECTION1_ARM.to_string());
                py.emp_pos_au = Some([1.0 + 1e-9, 0.0, 0.0]);
                rows.push(py);
                let mut assist = base(obj, pop, "assist", "propagation", dt);
                assist.propagation_uncertainty = Some(ASSIST_TIMING_ARM.to_string());
                assist.assist_time_ms = Some(cell(oi, di, 0.2));
                rows.push(assist);

                // Ephemeris: one observer.
                let mut er = base(obj, pop, "rust", "ephemeris", dt);
                er.propagation_uncertainty = Some(EMP_SECTION1_ARM.to_string());
                er.observer = Some("F51".to_string());
                er.separation_arcsec = Some(cell(oi, di, 2e-5));
                er.d_ra_arcsec = Some(cell(oi, di, 1e-5));
                er.d_dec_arcsec = Some(cell(oi, di, 1e-5));
                er.emp_time_ms = Some(cell(oi, di, 1.0));
                rows.push(er);
                let mut ec = base(obj, pop, "core", "ephemeris", dt);
                ec.propagation_uncertainty = Some(EMP_SECTION1_ARM.to_string());
                ec.observer = Some("F51".to_string());
                ec.d_ra_arcsec = Some(cell(oi, di, 1e-5));
                ec.d_dec_arcsec = Some(cell(oi, di, 1e-5));
                ec.grss_separation_arcsec = Some(cell(oi, di, 7e-4));
                ec.findorb_separation_arcsec = Some(cell(oi, di, 3e-2));
                rows.push(ec);
                let mut ep = base(obj, pop, "python", "ephemeris", dt);
                ep.propagation_uncertainty = Some(EMP_SECTION1_ARM.to_string());
                ep.observer = Some("F51".to_string());
                ep.d_ra_arcsec = Some(cell(oi, di, 1e-5));
                ep.d_dec_arcsec = Some(cell(oi, di, 1e-5));
                rows.push(ep);
            }
            // Orbit determination, optical: empyrean fits all but Vesta.
            let mut od = base(obj, pop, "rust", "orbit_determination", 0.0);
            od.od_converged = Some(*obj != "Vesta");
            od.n_obs_used = Some(100);
            od.od_reduced_chi2 = Some(0.8);
            od.emp_pos_au = Some([1.0, 0.0, 0.0]);
            rows.push(od);
            let mut oc = base(obj, pop, "core", "orbit_determination", 0.0);
            oc.od_converged = Some(true);
            oc.n_obs_used = Some(100);
            oc.od_reduced_chi2 = Some(0.8);
            oc.emp_pos_au = Some([1.0, 0.0, 0.0]);
            // find_orb: a fit, a fit under its coverage floor, a fit, no row.
            match *obj {
                "Apophis" | "Eros" => {
                    oc.findorb_rms_residual = Some(0.4);
                    oc.findorb_n_obs_used = Some(100);
                    oc.findorb_n_obs_rejected = Some(2);
                }
                "Bennu" => {
                    oc.findorb_rms_residual = Some(0.4);
                    oc.findorb_n_obs_used = Some(10);
                    oc.findorb_n_obs_rejected = Some(90);
                }
                _ => {}
            }
            // layup: a fit, two failures under two flags, no record.
            match *obj {
                "Apophis" => {
                    oc.layup_converged = Some(true);
                    oc.layup_reduced_chi2 = Some(0.9);
                }
                "Bennu" => {
                    oc.layup_converged = Some(false);
                    oc.layup_flag = Some(3);
                    oc.layup_flag_name = Some("FLAG_NO_ROOT_CONVERGED".to_string());
                }
                "Eros" => {
                    oc.layup_converged = Some(false);
                    oc.layup_flag = Some(4);
                    oc.layup_flag_name = Some("FLAG_BUILDUP_FAILED".to_string());
                }
                _ => {}
            }
            oc.ref_od_data_arc_days = Some(9000);
            if *obj == "Apophis" {
                oc.ref_od_n_del_obs_used = Some(10);
            }
            rows.push(oc);
        }
        // Radar arcs: Apophis (the excused fit) and Bennu. The reference declines
        // both rows; GRSS fits Apophis and does not converge on Bennu.
        for obj in ["Apophis", "Bennu"] {
            let mut rr = base(obj, "NEO", "rust", "orbit_determination_radar", 0.0);
            rr.od_converged = Some(obj != "Apophis");
            rr.n_obs_used = Some(200);
            rr.od_reduced_chi2 = Some(1.1);
            rr.emp_pos_au = Some([1.0, 0.0, 0.0]);
            rr.source_version = Some(rc1_engine().replace(' ', "\n"));
            rows.push(rr);
            let mut rc = base(obj, "NEO", "core", "orbit_determination_radar", 0.0);
            rc.grss_converged = Some(obj == "Apophis");
            rc.grss_rms_arcsec = Some(0.5);
            rows.push(rc);
        }
        // The distribution channels' OD rows, exact against core.
        for (obj, pop) in OBJECTS {
            for ch in ["python", "cli", "c"] {
                let mut r = base(obj, pop, ch, "orbit_determination", 0.0);
                r.od_converged = Some(true);
                r.emp_pos_au = Some([1.0, 0.0, 0.0]);
                rows.push(r);
            }
        }
        let cmps = [0.5, 1.5, 2.5, 4.0]
            .iter()
            .zip(OBJECTS)
            .map(|(&sigma, (obj, _))| OrbitComparison {
                object: obj.to_string(),
                reference: "sbdb".to_string(),
                common_epoch_mjd_tdb: 61200.0,
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
            })
            .collect();
        (rows, cmps)
    }

    fn render(
        rows: &[ValidationResult],
        cmps: &[OrbitComparison],
        archive_timestamp: Option<&str>,
    ) -> String {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(rows, cmps, &out, None, None, &[], &[], archive_timestamp).unwrap();
        std::fs::read_to_string(&out).unwrap()
    }

    const FAKE_TS: &str = "20990102_030405";

    fn facts_for(rows: &[ValidationResult]) -> RunFacts<'static> {
        let mut dts: Vec<i64> = rows
            .iter()
            .filter(|r| r.test_type == "propagation" && r.channel == "core")
            .map(|r| r.dt_days as i64)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        dts.sort();
        let dts: &'static [i64] = Box::leak(dts.into_boxed_slice());
        RunFacts {
            n_objects: OBJECTS.len(),
            n_populations: 2,
            dt_values: dts,
            epoch_mjd_tdb: 61200.0,
            run_date: "2026-11-21",
        }
    }

    /// The text between `open` and the next `close`, after the first `open`.
    fn between<'a>(html: &'a str, open: &str, close: &str) -> &'a str {
        let a = html
            .find(open)
            .unwrap_or_else(|| panic!("`{open}` not in the page"))
            + open.len();
        let b = a + html[a..]
            .find(close)
            .unwrap_or_else(|| panic!("`{close}` not found"));
        &html[a..b]
    }

    fn tool_row<'a>(card: &'a RangeCard, slug: &str) -> &'a RangeRow {
        card.tools
            .iter()
            .find(|t| t.slug == slug)
            .unwrap_or_else(|| panic!("{slug} is not on the card"))
    }

    // ───────── every card number equals the panel statistic it summarises ─────

    /// The propagation and ephemeris cards' median, p90 and count equal the
    /// "p90 X · median Y · N cells" line of the panel for the same pair, for every
    /// tool on the card.
    #[test]
    fn propagation_and_ephemeris_cards_equal_the_panel_stat_lines() {
        let (rows, cmps) = fixture();
        let html = render(&rows, &cmps, None);
        let d = summary_of_plain(&html);
        for (card, panel) in [
            (&d.cards.propagation, "h1-prop"),
            (&d.cards.ephemeris, "h2-eph"),
        ] {
            assert!(card.tools.len() >= 3, "{panel}: too few tools on the card");
            for t in &card.tools {
                let id = if t.slug == "empyrean" {
                    format!("id=\"{panel}-summary\"")
                } else {
                    format!("id=\"{panel}-{}-jpl-summary\"", t.slug)
                };
                let line = between(&html, &id, "</div>");
                let line = &line[line.find('>').unwrap() + 1..];
                let want = format!(
                    "p90 {} · median {} · {} cells",
                    t.upper_text.as_deref().unwrap(),
                    t.median_text.as_deref().unwrap(),
                    t.n
                );
                assert!(
                    line.starts_with(&want),
                    "{panel} {}: the card says `{want}`, the panel says `{line}`",
                    t.slug
                );
            }
        }
    }

    /// Parse the embedded JSON only (no recomputation).
    fn summary_of_plain(html: &str) -> SummaryData {
        let (rows, cmps) = fixture();
        let facts = facts_for(&rows);
        let ts = html.contains(FAKE_TS).then_some(FAKE_TS);
        let fresh = build_summary(&rows, &cmps, ts, &facts);
        let open = "<script id=\"summary-data\" type=\"application/json\">";
        let raw = between(html, open, "</script>").replace("<\\/", "</");
        assert_eq!(
            serde_json::to_string(&fresh).unwrap(),
            raw,
            "the embedded summary is the summary computed from the rows"
        );
        fresh
    }

    /// The orbit-determination card's fit, did-not-converge and not-attempted
    /// counts equal each panel's headline.
    #[test]
    fn od_card_counts_equal_the_panel_headlines() {
        let (rows, cmps) = fixture();
        let html = render(&rows, &cmps, None);
        let d = summary_of_plain(&html);
        let group = between(&html, "data-section=\"h3-od\"", "pair-notcompared generic");
        assert!(d.cards.orbit_determination.tools.len() >= 4);
        for t in &d.cards.orbit_determination.tools {
            let marker = format!("<div class=\"pair-panel\" data-tool=\"{}\"", t.slug);
            let panel = &group[group.find(&marker).unwrap_or_else(|| panic!("{marker}"))..];
            let head = between(panel, "<div class=\"grid-summary\"", "</div>");
            let head = &head[head.find('>').unwrap() + 1..];
            let mut want = format!("{} of {} arcs fit", t.fit, t.total);
            if t.not_converged > 0 {
                want.push_str(&format!(" · {} did not converge", t.not_converged));
            }
            if t.not_attempted > 0 {
                want.push_str(&format!(" · {} not attempted", t.not_attempted));
            }
            assert!(
                head.starts_with(&want),
                "{}: the card says `{want}`, the panel says `{head}`",
                t.slug
            );
            assert_eq!(t.fit + t.not_converged + t.not_attempted, t.total);
        }
        // Every tool the OD panels carry is on the card, in panel order.
        let panel_slugs: Vec<&str> = group
            .match_indices("<div class=\"pair-panel\" data-tool=\"")
            .map(|(i, m)| {
                let s = &group[i + m.len()..];
                &s[..s.find('"').unwrap()]
            })
            .collect();
        let card_slugs: Vec<&str> = d
            .cards
            .orbit_determination
            .tools
            .iter()
            .map(|t| t.slug.as_str())
            .collect();
        assert_eq!(card_slugs, panel_slugs);
    }

    /// The timing card's median, p95 and count equal the Part 2 timing matrix row
    /// of the same tool, for every tool that matrix lists on propagation.
    #[test]
    fn timing_card_equals_the_part2_matrix_rows() {
        let (rows, cmps) = fixture();
        let html = render(&rows, &cmps, None);
        let d = summary_of_plain(&html);
        let matrix = between(&html, "id=\"t1-speed-matrix\"", "</table>");
        let matrix_row = |label: &str| -> Vec<String> {
            let open = format!("<td class=\"gtool\">{label}</td>");
            let i = matrix
                .find(&open)
                .unwrap_or_else(|| panic!("no matrix row `{label}`"));
            let tr = &matrix[i..i + matrix[i..].find("</tr>").unwrap()];
            let mut cells: Vec<String> = tr
                .split("title=\"")
                .skip(1)
                .map(|c| c[..c.find('"').unwrap()].to_string())
                .collect();
            let n = between(tr, "<td class=\"cn-n\">", "</td>");
            cells.push(n.split_whitespace().next().unwrap().to_string());
            cells
        };
        for (slug, label) in [
            ("empyrean", "empyrean · first order"),
            ("assist", "ASSIST default (first order)"),
            ("kete", "kete"),
            ("grss", "GRSS"),
        ] {
            let t = tool_row(&d.cards.timing, slug);
            let m = matrix_row(label);
            // p5, p50, p95 titles, then n.
            assert_eq!(
                t.median_text.as_deref(),
                Some(m[1].as_str()),
                "{slug} median"
            );
            assert_eq!(t.upper_text.as_deref(), Some(m[2].as_str()), "{slug} p95");
            assert_eq!(t.n.to_string(), m[3], "{slug} n");
        }
        // A tool with no propagation timing is on the card and says so.
        let find = tool_row(&d.cards.timing, "findorb");
        assert!(find.median.is_none());
        assert_eq!(find.note.as_deref(), Some("not timed on propagation"));
    }

    /// The channel-fidelity card's 4 x 3 cells equal the matrix's cells.
    #[test]
    fn fidelity_card_equals_the_channel_matrix() {
        let (rows, cmps) = fixture();
        let html = render(&rows, &cmps, None);
        let d = summary_of_plain(&html);
        let table = between(
            &html,
            "<table class=\"agrid fidgrid\" data-grid=\"fidelity\"><thead><tr><th class=\"gobj\">channel</th>",
            "</table>",
        );
        let card = &d.cards.channel_fidelity;
        assert_eq!(card.channels.len(), 4);
        assert_eq!(card.axes.len(), 3);
        for ch in &card.channels {
            let open = format!("<td class=\"gtool\">{}</td>", ch.channel);
            let i = table.find(&open).unwrap();
            let tr = &table[i..i + table[i..].find("</tr>").unwrap()];
            let cells: Vec<&str> = tr.split("<td class=\"fc").skip(1).collect();
            assert_eq!(cells.len(), 3, "{}: three axis cells", ch.channel);
            for (cell, html_cell) in ch.cells.iter().zip(cells) {
                match cell {
                    None => assert!(html_cell.contains("hatch")),
                    Some(c) => {
                        let want = format!(
                            "title=\"{} of {} rows ≤ ULP · max {}\"",
                            c.n_within_ulp, c.n_compared, c.max_text
                        );
                        assert!(
                            html_cell.contains(&want),
                            "{}: `{want}` not in `{html_cell}`",
                            ch.channel
                        );
                        assert!(
                            html_cell.contains(&format!(">{}", c.state)),
                            "{}: state {}",
                            ch.channel,
                            c.state
                        );
                    }
                }
            }
        }
        // The fixture makes the rust-vs-python difference real: python is TOL.
        let py = &card
            .channels
            .iter()
            .find(|c| c.channel == "python")
            .unwrap()
            .cells[0];
        assert_eq!(py.as_ref().unwrap().state, "TOL");
    }

    /// The uncertainty card's observed and expected values equal the 4.1 table's.
    #[test]
    fn uncertainty_card_equals_the_covariance_table() {
        let (rows, cmps) = fixture();
        let html = render(&rows, &cmps, None);
        let d = summary_of_plain(&html);
        let u = &d.cards.uncertainty;
        let table = between(&html, "<table class=\"agrid covtab\">", "</table>");
        let cells = |label: &str| -> (String, String) {
            let open = format!("<td class=\"gtool\">{label}</td><td class=\"r1c\">");
            let i = table.find(&open).unwrap() + open.len();
            let exp = table[i..i + table[i..].find('<').unwrap()].to_string();
            let j = table[i..].find("<td class=\"r1c\">").unwrap() + i + "<td class=\"r1c\">".len();
            let obs = table[j..j + table[j..].find('<').unwrap()].to_string();
            (exp, obs)
        };
        let (exp, obs) = cells("median σ_eq");
        assert_eq!(
            (
                u.median.expected_text.as_str(),
                u.median.observed_text.as_str()
            ),
            (exp.as_str(), obs.as_str())
        );
        let (exp, obs) = cells("fraction ≤ 1σ");
        assert_eq!(
            obs,
            format!(
                "{} ({}/{})",
                u.within_1.percent_text, u.within_1.count, u.within_1.of
            )
        );
        assert_eq!(exp, u.within_1.expected_text);
        let (exp, obs) = cells("fraction ≤ 3σ");
        assert_eq!(
            obs,
            format!(
                "{} ({}/{})",
                u.within_3.percent_text, u.within_3.count, u.within_3.of
            )
        );
        assert_eq!(exp, u.within_3.expected_text);
        // The fixture: Apophis is radar-informed and out; 0.5 is excluded with it.
        assert_eq!(u.n_radar_excluded, 1);
        assert_eq!(u.n_objects, 3);
    }

    // ───────── embedded data ─────────

    /// The summary JSON is the one embedded copy, equal to the data computed from
    /// the rows, and under 150 kB.
    #[test]
    fn the_embedded_summary_json_is_small_and_the_cards_render_from_it() {
        let (rows, cmps) = fixture();
        let html = render(&rows, &cmps, Some(FAKE_TS));
        let open = "<script id=\"summary-data\" type=\"application/json\">";
        assert_eq!(html.matches(open).count(), 1, "one embedded summary");
        let raw = between(&html, open, "</script>");
        assert!(
            raw.len() < 150_000,
            "the embedded summary is {} bytes, over the 150 kB budget",
            raw.len()
        );
        let d = summary_of_plain(&html);
        // Every number a card prints is in the data.
        for t in &d.cards.propagation.tools {
            assert!(html.contains(&esc(t.median_text.as_deref().unwrap())));
        }
        assert!(raw.contains("\"propagation\"") && raw.contains("\"orbit_determination\""));
    }

    // ───────── exceptions ─────────

    fn kinds(d: &SummaryData) -> Vec<ExceptionKind> {
        d.exceptions.iter().map(|c| c.kind).collect()
    }

    fn summary_for(rows: &[ValidationResult], cmps: &[OrbitComparison]) -> SummaryData {
        build_summary(rows, cmps, None, &facts_for(rows))
    }

    /// The chips come from the rows by the code `ci-check` uses: change the rows
    /// and the chip changes.
    #[test]
    fn exceptions_follow_the_rows() {
        let (rows, cmps) = fixture();
        let d = summary_for(&rows, &cmps);
        let k = kinds(&d);
        for want in [
            ExceptionKind::FloorExcused,
            ExceptionKind::ReferenceDeclined,
            ExceptionKind::NotAttempted,
            ExceptionKind::FindorbCoverageFloor,
            ExceptionKind::LayupNotConverged,
            ExceptionKind::ExternalRadarNotConverged,
        ] {
            assert!(k.contains(&want), "no {want:?} chip in {k:?}");
        }
        let chip = |d: &SummaryData, kind: ExceptionKind| {
            d.exceptions.iter().find(|c| c.kind == kind).cloned()
        };
        // The excused fit names the object and what it is.
        let excused = chip(&d, ExceptionKind::FloorExcused).unwrap();
        assert_eq!(excused.object.as_deref(), Some("Apophis"));
        assert!(excused.text.contains("optical+radar fit excused"));
        // The declined count is `ci-check`'s: both core radar rows.
        assert_eq!(
            chip(&d, ExceptionKind::ReferenceDeclined).unwrap().count,
            Some(2)
        );
        assert_eq!(reference_declined_rows(&rows).len(), 2);
        // find_orb: Bennu is under the floor; layup: two failures, one per flag.
        assert_eq!(
            chip(&d, ExceptionKind::FindorbCoverageFloor).unwrap().count,
            Some(1)
        );
        let layup = chip(&d, ExceptionKind::LayupNotConverged).unwrap();
        assert_eq!(layup.count, Some(2));
        assert!(
            layup.detail.join(" ").contains("FLAG_NO_ROOT_CONVERGED 1"),
            "{}",
            layup.text
        );
        assert!(
            layup.detail.join(" ").contains("FLAG_BUILDUP_FAILED 1"),
            "{}",
            layup.text
        );
        assert_eq!(layup.text, "layup: 2 arcs did not converge");
        // The not-attempted chip is ONE folded chip; each tool is a line inside it.
        let folded = d
            .exceptions
            .iter()
            .filter(|c| c.kind == ExceptionKind::NotAttempted)
            .collect::<Vec<_>>();
        assert_eq!(folded.len(), 1, "one folded not-attempted chip");
        let lines = folded[0].detail.join("\n");
        for want in ["find_orb, layup: ", "GRSS: "] {
            assert!(
                lines.contains(want),
                "`{want}` not in the folded detail: {lines}"
            );
        }
        // GRSS did not converge on Bennu's radar arc.
        let grss = chip(&d, ExceptionKind::ExternalRadarNotConverged).unwrap();
        assert_eq!(grss.object.as_deref(), Some("Bennu"));

        // The Apophis fit converges: the exception goes unused, the chip says so.
        let mut r2 = rows.clone();
        for r in r2.iter_mut().filter(|r| {
            r.channel == "rust"
                && r.test_type == "orbit_determination_radar"
                && r.object == "Apophis"
        }) {
            r.od_converged = Some(true);
        }
        let k2 = kinds(&summary_for(&r2, &cmps));
        assert!(!k2.contains(&ExceptionKind::FloorExcused));
        assert!(k2.contains(&ExceptionKind::FloorUnused));

        // The same fit on another engine: the chip names the failure.
        let mut r3 = rows.clone();
        for r in r3.iter_mut().filter(|r| {
            r.channel == "rust"
                && r.test_type == "orbit_determination_radar"
                && r.object == "Apophis"
        }) {
            r.source_version = Some("empyrean-core 0.12.0".to_string());
        }
        assert!(kinds(&summary_for(&r3, &cmps)).contains(&ExceptionKind::FloorWrongEngine));

        // No declined rows, no layup failure, no under-floor find_orb arc: the
        // chips are gone.
        let mut r4 = rows.clone();
        for r in r4.iter_mut() {
            if r.test_type == "orbit_determination_radar" && r.channel == "core" {
                r.emp_pos_au = Some([1.0, 0.0, 0.0]);
                r.od_converged = Some(true);
            }
            if r.channel == "core" && r.test_type == "orbit_determination" {
                r.layup_converged = Some(true);
                r.findorb_n_obs_used = r.findorb_n_obs_used.map(|_| 100);
            }
        }
        let k4 = kinds(&summary_for(&r4, &cmps));
        assert!(!k4.contains(&ExceptionKind::ReferenceDeclined), "{k4:?}");
        assert!(!k4.contains(&ExceptionKind::LayupNotConverged));
        assert!(!k4.contains(&ExceptionKind::FindorbCoverageFloor));
    }

    /// The strip stays short: at most six top-level chips, the per-tool lines
    /// folded inside one closed <details> chip, every exception one click away.
    #[test]
    fn the_exceptions_strip_has_at_most_six_chips_with_the_detail_folded() {
        let (rows, cmps) = fixture();
        let html = render(&rows, &cmps, None);
        let strip = between(&html, "id=\"vr-exceptions\"", "</section>");
        let strip = &strip[..strip
            .find("</div></div>")
            .map(|i| i + 12)
            .unwrap_or(strip.len())];
        let top = strip.matches("class=\"vr-chip").count();
        assert!(top <= 6, "{top} top-level chips in the strip");
        assert!(top >= 5, "the strip lost chips: {top}");
        let folds = strip.matches("<details class=\"vr-chip vr-fold\"").count();
        assert!(folds >= 2, "the not-attempted and layup chips are folded");
        assert!(
            !strip.contains("<details class=\"vr-chip vr-fold\" open"),
            "folds start closed"
        );
        // Every per-tool line is still on the page, inside its fold.
        assert!(strip.contains("GRSS: 4 of 4 objects not attempted in orbit determination"));
        assert!(strip.contains("FLAG_NO_ROOT_CONVERGED 1"));
        assert!(strip.contains("Not every tool ran every object ("));
        // No chip text runs past about eight words.
        for part in strip.split("<a href=\"#").skip(1) {
            let t = &part[part.find('>').unwrap() + 1..part.find("</a>").unwrap()];
            assert!(t.split_whitespace().count() <= 9, "chip too long: {t}");
        }
        for part in strip.split("<summary>").skip(1) {
            let t = &part[..part.find("</summary>").unwrap()];
            assert!(t.split_whitespace().count() <= 9, "chip too long: {t}");
        }
    }

    /// The floor-exception verdict `ci-check` gates on is the one the strip names.
    #[test]
    fn the_floor_exception_verdict_is_shared_with_the_gate() {
        let e = &FLOOR_EXCEPTIONS[0];
        let multi_line = e.engine.replace(' ', "\n   ");
        assert_eq!(
            floor_exception_verdict(e, false, Some(&multi_line)),
            FloorExceptionVerdict::Excused
        );
        assert_eq!(
            floor_exception_verdict(e, true, Some(e.engine)),
            FloorExceptionVerdict::Unused
        );
        assert_eq!(
            floor_exception_verdict(e, false, Some("other")),
            FloorExceptionVerdict::WrongEngine {
                ran_on: "other".to_string()
            }
        );
        assert_eq!(
            floor_exception_verdict(e, false, None),
            FloorExceptionVerdict::WrongEngine {
                ran_on: String::new()
            }
        );
    }

    // ───────── cite, masthead, scale ─────────

    #[test]
    fn the_cite_block_links_the_archive_only_when_published() {
        let (rows, cmps) = fixture();
        let published = render(&rows, &cmps, Some(FAKE_TS));
        let url = format!(
            "https://storage.googleapis.com/empyrean-validation/archive/{FAKE_TS}/validation_report.html"
        );
        let cite = between(
            &published,
            "<div class=\"vr-cite\" id=\"vr-cite\">",
            "</div>",
        );
        assert!(
            cite.contains(&url),
            "the cite block carries the archive link"
        );
        assert!(
            cite.contains("empyrean 0.11.0-rc.1"),
            "the empyrean version"
        );
        assert!(cite.contains("empyrean-core 0.11.1") && cite.contains("hyperjet 1.15.0"));
        assert!(cite.contains("vr-cite-copy"), "a copy button");
        assert!(
            published.contains(&format!("run {FAKE_TS}")),
            "the masthead names the run"
        );
        let d = summary_of_plain(&published);
        assert_eq!(d.cite.archive_url.as_deref(), Some(url.as_str()));

        let unpublished = render(&rows, &cmps, None);
        let cite = between(
            &unpublished,
            "<div class=\"vr-cite\" id=\"vr-cite\">",
            "</div>",
        );
        assert!(
            !cite.contains("storage.googleapis.com"),
            "an unpublished render has no link"
        );
        assert!(cite.contains("unpublished render"));
        assert!(
            unpublished.contains("unpublished render"),
            "the masthead says so too"
        );
        assert!(summary_of_plain(&unpublished).cite.archive_url.is_none());
    }

    #[test]
    fn the_masthead_is_one_row_with_the_links_the_page_keeps() {
        let (rows, cmps) = fixture();
        let html = render(&rows, &cmps, Some(FAKE_TS));
        let m = between(
            &html,
            "<div class=\"masthead\">",
            "<section class=\"vr-layer\"",
        );
        assert!(m.contains("empyrean <span>validation report</span>"));
        assert!(!m.contains("EMPYREAN") && !m.contains("DYNAMICS"));
        for link in [
            "#vr-cite\">Cite this run",
            "validation.json\" download>Dataset",
            "#s05c\">Configurations",
        ] {
            assert!(m.contains(link), "masthead lacks `{link}`");
        }
        assert!(
            m.contains("id=\"theme-toggle\""),
            "the theme toggle is kept"
        );
        assert_eq!(m.matches("<h1>").count(), 1);
    }

    /// The six cards come in order, each links its section, and every tool on an
    /// axis is on its card in registry order with empyrean first; no ranking word.
    #[test]
    fn six_cards_every_tool_registry_order_no_ranking() {
        let (rows, cmps) = fixture();
        let html = render(&rows, &cmps, None);
        let layer = between(&html, "<section class=\"vr-layer\"", "</section>");
        let titles: Vec<&str> = layer
            .match_indices("<h3>")
            .map(|(i, _)| &layer[i + 4..i + 4 + layer[i + 4..].find("</h3>").unwrap()])
            .collect();
        assert_eq!(
            titles,
            [
                "Propagation",
                "Ephemeris",
                "Orbit determination",
                "Timing",
                "Channel fidelity",
                "Uncertainty"
            ]
        );
        let d = summary_of_plain(&html);
        let registry: Vec<&str> = crate::tool_registry::TOOLS.iter().map(|t| t.slug).collect();
        for card in [&d.cards.propagation, &d.cards.ephemeris, &d.cards.timing] {
            let idx: Vec<usize> = card
                .tools
                .iter()
                .map(|t| registry.iter().position(|s| *s == t.slug).unwrap())
                .collect();
            let mut sorted = idx.clone();
            sorted.sort();
            assert_eq!(idx, sorted, "tools are in registry order");
            assert_eq!(card.tools[0].slug, "empyrean");
        }
        // Every external the registry says ran propagation is on the card.
        let ran: BTreeSet<&str> = tools_in_run(&rows).into_iter().collect();
        for t in &d.cards.timing.tools {
            assert!(ran.contains(t.slug.as_str()));
        }
        for t in ["assist", "findorb", "grss"] {
            assert!(
                d.cards.propagation.tools.iter().any(|x| x.slug == t),
                "{t} on propagation"
            );
        }
        let lower = layer.to_lowercase();
        for word in ["better", "worse", "faster", "slower", "winner", "rank"] {
            assert!(!lower.contains(word), "the layer says `{word}`");
        }
        for anchor in ["s02", "s06", "s08b", "s-prop-c", "s10", "s12"] {
            assert!(
                layer.contains(&format!("href=\"#{anchor}\"")),
                "no link to #{anchor}"
            );
            assert!(
                html.contains(&format!("id=\"{anchor}\"")),
                "#{anchor} is not on the page"
            );
        }
        // Copy rules: no em dash, "au" not "AU", the product in lowercase.
        assert!(!layer.contains('\u{2014}'), "an em dash in the layer");
        assert!(!layer.contains(" AU"), "AU in the layer");
    }

    /// The type scale: no text under 11 px in the new layer, three families, a
    /// one-column stack at phone width.
    #[test]
    fn the_layer_type_scale_and_phone_stack() {
        for line in SUMMARY_CSS.lines() {
            let mut rest = line;
            while let Some(i) = rest.find("font-size:") {
                let v = rest[i + 10..].trim_start();
                let px: f64 = v
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || *c == '.')
                    .collect::<String>()
                    .parse()
                    .unwrap_or_else(|_| panic!("a font-size without px in `{line}`"));
                assert!(px >= 11.0, "text under 11 px in the layer: `{line}`");
                rest = &rest[i + 10..];
            }
        }
        for face in ["--ed-font-display", "--ed-font-body", "--ed-font-mono"] {
            assert!(SUMMARY_CSS.contains(face), "{face} is not used");
        }
        let phone = between(SUMMARY_CSS, "@media (max-width: 700px)", "\n  }\n");
        assert!(phone.contains(".vr-cards { grid-template-columns: minmax(0, 1fr); }"));
        assert!(
            !SUMMARY_CSS.contains("#fff") && !SUMMARY_CSS.contains("rgb("),
            "colours come from tokens"
        );
    }

    #[test]
    fn axis_labels_carry_units_never_bare_exponents() {
        assert_eq!(axis_text(GridAxis::PropKm, -3), "1 m");
        assert_eq!(axis_text(GridAxis::PropKm, 6), "10⁶ km");
        assert_eq!(axis_text(GridAxis::EphMas, -3), "1 µas");
        assert_eq!(axis_text(GridAxis::EphMas, 3), "1\u{2033}");
        assert_eq!(axis_text(GridAxis::TimingMs, 0), "1 ms");
        assert_eq!(axis_text(GridAxis::TimingMs, 5), "100 s");
        assert_eq!(axis_text(GridAxis::TimingMs, -2), "10 µs");
    }

    // ───────── runner path prefix ─────────

    #[test]
    fn the_runner_work_directory_prefix_is_shortened_once_everywhere() {
        let real = "/home/runner/work/empyrean-validation/empyrean-validation/empyrean-validation/runners/sigmafill/veres2017_sigma_table.csv";
        assert_eq!(
            shorten_runner_path(real),
            "…/empyrean-validation/runners/sigmafill/veres2017_sigma_table.csv"
        );
        assert_eq!(
            shorten_runner_path(
                "file /home/runner/work/r/r/a.py line 3 and /home/runner/work/q/q/b.py"
            ),
            "file …/a.py line 3 and …/b.py"
        );
        assert_eq!(shorten_runner_path("/Users/runner/work/x/x/y"), "…/y");
        // Nothing else changes: other paths, one-segment prefixes and the tool's
        // own punctuation (an em dash) stay as written.
        for keep in [
            "/home/runner/work/a/b/c.py",
            "/home/other/work/a/a/c.py",
            "/home/runner/work/only",
            "failed — see /usr/lib/x.py",
            "",
        ] {
            assert_eq!(shorten_runner_path(keep), keep);
        }
        // The HTML escape and the dataset projection both apply it.
        assert_eq!(
            attr_escape(&format!("<{real}>")),
            "&lt;…/empyrean-validation/runners/sigmafill/veres2017_sigma_table.csv&gt;"
        );
        let mut r = ValidationResult::empty();
        r.notes = format!("could not read {real}");
        r.object = "X".to_string();
        let ds = project_dataset_json(&[r]).unwrap();
        assert!(!ds.contains("/home/runner"), "{ds}");
        assert!(ds.contains("could not read …/empyrean-validation/runners/"));
    }

    /// A failed fit whose note carries the runner path shows it shortened on the page.
    #[test]
    fn a_failed_fit_note_is_shown_without_the_runner_prefix() {
        let (mut rows, cmps) = fixture();
        for r in rows.iter_mut().filter(|r| {
            r.channel == "rust" && r.test_type == "orbit_determination" && r.object == "Vesta"
        }) {
            r.notes = "no fixture at /home/runner/work/empyrean-validation/empyrean-validation/fixtures/psv/Vesta.psv".to_string();
        }
        let html = render(&rows, &cmps, None);
        if let Some(i) = html.find("/home/runner/work") {
            panic!(
                "a runner path is on the page: {}",
                &html[i.saturating_sub(200)..i + 120]
            );
        }
        assert!(html.contains("no fixture at …/fixtures/psv/Vesta.psv"));
    }
}
