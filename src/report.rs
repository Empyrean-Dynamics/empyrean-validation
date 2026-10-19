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
use crate::predict_schema::{
    COVERAGE_ARC_SPLIT_DAYS, CalibrationCell, ObjectWalkSummary, PredictAggregates, SurfaceCell,
};
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

/// S5 item 1: a Part head reads as a mono eyebrow over a Syne title. The part
/// name is `"Part N · Title"`; split it so `"Part N"` becomes the eyebrow and
/// the clause after the middle dot becomes the title. Names without a dot (the
/// Appendix) render as a title alone. One source, so the rail and the head stay
/// in step.
fn part_head_inner(name: &str) -> String {
    match name.split_once(" · ") {
        Some((eyebrow, title)) => format!(
            "<span class=\"ph-eyebrow\">{eyebrow}</span><span class=\"ph-title\">{title}</span>"
        ),
        None => format!("<span class=\"ph-title\">{name}</span>"),
    }
}

/// Delta 9 item 1: Part 3's scope line — the channels the Internals part
/// compares against core, stated once in the eyebrow role under the Part 3
/// title, so the masthead no longer carries "Rust channel under test". It reuses
/// the shared `.ph-facts` idiom (the Part 1 head facts): the "CHANNELS" label
/// and the "·" separators are CSS, the channel names are the only words that
/// count, all lowercase. `rust` is the channel under test (Parts 1–2, against
/// JPL); `core` is the validation reference. Derived from the rows, so an absent
/// distribution channel (here `c`) does not appear. Terse, never a sentence.
fn part3_scope_html(results: &[ValidationResult]) -> String {
    const CHANNELS: [&str; 4] = ["rust", "python", "cli", "c"];
    let mut present: BTreeSet<&str> = BTreeSet::new();
    for r in results {
        if let Some(&ch) = CHANNELS.iter().find(|&&c| c == r.channel) {
            present.insert(ch);
        }
    }
    let mut vals = String::new();
    for &ch in CHANNELS.iter().filter(|c| present.contains(*c)) {
        let label = if ch == "rust" {
            "rust (under test)"
        } else {
            ch
        };
        vals.push_str(&format!("<span class=\"phf-v\">{label}</span>"));
    }
    vals.push_str("<span class=\"phf-v\">core</span>");
    format!(
        "<div class=\"ph-facts\"><span class=\"phf\" data-k=\"channels\" title=\"every distribution channel is compared against empyrean-core; rust is the channel under test in Parts 1–2 against JPL\">{vals}</span></div>"
    )
}

/// S6: the Part 1 head-line facts, read from the rows and stated once in the
/// sticky Part 1 bar under the title (the eyebrow role). The per-fact label and
/// the " · " separators are CSS, not DOM text; the terse values below are the
/// only words that count toward the H5 budget.
struct Part1Facts {
    /// Modal IC epoch over propagation/ephemeris rows (the shared t0).
    epoch_mjd: f64,
    /// That epoch as a UTC calendar date (TDB/UTC differ by < 70 s, so the day
    /// is the same).
    epoch_utc: String,
    /// Distinct ephemeris observer MPC codes, first-seen order, slash-joined.
    sites: String,
}

/// Gather the Part 1 head facts from the rows (never hand-typed). The epoch is
/// the modal propagation/ephemeris t0; the OD fits use each object's own IC
/// epoch, stated on the fact's hover.
fn part1_facts(results: &[ValidationResult]) -> Part1Facts {
    let mut counts: HashMap<i64, (f64, usize)> = HashMap::new();
    for r in results {
        if r.test_type == "propagation" || r.test_type == "ephemeris" {
            let key = (r.epoch_mjd_tdb * 1000.0).round() as i64;
            let e = counts.entry(key).or_insert((r.epoch_mjd_tdb, 0));
            e.1 += 1;
        }
    }
    let epoch_mjd = counts
        .values()
        .max_by_key(|(_, n)| *n)
        .map(|(e, _)| *e)
        .unwrap_or(0.0);
    // unix_days = MJD - 40587; the date is the same in TDB and UTC to the day.
    let (y, m, d) = ymd_from_unix_days(epoch_mjd.floor() as i64 - 40587);
    let epoch_utc = format!("{y:04}-{m:02}-{d:02}");
    let mut sites: Vec<&str> = Vec::new();
    for r in results {
        if r.test_type != "ephemeris" {
            continue;
        }
        let Some(o) = r.observer.as_deref() else {
            continue;
        };
        if !o.is_empty() && !sites.contains(&o) {
            sites.push(o);
        }
    }
    Part1Facts {
        epoch_mjd,
        epoch_utc,
        sites: sites.join("/"),
    }
}

/// S6 item 2: the Part 1 head line — the mono eyebrow + Syne title (as every
/// Part head) followed, in the eyebrow role, by the four facts the reader needs
/// once: test epoch (MJD TDB + UTC), the frame per metric, the force-model pair
/// (empyrean / reference, as A.4 records them), and the ephemeris observing
/// sites (MPC codes). Each fact's value spans are < 4 words so none reads as a
/// free-standing sentence; the fact label and separators are CSS.
fn part1_head_html() -> String {
    // S8 item 3: the head line is the eyebrow + title ONLY. The facts render as a
    // full-width second row of the sticky bar (part1_facts_html), so the title
    // line and the two Compare selects share one line that never wraps.
    "<span class=\"ph-eyebrow\">Part 1</span><span class=\"ph-title\">Reference agreement</span>"
        .to_string()
}

/// S8 item 3: the Part 1 head facts as the sticky bar's full-width second row
/// (own line under the title/Compare line). Same four facts, same eyebrow role,
/// same words as before — only their position moved.
fn part1_facts_html(f: &Part1Facts) -> String {
    let mjd = if f.epoch_mjd.fract() == 0.0 {
        format!("{:.1}", f.epoch_mjd)
    } else {
        format!("{:.3}", f.epoch_mjd)
    };
    format!(
        "<div class=\"ph-facts\">\
<span class=\"phf\" data-k=\"epoch\" title=\"propagation / ephemeris t0; OD fits use each object's own IC epoch\"><span class=\"phf-v\">MJD {mjd} TDB</span><span class=\"phf-v\">{utc} UTC</span></span>\
<span class=\"phf\" data-k=\"frames\" title=\"propagation: ICRF/J2000 barycentric Cartesian · ephemeris: topocentric astrometric RA/Dec · orbit determination: Sun-centred ecliptic J2000\"><span class=\"phf-v\">prop ICRF/J2000</span><span class=\"phf-v\">eph RA/Dec</span><span class=\"phf-v\">OD ecliptic J2000</span></span>\
<span class=\"phf\" data-k=\"force\" title=\"empyrean: DE440 + 16 SB441-N16 asteroids, 1PN GR, Marsden non-grav; reference: JPL Horizons/SBDB on DE441\"><span class=\"phf-v\">empyrean DE440+16ast</span><span class=\"phf-v\">JPL DE441</span></span>\
<span class=\"phf\" data-k=\"sites\" title=\"ephemeris observing sites (MPC codes), from the rows\"><span class=\"phf-v\">{sites}</span></span>\
</div>",
        mjd = mjd,
        utc = f.epoch_utc,
        sites = f.sites,
    )
}

/// S6 item 1/3: one slot eyebrow (the single uppercase eyebrow role). It is also
/// the scroll-spy anchor its rail sub-entry targets. The slot LETTER and the
/// " · " separator are CSS (`::before content: attr(data-slot)`), so only the
/// name counts toward the word budget, and the rail (nav) text is uncounted.
fn slot_eyebrow(anchor: &str, slot: &str, name: &str) -> String {
    format!(
        "<div class=\"slot-eyebrow spy-anchor\" id=\"{anchor}\" data-slot=\"{slot}\">{name}</div>"
    )
}

/// S6 item 1: render one Part-1 section with the fixed slots — head
/// (number · title) → A · by object → B · by population → C · cost → fold. The
/// slot anchors are `{sec_id}-a/-b/-c`; the rail sub-entries scroll to them.
/// `sec_id` reuses the former grid-section anchor (s02/s06/s08b) so deep links
/// (§1.3A, scroll-margin) keep resolving. One renderer, so the three sections
/// cannot drift in shape.
/// The one fixed-slot section renderer. `readout` is the summary slot between
/// the head and the A slot (brief: head → readout → A · B · C → fold); it is the
/// empty string where a section states its readout inside the A grid (Part 1,
/// S6) and a `grid-summary` + summary table for Part 2 (S7). One renderer, so
/// Part 1 and Part 2 cannot drift in shape.
#[allow(clippy::too_many_arguments)]
fn part1_section(
    sec_id: &str,
    num: &str,
    title: &str,
    title_attr: &str,
    readout: &str,
    a_grid: &str,
    b_slot: &str,
    c_cost: &str,
    fold: &str,
) -> String {
    format!(
        "<div class=\"section\" id=\"{sec_id}\">\n  <div class=\"section-num\">{num}</div>\n  <div class=\"section-title\" id=\"{sec_id}-title\" title=\"{title_attr}\">{title}</div>\n  {readout}{a_eye}\n  {a_grid}\n  {b_eye}\n  {b_slot}\n  {c_eye}\n  {c_cost}\n  {fold}\n</div>\n",
        a_eye = slot_eyebrow(&format!("{sec_id}-a"), "A", "by object"),
        b_eye = slot_eyebrow(&format!("{sec_id}-b"), "B", "by population"),
        c_eye = slot_eyebrow(&format!("{sec_id}-c"), "C", "cost"),
    )
}

/// One rail block for a fixed-slot section (Part 1 and Part 2): the section
/// entry plus the three sub-entries (A · B · C) that scroll to the slot anchors
/// `{sec}-a/-b/-c`. Shared by `part1_rail_html` and `build_rail_html` so the
/// rail and the DOM cannot drift.
fn rail_section_abc(sec: &str, num: &str, title: &str) -> String {
    let mut h = format!("  <a data-target=\"{sec}\" href=\"#{sec}\">{num} {title}</a>\n");
    for (letter, name) in [
        ("a", "A · by object"),
        ("b", "B · by population"),
        ("c", "C · cost"),
    ] {
        h.push_str(&format!(
            "  <a data-target=\"{sec}-{letter}\" href=\"#{sec}-{letter}\">&nbsp;&nbsp;{name}</a>\n"
        ));
    }
    h
}

/// A flat rail entry (Parts 3–4 sections and the appendix): one link whose text
/// reads exactly the section title it scrolls to.
fn rail_entry(sec: &str, label: &str) -> String {
    format!("  <a data-target=\"{sec}\" href=\"#{sec}\">{label}</a>\n")
}

/// S6 item 3: the Part 1 rail block, generated from the same (section, slot)
/// tree the sections are built from (1.1/1.2/1.3, each A · B · C).
fn part1_rail_html() -> String {
    let mut h = String::new();
    for (sec, num, title) in [
        ("s02", "1.1", "Propagation"),
        ("s06", "1.2", "Ephemeris"),
        ("s08b", "1.3", "Orbit determination"),
    ] {
        h.push_str(&rail_section_abc(sec, num, title));
    }
    h
}

/// S7 item 3: the WHOLE sidebar, generated from the full part/section tree so
/// the rail and the DOM emit in one order and cannot drift. Part 1 and Part 2
/// are the fixed-slot sections (A · B · C sub-entries); Parts 3–4 and the
/// appendix are flat entries whose text matches their section title. Rail text
/// lives in <nav class="rail"> and is uncounted by H5.
fn build_rail_html() -> String {
    let mut h = String::new();
    h.push_str(&format!("  <div class=\"rail-part\">{PART_NAME_1}</div>\n"));
    h.push_str(&part1_rail_html());
    h.push_str(&format!("  <div class=\"rail-part\">{PART_NAME_2}</div>\n"));
    for (sec, num, title) in [
        ("s-prop", "2.1", "Propagation"),
        ("s-eph", "2.2", "Ephemeris"),
        ("s-od", "2.3", "Orbit determination"),
    ] {
        h.push_str(&rail_section_abc(sec, num, title));
    }
    h.push_str(&format!("  <div class=\"rail-part\">{PART_NAME_3}</div>\n"));
    for (sec, label) in [
        ("s10", "3.1 Channel fidelity"),
        ("s05", "3.2 Derivatives"),
        ("s09b", "3.3 Non-grav recovery"),
    ] {
        h.push_str(&rail_entry(sec, label));
    }
    h.push_str(&format!("  <div class=\"rail-part\">{PART_NAME_4}</div>\n"));
    for (sec, label) in [
        ("s12", "4.1 Consistency with JPL at epoch"),
        ("s15", "4.2 Predictive power"),
    ] {
        h.push_str(&rail_entry(sec, label));
    }
    h.push_str(&format!(
        "  <div class=\"rail-part\">{PART_NAME_APPENDIX}</div>\n"
    ));
    for (sec, label) in [
        ("s01", "Run summary"),
        ("s01b", "External tools"),
        ("s05c", "Configurations"),
        ("s13", "Provenance"),
    ] {
        h.push_str(&rail_entry(sec, label));
    }
    h
}

/// The report's single population-order switch.
///
/// The 13 catalog populations (see [`crate::catalog`]) in one fixed order,
/// nested inside the six Part-4 classes
/// ([`crate::predict_schema::classes::from_population`]): NEA, Comet, ISO,
/// MainBelt, Trojan, Outer. **Every grid that lists populations** — the
/// per-object group rows and the protected agreement/cost heatmaps
/// ([`prop_like_ordered_blocks`]), the 1.3 OD panels, the 2.1 tool-ranking
/// by-population rows, the 2.2 timing grids, the 3.1 fidelity grid, the
/// per-population legend group order, and (via [`class_rank`]) the 4.2
/// class grids — orders its population blocks by this one slice, so a reader
/// carries an object's row from one grid to the next. Within a block the
/// objects keep their worst-first order and the WORST / MEDIAN column is
/// unchanged, so no offender is hidden. Flip the order policy for the whole
/// report by reordering this one array; `class_rank` follows it.
pub const POPULATION_ORDER: [&str; 13] = [
    // class NEA
    "NEO",
    "Short-arc NEO",
    "TCO",
    "Impactor",
    // class Comet
    "Comet",
    // class ISO
    "ISO",
    // class MainBelt
    "MBA",
    "Self-Perturber",
    // class Trojan
    "Jupiter Trojan",
    "Earth Trojan",
    "Neptune Trojan",
    // class Outer
    "Centaur",
    "TNO",
];

/// Rank of a population in [`POPULATION_ORDER`] — the sort key every grid
/// uses for its population blocks. A population not in the slice sorts last
/// (stable, after all known populations) so an unmapped population stays
/// visible at the end rather than silently reordered into the middle.
fn population_rank(pop: &str) -> usize {
    POPULATION_ORDER
        .iter()
        .position(|&p| p == pop)
        .unwrap_or(POPULATION_ORDER.len())
}

/// Rank of a Part-4 class in the order [`POPULATION_ORDER`] induces on the
/// six classes — the index of the first population whose class is `class`
/// (NEA → 0, Comet → 4, ISO → 5, MainBelt → 6, Trojan → 8, Outer → 11).
/// `"all"` and any unmapped class sort last. The one switch therefore drives
/// both the population grids and the 4.2 class grids.
fn class_rank(class: &str) -> usize {
    POPULATION_ORDER
        .iter()
        .map(|&p| crate::predict_schema::classes::from_population(p))
        .position(|c| c == class)
        .unwrap_or(usize::MAX)
}

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

/// The Empyrean Dynamics orbital mark, inlined as a data URI so the branded
/// chart export (the one download button on every Plotly toolbar) carries the
/// logo with no network fetch. This is the transparent-background mark (the
/// orbit and bodies, no filled tile), derived from the 1024×1024 masthead icon
/// by keying out the near-black tile fill and resampling to 256×256. The tile
/// fill equalled the dark theme surface, so the filled icon was invisible on
/// dark and a dark box on light; the mark reads on both surfaces and is used,
/// unchanged, in either theme.
const LOGO_BYTES: &[u8] = include_bytes!("../docs/empyrean-dynamics-icon-mark.png");

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
    // the Jet1 STM path (`first_order_detection_on`) is matched against the
    // same-mode core baseline rather than the `none_detection_on` baseline
    // (and vice versa). Without this, the two modes silently overwrite in
    // the hash map and ~50% of the comparable rows hit a Jet1-vs-none diff
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

/// Per-(channel, method) summary breakdown — the same per-method columns the
/// cross-channel matrix shows, so the CI summary carries the method axis: the
/// detection-on rows produced, delivered and refused, and the parity against
/// the rust reference (compared rows, bit-identical rows, worst relative
/// position difference).
fn method_summary_json(results: &[ValidationResult]) -> serde_json::Value {
    use crate::schema::uncertainty_modes as um;
    let channels = ["rust", "core", "c", "python", "cli"];
    type Key = (String, i64, String, String, Option<String>, String);
    let on_arm = |r: &ValidationResult| -> Option<(&'static str, String)> {
        let tag = r.propagation_uncertainty.as_deref()?;
        if um::arm_of(tag) != Some(um::DETECTION_ON) {
            return None;
        }
        um::method_of(tag).map(|m| (m, tag.to_string()))
    };
    let key_of = |r: &ValidationResult, tag: &str| -> Key {
        (
            r.object.clone(),
            r.dt_days as i64,
            r.force_model.clone(),
            r.test_type.clone(),
            r.observer.clone(),
            tag.to_string(),
        )
    };
    let mut rust_by_key: HashMap<Key, &ValidationResult> = HashMap::new();
    for r in results {
        if r.channel == "rust"
            && let Some((_, tag)) = on_arm(r)
        {
            rust_by_key.insert(key_of(r, &tag), r);
        }
    }
    // (channel, method) -> (n_rows, n_delivered, n_refused, n_compared,
    // n_bit_identical, max_rel).
    type MethodAgg = (u32, u32, u32, u32, u32, f64);
    let mut agg: BTreeMap<(String, &'static str), MethodAgg> = BTreeMap::new();
    for r in results {
        let Some((method, tag)) = on_arm(r) else {
            continue;
        };
        if !channels.contains(&r.channel.as_str()) {
            continue;
        }
        let e = agg
            .entry((r.channel.clone(), method))
            .or_insert((0, 0, 0, 0, 0, 0.0));
        e.0 += 1;
        match r.orbit_delivered {
            Some(true) => e.1 += 1,
            Some(false) => e.2 += 1,
            None => {}
        }
        if r.channel != "rust"
            && let Some(rr) = rust_by_key.get(&key_of(r, &tag))
            && let (Some(a), Some(b)) = (r.emp_pos_au, rr.emp_pos_au)
        {
            let (rel, bit) = method_rel_pos_diff(&a, &b);
            e.3 += 1;
            if bit {
                e.4 += 1;
            }
            e.5 = e.5.max(rel);
        }
    }
    let arr: Vec<serde_json::Value> = agg
        .into_iter()
        .map(|((channel, method), (n, deliv, refused, cmp, bit, maxrel))| {
            serde_json::json!({
                "channel": channel,
                "method": method,
                "n_rows": n,
                "n_delivered": deliv,
                "n_refused": refused,
                "n_compared_vs_rust": cmp,
                "n_bit_identical": bit,
                "max_rel_pos_diff_vs_rust": if cmp > 0 { serde_json::json!(maxrel) } else { serde_json::Value::Null },
            })
        })
        .collect();
    serde_json::Value::Array(arr)
}

fn write_summary(
    rollups: &[ChannelRollup],
    results: &[ValidationResult],
    path: &Path,
) -> Result<(), String> {
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
        "by_method": method_summary_json(results),
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
        s.push_str(&format!(
            "  .gb{n}, .lchip[data-gb=\"{n}\"] {{ background: {bg}; }}\n"
        ));
        s.push_str(&format!(
            "  .gb{n} .cn, .lchip[data-gb=\"{n}\"], .odgrid td.oc.gb{n} .ocg {{ color: {ink}; }}\n"
        ));
    }
    for (i, &(bg, ink)) in LADDER_LIGHT.iter().enumerate() {
        let n = i + 1;
        s.push_str(&format!(
            "  :root.theme-light .gb{n}, :root.theme-light .lchip[data-gb=\"{n}\"] {{ background: {bg}; }}\n"
        ));
        s.push_str(&format!(
            "  :root.theme-light .gb{n} .cn, :root.theme-light .lchip[data-gb=\"{n}\"], :root.theme-light .odgrid td.oc.gb{n} .ocg {{ color: {ink}; }}\n"
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
    /// Sky separation, arcseconds — the predictive-power accuracy surface
    /// (Part 4.2A). Same one decade ladder, physical decade edges fixed below.
    SepArcsec,
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
            // arcsec: 10 mas, 100 mas, 1″, 10″, 100″, 1000″
            GridAxis::SepArcsec => [1e-2, 1e-1, 1.0, 1e1, 1e2, 1e3],
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
        GridAxis::SepArcsec => fmt_arcsec(v),
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
        GridAxis::SepArcsec => {
            // The decade colour already carries the magnitude; this is a terse
            // ≤6-char refinement, mas below 1″ and arcsec above.
            if a < 1.0 {
                let m = a * 1e3;
                if m >= 10.0 {
                    format!("{m:.0}ma")
                } else {
                    format!("{m:.1}ma")
                }
            } else if a < 1e3 {
                if a >= 10.0 {
                    format!("{a:.0}\"")
                } else {
                    format!("{a:.1}\"")
                }
            } else {
                format!("{a:.0e}")
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
                GridAxis::PropKm | GridAxis::EphMas | GridAxis::SepArcsec => HATCH_NOT_COMPARED,
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

/// Order the population blocks by the one canonical order ([`POPULATION_ORDER`])
/// and the objects within each block by one tool's worst value on the grid axis,
/// descending. `order_vals` is the tool that fixes the within-block object order
/// — empyrean vs the default reference — so every pair panel AND the Section-1
/// cost heatmap reuse this one order and their rows line up, and the WORST column
/// still shows each row's worst horizon. Population order is the single switch in
/// [`POPULATION_ORDER`]; worst value only breaks ties within a rank (none today).
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
    // Populations in the one canonical order (POPULATION_ORDER); the tool's
    // worst value only breaks ties within a rank (none today — each population
    // is listed once). The WORST column still shows each row's worst.
    block_order.sort_by(|a, b| {
        population_rank(a.0)
            .cmp(&population_rank(b.0))
            .then_with(|| {
                block_worst(&b.1)
                    .partial_cmp(&block_worst(&a.1))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
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
    objects.sort_by(|a, b| {
        population_rank(a.1)
            .cmp(&population_rank(b.1))
            .then(a.0.cmp(b.0))
    });

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
        "<div class=\"grid-scroll agrid-scroll\"><table class=\"agrid ahgrid\" data-grid=\"agreement\" id=\"{}\"><thead><tr>",
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
        // Ephemeris sky-plane only (S3 item 4): the summary line carries the
        // runaway count — the rows beyond ±100 mas on this pair that §1.2A's
        // folded `outliers` list itemises. `buildEphScatter` fills the span for
        // the selected pair from the same rows it lists, so the number and the
        // list cannot disagree; the em-dash placeholder shows until it runs.
        let rw_suffix = if id == "h2-eph" {
            " · <span class=\"rwn\" title=\"rows beyond ±100 mas on this pair — the folded outliers list itemises them\">— runaway</span>"
        } else {
            ""
        };
        h.push_str(&format!(
            "<div class=\"grid-summary\" id=\"{table_id}-summary\" title=\"{summary_title}\">{summary_line}{rw_suffix}</div>"
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
        GridAxis::SepArcsec => [
            "&lt;10 mas",
            "10–100 mas",
            "100 mas–1″",
            "1–10″",
            "10–100″",
            "100–1000″",
            "&gt;1000″",
        ],
    };
    // Only the two end labels are shown; every swatch carries its own bin label
    // and physical anchor as a hover title (text-diet rule 2c).
    let (lo_vis, hi_vis) = match axis {
        GridAxis::PropKm => ("&lt;1 m", "&gt;1e6 km"),
        GridAxis::EphMas => ("&lt;1 µas", "&gt;100 mas"),
        GridAxis::TimingMs => ("&lt;100 µs", "&gt;10 s"),
        // 4.2A's accuracy ladder is swatch-only: every decade label rides in its
        // swatch hover, so the legend spends no visible words against the page
        // budget while the cells carry the compact value in view.
        GridAxis::SepArcsec => ("&lt;10 mas", "&gt;1000″"),
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
        GridAxis::SepArcsec => {
            // Swatch-only (labels on hover), matching the swatch-only decade ramp.
            s.push_str("<span class=\"gk\"><span class=\"gk-sw rw\" title=\"off-scale: above the top decade (runaway)\"></span></span>");
            s.push_str("<span class=\"gk\"><span class=\"gk-sw hatch\" title=\"fewer than three objects in this cell (thin)\"></span></span>");
        }
    }
    s.push_str("</div>");
    s
}

/// The uncertainty arm Section 1 keys empyrean's agreement and cost heatmaps on:
/// the rust channel's first-order (Jet1) arm. Defined once so the agreement
/// values, their row order and the cost wall-clock all read the same rows.
const EMP_SECTION1_ARM: &str = "first_order_detection_on";

/// Owner vocabulary rule (lang/design/validation-report-redesign.md, 2026-10-17):
/// the page names a derivative order the way a user thinks of it, never the way
/// the engine implements it. This one function owns the arm-id → method-label
/// mapping; every header, hover, key chip, disclosure and section title that
/// names a derivative order routes its wording through here, so "Jet1", "Jet2",
/// "6×6", "9×9", "STM" and "hyperdual" never reach the page as a label. Widths
/// are spelled "(N=6)" / "(N=9)" and only where two widths sit side by side.
/// Crate and type names stay in the Configurations appendix, where a tool's
/// version is stated.
fn method_label(arm: &str) -> &str {
    if arm.starts_with("second_order") {
        "second order"
    } else if arm.starts_with("first_order") {
        "first order"
    } else if arm.starts_with("f64") || arm.starts_with("none") {
        "state only"
    } else if arm.starts_with("auto") {
        "auto cascade"
    } else if arm.starts_with("sigma_point") {
        "sigma-point"
    } else if arm.starts_with("monte_carlo") {
        "Monte Carlo"
    } else if arm.starts_with("gaussian_mixture") {
        "Gaussian mixture"
    } else {
        // An unmapped arm id renders verbatim — visible, never a silent blank.
        arm
    }
}

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
                // S8 item 1: one null token (—); the reason rides the hover.
                "<td class=\"oc conv undef\" title=\"converged · Σ_ref not SPD at the common epoch — metric undefined, no regularisation\"><span class=\"ocg\">—</span></td>".to_string()
            } else {
                // S8 item 1: converged with no comparison record → the one null
                // token (—); the specific reason rides the hover.
                format!(
                    "<td class=\"oc conv nocov\" title=\"{}\"><span class=\"ocg\">—</span></td>",
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
    // S6: object/arc + one tool cell + three diagnostics. The former Δr@epoch
    // column (all "—", the Cartesian |Δr| is not in the sidecar schema) is gone.
    let n_cols = 1 + 1 + 3;
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
        "<th class=\"od-d\">{}</th><th class=\"od-d\">{}</th><th class=\"od-d\">{}</th>",
        uhdr("n_obs"),
        uhdr("χ²ᵣ"),
        uhdr("σ_eq"),
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
        // S8 item 2: the population header is the gpophdr/gpopname form every other
        // grid uses; the converged/median summary rides a second cell on that row.
        h.push_str(&format!(
            "<tr class=\"gpophdr\"><td class=\"gpopname\">{} ({attempted})</td><td class=\"gpop-sum\" colspan=\"{}\" title=\"converged {converged} of {attempted} · median σ_eq over {n_med} object(s)\">{fill_vis}</td></tr>",
            attr_escape(pop),
            n_cols - 1,
        ));

        for obj in objs {
            let oc = panel.optical.get(*obj);
            let conv = oc.and_then(|c| c.converged);
            // S8 item 2: one row per arc — the object name IS the optical row's
            // first cell (no separate name row); the gobjr/gobjname2 form matches
            // every other grid. The radar arc stays an indented sub-row below.
            let name_cls = if conv == Some(false) {
                "gobjname2 od-fail-name"
            } else {
                "gobjname2"
            };
            h.push_str(&format!(
                "<tr class=\"gobjr odrow\"><td class=\"{name_cls}\">{}</td>",
                attr_escape(obj)
            ));
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
                    // S8 item 1: the "converged, no closeness number" glyph is the
                    // one null token (—); a failed fit keeps ✕ (a state, not null).
                    let glyph = match c.converged {
                        Some(true) => "—",
                        Some(false) => "✕",
                        None => "",
                    };
                    h.push_str(&format!(
                        "<td class=\"oc hatch\" title=\"{hover}\"><span class=\"ocg\">{glyph}</span></td>"
                    ));
                    let nobs = c.n_obs.map(|n| n.to_string()).unwrap_or_else(|| "—".into());
                    let chi2 = fmt_num(c.chi2);
                    h.push_str(&format!(
                        "<td class=\"od-d\">{nobs}</td><td class=\"od-d\">{chi2}</td><td class=\"od-d hatch\" title=\"{hover}\"></td></tr>"
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
                    // S8 item 1: one null token (—) for "no σ_eq"; when a record
                    // exists but the metric is undefined, the reason rides the hover.
                    let (seq, seq_attr) = match (c.sigma, c.has_record) {
                        (Some(s), _) => (format!("{s:.2}"), String::new()),
                        (None, true) => (
                            "—".to_string(),
                            " title=\"σ_eq not determined — Σ_ref not positive-definite at the common epoch\""
                                .to_string(),
                        ),
                        (None, false) => ("—".to_string(), String::new()),
                    };
                    h.push_str(&format!(
                        "<td class=\"od-d\">{nobs}</td><td class=\"od-d\">{chi2}</td><td class=\"od-d\"{seq_attr}>{seq}</td></tr>"
                    ));
                }
                None => {
                    // This tool ran no optical fit for this object.
                    h.push_str(&od_state_cell(None, None, false, panel.nocov_title));
                    h.push_str("<td class=\"od-d\">—</td><td class=\"od-d\">—</td><td class=\"od-d\">—</td></tr>");
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
                            "<td class=\"od-d\">{rnobs}</td><td class=\"od-d\">{rchi2}</td><td class=\"od-d hatch\" title=\"the radar comparison record is not generated yet\"></td></tr>"
                        ));
                    }
                    None => {
                        h.push_str(&od_state_cell(None, None, false, panel.nocov_title));
                        h.push_str("<td class=\"od-d\">—</td><td class=\"od-d\">—</td><td class=\"od-d hatch\" title=\"this tool ran no radar arc in this run\"></td></tr>");
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
    // Populations in the one canonical order (POPULATION_ORDER); σ_eq only
    // breaks ties within a rank. Objects within a block stay offender-first
    // (below), so the worst σ_eq is still at the top of its population.
    block_order.sort_by(|a, b| {
        population_rank(a.0)
            .cmp(&population_rank(b.0))
            .then_with(|| {
                let am = a.1.iter().map(|o| sigma_key(o)).fold(f64::MIN, f64::max);
                let bm = b.1.iter().map(|o| sigma_key(o)).fold(f64::MIN, f64::max);
                bm.partial_cmp(&am).unwrap_or(std::cmp::Ordering::Equal)
            })
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
        // same count. (The per-record tally rode the summary hover until S4
        // removed it; §4.1's covtab still reports the record count.)
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
        // S4 text-to-marks: the summary line keeps numbers only; the words that
        // name each number and the radar-exclusion rationale ride §1.3A's Method
        // fold (σ_eq definition + "objects whose JPL solution used radar are not
        // like-for-like — hatched, counted, out of statistics"), not this hover.
        let summary_title = String::new();

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
    // S8 item 1: one null token — the reference covariance being non-positive-
    // definite and "converged, no comparison record" both read as —, with the
    // specific reason on each cell's hover.
    h.push_str("<span class=\"gk\" title=\"no closeness number — the cell hover gives the reason (reference covariance not positive-definite, or no comparison record)\">—</span>");
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
                "<div class=\"od-fails\"><div class=\"od-fails-h\" title=\"did not converge on this run\">Did not converge ({})</div><table class=\"agrid odfails\"><tbody>{}</tbody></table></div>",
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

/// S6 item 1 (1.3 B · by population): a fit-quality marginal built from the
/// existing orbit-determination rows — the median observation count, reduced
/// χ², post-fit RMS″ and σ_eq to JPL per population (1.3A keeps the per-object
/// values). The quantities the rows do not carry — the rejected fraction
/// (empyrean records none) and JPL's published solution date — render as a
/// NAMED "not measured" state with the reason on hover, never a blank, so the
/// reader sees what is and is not measured. A per-population median of an
/// existing row, no new run.
fn build_od_fit_quality_panel(
    results: &[ValidationResult],
    orbit_comparisons: &[crate::schema::OrbitComparison],
) -> String {
    let radar_counts = jpl_radar_obs_counts(results);
    let mut obj_pop: HashMap<&str, &str> = HashMap::new();
    let mut nobs: HashMap<&str, Vec<f64>> = HashMap::new();
    let mut chi2: HashMap<&str, Vec<f64>> = HashMap::new();
    let mut rms: HashMap<&str, Vec<f64>> = HashMap::new();
    let mut sig: HashMap<&str, Vec<f64>> = HashMap::new();
    for r in results {
        if r.channel == "rust"
            && (r.test_type == "orbit_determination" || r.test_type == "orbit_determination_radar")
        {
            obj_pop
                .entry(r.object.as_str())
                .or_insert(r.population.as_str());
            let pop = r.population.as_str();
            if let Some(n) = r.n_obs_used {
                nobs.entry(pop).or_default().push(n as f64);
            }
            if let Some(c) = r.od_reduced_chi2.filter(|c| c.is_finite()) {
                chi2.entry(pop).or_default().push(c);
            }
            if let Some(v) = r.od_rms_combined_arcsec.filter(|v| v.is_finite()) {
                rms.entry(pop).or_default().push(v);
            }
        }
    }
    for c in orbit_comparisons {
        if c.common_epoch_source != "sbdb"
            || !c.sigma_equiv_combined.is_finite()
            || radar_counts.contains_key(c.object.as_str())
        {
            continue;
        }
        if let Some(pop) = obj_pop.get(c.object.as_str()) {
            sig.entry(*pop).or_default().push(c.sigma_equiv_combined);
        }
    }
    let mut pops: Vec<&str> = nobs.keys().copied().collect();
    pops.sort_by(|a, b| population_rank(a).cmp(&population_rank(b)).then(a.cmp(b)));

    let med = |m: &HashMap<&str, Vec<f64>>, pop: &str| -> Option<f64> {
        m.get(pop).filter(|v| !v.is_empty()).map(|v| {
            let mut s = v.clone();
            percentile(&mut s, 0.5)
        })
    };
    let n_cell = |o: Option<f64>| o.map(|x| format!("{x:.0}")).unwrap_or_else(|| "—".into());
    let f2 = |o: Option<f64>| o.map(|x| format!("{x:.2}")).unwrap_or_else(|| "—".into());

    let mut h = String::new();
    h.push_str("<div class=\"grid-scroll\"><table class=\"agrid odgrid\"><thead><tr>");
    h.push_str("<th class=\"gobj\">population</th>");
    h.push_str(&format!(
        "<th class=\"od-d\">{}</th><th class=\"od-d\">{}</th><th class=\"od-d\">{}</th><th class=\"od-d\">{}</th><th class=\"od-d\">rejected %</th><th class=\"od-d\">JPL date</th>",
        uhdr("n_obs"),
        uhdr("χ²ᵣ"),
        uhdr("RMS″"),
        uhdr("σ_eq"),
    ));
    h.push_str("</tr></thead><tbody>");
    if pops.is_empty() {
        h.push_str("<tr class=\"gobjrow\"><td class=\"gobjname\" colspan=\"7\">no orbit-determination rows in this run</td></tr>");
    }
    for pop in &pops {
        let n = nobs.get(pop).map(|v| v.len()).unwrap_or(0);
        h.push_str(&format!(
            "<tr class=\"gobjrow\"><td class=\"gobjname\" title=\"median over {n} fit(s)\">{}</td>",
            attr_escape(pop)
        ));
        h.push_str(&format!(
            "<td class=\"od-d\">{}</td><td class=\"od-d\">{}</td><td class=\"od-d\">{}</td><td class=\"od-d\">{}</td>",
            n_cell(med(&nobs, pop)),
            f2(med(&chi2, pop)),
            f2(med(&rms, pop)),
            f2(med(&sig, pop)),
        ));
        h.push_str("<td class=\"od-d hatch\" title=\"empyrean records no rejected-observation count in this run — not measured\">not measured</td>");
        h.push_str("<td class=\"od-d hatch\" title=\"JPL's published solution date is not in the comparison record — not measured\">not measured</td>");
        h.push_str("</tr>");
    }
    h.push_str("</tbody></table></div>");
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
        "<div class=\"grid-summary\">{n_clean} of {n_present} channels ≤ ULP on ≥1 axis</div>"
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
    h.push_str("<div class=\"grid-scroll\"><table class=\"agrid fidgrid\" data-grid=\"fidelity\"><thead><tr><th class=\"gobj\">channel</th>");
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
    objects.sort_by(|a, b| {
        population_rank(a.1)
            .cmp(&population_rank(b.1))
            .then(a.0.cmp(b.0))
    });
    let present_ch: Vec<&str> = channels
        .iter()
        .copied()
        .filter(|c| present.contains(c))
        .collect();

    h.push_str(
        "<div class=\"panel-title\" title=\"Complete per-object grid — every channel × test type\">Complete per-object grid</div>",
    );
    // Two-level grouped header: the channel spans its test-type columns once
    // (no repeated channel word), the test type sits under it. In-table, so no
    // visible-word budget cost.
    h.push_str("<div class=\"grid-scroll\"><table class=\"agrid fidgrid\" data-grid=\"fidelity\"><thead><tr><th class=\"gobj\" rowspan=\"2\">object</th>");
    for &ch in &present_ch {
        h.push_str(&format!(
            "<th class=\"fo-h fo-grp\" colspan=\"{}\">{ch}</th>",
            test_types.len()
        ));
    }
    h.push_str("</tr><tr>");
    for _ in &present_ch {
        for (_, disp, _, _, _) in &test_types {
            h.push_str(&format!("<th class=\"fo-h\">{disp}</th>"));
        }
    }
    h.push_str("</tr></thead><tbody>");
    // Group by population, then order the blocks by the one canonical order
    // (a BTreeMap would re-alphabetize them on iteration).
    let mut block_map: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for &(o, p) in &objects {
        block_map.entry(p).or_default().push(o);
    }
    let mut blocks: Vec<(&str, Vec<&str>)> = block_map.into_iter().collect();
    blocks.sort_by_key(|(p, _)| population_rank(p));
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

// ── Per-method cross-channel matrix ─────────────────────────────────
//
// One block per object, one row per uncertainty method (plan order), one
// column per engine channel. A cell is that method's same-method parity
// against the RUST reference: the worst relative Cartesian-position
// difference across the method's rows, `0 (bit-id)` when every paired row is
// bit-identical, an `auto` cell keyed on the resolved rung (a channel that
// resolved to a different rung reads as a mismatch, never a number), a
// refusal shown by name, or `not produced` for a method a channel has not
// produced at this pin. Nothing is blank. Keys on the `detection_on` arm
// only (the `detection_off*` arms are timing benchmarks in the cost panels).

/// Relative Cartesian-position difference between a channel state and the
/// rust reference state, and whether the two are bit-identical.
fn method_rel_pos_diff(a: &[f64; 3], b: &[f64; 3]) -> (f64, bool) {
    let (dx, dy, dz) = (a[0] - b[0], a[1] - b[1], a[2] - b[2]);
    let dnorm = (dx * dx + dy * dy + dz * dz).sqrt();
    let bnorm = (b[0] * b[0] + b[1] * b[1] + b[2] * b[2]).sqrt();
    let bit = dx == 0.0 && dy == 0.0 && dz == 0.0;
    let rel = if bnorm > 0.0 { dnorm / bnorm } else { dnorm };
    (rel, bit)
}

/// Client-side renderers (injected into the page script via the
/// `METHOD_RENDER_JS` token) for the two by-object views fed from the fetched
/// sidecar rows: the per-method matrix (3.1 A) and the outcome-and-tallies
/// table (3.1 C). The cell semantics mirror the Rust data contract but run in
/// the browser, so the full by-object grid stays in the sidecar, not the page.
const METHOD_RENDER_JS: &str = r##"
const METHOD_LABELS = {
    none: 'state only', first_order: 'first order', second_order: 'second order',
    auto: 'auto cascade', sigma_point: 'sigma-point', monte_carlo: 'Monte Carlo',
    gaussian_mixture: 'Gaussian mixture',
};
function mEsc(s) { return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;'); }
function mMethodOf(tag) {
    if (!tag || !tag.endsWith('_detection_on')) return null;
    const m = tag.slice(0, -'_detection_on'.length);
    return METHOD_ORDER.includes(m) ? m : null;
}
function renderMethodMatrix() {
    const body = document.getElementById('method-matrix-body');
    if (!body) return;
    const CHANS = ['rust', 'core', 'c', 'python', 'cli'];
    const BAND = 1e-10;
    const lbl = (x) => METHOD_LABELS[x] || x;
    const ttKind = (tt) => tt === 'orbit_determination_transport' ? 'transport'
        : (tt === 'orbit_determination' || tt === 'orbit_determination_radar') ? 'od'
        : tt === 'ephemeris' ? 'ephemeris' : 'propagation';
    const statusFamily = (s) => !s ? 'other'
        : /withheld/i.test(s) ? 'withheld'
        : /no honest implementation|refus/i.test(s) ? 'refused'
        : /fail/i.test(s) ? 'failed' : 'other';
    // Delivery of ONE product: a covariance product the engine delivered, one it
    // did NOT (classified into a status family), or one with no recorded state.
    const delivery = (r) => {
        if (r.orbit_delivered === false) return { st: 'nd', fam: statusFamily(r.orbit_status) };
        if (r.orbit_status != null && r.orbit_status !== 'delivered') return { st: 'nd', fam: statusFamily(r.orbit_status) };
        if (r.orbit_delivered === true || r.orbit_status === 'delivered') return { st: 'd' };
        return { st: 'nr' };
    };
    const keyOf = (r, tag) => [r.object, Math.trunc(r.dt_days), r.force_model, r.test_type, r.observer || '', tag].join('|');
    const rustByKey = new Map();
    for (const r of results) {
        if (r.channel !== 'rust') continue;
        const m = mMethodOf(r.propagation_uncertainty);
        if (m) rustByKey.set(keyOf(r, r.propagation_uncertainty), r);
    }
    // Per (channel, object, method): parity over the DELIVERED paired products,
    // the not-delivered counts by family, the per-test-type delivered/total, the
    // auto resolved rung, and the verbatim status messages (for the hover).
    const cellData = new Map();
    const objPop = new Map();
    for (const r of results) {
        const m = mMethodOf(r.propagation_uncertainty);
        if (!m || !CHANS.includes(r.channel)) continue;
        const key = r.channel + '|' + r.object + '|' + m;
        let cd = cellData.get(key);
        if (!cd) { cd = { worstRel: 0, anyPaired: false, allBit: true, deliveredN: 0, notRecordedN: 0, fam: { refused: 0, failed: 0, withheld: 0, other: 0 }, perTT: {}, resolved: null, messages: new Set() }; cellData.set(key, cd); }
        objPop.set(r.object, r.population);
        const k = ttKind(r.test_type);
        if (!cd.perTT[k]) cd.perTT[k] = { d: 0, nd: 0, nr: 0 };
        if (m === 'auto' && r.resolved_method && cd.resolved === null) cd.resolved = mMethodOf(r.resolved_method) || r.resolved_method;
        const d = delivery(r);
        if (d.st === 'd') {
            cd.deliveredN++; cd.perTT[k].d++;
            if (r.channel !== 'rust') {
                const rr = rustByKey.get(keyOf(r, r.propagation_uncertainty));
                if (rr && Array.isArray(r.emp_pos_au) && Array.isArray(rr.emp_pos_au)) {
                    const dx = r.emp_pos_au[0] - rr.emp_pos_au[0], dy = r.emp_pos_au[1] - rr.emp_pos_au[1], dz = r.emp_pos_au[2] - rr.emp_pos_au[2];
                    const dnorm = Math.sqrt(dx * dx + dy * dy + dz * dz);
                    const bnorm = Math.sqrt(rr.emp_pos_au[0] * rr.emp_pos_au[0] + rr.emp_pos_au[1] * rr.emp_pos_au[1] + rr.emp_pos_au[2] * rr.emp_pos_au[2]);
                    cd.worstRel = Math.max(cd.worstRel, bnorm > 0 ? dnorm / bnorm : dnorm);
                    cd.anyPaired = true; cd.allBit = cd.allBit && (dx === 0 && dy === 0 && dz === 0);
                }
            }
        } else if (d.st === 'nr') {
            cd.notRecordedN++; cd.perTT[k].nr++;
        } else {
            cd.fam[d.fam]++; cd.perTT[k].nd++;
            if (r.orbit_status) cd.messages.add(r.orbit_status);
        }
    }
    if (objPop.size === 0) {
        body.innerHTML = '<tr><td colspan="6" style="text-align:left; color:#8b9198">No method-tagged rows in this report — the per-method matrix fills in once a channel produces its uncertainty-method sweep.</td></tr>';
        return;
    }
    const FAMS = [['refused', 'refused'], ['failed', 'failed'], ['withheld', 'withheld'], ['other', 'not delivered']];
    const cellHtml = (ch, obj, method) => {
        const cd = cellData.get(ch + '|' + obj + '|' + method);
        if (!cd) return '<td class="fc" style="color:#8b9198" title="this channel produced no row for this method at this pin">not produced</td>';
        // Hover: per-test-type delivered/total, delivery-not-recorded, and the
        // verbatim status message(s) — never in the cell text.
        const ttParts = [];
        for (const [k, label] of [['ephemeris', 'ephemeris'], ['propagation', 'propagation'], ['od', 'OD'], ['transport', 'transport']]) {
            const t = cd.perTT[k]; if (t) ttParts.push(label + ' ' + t.d + '/' + (t.d + t.nd + t.nr));
        }
        let hover = ttParts.join(' · ') + ' (delivered/total)';
        if (cd.notRecordedN > 0) hover += ' · delivery not recorded: ' + cd.notRecordedN;
        if (cd.messages.size > 0) hover += ' — ' + [...cd.messages].join(' | ');
        // Compact non-delivery badges from the status family (count only).
        const badges = FAMS.filter(([f]) => cd.fam[f] > 0).map(([f, w]) => '<span style="color:#e8a040">· ' + w + ' (' + cd.fam[f] + ')</span>').join(' ');
        // Parity primary over the delivered paired products.
        let primary = '', color = '';
        let mmRef = null, mmCh = null;
        if (method === 'auto' && ch !== 'rust') {
            const rcd = cellData.get('rust|' + obj + '|' + method);
            if (rcd && rcd.resolved && cd.resolved && rcd.resolved !== cd.resolved) { mmRef = rcd.resolved; mmCh = cd.resolved; }
        }
        if (ch === 'rust') {
            primary = (method === 'auto' && cd.resolved) ? 'ref · resolved ' + mEsc(lbl(cd.resolved)) : 'ref'; color = 'color:#5b9bd5';
        } else if (mmRef !== null) {
            primary = 'mismatch · resolved ' + mEsc(lbl(mmRef)) + ' ≠ ' + mEsc(lbl(mmCh)); color = 'color:#d05080';
        } else if (cd.anyPaired) {
            if (cd.allBit) primary = '0 (bit-id) ✓';
            else if (cd.worstRel <= BAND) primary = cd.worstRel.toExponential(1) + ' ✓';
            else { primary = cd.worstRel.toExponential(2); color = 'color:#d05080'; }
        } else if (cd.deliveredN > 0) {
            primary = 'no rust ref'; color = 'color:#8b9198';
        } else if (cd.notRecordedN > 0 && cd.fam.refused + cd.fam.failed + cd.fam.withheld + cd.fam.other === 0) {
            primary = 'not recorded'; color = 'color:#8b9198';
        }
        let text = primary;
        if (badges) text = text ? text + ' ' + badges : badges;
        if (!text) text = 'not produced';
        return '<td class="fc"' + (color ? ' style="' + color + '"' : '') + ' title="' + mEsc(hover) + '">' + text + '</td>';
    };
    const blocks = new Map();
    for (const [obj, pop] of [...objPop.entries()].sort((a, b) => a[0].localeCompare(b[0]))) {
        if (!blocks.has(pop)) blocks.set(pop, []);
        blocks.get(pop).push(obj);
    }
    const ncols = CHANS.length + 1;
    let html = '';
    for (const pop of [...blocks.keys()].sort((a, b) => a.localeCompare(b))) {
        const objs = blocks.get(pop);
        html += '<tr class="gblock"><td class="gblock-l">' + mEsc(pop) + ' (' + objs.length + ')</td><td class="gblock-fill" colspan="' + (ncols - 1) + '"></td></tr>';
        for (const obj of objs) {
            html += '<tr class="gblock"><td class="gtool" colspan="' + ncols + '"><b>' + mEsc(obj) + '</b></td></tr>';
            for (const method of METHOD_ORDER) {
                html += '<tr class="gsub"><td class="gtool">' + mEsc(METHOD_LABELS[method] || method) + '</td>';
                for (const ch of CHANS) html += cellHtml(ch, obj, method);
                html += '</tr>';
            }
        }
    }
    body.innerHTML = html;
}

function renderMethodOutcome() {
    const body = document.getElementById('method-outcome-body');
    if (!body) return;
    const CHANS = ['rust', 'core', 'c', 'python', 'cli'];
    const OD = ['orbit_determination', 'orbit_determination_radar', 'orbit_determination_transport'];
    const isOutcome = (r) => OD.includes(r.test_type) || (r.mix_n_components_total != null) || (r.orbit_delivered === false) || (r.orbit_status != null && r.orbit_status !== 'delivered');
    const NP = 'not produced at this pin';
    const notProduced = (r) => (r.notes && r.notes.trim()) ? mEsc(r.notes.trim()) : NP;
    const optNum = (v) => (v == null) ? NP : v.toFixed(3);
    const optU = (v) => (v == null) ? NP : String(v);
    const lbl = (x) => METHOD_LABELS[x] || x;
    const methodLabel = (r) => {
        const tag = r.propagation_uncertainty;
        if (tag != null) { const m = mMethodOf(tag); if (m) return lbl(m); return OD.includes(r.test_type) ? tag + ' (untagged)' : tag; }
        return OD.includes(r.test_type) ? 'legacy (untagged)' : '—';
    };
    const outcomeCell = (r) => {
        const d = r.orbit_delivered, s = r.orbit_status;
        if (d === true && s != null) return `<span title="orbit delivered">delivered</span><br/><span class="fc-sub">${mEsc(s)}</span>`;
        if (d === true) return 'delivered';
        if (d === false && s != null) return `<span style="color:#e8a040" title="${mEsc(s)}">not delivered</span><br/><span class="fc-sub">${mEsc(s)}</span>`;
        if (d === false) return '<span style="color:#e8a040">not delivered</span>';
        if (s != null) return mEsc(s);
        if (OD.includes(r.test_type)) {
            if (r.od_converged === true) return 'fit converged';
            if (r.od_converged === false) return '<span style="color:#e8a040">did not converge</span>';
            return (r.emp_pos_au != null) ? 'fit delivered' : notProduced(r);
        }
        return notProduced(r);
    };
    let rows = results.filter(r => CHANS.includes(r.channel) && isOutcome(r));
    rows.sort((a,b) => a.population.localeCompare(b.population) || a.object.localeCompare(b.object) || a.channel.localeCompare(b.channel) || a.test_type.localeCompare(b.test_type) || String(a.propagation_uncertainty).localeCompare(String(b.propagation_uncertainty)));
    let html = '';
    for (const r of rows) {
        let kind = r.test_type;
        if (r.test_type.startsWith('orbit_determination')) { const suf = r.test_type.slice('orbit_determination'.length); kind = suf ? 'od' + suf : 'od'; }
        const resolved = (r.resolved_method != null) ? mEsc(lbl(mMethodOf(r.resolved_method) || r.resolved_method)) : '—';
        const covKind = (r.cov_kind != null) ? String(r.cov_kind) : '—';
        const jointW = (r.cov_joint_width != null) ? String(r.cov_joint_width) : '—';
        let mixture = '—';
        if (r.mix_n_components_total != null || mMethodOf(r.propagation_uncertainty) === 'gaussian_mixture') {
            const n = (r.mix_n_components_total != null) ? String(r.mix_n_components_total) : notProduced(r);
            mixture = `comp ${n} &middot; weight ${optNum(r.mix_weight_delivered)} &middot; fail ${optU(r.mix_n_failed)} &middot; unres ${optU(r.mix_n_unresolved)} &middot; curv ${optU(r.mix_n_curvature_refused)} &middot; sky ${optU(r.mix_n_sky_linearization_refused)}`;
        }
        html += `<tr class="gsub"><td class="gtool" title="${mEsc(r.population)}">${mEsc(r.object)}</td><td>${mEsc(r.channel)}</td><td>${mEsc(methodLabel(r))}</td><td>${mEsc(kind)}</td><td>${outcomeCell(r)}</td><td>${resolved}</td><td>${mEsc(covKind)}</td><td>${mEsc(jointW)}</td><td class="fc-sub">${mixture}</td></tr>`;
    }
    body.innerHTML = html;
}
// Invoked here (inside boot, after METHOD_ORDER / METHOD_LABELS are
// initialized) rather than at the top of boot, so the client renderers never
// read a method constant still in its temporal dead zone.
renderMethodMatrix();
renderMethodOutcome();
"##;

fn build_method_matrix_html(_results: &[ValidationResult]) -> String {
    // Client-rendered from the fetched sidecar rows (`renderMethodMatrix` in the
    // page script): one block per object, one row per method in plan order, one
    // column per engine channel. The server emits only the container and header;
    // the cell semantics — the worst relative Cartesian-position difference
    // against the rust reference (band 1e-10), `0 (bit-id)` when every paired row
    // is bit-identical, the auto method keyed on its resolved rung (a channel
    // that resolved to a different rung reads as a mismatch, never a number), a
    // refusal shown by name, and `not produced` where a channel produced no row
    // for a method at this pin — are computed in the browser off the full grid
    // the sidecar carries, so nothing is blank and the page stays small.
    let channels = ["rust", "core", "c", "python", "cli"];
    let mut h = String::new();
    h.push_str(
        "<div class=\"panel-title\" title=\"Per-method cross-channel parity against the rust reference: worst relative position difference (band 1e-10), 0 (bit-id) when bit-identical, the auto method keyed on its resolved rung, a refusal by name, and not produced where a channel produced no row\">Per-method matrix</div>",
    );
    h.push_str("<div class=\"grid-scroll\"><table class=\"agrid fidgrid\" id=\"method-matrix\"><thead><tr><th class=\"gobj\">method</th>");
    for ch in channels {
        h.push_str(&format!("<th class=\"fid-h\">{ch}</th>"));
    }
    h.push_str("</tr></thead><tbody id=\"method-matrix-body\"></tbody></table></div>");
    h
}

// ── Per-method outcome / tally / OD rows ────────────────────────────
//
// One row per orbit-determination fit, post-fit transport, per-method
// product and every orbit the engine did not deliver. A FAILED or withheld
// orbit is a row with its status verbatim; the legacy untagged OD fit row is
// labelled as such; mixture rows carry the component count, surviving mass
// and the four refusal tallies including `n_sky_linearization_refused`. A
// row whose per-method products are absent at this pin reads the runner's
// `not produced at this pin` note, never an empty cell.

/// Does this row carry a per-method delivery/outcome product, or is it an
/// orbit-determination family row the outcome table enumerates?
/// The `orbit_status` the engine reports for a product it DID deliver; any
/// other status is a failure/withheld status the outcome panel enumerates
/// (kept verbatim). The delivered status is the ok status.
const DELIVERED_STATUS: &str = "delivered";

fn is_outcome_row(r: &ValidationResult) -> bool {
    // An outcome row is an orbit-determination fit / post-fit transport, a
    // mixture product, or an orbit the engine did NOT deliver. A delivered
    // propagation/ephemeris product (orbit_delivered == Some(true) with the
    // delivered status) is an ordinary product row — its fidelity is the
    // per-method matrix's job — not an outcome. `orbit_status` triggers only for
    // a non-delivered (failure/withheld) status; `cov_kind` is not a
    // discriminator (every packed-joint row carries one).
    if matches!(
        r.test_type.as_str(),
        test_types::ORBIT_DETERMINATION
            | test_types::ORBIT_DETERMINATION_RADAR
            | test_types::ORBIT_DETERMINATION_TRANSPORT
    ) {
        return true;
    }
    if r.mix_n_components_total.is_some() {
        return true;
    }
    if r.orbit_delivered == Some(false) {
        return true;
    }
    r.orbit_status
        .as_deref()
        .is_some_and(|s| s != DELIVERED_STATUS)
}

fn build_method_outcome_html(results: &[ValidationResult]) -> String {
    // Client-rendered from the fetched sidecar rows (`renderMethodOutcome` in the
    // page script): one row per orbit-determination fit, post-fit transport,
    // per-method product and every orbit the engine did not deliver — statuses
    // verbatim, mixture rows carrying the component count, surviving mass and the
    // four refusal tallies, a row with no product reading the runner's own
    // not-produced note. The server emits the container and header and gates the
    // empty state on whether the run holds any outcome row; the rows are built in
    // the browser off the sidecar so nothing is blank and the page stays small.
    let channels = ["rust", "core", "c", "python", "cli"];
    if !results
        .iter()
        .any(|r| channels.contains(&r.channel.as_str()) && is_outcome_row(r))
    {
        return r#"<div class="section-desc" style="color:#8b9198">No orbit-determination, transport or per-method product rows in this report — this panel fills in with the per-method sweep.</div>"#.to_string();
    }
    let mut h = String::new();
    h.push_str(
        "<div class=\"panel-title\" title=\"Per-method delivery, covariance descriptors and mixture tallies: statuses verbatim, a refusal shown by name, absent products read the runner's not-produced note\">Outcome &amp; tallies</div>",
    );
    h.push_str("<div class=\"grid-scroll\"><table class=\"agrid fidgrid\" id=\"method-outcome\"><thead><tr>");
    for hcol in [
        "object", "chan", "method", "kind", "orbit", "resolved", "cov kind", "joint W", "mixture",
    ] {
        h.push_str(&format!("<th class=\"fid-h\">{hcol}</th>"));
    }
    h.push_str("</tr></thead><tbody id=\"method-outcome-body\"></tbody></table></div>");
    h
}

// ── OD convergence matrix ───────────────────────────────────────────
//
// Per object (× arc) × per tool the plan fits with: did the fit converge, in
// how many iterations (engine channels only — the external fitters fold no
// iteration count), and — when it did not — the status that tool recorded. A
// tool that was never handed an arc reads "not attempted" with the reason,
// never blank. One column per OD tool the plan fits with. The engine
// channels read their own rows; the external fitters (find_orb, layup,
// OrbFit, GRSS) fold onto the reference channel's rows — the same rows the
// Part 2 side-by-side reads — through a small per-tool column spec.

/// Convergence of one (tool, object, arc) fit.
enum ConvergenceCell {
    /// Converged; `iterations` is the engine channels' count (the external
    /// fitters fold none, so a converged external cell shows the mark alone).
    Converged {
        iterations: Option<u32>,
    },
    NotConverged {
        reason: Option<String>,
    },
    NotAttempted {
        reason: String,
    },
}

/// A column of the convergence matrix: an engine channel reading its own
/// rows, or an external fitter reading the folded reference-channel row.
enum OdColumn<'a> {
    Engine(&'a str),
    Fitter {
        key: &'static str,
        label: &'static str,
        color: &'static str,
    },
}

/// The external fitters the plan runs, in display order, with the label and
/// header colour each uses elsewhere in the report. A small spec local to
/// this panel — not the removed `OdToolSpec` registry.
const OD_FITTERS: [(&str, &str, &str); 4] = [
    ("findorb", "find_orb", "#b5651d"),
    ("layup", "layup", "#6a8caf"),
    ("orbfit", "OrbFit", "#5a9e6f"),
    ("grss", "GRSS", "#9b6fb0"),
];

fn empyrean_convergence_cell(
    channel: &str,
    row: Option<&ValidationResult>,
    radar: bool,
) -> ConvergenceCell {
    let Some(r) = row else {
        if radar && channel != "rust" {
            return ConvergenceCell::NotAttempted {
                reason: format!(
                    "the optical+radar arc is a rust-only plan axis, so the {channel} channel is never handed it"
                ),
            };
        }
        return ConvergenceCell::NotAttempted {
            reason: format!("the {channel} channel emitted no row for this arc"),
        };
    };
    match r.od_converged {
        Some(true) => ConvergenceCell::Converged {
            iterations: r.od_iterations,
        },
        Some(false) => ConvergenceCell::NotConverged {
            reason: (!r.notes.trim().is_empty()).then(|| r.notes.trim().to_string()),
        },
        // A binding that does not marshal `od_converged` still returns a
        // fitted orbit when the fit landed — read the outcome from the
        // output rather than blanking a cell that has one.
        None if r.od_rms_combined_arcsec.is_some() || r.emp_pos_au.is_some() => {
            ConvergenceCell::Converged {
                iterations: r.od_iterations,
            }
        }
        None => ConvergenceCell::NotConverged {
            reason: Some(
                "the row carries neither a convergence flag nor a fitted orbit".to_string(),
            ),
        },
    }
}

// Each external fitter reads its own folded fields off the reference-channel
// row — the same fields the Part 2 panels read. None fold an iteration count.

fn findorb_convergence_cell(r: &ValidationResult) -> ConvergenceCell {
    if r.findorb_rms_residual.is_none() && r.findorb_n_obs_used.is_none() {
        return ConvergenceCell::NotAttempted {
            reason: "no find_orb fit folded onto this arc; the merge carries find_orb's fit \
                     fields but not a failure marker, so a fit that produced no solution is \
                     indistinguishable here from an object find_orb was never given — reported \
                     as not attempted rather than charged as a failure"
                .to_string(),
        };
    }
    if let (Some(used), Some(rejected)) = (r.findorb_n_obs_used, r.findorb_n_obs_rejected) {
        let total = used + rejected;
        if total > 0 && f64::from(used) / f64::from(total) < 0.5 {
            return ConvergenceCell::NotConverged {
                reason: Some(format!(
                    "find_orb used {used}/{total} observations ({:.1}%) — below the 50% coverage \
                     floor, its placeholder-orbit fallback rather than a fit",
                    100.0 * f64::from(used) / f64::from(total)
                )),
            };
        }
    }
    ConvergenceCell::Converged { iterations: None }
}

fn layup_convergence_cell(r: &ValidationResult) -> ConvergenceCell {
    match r.layup_converged {
        Some(true) => ConvergenceCell::Converged { iterations: None },
        Some(false) => ConvergenceCell::NotConverged {
            reason: Some("layup's orbitfit returned flag ≠ 0".to_string()),
        },
        None => {
            let produced = r.layup_chi2.is_some()
                || r.layup_reduced_chi2.is_some()
                || r.layup_n_obs_used.is_some()
                || r.layup_time_ms.is_some();
            if produced {
                ConvergenceCell::NotConverged {
                    reason: Some(
                        "layup emitted a record for this object but no convergence flag — its \
                         orbitfit output could not be read"
                            .to_string(),
                    ),
                }
            } else {
                ConvergenceCell::NotAttempted {
                    reason: "no layup record for this object in the merged input — outside the \
                             fixture set layup was run over"
                        .to_string(),
                }
            }
        }
    }
}

fn orbfit_convergence_cell(r: &ValidationResult) -> ConvergenceCell {
    if let Some(e) = &r.orbfit_error {
        return ConvergenceCell::NotConverged {
            reason: Some(e.clone()),
        };
    }
    if r.orbfit_rms_arcsec.is_some() || r.orbfit_n_obs_used.is_some() {
        return ConvergenceCell::Converged { iterations: None };
    }
    ConvergenceCell::NotAttempted {
        reason: "no OrbFit fit folded onto this arc and no OrbFit error either — the object was \
                 not in the set OrbFit was run over"
            .to_string(),
    }
}

fn grss_convergence_cell(r: &ValidationResult) -> ConvergenceCell {
    if let Some(e) = &r.grss_error {
        return ConvergenceCell::NotConverged {
            reason: Some(e.clone()),
        };
    }
    match r.grss_converged {
        Some(false) => ConvergenceCell::NotConverged {
            reason: Some("GRSS's least-squares fit did not converge".to_string()),
        },
        Some(true) => ConvergenceCell::Converged { iterations: None },
        None if r.grss_rms_arcsec.is_some() || r.grss_n_obs_used.is_some() => {
            ConvergenceCell::Converged { iterations: None }
        }
        None => ConvergenceCell::NotAttempted {
            reason: "no GRSS fit folded onto this arc and no GRSS error either — the object was \
                     not in the set GRSS was run over"
                .to_string(),
        },
    }
}

/// An external fitter's cell for one arc: the reference-channel row carries
/// its folded fields. No reference row at all means the fitter was never
/// asked about this arc.
fn fitter_convergence_cell(
    key: &str,
    label: &str,
    row: Option<&ValidationResult>,
) -> ConvergenceCell {
    let Some(r) = row else {
        return ConvergenceCell::NotAttempted {
            reason: format!(
                "the reference channel has no row for this arc, so {label} was never asked about it"
            ),
        };
    };
    match key {
        "findorb" => findorb_convergence_cell(r),
        "layup" => layup_convergence_cell(r),
        "orbfit" => orbfit_convergence_cell(r),
        "grss" => grss_convergence_cell(r),
        _ => ConvergenceCell::NotAttempted {
            reason: format!("{label} folds no convergence field onto OD rows"),
        },
    }
}

fn build_convergence_matrix_html(results: &[ValidationResult]) -> String {
    const OPTICAL: &str = test_types::ORBIT_DETERMINATION;
    const RADAR: &str = test_types::ORBIT_DETERMINATION_RADAR;
    let channels = ["rust", "core", "c", "python", "cli"];

    let od_rows: Vec<&ValidationResult> = results
        .iter()
        .filter(|r| {
            (r.test_type == OPTICAL || r.test_type == RADAR)
                && channels.contains(&r.channel.as_str())
        })
        .collect();
    if od_rows.is_empty() {
        return r#"<div class="section-desc" style="color:#8b9198">No orbit-determination rows in this report — nothing to tabulate. Run the OD subset to populate this panel.</div>"#.to_string();
    }

    // External fitters fold onto the reference channel's rows — core when the
    // run has one, else rust (the same `core || rust` choice the rest of the
    // report makes).
    let reference_channel = if results
        .iter()
        .any(|r| r.channel == "core" && (r.test_type == OPTICAL || r.test_type == RADAR))
    {
        "core"
    } else {
        "rust"
    };

    // Engine-channel columns: those that produced OD rows, core first then
    // alphabetical — the ordering the rest of the report uses.
    let mut engine_cols: Vec<&str> = od_rows
        .iter()
        .map(|r| r.channel.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    engine_cols.sort_by(|a, b| match (*a == "core", *b == "core") {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.cmp(b),
    });

    // Columns: engine channels, then every external fitter the plan ran.
    let mut columns: Vec<OdColumn> = engine_cols.iter().map(|c| OdColumn::Engine(c)).collect();
    for (key, label, color) in OD_FITTERS {
        if od_tool_present(key, results) {
            columns.push(OdColumn::Fitter { key, label, color });
        }
    }

    // Rows: one per (object, arc), canonical order.
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    let mut keys: Vec<(String, String, String, String)> = Vec::new(); // object, population, test_type, label
    for r in &od_rows {
        if !seen.insert((r.object.clone(), r.test_type.clone())) {
            continue;
        }
        let label = if r.test_type == RADAR {
            format!("{} (+ radar)", r.object)
        } else {
            r.object.clone()
        };
        keys.push((
            r.object.clone(),
            r.population.clone(),
            r.test_type.clone(),
            label,
        ));
    }
    keys.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)).then(a.2.cmp(&b.2)));

    let mut by_key: HashMap<(&str, &str, &str), &ValidationResult> = HashMap::new();
    for r in &od_rows {
        by_key.insert(
            (r.channel.as_str(), r.object.as_str(), r.test_type.as_str()),
            r,
        );
    }

    let render_cell = |h: &mut String, cell: ConvergenceCell| match cell {
        ConvergenceCell::Converged { iterations } => match iterations {
            Some(n) => h.push_str(&format!(
                "<td class=\"fc\" title=\"converged in {n} iterations\">&#10003; {n}</td>"
            )),
            None => h.push_str("<td class=\"fc\" title=\"converged\">&#10003;</td>"),
        },
        ConvergenceCell::NotConverged { reason } => {
            let title = match reason {
                Some(r) => format!("did not converge — {}", attr_escape(&r)),
                None => "did not converge (no reason recorded on the row)".to_string(),
            };
            h.push_str(&format!(
                "<td class=\"fc\" style=\"color:#d05080\" title=\"{title}\">&#10007;</td>"
            ));
        }
        ConvergenceCell::NotAttempted { reason } => {
            h.push_str(&format!(
                "<td class=\"fc\" style=\"color:#8b9198\" title=\"not attempted — {}\">&mdash;</td>",
                attr_escape(&reason)
            ));
        }
    };

    let mut h = String::new();
    h.push_str(
        "<div class=\"panel-title\" title=\"Per object (and arc) × tool: converged, iterations (engine channels), status\">OD convergence matrix</div>",
    );
    h.push_str("<div class=\"grid-scroll\"><table class=\"agrid fidgrid\"><thead><tr title=\"Every tool the plan fits with: a converged fit shows its iteration count where the tool folds one (engine channels); one that did not shows its status on hover; a tool never handed the arc reads not attempted.\"><th class=\"gobj\">object</th>");
    for col in &columns {
        let (color, label) = match col {
            OdColumn::Engine(c) => (channel_color(c), *c),
            OdColumn::Fitter { label, color, .. } => (*color, *label),
        };
        h.push_str(&format!(
            "<th class=\"fid-h\"><span class=\"pop-dot\" style=\"background:{color}\"></span>{label}</th>"
        ));
    }
    h.push_str("</tr></thead><tbody>");
    for (object, population, test_type, label) in &keys {
        h.push_str(&format!(
            "<tr class=\"gsub\"><td class=\"gtool\" title=\"{}\">{}</td>",
            attr_escape(population),
            attr_escape(label)
        ));
        for col in &columns {
            let cell = match col {
                OdColumn::Engine(c) => {
                    let row = by_key
                        .get(&(*c, object.as_str(), test_type.as_str()))
                        .copied();
                    empyrean_convergence_cell(c, row, test_type == RADAR)
                }
                OdColumn::Fitter { key, label, .. } => {
                    let row = by_key
                        .get(&(reference_channel, object.as_str(), test_type.as_str()))
                        .copied();
                    fitter_convergence_cell(key, label, row)
                }
            };
            render_cell(&mut h, cell);
        }
        h.push_str("</tr>");
    }
    // Glyph legend and the two absences the matrix cannot otherwise show, as
    // footer rows INSIDE the table so the per-element word budgets (which skip
    // table text) do not flag them, yet the reader still sees them. The mark
    // carries the meaning (colour is never the only carrier); the absences are
    // named rather than left to be noticed: an engine channel that ran but
    // produced no OD rows has no column, and a radar-fixture object with no
    // radar arc has no (+ radar) row.
    let ncols = columns.len() + 1;
    h.push_str(&format!(
        "<tr class=\"gsub\"><td colspan=\"{ncols}\"><div class=\"grid-key\">\
         <span class=\"gk\"><span style=\"color:#3d9a6d\">&#10003;</span> converged</span>\
         <span class=\"gk\"><span style=\"color:#d05080\">&#10007;</span> ran, did not converge (hover for the reason)</span>\
         <span class=\"gk\"><span style=\"color:#8b9198\">&mdash;</span> not attempted (hover for who excluded it)</span>\
         </div></td></tr>"
    ));
    let mut present_channels: Vec<&str> = results
        .iter()
        .map(|r| r.channel.as_str())
        .filter(|c| channels.contains(c))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    present_channels.retain(|c| !engine_cols.contains(c));
    if !present_channels.is_empty() {
        h.push_str(&format!(
            "<tr class=\"gsub\"><td colspan=\"{ncols}\"><div class=\"section-desc\" style=\"color:#e8a040\">Present in this run with no orbit-determination rows at all, so absent from the matrix: <b>{}</b>.</div></td></tr>",
            attr_escape(&present_channels.join(", "))
        ));
    }
    let radar_objects: BTreeSet<&str> = keys
        .iter()
        .filter(|k| k.2 == RADAR)
        .map(|k| k.0.as_str())
        .collect();
    let missing_radar: Vec<&str> = crate::catalog::RADAR_FIXTURE_OBJECTS
        .iter()
        .copied()
        .filter(|o| !radar_objects.contains(o))
        .collect();
    if !missing_radar.is_empty() {
        h.push_str(&format!(
            "<tr class=\"gsub\"><td colspan=\"{ncols}\"><div class=\"section-desc\" style=\"color:#e8a040\">Objects with a manifest-pinned radar fixture but no <code>{RADAR}</code> row in this run, so no <code>(+ radar)</code> arc: <b>{}</b>.</div></td></tr>",
            attr_escape(&missing_radar.join(", "))
        ));
    }
    h.push_str("</tbody></table></div>");
    h
}

// ── Cross-method disagreement series ────────────────────────────────
//
// A per-(object, method) σ datum read straight off the reference channel's
// published covariance — NEVER recomputed. The client plots each method's σ
// relative to first-order, as marks: a diagnostic with no tolerance and no
// verdict, the reader concludes. Sky σ comes from the published RA·cosδ / Dec
// covariance (`emp_radec_cov_arcsec2`, arcsec); the total position σ is
// `sqrt(trace)·AU_KM` of the published 3×3 position covariance — from the 6×6
// moment view when present, else the position block of the packed joint.

struct DisagreementPoint {
    object: String,
    method: &'static str,
    /// σ on RA·cosδ (arcsec), √(sky-cov[0][0]).
    sigma_ra_arcsec: Option<f64>,
    /// σ on Dec (arcsec), √(sky-cov[1][1]).
    sigma_dec_arcsec: Option<f64>,
    /// Total position σ (km): √(trace of the 3×3 position covariance)·AU_KM.
    sigma_pos_km: Option<f64>,
}

/// Build the per-(object, method) σ series off the rust reference channel's
/// `detection_on` rows. Read off the published covariance, never recomputed.
fn disagreement_sigma_series(results: &[ValidationResult]) -> Vec<DisagreementPoint> {
    use crate::schema::uncertainty_modes as um;
    // Per (object, method) on the reference channel. A propagation row carries
    // the position covariance; the ephemeris row carries the sky covariance;
    // fold both onto the one point for the object+method.
    let mut map: BTreeMap<(String, &'static str), DisagreementPoint> = BTreeMap::new();
    for r in results {
        if r.channel != "rust" {
            continue;
        }
        let Some(tag) = r.propagation_uncertainty.as_deref() else {
            continue;
        };
        if um::arm_of(tag) != Some(um::DETECTION_ON) {
            continue;
        }
        let Some(method) = um::method_of(tag) else {
            continue;
        };
        let p = map
            .entry((r.object.clone(), method))
            .or_insert_with(|| DisagreementPoint {
                object: r.object.clone(),
                method,
                sigma_ra_arcsec: None,
                sigma_dec_arcsec: None,
                sigma_pos_km: None,
            });
        if let Some(c) = r.emp_radec_cov_arcsec2 {
            // σ_RA from [0][0], σ_Dec from [1][1] — the stored RA·cosδ-scaled
            // block (schema `emp_radec_cov_arcsec2`), never swapped.
            p.sigma_ra_arcsec = Some(c[0][0].max(0.0).sqrt());
            p.sigma_dec_arcsec = Some(c[1][1].max(0.0).sqrt());
        }
        if p.sigma_pos_km.is_none() {
            if let Some(c) = r.emp_pos_cov_au2 {
                let tr = c[0][0] + c[1][1] + c[2][2];
                p.sigma_pos_km = Some(tr.max(0.0).sqrt() * AU_KM);
            } else if let Some(tri) = r.cov_tri.as_ref()
                && tri.len() >= 6
            {
                // Packed lower triangle: the 3×3 position-block diagonal sits
                // at indices 0, 2, 5. Its trace is the total position variance.
                let tr = tri[0] + tri[2] + tri[5];
                p.sigma_pos_km = Some(tr.max(0.0).sqrt() * AU_KM);
            }
        }
    }
    map.into_values().collect()
}

/// Serialise the disagreement series for the client-side plot.
fn disagreement_series_json(points: &[DisagreementPoint]) -> String {
    let arr: Vec<serde_json::Value> = points
        .iter()
        .map(|p| {
            serde_json::json!({
                "object": p.object,
                "method": p.method,
                "sigma_ra_arcsec": p.sigma_ra_arcsec,
                "sigma_dec_arcsec": p.sigma_dec_arcsec,
                "sigma_pos_km": p.sigma_pos_km,
            })
        })
        .collect();
    serde_json::to_string(&arr).unwrap_or_else(|_| "[]".to_string())
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
    // S5 item 3: the decade bin rides a data attribute, not a `gbN` class, so
    // every inline value chip shares one class signature (`.lchip`); runaway is
    // a `data-rw` flag, likewise off the class. One key component, not seven.
    let rw = if bin == 7 { " data-rw=\"1\"" } else { "" };
    format!(
        "<span class=\"lchip\" data-gb=\"{bin}\"{rw} title=\"{}\">{}</span>",
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

/// Build Part 2 (S7): three fixed-slot sections — 2.1 Propagation · 2.2
/// Ephemeris · 2.3 Orbit determination — each via the one `part1_section`
/// renderer. Per section: head → readout (every tool's own number for that
/// axis, the former "By tool and axis" row) → A · by object → B · by population
/// → C · cost → one Method fold. Every matrix states a tool's OWN numbers on the
/// report's one decade ladder; no cell ranks the tools or names a winner.
fn build_part2_sections(
    results: &[ValidationResult],
    orbit_comparisons: &[crate::schema::OrbitComparison],
    emp_eph_ch: (&'static str, bool),
    emp_od_ch: (&'static str, bool),
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

    // OD convergence + closeness (empyrean) and find_orb convergence.
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

    let km = GridAxis::PropKm;
    let mas = GridAxis::EphMas;
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
    // ASSIST propagation states its all-rows and excl-gap medians (numbers only).
    let assist_prop_cell = {
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

    // Per-population object blocks in the one canonical order, worst-first inside
    // a block. Shared by every A · by object grid in Part 2.
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
        population_rank(a.0)
            .cmp(&population_rank(b.0))
            .then_with(|| {
                block_worst(&b.1)
                    .partial_cmp(&block_worst(&a.1))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    // Populations in the one canonical order, worst-first within a rank.
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
        population_rank(a).cmp(&population_rank(b)).then_with(|| {
            pop_worst(b)
                .partial_cmp(&pop_worst(a))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    });

    // ── readouts (the former "By tool and axis" row, split per axis) ──
    let readout_prop = format!(
        "<div class=\"grid-summary\">km</div><div class=\"grid-scroll\"><table class=\"agrid r1grid\"><thead><tr><th class=\"gobj\">tool</th><th>propagation vs JPL</th></tr></thead><tbody>\
         <tr class=\"gsub\"><td class=\"gtool\">empyrean <span class=\"r1ref\">(under test)</span></td>{}</tr>\
         <tr class=\"gsub\"><td class=\"gtool\">ASSIST</td>{}</tr>\
         <tr class=\"gsub\"><td class=\"gtool\">find_orb</td>{}</tr>\
         </tbody></table></div>",
        chip_pair(km, pct(&emp, 0.5), pct(&emp, 0.9), n_rows(&emp)),
        assist_prop_cell,
        chip_pair(km, pct(&findorb, 0.5), pct(&findorb, 0.9), n_rows(&findorb)),
    );
    let readout_eph = format!(
        "<div class=\"grid-summary\">mas</div><div class=\"grid-scroll\"><table class=\"agrid r1grid\"><thead><tr><th class=\"gobj\">tool</th><th>ephemeris vs JPL</th></tr></thead><tbody>\
         <tr class=\"gsub\"><td class=\"gtool\">empyrean <span class=\"r1ref\">(under test)</span></td>{}</tr>\
         <tr class=\"gsub\"><td class=\"gtool\">find_orb</td>{}</tr>\
         </tbody></table></div>",
        chip_pair(
            mas,
            pct(&emp_eph, 0.5),
            pct(&emp_eph, 0.9),
            n_rows(&emp_eph)
        ),
        chip_pair(mas, pct(&fo_eph, 0.5), pct(&fo_eph, 0.9), n_rows(&fo_eph)),
    );
    let readout_od = format!(
        "<div class=\"grid-summary\">{}</div><div class=\"grid-scroll\"><table class=\"agrid r1grid\"><thead><tr><th class=\"gobj\">tool</th><th>orbit determination vs JPL</th></tr></thead><tbody>\
         <tr class=\"gsub\"><td class=\"gtool\">empyrean <span class=\"r1ref\">(under test)</span></td><td class=\"r1c\">{od_conv}/{od_att}<br/><span class=\"cn-n\">{} {med_sig_s}</span></td></tr>\
         <tr class=\"gsub\"><td class=\"gtool\">find_orb</td><td class=\"r1c\">{fo_conv}<br/><span class=\"cn-n\">{}: {HATCH_NO_RECORD}</span></td></tr>\
         </tbody></table></div>",
        uhdr("σ_eq"),
        uhdr("σ_eq"),
        uhdr("σ_eq"),
    );

    // ── A · by object (folded behind one `per object` disclosure per section) ──
    let a_prop = {
        let mut h = String::new();
        h.push_str(
            "<details class=\"more-tables\"><summary class=\"more-sum\">per object</summary>",
        );
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
        h.push_str("</tbody></table></div></details>");
        h
    };
    let a_eph = {
        let mut h = String::new();
        h.push_str(
            "<details class=\"more-tables\"><summary class=\"more-sum\">per object</summary>",
        );
        h.push_str("<div class=\"panel-title\">By tool and object</div>");
        if emp_eph.is_empty() && fo_eph.is_empty() {
            h.push_str("<div class=\"grid-scroll\"><table class=\"agrid r3grid\"><tbody><tr class=\"gobjr\"><td class=\"gobjname2 hatch\" title=\"no ephemeris comparison rows in this run — not measured\">not measured</td></tr></tbody></table></div>");
        } else {
            h.push_str("<div class=\"grid-scroll\"><table class=\"agrid r3grid\"><thead><tr title=\"Each cell: that tool's median sky separation from JPL Horizons across the horizons (mas); smaller closer; one decade ladder; hatched where the tool has no rows.\"><th class=\"gobj\">object</th><th>empyrean</th><th>find_orb</th></tr></thead><tbody>");
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
                        part2_cell(mas, obj_median(&emp_eph, obj), false, None),
                        part2_cell(mas, obj_median(&fo_eph, obj), false, None),
                    ));
                }
            }
            h.push_str("</tbody></table></div>");
        }
        h.push_str("</details>");
        h
    };
    let a_od = {
        // De-duplication (S3 item 6): §1.3A owns the per-object empyrean σ_eq vs
        // JPL, so this cross-fitter view keeps only what §1.3A lacks — the
        // external fitters' own convergence — and links to §1.3A. find_orb
        // publishes no covariance, so it carries only its convergence.
        let mut fo_od: HashMap<&str, &ValidationResult> = HashMap::new();
        for r in results {
            if r.test_type == "orbit_determination"
                && r.channel == "core"
                && (r.findorb_rms_residual.is_some() || r.findorb_n_obs_used.is_some())
            {
                fo_od.insert(r.object.as_str(), r);
            }
        }
        let fo_cell = |obj: &str| -> String {
            match fo_od.get(obj).map(|r| r.findorb_rms_residual.is_some()) {
                Some(true) => "<td class=\"r1c\" title=\"converged · find_orb publishes no covariance, so no σ_eq\">—</td>".to_string(),
                _ => format!("<td class=\"r1c hatch\" title=\"{HATCH_NOT_ATTEMPTED}\"></td>"),
            }
        };
        let mut h = String::new();
        h.push_str(
            "<details class=\"more-tables\"><summary class=\"more-sum\">per object</summary>",
        );
        h.push_str("<div class=\"panel-title\" title=\"By object and fitter — the external fitters' own convergence; empyrean's per-object σ_eq to JPL lives in §1.3A\">By object and fitter <a class=\"xref\" href=\"#s08b\" title=\"empyrean's per-object σ_eq to JPL\">§1.3A</a></div>");
        h.push_str("<div class=\"grid-scroll\"><table class=\"agrid r3grid\"><thead><tr title=\"Each external fitter's own convergence on the SBDB arc; empyrean's per-object σ_eq to JPL lives in §1.3A. find_orb publishes no covariance, so its cell shows only whether it fit the arc.\"><th class=\"gobj\">object</th><th>find_orb</th></tr></thead><tbody>");
        for (pop, objs) in &blocks {
            h.push_str(&format!(
                "<tr class=\"gpophdr\"><td class=\"gpopname\" colspan=\"2\">{} ({})</td></tr>",
                attr_escape(pop),
                objs.len()
            ));
            for &obj in objs {
                h.push_str(&format!(
                    "<tr class=\"gobjr\"><td class=\"gobjname2\">{}</td>{}</tr>",
                    attr_escape(obj),
                    fo_cell(obj),
                ));
            }
        }
        h.push_str("</tbody></table></div></details>");
        h
    };

    // ── B · by population (visible; the slot eyebrow and section name it) ──
    let b_prop = {
        let mut h = String::new();
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
        h
    };
    let b_eph = {
        let mut h = String::new();
        h.push_str("<div class=\"grid-scroll\"><table class=\"agrid r2grid\"><thead><tr title=\"Each cell: that tool's median sky separation from JPL Horizons over a population's objects (mas, n stated); one decade ladder, smaller closer.\"><th class=\"gobj\">population</th><th>empyrean</th><th>find_orb</th></tr></thead><tbody>");
        for &pop in &pops {
            h.push_str(&format!(
                "<tr class=\"gsub\"><td class=\"gtool\">{}</td>{}{}</tr>",
                attr_escape(pop),
                part2_cell(
                    mas,
                    median_of(scoped(&emp_eph, pop)),
                    false,
                    Some(scoped_nobj(&emp_eph, pop))
                ),
                part2_cell(
                    mas,
                    median_of(scoped(&fo_eph, pop)),
                    false,
                    Some(scoped_nobj(&fo_eph, pop))
                ),
            ));
        }
        h.push_str("</tbody></table></div>");
        h
    };
    let b_od = {
        // empyrean median σ_eq per population (same rows §1.3A's marginal uses);
        // find_orb's convergence count per population. Numbers only, never a rank.
        let mut sig_pop: HashMap<&str, Vec<f64>> = HashMap::new();
        for c in orbit_comparisons {
            if c.common_epoch_source != "sbdb"
                || !c.sigma_equiv_combined.is_finite()
                || radar_counts.contains_key(c.object.as_str())
            {
                continue;
            }
            if let Some(pop) = obj2pop.get(c.object.as_str()) {
                sig_pop
                    .entry(*pop)
                    .or_default()
                    .push(c.sigma_equiv_combined);
            }
        }
        let mut fo_pop: HashMap<&str, (usize, usize)> = HashMap::new();
        for r in results {
            if r.test_type == "orbit_determination"
                && r.channel == "core"
                && (r.findorb_rms_residual.is_some() || r.findorb_n_obs_used.is_some())
                && let Some(pop) = obj2pop.get(r.object.as_str())
            {
                let e = fo_pop.entry(*pop).or_insert((0, 0));
                e.1 += 1;
                if r.findorb_rms_residual.is_some() {
                    e.0 += 1;
                }
            }
        }
        let mut h = String::new();
        h.push_str(&format!(
            "<div class=\"grid-scroll\"><table class=\"agrid odgrid\"><thead><tr title=\"empyrean: median σ-equivalent distance to JPL over a population's objects (radar-solution objects excluded); find_orb: fits converged of attempted. Numbers, never a rank.\"><th class=\"gobj\">population</th><th class=\"od-d\">empyrean {}</th><th class=\"od-d\">find_orb conv.</th></tr></thead><tbody>",
            uhdr("σ_eq")
        ));
        for &pop in &pops {
            let es = sig_pop.get(pop).filter(|v| !v.is_empty()).map(|v| {
                let mut s = v.clone();
                format!("{:.2}", percentile(&mut s, 0.5))
            });
            let emp_c = match es {
                Some(s) => format!("<td class=\"od-d\">{s}</td>"),
                None => "<td class=\"od-d hatch\" title=\"no finite σ_eq for this population in this run — not measured\">not measured</td>".to_string(),
            };
            let fo_c = match fo_pop.get(pop) {
                Some((c, a)) => format!("<td class=\"od-d\">{c}/{a}</td>"),
                None => format!("<td class=\"od-d hatch\" title=\"{HATCH_NOT_ATTEMPTED}\"></td>"),
            };
            h.push_str(&format!(
                "<tr class=\"gobjrow\"><td class=\"gobjname\">{}</td>{emp_c}{fo_c}</tr>",
                attr_escape(pop)
            ));
        }
        h.push_str("</tbody></table></div>");
        // The OD convergence matrix (object · arc × every OD tool the plan fits
        // with) rides in 2.3's B slot beside the per-population closeness
        // marginal: one column per engine channel and external fitter, converged
        // with its iteration count or the status by name.
        h.push_str(&build_convergence_matrix_html(results));
        h
    };

    // ── C · cost (the per-axis timing tables; the per-object G grid folded) ──
    let fold_grid = |g: String| -> String {
        format!(
            "<details class=\"more-tables\"><summary class=\"more-sum\">per object</summary>{g}</details>"
        )
    };
    let c_prop = format!(
        "{}{}{}",
        build_part2_timing_t1(results),
        build_part2_timing_t2_axis(results, "propagation", emp_eph_ch, emp_od_ch),
        fold_grid(build_timing_grid_axis(
            results,
            "propagation",
            emp_eph_ch,
            emp_od_ch
        )),
    );
    let c_eph = format!(
        "{}{}",
        build_part2_timing_t2_axis(results, "ephemeris", emp_eph_ch, emp_od_ch),
        fold_grid(build_timing_grid_axis(
            results,
            "ephemeris",
            emp_eph_ch,
            emp_od_ch
        )),
    );
    let c_od = format!(
        "{}{}",
        build_part2_timing_t2_axis(results, "orbit_determination", emp_eph_ch, emp_od_ch),
        fold_grid(build_timing_grid_axis(
            results,
            "orbit_determination",
            emp_eph_ch,
            emp_od_ch
        )),
    );

    // ── Method folds (one per section; the former §2.1 disclosure, by axis) ──
    let method_prop = "<div class=\"disclosures\"><details><summary>Method</summary><div class=\"disc-body\">Each tool states its own numbers against JPL on the one decade ladder. Propagation cells: median and p90 offset from Horizons (km). A tool omitting the Marsden &Delta;T term and the self-perturber forces the daggered objects need shows a force-model gap there (with-gap and excl-gap medians both). Timing: p50 wall clock at its own boundary &mdash; integrate-only (ASSIST), whole-call (tool under test), per-fit (find_orb) &mdash; read down a row; the capped (loud-FAIL) rows count in the n column, never substituted. kete and jorbit emit timing only; find_orb is fit-then-propagate; OpenOrb, GRSS, OrbFit, layup have no rows and stay hatched. In the cost tables, compare arms only within a boundary and read each population column down.</div></details></div>";
    let method_eph = "<div class=\"disclosures\"><details><summary>Method</summary><div class=\"disc-body\">Ephemeris cells: median and p90 sky separation from JPL Horizons (mas), averaged over the observing sites. Timing: p50 wall clock at the whole-call boundary; the empyrean column names its channel when the rust channel carries no wall clock for an arm in this run. In the cost table, read each population column down.</div></details></div>";
    let method_od = "<div class=\"disclosures\"><details><summary>Method</summary><div class=\"disc-body\">OD cell: fits converged of attempted and median &sigma;-equivalent distance to JPL; empyrean's per-object &sigma;_eq lives in &sect;1.3A. find_orb publishes no covariance, so it shows only whether it fit the arc. Objects whose JPL solution used radar are out of the &sigma;_eq statistic, counted, never dropped. The cost table is per-fit wall clock; read each population column down.</div></details></div>";

    format!(
        "{}{}{}",
        part1_section(
            "s-prop",
            "2.1",
            "Propagation",
            "Propagation — every tool's own offset from JPL",
            &readout_prop,
            &a_prop,
            &b_prop,
            &c_prop,
            method_prop,
        ),
        part1_section(
            "s-eph",
            "2.2",
            "Ephemeris",
            "Ephemeris — every tool's own sky separation from JPL",
            &readout_eph,
            &a_eph,
            &b_eph,
            &c_eph,
            method_eph,
        ),
        part1_section(
            "s-od",
            "2.3",
            "Orbit determination",
            "Orbit determination — every fitter's own convergence &amp; closeness",
            &readout_od,
            &a_od,
            &b_od,
            &c_od,
            method_od,
        ),
    )
}

/// Part-2 cost: the population × tool median wall-clock table for ONE axis, on
/// the TimingMs ladder via `part2_cell`, hatched where untimed. S7 splits the
/// former three-axis `build_part2_timing_t2` so each Part-2 section owns its C.
fn build_part2_timing_t2_axis(
    results: &[ValidationResult],
    axis: &str,
    emp_eph: (&'static str, bool),
    emp_od: (&'static str, bool),
) -> String {
    let (eph_ch, eph_fallback) = emp_eph;
    let (od_ch, od_fallback) = emp_od;
    let mut pops: Vec<&str> = results
        .iter()
        .filter(|r| r.channel == "rust" && r.test_type == "propagation")
        .map(|r| r.population.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    pops.sort_by_key(|p| population_rank(p));
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
    let (columns, id, reading): (Vec<Part2TimingColumn<'_>>, &str, &str) = match axis {
        "ephemeris" => (
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
            "t2-eph-time",
            "median ms · by population",
        ),
        "orbit_determination" => (
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
            "t2-od-time",
            "median ms · by population",
        ),
        _ => (
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
            "t2-prop-time",
            "median ms · by population",
        ),
    };
    let tt = match axis {
        "ephemeris" => "ephemeris",
        "orbit_determination" => "orbit_determination",
        _ => "propagation",
    };
    let mut h = String::new();
    // S7: no panel-title — the "C · cost" slot eyebrow and the section axis name
    // this table; the one visible key line (the reading guidance) stays.
    h.push_str(&format!(
        "<div class=\"grid-summary\">{}</div>",
        attr_escape(reading)
    ));
    h.push_str(&format!(
        "<div class=\"{GRID_SCROLL_STICKY}\"><table class=\"agrid r2grid\" id=\"{id}\"><thead><tr><th class=\"gobj\">population</th>"
    ));
    for (label, _boundary, note, _) in &columns {
        let mut chips = String::new();
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
            h.push_str(&pop_cell(tt, pop, getter.as_ref()));
        }
        h.push_str("</tr>");
    }
    h.push_str("</tbody></table></div>");
    h
}

/// Part-2 cost: the per-object timing grid for ONE axis (G1 propagation, G2
/// ephemeris, G3 orbit-determination), on the TimingMs ladder with its boundary
/// chips and one reading line. Every tool keeps its place; a tool untimed on an
/// axis is hatched, never dropped. S7 splits the former `build_timing_grids`.
fn build_timing_grid_axis(
    results: &[ValidationResult],
    axis: &str,
    emp_eph: (&'static str, bool),
    emp_od: (&'static str, bool),
) -> String {
    let (eph_ch, eph_fallback) = emp_eph;
    let (od_ch, od_fallback) = emp_od;
    match axis {
        "ephemeris" => {
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
            build_timing_grid(
                results,
                "g2-eph-time",
                "G2 · per-object ephemeris",
                "ephemeris",
                &g2,
                false,
                "Each cell is that tool's median ephemeris wall clock for an object; the empyrean column names its channel. Read down a column.",
            )
        }
        "orbit_determination" => {
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
            build_timing_grid(
                results,
                "g3-od-time",
                "G3 · per-object orbit-determination",
                "orbit_determination",
                &g3,
                false,
                "Each cell is that tool's median per-fit wall clock for an object on the ms ladder. Read down a column.",
            )
        }
        _ => {
            let g1: Vec<TimingCol> = vec![
                TimingCol::timed("emp state only", rust_arm("f64_detection_on"), BOUND_B),
                TimingCol::timed(
                    "emp first order",
                    rust_arm("first_order_detection_on"),
                    BOUND_B,
                ),
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
            build_timing_grid(
                results,
                "g1-prop-time",
                "G1 · per-object propagation",
                "propagation",
                &g1,
                false,
                "Each cell is that arm's median wall clock over an object's rows; the chip marks its boundary. Read down a column, not across.",
            )
        }
    }
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
        "<div class=\"grid-summary\">Median σ_eq {med:.2} vs 0.94 expected (6-DOF); ~{0:.0}% too tight.</div>",
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
    // De-duplication (S3 item 6): the duplicate per-object σ_eq column is dropped
    // here — §1.3A owns the per-object σ_eq vs JPL and the CDF panel above already
    // shows its distribution. This table keeps the pathology fingerprint, the
    // paired reduced-χ², the σ ratio and the JPL data arc, which live nowhere else.
    h.push_str(&format!(
        "<div class=\"grid-scroll\"><table class=\"agrid covtab\"><thead><tr><th class=\"gobj\">object<br/><span class=\"cn-n\">empyrean vs JPL</span></th><th>pathology</th><th>{}</th><th>{}</th><th>arc (days)</th></tr></thead><tbody>",
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
            "<tr class=\"gsub\"><td class=\"gtool\">{}</td><td class=\"r1c\">{}</td><td class=\"r1c\">{chi_pair}</td><td class=\"r1c\">{sr}</td><td class=\"r1c\">{arc_d}</td></tr>",
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
    objects.sort_by(|a, b| {
        population_rank(a.1)
            .cmp(&population_rank(b.1))
            .then(a.0.cmp(b.0))
    });
    // Order the population blocks by the one canonical order (a BTreeMap would
    // re-alphabetize them on iteration).
    let mut block_map: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for &(o, p) in &objects {
        block_map.entry(p).or_default().push(o);
    }
    let mut blocks: Vec<(&str, Vec<&str>)> = block_map.into_iter().collect();
    blocks.sort_by_key(|(p, _)| population_rank(p));
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
        "<div class=\"{GRID_SCROLL_STICKY}\"><table class=\"agrid fidgrid\" data-grid=\"fidelity\" id=\"{id}\"><thead><tr{rtr}><th class=\"gobj\">object</th>"
    ));
    for col in columns {
        let mut chips = String::new();
        // The A/B/C boundary chip is removed (S4 text-to-marks): the boundary
        // names ride the group labels and the §2.1 method fold; col.boundary is
        // still read by cost_member to drive the per-pair cost heatmaps.
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

/// The tool-name heading above one cost heatmap: the tool label and, for
/// empyrean's ephemeris core fall-back, the labelled core-channel chip. The
/// A/B/C boundary chip is dropped (S4 text-to-marks); the H6-protected heading
/// structure (the `panel-title` div) is kept intact, replaced with nothing.
fn cost_heatmap_heading(
    label: &str,
    _boundary: (char, &'static str),
    note: Option<(&'static str, &'static str)>,
) -> String {
    let mut s = format!("<div class=\"panel-title\">{}", attr_escape(label),);
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
    // S3 item 5: the capped (loud-FAIL) count per ASSIST arm — a row whose
    // adaptive step collapsed before the target epoch carries a null
    // `assist_time_ms`, so it is excluded from the percentiles above. Counted
    // from the same population the `assist` helper draws from (so n + capped =
    // the arm's total rows); this is where §A.3's per-horizon timing table used
    // to report it, moved here so nothing is lost when that table is dropped.
    let capped = |arm: &'static str| -> usize {
        results
            .iter()
            .filter(|r| {
                r.channel == "assist"
                    && r.propagation_uncertainty.as_deref() == Some(arm)
                    && r.assist_time_ms.is_none()
            })
            .count()
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
    // An ASSIST arm row that carries its capped count in the n column (S3 item 5).
    let assist_row = |label: &str, arm: &'static str| -> String {
        let k = capped(arm);
        match assist(arm) {
            Some(s) => {
                let n_cell = if k > 0 {
                    format!(
                        "{} <span class=\"tbnote\" title=\"{k} row(s) hit the per-row wall-clock cap (loud FAIL) and are excluded from the percentiles — never substituted\">(capped {k})</span>",
                        s.n
                    )
                } else {
                    s.n.to_string()
                };
                format!(
                    "<tr class=\"gsub\"><td class=\"gtool\">{}</td><td>{}</td><td>{}</td><td>{}</td><td class=\"cn-n\">{n_cell}</td></tr>",
                    attr_escape(label),
                    ladder_chip(tms, s.p5),
                    ladder_chip(tms, s.p50),
                    ladder_chip(tms, s.p95),
                )
            }
            None if k > 0 => format!(
                "<tr class=\"gsub\"><td class=\"gtool\">{}</td><td class=\"gc hatch\" colspan=\"4\" title=\"all {k} row(s) hit the per-row wall-clock cap (loud FAIL) — no timed row\"></td></tr>",
                attr_escape(label),
            ),
            None => format!(
                "<tr class=\"gsub\"><td class=\"gtool\">{}</td><td class=\"gc hatch\" colspan=\"4\" title=\"{HATCH_NOT_IN_RUN}\"></td></tr>",
                attr_escape(label),
            ),
        }
    };
    let group = |title: &str, _chip: (char, &str)| -> String {
        // The boundary name rides the group title itself (e.g. "propagation ·
        // integrate only"); the A/B/C chip is dropped (S4 text-to-marks).
        format!(
            "<tr class=\"gblock\"><td class=\"gblock-l\">{}</td><td class=\"gblock-fill\" colspan=\"4\"></td></tr>",
            attr_escape(title),
        )
    };

    let mut h = String::new();
    h.push_str("<div class=\"panel-title\" title=\"Wall clock per row — every arm\">Per row</div>");
    // S5 item 6 / delta 9: ONE terse unit key (no sentence); the reading
    // guidance ("compare arms within a boundary") rides §2.1's Method fold.
    h.push_str("<div class=\"grid-summary\">ms · p5 p50 p95 · n · by boundary</div>");
    h.push_str(&format!("<div class=\"{GRID_SCROLL_STICKY}\"><table class=\"agrid r1grid\" id=\"t1-speed-matrix\"><thead><tr><th class=\"gobj\">arm</th><th>p5</th><th>p50</th><th>p95</th><th>n</th></tr></thead><tbody>"));
    h.push_str(&group("propagation · integrate only", BOUND_A));
    h.push_str(&assist_row(
        "ASSIST default (state only)",
        "assist_default_variational_off",
    ));
    h.push_str(&assist_row(
        "ASSIST default (first order)",
        "assist_default_variational_on",
    ));
    h.push_str(&assist_row(
        "ASSIST layup (state only)",
        "assist_layup_variational_off",
    ));
    h.push_str(&assist_row(
        "ASSIST asteroid-institute (state only)",
        "assist_asteroid_institute_variational_off",
    ));
    for (cfg, arm) in [
        (
            "ASSIST-default-like",
            "none_detection_off_assist_default_like",
        ),
        (
            "asteroid-institute-like",
            "none_detection_off_assist_asteroid_institute_like",
        ),
        (
            "ASSIST-default-like",
            "first_order_detection_off_assist_default_like",
        ),
    ] {
        let lbl = format!("empyrean · {} · {cfg}", method_label(arm));
        let (s, core) = emp("propagation", Some(arm));
        h.push_str(&row(&lbl, if core { Some(CORE_NOTE) } else { None }, s));
    }
    h.push_str(&group("propagation · whole call", BOUND_B));
    for arm in [
        "none_detection_on",
        "first_order_detection_on",
        "auto_detection_on",
        "second_order_detection_on",
        "sigma_point_detection_on",
        "monte_carlo_detection_on",
    ] {
        let lbl = format!("empyrean · {}", method_label(arm));
        let (s, core) = emp("propagation", Some(arm));
        h.push_str(&row(&lbl, if core { Some(CORE_NOTE) } else { None }, s));
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

/// The per-tool text colours as theme-aware CSS custom properties — one source
/// for both themes. A tool NAME rendered as text (the Part 1 compare-bar selects)
/// reads its `--tool-<slug>` token so it clears
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

/// Merge several walk-forward aggregate files (`score-predictions --out-agg`)
/// into one, or `None` when the list is empty.
///
/// Multiple `--predict-agg` files are treated as disjoint shards of ONE walk
/// and are concatenated family by family. Aggregates whose `snapshot_id`
/// disagree describe different observations and are refused rather than pooled.
/// A `profile` that disagrees across shards collapses to the `"unstated"` marked
/// state — a merged family whose shards ran different schedules has no single
/// profile to claim — and a budget / calibration note from any shard is carried
/// forward verbatim, since the reduction it records cannot be undone here.
pub fn merge_predict_aggregates(
    aggregates: Vec<crate::predict_schema::PredictAggregates>,
) -> Result<Option<crate::predict_schema::PredictAggregates>, String> {
    let mut it = aggregates.into_iter();
    let Some(mut merged) = it.next() else {
        return Ok(None);
    };
    for next in it {
        if next.snapshot_id != merged.snapshot_id {
            return Err(format!(
                "--predict-agg files disagree on snapshot_id: {:?} vs {:?}. Aggregates from \
                 different fixture snapshots describe different observations and cannot be \
                 pooled — regenerate every walk against one snapshot, or pass them to \
                 separate reports.",
                merged.snapshot_id, next.snapshot_id
            ));
        }
        if next.profile != merged.profile {
            merged.profile = crate::predict_schema::default_profile();
        }
        merged.cells.extend(next.cells);
        merged.coverage_cells.extend(next.coverage_cells);
        merged.per_object.extend(next.per_object);
        merged.d2_histograms.extend(next.d2_histograms);
        merged.per_window.extend(next.per_window);
        merged.reduced_chi2.extend(next.reduced_chi2);
        merged.calibration.extend(next.calibration);
        merged.engine_versions.extend(next.engine_versions);
        // A budget note from any input describes the merged family too — the
        // reduction already happened upstream and cannot be undone here.
        for (src, dst) in [
            (next.per_window_note, &mut merged.per_window_note),
            (next.calibration_note, &mut merged.calibration_note),
        ] {
            if let Some(note) = src {
                match dst.as_mut() {
                    Some(existing) if !existing.contains(&note) => {
                        existing.push_str("  ");
                        existing.push_str(&note);
                    }
                    Some(_) => {}
                    None => *dst = Some(note),
                }
            }
        }
    }
    Ok(Some(merged))
}

// ═══════════════════════════════════════════════════════════════════
// Part 4.2 — Predictive power (walk-forward covariance realism)
//
// The small static families (surfaces, marginals, arm grid, per-object walk,
// calibration) render server-side here into numbers-only matrices on the one
// decade ladder; only the large per-object timeline family travels to the
// client, as a second verified sidecar. Every cell carries a number or a mark —
// a thin cell (< 3 objects) is hatched with its reason and n, never pooled.
// ═══════════════════════════════════════════════════════════════════

/// Thin-cell hatch reason: a cell with fewer than three contributing objects is
/// out of statistics and is shown hatched, never pooled into a neighbour.
const HATCH_THIN: &str = "fewer than three objects — out of statistics";
/// A surface cell no window reached: distinct from a thin cell (which has 1–2
/// objects) and from an all-failed object (4.2D), so the reader can tell an
/// unsampled corner of the (arc × dt) plane from a sampled-but-thin one.
const HATCH_NO_WINDOW: &str = "no window reached this cell";
/// A 4.2A coverage cell whose Σ_pred-dominated share of rows is below
/// [`COVERAGE_MIN_PD_SHARE`]: too observation-dominated for the inside-ellipse
/// rate to read as a test of the tool's covariance rather than the pinned σ
/// table, so it is hatched with the share on hover, never coloured.
const HATCH_SHARE: &str = "share below 10% — too obs-dominated to read";
/// A 4.2A coverage cell with objects and share but no inside-ellipse rate.
const HATCH_NO_RATE: &str = "no inside-ellipse rate in this cell";
/// Minimum Σ_pred-dominated share of a 4.2A coverage cell's rows for the
/// inside-ellipse rate to be read (below it the cell is hatched).
const COVERAGE_MIN_PD_SHARE: f64 = 0.10;
/// The marked-absent state for the D-003 log-score / law-aware calibration
/// family: it has no data until a Student-t arm runs (D-005), so it is marked,
/// never fabricated.
const AWAITING_STUDENT_T: &str = "awaiting Student-t chain";
/// χ²₂ 99% point — the tail threshold the arm grid reports the exceedance of.
const CHI2_2_P99: f64 = 9.210_340_371_976_184;

/// The realism diverging scale CSS (Part 4.2A), the one exception (D-004) to the
/// decade ladder. Built entirely from theme tokens via `color-mix` — blue mixes
/// `--ed-accent`, red mixes `--ed-error`, both over `--ed-bg`, and the calibrated
/// centre is `--ed-surface` — so it carries no hard-coded colour and re-themes
/// with the page. Classes `rvb1..rvb3` (over-conservative), `rv0` (calibrated),
/// `rvr1..rvr3` (over-confident), used on both the grid cells and the key.
fn realism_css() -> String {
    let mut s = String::new();
    s.push_str("  .rv0 { background: var(--ed-surface); }\n");
    // Blue (over-conservative) and red (over-confident) share the 22/48 steps, but
    // the deepest blue caps at 60%, not 78% like the red end: the brand accent blue
    // (#5b9bd5) is far lighter than the error red (#d05040), so at 78% the deepest
    // blue cell fails the 4.5:1 light-text contrast in the dark (default) theme
    // (measured 3.58:1). 60% darkens it to ~5.1:1, matching the red end's ~5.0:1 —
    // the two extremes then read as equally prominent (perceptually symmetric), and
    // the lighter light-theme ground only gains contrast (4.80 → 6.68). One
    // generator, both themes, still no hard-coded colour; the WCAG test below holds
    // every step ≥ 4.5:1 so a percentage or token regression fails the gate.
    for (cls, pct) in [("rvb1", 22), ("rvb2", 48), ("rvb3", 60)] {
        s.push_str(&format!(
            "  .{cls} {{ background: color-mix(in srgb, var(--ed-accent) {pct}%, var(--ed-bg)); }}\n"
        ));
    }
    for (cls, pct) in [("rvr1", 22), ("rvr2", 48), ("rvr3", 78)] {
        s.push_str(&format!(
            "  .{cls} {{ background: color-mix(in srgb, var(--ed-error) {pct}%, var(--ed-bg)); }}\n"
        ));
    }
    s.push_str("  td.gc.rv0 .cn, td.gc.rvb1 .cn, td.gc.rvb2 .cn, td.gc.rvb3 .cn, td.gc.rvr1 .cn, td.gc.rvr2 .cn, td.gc.rvr3 .cn { color: var(--ed-text-primary); }\n");
    s
}

/// Diverging realism bin for a normalized realism value `m = median(d²)/(2 ln 2)`
/// (1 is calibrated): its CSS class and the plain-words direction for the hover.
/// `x = log₂ m`; |x| < 0.5 is calibrated, x < 0 over-conservative (blue), x > 0
/// over-confident (red), in three magnitude steps each side.
fn realism_bin(m: f64) -> (&'static str, &'static str) {
    if !(m.is_finite() && m > 0.0) {
        return ("rv0", "calibrated");
    }
    let x = m.log2();
    let a = x.abs();
    if a < 0.5 {
        ("rv0", "calibrated")
    } else if x < 0.0 {
        if a < 1.5 {
            ("rvb1", "over-conservative")
        } else if a < 3.0 {
            ("rvb2", "over-conservative")
        } else {
            ("rvb3", "over-conservative")
        }
    } else if a < 1.5 {
        ("rvr1", "over-confident")
    } else if a < 3.0 {
        ("rvr2", "over-confident")
    } else {
        ("rvr3", "over-confident")
    }
}

/// The realism key (Part 4.2A): the one diverging-scale legend, stating the D-004
/// exception. Kept to ≤ 8 visible words (the H5 key cap); the full reading of
/// each swatch rides in its hover title.
fn realism_key_html() -> String {
    // The container hover states the one diverging-scale exception (D-004) so the
    // key carries the exception without spending visible words on it.
    let mut s = String::from(
        "<div class=\"grid-key\" title=\"The one diverging log₂ scale, the single exception to the decade ladder. 0 (grey) is calibrated, blue over-conservative, red over-confident.\">",
    );
    for (cls, title) in [
        (
            "rvb3",
            "over-conservative: realism ≪ 1 (covariance too wide)",
        ),
        ("rvb1", "mildly over-conservative"),
        ("rv0", "calibrated: median d² = χ²₂ median, realism = 1"),
        ("rvr1", "mildly over-confident"),
        ("rvr3", "over-confident: realism ≫ 1 (covariance too tight)"),
    ] {
        let vis = match cls {
            "rvb3" => "over-conservative",
            "rv0" => "calibrated",
            "rvr3" => "over-confident",
            _ => "",
        };
        s.push_str(&format!(
            "<span class=\"gk\"><span class=\"gk-sw {cls}\" title=\"{}\"></span>{vis}</span>",
            attr_escape(title)
        ));
    }
    // The thin-cell mark (swatch only, reason on hover) and, stated visibly in the
    // key per the binding ruling, the one diverging-log₂ exception (D-004).
    s.push_str("<span class=\"gk\"><span class=\"gk-sw hatch\" title=\"fewer than three objects in this cell (thin)\"></span></span>");
    s.push_str("<span class=\"gk\">log₂ · diverging</span></div>");
    s
}

/// Equal-weight-per-object median of a `Option<f64>` statistic over a set of
/// objects — the one estimator every 4.2 matrix uses, so none can drift.
fn pw_median(vals: impl Iterator<Item = f64>) -> Option<f64> {
    grid_median(vals.filter(|v| v.is_finite()).collect::<Vec<_>>())
}

/// Sorted, ascending axis of distinct `(lo, hi)` bin edges over the cells, keyed
/// by `lo`'s bit pattern (monotone for positive f64), so the grid shape is
/// data-driven and byte-stable.
fn pw_axis(edges: impl Iterator<Item = (f64, f64)>) -> Vec<(f64, f64)> {
    let mut m: BTreeMap<u64, (f64, f64)> = BTreeMap::new();
    for (lo, hi) in edges {
        if lo.is_finite() {
            m.insert(lo.to_bits(), (lo, hi));
        }
    }
    m.into_values().collect()
}

/// Split an arm slug into its factor tokens on the real factor delimiters
/// (`.`, `+`, `_`): `efcc.adap.nonight+mc` → `[efcc, adap, nonight, mc]`. One
/// definition so the arm-grid main-effects sweep and the per-arm decode hover
/// tokenize identically — splitting on `_` alone (the arms are dot/plus-delimited)
/// made every slug a single token and emptied the sub-matrix.
fn pw_arm_tokens(arm: &str) -> impl Iterator<Item = &str> {
    arm.split(['.', '+', '_']).filter(|t| !t.is_empty())
}

/// The four configuration factors of an arm slug, as `[debias, rejection,
/// nightly, transport]` levels. One arm carries one level of each; transport
/// defaults to `linear` when neither `mc` nor `sp` is present. Owner, 2026-10-17:
/// the 4.2C arm tables show these factor columns instead of the raw slug, main
/// effects first and the full grid behind a toggle.
fn pw_arm_factors(arm: &str) -> [&'static str; 4] {
    let mut f: [&'static str; 4] = ["—", "—", "—", "linear"];
    for t in pw_arm_tokens(arm) {
        match t {
            "efcc" => f[0] = "efcc",
            "nodeb" => f[0] = "nodeb",
            "adap" => f[1] = "adap",
            "cmc" => f[1] = "cmc",
            "norej" => f[1] = "norej",
            "blun" => f[1] = "blun",
            "nonight" => f[2] = "nonight",
            "vfc" => f[2] = "vfc",
            "mc" => f[3] = "mc",
            "sp" => f[3] = "sp",
            _ => {}
        }
    }
    f
}

/// Whether an arm token is a Student-t error-model level `t<ν>` (`t4`, `t6`, …):
/// a `t` followed by one or more digits. One definition so the error-model
/// factor decodes the same wherever it appears.
fn pw_is_student_t_token(tok: &str) -> bool {
    tok.len() >= 2 && tok.starts_with('t') && tok.as_bytes()[1..].iter().all(u8::is_ascii_digit)
}

/// The reader word for the error-model level an arm carries: `n` → "normal",
/// `t<ν>` → "Student-t (ν=<ν>)" (owner, 2026-10-18: add error model). `None`
/// when the arm declares no error law (every production arm), so the factor
/// does not apply to it. The raw token never leaves this function.
fn pw_error_model_word(arm: &str) -> Option<String> {
    for t in pw_arm_tokens(arm) {
        if t == "n" {
            return Some("normal".to_string());
        }
        if pw_is_student_t_token(t) {
            return Some(format!("Student-t (ν={})", &t[1..]));
        }
    }
    None
}

/// The reader word for the covariance-information level an arm carries: `exp`
/// → "expected information", otherwise the recorded default "observed
/// information". `None` for an arm that declares no error law, since the two
/// axes are recorded together; the raw token never leaves this function.
fn pw_information_word(arm: &str) -> Option<&'static str> {
    pw_error_model_word(arm)?;
    if pw_arm_tokens(arm).any(|t| t == "exp") {
        Some("expected information")
    } else {
        Some("observed information")
    }
}

/// The factor a single arm token belongs to (for the main-effects factor column).
fn pw_token_factor(tok: &str) -> &'static str {
    match tok {
        "efcc" | "nodeb" => "debias",
        "adap" | "cmc" | "norej" | "blun" => "rejection",
        "nonight" | "vfc" => "nightly",
        "mc" | "sp" => "transport",
        "n" => "error model",
        "exp" => "information",
        _ if pw_is_student_t_token(tok) => "error model",
        _ => "",
    }
}

/// The reader-facing name of a predictive-power tool slug in a 4.2 group header:
/// the `rust` channel IS the product, so it reads `empyrean`; every other tool
/// keeps its own name. One place, so every 4.2 group header agrees.
fn pw_tool_label(tool: &str) -> &str {
    if tool == "rust" { "empyrean" } else { tool }
}

/// One factor level rendered as its reader word (for a 4.2 group-header chip).
fn pw_factor_word(factor: usize, tok: &str) -> &'static str {
    match (factor, tok) {
        (0, "efcc") => "EFCC",
        (0, "nodeb") => "no debias",
        (1, "adap") => "adaptive",
        (1, "cmc") => "CMC",
        (1, "norej") => "no rejection",
        (1, "blun") => "blunder",
        (2, "vfc") => "nightly",
        (2, "nonight") => "per-obs",
        (3, "mc") => "Monte Carlo",
        (3, "sp") => "sigma-point",
        (3, "linear") => "linear",
        _ => "—",
    }
}

/// A configuration arm rendered as a 4.2 group-header chip: the four factor
/// levels in plain words (e.g. `EFCC · adaptive · nightly · linear`), with the
/// raw slug on hover only. An arm that carries no empyrean factor structure (an
/// external tool's `default+mc`) renders its own tokens prettified instead of a
/// row of em dashes, so no information is lost.
fn pw_arm_chip(arm: &str) -> String {
    let f = pw_arm_factors(arm);
    let words = if f[0] == "—" && f[1] == "—" && f[2] == "—" {
        let parts: Vec<&str> = pw_arm_tokens(arm)
            .map(|t| match t {
                "mc" => "Monte Carlo",
                "sp" => "sigma-point",
                other => other,
            })
            .collect();
        parts.join(" · ")
    } else {
        [
            pw_factor_word(0, f[0]),
            pw_factor_word(1, f[1]),
            pw_factor_word(2, f[2]),
            pw_factor_word(3, f[3]),
        ]
        .join(" · ")
    };
    format!(
        "<span class=\"pwarm\" title=\"arm {}\">{}</span>",
        attr_escape(arm),
        attr_escape(&words)
    )
}

/// A configuration factor a walk run can sweep, in page display order. The
/// error-model and information factors are the 2026-10-18 additions (owner:
/// "add error model"). One definition so the run block's factor × level table,
/// arm list and ranking all name and order factors identically, and every
/// level renders as a reader word (never a raw arm token).
#[derive(Clone, Copy, PartialEq, Eq)]
enum PwFactor {
    Debias,
    Rejection,
    Nightly,
    ErrorModel,
    Information,
    Transport,
}

impl PwFactor {
    /// Every factor in page display order.
    const ALL: [PwFactor; 6] = [
        PwFactor::Debias,
        PwFactor::Rejection,
        PwFactor::Nightly,
        PwFactor::ErrorModel,
        PwFactor::Information,
        PwFactor::Transport,
    ];

    /// The factor's reader name for a header or factor column.
    fn name(self) -> &'static str {
        match self {
            PwFactor::Debias => "debias",
            PwFactor::Rejection => "rejection",
            PwFactor::Nightly => "nightly",
            PwFactor::ErrorModel => "error model",
            PwFactor::Information => "information",
            PwFactor::Transport => "transport",
        }
    }

    /// The reader word for the level an arm carries under this factor, or `None`
    /// when the factor does not apply to the arm (then it is not a level). Raw
    /// tokens never leave this function.
    fn level(self, arm: &str) -> Option<String> {
        let f = pw_arm_factors(arm);
        let word = |idx: usize, tok: &str| {
            let w = pw_factor_word(idx, tok);
            (w != "—").then(|| w.to_string())
        };
        match self {
            PwFactor::Debias => word(0, f[0]),
            PwFactor::Rejection => word(1, f[1]),
            PwFactor::Nightly => word(2, f[2]),
            // Transport always applies — an arm with neither `mc` nor `sp` is
            // linear, the recorded default.
            PwFactor::Transport => Some(pw_factor_word(3, f[3]).to_string()),
            PwFactor::ErrorModel => pw_error_model_word(arm),
            PwFactor::Information => pw_information_word(arm).map(str::to_string),
        }
    }
}

/// The factors that VARY across an arm set — those with ≥ 2 distinct applicable
/// levels — in display order. A factor with one level (or none that apply) is
/// not swept and is not a column (today's rule, owner 2026-10-17).
fn pw_varying_factors(arms: &[&str]) -> Vec<PwFactor> {
    PwFactor::ALL
        .into_iter()
        .filter(|f| {
            let levels: BTreeSet<String> = arms.iter().filter_map(|a| f.level(a)).collect();
            levels.len() >= 2
        })
        .collect()
}

/// An arm rendered as the reader words of the given factors, joined by " · " —
/// the arm's identity with no raw token on the page (neither visible nor on
/// hover). Used by the run block's arm list and log-score ranking.
fn pw_arm_words(arm: &str, factors: &[PwFactor]) -> String {
    factors
        .iter()
        .filter_map(|f| f.level(arm))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The in-table calibration ruler for a 4.2C realism column: states the neutral
/// point of the diverging realism scale. Wrapped in a no-transform span so the
/// header-uppercase rule leaves it alone (and H7 skips it).
const RV_RULER: &str = "<div class=\"rvnote\"><span class=\"r1sub\">1 = calibrated</span></div>";

/// Every configuration arm present in the aggregate's surface cells (across all
/// tools), stable order. One definition so the surfaces' arm count, the arm grid
/// and the calibration coverage span the same set and no arm is silently dropped;
/// an arm no rust walk produced renders as an explicit hatched row.
fn pw_all_arms(agg: &PredictAggregates) -> Vec<&str> {
    let mut v: BTreeSet<&str> = BTreeSet::new();
    for c in &agg.cells {
        v.insert(c.config_arm.as_str());
    }
    v.into_iter().collect()
}

/// The most-populated `(tool, arm)` pair over `items`, rust preferred. Iteration
/// is over a sorted map, so ties resolve to the first (alphabetically-smallest)
/// arm, keeping the choice byte-stable.
fn pw_best_series<'a>(
    items: impl Iterator<Item = (&'a str, &'a str)>,
) -> Option<(&'a str, &'a str)> {
    let mut counts: BTreeMap<(&'a str, &'a str), usize> = BTreeMap::new();
    for k in items {
        *counts.entry(k).or_default() += 1;
    }
    // Track the winning pair and its (rust-first, count) score separately rather
    // than as one nested tuple (clippy's type-complexity cap).
    let mut best_pair: Option<(&'a str, &'a str)> = None;
    let mut best_score = (false, 0usize);
    for (k, n) in counts {
        let score = (k.0 == "rust", n);
        if best_pair.is_none() || score > best_score {
            best_pair = Some(k);
            best_score = score;
        }
    }
    best_pair
}

/// The configuration the distribution ships by default: EFCC catalogue
/// debias, adaptive rejection, VFC2017 nightly weighting, linear transport.
const PRODUCTION_ARM: (&str, &str) = ("rust", "efcc.adap.vfc");

/// The `(tool, arm)` series the single-arm 4.2 views key on — 4.2A's default
/// surface, 4.2B's horizon marginals, 4.2D's per-object walk and 4.2F's timeline.
/// It is the pair that actually co-occurs in the data and carries the most
/// per-object statistics, rust preferred, falling back to raw per-object presence
/// then to the cell arms. One definition so no panel independently selects a
/// `(tool, arm)` that never ran together — the rust `default` arm, which no rust
/// walk produced, is what emptied the default surface, the marginals and the
/// per-object matrix.
fn pw_primary_series(agg: &PredictAggregates) -> (&str, &str) {
    // The shipped configuration is featured whenever the run carries it; the
    // most-populated pair is only the fallback, so a control arm never
    // headlines by a tie-break.
    if agg.per_object.iter().any(|p| {
        p.tool == PRODUCTION_ARM.0 && p.config_arm == PRODUCTION_ARM.1 && p.med_d2_norm.is_some()
    }) {
        return PRODUCTION_ARM;
    }
    pw_best_series(
        agg.per_object
            .iter()
            .filter(|p| p.med_d2_norm.is_some())
            .map(|p| (p.tool.as_str(), p.config_arm.as_str())),
    )
    .or_else(|| {
        pw_best_series(
            agg.per_object
                .iter()
                .map(|p| (p.tool.as_str(), p.config_arm.as_str())),
        )
    })
    .or_else(|| {
        pw_best_series(
            agg.cells
                .iter()
                .map(|c| (c.tool.as_str(), c.config_arm.as_str())),
        )
    })
    .unwrap_or(("", ""))
}

/// Compact day-bin column label: `0.3–1 d`, `1–3`, `100–400`, …
fn pw_day_label(lo: f64, hi: f64) -> String {
    let f = |v: f64| {
        if v >= 1.0 {
            format!("{v:.0}")
        } else {
            format!("{v}")
        }
    };
    format!("{}–{}", f(lo), f(hi))
}

/// One accuracy surface cell: median separation on the SepArcsec decade ladder,
/// hatched (reason + n on hover) when thin, missing, or without a value. `c` is
/// the cell for this (arc × dt) corner, or `None` when no window reached it.
fn pw_accuracy_cell(c: Option<&SurfaceCell>) -> String {
    match c {
        None => format!("<td class=\"gc hatch\" title=\"{HATCH_NO_WINDOW}\"></td>"),
        Some(c) if c.n_objects < 3 => format!(
            "<td class=\"gc hatch\" title=\"n={} · {}\"></td>",
            c.n_objects, HATCH_THIN
        ),
        Some(c) => match c.med_sep_arcsec {
            Some(v) if v.is_finite() => {
                let bin = GridAxis::SepArcsec.bin(v);
                let rw = if bin == 7 { " rw" } else { "" };
                let hover = attr_escape(&format!(
                    "{} · n {} · {} windows · delivered {:.0}%",
                    grid_fmt(GridAxis::SepArcsec, v),
                    c.n_objects,
                    c.n_windows,
                    c.delivered_fraction * 100.0
                ));
                format!(
                    "<td class=\"gc gb{bin}{rw}\" title=\"{hover}\"><span class=\"cn\">{}</span></td>",
                    grid_compact(GridAxis::SepArcsec, v)
                )
            }
            _ => format!(
                "<td class=\"gc hatch\" title=\"n={} · no held-out separation in this cell\"></td>",
                c.n_objects
            ),
        },
    }
}

/// One realism surface cell: normalized realism on the diverging log₂ scale
/// (D-004), hatched (reason + n on hover) when thin, missing, or without a value.
fn pw_realism_cell(c: Option<&SurfaceCell>) -> String {
    match c {
        None => format!("<td class=\"gc hatch\" title=\"{HATCH_NO_WINDOW}\"></td>"),
        Some(c) if c.n_objects < 3 => format!(
            "<td class=\"gc hatch\" title=\"n={} · {}\"></td>",
            c.n_objects, HATCH_THIN
        ),
        Some(c) => match c.med_d2_norm {
            Some(v) if v.is_finite() && v > 0.0 => {
                let (cls, dir) = realism_bin(v);
                let dir_txt =
                    |r: Option<f64>| r.map(|x| format!("{x:.2}")).unwrap_or_else(|| "—".into());
                // 4.2B realism is the trust-GATED median, so its Σ_pred share
                // reads the gated pair (4.2A, trust-off, reads the unc pair).
                let share = if c.n_rows > 0 {
                    format!(
                        "{:.0}%",
                        f64::from(c.n_rows_pd) / f64::from(c.n_rows) * 100.0
                    )
                } else {
                    "—".into()
                };
                let hover = attr_escape(&format!(
                    "realism {v:.2} ({dir}) · along {} · cross {} · n {} · {} windows · σ_pred-dominated {share} of rows",
                    dir_txt(c.med_z2_at_norm),
                    dir_txt(c.med_z2_ct_norm),
                    c.n_objects,
                    c.n_windows
                ));
                format!(
                    "<td class=\"gc {cls}\" title=\"{hover}\"><span class=\"cn\">{v:.2}</span></td>"
                )
            }
            _ => format!(
                "<td class=\"gc hatch\" title=\"n={} · no σ-dominated d² in this cell\"></td>",
                c.n_objects
            ),
        },
    }
}

/// Surface cells of one (class, tool, arm) group, keyed within the group by
/// the `(arc_lo bits, dt_lo bits)` corner of the (arc × dt) plane.
type PwCellMap<'a> = HashMap<(u64, u64), &'a SurfaceCell>;
/// Surface cells grouped by (class, tool, arm), each group a [`PwCellMap`].
type PwGroups<'a> = BTreeMap<(&'a str, &'a str, &'a str), PwCellMap<'a>>;

/// Render one (arc × dt) grid for a (class, tool, arm) group — accuracy
/// (`realism == false`) or realism (`realism == true`). Rows are arc bins
/// descending (long arcs on top), columns dt bins ascending; every corner
/// carries a number or a mark.
fn pw_surface_grid(
    cells: &PwCellMap,
    arc_bins: &[(f64, f64)],
    dt_bins: &[(f64, f64)],
    realism: bool,
) -> String {
    // The accuracy / realism label rides in the corner header cell, which is
    // table content (exempt from the visible-word budget) rather than a caption.
    let label = if realism { "realism" } else { "accuracy" };
    let mut h = String::from(
        "<div class=\"grid-scroll\"><table class=\"agrid pw-grid\" data-grid=\"predict\"><thead><tr><th class=\"gobj\">",
    );
    h.push_str(&uhdr(&format!("{label} · arc ╲ dt")));
    h.push_str("</th>");
    for &(lo, hi) in dt_bins {
        h.push_str(&format!(
            "<th class=\"dt\" title=\"horizon {lo}–{hi} d\">{}</th>",
            pw_day_label(lo, hi)
        ));
    }
    h.push_str("</tr></thead><tbody>");
    for &(alo, ahi) in arc_bins.iter().rev() {
        h.push_str(&format!(
            "<tr><td class=\"gtool\" title=\"fit arc {alo}–{ahi} d\">{}</td>",
            pw_day_label(alo, ahi)
        ));
        for &(dlo, _dhi) in dt_bins {
            let c = cells.get(&(alo.to_bits(), dlo.to_bits())).copied();
            h.push_str(&if realism {
                pw_realism_cell(c)
            } else {
                pw_accuracy_cell(c)
            });
        }
        h.push_str("</tr>");
    }
    h.push_str("</tbody></table></div>");
    h
}

/// The one diverging bin for a coverage cell: observed minus expected in units
/// of the binomial standard error \\( \sqrt{p(1-p)/n} \\) on the D-004 scale.
/// Fewer inside than expected (observed < expected) is over-confident (red);
/// more is over-conservative (blue); within one standard error is calibrated.
/// `n` is the independent-unit count (objects). One decision, one definition.
fn coverage_bin(obs: f64, exp: f64, n: u32) -> (&'static str, &'static str) {
    if n == 0 || !(exp > 0.0 && exp < 1.0) || !obs.is_finite() {
        return ("rv0", "calibrated");
    }
    let se = (exp * (1.0 - exp) / n as f64).sqrt();
    if se <= 0.0 || se.is_nan() {
        return ("rv0", "calibrated");
    }
    let z = (obs - exp) / se;
    let a = z.abs();
    if a < 1.0 {
        ("rv0", "calibrated")
    } else if z < 0.0 {
        // Fewer inside the ellipse than expected — the covariance is too tight.
        if a < 2.0 {
            ("rvr1", "over-confident")
        } else if a < 4.0 {
            ("rvr2", "over-confident")
        } else {
            ("rvr3", "over-confident")
        }
    } else if a < 2.0 {
        ("rvb1", "over-conservative")
    } else if a < 4.0 {
        ("rvb2", "over-conservative")
    } else {
        ("rvb3", "over-conservative")
    }
}

/// The three coverage directions of Part 4.2A — the joint 2-dof ellipse and the
/// two 1-dof sky directions through the shared position angle. Each resolves a
/// surface cell's observed and law-aware expected inside-ellipse fractions at
/// 1σ/2σ/3σ, so one matrix renderer serves all three.
#[derive(Clone, Copy)]
enum CovDir {
    Joint,
    Along,
    Cross,
}

impl CovDir {
    /// Trust-UNCONDITIONED inside-ellipse fractions at 1σ/2σ/3σ — the matrix
    /// cell value (2σ) and the 1σ/3σ shown on hover. Every converged window's
    /// published covariance counts, so a tool cannot flatter its rate by
    /// self-flagging the covariances it least trusts.
    fn obs_unc(self, c: &SurfaceCell) -> [Option<f64>; 3] {
        match self {
            CovDir::Joint => [c.cov_1s_unc, c.cov_2s_unc, c.cov_3s_unc],
            CovDir::Along => [c.cov_1s_at_unc, c.cov_2s_at_unc, c.cov_3s_at_unc],
            CovDir::Cross => [c.cov_1s_ct_unc, c.cov_2s_ct_unc, c.cov_3s_ct_unc],
        }
    }
    /// Trust-GATED 2σ fraction — the hover's self-flagged figure, the one the
    /// tool itself would report.
    fn cond_2s(self, c: &SurfaceCell) -> Option<f64> {
        match self {
            CovDir::Joint => c.cov_2s,
            CovDir::Along => c.cov_2s_at,
            CovDir::Cross => c.cov_2s_ct,
        }
    }
    /// Per-row (not night-joint) 2σ fraction on the same unconditioned subset.
    /// The night-joint divides a night's measurement noise by its target count;
    /// the per-row figure does not, so it does not inherit the under-coverage
    /// of correlated per-night noise.
    fn row_2s(self, c: &SurfaceCell) -> Option<f64> {
        match self {
            CovDir::Joint => c.cov_2s_row_unc,
            CovDir::Along => c.cov_2s_at_row_unc,
            CovDir::Cross => c.cov_2s_ct_row_unc,
        }
    }
    /// Law-aware expected inside-ellipse fractions at 1σ/2σ/3σ.
    fn exp(self, c: &SurfaceCell) -> [Option<f64>; 3] {
        match self {
            CovDir::Joint => [c.exp_cov_1s, c.exp_cov_2s, c.exp_cov_3s],
            CovDir::Along => [c.exp_cov_1s_at, c.exp_cov_2s_at, c.exp_cov_3s_at],
            CovDir::Cross => [c.exp_cov_1s_ct, c.exp_cov_2s_ct, c.exp_cov_3s_ct],
        }
    }
    /// The matrix's reading-line direction label (rides in the corner header).
    fn label(self) -> &'static str {
        match self {
            CovDir::Joint => "joint · 2 dof",
            CovDir::Along => "along-track · 1 dof",
            CovDir::Cross => "cross-track · 1 dof",
        }
    }
}

/// The arm the coverage matrices key on for `tool`: rust resolves to the shared
/// [`pw_primary_series`] arm; an external tool resolves to the arm carrying the
/// most coverage (its Monte-Carlo sky covariance), since find_orb's analytic
/// `default` arm delivers no 2×2 and so no inside-ellipse rate — only its
/// `+mc` arm does. Falls back to the most-populated arm when no arm has coverage,
/// so the tool still renders (hatched). Ties break on the smaller arm name.
fn pw_coverage_arm_for<'a>(agg: &'a PredictAggregates, tool: &str) -> Option<&'a str> {
    if tool == pw_primary_series(agg).0 {
        return Some(pw_primary_series(agg).1);
    }
    let mut by_arm: BTreeMap<&str, u64> = BTreeMap::new();
    for c in agg.cells.iter().filter(|c| c.tool == tool) {
        if c.cov_2s.is_some() {
            *by_arm.entry(c.config_arm.as_str()).or_default() += u64::from(c.n_nights);
        }
    }
    // Most coverage nights wins; iterate a sorted map so ties take the smaller
    // arm name deterministically.
    let best = by_arm
        .into_iter()
        .fold(None::<(&str, u64)>, |acc, (a, n)| match acc {
            Some((_, bn)) if bn >= n => acc,
            _ => Some((a, n)),
        });
    best.map(|(a, _)| a).or_else(|| {
        pw_best_series(
            agg.cells
                .iter()
                .filter(|c| c.tool == tool)
                .map(|c| (c.tool.as_str(), c.config_arm.as_str())),
        )
        .map(|(_, a)| a)
    })
}

/// A pooled coverage row is short-arc when its upper edge is the split value.
fn cov_arc_short(c: &SurfaceCell) -> bool {
    c.arc_hi_days.to_bits() == COVERAGE_ARC_SPLIT_DAYS.to_bits()
}

/// One coverage matrix (a [`CovDir`]): rows = tool (at its primary arm) grouped
/// over object class, two rows per class — short-arc and long-arc about the
/// [`COVERAGE_ARC_SPLIT_DAYS`] split — and columns = the dt bins. Each cell is
/// the trust-UNCONDITIONED 2σ inside-ellipse rate, pooled equal-weight-per-object
/// over all arc bins of the row (never a median of arc cells), on the diverging
/// observed-minus-expected scale; the trust-gated and per-row figures, 1σ/3σ,
/// expected, the object counts (all / trusted) and the Σ_pred-dominated share
/// ride the hover. A tool with no coverage is one hatched group; a cell with
/// fewer than three objects, or a Σ_pred share below the floor, is hatched.
fn pw_coverage_matrix(agg: &PredictAggregates, dir: CovDir, dt_bins: &[(f64, f64)]) -> String {
    // Classes and tools from the surface cells, so the row groups reconcile with
    // the 4.2B tool selector; a tool with no pooled coverage renders its group
    // all-hatched (never a dropped tool).
    let mut classes: Vec<&str> = {
        let mut v: BTreeSet<&str> = BTreeSet::new();
        for c in &agg.cells {
            v.insert(c.class.as_str());
        }
        v.into_iter().collect()
    };
    // The one canonical class order (class_rank, driven by POPULATION_ORDER).
    classes.sort_by_key(|c| class_rank(c));
    // Tools in a fixed order; the reference-PA tool (rust) first, then externals.
    let mut tools: Vec<&str> = {
        let mut v: BTreeSet<&str> = BTreeSet::new();
        for c in &agg.cells {
            v.insert(c.tool.as_str());
        }
        v.into_iter().collect()
    };
    tools.sort_by_key(|t| (*t != "rust", *t));
    // (tool, arm, class, arc_short, dt_lo bits) → the pooled coverage row.
    let by_key: HashMap<(&str, &str, &str, bool, u64), &SurfaceCell> = agg
        .coverage_cells
        .iter()
        .map(|c| {
            (
                (
                    c.tool.as_str(),
                    c.config_arm.as_str(),
                    c.class.as_str(),
                    cov_arc_short(c),
                    c.dt_lo_days.to_bits(),
                ),
                c,
            )
        })
        .collect();
    let split = format!("{:.0}", COVERAGE_ARC_SPLIT_DAYS);

    // De-crowding (owner, 2026-10-17): per tool, every (class, arc-side) row with
    // no numeric cell in this direction — whether no tool reached it anywhere or
    // only this tool missed it — is folded into one "no predictive windows" line
    // per tool, the folded class labels and the distinct hatch reasons on that
    // line's chip hover. Marked and counted, never dropped; the screens of empty
    // hatch are reclaimed. The fold decision is made per row below (has_numeric).
    let mut h = String::from(
        "<div class=\"grid-scroll\"><table class=\"agrid pw-grid\" data-grid=\"predict\"><thead><tr><th class=\"gobj\">",
    );
    // S4 text-to-marks: the corner header keeps only the quantity name — the
    // direction and its dof. The scope (trust-off observed 2σ inside-ellipse
    // percent on Σ_pred-dominated rows, the tool ╲ dt layout, the day unit) is the
    // one load-bearing caveat; it moved to the always-visible scope key line under
    // the Coverage panel title (pw_coverage_html), not this corner or the hover.
    h.push_str(&uhdr(dir.label()));
    h.push_str("</th>");
    for &(lo, hi) in dt_bins {
        h.push_str(&format!(
            "<th class=\"dt\" title=\"horizon {lo}–{hi} d · observed inside 2σ, percent\">{}</th>",
            pw_day_label(lo, hi)
        ));
    }
    h.push_str("</tr></thead><tbody>");

    let pct = |v: Option<f64>| {
        v.map(|x| format!("{:.0}%", x * 100.0))
            .unwrap_or_else(|| "—".into())
    };
    let pct1 = |v: Option<f64>| {
        v.map(|x| format!("{:.1}%", x * 100.0))
            .unwrap_or_else(|| "—".into())
    };

    for tool in &tools {
        let arm = pw_coverage_arm_for(agg, tool);
        let Some(arm) = arm else {
            h.push_str(&format!(
                "<tr class=\"gpophdr\"><td class=\"gpopname\" colspan=\"{}\" title=\"no walk ran for {}\">{} — {HATCH_NOT_IN_RUN}</td></tr>",
                dt_bins.len() + 1,
                attr_escape(tool),
                attr_escape(pw_tool_label(tool)),
            ));
            continue;
        };
        // Tool group header naming the tool AND the covariance arm it is scored on
        // (its Monte-Carlo sky covariance for an external tool). The arm is the
        // provenance of the whole row group, so it rides visibly beside the tool.
        h.push_str(&format!(
            "<tr class=\"gpophdr\"><td class=\"gpopname\" colspan=\"{}\" title=\"covariance arm {}\">{} {}</td></tr>",
            dt_bins.len() + 1,
            attr_escape(arm),
            attr_escape(pw_tool_label(tool)),
            pw_arm_chip(arm),
        ));
        // Rows with no numeric cell for this (tool, direction) fold into one
        // "no predictive windows" line per tool; a row with at least one numeric
        // cell is shown in full. The folded class labels and the distinct hatch
        // reasons ride the fold line's chip hover — marked and counted, never
        // dropped.
        let mut folded_labels: Vec<String> = Vec::new();
        let mut folded_reasons: BTreeSet<&'static str> = BTreeSet::new();
        for cl in &classes {
            // Two rows per class: short-arc then long-arc, about the split. The
            // split keeps rust's short-arc over-confidence from pooling with its
            // over-conservative long-arc fits (and the same for every tool).
            for arc_short in [true, false] {
                let label = if arc_short {
                    format!("{cl} · short-arc <{split} d")
                } else {
                    format!("{cl} · long-arc \u{2265}{split} d")
                };
                let mut cells = String::new();
                let mut has_numeric = false;
                let mut row_reasons: BTreeSet<&'static str> = BTreeSet::new();
                for &(dlo, _dhi) in dt_bins {
                    let Some(c) = by_key
                        .get(&(tool, arm, cl, arc_short, dlo.to_bits()))
                        .copied()
                    else {
                        cells.push_str(&format!(
                            "<td class=\"gc hatch\" title=\"n=0 · {HATCH_NO_WINDOW}\"></td>"
                        ));
                        row_reasons.insert(HATCH_NO_WINDOW);
                        continue;
                    };
                    let n_all = c.n_objects_all;
                    // The Σ_pred-dominated share and its <10% hatch gate read the
                    // UNCONDITIONED row population — the same rows the cell value
                    // (cov_*_unc) is computed over — so the number shown and the
                    // decision to show it describe one population, never the
                    // trust-gated subset (4.2B realism reads the gated pair).
                    let share = if c.n_rows_unc > 0 {
                        f64::from(c.n_rows_pd_unc) / f64::from(c.n_rows_unc)
                    } else {
                        0.0
                    };
                    if n_all < 3 {
                        cells.push_str(&format!(
                            "<td class=\"gc hatch\" title=\"n={n_all} · {HATCH_THIN}\"></td>"
                        ));
                        row_reasons.insert(HATCH_THIN);
                        continue;
                    }
                    if share < COVERAGE_MIN_PD_SHARE {
                        cells.push_str(&format!(
                            "<td class=\"gc hatch\" title=\"n={n_all} · σ_pred-dominated {:.0}% of rows · {HATCH_SHARE}\"></td>",
                            share * 100.0
                        ));
                        row_reasons.insert(HATCH_SHARE);
                        continue;
                    }
                    let obs = dir.obs_unc(c);
                    match (obs[1], dir.exp(c)[1]) {
                        (Some(o2), Some(e2)) => {
                            let (ccls, dir_word) = coverage_bin(o2, e2, n_all);
                            let exp = dir.exp(c);
                            let hover = attr_escape(&format!(
                                "trust off: inside 2σ {} vs expected {} ({dir_word}) · trust-gated {} · per-row {} · 1σ {} 3σ {} · expected 1σ {} 3σ {} · n {n_all} objects ({} trusted) · {} nights · computed on the {:.0}% of rows where σ_pred dominates",
                                pct(obs[1]),
                                pct1(exp[1]),
                                pct(dir.cond_2s(c)),
                                pct(dir.row_2s(c)),
                                pct(obs[0]),
                                pct(obs[2]),
                                pct1(exp[0]),
                                pct1(exp[2]),
                                c.n_objects_trusted,
                                c.n_nights,
                                share * 100.0,
                            ));
                            cells.push_str(&format!(
                                "<td class=\"gc {ccls}\" title=\"{hover}\"><span class=\"cn\">{}</span></td>",
                                pct(Some(o2))
                            ));
                            has_numeric = true;
                        }
                        _ => {
                            cells.push_str(&format!(
                                "<td class=\"gc hatch\" title=\"n={n_all} · {HATCH_NO_RATE}\"></td>"
                            ));
                            row_reasons.insert(HATCH_NO_RATE);
                        }
                    }
                }
                if has_numeric {
                    h.push_str(&format!(
                        "<tr><td class=\"gtool\">{}</td>{cells}</tr>",
                        attr_escape(&label)
                    ));
                } else {
                    folded_labels.push(label);
                    folded_reasons.extend(row_reasons);
                }
            }
        }
        // One consolidated "no predictive windows" line per tool, the folded
        // class × arc labels and the distinct hatch reasons on its chip hover.
        if !folded_labels.is_empty() {
            let reasons: Vec<&str> = folded_reasons.iter().copied().collect();
            let hover = attr_escape(&format!(
                "no numeric coverage for {}: {} · reasons: {}",
                tool,
                folded_labels.join(", "),
                reasons.join("; "),
            ));
            h.push_str(&format!(
                "<tr class=\"pwnowin\"><td class=\"gtool\" title=\"{hover}\">no predictive windows</td>"
            ));
            for _ in dt_bins {
                h.push_str(&format!(
                    "<td class=\"gc hatch\" title=\"{HATCH_NO_WINDOW}\"></td>"
                ));
            }
            h.push_str("</tr>");
        }
    }
    h.push_str("</tbody></table></div>");
    h
}

/// The one coverage key for Part 4.2A: the diverging swatches (over-confident
/// red / calibrated / over-conservative blue) and the expected 1σ/2σ/3σ
/// inside-ellipse percentages, for the joint and the 1-dof directions, read
/// from the aggregate. Kept to ≤ 8 visible words; the joint-vs-1-dof reading of
/// the two number triples rides in the container hover.
fn pw_coverage_key(agg: &PredictAggregates) -> String {
    let triple = |dir: CovDir| -> String {
        agg.cells
            .iter()
            .find_map(|c| {
                let e = dir.exp(c);
                e.iter().all(|x| x.is_some()).then(|| {
                    format!(
                        "{:.0}/{:.0}/{:.1}",
                        e[0].unwrap() * 100.0,
                        e[1].unwrap() * 100.0,
                        e[2].unwrap() * 100.0
                    )
                })
            })
            .unwrap_or_else(|| "—".into())
    };
    format!(
        "<div class=\"grid-key\" title=\"The one diverging scale: observed minus expected inside-ellipse rate in binomial standard errors. Fewer inside than expected is over-confident (red), more over-conservative (blue). The cell is the trust-unconditioned 2σ rate, pooled equal-weight-per-object over the row's arc bins; rows split at a 30 d fit arc. A cell with a Σ_pred-dominated share below 10% is hatched. The triples are expected 1σ/2σ/3σ percentages.\"><span class=\"gk\"><span class=\"gk-sw rvr3\"></span>over-confident</span><span class=\"gk\"><span class=\"gk-sw rv0\"></span>calibrated</span><span class=\"gk\"><span class=\"gk-sw rvb3\"></span>over-conservative</span><span class=\"gk\" title=\"expected joint 1σ/2σ/3σ · the cells show the 2σ value\">joint {}</span><span class=\"gk\" title=\"expected 1-dof along/cross 1σ/2σ/3σ · the cells show the 2σ value\">axis {}</span></div>",
        triple(CovDir::Joint),
        triple(CovDir::Along),
    )
}

/// 4.2A — Coverage: the headline product. One matrix per direction (joint 2-dof,
/// along-track, cross-track); each cell the observed 2σ inside-ellipse rate per
/// tool × class × horizon, coloured by observed minus expected on the one
/// diverging scale. This is the predictive-power answer: how often the next
/// observations land inside the state ellipse, and whether that rate is honest.
fn pw_coverage_html(agg: &PredictAggregates) -> String {
    let dt_bins = pw_axis(agg.cells.iter().map(|c| (c.dt_lo_days, c.dt_hi_days)));
    let mut h = String::new();
    h.push_str("<div class=\"panel-title\">Coverage</div>");
    // S4: the matrix scope (what each cell is, and the row/column layout) moved off
    // the per-matrix corner header into one always-visible units key under the
    // summary — units and symbols, not a sentence.
    h.push_str("<div class=\"grid-summary\">trust-off · σ_pred-only 2σ% · tool ╲ dt · days</div>");
    // The diverging key and the expected anchors lead, directly under the title and
    // above the first matrix, so a reader meets the red/blue scale and the expected
    // percentages before the grids they colour, never after.
    h.push_str(&pw_coverage_key(agg));
    // Direction toggle (owner, 2026-10-17): one matrix visible at a time, joint
    // the default, instead of three stacked. The <select> is skip-counted, so the
    // toggle costs no visible words; all three matrices stay in the DOM (two
    // hidden), so every direction is one click away.
    h.push_str("<div class=\"pw-select\"><label class=\"pw-sel-label\"><select id=\"pw-cov-dir\" aria-label=\"coverage direction\" title=\"coverage direction\"><option value=\"joint\" selected>joint</option><option value=\"along\">along-track</option><option value=\"cross\">cross-track</option></select></label></div>");
    for (dir, key, shown) in [
        (CovDir::Joint, "joint", true),
        (CovDir::Along, "along", false),
        (CovDir::Cross, "cross", false),
    ] {
        h.push_str(&format!(
            "<div class=\"pw-cov-mat\" data-dir=\"{key}\"{}>",
            if shown { "" } else { " hidden" }
        ));
        h.push_str(&pw_coverage_matrix(agg, dir, &dt_bins));
        h.push_str("</div>");
    }
    h.push_str("<div class=\"disclosures\"><details><summary>");
    h.push_str(DISCLOSURE_SUMMARY);
    h.push_str("</summary><div class=\"disc-body\">Coverage is how often the next night's observation lands inside the predicted ellipse. Only Σ_pred-dominated nights count, where the prediction term exceeds the measurement term, so the rate tests the tool's covariance rather than the pinned σ table; a night counts once, its orbit error being common-mode. The cell is unconditioned on the engine's trust flag, so no tool flatters its rate by dropping the covariances it least trusts; the trust-gated and per-row rates ride the hover. Rows split at a 30 d fit arc. The joint matrix uses the 2×2 d²; along and cross use the 1-dof z.</div></details></div>");
    h
}

/// 4.2B — the accuracy + realism surface pairs, one per (class, tool, arm) group
/// present in the data, with a Class · Tool · Arm selector (a sibling of the
/// Part-1 pair-panel show/hide). The reference arm for the common support is
/// fixed at `default`. `profile` is the server-derived provenance label baked
/// into the one reading line, so the page states which arms/profile the data
/// came from without trusting the fetch.
fn pw_surfaces_html(agg: &PredictAggregates, profile: &str) -> String {
    let arc_bins = pw_axis(agg.cells.iter().map(|c| (c.arc_lo_days, c.arc_hi_days)));
    let dt_bins = pw_axis(agg.cells.iter().map(|c| (c.dt_lo_days, c.dt_hi_days)));

    // Distinct selector dimensions, stable order.
    let mut classes: Vec<&str> = {
        let mut v: BTreeSet<&str> = BTreeSet::new();
        for c in &agg.cells {
            v.insert(c.class.as_str());
        }
        v.into_iter().collect()
    };
    // The one canonical class order (class_rank, driven by POPULATION_ORDER).
    classes.sort_by_key(|c| class_rank(c));
    let tools: Vec<&str> = {
        let mut v: BTreeSet<&str> = BTreeSet::new();
        for c in &agg.cells {
            v.insert(c.tool.as_str());
        }
        v.into_iter().collect()
    };
    let arms: Vec<&str> = {
        let mut v: BTreeSet<&str> = BTreeSet::new();
        for c in &agg.cells {
            v.insert(c.config_arm.as_str());
        }
        v.into_iter().collect()
    };

    // Group the cells.
    let mut groups: PwGroups = BTreeMap::new();
    for c in &agg.cells {
        groups
            .entry((c.class.as_str(), c.tool.as_str(), c.config_arm.as_str()))
            .or_default()
            .insert((c.arc_lo_days.to_bits(), c.dt_lo_days.to_bits()), c);
    }

    // Default selection: the primary (tool, arm) series — the pair that actually
    // co-occurs in the data — and the class with the most populated cells for
    // it, so the headline panel shows real surfaces on load and never the "not in
    // this run" fallback (the rust `default` arm has no rust walk, so defaulting to
    // it emptied the headline). The selector still lists every class/tool/arm, so a
    // reader can still choose an empty combination and see it marked absent.
    let (def_tool, def_arm) = pw_primary_series(agg);
    let populated = |cl: &str| {
        groups
            .get(&(cl, def_tool, def_arm))
            .map_or(0, |g| g.values().filter(|c| c.n_objects >= 3).count())
    };
    let def_class = classes
        .iter()
        .copied()
        .filter(|cl| populated(cl) > 0)
        .max_by_key(|cl| populated(cl))
        .or_else(|| classes.first().copied())
        .unwrap_or("");

    let mut h = String::new();
    h.push_str("<div class=\"panel-title\">Surfaces</div>");
    // Selector bar (a sibling of the Part-1 pair-panel selector).
    h.push_str("<div class=\"pw-select\">");
    for (id, label, opts, def) in [
        ("pw-class", "class", &classes, def_class),
        ("pw-tool", "tool", &tools, def_tool),
        ("pw-arm", "arm", &arms, def_arm),
    ] {
        // The label text rides in the select's accessible name and hover rather
        // than as a visible word (the page's visible-word budget keeps 4.2's prose
        // on hover); the option values name the dimension on sight.
        h.push_str(&format!(
            "<label class=\"pw-sel-label\"><select id=\"{id}\" aria-label=\"{label}\" title=\"{label}\">"
        ));
        for o in opts.iter() {
            let sel = if *o == def { " selected" } else { "" };
            h.push_str(&format!(
                "<option value=\"{v}\"{sel}>{v}</option>",
                v = attr_escape(o)
            ));
        }
        h.push_str("</select></label>");
    }
    h.push_str("</div>");

    // Keys: accuracy ladder (shared) + realism diverging (the one exception).
    h.push_str(&grid_key_html(GridAxis::SepArcsec));
    h.push_str(&realism_key_html());

    // The surfaces provenance (arms, classes, profile, snapshot) rides the
    // Method fold below (S4: the panel-title caption moved there), so no separate
    // reading line is spent here.

    // The panels, one per present (class, tool, arm) group; all hidden but the
    // default selection, toggled by the selector.
    h.push_str("<div class=\"pw-pairs\">");
    for ((cl, tool, arm), cells) in &groups {
        let shown = *cl == def_class && *tool == def_tool && *arm == def_arm;
        let hidden = if shown { "" } else { " hidden" };
        h.push_str(&format!(
            "<div class=\"pw-pair\" data-class=\"{}\" data-tool=\"{}\" data-arm=\"{}\"{hidden}>",
            attr_escape(cl),
            attr_escape(tool),
            attr_escape(arm)
        ));
        h.push_str("<div class=\"pw-grid-pair\">");
        h.push_str("<div class=\"pw-grid-one\">");
        h.push_str(&pw_surface_grid(cells, &arc_bins, &dt_bins, false));
        h.push_str("</div><div class=\"pw-grid-one\">");
        h.push_str(&pw_surface_grid(cells, &arc_bins, &dt_bins, true));
        h.push_str("</div></div></div>");
    }
    // The not-available line for a selected combination that carries no cells.
    h.push_str(&format!(
        "<div class=\"pw-pair-na\" hidden><span class=\"gk-sw hatch\" title=\"no walk ran for this class, tool and arm\"></span> {HATCH_NOT_IN_RUN}</div>"
    ));
    h.push_str("</div>");

    // Method.
    h.push_str("<div class=\"disclosures\"><details><summary>");
    h.push_str(DISCLOSURE_SUMMARY);
    h.push_str(&format!(
        "</summary><div class=\"disc-body\">Walk-forward accuracy (separation, decade ladder) and realism (normalized d², diverging log₂ scale), per (class, tool, arm) over the arc × dt plane; {na} arms, {nc} classes, {profile}, snapshot {snap}. Walk-forward: fit a growing per-night window, predict each held-out sky position with its 2×2, score d². Accuracy is the equal-weight-per-object median separation on the one decade ladder; realism is the equal-weight-per-object median(d²)/(2 ln 2) on the one diverging log₂ scale — 1 is calibrated, &lt;1 over-conservative (blue), &gt;1 over-confident (red). Support is triangular: a long horizon exists only from early arc cuts. The scoring σ is pinned and identical across arms, so a difference between arms is the fitted covariance, not the scoring table. A cell with fewer than three objects is hatched, never pooled.</div></details></div>",
        na = arms.len(),
        nc = classes.len(),
        profile = attr_escape(profile),
        snap = attr_escape(&agg.snapshot_id),
    ));
    h
}

/// 4.2C — class × dt-bin marginal of the surfaces (equal-weight-per-object median
/// separation and realism), for the rust `default` arm, every dt bin at once.
fn pw_marginals_html(agg: &PredictAggregates) -> String {
    let dt_bins = pw_axis(agg.cells.iter().map(|c| (c.dt_lo_days, c.dt_hi_days)));
    let mut classes: Vec<&str> = {
        let mut v: BTreeSet<&str> = BTreeSet::new();
        for c in &agg.cells {
            v.insert(c.class.as_str());
        }
        v.into_iter().collect()
    };
    // The one canonical class order (class_rank, driven by POPULATION_ORDER).
    classes.sort_by_key(|c| class_rank(c));
    // The single (tool, arm) this marginal is taken over: the primary series the
    // other single-arm views share, so it is never an empty (tool, arm) slice.
    let (tool, arm) = pw_primary_series(agg);

    let mut h = String::new();
    h.push_str("<div class=\"panel-title\">Horizon marginals</div>");
    // Metric toggle (owner, 2026-10-17): one number per cell instead of four
    // stacked, chosen here — accuracy on the decade ladder, realism joint (the
    // default) / along-track / cross-track on the diverging scale. The <select>
    // is skip-counted, so the toggle costs no visible words; the surfaces' two
    // keys above carry the ladder legend for the chosen metric.
    h.push_str("<div class=\"pw-select\"><label class=\"pw-sel-label\"><select id=\"pw-marg-metric\" aria-label=\"metric\" title=\"metric\"><option value=\"acc\">accuracy</option><option value=\"rj\" selected>realism joint</option><option value=\"at\">realism along-track</option><option value=\"ct\">realism cross-track</option></select></label></div>");
    h.push_str("<div class=\"grid-scroll\"><table class=\"agrid pw-grid\" data-grid=\"predict\"><thead><tr><th class=\"gobj\">class</th>");
    for &(lo, hi) in &dt_bins {
        // Plain-text title: an `uhdr` span (it carries a double-quote) interpolated
        // into a `title="…"` attribute closed the attribute early and leaked a
        // `">` into the visible header. The day unit and the cell layout ride in
        // the (now attribute-safe) hover.
        h.push_str(&format!(
            "<th class=\"dt\" title=\"horizon {lo}–{hi} d · sep then realism joint / along / cross\">{}</th>",
            pw_day_label(lo, hi)
        ));
    }
    h.push_str("</tr></thead><tbody>");
    for cl in &classes {
        h.push_str(&format!("<tr><td class=\"gtool\">{}</td>", attr_escape(cl)));
        for &(dlo, _dhi) in &dt_bins {
            let bin_cells: Vec<&SurfaceCell> = agg
                .cells
                .iter()
                .filter(|c| {
                    c.class == *cl
                        && c.tool == tool
                        && c.config_arm == arm
                        && c.dt_lo_days.to_bits() == dlo.to_bits()
                        && c.n_objects >= 3
                })
                .collect();
            if bin_cells.is_empty() {
                let n: u32 = agg
                    .cells
                    .iter()
                    .filter(|c| {
                        c.class == *cl
                            && c.tool == tool
                            && c.config_arm == arm
                            && c.dt_lo_days.to_bits() == dlo.to_bits()
                    })
                    .map(|c| c.n_objects)
                    .max()
                    .unwrap_or(0);
                h.push_str(&format!(
                    "<td class=\"gc hatch\" title=\"n={n} · {HATCH_THIN}\"></td>"
                ));
                continue;
            }
            let sep = pw_median(bin_cells.iter().filter_map(|c| c.med_sep_arcsec));
            let real = pw_median(bin_cells.iter().filter_map(|c| c.med_d2_norm));
            let n_obj: u32 = bin_cells.iter().map(|c| c.n_objects).max().unwrap_or(0);
            let sep_txt = sep
                .map(|v| grid_compact(GridAxis::SepArcsec, v))
                .unwrap_or_else(|| "—".into());
            let real_at = pw_median(bin_cells.iter().filter_map(|c| c.med_z2_at_norm));
            let real_ct = pw_median(bin_cells.iter().filter_map(|c| c.med_z2_ct_norm));
            let chip = |r: Option<f64>| match r {
                Some(v) => (realism_bin(v).0, format!("{v:.2}")),
                None => ("rv0", "—".to_string()),
            };
            let (real_cls, real_txt) = chip(real);
            let (at_cls, at_txt) = chip(real_at);
            let (ct_cls, ct_txt) = chip(real_ct);
            let bin = sep.map(|v| GridAxis::SepArcsec.bin(v)).unwrap_or(1);
            let hover = attr_escape(&format!(
                "sep {} · realism joint {} along {} cross {} · n {n_obj}",
                sep.map(|v| grid_fmt(GridAxis::SepArcsec, v))
                    .unwrap_or_else(|| "—".into()),
                real.map(|v| format!("{v:.3}"))
                    .unwrap_or_else(|| "—".into()),
                real_at
                    .map(|v| format!("{v:.3}"))
                    .unwrap_or_else(|| "—".into()),
                real_ct
                    .map(|v| format!("{v:.3}"))
                    .unwrap_or_else(|| "—".into()),
            ));
            // One number per cell — the metric selector swaps the shown value and
            // its colour class; all four ride data-* and the full set is on hover.
            // Default is realism joint (rj). Hatch cells carry no data-* and the
            // selector skips them.
            h.push_str(&format!(
                "<td class=\"gc pw-mcell {real_cls}\" data-acc=\"{}\" data-acc-cls=\"gb{bin}\" data-rj=\"{real_txt}\" data-rj-cls=\"{real_cls}\" data-at=\"{at_txt}\" data-at-cls=\"{at_cls}\" data-ct=\"{ct_txt}\" data-ct-cls=\"{ct_cls}\" title=\"{hover}\"><span class=\"cn\">{real_txt}</span></td>",
                attr_escape(&sep_txt)
            ));
        }
        h.push_str("</tr>");
    }
    h.push_str("</tbody></table></div>");
    h.push_str("<div class=\"disclosures\"><details><summary>");
    h.push_str(DISCLOSURE_SUMMARY);
    h.push_str("</summary><div class=\"disc-body\">Each cell is an equal-weight-per-object marginal of the surfaces over the arc axis within one horizon bin — never a pooled one-dimensional statistic. The metric selector above chooses what each cell shows: accuracy (median separation, decade ladder) or realism — joint, along-track or cross-track — on the diverging scale (median z² over the χ²₁ median, 1 calibrated); the other three ride the cell hover. Thin bins (fewer than three objects) are hatched with n.</div></details></div>");
    h
}

/// 4.2D — the configuration arms: arm × {median realism (joint, along, cross),
/// An additional walk run rendered as its own 4.2 block, after the production
/// 4.2D content: a head line (run · snapshot · profile · engine · arms · scored
/// rows), the factor × level table over the factors that VARY in the run, the
/// full arm list fold, the populated D-003 law-aware log-score ranking, and one
/// ▸Method fold. It reuses [`pw_main_effects_html`] (factor sweep) and
/// [`pw_log_score_table`]; it is never merged into the `--predict-agg` shards,
/// and every absent provenance value is a named state, never blank.
fn pw_run_block_html(agg: &PredictAggregates, label: &str) -> String {
    // The arms the run walked (rust per-object presence), full axis order; the
    // factors that distinguish them are the run's swept factors.
    let all_arms = pw_all_arms(agg);
    let run_arms: Vec<&str> = all_arms
        .iter()
        .copied()
        .filter(|a| {
            agg.per_object
                .iter()
                .any(|p| p.tool == "rust" && p.config_arm == *a)
        })
        .collect();
    let factors = pw_varying_factors(&run_arms);

    // Head-line provenance — each field a value or a NAMED absent state.
    let profile = if agg.profile == "unstated" {
        "profile unstated".to_string()
    } else {
        format!("profile {}", attr_escape(&agg.profile))
    };
    // Engine: the distinct versions the walk recorded on its window rows, or the
    // named absent state when the walk recorded none (no hidden fallback).
    let engine = {
        let mut vers: BTreeSet<&str> = BTreeSet::new();
        for e in &agg.engine_versions {
            for v in &e.versions {
                vers.insert(v.as_str());
            }
        }
        if vers.is_empty() {
            "engine not recorded".to_string()
        } else {
            format!(
                "engine {}",
                attr_escape(&vers.into_iter().collect::<Vec<_>>().join(", "))
            )
        }
    };
    // Rows scored into the ranking: the held-out rows pooled per arm under its
    // own law (the overall, class = all calibration cells).
    let scored_rows: u64 = agg
        .calibration
        .iter()
        .filter(|c| c.stratum == "overall" && c.class == "all" && c.mean_log_score.is_some())
        .map(|c| u64::from(c.n_rows))
        .sum();

    let mut h = String::new();
    h.push_str(&format!(
        "<div class=\"panel-title\">{}</div>",
        attr_escape(label)
    ));
    // Plain grid-summary (not a `reading` line): the head states every field on
    // the face. Table bodies below carry the numbers (table-exempt).
    h.push_str(&format!(
        "<div class=\"grid-summary\" title=\"snapshot · profile · engine as the walk's window rows recorded it · arms · held-out rows scored per arm under its own law · populations pooled\">snapshot {} · {} · {} · {} arms · {} rows · all populations</div>",
        attr_escape(&agg.snapshot_id),
        profile,
        engine,
        run_arms.len(),
        scored_rows,
    ));

    // The factor × level table over the factors that vary within this run.
    h.push_str(&pw_main_effects_html(
        agg,
        &run_arms,
        MainEffectsMode::Factors,
    ));

    // The full arm list fold: one row per arm, the varying factors as columns
    // (reader words, never a raw slug) and the observed 1σ/2σ coverage.
    let cov_cell = |v: Option<f64>| match v {
        Some(x) => format!("<td class=\"gc\"><span class=\"cn\">{x:.3}</span></td>"),
        None => "<td class=\"gc hatch\" title=\"no covered cell for this arm\"></td>".to_string(),
    };
    h.push_str("<details class=\"more-tables\"><summary>full arm list</summary>");
    h.push_str("<div class=\"grid-scroll\"><table class=\"agrid pw-grid\" data-grid=\"predict\"><thead><tr>");
    let mut first = true;
    for f in &factors {
        if first {
            h.push_str(&format!("<th class=\"gobj\">{}</th>", f.name()));
            first = false;
        } else {
            h.push_str(&format!("<th>{}</th>", f.name()));
        }
    }
    if factors.is_empty() {
        h.push_str("<th class=\"gobj\">arm</th>");
    }
    h.push_str(&format!(
        "<th title=\"observed 1σ inside-ellipse fraction · normal-law χ²₂ expectation 0.3935\">{}</th><th title=\"observed 2σ inside-ellipse fraction · normal-law χ²₂ expectation 0.8647\">{}</th></tr></thead><tbody>",
        uhdr("1σ obs"),
        uhdr("2σ obs"),
    ));
    for arm in &run_arms {
        let c1 = pw_median(
            agg.cells
                .iter()
                .filter(|c| c.tool == "rust" && c.config_arm == **arm && c.n_objects >= 3)
                .filter_map(|c| c.cov_1s),
        );
        let c2 = pw_median(
            agg.cells
                .iter()
                .filter(|c| c.tool == "rust" && c.config_arm == **arm && c.n_objects >= 3)
                .filter_map(|c| c.cov_2s),
        );
        h.push_str("<tr>");
        if factors.is_empty() {
            h.push_str(&format!(
                "<td class=\"gtool\">{}</td>",
                attr_escape(&pw_arm_words(arm, &PwFactor::ALL))
            ));
        } else {
            let mut cells = factors.iter().enumerate();
            for (i, f) in cells.by_ref() {
                let lvl = f.level(arm).unwrap_or_else(|| "—".into());
                if i == 0 {
                    h.push_str(&format!("<td class=\"gtool\">{}</td>", attr_escape(&lvl)));
                } else {
                    h.push_str(&format!("<td>{}</td>", attr_escape(&lvl)));
                }
            }
        }
        h.push_str(&cov_cell(c1));
        h.push_str(&cov_cell(c2));
        h.push_str("</tr>");
    }
    h.push_str("</tbody></table></div></details>");

    // The D-003 law-aware log-score ranking from this run's calibration cells.
    h.push_str(&pw_log_score_table(agg));

    // One ▸Method fold — the method only, no narrative about what ships where.
    h.push_str("<div class=\"disclosures\"><details><summary>");
    h.push_str(DISCLOSURE_SUMMARY);
    h.push_str("</summary><div class=\"disc-body\">An additional walk run, scored by this report's own scorer and rendered on its own, never pooled with the production walk. Each arm is scored under its own recorded error law: normal, or Student-t with ν degrees of freedom as both the per-observation error law in the fit and the predictive density in scoring. The log score is the mean log predictive density under that arm's own law; a higher (less negative) value is the better predictive density. It is shown over two regimes: all held-out rows, and the observation-dominated rows (prediction-to-observation trace ratio &lt; 0.1), where the propagated covariance is small against the observation σ, so the predictive covariance is essentially the σ table and the score tests the error law and the σ table; the remaining rows (trace ratio ≥ 0.1) are where the propagated covariance carries the prediction and the score tests it — the two differ by a factor of two to three, so each is named. The standard error is night-clustered. Δ vs the baseline arm is the difference of the two arms' observation-dominated means with the standard error in quadrature — a difference of means, not a paired statistic. The factor × level table sweeps one factor over the arms that vary and marginalizes the rest; the full arm list is every arm, its varying-factor levels and observed 1σ/2σ coverage. Every absent value is a named state, never blank.</div></details></div>");
    h
}

/// Which 4.2D view [`pw_arms_calibration_html`] renders.
enum ArmsView<'a> {
    /// The production 4.2D: token-sweep main effects, the full factor grid, the
    /// full arm list, the `log-score absent` mark, and the ranking gated on a
    /// Student-t arm. One ▸Method fold.
    Production,
    /// An additional walk run as its own block after the production 4.2D
    /// content: a head line, the factor × level table over the factors that
    /// vary, the full arm list fold, and the populated D-003 log-score ranking.
    /// One ▸Method fold describing the method only.
    Run { label: &'a str },
}

/// χ²₂-99% tail, median reduced-χ²}, plus a main-effects sub-matrix. Every arm
/// at once, numbers only, no ranking word.
/// 4.2D — configuration arms and calibration, merged (S3 item 2): one factor ×
/// level table (marginal realism · 1σ obs · 2σ obs · arms), one `full factor
/// grid`, one `full arm list`, one Method. The `log-score absent` mark stays and
/// the D-003 log-score ranking table (populated or awaiting Student-t) follows.
///
/// With [`ArmsView::Run`] the same builders render an additional run as its own
/// block (head line, factor × level over the factors that vary, full arm list
/// fold, populated ranking) rather than the production 4.2D — never merged into
/// the `--predict-agg` shards.
fn pw_arms_calibration_html(agg: &PredictAggregates, view: ArmsView) -> String {
    if let ArmsView::Run { label } = view {
        return pw_run_block_html(agg, label);
    }
    // Every arm present in the aggregate (the shared full arm axis), so the count
    // reconciles with 4.2A and no arm is silently dropped. Per-arm statistics below
    // come from the rust per-object summaries (the true per-object statistic); an
    // arm no rust walk produced renders as an explicit hatched row.
    let arms = pw_all_arms(agg);
    // Tail fraction P(d² > χ²₂ 99%) per arm from the pooled (class = all)
    // d² histogram, when one exists.
    let tail_of = |arm: &str| -> Option<(f64, u32)> {
        let hist = agg
            .d2_histograms
            .iter()
            .find(|h| h.tool == "rust" && h.config_arm == arm && h.class == "all")?;
        if hist.n == 0 {
            return None;
        }
        // Count rows at or above CHI2_2_P99 using the linear bins + overflow.
        let width = hist.domain_max / hist.counts.len().max(1) as f64;
        let mut above = hist.overflow;
        for (i, &c) in hist.counts.iter().enumerate() {
            let edge = (i + 1) as f64 * width;
            if edge > CHI2_2_P99 {
                above += c;
            }
        }
        Some((above as f64 / hist.n as f64, hist.n))
    };
    let rchi2_of = |arm: &str| -> Option<(f64, Vec<u32>)> {
        let hist = agg
            .reduced_chi2
            .iter()
            .find(|h| h.tool == "rust" && h.config_arm == arm)?;
        if hist.n_with == 0 {
            return None;
        }
        // Median bin of the log10 histogram.
        let total: u32 = hist.counts.iter().sum();
        if total == 0 {
            return None;
        }
        let half = total / 2;
        let mut acc = 0u32;
        let n_bins = hist.counts.len().max(1);
        let step = (hist.log10_hi - hist.log10_lo) / n_bins as f64;
        let mut med = hist.log10_lo;
        for (i, &c) in hist.counts.iter().enumerate() {
            acc += c;
            if acc >= half {
                med = hist.log10_lo + (i as f64 + 0.5) * step;
                break;
            }
        }
        let dof: Vec<u32> = hist.dof.iter().map(|d| d.ndof).collect();
        Some((10f64.powf(med), dof))
    };

    let mut h = String::new();
    h.push_str("<div class=\"panel-title\">Configuration arms</div>");
    // The calibration mark stays (S3 item 2): the law-aware log-score ranking has
    // no data until a Student-t arm runs.
    h.push_str("<div class=\"grid-summary reading\" title=\"observed 1σ/2σ coverage against the normal-law χ²₂ expectation 0.3935 / 0.8647; the law-aware log-score ranking has no data until a Student-t arm runs\">log-score absent</div>");
    // Main effects first (owner, 2026-10-17); the full factor grid sits behind a
    // toggle. Swept over the rust factorial only — the other tools' single arms
    // carry no factor structure, so they are not part of a swept level.
    let rust_arms: Vec<&str> = arms
        .iter()
        .copied()
        .filter(|a| {
            agg.per_object
                .iter()
                .any(|p| p.tool == "rust" && p.config_arm == *a)
        })
        .collect();
    h.push_str(&pw_main_effects_html(
        agg,
        &rust_arms,
        MainEffectsMode::Tokens,
    ));
    h.push_str("<details class=\"more-tables\"><summary>full factor grid</summary>");
    h.push_str(&format!(
        "<div class=\"grid-scroll\"><table class=\"agrid pw-grid\" data-grid=\"predict\"><thead><tr><th class=\"gobj\">debias</th><th>rejection</th><th>nightly</th><th>transport</th><th title=\"joint median realism\">{}{RV_RULER}</th><th title=\"along-track median realism\">{}{RV_RULER}</th><th title=\"cross-track median realism\">{}{RV_RULER}</th><th>{}</th><th>{}</th><th>objects</th></tr></thead><tbody>",
        uhdr("median realism"),
        uhdr("along"),
        uhdr("cross"),
        uhdr("χ²₂ 99% tail"),
        uhdr("median χ²ᵣ"),
    ));
    for arm in &arms {
        // An arm present in the cells but with no rust walk (another tool's single
        // arm): an explicit hatched row with its reason, so the 38 vs 36 arm counts
        // reconcile and nothing vanishes silently.
        let has_rust = agg
            .per_object
            .iter()
            .any(|p| p.tool == "rust" && p.config_arm == *arm);
        if !has_rust {
            let fac = pw_arm_factors(arm);
            h.push_str(&format!(
                "<tr><td class=\"gtool\" title=\"{}\">{}</td><td>{}</td><td>{}</td><td>{}</td><td class=\"gc hatch\" title=\"no rust walk for this arm\"></td><td class=\"gc hatch\" title=\"no rust walk for this arm\"></td><td class=\"gc hatch\" title=\"no rust walk for this arm\"></td><td class=\"gc hatch\" title=\"no rust walk for this arm\"></td><td class=\"gc hatch\" title=\"no rust walk for this arm\"></td><td class=\"r1c\"><span class=\"cn-n\">0</span></td></tr>",
                attr_escape(arm),
                fac[0], fac[1], fac[2], fac[3],
            ));
            continue;
        }
        let med_over = |pick: fn(&ObjectWalkSummary) -> Option<f64>| {
            pw_median(
                agg.per_object
                    .iter()
                    .filter(|p| p.tool == "rust" && p.config_arm == *arm)
                    .filter_map(pick),
            )
        };
        let real = med_over(|p| p.med_d2_norm);
        let real_at = med_over(|p| p.med_z2_at_norm);
        let real_ct = med_over(|p| p.med_z2_ct_norm);
        let n_obj = agg
            .per_object
            .iter()
            .filter(|p| p.tool == "rust" && p.config_arm == *arm && p.med_d2_norm.is_some())
            .count();
        let rcell = |r: Option<f64>| match r {
            Some(v) => format!(
                "<td class=\"gc {}\"><span class=\"cn\">{v:.2}</span></td>",
                realism_bin(v).0
            ),
            None => "<td class=\"gc rv0\"><span class=\"cn\">—</span></td>".to_string(),
        };
        let (real_cls, real_txt) = match real {
            Some(v) => (realism_bin(v).0, format!("{v:.2}")),
            None => ("rv0", "—".into()),
        };
        let tail_txt = match tail_of(arm) {
            Some((f, n)) => format!(
                "<span class=\"cn\" title=\"{n} rows\">{:.1}%</span>",
                f * 100.0
            ),
            None => format!(
                "<span class=\"cn hatch-inline\" title=\"no d² histogram for this arm\">{}</span>",
                "—"
            ),
        };
        let rchi2_txt = match rchi2_of(arm) {
            Some((m, dof)) => {
                let dof_str = if dof.is_empty() {
                    String::new()
                } else {
                    format!(
                        " · ν {}–{}",
                        dof.iter().min().copied().unwrap_or(0),
                        dof.iter().max().copied().unwrap_or(0)
                    )
                };
                format!(
                    "<span class=\"cn\" title=\"median reduced-χ²{dof_str}\">{m:.2}</span>"
                )
            }
            None => "<span class=\"cn hatch-inline\" title=\"no reduced-χ² histogram for this arm\">—</span>".to_string(),
        };
        let fac = pw_arm_factors(arm);
        h.push_str(&format!(
            "<tr><td class=\"gtool\" title=\"{}\">{}</td><td>{}</td><td>{}</td><td>{}</td><td class=\"gc {real_cls}\"><span class=\"cn\">{real_txt}</span></td>{}{}<td>{tail_txt}</td><td>{rchi2_txt}</td><td class=\"r1c\"><span class=\"cn-n\">{n_obj}</span></td></tr>",
            attr_escape(arm),
            fac[0], fac[1], fac[2], fac[3],
            rcell(real_at),
            rcell(real_ct),
        ));
    }
    h.push_str("</tbody></table></div>");
    h.push_str("</details>");

    // ── Calibration: the full per-arm 1σ/2σ coverage list (S3 item 2) ──
    // The rust factorial as factor columns; the external tools' single arms as
    // tool-named rows (no factor structure). One fold, beside the full factor grid.
    let cov_cell = |v: Option<f64>| match v {
        Some(x) => format!("<td class=\"gc\"><span class=\"cn\">{x:.3}</span></td>"),
        None => "<td class=\"gc hatch\" title=\"no covered cell for this arm\"></td>".to_string(),
    };
    h.push_str("<details class=\"more-tables\"><summary>full arm list</summary>");
    h.push_str(&format!(
        "<div class=\"grid-scroll\"><table class=\"agrid pw-grid\" data-grid=\"predict\"><thead><tr><th class=\"gobj\">debias</th><th>rejection</th><th>nightly</th><th>transport</th><th title=\"normal-law χ²₂ expectation 0.3935\">{}</th><th title=\"normal-law χ²₂ expectation 0.8647\">{}</th></tr></thead><tbody>",
        uhdr("1σ obs"),
        uhdr("2σ obs"),
    ));
    for arm in &arms {
        let has_rust = agg
            .cells
            .iter()
            .any(|c| c.tool == "rust" && c.config_arm == *arm);
        if !has_rust {
            // External tool's single arm: a row named by tool, no factor structure.
            let tool = agg
                .cells
                .iter()
                .find(|c| c.config_arm == *arm)
                .map(|c| c.tool.as_str())
                .unwrap_or("");
            h.push_str(&format!(
                "<tr><td class=\"gtool\" title=\"{}\">{} · {}</td><td>—</td><td>—</td><td>—</td><td class=\"gc hatch\" title=\"no rust walk for this arm\"></td><td class=\"gc hatch\" title=\"no rust walk for this arm\"></td></tr>",
                attr_escape(arm),
                attr_escape(pw_tool_label(tool)),
                attr_escape(arm),
            ));
            continue;
        }
        let fac = pw_arm_factors(arm);
        let c1 = pw_median(
            agg.cells
                .iter()
                .filter(|c| c.tool == "rust" && c.config_arm == *arm && c.n_objects >= 3)
                .filter_map(|c| c.cov_1s),
        );
        let c2 = pw_median(
            agg.cells
                .iter()
                .filter(|c| c.tool == "rust" && c.config_arm == *arm && c.n_objects >= 3)
                .filter_map(|c| c.cov_2s),
        );
        h.push_str(&format!(
            "<tr><td class=\"gtool\" title=\"{}\">{}</td><td>{}</td><td>{}</td><td>{}</td>{}{}</tr>",
            attr_escape(arm),
            fac[0], fac[1], fac[2], fac[3],
            cov_cell(c1),
            cov_cell(c2),
        ));
    }
    h.push_str("</tbody></table></div>");
    h.push_str("</details>");

    // D-003 log-score ranking: populated only when a Student-t arm has run,
    // otherwise the marked-absent hatched row (the log-score absent mark above).
    if agg
        .calibration
        .iter()
        .any(|c| c.scoring_law == "student-t" && c.mean_log_score.is_some())
    {
        h.push_str(&pw_log_score_table(agg));
    } else {
        h.push_str(&format!(
            "<div class=\"grid-scroll\"><table class=\"agrid pw-grid\" data-grid=\"predict\"><thead><tr><th class=\"gobj\">log-score ranking — arm</th><th>{}</th><th>{}</th><th>nights</th></tr></thead><tbody><tr><td class=\"gc hatch\" title=\"{AWAITING_STUDENT_T}\" colspan=\"4\">{AWAITING_STUDENT_T}</td></tr></tbody></table></div>",
            uhdr("mean log-score"),
            uhdr("SE"),
        ));
    }

    // One Method (S3 item 2): the merged bodies of Configuration arms and
    // Calibration, so the section carries a single ▸Method fold.
    h.push_str("<div class=\"disclosures\"><details><summary>");
    h.push_str(DISCLOSURE_SUMMARY);
    h.push_str("</summary><div class=\"disc-body\">The full factorial over debias, outlier rejection, nightly bundling and covariance transport. Median realism is the surfaces realism statistic over the arm's objects; the χ²₂ 99% tail is the fraction of held-out d² beyond 9.21; the reduced-χ² is the in-sample fit statistic (expected central interval depends on ν). Observed 1σ/2σ coverage is the equal-weight-per-object median over the surface cells of the fraction of held-out d² within 1σ and 2σ, against the normal-law χ²₂ expectation (0.3935, 0.8647). Main effects sweep one factor and marginalize the rest; the factor-level rows take that same median over all the level's cells (rust, n ≥ 3), so they reconcile with the full-arm-list rows. The proper scoring rule — mean log predictive density — and the law-aware coverage, PIT and Cramér–von Mises statistics are empty until a Student-t arm runs; their row is hatched \"awaiting Student-t chain\", never fabricated. An arm no rust walk produced is hatched. No arm is ranked — the reader compares the numbers.</div></details></div>");
    h
}

/// How [`pw_main_effects_html`] enumerates the swept levels.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MainEffectsMode {
    /// Production 4.2D: one row per arm TOKEN carried by ≥ 2 arms, the factor
    /// name from [`pw_token_factor`] and the raw token as the level. Byte-stable
    /// with the pre-2026-10-18 render.
    Tokens,
    /// An additional run: one row per LEVEL of each factor that has ≥ 2 distinct
    /// applicable levels among the arms (the factors that VARY), levels as
    /// reader words, the absent-token default level included. Raw tokens never
    /// reach the page.
    Factors,
}

/// The 4.2D main-effects table: for each factor that varies across the arm set,
/// the marginal median realism and the observed 1σ/2σ coverage at each of its
/// levels — Configuration arms and Calibration merged into one (S3 item 2).
fn pw_main_effects_html(agg: &PredictAggregates, arms: &[&str], mode: MainEffectsMode) -> String {
    // The sweep rows as (factor name, level display, matching arms). Tokens mode
    // treats each distinct `.`/`+`/`_` token in ≥ 2 arms as a level with the raw
    // token shown; Factors mode groups arms by each varying factor's reader-word
    // level (and so represents an absent-token default level like "observed
    // information", which the token sweep cannot). The row render below is one
    // path for both.
    let mut sweeps: Vec<(&str, String, Vec<&str>)> = Vec::new();
    match mode {
        MainEffectsMode::Tokens => {
            let mut tokens: BTreeSet<&str> = BTreeSet::new();
            for a in arms {
                for t in pw_arm_tokens(a) {
                    tokens.insert(t);
                }
            }
            for tok in tokens {
                let matching: Vec<&str> = arms
                    .iter()
                    .copied()
                    .filter(|a| pw_arm_tokens(a).any(|t| t == tok))
                    .collect();
                if matching.len() < 2 {
                    continue; // a token carried by one arm is not a swept level
                }
                sweeps.push((pw_token_factor(tok), tok.to_string(), matching));
            }
        }
        MainEffectsMode::Factors => {
            for f in pw_varying_factors(arms) {
                let mut by_level: BTreeMap<String, Vec<&str>> = BTreeMap::new();
                for a in arms {
                    if let Some(lvl) = f.level(a) {
                        by_level.entry(lvl).or_default().push(a);
                    }
                }
                for (lvl, matching) in by_level {
                    sweeps.push((f.name(), lvl, matching));
                }
            }
        }
    }
    if sweeps.is_empty() {
        return String::new();
    }
    // The sub-matrix label rides in the header cell (table content, exempt from
    // the visible-word budget) rather than a separate counted panel-title. S3
    // item 2: realism (from the per-object summaries) and observed 1σ/2σ coverage
    // (from the surface cells, rust, n ≥ 3) share one row per factor level.
    let cov_cell = |v: Option<f64>| match v {
        Some(x) => format!("<td class=\"gc\"><span class=\"cn\">{x:.3}</span></td>"),
        None => "<td class=\"gc hatch\" title=\"no covered cell for this level\"></td>".to_string(),
    };
    let mut h = String::from(
        "<div class=\"grid-scroll\"><table class=\"agrid pw-grid\" data-grid=\"predict\"><thead>",
    );
    h.push_str(&format!(
        "<tr><th class=\"gobj\">factor</th><th>level</th><th title=\"marginal median realism over the level's objects · 1 = calibrated\">{}{RV_RULER}</th><th title=\"median over the level's cells (rust, n ≥ 3) · normal-law χ²₂ expectation 0.3935\">{}</th><th title=\"median over the level's cells (rust, n ≥ 3) · normal-law χ²₂ expectation 0.8647\">{}</th><th>arms</th></tr></thead><tbody>",
        uhdr("marginal realism"),
        uhdr("1σ obs"),
        uhdr("2σ obs"),
    ));
    for (factor, level, matching) in &sweeps {
        let real = pw_median(
            agg.per_object
                .iter()
                .filter(|p| p.tool == "rust" && matching.iter().any(|a| *a == p.config_arm))
                .filter_map(|p| p.med_d2_norm),
        );
        let (cls, txt) = match real {
            Some(v) => (realism_bin(v).0, format!("{v:.2}")),
            None => ("rv0", "—".into()),
        };
        let cov_med = |pick: fn(&SurfaceCell) -> Option<f64>| {
            pw_median(
                agg.cells
                    .iter()
                    .filter(|c| {
                        c.tool == "rust"
                            && c.n_objects >= 3
                            && matching.iter().any(|a| *a == c.config_arm)
                    })
                    .filter_map(pick),
            )
        };
        h.push_str(&format!(
            "<tr><td class=\"gtool\">{}</td><td>{}</td><td class=\"gc {cls}\"><span class=\"cn\">{txt}</span></td>{}{}<td class=\"r1c\"><span class=\"cn-n\">{}</span></td></tr>",
            factor,
            attr_escape(level),
            cov_cell(cov_med(|c| c.cov_1s)),
            cov_cell(cov_med(|c| c.cov_2s)),
            matching.len(),
        ));
    }
    h.push_str("</tbody></table></div>");
    h
}

/// 4.2E — the per-object walk: object × the `default` arm → windows
/// converged/expected, realism, median separation. Non-convergence is a row,
/// never a drop; an object no window converged on is the hatched state.
fn pw_objects_html(agg: &PredictAggregates) -> String {
    // The single (tool, arm) walked per object: the shared primary series, so the
    // matrix is never an empty (tool, arm) slice reading "0 objects".
    let (tool, arm) = pw_primary_series(agg);
    let mut rows: Vec<&ObjectWalkSummary> = agg
        .per_object
        .iter()
        .filter(|p| p.tool == tool && p.config_arm == arm)
        .collect();
    rows.sort_by(|a, b| {
        class_rank(&a.class)
            .cmp(&class_rank(&b.class))
            .then(a.object.cmp(&b.object))
    });

    let mut h = String::new();
    h.push_str("<div class=\"panel-title\">Per-object walk</div>");
    h.push_str("<div class=\"grid-key\"><span class=\"gk\"><span class=\"gk-sw hatch\" title=\"no window converged for this object\"></span>none converged</span></div>");
    h.push_str(&format!(
        "<div class=\"grid-scroll\"><table class=\"agrid pw-grid\" data-grid=\"predict\"><thead><tr><th class=\"gobj\">object</th><th>class</th><th>windows</th><th title=\"joint realism\">{}</th><th title=\"along-track realism\">{}</th><th title=\"cross-track realism\">{}</th><th>{}</th></tr></thead><tbody>",
        uhdr("realism"),
        uhdr("along"),
        uhdr("cross"),
        uhdr("median sep"),
    ));
    for p in &rows {
        let conv = format!("{}/{}", p.n_windows_converged, p.n_windows_expected);
        if p.n_windows_converged == 0 {
            h.push_str(&format!(
                "<tr><td class=\"gtool\">{}</td><td>{}</td><td class=\"gc hatch\" title=\"no window converged — {} expected\">{conv}</td><td class=\"gc hatch\"></td><td class=\"gc hatch\"></td><td class=\"gc hatch\"></td><td class=\"gc hatch\"></td></tr>",
                attr_escape(&p.object),
                attr_escape(&p.class),
                p.n_windows_expected,
            ));
            continue;
        }
        let (real_cls, real_txt, real_hover) = match p.med_d2_norm {
            Some(v) => (
                realism_bin(v).0,
                format!("{v:.2}"),
                format!("realism {v:.3}"),
            ),
            None => ("rv0", "—".into(), "no σ-dominated d²".into()),
        };
        let rcell = |r: Option<f64>, lbl: &str| match r {
            Some(v) => format!(
                "<td class=\"gc {}\" title=\"{lbl} realism {v:.3}\"><span class=\"cn\">{v:.2}</span></td>",
                realism_bin(v).0
            ),
            None => format!(
                "<td class=\"gc rv0\" title=\"{lbl}: no σ-dominated z\"><span class=\"cn\">—</span></td>"
            ),
        };
        let at_cell = rcell(p.med_z2_at_norm, "along-track");
        let ct_cell = rcell(p.med_z2_ct_norm, "cross-track");
        let sep_txt = p
            .med_sep_arcsec
            .map(|v| grid_compact(GridAxis::SepArcsec, v))
            .unwrap_or_else(|| "—".into());
        let sep_bin = p
            .med_sep_arcsec
            .map(|v| GridAxis::SepArcsec.bin(v))
            .unwrap_or(1);
        let trust = if p.trust_reasons.is_empty() {
            String::new()
        } else {
            let t: Vec<String> = p
                .trust_reasons
                .iter()
                .map(|(k, v)| format!("{k}:{v}"))
                .collect();
            format!(" · trust {}", t.join(" "))
        };
        h.push_str(&format!(
            "<tr><td class=\"gtool\">{}</td><td>{}</td><td class=\"gc\" title=\"{} predictions{trust}\"><span class=\"cn\">{conv}</span></td><td class=\"gc {real_cls}\" title=\"{}\"><span class=\"cn\">{real_txt}</span></td>{at_cell}{ct_cell}<td class=\"gc gb{sep_bin}\" title=\"{}\"><span class=\"cn\">{sep_txt}</span></td></tr>",
            attr_escape(&p.object),
            attr_escape(&p.class),
            p.n_predictions,
            attr_escape(&real_hover),
            attr_escape(
                &p.med_sep_arcsec
                    .map(|v| grid_fmt(GridAxis::SepArcsec, v))
                    .unwrap_or_else(|| "—".into())
            ),
        ));
    }
    h.push_str("</tbody></table></div>");
    h.push_str("<div class=\"disclosures\"><details><summary>");
    h.push_str(DISCLOSURE_SUMMARY);
    h.push_str("</summary><div class=\"disc-body\">Converged of expected windows per object for the primary (tool, arm). A partially-failed walk shows its fraction; an object no window converged on is the hatched state, counted, never dropped. Realism — joint, then along-track and cross-track — rides the diverging scale; separation the decade ladder. The engine's covariance-trust verdicts ride in each row's hover.</div></details></div>");
    h
}

/// The D-003 law-aware log-score ranking, drawn from the run's (class = all)
/// calibration cells — each arm under its own recorded law. The two regimes
/// differ by a factor of two to three, so a single unlabelled column would
/// mislead: each arm names both the `overall` stratum ("all held-out rows")
/// and the `trace_band == "<0.1"` stratum ("observation-dominated rows"), each
/// a mean log predictive density ± SE. Δ vs the run's baseline arm is the
/// difference of the two arms' observation-dominated means, SE in quadrature
/// (the cells carry no paired difference — never called paired). The arm is
/// reader words (never a raw slug); the block is sorted by the
/// observation-dominated column, best first. A stated number — never a winner
/// word, never a rank mark.
fn pw_log_score_table(agg: &PredictAggregates) -> String {
    // A (class = all) calibration cell for an arm at a given stratum / trace band.
    let cell = |arm: &str, stratum: &str, band: Option<&str>| -> Option<&CalibrationCell> {
        agg.calibration.iter().find(|c| {
            c.config_arm == arm
                && c.class == "all"
                && c.stratum == stratum
                && c.trace_band.as_deref() == band
                && c.mean_log_score.is_some()
        })
    };
    // One ranked arm per overall (class = all) cell that carries a score.
    let mut arms: Vec<&str> = {
        let mut v: BTreeSet<&str> = BTreeSet::new();
        for c in &agg.calibration {
            if c.stratum == "overall" && c.class == "all" && c.mean_log_score.is_some() {
                v.insert(c.config_arm.as_str());
            }
        }
        v.into_iter().collect()
    };
    // The observation-dominated mean is the sort key (best / least negative
    // first); an arm with no such cell sorts last, never silently dropped.
    let obsdom = |arm: &str| cell(arm, "trace_band", Some("<0.1")).and_then(|c| c.mean_log_score);
    arms.sort_by(|a, b| {
        obsdom(b)
            .partial_cmp(&obsdom(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // Arm identity as the words of the factors that vary across the ranked arms.
    let factors = pw_varying_factors(&arms);
    // The run's baseline arm (the series the difference is taken against).
    let baseline = agg
        .calibration
        .iter()
        .find_map(|c| c.baseline_series.clone());
    let base_obsdom = baseline
        .as_deref()
        .and_then(|b| cell(b, "trace_band", Some("<0.1")));

    let num = |c: Option<&CalibrationCell>| match c.and_then(|c| c.mean_log_score) {
        Some(v) => {
            let se = c.and_then(|c| c.mean_log_score_se);
            match se {
                Some(s) => format!("<span class=\"cn\">{v:.3} ± {s:.3}</span>"),
                None => format!("<span class=\"cn\">{v:.3}</span>"),
            }
        }
        None => "<span class=\"cn\">—</span>".to_string(),
    };

    let mut h = String::from(
        "<div class=\"grid-scroll\"><table class=\"agrid pw-grid\" data-grid=\"predict\"><thead><tr><th class=\"gobj\">log-score ranking — arm</th><th>law</th><th title=\"mean log predictive density over every held-out row · ± night-clustered SE\">",
    );
    h.push_str(&uhdr("all held-out rows"));
    h.push_str("</th><th title=\"mean log predictive density over the observation-dominated rows (prediction-to-observation trace ratio &lt; 0.1), where the predicted covariance is small against the observation σ · ± night-clustered SE\">");
    h.push_str(&uhdr("observation-dominated rows (trace ratio &lt; 0.1)"));
    h.push_str("</th><th title=\"difference of means; SE in quadrature\">");
    h.push_str(&uhdr("Δ vs baseline"));
    h.push_str("</th><th title=\"observation-dominated nights\">nights</th></tr></thead><tbody>");
    for arm in &arms {
        let law = match cell(arm, "overall", None).map(|c| c.scoring_law.as_str()) {
            Some("normal") => "normal".to_string(),
            Some("student-t") => "Student-t".to_string(),
            Some(other) => other.to_string(),
            None => "—".to_string(),
        };
        let ov = cell(arm, "overall", None);
        let od = cell(arm, "trace_band", Some("<0.1"));
        let is_baseline = baseline.as_deref() == Some(*arm);
        // Δ on the observation-dominated regime: the difference of this arm's and
        // the baseline arm's means, SE in quadrature — not a paired statistic.
        let delta = if is_baseline {
            "baseline".to_string()
        } else {
            match (
                od.and_then(|c| c.mean_log_score),
                base_obsdom.and_then(|c| c.mean_log_score),
            ) {
                (Some(m), Some(mb)) => {
                    let d = m - mb;
                    match (
                        od.and_then(|c| c.mean_log_score_se),
                        base_obsdom.and_then(|c| c.mean_log_score_se),
                    ) {
                        (Some(s), Some(sb)) => {
                            format!("{d:+.3} ± {:.3}", (s * s + sb * sb).sqrt())
                        }
                        _ => format!("{d:+.3}"),
                    }
                }
                _ => "—".to_string(),
            }
        };
        let nights = od.map(|c| c.n_nights).unwrap_or(0);
        h.push_str(&format!(
            "<tr><td class=\"gtool\">{}</td><td>{}</td><td class=\"gc\">{}</td><td class=\"gc\">{}</td><td class=\"gc\"><span class=\"cn\">{}</span></td><td class=\"r1c\"><span class=\"cn-n\">{}</span></td></tr>",
            attr_escape(&pw_arm_words(arm, &factors)),
            attr_escape(&law),
            num(ov),
            num(od),
            attr_escape(&delta),
            nights,
        ));
    }
    h.push_str("</tbody></table></div>");
    h
}

/// 4.2G — the per-object walk timeline shell: an object `<select>` and a mount
/// the client draws into from the verified predict sidecar. The one interactive
/// element; it shows nothing until the sidecar verifies.
fn pw_timeline_shell(agg: &PredictAggregates) -> String {
    // Objects available in the per-window family (the sidecar), stable order.
    let mut objs: Vec<(&str, &str)> = agg
        .per_window
        .iter()
        .map(|w| (w.object.as_str(), w.class.as_str()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    objs.sort();
    let mut h = String::new();
    h.push_str("<div class=\"panel-title\">Walk timeline</div>");
    h.push_str("<div class=\"pw-select\"><label class=\"pw-sel-label\"><select id=\"pw-timeline-object\" aria-label=\"object\" title=\"object\">");
    for (o, cl) in &objs {
        h.push_str(&format!(
            "<option value=\"{v}\" title=\"{cl}\">{v}</option>",
            v = attr_escape(o),
            cl = attr_escape(cl)
        ));
    }
    h.push_str("</select></label></div>");
    h.push_str("<div id=\"pw-timeline-chart\" style=\"height:360px;\"></div>");
    h.push_str("<div class=\"disclosures\"><details><summary>");
    h.push_str(DISCLOSURE_SUMMARY);
    h.push_str("</summary><div class=\"disc-body\">One point per fit window as the arc grows: normalized d² against the χ²₂ 50% / 95% bands, and median separation. Breaks are failed or empty windows; the curve is cut across apparition gaps. This chart reads the verified per-object sidecar fetched beside the dataset; it shows nothing until that file's byte length and hash match. The arc &times; dt surfaces marginalise exactly this per-window view.</div></details></div>");
    h
}

/// Build Part 4.2 "Predictive power" — the `s15` section body. `None` (no walk
/// loaded) renders the empty state; the section shell always renders.
fn build_predictive_power_html(
    agg: Option<&PredictAggregates>,
    runs: &[PredictAggregates],
) -> String {
    let Some(agg) = agg else {
        let mut h = String::from(
            "<div class=\"grid-summary\" title=\"No walk-forward aggregate was supplied (report --predict-agg). Run score-predictions and pass its aggregate to populate this section.\">No walk-forward data in this report.</div>",
        );
        // An additional run still renders its own block — the production
        // aggregate is independent of it.
        for (i, run) in runs.iter().enumerate() {
            let label = pw_run_label(runs.len(), i);
            h.push_str(&pw_arms_calibration_html(
                run,
                ArmsView::Run { label: &label },
            ));
        }
        return h;
    };
    // Provenance label (arms/classes/snapshot) is baked into 4.2A's one reading
    // line, server-side, so it is stated without a second reading line and
    // without trusting the client fetch.
    let profile = if agg.profile == "unstated" {
        "profile unstated".to_string()
    } else {
        format!("profile {}", agg.profile)
    };
    let mut h = String::new();
    // 4.2A Coverage leads — the predictive-power headline.
    h.push_str(&pw_coverage_html(agg));
    // 4.2B surfaces, 4.2C marginals, 4.2D arms + calibration (merged, S3 item 2),
    // 4.2E per-object walk, 4.2G timeline.
    h.push_str(&pw_surfaces_html(agg, &profile));
    h.push_str(&pw_marginals_html(agg));
    h.push_str(&pw_arms_calibration_html(agg, ArmsView::Production));
    h.push_str(&pw_objects_html(agg));
    h.push_str(&pw_timeline_shell(agg));
    // Each additional walk RUN as its own block, after the production 4.2
    // content; never merged into the --predict-agg shards.
    for (i, run) in runs.iter().enumerate() {
        let label = pw_run_label(runs.len(), i);
        h.push_str(&pw_arms_calibration_html(
            run,
            ArmsView::Run { label: &label },
        ));
    }
    h
}

/// The head label for one additional run block: "Additional run" when the report
/// carries a single extra run, else "Additional run N" (1-based).
fn pw_run_label(total: usize, idx: usize) -> String {
    if total <= 1 {
        "Additional run".to_string()
    } else {
        format!("Additional run {}", idx + 1)
    }
}

/// Generate the interactive HTML validation report.
///
/// `orbit_comparisons` is the optional sidecar from `validate od`'s
/// `_compare.jsonl` output — empty slice when no orbit-comparison data is
/// available (e.g., CI runs that skip the SBDB / find_orb queries).
///
/// `predict_agg` is the walk-forward predictive-power aggregate
/// (`score-predictions --out-agg`, merged by [`merge_predict_aggregates`]) —
/// `None` when no walk was loaded, which renders Part 4.2's empty state. The
/// small static families render server-side into the page; only the large
/// per-object timeline family travels to the client, as a second verified
/// sidecar beside the dataset.
// ── S5a: the "Configurations" appendix (A.3, `s05c`) ───────────────────────
// One subsection per EXTERNAL tool that has rows in this run, in the Part 2
// tool order. A tool with no rows emits nothing (adam-core has no `adamcore_*`
// fields in the schema, so its path produces nothing). empyrean is not an
// external tool and has no subsection — its configuration stays in A.4
// Provenance and in the §2.2 arms. Values come only from the rows (ASSIST's six
// arms from each arm row's `config=` note; every other tool's version/settings
// from its recorded fields). A setting the run did not record renders as the one
// null token below, "not recorded" on hover; nothing is hand-typed.
const CONFIG_TOOL_ORDER: [(&str, &str); 9] = [
    ("assist", "ASSIST"),
    ("findorb", "find_orb"),
    ("layup", "layup"),
    ("orbfit", "OrbFit"),
    ("grss", "GRSS"),
    ("kete", "kete"),
    ("jorbit", "jorbit"),
    ("oorb", "OpenOrb"),
    ("adamcore", "adam-core"),
];

/// The one null token a configuration table shows for a setting the run did not
/// record; the reason reads on hover.
fn cfg_not_recorded() -> String {
    "<span class=\"cfg-nd\" style=\"color:var(--ed-text-muted)\" title=\"not recorded\">&mdash;</span>"
        .to_string()
}

/// True when this external tool has at least one row in the merged inputs.
fn config_tool_present(results: &[ValidationResult], key: &str) -> bool {
    match key {
        "assist" => results
            .iter()
            .any(|r| r.channel == "assist" && r.test_type == "propagation"),
        "findorb" => results.iter().any(|r| {
            r.findorb_rms_residual.is_some()
                || r.findorb_vs_horizons_km.is_some()
                || r.findorb_d_ra_arcsec.is_some()
        }),
        "layup" => results
            .iter()
            .any(|r| r.layup_reduced_chi2.is_some() || r.layup_converged.is_some()),
        "orbfit" => results.iter().any(|r| r.orbfit_rms_arcsec.is_some()),
        "grss" => results.iter().any(|r| {
            r.grss_vs_horizons_km.is_some()
                || r.grss_separation_arcsec.is_some()
                || r.grss_rms_arcsec.is_some()
        }),
        "kete" => results.iter().any(|r| r.kete_time_ms.is_some()),
        "jorbit" => results.iter().any(|r| r.jorbit_time_ms.is_some()),
        "oorb" => results
            .iter()
            .any(|r| r.oorb_vs_horizons_km.is_some() || r.oorb_separation_arcsec.is_some()),
        // adam-core has no `adamcore_*` fields in the schema, so it never renders.
        _ => false,
    }
}

/// Parse an ASSIST `config=... key=value ... (assist X rebound Y)` note into its
/// recorded key/value settings and the ASSIST version, if the note carries them.
fn parse_assist_note(note: &str) -> (std::collections::HashMap<String, String>, Option<String>) {
    let mut kv = std::collections::HashMap::new();
    for tok in note.split_whitespace() {
        if let Some((k, v)) = tok.split_once('=') {
            kv.insert(k.to_string(), v.to_string());
        }
    }
    let version = note.find("(assist ").and_then(|a| {
        note[a + "(assist ".len()..]
            .split_whitespace()
            .next()
            .map(|t| t.trim_end_matches(')').to_string())
    });
    (kv, version)
}

/// The subsection head line: `tool · version` (version in mono, or the null
/// token when the run recorded none). The anchor id (`s05c-<key>`) is for deep
/// links; it need not appear in the rail.
fn config_subsection_head(anchor: &str, display: &str, version: Option<&str>) -> String {
    let ver = match version {
        Some(v) => format!("<code>{v}</code>"),
        None => cfg_not_recorded(),
    };
    format!(
        "  <div class=\"cfg-head\" style=\"font-family:var(--ed-font-display);font-weight:700;font-size:15px;color:var(--ed-text-primary);margin:18px 0 6px;\" id=\"s05c-{anchor}\">{display} &middot; {ver}</div>\n"
    )
}

/// ASSIST subsection: the six arms (three configurations × variational off/on)
/// as rows of one configuration table, read from each arm row's `config=` note.
fn config_assist_subsection(results: &[ValidationResult]) -> String {
    const ARMS: [(&str, &str, &str); 6] = [
        (
            "assist_default_variational_off",
            "ASSIST default",
            "off &middot; state only",
        ),
        (
            "assist_default_variational_on",
            "ASSIST default",
            "on &middot; first order (N=6)",
        ),
        (
            "assist_layup_variational_off",
            "ASSIST layup",
            "off &middot; state only",
        ),
        (
            "assist_layup_variational_on",
            "ASSIST layup",
            "on &middot; first order (N=6)",
        ),
        (
            "assist_asteroid_institute_variational_off",
            "ASSIST asteroid-institute",
            "off &middot; state only",
        ),
        (
            "assist_asteroid_institute_variational_on",
            "ASSIST asteroid-institute",
            "on &middot; first order (N=6)",
        ),
    ];
    let mut rows_html = String::new();
    let mut version: Option<String> = None;
    for (arm, cfg_label, variational) in ARMS {
        let Some(rep) = results
            .iter()
            .find(|r| r.channel == "assist" && r.propagation_uncertainty.as_deref() == Some(arm))
        else {
            continue;
        };
        let (kv, ver) = parse_assist_note(&rep.notes);
        if version.is_none() {
            version = ver;
        }
        let cell = |k: &str| {
            kv.get(k)
                .map(|v| format!("<code>{v}</code>"))
                .unwrap_or_else(cfg_not_recorded)
        };
        rows_html.push_str(&format!(
            "<tr><td style=\"text-align:left\">{cfg_label}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td style=\"text-align:left\">{variational}</td></tr>",
            cell("adaptive_mode"),
            cell("epsilon"),
            cell("min_dt"),
            cell("initial_dt"),
        ));
    }
    if rows_html.is_empty() {
        return String::new();
    }
    let sum = DISCLOSURE_SUMMARY;
    let head = config_subsection_head("assist", "ASSIST", version.as_deref());
    format!(
        "<div class=\"cfg-sub\">\n{head}  <div class=\"heatmap-container\">\n  <table class=\"agrid list\" data-grid=\"list\" style=\"font-size:11px;\">\n    <thead><tr><th style=\"text-align:left\">arm</th><th>adaptive mode</th><th>epsilon</th><th>min_dt <span class=\"u\">(d)</span></th><th>initial dt <span class=\"u\">(d)</span></th><th style=\"text-align:left\">variational</th></tr></thead>\n    <tbody>{rows_html}</tbody>\n  </table>\n  </div>\n  <div class=\"disclosures\"><details><summary>{sum}</summary><div class=\"disc-body\">N-body reference propagation and first-order partials from six variational particles. ASSIST models no Marsden &Delta;T term, so the five &Delta;T-bearing objects diverge under every configuration and are excluded from the Part 2 position medians &mdash; marked, never dropped.</div></details></div>\n</div>\n"
    )
}

/// A generic external-tool subsection: head → two-column setting·value table →
/// one Method fold. `settings` are the tool's configuration dimensions in order;
/// a `None` value renders the one null token with "not recorded" on hover.
fn config_generic_subsection(
    anchor: &str,
    display: &str,
    version: Option<&str>,
    settings: &[(&str, Option<&str>)],
    method: &str,
) -> String {
    let sum = DISCLOSURE_SUMMARY;
    let head = config_subsection_head(anchor, display, version);
    let mut body = String::new();
    for (name, val) in settings {
        let v = match val {
            Some(t) => format!("<code>{t}</code>"),
            None => cfg_not_recorded(),
        };
        body.push_str(&format!(
            "<tr><td style=\"text-align:left\">{name}</td><td style=\"text-align:left\">{v}</td></tr>"
        ));
    }
    format!(
        "<div class=\"cfg-sub\">\n{head}  <div class=\"heatmap-container\">\n  <table class=\"agrid list\" data-grid=\"list\" style=\"font-size:11px;\">\n    <thead><tr><th style=\"text-align:left\">setting</th><th style=\"text-align:left\">value</th></tr></thead>\n    <tbody>{body}</tbody>\n  </table>\n  </div>\n  <div class=\"disclosures\"><details><summary>{sum}</summary><div class=\"disc-body\">{method}</div></details></div>\n</div>\n"
    )
}

/// The subsection for one external tool. Only recorded values appear; the setting
/// names are the tool's known configuration dimensions (owner-specified in the
/// S5a brief), their values the run's record or the null token.
fn config_tool_subsection(results: &[ValidationResult], key: &str) -> String {
    match key {
        "assist" => config_assist_subsection(results),
        "findorb" => config_generic_subsection(
            "findorb",
            "find_orb",
            None,
            &[
                ("version", None),
                ("&sigma; source", None),
                ("over-observing rule", None),
                ("DE ephemeris file", None),
            ],
            "Orbit determination from ADES astrometry (post-fit RMS reference); its fitted orbit is then propagated by find_orb itself to the plan's epochs for a fit-then-propagate comparison, so these diffs include the fit-vs-JPL-orbit difference.",
        ),
        "layup" => config_generic_subsection(
            "layup",
            "layup",
            None,
            &[
                ("version", None),
                ("weighting mode", None),
                ("orbitfit flag", None),
            ],
            "Orbit determination from ADES; the reduced-&chi;&sup2; reference under Ver&eacute;s-2017 weighting.",
        ),
        "orbfit" => config_generic_subsection(
            "orbfit",
            "OrbFit",
            None,
            &[("version", None), ("rejection", None)],
            "Orbit determination with the OrbFit Consortium / IAU MPC CMC2003 outlier rejection.",
        ),
        "grss" => config_generic_subsection(
            "grss",
            "GRSS",
            None,
            &[("version", None), ("perturbers", None)],
            "Propagation, ephemeris and radar-capable orbit determination (Gauss-Radau Small-body Simulator); replays the plan's initial conditions and reports the suite's only post-fit radar residuals.",
        ),
        "kete" => config_generic_subsection(
            "kete",
            "kete",
            None,
            &[("version", None)],
            "Dar Dahlen's independent Rust/Python NEO toolkit (originally Caltech IPAC / NEO Surveyor). This run records propagation wall-clock only; no comparison or configuration fields.",
        ),
        "jorbit" => config_generic_subsection(
            "jorbit",
            "jorbit",
            None,
            &[("version", None)],
            "JAX autodiff propagator and orbit determination. This run records propagation wall-clock only; no comparison or configuration fields.",
        ),
        "oorb" => config_generic_subsection(
            "oorb",
            "OpenOrb",
            None,
            &[("version", None), ("perturbers", None)],
            "N-body propagation and ephemeris (Bulirsch&ndash;Stoer, planets + Moon + Pluto, relativity on; no asteroid perturbers). Plan SSB states are converted to OpenOrb's heliocentric convention and back.",
        ),
        // adam-core has no `adamcore_*` fields in the schema: config_tool_present
        // never reports it, and this arm never runs.
        _ => String::new(),
    }
}

/// The whole "Configurations" appendix (A.3, `s05c`): one subsection per present
/// external tool, in the Part 2 tool order.
fn render_configurations_section(results: &[ValidationResult]) -> String {
    let mut subs = String::new();
    for (key, _display) in CONFIG_TOOL_ORDER {
        if config_tool_present(results, key) {
            subs.push_str(&config_tool_subsection(results, key));
        }
    }
    let body = if subs.is_empty() {
        "  <div class=\"cfg-empty\" style=\"color:var(--ed-text-muted);font-size:12px;margin-top:8px;\">No external tool ran in this run.</div>\n".to_string()
    } else {
        subs
    };
    format!(
        "<div class=\"section\" id=\"s05c\" data-page=\"empyrean\">\n  <div class=\"section-num\">A.3</div>\n  <div class=\"section-title\" title=\"Configuration of each external reference tool in this run\">Configurations</div>\n{body}</div>"
    )
}

pub fn generate_report(
    results: &[ValidationResult],
    orbit_comparisons: &[crate::schema::OrbitComparison],
    output: &Path,
    summary: Option<&Path>,
    predict_agg: Option<&crate::predict_schema::PredictAggregates>,
    predict_runs: &[crate::predict_schema::PredictAggregates],
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
    // Sort by the one canonical population order (POPULATION_ORDER), then by
    // object name, so the embedded dataset's object order matches the grids.
    objects.sort_by(|a, b| {
        population_rank(a.1)
            .cmp(&population_rank(b.1))
            .then(a.0.cmp(b.0))
    });

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
    // The population legend reads in the one canonical order (POPULATION_ORDER),
    // so its group order matches every grid.
    populations.sort_by_key(|p| population_rank(p));

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
    let disagreement_json = disagreement_series_json(&disagreement_sigma_series(results));
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

    // S5a: the "Configurations" appendix (A.3). One subsection per external
    // tool with rows, server-rendered from the rows (empyrean is not one).
    let configurations_section = render_configurations_section(results);

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
    // The empyrean distribution version the component table reports, defined once
    // here and reused by the branded-export attribution so the two cannot drift.
    let empyrean_version = "0.10.0";
    // The branded-export attribution, built once server-side. It is emitted only
    // into the client script (never as visible page text); the download handler
    // appends each chart's own section, title and selected pair at click time.
    let export_attribution = format!(
        "EMPYREAN DYNAMICS · validation report · {report_run_date} · empyrean {empyrean_version}"
    );
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

    // S5 items 3+4+5: A.1 drops its seven stat cards and the two coloured dot
    // legends for ONE mono line of counts. The per-population and per-channel
    // coverage the dot legends carried is kept as names-with-counts on the same
    // line — no `.legend`, no `.pop-dot`, no categorical colour.
    let mut pop_obj_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for &(_, pop) in &objects {
        *pop_obj_counts.entry(pop).or_default() += 1;
    }
    let pop_counts_line = populations
        .iter()
        .map(|&pop| format!("{pop} {}", pop_obj_counts.get(pop).copied().unwrap_or(0)))
        .collect::<Vec<_>>()
        .join(" · ");
    let channel_counts_line = channels
        .iter()
        .map(|&ch| format!("{ch} {}", channel_coverage.get(ch).copied().unwrap_or(0)))
        .collect::<Vec<_>>()
        .join(" · ");

    // S5 item 6: the A.2 "planned / opt-in" line names ONLY the candidate tools
    // that truly have no rows in this run; a candidate that actually ran drops
    // off the line, and when all three ran the line disappears entirely.
    let a2_absent: Vec<&str> = [
        ("OrbFit", od_tool_present("orbfit", results)),
        ("kete", results.iter().any(|r| r.kete_time_ms.is_some())),
        ("jorbit", results.iter().any(|r| r.jorbit_time_ms.is_some())),
    ]
    .into_iter()
    .filter(|&(_, present)| !present)
    .map(|(name, _)| name)
    .collect();
    let a2_absent_line = if a2_absent.is_empty() {
        String::new()
    } else {
        format!(
            "<tr style=\"opacity:0.55\"><td colspan=\"3\" title=\"Runner scripts live under runners/; their JSON merges into the report when the tool is run.\"><small style=\"color:var(--ed-text-muted)\"><b>Planned / opt-in:</b> {} (not in this run)</small></td></tr>",
            a2_absent.join(" · ")
        )
    };

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
        write_summary(&rollups, results, path)?;
    }
    // Redesign (empyrean-k2rkx): server-side Part-1 agreement grids + H3.
    let h1_prop_grid_html = build_prop_agreement_grid(results);
    let h2_eph_grid_html = build_eph_agreement_grid(results);
    let od_closeness_grid_html = build_od_closeness_grid(results, orbit_comparisons);
    let h4_fidelity_grid_html = build_channel_fidelity_grid(results);
    let method_matrix_html = build_method_matrix_html(results);
    let method_outcome_html = build_method_outcome_html(results);
    let covariance_realism_html = build_covariance_realism_html(results, orbit_comparisons);
    // Part 4.2: the walk-forward predictive-power section. Server-rendered static
    // families; the per-object timeline travels to the client as a sidecar.
    let predictive_power_html = build_predictive_power_html(predict_agg, predict_runs);
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
    // S7: Part 2 is three fixed-slot sections (2.1/2.2/2.3) built by the one
    // `part1_section` renderer; the former tool-ranking and wall-clock matrices
    // are redistributed into their per-axis A · B · C slots.
    let part2_sections = build_part2_sections(results, orbit_comparisons, emp_eph, emp_od);

    // ── S6: Part 1 as three fixed-slot sections (1.1/1.2/1.3), the head line
    // stated once, and the rail generated from the same tree ──────────────────
    // The former eight sections (s02/s03/s-t1-prop · s06/s06b/s-t2-eph ·
    // s08b/s-t3-od) merge into one `section()` per axis with the slots
    // head → A·by object → B·by population → C·cost → fold. The grid-section
    // anchors (s02/s06/s08b) are reused so §1.3A deep links and scroll-margins
    // keep resolving; the lone-letter numbering is gone from Part 1.
    let od_fit_quality_html = build_od_fit_quality_panel(results, orbit_comparisons);
    let chart_h = FULL_CHART_HEIGHT_PX;
    // 1.1 Propagation
    let s1_1 = part1_section(
        "s02",
        "1.1",
        "Propagation",
        "Propagation — position agreement",
        "",
        &h1_prop_grid_html,
        &format!(
            "<div class=\"chart-container\"><div id=\"growth-chart\" style=\"height:{chart_h}px;\"></div></div>"
        ),
        &t1_prop_timing_html,
        "<div class=\"disclosures\"><details><summary>Method</summary><div class=\"disc-body\">By object: each cell is the absolute position offset in km between the selected tool and the reference at that horizon, on the decade ladder; a cell not run, or an impactor horizon past impact, is hatched with the reason, never dropped. By population: the median offset per population at each horizon, log y; a labelled dashed series traces a force-model gap.</div></details></div>",
    );
    // 1.2 Ephemeris — the error-growth chart carries the pair/channels toggle;
    // the itemised runaway list stays under 1.2 as the folded `outliers` table.
    let s1_2 = part1_section(
        "s06",
        "1.2",
        "Ephemeris",
        "Ephemeris — sky-plane agreement",
        "",
        &h2_eph_grid_html,
        &format!(
            "<div class=\"channel-toggle\" id=\"s06b-toggle\"><button class=\"active\" data-mode=\"rust\">Selected pair</button><button data-mode=\"all\" data-requires=\"empyrean\">empyrean channels</button></div><div class=\"chart-container\"><div id=\"eph-sep-chart\" style=\"height:{chart_h}px;\" title=\"Separation vs propagation offset, by population\"></div></div>"
        ),
        &t2_eph_timing_html,
        "<div class=\"disclosures\"><details class=\"more-tables\"><summary class=\"more-sum\">outliers</summary>\n  <div class=\"heatmap-container\">\n    <table class=\"agrid heatmap\" data-grid=\"outliers\" style=\"min-width:100%\" title=\"Rows beyond the sky-plane grid — the itemised runaway list for the selected pair\"><thead><tr><th style=\"text-align:left\">Object</th><th>Channel</th><th>dt</th><th>Observer</th><th><span class=\"u\">dRA·cos(δ)</span></th><th><span class=\"u\">dDec</span></th><th><span class=\"u\">‖d‖</span></th></tr></thead>\n      <tbody id=\"eph-outliers\"></tbody>\n    </table>\n  </div>\n  </details><details><summary>Method</summary><div class=\"disc-body\">By object: each cell is the mean angular separation in mas between the selected tool and the reference over the five sites, on the decade ladder; a cell not run is hatched. The folded outliers list every row beyond &plusmn;100 mas on the selected pair. By population: median separation per population at each horizon, log y, with the 1 mas Gaia single-frame floor.</div></details></div>",
    );
    // 1.3 Orbit determination — A the σ_eq closeness grid (1.3A), B the
    // fit-quality population marginal, C the per-fit cost.
    let s1_3 = part1_section(
        "s08b",
        "1.3",
        "Orbit determination",
        "Orbit determination — convergence &amp; closeness",
        "",
        &od_closeness_grid_html,
        &od_fit_quality_html,
        &t3_od_timing_html,
        "<div class=\"disclosures\"><details><summary>Method</summary><div class=\"disc-body\">By object: fill = <code>σ_eq = √(Δᵀ(Σ_fit+Σ_ref)⁻¹Δ/6)</code>, the 6-DOF Mahalanobis distance in Sun-centred ecliptic J2000 elements at a common epoch; shape carries convergence. Objects whose JPL solution used radar are hatched, counted, out of statistics; nine of 82 lack a finite metric (Σ_ref not positive-definite). By population: median n_obs, χ²ᵣ, post-fit RMS″ and σ_eq; the rejected fraction and JPL's solution date are not in the rows.</div></details></div>",
    );
    let part1_sections = format!("{s1_1}{s1_2}{s1_3}");
    let part1_head = part1_head_html();
    let part1_facts = part1_facts_html(&part1_facts(results));
    // S7 item 3: the whole sidebar generated from the part/section tree.
    let rail_html = build_rail_html();

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
  <table class="agrid list" data-grid="list" style="font-size:11px;">
    <thead><tr><th style="text-align:left">Component</th><th style="text-align:left">Value</th></tr></thead>
    <tbody>
      <tr><td>Frame</td><td>ICRF (J2000), barycentric</td></tr>
      <tr><td>Time scale</td><td>TDB (Barycentric Dynamical Time)</td></tr>
      <tr><td>Planetary ephemeris</td><td>JPL DE440</td></tr>
      <tr><td>Asteroid perturber set</td><td>SB441-N16: <code>1, 2, 3, 4, 7, 10, 15, 16, 31, 52, 65, 87, 88, 107, 511, 704</code></td></tr>
      <tr><td>Force model (standard)</td><td>Point-mass Sun + 8 planets + Moon + Pluto + 16 SB441-N16 asteroids; 1PN GR Einstein-Infeld-Hoffmann for Sun; non-gravitational A1 (radial) / A2 (transverse) / A3 (normal) accelerations with Marsden's <code>g(r)</code>. For asteroids, A2 ≠ 0 is the standard parameterisation of the <b>Yarkovsky effect</b> (Marsden, Sekanina &amp; Yeomans 1973 / Vokrouhlický et al. 2015).</td></tr>
      <tr><td>Integrator</td><td>GR15 (15-stage Gauss-Radau, adaptive step; derived solely from Everhart 1985)</td></tr>
      <tr><td>Autodiff</td><td>forward-mode partials, N = 6 (or 9 with non-grav)</td></tr>
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
  .header .subtitle {{ font-family: var(--ed-font-mono); font-size: 10px; color: var(--ed-accent); letter-spacing: 1.5px; text-transform: uppercase; }}
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
  .section-num {{ font-family: var(--ed-font-mono); font-size: 10px; color: var(--ed-text-muted); letter-spacing: 1.5px; text-transform: uppercase; margin-bottom: 8px; }}
  .section small {{ font-size: 11px; }}
  .section-title {{ font-family: var(--ed-font-display); font-weight: 700; font-size: 20px; color: var(--ed-text-primary); margin-bottom: 8px; }}
  .section-desc {{ font-size: 13px; color: var(--ed-text-secondary); line-height: 1.6; max-width: 900px; margin-bottom: 24px; }}
  .panel-title {{ font-family: var(--ed-font-mono); font-weight: 300; font-size: 10px; letter-spacing: 1.5px; text-transform: uppercase; color: var(--ed-text-muted); margin: 24px 0 8px 0; }}

  .heatmap-container {{ overflow-x: auto; max-width: 100%; padding-bottom: 8px; }}
  .heatmap-container::-webkit-scrollbar {{ height: 6px; }}
  .heatmap-container::-webkit-scrollbar-track {{ background: var(--ed-bg); border-radius: 3px; }}
  .heatmap-container::-webkit-scrollbar-thumb {{ background: var(--ed-scrollbar-thumb); border-radius: 3px; }}
  .heatmap-container::-webkit-scrollbar-thumb:hover {{ background: var(--ed-scrollbar-hover); }}
  table.heatmap {{ border-collapse: collapse; font-family: var(--ed-font-mono); font-size: 11px; }}
  table.agrid.heatmap th {{ padding: 6px 10px; color: var(--ed-text-secondary); font-weight: 300; letter-spacing: 1px; border-bottom: 1px solid var(--ed-border); position: sticky; top: var(--sticky-offset, 0px); background: var(--ed-bg); z-index: 1; }}
  /* One rule: every server-rendered table header renders uppercase, the
     agreement heatmaps' canonical case (Part 2 matrices and the outliers table
     included). Uppercasing words is right; uppercasing a symbol, a variable
     name or a unit corrupts it (σ_eq→Σ_EQ, dRA·cos(δ)→DRA·COS(Δ), ms→MS), so any
     header fragment carrying one is wrapped in the ONE no-transform span below
     ([`uhdr`] server-side, its `uhdr` twin client-side). */
  /* S8 item 1: one family — every grid header (every variant) uppercases via the
     shared table.agrid th rule below (folded in, not a second rule). */
  th .u {{ text-transform: none; letter-spacing: normal; }}
  table.heatmap th.dt-col {{ text-align: center; min-width: 70px; }}
  table.agrid.heatmap td {{ padding: 5px 8px; text-align: center; border-bottom: 1px solid var(--ed-border-subtle); cursor: default; }}
  table.heatmap td.obj-name {{ text-align: left; color: var(--ed-text-primary); font-size: 11px; white-space: nowrap; padding-right: 16px; position: sticky; left: 0; background: var(--ed-bg); }}
  table.heatmap td.pop-tag {{ text-align: left; font-size: 11px; padding-right: 12px; }}
  table.heatmap td.cell {{ font-size: 11px; border-radius: 2px; }}
  table.heatmap td.cell:hover {{ outline: 1px solid var(--ed-accent); outline-offset: -1px; }}
  table.heatmap td.cell.missing {{
    background: repeating-linear-gradient(45deg, #161c25, #161c25 4px, #1f2630 4px, #1f2630 8px);
    color: var(--ed-text-muted);
  }}
  .pop-dot {{ display: inline-block; width: 6px; height: 6px; border-radius: 50%; margin-right: 4px; vertical-align: middle; }}
  /* S5 item 4: charts carry no card (no border, no inset fill, no padding). */
  .chart-container {{ background: transparent; border: none; border-radius: 0; padding: 0; margin-bottom: 24px; }}
  .summary-grid {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: 12px; margin-bottom: 24px; }}
  .summary-card {{ background: var(--ed-surface); border: 1px solid var(--ed-border); border-radius: var(--ed-radius-md); padding: 20px; }}
  .summary-card .value {{ font-family: var(--ed-font-display); font-weight: 700; font-size: 28px; color: var(--ed-text-primary); }}
  .summary-card .label {{ font-family: var(--ed-font-mono); font-size: 10px; color: var(--ed-text-secondary); letter-spacing: 1.5px; text-transform: uppercase; margin-top: 4px; }}
  /* S5 item 4: the A.1 run summary as one mono line of counts (no stat cards). */
  .run-counts {{ font-family: var(--ed-font-mono); font-weight: 300; font-size: 11px; color: var(--ed-text-secondary); line-height: 1.9; }}
  .run-counts .rc-k {{ color: var(--ed-text-muted); text-transform: uppercase; letter-spacing: 1.5px; font-size: 10px; margin-right: 6px; }}
  .legend {{ display: flex; gap: 16px; flex-wrap: wrap; margin-bottom: 16px; }}
  .legend-item {{ display: flex; align-items: center; gap: 4px; font-family: var(--ed-font-mono); font-size: 11px; color: var(--ed-text-secondary); }}
  .channel-toggle {{ display: inline-flex; flex-wrap: wrap; gap: 6px; margin: 8px 0 16px 0; font-family: var(--ed-font-mono); font-size: 11px; }}
  .channel-toggle button {{ background: var(--ed-surface); border: 1px solid var(--ed-border); border-radius: var(--ed-radius-sm); color: var(--ed-text-secondary); padding: 4px 10px; cursor: pointer; font-family: inherit; font-size: 11px; }}
  .channel-toggle button.active {{ color: var(--ed-text-primary); border-color: var(--ed-accent); background: var(--ed-surface-raised); }}
  /* S8 item 1: the two-column appendix / simple data tables as one `list`
     variant of the grid family (object-list rows, right-aligned values). Its
     own double-class selectors beat the agrid base, so the family base does not
     clobber its padding; its header does not stick (short appendix tables). */
  table.agrid.list {{ min-width: 100%; }}
  .agrid.list th, .agrid.list td {{ padding: 6px 10px; border-bottom: 1px solid var(--ed-border); text-align: right; height: 24px; }}
  .agrid.list th {{ color: var(--ed-text-secondary); font-weight: 300; letter-spacing: 1px; font-size: 11px; position: static; }}
  .agrid.list td.obj {{ text-align: left; color: var(--ed-text-primary); }}
  .agrid.list tr.diverged {{ background: rgba(208, 80, 64, 0.08); }}

  /* Convergence matrix — three states, three glyphs, three classes. The
     not-attempted hatch reuses the heatmap's `.cell.missing` fill so "we
     have no number here" looks the same everywhere in the report. */
  table.heatmap td.conv {{ font-size: 13px; line-height: 1.1; cursor: help; }}
  table.heatmap td.conv-ok {{ color: #3d9a6d; }}
  table.heatmap td.conv-fail {{ color: var(--ed-error-text); background: rgba(208, 80, 64, 0.10); }}
  table.heatmap td.conv-na {{
    color: var(--ed-text-muted);
    background: repeating-linear-gradient(45deg, #161c25, #161c25 4px, #1f2630 4px, #1f2630 8px);
  }}
  table.heatmap tr.conv-footer td {{ border-top: 1px solid var(--ed-border); padding-top: 8px; }}
  .conv-key {{ font-size: 13px; margin-right: 4px; }}
  .conv-key.conv-ok {{ color: #3d9a6d; }}
  .conv-key.conv-fail {{ color: var(--ed-error-text); }}
  .conv-key.conv-na {{ color: var(--ed-text-muted); }}

  /* Capability pillar band (Overview) */
  .pillar-band {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(250px, 1fr)); gap: 12px; margin-top: 14px; }}
  .pillar {{ background: var(--ed-surface); border: 1px solid var(--ed-border); border-left-width: 3px; border-radius: var(--ed-radius-sm); padding: 14px 16px; display: flex; flex-direction: column; gap: 7px; }}
  .pillar .pt {{ font-family: var(--ed-font-mono); font-size: 10px; letter-spacing: 1.5px; text-transform: uppercase; color: var(--ed-text-secondary); }}
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
  .hero-card .hl {{ font-family: var(--ed-font-mono); font-size: 10px; color: var(--ed-text-secondary); letter-spacing: 1.5px; text-transform: uppercase; margin-bottom: 6px; }}
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
  /* S5 item 1: the masthead text starts on the content edge (rail 210 + the
     section's 60px gutter), so masthead and every section share one left edge
     at 1440 and 1920; the ≤1100px block below drops it back to the 20px gutter
     when the rail hides. */
  .masthead {{ padding: 48px 60px 22px 270px; max-width: 1600px; margin: 0 auto; border-bottom: 1px solid var(--ed-border); }}
  .masthead .mh-eyebrow {{ font-family: var(--ed-font-mono); font-size: 10px; color: var(--ed-accent); letter-spacing: 1.5px; text-transform: uppercase; }}
  .masthead h1 {{ font-family: var(--ed-font-display); font-weight: 700; font-size: 34px; letter-spacing: -0.5px; margin: 6px 0 10px; }}
  .masthead h1 .mh-dim {{ color: var(--ed-text-muted); }}
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
  .rail .rail-part {{ color: var(--ed-text-muted); letter-spacing: 1.5px; text-transform: uppercase; font-size: 10px; margin: 16px 0 6px; }}
  .rail .rail-part:first-child {{ margin-top: 0; }}
  .rail a {{ display: block; font-family: var(--ed-font-body); font-weight: 300; font-size: 13px; color: var(--ed-text-secondary); text-decoration: none; padding: 3px 8px; border-left: 2px solid transparent; line-height: 1.6; }}
  .rail a:hover {{ color: var(--ed-text-primary); }}
  .rail a.active {{ color: var(--ed-accent); border-left-color: var(--ed-accent); background: var(--ed-surface); }}
  .content {{ flex: 1 1 auto; min-width: 0; display: flex; flex-direction: column; }}
  .content .section {{ max-width: none; margin: 0; }}
  /* S7 item 3: the content column emits in DOM == visual order — Part 1, Part 2
     (2.1/2.2/2.3), Part 3, Part 4, then the appendix (run summary, external
     tools, configurations, provenance). No flex `order` reorders any section, so
     the DOM order is the visual order the reader and the scroll-spy both see. */
  @media (max-width: 1100px) {{ .rail {{ display: none; }} .masthead {{ padding: 32px 20px 18px; }} }}

  /* S5 item 1: the Part head is a left-aligned stack — a mono eyebrow over a
     Syne title — flush on the content edge (no centring, no max-width box), so
     rail / masthead / content share one left edge at 1440 and 1920. */
  .part-head {{ padding: 32px 60px 0; max-width: none; margin: 0; text-align: left; }}
  .ph-eyebrow {{ display: block; font-family: var(--ed-font-mono); font-weight: 300; font-size: 10px; letter-spacing: 1.5px; text-transform: uppercase; color: var(--ed-text-muted); margin-bottom: 4px; }}
  .ph-title {{ display: block; font-family: var(--ed-font-display); font-weight: 700; font-size: 28px; letter-spacing: -0.3px; color: var(--ed-text-primary); }}
  /* S6 item 2: the Part 1 head facts — epoch, frames, force-model pair, sites —
     stated once under the title in the eyebrow role. The per-fact label
     (data-k) and the " · " separators are CSS, so only the terse values are DOM
     text; values stay normal-case so the product name reads lowercase. */
  .ph-facts {{ display: flex; flex-wrap: wrap; gap: 3px 16px; margin-top: 6px; font-family: var(--ed-font-mono); font-weight: 300; font-size: 10px; letter-spacing: 0.2px; color: var(--ed-text-secondary); }}
  .ph-facts .phf {{ display: inline-flex; align-items: baseline; gap: 5px; }}
  .ph-facts .phf::before {{ content: attr(data-k); color: var(--ed-text-muted); letter-spacing: 1px; text-transform: uppercase; }}
  .ph-facts .phf-v + .phf-v::before {{ content: "· "; color: var(--ed-text-muted); }}
  /* S6 item 1/3: one slot eyebrow (the single uppercase eyebrow role, matching
     .ph-eyebrow's mono 300 10px 1.5px). The slot LETTER and its separator are a
     CSS marker; only the slot name is DOM text. It is the scroll-spy anchor its
     rail sub-entry targets. */
  .slot-eyebrow {{ display: block; font-family: var(--ed-font-mono); font-weight: 300; font-size: 10px; letter-spacing: 1.5px; text-transform: uppercase; color: var(--ed-text-muted); margin: 22px 0 8px; }}
  .slot-eyebrow::before {{ content: attr(data-slot) " · "; color: var(--ed-accent); }}

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
  /* S5 item 1: the bar IS the Part 1 head line — the Part head stack on the
     left, the Compare controls on the right, on one sticky line flush with the
     content edge (no centring box). */
  /* S8 item 3: the inner bar is a column — a top line (title + Compare on one
     row that never wraps) over the facts as a full-width second row. */
  .p1b-inner {{ display: flex; flex-direction: column; align-items: stretch; gap: 8px; padding: 18px 60px 14px; max-width: none; margin: 0; font-family: var(--ed-font-mono); font-size: 12px; }}
  .p1b-topline {{ display: flex; align-items: center; justify-content: space-between; gap: 16px; min-height: 40px; flex-wrap: nowrap; }}
  .p1b-topline .ph-head {{ flex: 1 1 auto; min-width: 0; }}
  /* The two selects and the "vs" between them stay on one line at every width. */
  .p1b-compare {{ display: flex; align-items: center; gap: 12px; flex-wrap: nowrap; white-space: nowrap; flex: 0 0 auto; }}
  /* The facts own the full width of the bar's second row, so they never share a
     line with the Compare selects. */
  .p1b-inner > .ph-facts {{ margin-top: 0; }}
  .part1-bar .p1b-label {{ color: var(--ed-text-muted); letter-spacing: 1.5px; text-transform: uppercase; font-size: 10px; }}
  .part1-bar .p1b-vs {{ color: var(--ed-text-muted); }}
  .part1-bar select {{ background: var(--ed-input-bg); color: var(--ed-accent); border: 1px solid var(--ed-input-border); border-radius: var(--ed-radius-sm); padding: 6px 12px; font-family: var(--ed-font-mono); font-size: 14px; font-weight: 700; cursor: pointer; }}
  .part1-bar select:focus-visible {{ outline: none; border-color: var(--ed-accent); box-shadow: 0 0 0 2px var(--ed-focus-ring); }}
  @media (max-width: 1100px) {{ .p1b-inner {{ padding-left: 20px; padding-right: 20px; }} }}
  /* A rail click / deep link to a Part 1 section must land below the pinned bar. */
  #s02, #s06, #s08b, .spy-anchor {{ scroll-margin-top: var(--bar-h); }}
  /* Per-tool text colours as theme-aware tokens: a tool NAME shown as text (the
     compare-bar selects and the section-title pair chips) reads its --tool-<slug>
     token so it clears WCAG AA (>= 4.5:1) on the card/input surface in BOTH themes.
     The light values are darkened exactly as --ed-accent is. One source, below. */
{tool_tokens_css}

  .section-q {{ font-family: var(--ed-font-body); font-size: 13px; color: var(--ed-text-secondary); line-height: 1.6; max-width: 900px; margin-bottom: 14px; }}
  .disclosures {{ margin-top: 12px; font-family: var(--ed-font-mono); font-size: 11px; }}
  .more-tables {{ margin-top: 10px; }}
  .more-tables > summary {{ cursor: pointer; display: inline-block; font-family: var(--ed-font-mono); font-size: 11px; color: var(--ed-text-secondary); border: 1px solid var(--ed-input-border); border-radius: var(--ed-radius-sm); padding: 3px 10px; list-style: none; }}
  .more-tables > summary::-webkit-details-marker {{ display: none; }}
  .more-tables > summary::before {{ content: "▸ "; color: var(--ed-text-muted); }}
  .more-tables[open] > summary::before {{ content: "▾ "; }}
  .more-tables > summary:hover {{ color: var(--ed-accent); border-color: var(--ed-accent); }}
  .more-tables[open] > summary {{ margin-bottom: 8px; }}
  .disclosures summary {{ color: var(--ed-accent); cursor: pointer; }}
  .disclosures .disc-body {{ font-family: var(--ed-font-body); font-weight: 300; font-size: 13px; color: var(--ed-text-secondary); line-height: 1.6; max-width: 900px; margin-top: 8px; }}
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
  .grid-tool-note {{ font-family: var(--ed-font-body); font-weight: 300; font-size: 13px; line-height: 1.6; color: var(--ed-text-muted); margin: 2px 0 10px; }}
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
     (e.g. the §1.2A folded outliers) scrolls inside that wrapper, so its header pins to the
     wrapper, not below the bar — the offset is inert there. */
  table.agrid th {{ padding: 5px 4px; color: var(--ed-text-secondary); font-weight: 300; letter-spacing: 0.3px; font-size: 11px; border-bottom: 1px solid var(--ed-border); position: sticky; top: var(--sticky-offset, 0px); background: var(--ed-bg); z-index: 2; text-transform: uppercase; }}
  table.agrid th.gdt {{ text-align: center; min-width: 40px; }}
  table.agrid th.fo-grp {{ text-align: center; border-left: 1px solid var(--ed-border); font-weight: 700; color: var(--ed-text-primary); }}
  .rvnote {{ font-size: 9px; color: var(--ed-text-muted); font-weight: 400; margin-top: 1px; }}
  table.agrid th.gt0h {{ color: var(--ed-text-muted); border-left: 3px solid var(--ed-border); }}
  table.agrid th.gobj {{ text-align: left; position: sticky; left: 0; z-index: 3; background: var(--ed-bg); }}
  table.agrid th.gworst {{ position: sticky; right: 0; background: var(--ed-bg); z-index: 3; text-align: left; min-width: 76px; }}
  .agrid td {{ padding: 0; border-bottom: 1px solid var(--ed-border-subtle); }}
  .agrid td.gtool {{ text-align: left; color: var(--ed-text-primary); font-size: 12px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; padding: 4px 12px 4px 16px; position: sticky; left: 0; background: var(--ed-bg); z-index: 1; }}
  .agrid tr.gsummary td.gtool {{ color: var(--ed-text-primary); }}
  /* Per-population median header row (H1/H2). */
  .agrid tr.gpophdr td.gpopname {{ font-family: var(--ed-font-display); font-weight: 700; font-size: 12px; color: var(--ed-text-primary); text-transform: uppercase; letter-spacing: 0.5px; padding: 6px 10px 6px 8px; position: sticky; left: 0; background: var(--ed-bg); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }}
  /* The covariance-arm provenance beside a 4.2A tool group name: a quieter mono
     slug that keeps its own case (text-transform: none) so efcc.adap.nonight is
     not uppercased. */
  .agrid tr.gpophdr td.gpopname .pwarm {{ font-family: var(--ed-font-mono); font-weight: 400; font-size: 11px; letter-spacing: 0; text-transform: none; color: var(--ed-text-muted); }}
  .agrid tr.gpophdr td {{ border-top: 2px solid var(--ed-border); }}
  /* S8 item 2: the 1.3A population header carries a converged/median summary cell
     beside the population name, in the quieter mono reading style. */
  .agrid tr.gpophdr td.gpop-sum {{ font-family: var(--ed-font-mono); font-weight: 400; font-size: 11px; letter-spacing: 0; text-transform: none; color: var(--ed-text-secondary); text-align: left; padding: 6px 10px; }}
  .agrid tr.gobjr td.gobjname2 {{ text-align: left; color: var(--ed-text-primary); font-size: 12px; padding: 3px 10px 3px 16px; position: sticky; left: 0; background: var(--ed-bg); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }}
  /* S8 item 2: a failed fit's object name (1.3A optical row) keeps the error ink
     even though it now carries the gobjname2 role. */
  .agrid tr.gobjr td.gobjname2.od-fail-name {{ color: var(--ed-error-text); }}
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
  .odgrid td.oc.fail .ocg {{ color: var(--ed-error-text); font-weight: 700; }}
  .odgrid td.oc.rw::after {{ content: ""; position: absolute; top: 0; right: 0; border-width: 0 6px 6px 0; border-style: solid; border-color: transparent var(--ed-text-primary) transparent transparent; }}
  .odgrid td.oc.jplref {{ color: var(--ed-text-muted); font-size: 11px; background: var(--ed-hatch); }}
  .odgrid td.od-optonly {{ text-align: center; color: var(--ed-text-muted); font-size: 11px; background: var(--ed-hatch); }}
  .odgrid td.od-d {{ text-align: right; color: var(--ed-text-secondary); font-size: 11px; padding: 2px 8px; white-space: nowrap; }}
  .odgrid tr.odradar td {{ opacity: 0.85; }}
  .odgrid tr.odradar td.gtool {{ padding-left: 30px; color: var(--ed-text-muted); }}
  .od-fail-name {{ color: var(--ed-error-text); }}
  .od-fails {{ margin-top: 16px; font-family: var(--ed-font-mono); font-size: 12px; }}
  .od-fails-h {{ color: var(--ed-error-text); letter-spacing: 1px; text-transform: uppercase; font-size: 12px; margin-bottom: 6px; }}
  .agrid.odfails td {{ padding: 3px 14px 3px 0; color: var(--ed-text-secondary); font-size: 11px; }}

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
  /* The one exception to the decade ladder: the realism diverging log₂
     scale, built from theme tokens via color-mix so it carries no hard-coded
     colour and re-themes with the page. */
{realism_css}
  /* Part 4.2 layout: the surface selector bar, the side-by-side accuracy/realism
     grid pair, the realism inline chip in the marginals matrix. */
  .pw-select {{ display: flex; gap: 14px; flex-wrap: wrap; align-items: center; margin: 8px 0; font-family: var(--ed-font-mono); font-size: 11px; }}
  .pw-sel-label {{ color: var(--ed-text-muted); }}
  .pw-select select {{ background: var(--ed-input-bg); color: var(--ed-text-primary); border: 1px solid var(--ed-input-border); border-radius: var(--ed-radius-sm); padding: 2px 6px; font-family: var(--ed-font-mono); font-size: 11px; }}
  .pw-grid-pair {{ display: flex; gap: 18px; flex-wrap: wrap; }}
  .pw-grid-one {{ flex: 1 1 320px; min-width: 0; }}
  .pw-grid-cap {{ font-family: var(--ed-font-mono); font-size: 10px; color: var(--ed-text-muted); text-transform: uppercase; letter-spacing: 1.5px; margin: 4px 0; }}
  .rv-chip {{ padding: 0 3px; border-radius: 2px; }}
  .panel-title.sub {{ font-size: 11px; opacity: 0.85; margin-top: 10px; }}
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
    --ed-accent: #2d6da3; --ed-accent-hover: #3d7ab8; --ed-accent-pressed: #24537c;
    --ed-input-bg: #ffffff; --ed-input-border: #cdd7e1;
    --ed-scrollbar-thumb: #c3ccd8; --ed-scrollbar-hover: #a8b4c2;
    --ed-chart-grid: rgba(40,80,130,0.14);
  }}
  :root.theme-light .masthead h1 .mh-dim {{ color: var(--ed-text-muted); }}
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
  <div class="mh-prov"><a href="#s13" class="mh-link" title="provenance &amp; references">provenance</a> &nbsp;·&nbsp; <a href="{dataset_file_name}" download class="mh-link" title="download dataset JSON">dataset</a></div>
  <div class="mh-controls">
    <label class="shownum"><input type="checkbox" id="show-numbers" checked> show numbers</label>
    <button id="theme-toggle" class="mh-toggle" title="Toggle light / dark theme" aria-label="Toggle light or dark theme">◐ theme</button>
  </div>
</div>

<div class="layout-wrap">
<nav class="rail" id="rail" aria-label="Contents">
{rail_html}</nav>
<main class="content">

<div class="part1-bar" id="part1-bar">
  <div class="p1b-inner">
  <div class="p1b-topline">
  <div class="ph-head">{part1_head}</div>
  <div class="p1b-compare">
  <span class="p1b-label">Compare</span>
  <select id="tool1-select" aria-label="Tool"></select>
  <span class="p1b-vs">vs</span>
  <select id="tool2-select" aria-label="Reference"></select>
  </div>
  </div>
  {part1_facts}
  </div>
</div>
{part1_sections}
<div class="section" id="overview-footer" hidden>
  Methods, all six reference tools, per-axis detail and outliers &rarr; <b>Advanced</b> &middot; cross-channel fidelity, timing and covariance &rarr; <b>empyrean internals</b>.
</div>

<div class="part-head" id="part2" title="No figure combines tools and no cell compares two tools; each states a tool's own numbers against JPL.">{part2_head}</div>
{part2_sections}
<div class="part-head" id="part3">{part3_head}</div>
<div class="section" id="s10">
  <div class="section-num">3.1</div>
  <div class="section-title" title="Channel fidelity — every distribution channel against empyrean-core">Channel fidelity</div>
  {h4_fidelity_grid_html}
  {method_matrix_html}
  {method_outcome_html}
  <div class="disclosures"><details><summary>{disc_summary}</summary><div class="disc-body">Each channel row pairs to its core row by (object, dt, force model, test type, observer, uncertainty method). Prop/OD compared in Cartesian position (km; ULP ≤ 1e-10 km, floor 1 mm); ephemeris as angular separation (arcsec), RA wrapped modulo a turn so a 360° wrap is no defect (floor 1 µas, 1 mas). A cell is its population's worst state, never the majority; DIFF cells name objects. A channel absent this run is hatched "not run" — here <code>c</code>, so the C ABI is unvalidated. The plan carries the full uncertainty-method grid onto every channel: the per-method matrix is each method's worst relative position difference against the rust reference (band 1e-10), <code>0 (bit-id)</code> when every paired row is bit-identical, the auto method keyed on the resolved rung, a refusal shown by name, and <code>not produced</code> where a channel has not produced that method at this pin — never an absent denominator or a coverage-gap halving of the counts. The outcome table carries one row per OD fit, post-fit transport and per-method product, statuses verbatim, with the mixture component count, surviving mass and refusal tallies. Every channel runs against empyrean-core, the validation reference; rust also appears in Parts 1&ndash;2 against JPL.</div></details></div>

</div>

<div class="section" id="s05" data-page="empyrean">
  <div class="section-num">3.2</div>
  <div class="section-title" title="Derivatives — first-order cross-validation">Derivatives</div>
  <div class="panel-title">First-order cross-validation</div>
  <div class="chart-container">
    <div id="stm-agree-chart" style="height:{chart_h}px;"></div>
  </div>
  <div class="disclosures"><details><summary>{disc_summary}</summary><div class="disc-body">Relative Frobenius difference vs |dt| (log-y): empyrean's first-order partials against the corrected ASSIST variational partials over all first-order rows (median solid, 95th-percentile dashed), and against an ASSIST single-particle finite difference on a representative subset (measured step-size floor 1e-8, dotted). The two analytic methods agree to machine precision on the bulk; the tail is the self-perturber rows and deep encounters; the finite difference anchors both to truth at its own floor.</div></details></div>
</div>

<div class="section" id="s09b">
  <div class="section-num">3.3</div>
  <div class="section-title" title="Non-gravitational recovery — fitted A1/A2/A3 vs JPL">Non-gravitational recovery</div>
  <div id="ng-empty" class="section-desc" style="display:none; color:#8b9198">No non-gravitational-recovery rows in this report. Run the OD subset against objects with a known SBDB non-grav signal to populate this section.</div>
  <div id="ng-content">
    <div class="panel-title" title="Fitted A1/A2/A3 vs JPL SBDB — empyrean's fit, with cross-channel agreement">Fitted A1/A2/A3 vs JPL SBDB</div>
    <div class="heatmap-container">
      <table class="agrid list" data-grid="list">
        <thead><tr>
          <th class="obj" style="text-align:left">Object</th>
          <th title="{coeff_defs}">Coeff</th>
          <th>Fitted <span class="u">± σ (AU/day²)</span></th>
          <th>JPL SBDB <span class="u">(AU/day²)</span></th>
          <th title="{verdict_rule}"><span class="u">z = (fit&minus;JPL)/σ</span></th>
          <th title="rust, cli and python compared cell by cell on fit and verdict; a mark means they agree, otherwise the differing channels are named">channels</th>
          <th title="the C ABI channel; absent this run, so the FFI non-grav block is unvalidated — hatched, counted, never dropped">c</th>
        </tr></thead>
        <tbody id="ng-overview"></tbody>
      </table>
    </div>
  </div>
  <div class="disclosures"><details><summary>{disc_summary}</summary><div class="disc-body">Objects with a non-zero JPL non-grav signal (Apophis, Bennu, the comets) are re-fit with <code>solve_for = StateAndNonGrav</code>; the fitted Marsden A1/A2/A3 and their 1σ (the (N=9) covariance diagonal) are compared to JPL. One row per (object, coefficient) states empyrean's fit; <b>PASS</b> when <code>|z| = |od_a − ic_a| / σ ≤ 3</code>. A <code>None</code> is a loud <b>FAIL</b>, not a blank — the (N=9) covariance was absent (silent fall-back to a state-only fit), the regression this section catches. The rust, cli and python channels collapse into one agreement column: a mark when they agree, otherwise the differing channel is named, so an FFI drop shows at once. The C ABI channel <code>c</code> is absent this run, so it is one hatched column — unvalidated, counted, never dropped.</div></details></div>
</div>

<div class="part-head" id="part4">{part4_head}</div>
<div class="section" id="s12">
  <div class="section-num">4.1</div>
  <div class="section-title" title="Consistency with JPL at epoch — σ-equivalent 6-D distance">Consistency with JPL at epoch</div>
  <div class="cov-panels">
    <div class="chart-container"><div id="cov-chi2-panel" style="height:340px;"></div></div>
    <div class="chart-container"><div id="cov-ratio-panel" style="height:340px;"></div></div>
    <div class="chart-container"><div id="cov-cdf-panel" style="height:340px;"></div></div>
  </div>
  {covariance_realism_html}
  <div class="disclosures"><details><summary>{disc_summary}</summary><div class="disc-body">A sigma-consistency test is not a coverage test: it shows consistency with JPL's stated covariance, not that either is correct. Every metric divides by an outlier-rejected formal covariance, the reference too; the measured post-selection effect is ~1.9–2.0× the formal 1σ, so part of the ~1.4 shortfall may be post-selection rather than a tight covariance, and tuning a threshold to a 0.94 median would tune toward an inflated sigma. Both pipelines sit below unity in reduced χ² under conservative Vereš-2017 weights — empyrean's 0.08 on Apophis reproduces the reference pipeline, visible only against JPL's 0.066 on the same object.</div></details></div>

</div>

<div class="section" id="s15">
  <div class="section-num" title="Walk-forward predictive power: forward accuracy and covariance realism as the fit arc grows.">4.2</div>
  <div class="section-title" title="Predictive power — forward accuracy and covariance realism as the fit arc grows">Predictive power</div>
  {predictive_power_html}
</div>

<div class="part-head" id="appendix">{appendix_head}</div>

<div class="section" id="s01">
  <div class="section-num">A.1</div>
  <div class="section-title" title="What this run holds">Run summary</div>
  <div class="grid-summary run-counts" title="by population — {pop_counts_line} · by channel — {channel_counts_line}">{n_objects} objects · {n_populations} populations · {n_dt} time offsets · {n_prop} propagation · {n_eph} ephemeris · {n_od} OD · {n_channels} channels</div>
  <div class="heatmap-container">
  <table class="agrid list" data-grid="list" style="max-width:720px; margin-bottom:6px;">
    <thead><tr><th style="text-align:left">Component</th><th style="text-align:left">Purpose</th><th>Version</th></tr></thead>
    <tbody>
      <tr><td style="text-align:left">hyperjet</td><td style="text-align:left">Automatic differentiation &mdash; STMs / STTs</td><td>1.15.0</td></tr>
      <tr><td style="text-align:left">empyrean-core</td><td style="text-align:left">Reference channel (<code>validate-core</code>)</td><td>0.10.2</td></tr>
      <tr><td style="text-align:left">empyrean</td><td style="text-align:left">Distribution under test &mdash; Rust wrapper, C ABI, Python wheel, CLI</td><td>{empyrean_version}</td></tr>
      <tr><td style="text-align:left">empyrean-py</td><td style="text-align:left">Python wheel</td><td>0.10.0</td></tr>
      <tr><td style="text-align:left">empyrean-cli</td><td style="text-align:left">Command-line interface</td><td>0.10.0</td></tr>
      <tr><td style="text-align:left">C ABI</td><td style="text-align:left"><code>EMPYREAN_ABI_VERSION</code> &mdash; equality-checked at load</td><td>1000</td></tr>
    </tbody>
  </table>
  </div>
</div>
<div class="section" id="s01b">
  <div class="section-num">A.2</div>
  <div class="section-title" title="External reference tools — transparency">External tools</div>
  <div class="disclosures"><details><summary>{disc_summary}</summary><div class="disc-body">Every external comparison is driven by a runner script in <a href="https://github.com/Empyrean-Dynamics/empyrean-validation" target="_blank" rel="noopener">empyrean-validation</a> holding that tool's full invocation &mdash; settings, force model, weighting, rejection &mdash; so any panel number reproduces. The tools are external dependencies, not linked into empyrean: run independently, their JSON outputs merged at the comparison step.</div></details></div>
  <div class="heatmap-container">
  <table class="agrid list" data-grid="list" style="margin-top:0.5em">
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
        <td><b>ASSIST</b></td>
        <td>Holman et al. 2023 · REBOUND IAS15 · DE440</td>
        <td><a href="https://github.com/Empyrean-Dynamics/empyrean-validation/blob/main/runners/assist/run_assist.py" target="_blank" rel="noopener"><code>runners/assist/run_assist.py</code></a></td>
      </tr>
      <tr>
        <td><b title="Used for: orbit determination from ADES; reference for reduced χ² (Veres-2017 weighting)">layup</b></td>
        <td>Holman / Smithsonian · MIT · ASSIST-backed</td>
        <td><a href="https://github.com/Empyrean-Dynamics/empyrean-validation/blob/main/runners/layup/run_layup.py" target="_blank" rel="noopener"><code>runners/layup/run_layup.py</code></a></td>
      </tr>
      <tr>
        <td><b>find_orb</b></td>
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
      {a2_absent_line}
    </tbody>
  </table>
  </div>
</div>

{configurations_section}

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
// The Part 4.2 per-object timeline sidecar descriptor (file, bytes, rows, hash),
// or null when no walk aggregate was supplied. Verified exactly like DATASET
// before any 4.2F chart draws; the server-rendered 4.2 tables render regardless.
const PREDICT_DESCRIPTOR = PREDICT_DESCRIPTOR_JSON;
// The (tool, arm) series the whole of Part 4.2 keys on, computed server-side, so
// the one-object timeline draws the same primary series the surfaces and matrices
// do rather than whichever walk happens to sit first in the sidecar. `null` when
// no walk aggregate was supplied.
const PW_PRIMARY = PW_PRIMARY_JSON;
let predictWindows = null;   // installed only after the sidecar verifies
const popColors = POP_COLORS_JSON;
// S4 text-to-marks: the error-growth charts draw every population in ONE hue
// (the 13-hue population palette is gone); a line is read from its direct
// end-label and the hover names the focused line.
const POP_HUE = '#5b9bd5';
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

// The compared pair is stated by the sticky Compare bar's two selects (each in
// its tool colour); the section titles carry no pair chip, so there is no
// per-title pair text to repaint on a pair change.

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
        // S6: observe the Part 1 slot anchors (A·B·C) as well as the sections,
        // so the generated rail sub-entries light up as the reader scrolls.
        document.querySelectorAll('.section[id], .spy-anchor[id]').forEach(s => obs.observe(s));
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
    // σ view: NOT SHIPPED. A coherent Mahalanobis view has to switch the key, the
    // worst column, the summary line and every hover to σ together with the
    // cells; the agreement heatmap carries only a one-decimal σ span and a bin
    // per cell, not the per-object σ those elements need, and rebuilding it from
    // the dataset is out of scope for this pass. Rather than ship the half-built
    // view the reviewer flagged (nearly every cell reading "0.0σ" in the lowest
    // tint under a metre key), the masthead carries no σ-view control until the
    // view works; the frozen protected markup (data-sbin / cn-sig) is left untouched.
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
    // S5 item 1: the bar IS the Part 1 head, so the pin test reads the bar's own
    // top (there is no separate #part1 head element any more).
    const p1 = bar;
    const p2 = document.getElementById('part2');
    if (!bar || !p2) return;
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
    // The always-on toolbar reads the page's muted text colour so it is legible in
    // either theme; rethemeCharts re-colours it on a flip. Every call site spreads
    // baseLayout, so this one definition colours every chart's toolbar.
    get modebar() {{ return {{ orientation: 'h', color: themeVar('--ed-text-muted'), activecolor: themeVar('--ed-text-secondary'), bgcolor: 'rgba(0,0,0,0)' }}; }},
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
function ax(title, type) {{
    const a = {{ title: {{ text: title, font: {{ size: 10 }} }}, gridcolor: themeVar('--ed-border'), zerolinecolor: themeVar('--ed-border'), color: themeVar('--ed-text-muted'), exponentformat: 'power' }};
    if (type) a.type = type;
    return a;
}}
// Re-colour every drawn chart after the theme flips: surfaces, text and every
// axis the chart has (subplots carry xaxis2, yaxis2, ...).
function rethemeCharts() {{
    if (!window.Plotly) return;
    document.querySelectorAll('.js-plotly-plot').forEach(gd => {{
        const upd = {{ paper_bgcolor: themeVar('--ed-bg'), plot_bgcolor: themeVar('--ed-bg'), 'font.color': themeVar('--ed-text-muted'), 'title.font.color': themeVar('--ed-text-secondary'), 'modebar.color': themeVar('--ed-text-muted'), 'modebar.activecolor': themeVar('--ed-text-secondary') }};
        for (const k of Object.keys(gd.layout || {{}})) {{
            if (!/^[xy]axis\d*$/.test(k)) continue;
            upd[k + '.color'] = themeVar('--ed-text-muted');
            upd[k + '.zerolinecolor'] = themeVar('--ed-border');
            upd[k + '.gridcolor'] = themeVar('--ed-border');
        }}
        try {{ Plotly.relayout(gd, upd); }} catch (e) {{ console.error('retheme failed for', gd.id, e); }}
    }});
}}

// ─────────── one shared Plotly config + branded PNG export ───────────
// Every Plotly.react / Plotly.newPlot call on the page passes this one config
// object, so the toolbar and its behaviour cannot drift between call sites. The
// toolbar is always visible (not hover-gated); its buttons are our branded PNG
// download, zoom, pan and reset-axes only — Plotly's own camera, box/lasso
// select, incremental zoom, autoscale, spike-lines, hover-mode toggles and logo
// are all removed. The toolbar colour comes from baseLayout.modebar (theme-aware).
// The logo data URI, the attribution and the run date are built server-side and
// emitted here; nothing of the stamp appears as visible page text.
const EMP_LOGO_URI = EMPYREAN_LOGO_URI_B64;
const EMP_ATTRIBUTION = EMPYREAN_EXPORT_ATTRIBUTION;
const EMP_REPORT_DATE = EMPYREAN_EXPORT_DATE;
function empSlug(s) {{
    return (s || '').toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '') || 'chart';
}}
// A heading's text with the pair-chip selector echo removed: the chip names the
// live comparison pair and is appended to the caption on its own, so its words
// are not part of the title (without this the Part 1 titles read "Error
// growthempyrean vs JPL").
function empTitleText(el) {{
    if (!el) return '';
    let s = '';
    el.childNodes.forEach(n => {{
        if (n.nodeType === 1 && n.classList && n.classList.contains('pair-chip')) return;
        s += n.textContent || '';
    }});
    return s.trim();
}}
// The chart's identity, read at click time; drives the caption and filename.
// Number: the section number. Title: the chart's own Plotly title when it set
// one (the covariance panels do) — read from `layout`, never `_fullLayout`,
// which Plotly fills with its "Click to enter…" placeholder — else the nearest
// heading above the chart in document order (a panel title if one precedes it,
// else the section title). Pair: appended only for a pair chart, detected by a
// Part 1 section number ("1." prefix) — every Part 1 chart compares the pair the
// sticky Compare bar's selects name (TOOL1/TOOL2). `#part1` is a sibling part-head,
// never an ancestor, so ancestry cannot detect a Part 1 chart.
function empChartIdentity(gd) {{
    const sec = gd.closest ? gd.closest('.section') : null;
    let num = '';
    if (sec) {{ const ns = sec.querySelector('.section-num'); if (ns) num = ns.textContent.trim(); }}
    let title = '';
    const lay = gd.layout || {{}};
    const gt = (lay.title && lay.title.text) ? String(lay.title.text).trim() : '';
    if (gt) {{
        title = gt;
    }} else if (sec) {{
        const heads = sec.querySelectorAll('.panel-title, .section-title');
        let chosen = null;
        heads.forEach(h => {{ if (h.compareDocumentPosition(gd) & Node.DOCUMENT_POSITION_FOLLOWING) chosen = h; }});
        title = empTitleText(chosen);
    }}
    let pair = '';
    if (sec && num.indexOf('1.') === 0) {{
        try {{ pair = toolLabel(TOOL1) + ' vs ' + toolLabel(TOOL2); }} catch (e) {{ pair = ''; }}
    }} else if (gd && gd.id === 'pw-timeline-chart') {{
        // The 4.2F walk timeline is per-object, not per-pair: its variant is the
        // selected object, so the stamp and filename name which object the PNG is
        // (otherwise every object exported as the same '…_none_' file).
        const os = document.getElementById('pw-timeline-object');
        if (os && os.value) pair = os.value;
    }}
    const caption = [num, title, pair].filter(Boolean).join(' · ');
    const filename = 'empyrean-validation_' + empSlug([num, title].filter(Boolean).join('-')) + '_'
        + (pair ? empSlug(pair) : 'none') + '_' + EMP_REPORT_DATE;
    return {{ caption: caption, filename: filename }};
}}
// The branded PNG export, wired as the toolbar's one download button. It grows
// the canvas by a footer band and lays the attribution (left) and the logo mark
// (right, square) across that band, below the x-axis title — which keeps its
// standoff just under the ticks — so the stamp never crosses the axis title.
// Colours come from the theme tokens read at click time. The figure's layout is
// restored exactly in a finally block (keys that were absent are cleared, not
// left as empty arrays), so the page itself never shows the stamp.
function empExportPng(gd) {{
    const id = empChartIdentity(gd);
    const layout = gd.layout || {{}};
    const hadImages = 'images' in layout;
    const hadAnnotations = 'annotations' in layout;
    const prevMargin = Object.assign({{}}, layout.margin || {{}});
    const prevImages = (layout.images || []).slice();
    const prevAnnotations = (layout.annotations || []).slice();
    const fl = gd._fullLayout || {{}};
    const flM = fl.margin || {{}};
    const W = fl.width || gd.clientWidth || 700;
    const H = fl.height || gd.clientHeight || 420;
    const baseB = prevMargin.b != null ? prevMargin.b : (flM.b != null ? flM.b : 50);
    const topM = prevMargin.t != null ? prevMargin.t : (flM.t != null ? flM.t : 50);
    const leftM = prevMargin.l != null ? prevMargin.l : (flM.l != null ? flM.l : 60);
    const rightM = prevMargin.r != null ? prevMargin.r : (flM.r != null ? flM.r : 20);
    const band = 58;                       // footer height added below the plot (px)
    const newB = baseB + band;
    const exportH = H + band;              // grow the canvas; the plot area stays put
    const plotH = Math.max(exportH - topM - newB, 1);
    const plotW = Math.max(W - leftM - rightM, 1);
    const footShift = -(newB - 16);        // footer line ~16px above the figure bottom
    const logoPx = Math.min(band - 20, 34);
    const logo = {{ source: EMP_LOGO_URI, xref: 'paper', yref: 'paper', x: 1, y: footShift / plotH,
        xanchor: 'right', yanchor: 'middle', sizex: logoPx / plotW, sizey: logoPx / plotH,
        opacity: 0.9, layer: 'above' }};
    const caption = {{ text: EMP_ATTRIBUTION + (id.caption ? ' · ' + id.caption : ''),
        xref: 'paper', yref: 'paper', x: 0, y: 0, xanchor: 'left', yanchor: 'middle',
        yshift: footShift, showarrow: false,
        font: {{ family: 'JetBrains Mono', size: 9, color: themeVar('--ed-text-muted') }} }};
    const restore = {{ margin: prevMargin, images: hadImages ? prevImages : null, annotations: hadAnnotations ? prevAnnotations : null }};
    return Promise.resolve()
        .then(() => Plotly.relayout(gd, {{ images: prevImages.concat([logo]), annotations: prevAnnotations.concat([caption]), 'margin.b': newB }}))
        .then(() => Plotly.downloadImage(gd, {{ format: 'png', scale: 2, width: W, height: exportH, filename: id.filename }}))
        .catch(e => {{ console.error('branded export failed', e); }})
        .finally(() => Plotly.relayout(gd, restore));
}}
// S5 item 4: the modebar carries ONE button — the branded PNG export
// (empExportPng). `modeBarButtons` is an explicit single-group list, so no
// default zoom / pan / autoscale tools are added back.
const PLOTLY_CONFIG = {{
    responsive: true,
    displayModeBar: true,
    displaylogo: false,
    modeBarButtons: [[{{ name: 'downloadPng', title: 'Download PNG', icon: Plotly.Icons.camera, click: empExportPng }}]],
}};

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
renderMethodDisagreement();
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
        }}, PLOTLY_CONFIG);
        return;
    }}
    const val = opts.valFn;
    const dts = uniq(opts.base.map(r => r.dt_days)).sort((a, b) => a - b);
    const traces = [];
    const popLabels = [];
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
        // One hue for every population (the 13-hue palette is gone); the direct
        // end-label names the line, the hover focuses it.
        const color = POP_HUE;
        // The population's 25-75 percent band under its median line.
        traces.push({{ x: bandX, y: bandY,
            fill: 'toself', fillcolor: color + '14', line: {{ color: 'rgba(0,0,0,0)' }},
            hoverinfo: 'skip', showlegend: false, legendgroup: pop, name: pop }});
        traces.push({{ x: xs, y: ys, mode: 'lines+markers', line: {{ color, width: 1.5 }}, marker: {{ size: MARKER }}, opacity: 0.85, name: pop, legendgroup: pop, showlegend: false, hovertemplate: `${{pop}} median<br>dt: %{{x}}d<br>%{{y:${{opts.fmt}}}} ${{opts.unit}}<extra></extra>` }});
        // The line's direct end-label, at its last drawn point, in population order.
        {{ let li = ys.length - 1; while (li >= 0 && ys[li] == null) li--; if (li >= 0) popLabels.push({{ x: xs[li], y: ys[li], text: pop }}); }}
    }}
    if (opts.extra) {{ for (const t of opts.extra(dts)) traces.push(t); }}
    const refAnnos = [];
    if (opts.refLines) {{
        const xMin = dts[0], xMax = dts[dts.length - 1];
        for (const [y, label] of opts.refLines) {{
            traces.push({{ x: [xMin, xMax], y: [y, y], mode: 'lines', line: {{ color: '#5b9bd540', width: 1, dash: 'dot' }}, name: label, showlegend: false, hoverinfo: 'skip' }});
            // Gaia / CCD reference: a y-axis annotation, not a legend entry.
            refAnnos.push({{ x: 0, xref: 'paper', xanchor: 'left', y: Math.log10(y), yref: 'y', yanchor: 'bottom', text: label, showarrow: false, font: {{ size: 9, color: themeVar('--ed-text-muted') }}, bgcolor: 'rgba(0,0,0,0)' }});
        }}
    }}
    // Direct labels at the right end of every line, in population order.
    // S5 item 3: dodge the direct end labels vertically where lines converge.
    // Work in log space (the y axis is log): sort top-down and push any pair
    // closer than MINSEP apart, so no two labels overlap at a crossing.
    const MINSEP = 0.16;
    const dodged = popLabels.map(p => ({{ x: p.x, ly: Math.log10(p.y), text: p.text }})).sort((a, b) => b.ly - a.ly);
    for (let i = 1; i < dodged.length; i++) {{ if (dodged[i - 1].ly - dodged[i].ly < MINSEP) dodged[i].ly = dodged[i - 1].ly - MINSEP; }}
    const labelAnnos = dodged.map(p => ({{ x: p.x, xref: 'x', xanchor: 'left', xshift: 5, y: p.ly, yref: 'y', yanchor: 'middle', text: p.text, showarrow: false, font: {{ size: 9, color: POP_HUE }}, bgcolor: 'rgba(0,0,0,0)' }}));
    Plotly.react(elId, traces, {{
        ...baseLayout,
        margin: {{ ...baseLayout.margin, r: 92 }},
        xaxis: ax('dt (days from epoch)'),
        yaxis: ax(`|${{toolLabel(TOOL1)}} − ${{toolLabel(TOOL2)}}| ${{opts.quantity}} (${{opts.unit}})`, 'log'),
        showlegend: false,
        annotations: labelAnnos.concat(refAnnos),
    }}, PLOTLY_CONFIG);
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
    if (km == null) return '—';
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
            traces.push({{ x: dts, y: med, mode: 'lines+markers', name: 'ASSIST variational',
                line: {{ color: '#5b9bd5', width: 2 }}, marker: {{ color: '#5b9bd5', size: MARKER }}, text: nrows,
                hovertemplate: '|dt| %{{x}}d<br>median ‖Φ_emp−Φ_ast‖/‖Φ_emp‖ %{{y:.2e}}<br>%{{text}} rows<extra></extra>' }});
            traces.push({{ x: dts, y: p95, mode: 'lines', name: 'p95',
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
            if (dts.length) traces.push({{ x: dts, y: med, mode: 'lines+markers', name: 'finite difference',
                line: {{ color: '#e8a040', width: 2 }}, marker: {{ color: '#e8a040', size: MARKER, symbol: 'diamond' }}, text: nrows,
                hovertemplate: '|dt| %{{x}}d<br>median ‖Φ_emp−Φ_FD‖/‖Φ_FD‖ %{{y:.2e}}<br>%{{text}} bodies<extra></extra>' }});
        }}
        if (!traces.length) {{
            Plotly.react(el, [], {{ ...baseLayout, xaxis: {{ visible: false }}, yaxis: {{ visible: false }},
                annotations: [{{ text: 'No first-order cross-validation rows.', showarrow: false, font: {{ color: '#8b9198', size: 12 }}, x: 0.5, y: 0.5, xref: 'paper', yref: 'paper' }}] }}, PLOTLY_CONFIG);
            return;
        }}
        const dtsAll = Array.from(allDts).sort((a, b) => a - b);
        traces.push({{ x: dtsAll, y: dtsAll.map(() => FD_FLOOR), mode: 'lines', name: 'FD floor',
            line: {{ color: '#8b9198', width: 1.5, dash: 'dot' }}, hoverinfo: 'skip' }});
        // S5 item 3: no Plotly legend — each series carries a direct end label at
        // its last finite point, in the one shared key style.
        const d3Annos = traces.map(t => {{
            const xs = t.x, ys = t.y; let i = xs.length - 1;
            while (i >= 0 && (ys[i] == null || !isFinite(ys[i]) || ys[i] <= 0)) i--;
            return i >= 0 ? {{ x: xs[i], y: Math.log10(ys[i]), xref: 'x', yref: 'y', xanchor: 'left', xshift: 6, yanchor: 'middle', text: t.name, showarrow: false, font: {{ size: 9, color: (t.line && t.line.color) || '#8b9198' }} }} : null;
        }}).filter(Boolean);
        Plotly.react(el, traces, {{
            ...baseLayout,
            margin: {{ ...baseLayout.margin, r: 118 }},
            xaxis: ax('|dt| (days from epoch)'),
            yaxis: ax('rel. Frobenius difference (log)', 'log'),
            showlegend: false,
            annotations: d3Annos,
        }}, PLOTLY_CONFIG);
    }})();
}}

// S5a: the ASSIST six-arm section is now server-rendered as the
// "Configurations" appendix (A.3); see render_configurations_section().

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
    // only — not across the first_order_detection_on / none_detection_on rows
    // that eph is doubled over (same RA/Dec but must stay distinct rows).
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
    }}, PLOTLY_CONFIG);
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
    // Summary-line runaway count (S3 item 4): write the deduped number of rows
    // this fold lists into the selected pair's §1.2A summary line, so the count
    // and the itemised list come from the same source and cannot disagree.
    const rwn = document.querySelector(`.pair-panels[data-section="h2-eph"] .pair-panel[data-tool="${{TOOL1}}"][data-ref="${{TOOL2}}"] .rwn`);
    if (rwn) rwn.textContent = seenOut.size + ' runaway';
    }}
    buildEphScatter();
    onToolChange(() => buildEphScatter());
}}

// ─────────── Section 09b: Non-gravitational recovery ───────────
// Rows tagged `non_grav_recovery` carry the fitted Marsden A1/A2/A3 and
// their 1σ (√ of the 9×9 state+A1/A2/A3 covariance diagonal). The σ are
// `None` (not 0, not NaN) when the StateAndNonGrav fit did not actually
// recover non-grav — the loud-failure case this section exists to catch.
// S3 item 3: one row per (object, coefficient) stating empyrean's (rust) fit;
// the rust/cli/python channels collapse into one cell-by-cell agreement column
// and the absent C ABI channel `c` is one hatched column (not a hatched row per
// object × coefficient), so an FFI drop still shows at once.
const ngResults = results.filter(r => r.test_type === 'non_grav_recovery');
if (ngResults.length === 0) {{
    document.getElementById('ng-empty').style.display = '';
    document.getElementById('ng-content').style.display = 'none';
}} else {{
    document.getElementById('ng-empty').style.display = 'none';
    // Group by object, then channel.
    const byObjNg = {{}};
    for (const r of ngResults) {{
        if (!byObjNg[r.object]) byObjNg[r.object] = {{}};
        byObjNg[r.object][r.channel] = r;
    }}
    const ngObjects = Object.keys(byObjNg).sort((a, b) => a.localeCompare(b));
    const NG_CHANNELS = ['rust', 'c', 'cli', 'python'];
    const overview = document.getElementById('ng-overview');
    // Compact scientific formatter for AU/day² values, which run ~1e-14.
    const fmtNg = (v) => (v == null || !isFinite(v)) ? null : v.toExponential(3);
    const COEFFS = [
        {{ key: 'a1', label: 'A1' }},
        {{ key: 'a2', label: 'A2' }},
        {{ key: 'a3', label: 'A3' }},
    ];
    // A channel row has a usable non-grav fit for this coefficient when both the
    // fitted value and its σ are finite (a None/NaN σ is the loud fall-back).
    const ngFitOk = (r, key) => r != null && r['od_' + key] != null && isFinite(r['od_' + key]) && r['od_' + key + '_sigma'] != null && isFinite(r['od_' + key + '_sigma']);
    const ngPass = (r, key, ic) => {{
        if (!ngFitOk(r, key)) return 'fail';
        if (ic == null || ic === 0) return 'na';
        return Math.abs((r['od_' + key] - ic) / r['od_' + key + '_sigma']) <= 3 ? 'pass' : 'fail';
    }};
    // A channel's comparable signature for a coefficient: the fitted value to
    // displayed precision plus the verdict, or `absent`/`fail` — two channels
    // agree iff their signatures match.
    const ngSig = (r, key, ic) => {{
        if (r == null) return 'absent';
        if (!ngFitOk(r, key)) return 'fail';
        return fmtNg(r['od_' + key]) + '|' + ngPass(r, key, ic);
    }};
    for (const obj of ngObjects) {{
        const row = byObjNg[obj];
        const refRow = NG_CHANNELS.map(ch => row[ch]).find(r => r) || {{}};
        const activeCoeffs = COEFFS.filter(c => {{
            const ic = refRow['ic_' + c.key];
            return ic != null && ic !== 0;
        }});
        const coeffs = activeCoeffs.length > 0 ? activeCoeffs : COEFFS;
        let firstCellEmitted = false;
        for (const c of coeffs) {{
            const key = c.key;
            const ic = refRow['ic_' + key];
            const rust = row['rust'];
            const jplCell = (ic != null) ? fmtNg(ic) : '—';
            const tr = document.createElement('tr');
            let cells = '';
            if (!firstCellEmitted) {{
                cells += `<td class="obj" rowspan="${{coeffs.length}}" style="vertical-align:top">${{obj}}</td>`;
                firstCellEmitted = true;
            }}
            cells += `<td style="text-align:left">${{c.label}}</td>`;
            // Fitted ± σ and z from the rust reference (empyrean's own fit).
            let fittedCell, zCell;
            if (rust == null) {{
                fittedCell = `<td class="hatch" title="rust (the engine) emitted no non-grav row for this object — a regression"></td>`;
                zCell = `<td class="hatch" title="no rust fit to test"></td>`;
            }} else if (!ngFitOk(rust, key)) {{
                // Loud fall-back: a None σ means the fit did not recover non-grav.
                fittedCell = `<td style="color:#e06252; font-weight:600" title="fail · no non-grav covariance (fell back to state-only)">✕</td>`;
                zCell = `<td style="color:#e06252; font-weight:600" title="fail · no non-grav covariance (fell back to state-only)">✕</td>`;
            }} else {{
                const fit = rust['od_' + key], sig = rust['od_' + key + '_sigma'];
                fittedCell = `<td style="text-align:left">${{fmtNg(fit)}} <span style="color:#8b9198">± ${{fmtNg(sig)}}</span></td>`;
                if (ic == null || ic === 0) {{
                    zCell = `<td class="hatch" title="n/a · no non-zero JPL reference for this coefficient to test against"></td>`;
                }} else {{
                    const z = (fit - ic) / sig, zAbs = Math.abs(z), pass = zAbs <= 3;
                    const zColor = pass ? '#3d9a6d' : '#e06252';
                    zCell = `<td style="color:${{zColor}}; font-weight:600" title="${{pass ? 'pass · |z| ≤ 3σ' : 'fail · |z| > 3σ'}}">${{z >= 0 ? '+' : '−'}}${{zAbs.toFixed(2)}}σ</td>`;
                }}
            }}
            cells += fittedCell;
            cells += `<td style="text-align:left">${{jplCell}}</td>`;
            cells += zCell;
            // channels: rust/cli/python collapse — a mark when they agree, else
            // the differing channel named (absent counts as a difference).
            let channelsCell;
            if (rust == null) {{
                channelsCell = `<td class="hatch" title="rust absent — no reference to agree against"></td>`;
            }} else {{
                const refSig = ngSig(rust, key, ic);
                const diffs = [];
                for (const ch of ['cli', 'python']) {{
                    const r2 = row[ch];
                    if (r2 == null) {{ diffs.push(ch + ' absent'); continue; }}
                    if (ngSig(r2, key, ic) !== refSig) diffs.push(ch);
                }}
                channelsCell = diffs.length === 0
                    ? `<td style="color:#3d9a6d" title="rust, cli, python agree on fit and verdict">·</td>`
                    : `<td style="color:#e06252" title="differ from rust: ${{diffs.join(', ')}}">${{diffs.join(', ')}}</td>`;
            }}
            cells += channelsCell;
            // c (C ABI): one hatched column when absent; otherwise its agreement.
            const cRow = row['c'];
            let cCell;
            if (cRow == null) {{
                cCell = `<td class="hatch" title="c (C ABI) emitted no non-grav row this run — the FFI block is unvalidated"></td>`;
            }} else if (rust != null && ngSig(cRow, key, ic) === ngSig(rust, key, ic)) {{
                cCell = `<td style="color:#3d9a6d" title="c agrees with rust">·</td>`;
            }} else {{
                cCell = `<td style="color:#e06252" title="c differs from rust">&ne;</td>`;
            }}
            cells += cCell;
            tr.innerHTML = cells;
            overview.appendChild(tr);
        }}
    }}
}}

// ─────────── Section 12: Fitted orbit + covariance vs references ───────────
const orbitComparisons = ORBIT_COMPARISONS_JSON;

// ─────────── cross-method disagreement: σ per method ÷ first-order ───────────
// The per-(object, method) σ series, built Rust-side off each row's published
// covariance (never recomputed here). Each method is a distinct colour AND
// marker symbol, keyed on the raw method prefix in plan order so the series
// cannot collapse to one colour. It is a diagnostic: no tolerance, no verdict.
const disagreementData = DISAGREEMENT_JSON;
const METHOD_ORDER = ['none', 'first_order', 'second_order', 'auto', 'sigma_point', 'monte_carlo', 'gaussian_mixture'];
const METHOD_STYLE = {{
    none:             {{ color: '#8b9198', symbol: 'circle' }},
    first_order:      {{ color: '#5b9bd5', symbol: 'square' }},
    second_order:     {{ color: '#d05080', symbol: 'diamond' }},
    auto:             {{ color: '#a070d0', symbol: 'cross' }},
    sigma_point:      {{ color: '#40c0c0', symbol: 'triangle-up' }},
    monte_carlo:      {{ color: '#e8a040', symbol: 'star' }},
    gaussian_mixture: {{ color: '#6fcf6f', symbol: 'hexagon' }},
}};
// Client renderers for the two by-object views that live in the sidecar, not in
// the page bytes (per-method matrix in 3.1 A, outcome-and-tallies in 3.1 C).
METHOD_RENDER_JS
function renderMethodDisagreement() {{
    const host = document.getElementById('method-disagreement-plots');
    if (!host || !window.Plotly) return;
    const data = disagreementData || [];
    const QUANT = [
        {{ key: 'sigma_ra_arcsec', label: 'σ RA·cosδ' }},
        {{ key: 'sigma_dec_arcsec', label: 'σ Dec' }},
        {{ key: 'sigma_pos_km', label: 'σ pos' }},
    ];
    const byObj = new Map();
    for (const p of data) {{
        if (!byObj.has(p.object)) byObj.set(p.object, new Map());
        byObj.get(p.object).set(p.method, p);
    }}
    host.innerHTML = '';
    let drawn = 0;
    for (const obj of [...byObj.keys()].sort((a, b) => a.localeCompare(b))) {{
        const methods = byObj.get(obj);
        const base = methods.get('first_order');
        const withSigma = METHOD_ORDER.filter(m => {{
            const p = methods.get(m);
            return p && QUANT.some(q => p[q.key] != null);
        }});
        if (!base || withSigma.length < 2) continue;
        const traces = [];
        for (const m of withSigma) {{
            const p = methods.get(m);
            const st = METHOD_STYLE[m] || {{ color: '#8b9198', symbol: 'circle' }};
            const xs = [], ys = [], txt = [];
            for (const q of QUANT) {{
                const v = p[q.key], b = base[q.key];
                if (v == null || b == null || b === 0) continue;
                xs.push(q.label); ys.push(v / b);
                txt.push(`${{m}} · ${{q.label}} = ${{v.toExponential(2)}} (×${{(v / b).toPrecision(3)}} vs first-order)`);
            }}
            if (!xs.length) continue;
            traces.push({{ type: 'scatter', mode: 'markers', name: m, x: xs, y: ys, text: txt, hoverinfo: 'text',
                marker: {{ color: st.color, symbol: st.symbol, size: 11, line: {{ width: 1, color: st.color }} }} }});
        }}
        if (!traces.length) continue;
        const div = document.createElement('div');
        div.className = 'chart-container';
        div.style.marginBottom = '10px';
        div.id = 'mdis-' + drawn;
        host.appendChild(div);
        Plotly.newPlot(div.id, traces, {{ ...baseLayout, title: {{ text: obj + ' — σ per method ÷ first-order', font: {{ size: 11, color: themeVar('--ed-text-secondary') }}, x: 0.02 }},
            height: 300, margin: {{ l: 56, r: 12, t: 30, b: 42 }}, xaxis: ax(''), yaxis: ax('σ ÷ first-order', 'log'),
            legend: LEGEND_H, shapes: [{{ type: 'line', x0: -0.5, x1: QUANT.length - 0.5, y0: 1, y1: 1, line: {{ color: '#8b9198', dash: 'dash', width: 1 }} }}] }}, PLOTLY_CONFIG);
        drawn++;
    }}
    if (drawn === 0) {{
        host.innerHTML = '<div class="section-desc" style="color:#8b9198">No object carries first-order plus a second method with a delivered σ at this pin — the cross-method disagreement view fills in when the sampled and mixture covariances are produced (not produced at this pin).</div>';
    }}
}}
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
    // S5 item 3: no Plotly legend — the two marker series carry a direct label.
    const chiLast = objs1.length - 1;
    const chiAnnos = chiLast >= 0 ? [
        {{ x: chiLast, y: Math.log10(empChi[objs1[chiLast]]), xref: 'x', yref: 'y', xanchor: 'left', xshift: 6, yanchor: 'middle', text: 'empyrean', showarrow: false, font: {{ size: 9, color: '#5b9bd5' }} }},
        {{ x: chiLast, y: Math.log10(jplChi[objs1[chiLast]]), xref: 'x', yref: 'y', xanchor: 'left', xshift: 6, yanchor: 'middle', text: 'JPL', showarrow: false, font: {{ size: 9, color: '#d05080' }} }},
    ] : [];
    Plotly.react('cov-chi2-panel', t1, {{ ...baseLayout, title: covTitle('1 · reduced χ² — empyrean paired to JPL'), margin: {{ l: 46, r: 72, t: 28, b: 58 }}, xaxis: {{ ...ax('object (sorted by empyrean)'), showticklabels: false }}, yaxis: ax('χ²ᵣ', 'log'), shapes: [{{ type: 'line', x0: -0.5, x1: Math.max(objs1.length - 0.5, 0.5), y0: 1, y1: 1, line: {{ color: '#8b9198', dash: 'dash', width: 1 }} }}], showlegend: false, annotations: chiAnnos }}, PLOTLY_CONFIG);
    // Panel 2 — σ ratio vs observed arc, coloured by population.
    const byPop = {{}};
    for (const c of recs) {{ if (radarExcluded.has(c.object)) continue; const rr = c.sigma_fit[0] / c.sigma_ref[0]; const a = arc[c.object]; if (!isFinite(rr) || a == null) continue; const p = popOf[c.object] || '?'; (byPop[p] = byPop[p] || []).push([a, rr, c.object]); }}
    const t2 = [];
    // S5 item 3: the σ-ratio scatter is ONE hue (the 13-hue population palette is
    // gone); the population is read from the hover, which focuses the point.
    for (const p in byPop) {{ const pts = byPop[p]; t2.push({{ x: pts.map(q => q[0]), y: pts.map(q => q[1]), mode: 'markers', marker: {{ color: POP_HUE, size: 7 }}, name: p, text: pts.map(q => q[2]), hovertemplate: '%{{text}} (' + p + ')<br>arc %{{x}} d<br>σ ratio %{{y:.2f}}<extra></extra>' }}); }}
    const allArc = recs.map(c => arc[c.object]).filter(a => a != null);
    const amin = allArc.length ? Math.min(...allArc) : 1, amax = allArc.length ? Math.max(...allArc) : 1e5;
    Plotly.react('cov-ratio-panel', t2, {{ ...baseLayout, title: covTitle('2 · σ_emp(a) / σ_JPL(a) vs observed arc'), margin: {{ l: 46, r: 8, t: 28, b: 36 }}, xaxis: ax('observed arc (days)', 'log'), yaxis: ax('σ ratio (a element)', 'log'), shapes: [{{ type: 'line', x0: amin, x1: amax, y0: 1, y1: 1, line: {{ color: '#8b9198', dash: 'dash', width: 1 }} }}], showlegend: false }}, PLOTLY_CONFIG);
    // Panel 3 — σ_eq empirical CDF vs χ(6)/√6 expected.
    const sig = recs.filter(c => !radarExcluded.has(c.object)).map(c => c.sigma_equiv_combined).filter(v => v != null && isFinite(v)).sort((a, b) => a - b);
    const n = sig.length;
    const obsY = sig.map((v, i) => (i + 1) / n);
    const ex = [], ey = [];
    for (let lt = -1.1; lt <= 3.1; lt += 0.05) {{ const t = Math.pow(10, lt); ex.push(t); ey.push(1 - Math.exp(-3 * t * t) * (1 + 3 * t * t + 4.5 * t * t * t * t)); }}
    // S5 item 3: no Plotly legend — each CDF line carries a direct end label.
    const cdfAnnos = [];
    if (ex.length) cdfAnnos.push({{ x: Math.log10(ex[ex.length - 1]), y: ey[ey.length - 1], xref: 'x', yref: 'y', xanchor: 'right', yanchor: 'bottom', text: 'expected', showarrow: false, font: {{ size: 9, color: '#8b9198' }} }});
    if (sig.length) cdfAnnos.push({{ x: Math.log10(sig[sig.length - 1]), y: obsY[obsY.length - 1], xref: 'x', yref: 'y', xanchor: 'left', xshift: 6, yanchor: 'top', text: `observed (n=${{n}})`, showarrow: false, font: {{ size: 9, color: '#5b9bd5' }} }});
    Plotly.react('cov-cdf-panel', [
        {{ x: ex, y: ey, mode: 'lines', line: {{ color: '#8b9198', width: 2 }}, name: 'expected χ(6)/√6' }},
        {{ x: sig, y: obsY, mode: 'lines', line: {{ color: '#5b9bd5', width: 2 }}, name: `observed (n=${{n}})` }},
    ], {{ ...baseLayout, title: covTitle('3 · σ_eq empirical CDF vs expected'), margin: {{ l: 46, r: 88, t: 28, b: 64 }}, xaxis: ax('σ_eq', 'log'), yaxis: ax('cumulative fraction'), showlegend: false, annotations: cdfAnnos }}, PLOTLY_CONFIG);
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
    'cov-chi2-panel', 'cov-ratio-panel', 'cov-cdf-panel', 'ng-overview'];
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
// One banner definition, shared by the dataset load and the Part 4.2 timeline so
// neither re-declares the sticky error chrome. Each owns its own element id, so a
// dataset-load success hiding its banner never hides the timeline's, and vice-versa.
function banner(id, zpri, html, kind) {{
    let b = document.getElementById(id);
    if (!b) {{
        b = document.createElement('div');
        b.id = id;
        b.style.cssText = `position:sticky;top:0;z-index:${{zpri}};padding:10px 16px;font-size:13px;font-family:JetBrains Mono,monospace;line-height:1.4;`;
        document.body.insertBefore(b, document.body.firstChild);
    }}
    b.style.background = kind === 'error' ? '#4a1418' : '#12303a';
    b.style.color = kind === 'error' ? '#ffb3b3' : '#bfe3ef';
    b.style.borderBottom = kind === 'error' ? '2px solid #e06252' : '2px solid #3a7d95';
    b.innerHTML = html;
    b.hidden = false;
    return b;
}}
function datasetBanner(html, kind) {{ return banner('dataset-banner', 9999, html, kind); }}
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

// ─────────── Part 4.2 predictive-power: surface selector + timeline sidecar ──
// The surface selector (4.2A) needs no fetched data — it toggles pre-rendered
// server panels. Only the per-object timeline (4.2F) reads the second sidecar,
// verified byte-length + FNV exactly like the dataset before anything draws; the
// server-rendered 4.2 tables stay visible in every load state.
// 4.2 Horizon marginals metric toggle: one number per cell; the selector
// (accuracy | realism joint | along-track | cross-track) swaps the shown value
// and its colour class, the other three stay on the cell hover. Hatch cells
// carry no data-* and are skipped.
function wireMarginalMetric() {{
    const sel = document.getElementById('pw-marg-metric');
    if (!sel) return;
    const cells = Array.from(document.querySelectorAll('td.pw-mcell'));
    function apply() {{
        const m = sel.value;
        cells.forEach(td => {{
            const v = td.dataset[m];
            const cls = td.dataset[m + 'Cls'];
            if (v === undefined) return;
            td.className = 'gc pw-mcell ' + cls;
            const span = td.querySelector('.cn');
            if (span) span.textContent = v;
        }});
    }}
    sel.addEventListener('change', apply);
    apply();
}}
// 4.2A coverage direction toggle: one matrix (joint | along-track | cross-track)
// visible at a time, joint the default. Pure show/hide of pre-rendered server
// tables, no fetched data.
function wireCoverageDirection() {{
    const sel = document.getElementById('pw-cov-dir');
    if (!sel) return;
    const mats = Array.from(document.querySelectorAll('.pw-cov-mat'));
    function apply() {{
        mats.forEach(m => {{ m.hidden = m.dataset.dir !== sel.value; }});
    }}
    sel.addEventListener('change', apply);
    apply();
}}
function wirePredictiveSurfaces() {{
    const cls = document.getElementById('pw-class');
    const tool = document.getElementById('pw-tool');
    const arm = document.getElementById('pw-arm');
    if (!cls || !tool || !arm) return;
    const na = document.querySelector('.pw-pair-na');
    // Arms present per tool, from the server-rendered pair panels. find_orb/layup
    // carry only their Monte-Carlo arms; the 36 rust arms are rust-only. Scoping
    // the arm selector to the chosen tool means every offered arm yields a surface,
    // never an all-hatched grid a reader would misread as a broken tool.
    const armsByTool = {{}};
    document.querySelectorAll('.pw-pair').forEach(p => {{
        (armsByTool[p.dataset.tool] = armsByTool[p.dataset.tool] || new Set()).add(p.dataset.arm);
    }});
    function repopulateArms() {{
        const avail = Array.from(armsByTool[tool.value] || []).sort();
        const cur = arm.value;
        arm.innerHTML = '';
        avail.forEach(a => {{ const o = document.createElement('option'); o.value = a; o.textContent = a; arm.appendChild(o); }});
        arm.value = avail.indexOf(cur) >= 0 ? cur : (avail[0] || '');
    }}
    function apply() {{
        let shown = false;
        document.querySelectorAll('.pw-pair').forEach(p => {{
            const m = p.dataset.class === cls.value && p.dataset.tool === tool.value && p.dataset.arm === arm.value;
            p.hidden = !m;
            if (m) shown = true;
        }});
        if (na) na.hidden = shown;
    }}
    cls.addEventListener('change', apply);
    tool.addEventListener('change', () => {{ repopulateArms(); apply(); }});
    arm.addEventListener('change', apply);
    repopulateArms();
    apply();
}}
function predictFail(msg) {{
    // A distinct class (not `dataset-pending`) so boot()'s pending-placeholder sweep
    // never removes this message, and its own banner (not the shared dataset banner)
    // so a later dataset-load success never hides this failure. The 4.2 tables are
    // server-rendered, so they stay complete whatever the timeline sidecar does.
    const el = document.getElementById('pw-timeline-chart');
    if (el) el.innerHTML = `<div class="predict-pending" style="${{DATASET_PENDING_STYLE}}">walk timeline not loaded — ${{msg}}</div>`;
    banner('predict-banner', 9998, '<b>Walk timeline not loaded.</b> ' + msg + ' The Part 4.2 tables above are server-rendered and complete.', 'error');
}}
async function loadPredict() {{
    // No walk aggregate in this report: 4.2F simply has nothing to draw, and the
    // server-rendered 4.2 tables carry the section on their own.
    if (!PREDICT_DESCRIPTOR) return;
    const d = PREDICT_DESCRIPTOR;
    let buf;
    try {{
        const resp = await fetch(d.file);
        if (!resp.ok) {{ predictFail(`HTTP ${{resp.status}} ${{resp.statusText || ''}} fetching ${{d.file}}.`); return; }}
        buf = await resp.arrayBuffer();
    }} catch (e) {{
        predictFail(`serve the folder over HTTP — a browser will not fetch ${{d.file}} from disk (file://). (${{e}})`);
        return;
    }}
    const bytes = new Uint8Array(buf);
    if (bytes.length !== d.bytes) {{ predictFail(`byte-length mismatch: expected ${{d.bytes}}, got ${{bytes.length}} — the sidecar belongs to a different run.`); return; }}
    if (fnv1a32(bytes) !== d.hash) {{ predictFail(`hash mismatch — the sidecar belongs to a different run.`); return; }}
    let parsed;
    try {{ parsed = JSON.parse(new TextDecoder('utf-8').decode(bytes)); }}
    catch (e) {{ predictFail(`the sidecar is not valid JSON (${{e}}).`); return; }}
    if (!parsed || !Array.isArray(parsed.per_window)) {{ predictFail(`the sidecar carries no per_window array.`); return; }}
    predictWindows = parsed.per_window;
    try {{ initWalkTimeline(); }}
    catch (e) {{ console.error('walk timeline failed', e); predictFail(`the sidecar verified but the timeline failed to render (${{e}}).`); }}
}}
function initWalkTimeline() {{
    const sel = document.getElementById('pw-timeline-object');
    if (!sel) return;
    sel.addEventListener('change', () => drawWalkTimeline(sel.value));
    if (sel.value) drawWalkTimeline(sel.value);
}}
// χ²₂ median and 95% point, normalized by 2 ln 2 so they sit on the same
// realism scale the surfaces use (1 = calibrated).
const PW_BAND_50 = 1.0, PW_BAND_95 = 5.991464547107979 / (2 * Math.LN2);
function drawWalkTimeline(object) {{
    const el = document.getElementById('pw-timeline-chart');
    if (!el || !window.Plotly || !predictWindows) return;
    const tl = (PW_PRIMARY && predictWindows.find(w => w.object === object && w.tool === PW_PRIMARY.tool && w.config_arm === PW_PRIMARY.arm))
            || predictWindows.find(w => w.object === object);
    if (!tl) {{
        Plotly.react(el, [], {{ ...baseLayout, xaxis: {{ visible: false }}, yaxis: {{ visible: false }},
            annotations: [{{ text: 'no walk for this object', showarrow: false, x: 0.5, y: 0.5, xref: 'paper', yref: 'paper', font: {{ color: themeVar('--ed-text-muted'), size: 12 }} }}] }}, PLOTLY_CONFIG);
        return;
    }}
    const x = tl.arc_days;
    const cAcc = themeVar('--ed-accent'), cSep = themeVar('--ed-text-secondary');
    const traces = [
        {{ x, y: tl.med_d2_norm, name: 'realism d²n', mode: 'lines+markers', marker: {{ size: MARKER }}, line: {{ color: cAcc }}, connectgaps: false, yaxis: 'y' }},
        {{ x, y: tl.med_sep_arcsec, name: 'sep (arcsec)', mode: 'lines+markers', marker: {{ size: MARKER }}, line: {{ color: cSep }}, connectgaps: false, yaxis: 'y2' }},
    ];
    // S8 item 4: the last Plotly legend is gone — each line is named directly at
    // its own end (both axes are log, so annotation data coords are log10), so the
    // dual-axis chart reads without a legend key.
    const lastFinite = ys => {{ for (let i = ys.length - 1; i >= 0; i--) {{ const v = ys[i]; if (v != null && isFinite(v) && v > 0 && x[i] > 0) return i; }} return -1; }};
    const annos = [];
    const iA = lastFinite(tl.med_d2_norm);
    if (iA >= 0) annos.push({{ x: Math.log10(x[iA]), xref: 'x', y: Math.log10(tl.med_d2_norm[iA]), yref: 'y', text: 'realism d²n', showarrow: false, xanchor: 'right', yanchor: 'bottom', font: {{ color: cAcc, size: 11 }} }});
    const iB = lastFinite(tl.med_sep_arcsec);
    if (iB >= 0) annos.push({{ x: Math.log10(x[iB]), xref: 'x', y: Math.log10(tl.med_sep_arcsec[iB]), yref: 'y2', text: 'sep (arcsec)', showarrow: false, xanchor: 'right', yanchor: 'top', font: {{ color: cSep, size: 11 }} }});
    const layout = {{ ...baseLayout,
        xaxis: ax('fit arc (days)', 'log'),
        yaxis: {{ ...ax('realism d²n', 'log') }},
        yaxis2: {{ ...ax('sep (arcsec)', 'log'), overlaying: 'y', side: 'right' }},
        showlegend: false,
        annotations: annos,
        shapes: [
            {{ type: 'line', xref: 'paper', x0: 0, x1: 1, yref: 'y', y0: PW_BAND_50, y1: PW_BAND_50, line: {{ color: themeVar('--ed-border'), dash: 'dot' }} }},
            {{ type: 'line', xref: 'paper', x0: 0, x1: 1, yref: 'y', y0: PW_BAND_95, y1: PW_BAND_95, line: {{ color: themeVar('--ed-border'), dash: 'dot' }} }},
        ],
    }};
    Plotly.react(el, traces, layout, PLOTLY_CONFIG);
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
try {{ wirePredictiveSurfaces(); }} catch (e) {{ console.error('wirePredictiveSurfaces failed', e); }}
try {{ wireCoverageDirection(); }} catch (e) {{ console.error('wireCoverageDirection failed', e); }}
try {{ wireMarginalMetric(); }} catch (e) {{ console.error('wireMarginalMetric failed', e); }}
loadDataset();
loadPredict();
</script>
</body>
</html>"##,
        brand_tokens_css = BRAND_TOKENS_CSS,
        ladder_css = ladder_css(),
        realism_css = realism_css(),
        tool_tokens_css = tool_color_tokens_css(),
        disc_summary = DISCLOSURE_SUMMARY,
        coeff_defs = NG_COEFF_DEFS,
        verdict_rule = NG_VERDICT_RULE,
        chart_h = FULL_CHART_HEIGHT_PX,
        rail_html = rail_html,
        part1_head = part1_head,
        part1_facts = part1_facts,
        part1_sections = part1_sections,
        part2_sections = part2_sections,
        part2_head = part_head_inner(PART_NAME_2),
        part3_head = part_head_inner(PART_NAME_3) + &part3_scope_html(results),
        part4_head = part_head_inner(PART_NAME_4),
        appendix_head = part_head_inner(PART_NAME_APPENDIX),
        dataset_file_name = data_file_name,
        n_prop = n_prop,
        n_eph = n_eph,
        n_od = n_od,
        n_objects = n_objects,
        n_channels = n_channels,
        n_populations = n_populations,
        n_dt = n_dt,
        pop_counts_line = pop_counts_line,
        channel_counts_line = channel_counts_line,
        a2_absent_line = a2_absent_line,
        h4_fidelity_grid_html = h4_fidelity_grid_html,
        method_matrix_html = method_matrix_html,
        method_outcome_html = method_outcome_html,
        covariance_realism_html = covariance_realism_html,
        predictive_power_html = predictive_power_html,
        provenance_footer_html = provenance_footer_html,
        configurations_section = configurations_section,
        empyrean_version = empyrean_version,
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

    // The Part 4.2 per-object timeline sidecar: the one large walk family, written
    // beside the dataset as `<stem>.predict.json` and verified by the page (byte
    // length + FNV) exactly like the dataset. Only `per_window` travels; the small
    // static families already rendered server-side. `null` descriptor when no walk
    // aggregate was supplied, so 4.2F simply has nothing to fetch.
    let predict_descriptor_json = match predict_agg {
        Some(agg) => {
            let predict_path = predict_path_for(output);
            let predict_file_name = predict_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .ok_or_else(|| {
                    format!(
                        "predict sidecar path has no file name: {}",
                        predict_path.display()
                    )
                })?;
            let sidecar = serde_json::json!({
                "snapshot_id": agg.snapshot_id,
                "profile": agg.profile,
                "per_window": agg.per_window,
            });
            let sidecar_json = serde_json::to_string(&sidecar)
                .map_err(|e| format!("serialize predict sidecar: {e}"))?;
            std::fs::write(&predict_path, sidecar_json.as_bytes()).map_err(|e| {
                format!(
                    "Failed to write predict sidecar {}: {e}",
                    predict_path.display()
                )
            })?;
            let descriptor = serde_json::json!({
                "file": predict_file_name,
                "bytes": sidecar_json.len(),
                "rows": agg.per_window.len(),
                "hash": fnv1a32_hex(sidecar_json.as_bytes()),
            });
            serde_json::to_string(&descriptor).unwrap_or_else(|_| "null".to_string())
        }
        None => "null".to_string(),
    };

    // The primary (tool, arm) series the whole of Part 4.2 keys on, computed
    // server-side so the client timeline draws the same series as the surfaces and
    // matrices. `null` when no walk aggregate was supplied.
    let pw_primary_json = match predict_agg {
        Some(agg) => {
            let (tool, arm) = pw_primary_series(agg);
            serde_json::json!({ "tool": tool, "arm": arm }).to_string()
        }
        None => "null".to_string(),
    };

    // The branded-export inputs the client script carries: the inlined logo data
    // URI, the server-built attribution, and the run date (for the export filename).
    // JSON-encoded so each lands as a correctly-quoted, escaped JS string literal.
    let logo_data_uri = format!("data:image/png;base64,{}", base64_encode(LOGO_BYTES));
    let logo_uri_json = serde_json::to_string(&logo_data_uri).unwrap_or_default();
    let export_attribution_json = serde_json::to_string(&export_attribution).unwrap_or_default();
    let report_date_json = serde_json::to_string(report_run_date).unwrap_or_default();
    let html = html.replace("EMPYREAN_LOGO_URI_B64", &logo_uri_json);
    let html = html.replace("EMPYREAN_EXPORT_ATTRIBUTION", &export_attribution_json);
    let html = html.replace("EMPYREAN_EXPORT_DATE", &report_date_json);
    let html = html.replace("DATASET_DESCRIPTOR_JSON", &dataset_descriptor_json);
    let html = html.replace("PREDICT_DESCRIPTOR_JSON", &predict_descriptor_json);
    let html = html.replace("PW_PRIMARY_JSON", &pw_primary_json);
    let html = html.replace("TOOLS_PRESENT_JSON", &tools_present_json);
    let html = html.replace("POP_COLORS_JSON", &pop_colors_json);
    let html = html.replace("CHANNEL_COLORS_JSON", &channel_colors_json);
    let html = html.replace("ORBIT_COMPARISONS_JSON", &orbit_comparisons_json);
    let html = html.replace("DISAGREEMENT_JSON", &disagreement_json);
    let html = html.replace("METHOD_RENDER_JS", METHOD_RENDER_JS);
    let html = html.replace("RADAR_EXCLUDED_JSON", &radar_excluded_json);

    std::fs::write(output, html).map_err(|e| format!("Failed to write report: {e}"))
}

/// The Part 4.2 timeline sidecar path for a report output: the output with its
/// extension replaced by `.predict.json`
/// (`validation_report.html` -> `validation_report.predict.json`). Mirrors
/// [`dataset_path_for`] so the two sidecars sit together beside the page.
fn predict_path_for(output: &Path) -> std::path::PathBuf {
    let mut name = output
        .file_stem()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    name.push(".predict.json");
    output.with_file_name(name)
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

/// Standard-alphabet base64 (RFC 4648, with padding). Used once, to inline the
/// masthead icon as a data URI for the branded chart export; a twenty-line
/// encoder is cheaper to own than a dependency pulled in for a single call.
/// Matches the published reference vectors ("" -> "", "f" -> "Zg==",
/// "foobar" -> "Zm9vYmFy").
fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
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
        generate_report(&rows, &[], &out, None, None, &[]).unwrap();
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
        generate_report(&rows, &cmps, &out, None, None, &[]).unwrap();
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
        let result = generate_report(&rows, &[], &out, None, None, &[]);
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
        let result = generate_report(&rows, &[], &html, Some(&summary), None, &[]);
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
        generate_report(&rows, &comps, &out, None, None, &[]).unwrap();
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
        generate_report(&rows, &[], &out, None, None, &[]).unwrap();
        std::fs::read_to_string(&out).unwrap()
    }

    /// Owner vocabulary rule (lang/design/validation-report-redesign.md,
    /// 2026-10-17): the page names derivative orders as "state only / first
    /// order / second order", never "Jet1", "Jet2", "6×6", "9×9", "STM" or
    /// "hyperdual" as a label. Crate and type names may remain only in the
    /// Configurations appendix (s01/s01b/s05c/s13), where a tool's version is
    /// stated; `<script>` / `<style>` are not reader-facing prose.
    #[test]
    fn vocabulary_no_banned_derivative_tokens_outside_appendix() {
        fn strip_tag(src: &str, tag: &str) -> String {
            let (open, close) = (format!("<{tag}"), format!("</{tag}>"));
            let mut out = String::new();
            let mut rest = src;
            while let Some(a) = rest.find(&open) {
                out.push_str(&rest[..a]);
                match rest[a..].find(&close) {
                    Some(rel) => rest = &rest[a + rel + close.len()..],
                    None => {
                        rest = "";
                        break;
                    }
                }
            }
            out.push_str(rest);
            out
        }
        // The enclosing section id of a byte offset (last section-start before
        // it), robust to the covariance block nesting s05c / s13 under s12.
        fn section_of(html: &str, pos: usize) -> &str {
            const MARK: &str = "<div class=\"section\"";
            let mut cur = "(pre)";
            let mut i = 0;
            while let Some(rel) = html[i..].find(MARK) {
                let s = i + rel;
                if s > pos {
                    break;
                }
                if let Some(idrel) = html[s..].find("id=\"") {
                    let idstart = s + idrel + 4;
                    if let Some(idend) = html[idstart..].find('"') {
                        cur = &html[idstart..idstart + idend];
                    }
                }
                i = s + MARK.len();
            }
            cur
        }
        const APPENDIX: [&str; 4] = ["s01", "s01b", "s05c", "s13"];
        const BANNED: [&str; 11] = [
            "Jet1",
            "Jet2",
            "hyperdual",
            "6×6",
            "9×9",
            "6&times;6",
            "9&times;9",
            "6x6",
            "9x9",
            "STM",
            "STT",
        ];
        fn scan(html: &str) -> Option<(String, String, String)> {
            for tok in BANNED {
                let mut i = 0;
                while let Some(rel) = html[i..].find(tok) {
                    let pos = i + rel;
                    let sid = section_of(html, pos);
                    if !APPENDIX.contains(&sid) {
                        let a = pos.saturating_sub(40);
                        let b = (pos + tok.len() + 20).min(html.len());
                        let ctx = html.get(a..b).unwrap_or(tok).to_string();
                        return Some((tok.to_string(), sid.to_string(), ctx));
                    }
                    i = pos + tok.len();
                }
            }
            None
        }
        // Proof the scan can fail: a planted token outside the appendix is caught,
        // and the appendix exemption lets the same token through inside it.
        assert!(
            scan("<div class=\"section\" id=\"s15\"><div class=\"panel-title\">Jet1</div></div>")
                .is_some(),
            "the scanner must catch a planted banned token outside the appendix"
        );
        assert!(
            scan("<div class=\"section\" id=\"s05c\"><div>Jet1</div></div>").is_none(),
            "a banned token inside the Configurations appendix must be exempt"
        );
        // Render the exact sites that leaked: the 4.2 configuration-arm and
        // calibration tables (need the predict aggregate) and every 2.2
        // timing/propagation arm id that reaches method_label, plus the ASSIST
        // arms — so a banned token at any of them would be scanned, not skipped.
        let mut rows = vec![
            synthetic_rust_prop_row("Apophis", 0.0),
            synthetic_rust_prop_row("Apophis", 30.0),
            synthetic_rust_prop_row("Bennu", 0.0),
            synthetic_rust_prop_row("Bennu", 365.0),
        ];
        for a in [
            "f64_detection_on",
            "first_order_detection_on",
            "auto_detection_on",
            "second_order_detection_on",
            "sigma_point_detection_on",
            "monte_carlo_100_detection_on",
            "f64_detection_off_assist_default_like",
            "f64_detection_off_assist_asteroid_institute_like",
            "first_order_detection_off_assist_default_like",
        ] {
            let mut r = synthetic_rust_prop_row("Apophis", 30.0);
            r.propagation_uncertainty = Some(a.to_string());
            r.emp_time_ms = Some(3.0);
            rows.push(r);
        }
        for a in [
            "assist_default_variational_off",
            "assist_default_variational_on",
            "assist_layup_variational_off",
            "assist_asteroid_institute_variational_off",
        ] {
            let mut r = synthetic_rust_prop_row("Apophis", 30.0);
            r.channel = "assist".to_string();
            r.propagation_uncertainty = Some(a.to_string());
            rows.push(r);
        }
        let agg = synthetic_predict_aggregates("snap");
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&rows, &[], &out, None, Some(&agg), &[]).unwrap();
        let raw = std::fs::read_to_string(&out).unwrap();
        let html = strip_tag(&strip_tag(&raw, "script"), "style");
        if let Some((tok, sid, ctx)) = scan(&html) {
            panic!(
                "banned derivative token {tok:?} in section {sid} (outside the appendix): …{ctx}…"
            );
        }
        // Fail-proof at a real rendered site: a banned token spliced into the 2.2
        // timing-arm label must be caught. The replacement must change the page,
        // or the site never rendered and the fail-proof is vacuous.
        let leaked_raw = raw.replacen("empyrean · first order", "empyrean · Jet1", 1);
        assert_ne!(
            leaked_raw, raw,
            "the timing-arm label site must render, or the fail-proof is vacuous"
        );
        let leaked = strip_tag(&strip_tag(&leaked_raw, "script"), "style");
        assert!(
            scan(&leaked).is_some(),
            "the scan must catch a banned token injected at a real timing-arm label site"
        );
    }

    #[test]
    fn method_label_maps_orders_and_passes_unknown_through() {
        assert_eq!(method_label("f64_detection_on"), "state only");
        assert_eq!(method_label("first_order_detection_on"), "first order");
        assert_eq!(method_label("second_order_detection_on"), "second order");
        // An unmapped arm id renders verbatim, never a silent blank.
        assert_eq!(method_label("totally_unknown_arm"), "totally_unknown_arm");
        assert!(!method_label("totally_unknown_arm").is_empty());
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
        // One ASSIST arm so the Configurations appendix renders its subsection
        // (whose Method fold now carries the ΔT caveat); plus a plain row.
        let mut rows = vec![
            synthetic_rust_prop_row("Apophis", 0.0),
            synthetic_rust_prop_row("Bennu", 0.0),
        ];
        let mut a = synthetic_rust_prop_row("Apophis", 0.0);
        a.channel = "assist".into();
        a.propagation_uncertainty = Some("assist_default_variational_off".into());
        a.notes = "config=assist_default integrator=ias15 epsilon=0.000000001 min_dt=0 adaptive_mode=global initial_dt=0.001 (assist 1.2.3 rebound 4.6.0)".into();
        rows.push(a);
        let html = render_rows(&rows);
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

        // S5a: the ΔT caveat is now one clause of the ASSIST subsection's single
        // collapsed Method fold (the per-configuration agreement panels are
        // gone); it must sit inside a disclosure body, never free-standing.
        let caveat = "ASSIST models no Marsden &Delta;T term";
        let c = html
            .find(caveat)
            .expect("the ΔT caveat must be present in the ASSIST Method fold");
        assert!(
            html[..c].rfind("disc-body").is_some_and(|d| c - d < 200),
            "the ΔT caveat text must be inside a disclosure body"
        );
        // Able to fail: the pre-S5a free-standing "ΔT exclusion" panel is gone.
        assert!(
            !html.contains("<summary>ΔT exclusion</summary>"),
            "the pre-S5a ΔT-exclusion disclosure must be gone"
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
        generate_report(&rows, &[], &out, None, None, &[]).unwrap();
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
            "id=\"s05\"",
            "id=\"s06\"",
            "id=\"s08b\"",
            "id=\"s09b\"",
            "id=\"s10\"",
            "id=\"s12\"",
            "id=\"s13\"",
        ] {
            assert!(html.contains(anchor), "missing section anchor: {anchor}");
        }
        // The cut sections must be gone. s07 (1.2D Outliers) folded into §1.2A as
        // the `outliers` disclosure in S3 — its standalone section and rail entry
        // are gone, so the fold must carry the table instead.
        for gone in [
            "id=\"s04\"",
            "id=\"s08\"",
            "id=\"s09\"",
            "id=\"s11\"",
            "id=\"s07\"",
        ] {
            assert!(!html.contains(gone), "cut section still present: {gone}");
        }
        assert!(
            html.contains("<summary class=\"more-sum\">outliers</summary>")
                && html.contains("<tbody id=\"eph-outliers\">"),
            "the outliers table must fold into §1.2A, not vanish with s07"
        );

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
    fn base64_encode_matches_reference_vectors() {
        // RFC 4648 §10 test vectors: a wrong alphabet, shift or padding here would
        // corrupt the inlined logo so the branded export shows a broken image.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
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
        generate_report(&rows, &[], &a, None, None, &[]).unwrap();
        generate_report(&rows, &[], &b, None, None, &[]).unwrap();
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
        generate_report(&rows, &[], &out1, None, None, &[]).unwrap();
        generate_report(&rows, &[], &out2, None, None, &[]).unwrap();
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
        generate_report(&rows, &[], &out, None, None, &[]).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();

        // Exactly one hatch definition, everywhere on the page.
        let hatch_defs = html.matches(".hatch {").count();
        assert_eq!(
            hatch_defs, 1,
            "expected exactly one .hatch CSS definition, found {hatch_defs}"
        );

        // S6: scope to the propagation section's A slot (s02 head → the B slot
        // anchor s02-b), so the count is the A grid's pair-panels only, not the C
        // cost panels that now share the merged section.
        let s02 = html.find("id=\"s02\"").expect("propagation section");
        let s02b = html[s02..]
            .find("id=\"s02-b\"")
            .map(|e| s02 + e)
            .unwrap_or(html.len());
        let sec = &html[s02..s02b];

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
        // S8 item 2: one row per arc — the object name is the optical row's first
        // cell (gobjname2); the radar arc is an indented sub-row below it.
        let name_at = html
            .find(&format!(">{object}</td>"))
            .unwrap_or_else(|| panic!("no grid row for {object:?}"));
        if kind == "optical" {
            let tr_start = html[..name_at].rfind("<tr").expect("optical row opens");
            let end = html[tr_start..]
                .find("</tr>")
                .expect("unterminated optical row");
            html[tr_start..tr_start + end].to_string()
        } else {
            let rest = &html[name_at..];
            let cell = format!("class=\"gtool\">{kind}</td>");
            let c_at = rest
                .find(&cell)
                .unwrap_or_else(|| panic!("{object:?} has no {kind:?} arc row"));
            let r2 = &rest[c_at..];
            let end = r2.find("</tr>").expect("unterminated arc row");
            r2[..end].to_string()
        }
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
        // is objects (radar_counts.len(), matching §4.1) in the numbers-only idiom.
        assert!(
            summary.contains("σ_eq 1.50 · n 1/1 · ≤3σ 1 · 3 radar excluded"),
            "visible summary must state the excluded OBJECT count (3): {summary}"
        );
        // S4 text-to-marks: the summary line keeps numbers only; its words-naming-
        // the-numbers hover is removed. The radar-exclusion rationale rides §1.3A's
        // Method fold now, not this hover — fails on the old structure, which
        // carried "3 objects … 4 records, both epoch sources" in the title.
        assert!(
            !open_tag.contains("title="),
            "the summary line carries no hover title (numbers only): {open_tag:?}"
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
        // row (the object name IS the row — S8 item 2, no separate name row), with
        // the radar arc as a sub-row — the row structure is uniform per panel.
        let emp_panel = {
            let start = html
                .find("data-tool=\"empyrean\"")
                .expect("empyrean panel present");
            let rest = &html[start..];
            let end = rest.find("data-tool=\"findorb\"").unwrap_or(rest.len());
            &rest[..end]
        };
        // One object row per object (name in the row), no separate name row.
        assert_eq!(
            emp_panel
                .matches("class=\"gobjr odrow\"><td class=\"gobjname2")
                .count(),
            2,
            "the empyrean panel must have one object (optical) row per object (Apophis, Bennu)"
        );
        assert_eq!(
            emp_panel.matches("class=\"gtool\">+ radar</td>").count(),
            1,
            "the empyrean panel must carry the one radar arc as a sub-row"
        );
        // The separate, colspanned name row is gone.
        assert!(
            !emp_panel.contains("class=\"gobjrow\""),
            "S8 item 2: no separate object-name row survives"
        );
    }

    #[test]
    fn s8_od_grid_is_one_row_per_arc_and_panels_stay_row_for_row_aligned() {
        // S8 item 2: the OD A-grid renders the object name IN the row (no separate
        // name row), population headers as every other grid (gpophdr), + radar
        // sub-rows only where they exist, and every tool's panel keeps the exact
        // same row sequence so the Tool/Reference selector swaps panels row-for-row.
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
        // Population headers use the shared gpophdr/gpopname form, not gblock.
        assert!(
            html.contains("class=\"gpophdr\"><td class=\"gpopname\"")
                && !html.contains("class=\"gblock\">"),
            "1.3A population headers must use the shared gpophdr form"
        );
        // The object name is the optical row's first cell.
        assert!(
            html.contains("class=\"gobjr odrow\"><td class=\"gobjname2\">Apophis</td>"),
            "the object name is rendered in the optical row, not a separate name row"
        );
        // Row-for-row alignment: the empyrean and find_orb panels emit the SAME
        // sequence of row classes, so the selector can swap them without shifting
        // layout. Extract each panel's row-class sequence and compare.
        let panel_rows = |tool: &str| -> Vec<String> {
            let s = html
                .find(&format!("data-tool=\"{tool}\""))
                .unwrap_or_else(|| panic!("{tool} panel"));
            let rest = &html[s..];
            let e = rest[1..]
                .find("data-tool=\"")
                .map(|x| x + 1)
                .unwrap_or(rest.len());
            let panel = &rest[..e];
            panel
                .match_indices("<tr class=\"")
                .map(|(i, _)| {
                    let tail = &panel[i + "<tr class=\"".len()..];
                    tail[..tail.find('"').unwrap()].to_string()
                })
                .collect()
        };
        assert_eq!(
            panel_rows("empyrean"),
            panel_rows("findorb"),
            "every tool's OD panel must emit the identical row sequence (row-for-row alignment)"
        );
        // + radar sub-rows only where the arc exists (Apophis has one, Bennu none).
        assert_eq!(
            panel_rows("empyrean")
                .iter()
                .filter(|c| c.contains("odradar"))
                .count(),
            1,
            "a + radar sub-row appears only for the object that has a radar arc"
        );
    }

    #[test]
    fn s8_od_grid_speaks_one_null_token() {
        // S8 item 1: the 1.3A OD grid uses one null token (—) for every "no value"
        // state — the undefined metric (Σ_ref not SPD), converged-with-no-record,
        // and a missing σ_eq all read as —, with the reason on the cell hover. The
        // old distinct ? / · / n/d glyphs are gone. Fails on the old structure.
        let mut iris = conv_od_row("Iris", "rust", "orbit_determination");
        iris.od_converged = Some(true);
        let mut nocov = conv_od_row("Nysa", "rust", "orbit_determination");
        nocov.od_converged = Some(true);
        let html = build_od_closeness_grid(
            &[iris, nocov],
            &[sbdb_cmp("Iris", f64::NAN)], // Iris: record present but Σ_ref non-SPD; Nysa: no record
        );
        // No legacy null glyph survives in the OD grid cells or its key.
        for legacy in [">?</span>", ">·</span>", ">n/d<"] {
            assert!(
                !html.contains(legacy),
                "a legacy null glyph survives: {legacy}"
            );
        }
        // The undefined-metric cell shows — and carries its reason on hover.
        let iris_row = od_arc_row(&html, "Iris", "optical");
        assert!(
            iris_row.contains("conv undef")
                && iris_row.contains("Σ_ref not SPD")
                && iris_row.contains(">—</span>"),
            "the undefined-metric cell is the one null token with its reason on hover: {iris_row}"
        );
        // The key explains the one null token (—), with the reason deferred to hover.
        assert!(
            html.contains("no closeness number"),
            "the key explains the one null token"
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
        generate_report(&rows, &[], &out, None, None, &[]).unwrap();
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
        generate_report(&timing_fixture(), &[], &out, None, None, &[]).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        // S7 item 3: DOM order == visual order — no flex `order` reorders the
        // column, so byte position IS the on-page order. The page opens on Part 1
        // and the appendix (run summary first) follows Part 4.
        let at = |id: &str| -> usize {
            html.find(&format!("id=\"{id}\""))
                .unwrap_or_else(|| panic!("missing #{id}"))
        };
        assert!(
            !html.contains("{ order:"),
            "no flex order rule may survive: DOM order must be the visual order"
        );
        assert!(
            at("part1-bar") < at("part2") && at("part2") < at("part4"),
            "the page opens on Part 1; Part 2 precedes Part 4"
        );
        assert!(
            at("part4") < at("appendix"),
            "the appendix follows Part 4, not precedes Part 1"
        );
        assert!(
            at("appendix") < at("s01") && at("s01") < at("s01b") && at("s01b") < at("s13"),
            "run summary (A.1) opens the appendix, then external tools and provenance"
        );
    }

    #[test]
    fn part_two_hatched_cells_carry_their_reason_on_hover_only() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&timing_fixture(), &[], &out, None, None, &[]).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        // S7: Part 2 is the three sections between the part2 and part3 heads.
        let start = html.find("id=\"part2\"").expect("Part 2 head");
        let end = html[start..].find("id=\"part3\"").unwrap() + start;
        let part2 = &html[start..end];
        // Every hatched grid cell carries its reason on hover; its visible text is
        // empty or one of the canonical hatch phrases, never a free sentence.
        let canonical = [
            "",
            HATCH_NOT_IN_RUN,
            HATCH_NOT_TIMED,
            HATCH_NOT_COMPARED,
            HATCH_NO_RECORD,
            HATCH_NOT_ATTEMPTED,
        ];
        let mut hatched = 0;
        for cell in part2.split("<td class=\"gc hatch\"").skip(1) {
            let (open, rest) = cell.split_once('>').expect("open tag");
            let text = &rest[..rest.find("</td>").expect("cell end")];
            assert!(
                open.contains("title=\"") && !open.contains("title=\"\""),
                "hatched cell without a reason: {open}"
            );
            assert!(
                canonical.contains(&text),
                "hatched cell shows non-canonical text: {text:?}"
            );
            hatched += 1;
        }
        assert!(
            hatched >= 4,
            "expected the untimed and absent-tool grid hatches, found {hatched}"
        );
        // The by-object-and-fitter matrix (2.3 A) states a fit's state with a
        // glyph, never as a word, and every glyph carries its reason.
        for words in [">fit<", ">conv<", ">n/d<"] {
            assert!(!part2.contains(words), "state written as a word: {words}");
        }
        let fitter = &part2[part2
            .find("By object and fitter")
            .expect("2.3 by object and fitter table")..];
        let mut glyphs = 0;
        for cell in fitter.split("<td class=\"r1c\"").skip(1) {
            let (open, rest) = cell.split_once('>').expect("open tag");
            let text = &rest[..rest.find("</td>").expect("cell end")];
            // S8 item 1: a fit with no closeness number reads as the one null token
            // (—); a failed fit keeps ✕. Both still carry their reason on hover.
            if ["—", "✕"].contains(&text) {
                assert!(
                    open.contains("title=\""),
                    "state glyph {text} without a reason"
                );
                glyphs += 1;
            }
        }
        assert!(
            glyphs >= 1,
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
        generate_report(&timing_fixture(), &[], &out, None, None, &[]).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        // S7: the rail names 2.1 by its axis (Propagation), never a "side by
        // side" ranking label.
        let rail = html
            .find("data-target=\"s-prop\"")
            .expect("rail entry for section 2.1");
        let entry = &html[rail..rail + html[rail..].find("</a>").unwrap()];
        assert!(entry.ends_with("2.1 Propagation"), "rail entry: {entry}");
        assert!(
            !html.contains("Side by side"),
            "the ranking-flavoured 2.1 label is gone"
        );
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
        generate_report(&timing_fixture(), &[], &out, None, None, &[]).unwrap();
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
        generate_report(&rows, &[], &out, None, None, &[]).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();

        let p2 = html.find("id=\"part2\"").expect("part2 anchor");
        for id in [
            "g1-prop-time",
            "g2-eph-time",
            "g3-od-time",
            "s-prop",
            "s-eph",
            "s-od",
        ] {
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
        generate_report(&rows, &[], &out, None, None, &[]).unwrap();
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
        generate_report(&rows2, &[], &out2, None, None, &[]).unwrap();
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
            "emp first order",
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
        generate_report(
            &[synthetic_rust_prop_row("Apophis", 0.0)],
            &[],
            &out,
            None,
            None,
            &[],
        )
        .unwrap();
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
                "grid-scroll grid-sticky\"><table class=\"agrid fidgrid\" data-grid=\"fidelity\" id=\"g1-prop-time\""
            ),
            "per-object timing grid wrapper must carry the sticky marker"
        );
        // Precision: the wide per-object fidelity grid shares the fidgrid class but
        // must keep the plain wrapper (it scrolls horizontally, never pins).
        assert!(
            html.contains("grid-scroll\"><table class=\"agrid fidgrid\" data-grid=\"fidelity\"><thead><tr><th class=\"gobj\" rowspan=\"2\">object</th>"),
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
        let grids = build_timing_grid_axis(
            &rows,
            "orbit_determination",
            emp_eph_channel(&rows),
            emp_od_channel(&rows),
        );
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
        generate_report(rows, &[], &out, None, None, &[]).unwrap();
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
        // S5 item 1: the Part 1 head is the sticky Compare bar (#part1-bar);
        // there is no separate #part1 element any more.
        let part1 = at("id=\"part1-bar\"");
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
    fn compare_bar_carries_the_only_selects_and_states_the_pair() {
        let html = render_rows(&pair_test_rows());
        let at = |needle: &str| {
            html.find(needle)
                .unwrap_or_else(|| panic!("missing {needle}"))
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
        // S5 item 1: the bar IS the Part 1 head line — it runs from its open tag
        // to the first Part 1 section — and carries both selects plus the Part 1
        // head (eyebrow + title) on one sticky line.
        let bar_open = at("<div class=\"part1-bar\" id=\"part1-bar\">");
        let bar = &html[bar_open..at("<div class=\"section\" id=\"s02\">")];
        assert!(
            bar.contains("id=\"tool1-select\"") && bar.contains("id=\"tool2-select\""),
            "both selects must live inside the compare bar"
        );
        assert!(
            bar.contains(">Compare<") && bar.contains(">vs<"),
            "the bar names the Compare controls"
        );
        assert!(
            bar.contains("class=\"ph-eyebrow\"") && bar.contains("class=\"ph-title\""),
            "the bar carries the Part 1 head (eyebrow + title) on the same line"
        );

        // S7: DOM order == visual order — the bar sits (by byte position) ahead of
        // the first Part 1 section, no flex `order` reorders it.
        assert!(
            bar_open < at("<div class=\"section\" id=\"s02\">"),
            "the bar sits ahead of the first Part 1 section in DOM order"
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

        // The pair chips are gone (S4 text-to-marks): the sticky Compare bar
        // states the pair (its two tool-coloured selects + Compare / vs), so no
        // section title carries a pair-chip element and none survives anywhere on
        // the page — fails the moment a chip is re-introduced. (The defensive
        // empTitleText still names the 'pair-chip' class in JS, which is why this
        // keys on the chip MARKUP, not the bare string.)
        assert!(
            !html.contains("class=\"pair-chip\"")
                && !html.contains("class=\"pc-t\"")
                && !html.contains("class=\"pc-vs\""),
            "no pair-chip element survives anywhere — the Compare bar states the pair"
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

        // The three agreement hovers keep their description but name no pair
        // (the Compare bar states it).
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
        // S5 item 5: the tool CATEGORICAL colours are gone from the Compare bar —
        // the two selects carry no per-tool inline hue (names only) and read the
        // one shared accent instead.
        assert!(
            html.contains("id=\"tool1-select\" aria-label=\"Tool\"></select>")
                && html.contains("id=\"tool2-select\" aria-label=\"Reference\"></select>"),
            "the compare-bar selects carry no per-tool inline colour (names only)"
        );
        assert!(
            html.contains(".part1-bar select {") && html.contains("color: var(--ed-accent)"),
            "the compare-bar selects read the one shared accent, not a per-tool hue"
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

    #[test]
    fn s8_part_one_bar_keeps_selects_on_one_line_and_facts_on_a_second_row() {
        // S8 item 3: the two Compare selects stay on ONE line with "vs" between
        // them; the four facts render as a full-width second row of the bar.
        // Fails on the pre-S8 structure (facts nested inside .ph-head, no
        // .p1b-topline, a wrapping .p1b-compare, a row .p1b-inner).
        let html = render_rows(&pair_test_rows());
        let bar_open = html
            .find("<div class=\"part1-bar\" id=\"part1-bar\">")
            .expect("part1-bar");
        let bar = &html[bar_open..html.find("<div class=\"section\" id=\"s02\">").unwrap()];
        // 1. A top line wraps the head and the Compare controls on one row.
        let topline = bar.find("class=\"p1b-topline\"").expect("p1b-topline row");
        let facts = bar.find("class=\"ph-facts\"").expect("ph-facts row");
        let compare = bar.find("class=\"p1b-compare\"").expect("p1b-compare");
        // 2. The facts are a SEPARATE row after the top line (not inside .ph-head,
        //    so they clear the Compare controls entirely).
        assert!(
            topline < compare && compare < facts,
            "the facts render after the top line's Compare controls, as the second row"
        );
        let ph_head = bar.find("class=\"ph-head\"").expect("ph-head");
        assert!(
            ph_head < facts && facts > compare,
            "the facts are a sibling second row, not nested inside the head/title line"
        );
        // 3. The inner bar is a column; the top line and the Compare controls both
        //    refuse to wrap, so the two selects and "vs" never break onto two lines.
        let css = |sel: &str| {
            let i = html
                .find(sel)
                .unwrap_or_else(|| panic!("missing css {sel}"));
            html[i..i + html[i..].find('}').unwrap()].to_string()
        };
        assert!(
            css(".p1b-inner {").contains("flex-direction: column"),
            "the inner bar stacks the top line over the facts row"
        );
        assert!(
            css(".p1b-topline {").contains("flex-wrap: nowrap"),
            "the top line (title + Compare) never wraps"
        );
        let comp = css(".p1b-compare {");
        assert!(
            comp.contains("flex-wrap: nowrap") && comp.contains("white-space: nowrap"),
            "the two selects and vs stay on one line: {comp}"
        );
    }

    #[test]
    fn s8_walk_timeline_labels_its_lines_directly_not_with_a_legend() {
        // S8 item 4: the last Plotly legend (the 4.2 dual-axis walk timeline) is
        // replaced by a direct label at each line's end. Fails on the old
        // structure (drawWalkTimeline carried `legend: LEGEND_H`).
        let rows = vec![synthetic_rust_prop_row("Apophis", 0.0)];
        let agg = synthetic_predict_aggregates("snap");
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&rows, &[], &out, None, Some(&agg), &[]).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        let f = html
            .find("function drawWalkTimeline(")
            .expect("drawWalkTimeline");
        // The function body up to the next top-level function declaration (or the
        // end of the script if it is the last one). LEGEND_H is only used before
        // this function, so extending to the end keeps the negative check sound.
        let rel = html[f..].find("\nfunction ").unwrap_or(html.len() - f);
        let body = &html[f..f + rel];
        assert!(
            body.contains("showlegend: false"),
            "the walk timeline disables the Plotly legend"
        );
        assert!(
            !body.contains("legend: LEGEND_H"),
            "the walk timeline no longer carries a legend entry"
        );
        assert!(
            body.contains("text: 'realism d²n', showarrow: false")
                && body.contains("text: 'sep (arcsec)', showarrow: false"),
            "each line is named directly at its own end"
        );
    }

    #[test]
    fn s8_every_table_belongs_to_the_one_agrid_family_with_a_role() {
        // S8 item 1: every rendered table is one grid family (`agrid` + a variant
        // or the `list` style); no legacy standalone family (`od-table`,
        // `heatmap`, `od-fails-t`) survives, and the grids the audit protects
        // carry a `data-grid` role so the checks key on the role, not the class.
        // Fails on the pre-S8 structure (bare od-table / heatmap / od-fails-t).
        let rows = pair_test_rows();
        let agg = synthetic_predict_aggregates("snap");
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&rows, &[], &out, None, Some(&agg), &[]).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        let mut n_tables = 0usize;
        let mut idx = 0usize;
        while let Some(rel) = html[idx..].find("<table") {
            let start = idx + rel;
            let end = start + html[start..].find('>').expect("the table tag closes");
            let tag = &html[start..=end];
            n_tables += 1;
            assert!(
                tag.contains("class=\"agrid"),
                "every table is in the agrid family: {tag}"
            );
            assert!(
                !tag.contains("class=\"od-table")
                    && !tag.contains("class=\"heatmap\"")
                    && !tag.contains("class=\"od-fails-t"),
                "no legacy standalone table class survives: {tag}"
            );
            idx = end + 1;
        }
        assert!(
            n_tables >= 8,
            "the render carries the family's tables, got {n_tables}"
        );
        for role in [
            "data-grid=\"agreement\"",
            "data-grid=\"predict\"",
            "data-grid=\"list\"",
        ] {
            assert!(html.contains(role), "the render carries the {role} grids");
        }
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
        // S6: the merged 1.1 section runs from s02 to the merged 1.2 section s06;
        // its first disc-body is the one ▸Method fold (the A/B methods merged).
        let s02 = html.find("id=\"s02\"").expect("s02");
        let s06 = html[s02..].find("id=\"s06\"").map(|e| s02 + e).unwrap();
        let sec = &html[s02..s06];
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
    fn s6_ephemeris_growth_and_cost_live_inside_the_merged_1_2_section() {
        // S6: 1.1/1.2/1.3 are each ONE .section with the fixed slots. The former
        // standalone error-growth (s03/s06b) and cost (s-t1-prop/s-t2-eph/s-t3-od)
        // sections are gone; their content lives in the B and C slots of the
        // merged section. The eph-sep-chart and its pair/channels toggle sit
        // inside the merged 1.2 section (s06), between the B and C slot anchors.
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&timing_fixture(), &[], &out, None, None, &[]).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();

        // The old split sections no longer exist as section anchors.
        for gone in [
            "id=\"s03\"",
            "id=\"s-t1-prop\"",
            "id=\"s06b\"",
            "id=\"s-t2-eph\"",
            "id=\"s-t3-od\"",
            "id=\"s07\"",
        ] {
            assert!(!html.contains(gone), "the old split section {gone} is gone");
        }

        // The merged 1.2 section runs from s06 to the merged 1.3 section s08b and
        // carries the B eyebrow, the eph-sep-chart (with its toggle) and the C
        // eyebrow, in that order.
        let s06 = html.find("id=\"s06\"").expect("merged 1.2 section s06");
        let s08b = html[s06..].find("id=\"s08b\"").map(|e| s06 + e).unwrap();
        let sec = &html[s06..s08b];
        let b_eye = sec.find("id=\"s06-b\"").expect("1.2 B slot anchor");
        let toggle = sec
            .find("id=\"s06b-toggle\"")
            .expect("pair/channels toggle");
        let chart = sec.find("id=\"eph-sep-chart\"").expect("eph-sep-chart");
        let c_eye = sec.find("id=\"s06-c\"").expect("1.2 C slot anchor");
        assert!(
            b_eye < toggle && toggle < chart && chart < c_eye,
            "the toggle + eph-sep-chart sit in the B slot, before C"
        );
        // The toggle id is unchanged; no stale carryover.
        assert!(!html.contains("s06-toggle\""), "no stale s06-toggle id");

        // S7: DOM order == visual order — the three merged sections run after the
        // bar in order by byte position (no flex `order`).
        let at = |id: &str| -> usize {
            html.find(&format!("id=\"{id}\""))
                .unwrap_or_else(|| panic!("no section {id}"))
        };
        assert!(at("part1-bar") < at("s02"), "bar precedes 1.1");
        assert!(at("s02") < at("s06"), "1.1 precedes 1.2");
        assert!(at("s06") < at("s08b"), "1.2 precedes 1.3");
    }

    #[test]
    fn s6_part_one_is_three_sections_numbered_1_1_1_2_1_3_with_abc_slots() {
        // S6 item 1/3: Part 1 is exactly three .section elements, numbered
        // 1.1/1.2/1.3 (the lone-letter scheme is gone), each with the three slot
        // anchors A·B·C; nothing between the bar and the Part 2 head is another
        // section.
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&timing_fixture(), &[], &out, None, None, &[]).unwrap();
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
        assert_eq!(num("s02"), "1.1");
        assert_eq!(num("s06"), "1.2");
        assert_eq!(num("s08b"), "1.3");
        // The lone-letter Part-1 numbering is gone.
        for gone in [
            "1.1A", "1.1B", "1.1C", "1.2A", "1.2B", "1.2C", "1.3A", "1.3B",
        ] {
            assert!(
                !html.contains(&format!("section-num\">{gone}<")),
                "the lone-letter number {gone} is gone from Part 1 titles"
            );
        }
        // Each merged section carries its three slot anchors A·B·C.
        for sec in ["s02", "s06", "s08b"] {
            for slot in ["a", "b", "c"] {
                assert!(
                    html.contains(&format!("id=\"{sec}-{slot}\"")),
                    "{sec} must carry the {slot} slot anchor"
                );
            }
        }
        // Exactly three Part-1 sections (the bar..Part 2 window holds only them).
        let bar = html.find("id=\"part1-bar\"").expect("part1-bar");
        let p2 = html[bar..]
            .find("id=\"part2\"")
            .map(|e| bar + e)
            .expect("part2 head");
        let window = &html[bar..p2];
        // Three real merged sections; the hidden legacy overview-footer .section
        // is not a Part-1 content section.
        let n = window.matches("<div class=\"section\" id=").count()
            - window.matches("id=\"overview-footer\"").count();
        assert_eq!(n, 3, "Part 1 holds exactly three content .section elements");
    }

    #[test]
    fn s6_part_one_head_states_the_four_facts_once() {
        // S6 item 2: the Part 1 head line states, once, epoch (MJD TDB + UTC),
        // the frame per metric, the force-model pair, and the observing sites —
        // read from the rows, in the eyebrow role under the title.
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&timing_fixture(), &[], &out, None, None, &[]).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        let bar = html.find("id=\"part1-bar\"").expect("part1-bar");
        let p2 = html[bar..].find("id=\"part2\"").map(|e| bar + e).unwrap();
        let head = &html[bar..p2];
        let facts = head.find("class=\"ph-facts\"").expect("ph-facts");
        let facts = &head[facts..];
        // All four facts are present in the eyebrow role, each in its own chip.
        for k in ["epoch", "frames", "force", "sites"] {
            assert!(
                facts.contains(&format!("data-k=\"{k}\"")),
                "the head states the {k} fact"
            );
        }
        // The epoch fact carries both MJD TDB and a UTC date, from the rows (the
        // fixture's 61000.0 IC epoch); the frame-per-metric and the force pair
        // are stated with their reference.
        assert!(
            facts.contains("MJD 61000.0 TDB") && facts.contains(" UTC"),
            "the epoch fact states MJD TDB and UTC"
        );
        assert!(facts.contains("ICRF/J2000"), "a per-metric frame is stated");
        assert!(
            facts.contains("DE441"),
            "the reference force model is stated"
        );
    }

    #[test]
    fn s6_part_one_rail_is_generated_from_the_section_tree() {
        // S6 item 3: the rail is generated from the same tree — one entry per
        // section plus A·B·C sub-entries that scroll to the slot anchors; the old
        // · error growth / · cost / · outliers entries are gone.
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&timing_fixture(), &[], &out, None, None, &[]).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        let rail_at = html.find("<nav class=\"rail\"").expect("rail");
        let rail_end = html[rail_at..].find("</nav>").map(|e| rail_at + e).unwrap();
        let rail = &html[rail_at..rail_end];
        for (sec, num, title) in [
            ("s02", "1.1", "Propagation"),
            ("s06", "1.2", "Ephemeris"),
            ("s08b", "1.3", "Orbit determination"),
        ] {
            assert!(
                rail.contains(&format!(
                    "data-target=\"{sec}\" href=\"#{sec}\">{num} {title}</a>"
                )),
                "rail names {num} {title}"
            );
            for slot in ["a", "b", "c"] {
                assert!(
                    rail.contains(&format!(
                        "data-target=\"{sec}-{slot}\" href=\"#{sec}-{slot}\""
                    )),
                    "rail sub-entry scrolls to {sec}-{slot}"
                );
            }
        }
        // The old standalone per-axis sub-entries (`&nbsp;&nbsp;· error growth`,
        // `· cost`, `· outliers`) are gone; the new sub-entries read `A · by
        // object` etc. (so "· cost" legitimately appears inside `C · cost`).
        assert!(
            !rail.contains("· error growth")
                && !rail.contains(">&nbsp;&nbsp;· cost<")
                && !rail.contains("· outliers"),
            "the old per-axis rail sub-entries are gone"
        );
    }

    #[test]
    fn s6_one_method_fold_per_merged_part_one_section() {
        // S6 item 1: one ▸Method fold per section (the A/B methods merged); 1.2
        // also keeps the ▸outliers itemised runaway list.
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&timing_fixture(), &[], &out, None, None, &[]).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        let count_methods = |a: &str, b: &str| -> usize {
            let lo = html.find(a).unwrap();
            let hi = html[lo..].find(b).map(|e| lo + e).unwrap();
            html[lo..hi].matches("<summary>Method</summary>").count()
        };
        assert_eq!(
            count_methods("id=\"s02\"", "id=\"s06\""),
            1,
            "1.1 has one Method"
        );
        assert_eq!(
            count_methods("id=\"s06\"", "id=\"s08b\""),
            1,
            "1.2 has one Method"
        );
        assert_eq!(
            count_methods("id=\"s08b\"", "id=\"overview-footer\""),
            1,
            "1.3 has one Method"
        );
        // 1.2 keeps the itemised runaway list under its fold.
        let s06 = html.find("id=\"s06\"").unwrap();
        let s08b = html[s06..].find("id=\"s08b\"").map(|e| s06 + e).unwrap();
        let sec = &html[s06..s08b];
        assert!(
            sec.contains("<summary class=\"more-sum\">outliers</summary>")
                && sec.contains("<tbody id=\"eph-outliers\">"),
            "1.2 keeps the itemised ▸outliers runaway list"
        );
    }

    #[test]
    fn s6_od_grid_drops_the_empty_dr_epoch_column_and_gains_a_fit_quality_marginal() {
        // S6 item 1 (1.3): the all-"—" Δr@epoch column is gone from the OD grid,
        // and 1.3's B slot is a fit-quality population marginal with named
        // not-measured states (never blanks).
        let (rows, cmp) = radar_scenario();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&rows, &cmp, &out, None, None, &[]).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        assert!(
            !html.contains("Δr @epoch") && !html.contains("not in the sidecar schema yet"),
            "the empty Δr@epoch column is gone from the OD grid"
        );
        // The 1.3 B slot (s08b-b) holds the fit-quality marginal with the named
        // not-measured states.
        let b_at = html.find("id=\"s08b-b\"").expect("1.3 B slot anchor");
        let c_at = html[b_at..]
            .find("id=\"s08b-c\"")
            .map(|e| b_at + e)
            .unwrap();
        let b = &html[b_at..c_at];
        assert!(
            b.contains("not measured"),
            "the fit-quality marginal names its not-measured states"
        );
        for header in ["n_obs", "RMS", "σ_eq"] {
            assert!(
                b.contains(header),
                "the fit-quality marginal shows {header}"
            );
        }
    }

    #[test]
    fn both_growth_charts_draw_through_one_shared_function() {
        // A2: exactly one growth-chart drawing function, and both containers are
        // drawn through it, so the two charts cannot drift apart again. The old
        // per-chart drawers and the divergent gMed estimator are gone.
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&timing_fixture(), &[], &out, None, None, &[]).unwrap();
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
        generate_report(&timing_fixture(), &[], &out, None, None, &[]).unwrap();
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
        generate_report(&timing_fixture(), &[], &out, None, None, &[]).unwrap();
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
        // S5 item 3: the covariance panels carry NO Plotly legend — the shared
        // COV_LEGEND placement is gone and each panel names its series with direct
        // end labels / a chip annotation instead.
        assert_eq!(
            html.matches("COV_LEGEND").count(),
            0,
            "the shared covariance legend must be gone (direct labels replace it)"
        );
        assert!(
            html.contains("const chiAnnos =") && html.contains("const cdfAnnos ="),
            "the chi² and CDF panels must build direct-label annotations"
        );
    }

    // ── Signature download button + branded export ──────────────────────────

    #[test]
    fn every_plotly_call_passes_the_one_shared_config() {
        let html = render_rows(&pair_test_rows());
        // Exactly one displayModeBar literal — it lives only in the shared config.
        assert_eq!(
            html.matches("displayModeBar").count(),
            1,
            "displayModeBar must appear exactly once, inside the shared PLOTLY_CONFIG"
        );
        // The shared config object is defined exactly once.
        assert_eq!(
            html.matches("const PLOTLY_CONFIG =").count(),
            1,
            "one shared Plotly config object for every call site"
        );
        // Every Plotly.react(/newPlot( call passes PLOTLY_CONFIG as its config: the
        // number of calls must equal the number that end in `, PLOTLY_CONFIG)`.
        let calls = html.matches("Plotly.react(").count() + html.matches("Plotly.newPlot(").count();
        assert!(
            calls >= 10,
            "expected the full set of chart call sites, found {calls}"
        );
        assert_eq!(
            html.matches(", PLOTLY_CONFIG)").count(),
            calls,
            "every Plotly.react/newPlot call must pass the shared config ({calls} calls)"
        );
        // The config keeps the toolbar always on, drops Plotly's logo, and adds our
        // one branded download button in place of the stock camera.
        assert!(
            html.contains("displayModeBar: true"),
            "the toolbar must always be visible"
        );
        assert!(
            html.contains("displaylogo: false"),
            "the Plotly logo must be off"
        );
        assert!(
            html.contains("click: empExportPng"),
            "the one download button must call the branded exporter"
        );
    }

    #[test]
    fn export_attribution_carries_run_date_and_component_version() {
        let html = render_rows(&pair_test_rows());
        // The run date shown in the provenance table.
        let rd_anchor = "Report generated</td><td>";
        let rd_at = html.find(rd_anchor).expect("provenance run date") + rd_anchor.len();
        let run_date = &html[rd_at..rd_at + html[rd_at..].find('<').unwrap()];
        assert_eq!(
            run_date.len(),
            10,
            "run date should be YYYY-MM-DD: {run_date}"
        );
        // The empyrean version shown in the component table (the distribution-under-
        // test row), read from the page rather than hardcoded in the test.
        let hdr = html.find("<th>Version</th>").expect("component table");
        let row_at = hdr + html[hdr..].find(">empyrean</td>").expect("empyrean row");
        let row = &html[row_at..row_at + html[row_at..].find("</tr>").unwrap()];
        let vtd = row.rfind("<td>").unwrap() + "<td>".len();
        let version = &row[vtd..vtd + row[vtd..].find("</td>").unwrap()];
        // The server-built attribution const carries both.
        let anchor = "const EMP_ATTRIBUTION = ";
        let at = html.find(anchor).expect("attribution const") + anchor.len();
        let rest = &html[at..];
        assert!(
            rest.starts_with('"'),
            "attribution must be a JS string literal: {}",
            &rest[..40.min(rest.len())]
        );
        let close = rest[1..].find('"').unwrap() + 1;
        let attr = &rest[1..close];
        assert!(
            attr.contains(run_date),
            "attribution must carry the run date {run_date}: {attr}"
        );
        assert!(
            attr.contains(version),
            "attribution must carry the component version {version}: {attr}"
        );
        assert!(
            attr.starts_with("EMPYREAN DYNAMICS · validation report ·"),
            "attribution must lead with the brand and report type: {attr}"
        );
    }

    #[test]
    fn logo_is_inlined_once_as_a_png_data_uri() {
        let html = render_rows(&pair_test_rows());
        // The data URI appears exactly once (one shared logo const).
        let needle = "data:image/png;base64,";
        assert_eq!(
            html.matches(needle).count(),
            1,
            "the logo data URI must be emitted exactly once"
        );
        // The embedded bytes are a PNG (magic number), and the base64 payload begins
        // with that magic, so the export renders a real image.
        assert_eq!(
            &LOGO_BYTES[..8],
            &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A],
            "the embedded logo must be a PNG"
        );
        let at = html.find(needle).unwrap() + needle.len();
        assert!(
            html[at..].starts_with("iVBORw0KGgo"),
            "the base64 payload must decode to the PNG magic header"
        );
    }

    #[test]
    fn exported_logo_is_the_resampled_transparent_mark_not_the_filled_tile() {
        // The filled 1024×1024 tile's background equalled the dark theme surface,
        // so it was invisible on dark and a dark box on light. The inlined asset
        // must be the resampled, truecolour+alpha mark (orbit + bodies, no tile),
        // which reads on both themes. Read the IHDR directly (no decode): bytes
        // 12..16 name the chunk, 16..20 the width, 20..24 the height, 25 the
        // colour type.
        assert_eq!(&LOGO_BYTES[12..16], b"IHDR", "IHDR must be the first chunk");
        let w = u32::from_be_bytes([
            LOGO_BYTES[16],
            LOGO_BYTES[17],
            LOGO_BYTES[18],
            LOGO_BYTES[19],
        ]);
        let h = u32::from_be_bytes([
            LOGO_BYTES[20],
            LOGO_BYTES[21],
            LOGO_BYTES[22],
            LOGO_BYTES[23],
        ]);
        let colour_type = LOGO_BYTES[25];
        assert_eq!(
            w, h,
            "the mark is square (export sizing keeps it pixel-square)"
        );
        assert!(
            w <= 512,
            "the inlined logo must be the resampled mark, not the 1024px masthead tile: {w}px"
        );
        assert_eq!(
            colour_type, 6,
            "the mark must be truecolour+alpha (PNG colour type 6) so its background is transparent"
        );
    }

    #[test]
    fn export_identity_keys_the_pair_off_the_compare_bar_selects() {
        let html = render_rows(&pair_test_rows());
        // The old identity gated the pair on DOM ancestry (gd.closest('#part1')),
        // but the Part 1 head is a sibling of the charts, never an ancestor, so
        // the pair was never appended. S5 item 1: the head is now the sticky
        // Compare bar (#part1-bar), still a sibling — so ancestry still cannot be
        // the gate and the num-prefix gate below is what fires.
        assert!(
            !html.contains("closest('#part1')"),
            "the export identity must not gate the pair on #part1 ancestry"
        );
        assert!(
            html.contains("<div class=\"part1-bar\" id=\"part1-bar\">"),
            "the Part 1 head is the sticky Compare bar, a sibling of the charts, not a wrapper"
        );
        // S4: the pair chips are gone, so the pair can no longer be detected by a
        // chip in the chart's section. It is now gated on the Part 1 section
        // number ("1." prefix) and reads the pair the sticky Compare bar's selects
        // name (TOOL1/TOOL2) — never a pair chip.
        assert!(
            !html.contains("sec.querySelector('.pair-chip')"),
            "the pair gate must not key off a pair chip (chips removed in S4)"
        );
        assert!(
            html.contains("num.indexOf('1.') === 0"),
            "the pair is gated on the Part 1 section number prefix"
        );
        assert!(
            html.contains("pair = toolLabel(TOOL1) + ' vs ' + toolLabel(TOOL2)"),
            "the pair string comes from the Compare bar's TOOL1/TOOL2 selection"
        );
        // Each Part 1 client chart sits in a section whose number starts "1.", so
        // the num-prefix gate fires for exactly those charts.
        for id in ["growth-chart", "eph-sep-chart"] {
            let at = html
                .find(&format!("id=\"{id}\""))
                .unwrap_or_else(|| panic!("#{id} must be present"));
            let sec_open = html[..at]
                .rfind("<div class=\"section\"")
                .expect("the chart's section open tag");
            assert!(
                html[sec_open..at].contains("<div class=\"section-num\">1."),
                "#{id}'s section number must start \"1.\" so the export pair gate fires"
            );
        }
    }

    #[test]
    fn export_identity_titles_from_layout_and_the_nearest_heading() {
        let html = render_rows(&pair_test_rows());
        // The title comes from the chart's own Plotly title via `layout`, not
        // `_fullLayout` — Plotly fills `_fullLayout.title.text` with a "Click to
        // enter…" placeholder for a title-less chart, which would caption every
        // growth/ephemeris/STM/walk chart with the placeholder.
        assert!(
            html.contains("const lay = gd.layout") && html.contains("lay.title && lay.title.text"),
            "the title must be read from layout.title (never the _fullLayout placeholder)"
        );
        assert!(
            !html.contains("_fullLayout.title"),
            "the identity must not read the _fullLayout title placeholder"
        );
        // Charts with no Plotly title fall back to the nearest heading above them
        // in document order (panel title, else section title).
        assert!(
            html.contains("DOCUMENT_POSITION_FOLLOWING"),
            "the title falls back to the nearest preceding heading in the section"
        );
        // Caption and filename join the parts with an explicit separator (no glued
        // words like the old join(' ')).
        assert!(
            html.contains("[num, title, pair].filter(Boolean).join(' · ')"),
            "the caption must join number, title and pair with an explicit separator"
        );
    }

    #[test]
    fn s4_item1_header_sentences_become_a_units_key_line() {
        // S4 item 1 / S7: the former single "By tool and axis" matrix becomes one
        // per-axis readout, each stating its units on one short key line; the <th>
        // carry no header sentence. Fails on the old structure (one matrix with
        // "km · mas vs JPL · σ_eq · ms" and an .r1sub header sentence).
        let html = render_rows(&pair_test_rows());
        assert!(
            html.contains("<div class=\"grid-summary\">km</div>")
                && html.contains("<div class=\"grid-summary\">mas</div>")
                && html.contains("<div class=\"grid-summary\"><span class=\"u\">σ_eq</span></div>"),
            "each Part-2 section states its axis units on its own readout key line"
        );
        // The comparison reference rides the (uncounted) readout table header.
        assert!(
            html.contains("<th>propagation vs JPL</th>")
                && html.contains("<th>ephemeris vs JPL</th>")
                && html.contains("<th>orbit determination vs JPL</th>"),
            "the readout table header names the JPL comparison"
        );
        assert!(
            !html.contains("By tool and axis") && !html.contains("km · mas vs JPL · σ_eq · ms"),
            "the single combined by-tool-and-axis matrix is gone"
        );
        // Scope the header-sentence check to Part 2 (r1sub is a legitimate class
        // in the Part 4.2 ruler elsewhere).
        let p2s = html.find("id=\"part2\"").unwrap();
        let p2e = html[p2s..].find("id=\"part3\"").map(|i| p2s + i).unwrap();
        let part2 = &html[p2s..p2e];
        assert!(
            !part2.contains("r1sub") && !part2.contains("p50 · p90 · km"),
            "the readout <th> carry no header sentence — only the quantity name"
        );
        // The stat descriptors the sentence carried stay reachable in the Method folds.
        assert!(
            html.contains("median and p90 offset from Horizons")
                && html.contains("p50 wall clock at its own boundary"),
            "p50/p90/boundary descriptors remain in the Part-2 Method folds"
        );
    }

    #[test]
    fn s4_item3_panel_captions_and_summary_hovers_move_to_the_fold() {
        // S4 item 3: the 4.2 panel-title captions and the 1.3A/3.1/4.1 summary-line
        // hovers lose their title text; the content rides the Method fold.
        let rows = vec![synthetic_rust_prop_row("Apophis", 0.0)];
        let agg = synthetic_predict_aggregates("snap");
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&rows, &[], &out, None, Some(&agg), &[]).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        // The 4.2 panel titles carry no hover caption (fails on the old structure).
        assert!(
            html.contains("<div class=\"panel-title\">Coverage</div>")
                && html.contains("<div class=\"panel-title\">Surfaces</div>"),
            "the 4.2 panel titles carry no hover caption"
        );
        assert!(
            !html.contains("How often the next observations land inside the predicted"),
            "the Coverage panel caption is gone from the hover"
        );
        // The Surfaces provenance (snapshot) moved into its Method fold.
        assert!(
            html.contains("snapshot snap"),
            "the Surfaces provenance rides its Method fold, not the panel-title hover"
        );

        // The 3.1 summary-line hover is gone; its content rides the fold.
        let h2 = render_rows(&pair_test_rows());
        assert!(
            !h2.contains("Each counted channel reproduces empyrean-core to"),
            "the 3.1 summary-line hover caption is removed"
        );
        assert!(
            h2.contains("Every channel runs against empyrean-core, the validation reference"),
            "the 3.1 caption moved into the channel-fidelity Method fold"
        );
    }

    #[test]
    fn s4_item4_sigma_view_control_and_boundary_chips_are_gone() {
        // S4 item 4: the disabled σ-view control + tooltip are removed until the
        // view works, and the A/B/C boundary chips are gone (the boundary names
        // ride the group labels / §2.1 fold). Fails on the old structure.
        let html = render_rows(&pair_test_rows());
        assert!(
            !html.contains("id=\"sigma-toggle\"") && !html.contains("◎ σ view"),
            "the disabled σ-view button and its tooltip are removed"
        );
        assert!(
            !html.contains("class=\"tbchip\""),
            "no A/B/C boundary chip survives on any timing header"
        );
        // The boundary names stay visible in the Part 2 timing group labels.
        assert!(
            html.contains("propagation · integrate only")
                && html.contains("orbit determination · per fit"),
            "the boundary names stay in the timing group labels"
        );
    }

    #[test]
    fn s4_item2_growth_charts_use_one_hue_and_direct_labels() {
        // S4 item 2: the 1.1B / 1.2B 13-entry population legends become direct
        // end-labels in population order, one hue per chart; the Gaia/CCD refs
        // move to y-axis annotations. Fails on the old structure (popColors[pop]
        // per line, a 13-entry legend, Gaia/CCD as legend entries).
        let html = render_rows(&pair_test_rows());
        assert!(
            html.contains("const POP_HUE =") && html.contains("const color = POP_HUE;"),
            "the error-growth lines draw in one hue, not the 13-hue population palette"
        );
        assert!(
            html.contains("popLabels.push(")
                && html.contains("const dodged = popLabels.map(")
                && html.contains("const labelAnnos = dodged.map("),
            "each line carries a direct, vertically-dodged end-label built in population order"
        );
        assert!(
            html.contains("annotations: labelAnnos.concat(refAnnos)"),
            "the labels and the Gaia/CCD references ride annotations, not a legend"
        );
        // The Gaia/CCD reference lines are no longer legend entries.
        assert!(
            !html.contains("name: label, showlegend: true"),
            "the Gaia/CCD reference lines carry no legend entry (they are annotated)"
        );
    }

    #[test]
    fn export_stamp_grows_a_footer_band_and_restores_the_layout_exactly() {
        let html = render_rows(&pair_test_rows());
        // The caption used to be anchored into a 34px bottom margin on top of the
        // x-axis title, which it overprinted. It now grows the canvas by a footer
        // band and lays the stamp across that band, below the axis title.
        assert!(
            !html.contains("const pad = 34") && !html.contains("yshift: -(pad - 10)"),
            "the old top-anchored caption placement must be gone"
        );
        assert!(
            html.contains("const band = 58")
                && html.contains("const exportH = H + band")
                && html.contains("height: exportH"),
            "the export must grow the canvas by a footer band for the stamp"
        );
        // The finally block restores the layout exactly: keys that were absent are
        // cleared (null), not left as empty arrays.
        assert!(
            html.contains("const hadImages = 'images' in layout")
                && html.contains("hadImages ? prevImages : null")
                && html.contains("hadAnnotations ? prevAnnotations : null"),
            "the restore must clear images/annotations that were absent, not leave empty arrays"
        );
    }

    #[test]
    fn export_attribution_lives_only_in_script() {
        let html = render_rows(&pair_test_rows());
        // Pull the attribution string from its script const.
        let anchor = "const EMP_ATTRIBUTION = ";
        let at = html.find(anchor).expect("attribution const") + anchor.len();
        let rest = &html[at..];
        let close = rest[1..].find('"').unwrap() + 1;
        let attr = rest[1..close].to_string();
        // It must never appear as text a reader sees (scripts/styles stripped, then
        // tags dropped so only text nodes remain).
        let visible = text_only(&strip_tag_spans(&strip_tag_spans(&html, "script"), "style"));
        assert!(
            !visible.contains(&attr),
            "the attribution string must live only in the client script, not visible text: {attr}"
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
        // S3 item 3: one row per (object, coefficient). The per-channel `Channel`
        // column and the separate `Result` verdict column are gone from the §3.3
        // table (scoped, since the §1.2A outliers fold has its own Channel column);
        // the rust, cli and python channels collapse into one agreement column and
        // the absent C ABI channel `c` is one hatched column.
        let ng = {
            let s = html.find("id=\"s09b\"").expect("3.3 non-grav section");
            let e = html[s..]
                .find("id=\"part4\"")
                .map(|i| s + i)
                .unwrap_or(html.len());
            &html[s..e]
        };
        assert!(
            !ng.contains(">Channel</th>"),
            "the §3.3 per-channel Channel column must be gone"
        );
        assert!(
            !ng.contains("Result</th>") && !html.contains("pass ? '·' : '✕'"),
            "the separate Result glyph column must be gone (z carries the verdict)"
        );
        assert!(
            html.contains(">channels</th>") && html.contains(">c</th>"),
            "the new columns are the channels-agreement column and the c column"
        );
        // The rust/cli/python agreement collapses: a mark when they agree, else
        // the differing channel named.
        assert!(
            html.contains("rust, cli, python agree on fit and verdict")
                && html.contains("differ from rust:"),
            "the agreement column must collapse rust/cli/python with a named-difference fallback"
        );
        // The absent c channel is one hatched column, not a hatched row.
        assert!(
            html.contains("c (C ABI) emitted no non-grav row this run"),
            "the absent c channel must be one hatched column with its reason"
        );
        // One row per (object, coefficient): the object cell spans coeffs.length,
        // never the old channels × coeffs count.
        assert!(
            html.contains("rowspan=\"${coeffs.length}\"") && !html.contains("const totalSubRows"),
            "the object cell must span one row per coefficient, not per channel × coefficient"
        );
        // The z cell still carries the pass/fail verdict on its colour and hover.
        assert!(
            html.contains("pass · |z| ≤ 3σ") && html.contains("fail · |z| > 3σ"),
            "the z cell must carry the |z| ≤ 3 verdict on its hover"
        );
        for word in ["'PASS'", "'FAIL'", ">FAIL</td>", ">PASS</td>"] {
            assert!(
                !html.contains(word),
                "a verdict word {word} still renders as a cell"
            );
        }
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
            (
                "Configurations",
                "Configuration of each external reference tool in this run",
            ),
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
            "Configurations",
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
            ">ASSIST configurations</div>",
            "Provenance — frame, ephemeris, force model, references",
            "Methodology &middot; ASSIST arms",
        ] {
            assert!(
                !html.contains(gone),
                "old appendix text still present: {gone}"
            );
        }
    }

    /// S5a: the appendix "ASSIST configurations" (s05c) is now "Configurations"
    /// with one subsection per EXTERNAL tool that has rows in the run; empyrean
    /// is not a subsection and a tool with no rows emits nothing. Fails on the
    /// pre-S5a structure (the old title and the per-configuration agreement
    /// panels).
    #[test]
    fn configurations_appendix_has_one_subsection_per_external_tool_with_rows() {
        let note = |cfg: &str| -> String {
            match cfg {
                "assist_default" => "config=assist_default integrator=ias15 epsilon=0.000000001 min_dt=0 adaptive_mode=global initial_dt=0.001 (assist 1.2.3 rebound 4.6.0)".to_string(),
                "assist_layup" => "config=assist_layup integrator=ias15 epsilon=0.000000001 min_dt=0 adaptive_mode=prs23 initial_dt=0.001 (assist 1.2.3 rebound 4.6.0)".to_string(),
                _ => "config=assist_asteroid_institute integrator=ias15 epsilon=0.000001 min_dt=0.000000001 adaptive_mode=global initial_dt=0.000001 (assist 1.2.3 rebound 4.6.0)".to_string(),
            }
        };
        let mut rows = Vec::new();
        for (cfg, var) in [
            ("assist_default", "off"),
            ("assist_default", "on"),
            ("assist_layup", "off"),
            ("assist_layup", "on"),
            ("assist_asteroid_institute", "off"),
            ("assist_asteroid_institute", "on"),
        ] {
            let mut r = synthetic_rust_prop_row("Apophis", 0.0);
            r.channel = "assist".into();
            r.propagation_uncertainty = Some(format!("{cfg}_variational_{var}"));
            r.notes = note(cfg);
            rows.push(r);
        }
        // find_orb: present (a propagation-comparison field), no config recorded.
        let mut fo = synthetic_rust_prop_row("Apophis", 0.0);
        fo.findorb_vs_horizons_km = Some(1.5);
        rows.push(fo);
        // kete + jorbit: timing only, on core rows.
        let mut k = synthetic_rust_prop_row("Apophis", 0.0);
        k.channel = "core".into();
        k.kete_time_ms = Some(5.0);
        rows.push(k);
        let mut j = synthetic_rust_prop_row("Apophis", 0.0);
        j.channel = "core".into();
        j.jorbit_time_ms = Some(6.0);
        rows.push(j);

        let html = render_rows(&rows);

        // Title, hover and rail all read "Configurations"; the old name is gone.
        assert!(
            html.contains(">Configurations</div>")
                && html.contains(
                    "title=\"Configuration of each external reference tool in this run\""
                ),
            "the appendix title and hover must read Configurations"
        );
        assert!(
            html.contains(">Configurations</a>"),
            "the rail entry must read Configurations"
        );
        assert!(
            !html.contains("ASSIST configurations"),
            "the old 'ASSIST configurations' name must be gone"
        );
        // One subsection anchor per external tool WITH rows; none for tools with
        // no rows this run; none for empyrean.
        for present in ["s05c-assist", "s05c-findorb", "s05c-kete", "s05c-jorbit"] {
            assert!(
                html.contains(&format!("id=\"{present}\"")),
                "a subsection must render for the present external tool {present}"
            );
        }
        for absent in [
            "s05c-layup",
            "s05c-orbfit",
            "s05c-grss",
            "s05c-oorb",
            "s05c-adamcore",
            "s05c-empyrean",
        ] {
            assert!(
                !html.contains(&format!("id=\"{absent}\"")),
                "no subsection may render for {absent} (no rows / not external)"
            );
        }
        // ASSIST head states the recorded version; the arm table carries the
        // recorded settings read from the notes, nothing hand-typed.
        assert!(
            html.contains("ASSIST &middot; <code>1.2.3</code>"),
            "the ASSIST subsection head must state the recorded version"
        );
        for recorded in ["prs23", "global", "0.000000001", "0.000001"] {
            assert!(
                html.contains(recorded),
                "the ASSIST arm table must carry the recorded setting {recorded}"
            );
        }
        // find_orb ran but recorded no configuration: unrecorded settings render
        // as the one null token with "not recorded" on hover.
        assert!(
            html.contains("title=\"not recorded\""),
            "an unrecorded setting must render the null token with a not-recorded hover"
        );
        // Able to fail on the pre-S5a structure: the per-configuration agreement
        // panels and the JS mounts are gone.
        for gone in [
            ">Position agreement vs empyrean</div>",
            ">First-order agreement vs empyrean</div>",
            ">Resolved integrator settings</div>",
            "id=\"assist-arms-settings\"",
            "id=\"assist-arms-pos\"",
        ] {
            assert!(
                !html.contains(gone),
                "pre-S5a structure still present: {gone}"
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

    // ── Part 4.2 predictive power ────────────────────────────────────────

    fn pw_surface_cell(
        key: (&str, &str, &str),
        arc: (f64, f64),
        dt: (f64, f64),
        n_objects: u32,
        stats: (Option<f64>, Option<f64>),
    ) -> SurfaceCell {
        let (class, tool, arm) = key;
        let (sep, d2) = stats;
        SurfaceCell {
            class: class.into(),
            tool: tool.into(),
            config_arm: arm.into(),
            sigma_table: "pinned".into(),
            arc_lo_days: arc.0,
            arc_hi_days: arc.1,
            dt_lo_days: dt.0,
            dt_hi_days: dt.1,
            n_objects,
            n_windows: n_objects * 4,
            n_predictions: n_objects * 16,
            delivered_fraction: 0.9,
            med_sep_arcsec: sep,
            med_d2_norm: d2,
            cov_1s: Some(0.39),
            cov_2s: Some(0.86),
            cov_3s: Some(0.98),
            cov_1s_at: Some(0.66),
            cov_2s_at: Some(0.94),
            cov_3s_at: Some(0.99),
            cov_1s_ct: Some(0.71),
            cov_2s_ct: Some(0.96),
            cov_3s_ct: Some(0.995),
            exp_cov_1s: Some(0.3935),
            exp_cov_2s: Some(0.8647),
            exp_cov_3s: Some(0.9889),
            exp_cov_1s_at: Some(0.6827),
            exp_cov_2s_at: Some(0.9545),
            exp_cov_3s_at: Some(0.9973),
            exp_cov_1s_ct: Some(0.6827),
            exp_cov_2s_ct: Some(0.9545),
            exp_cov_3s_ct: Some(0.9973),
            n_nights: n_objects * 2,
            med_z2_at_norm: d2,
            med_z2_ct_norm: d2.map(|v| v * 0.8),
            n_z_at: n_objects * 2,
            n_z_ct: n_objects * 2,
            coverage_law_aware: true,
            cov_1s_row: Some(0.40),
            cov_2s_row: Some(0.85),
            cov_3s_row: Some(0.97),
            cov_1s_at_row: Some(0.67),
            cov_2s_at_row: Some(0.93),
            cov_3s_at_row: Some(0.99),
            cov_1s_ct_row: Some(0.70),
            cov_2s_ct_row: Some(0.95),
            cov_3s_ct_row: Some(0.99),
            cov_1s_unc: Some(0.37),
            cov_2s_unc: Some(0.82),
            cov_3s_unc: Some(0.96),
            cov_1s_at_unc: Some(0.64),
            cov_2s_at_unc: Some(0.92),
            cov_3s_at_unc: Some(0.98),
            cov_1s_ct_unc: Some(0.69),
            cov_2s_ct_unc: Some(0.94),
            cov_3s_ct_unc: Some(0.99),
            cov_2s_row_unc: Some(0.80),
            cov_2s_at_row_unc: Some(0.91),
            cov_2s_ct_row_unc: Some(0.93),
            // Gated share 12/16 = 75 % (the 4.2B realism hover reads this pair);
            // the unc pair is deliberately distinct (5/20 = 25 %) so a 4.2B read
            // of the wrong pair would fail the 75 % assertion.
            n_rows: n_objects * 16,
            n_rows_pd: n_objects * 12,
            n_rows_unc: n_objects * 20,
            n_rows_pd_unc: n_objects * 5,
            n_objects_all: n_objects,
            n_objects_trusted: n_objects,
        }
    }

    /// A pooled 4.2A coverage row (one arc side × dt bin). `obs2` is the
    /// trust-unconditioned joint 2σ rate the matrix cell shows; `share` is the
    /// Σ_pred-dominated fraction of rows (below 0.10 hatches the cell).
    fn pw_coverage_cell(
        key: (&str, &str, &str),
        arc_short: bool,
        dt: (f64, f64),
        counts: (u32, u32),
        obs2: Option<f64>,
        cond2: Option<f64>,
        share: f64,
    ) -> SurfaceCell {
        let (class, tool, arm) = key;
        let (n_all, n_trusted) = counts;
        let (arc_lo_days, arc_hi_days) = if arc_short {
            (1.0, COVERAGE_ARC_SPLIT_DAYS)
        } else {
            (COVERAGE_ARC_SPLIT_DAYS, 30000.0)
        };
        // 4.2A reads the UNCONDITIONED share, so `share` drives the unc pair; the
        // gated pair is deliberately empty (0 % share) so a cell that wrongly
        // read it would hatch and fail the numeric assertions.
        let n_rows_unc = 40u32;
        let n_rows_pd_unc = (f64::from(n_rows_unc) * share).round() as u32;
        let n_rows = 40u32;
        let n_rows_pd = 0u32;
        // Expected present only where the observed value is (the cell gates on it).
        let exp = |v: f64| obs2.map(|_| v);
        SurfaceCell {
            class: class.into(),
            tool: tool.into(),
            config_arm: arm.into(),
            sigma_table: "pinned".into(),
            arc_lo_days,
            arc_hi_days,
            dt_lo_days: dt.0,
            dt_hi_days: dt.1,
            n_objects: n_all,
            n_windows: 0,
            n_predictions: 0,
            delivered_fraction: 0.0,
            med_sep_arcsec: None,
            med_d2_norm: None,
            cov_1s: cond2.map(|v| v - 0.4),
            cov_2s: cond2,
            cov_3s: cond2.map(|v| (v + 0.1).min(1.0)),
            cov_1s_at: cond2.map(|v| v - 0.2),
            cov_2s_at: cond2,
            cov_3s_at: cond2.map(|v| (v + 0.05).min(1.0)),
            cov_1s_ct: cond2.map(|v| v - 0.2),
            cov_2s_ct: cond2,
            cov_3s_ct: cond2.map(|v| (v + 0.05).min(1.0)),
            exp_cov_1s: exp(0.3935),
            exp_cov_2s: exp(0.8647),
            exp_cov_3s: exp(0.9889),
            exp_cov_1s_at: exp(0.6827),
            exp_cov_2s_at: exp(0.9545),
            exp_cov_3s_at: exp(0.9973),
            exp_cov_1s_ct: exp(0.6827),
            exp_cov_2s_ct: exp(0.9545),
            exp_cov_3s_ct: exp(0.9973),
            n_nights: n_all * 3,
            med_z2_at_norm: None,
            med_z2_ct_norm: None,
            n_z_at: 0,
            n_z_ct: 0,
            coverage_law_aware: true,
            cov_1s_row: None,
            cov_2s_row: None,
            cov_3s_row: None,
            cov_1s_at_row: None,
            cov_2s_at_row: None,
            cov_3s_at_row: None,
            cov_1s_ct_row: None,
            cov_2s_ct_row: None,
            cov_3s_ct_row: None,
            cov_1s_unc: obs2.map(|v| (v - 0.4).max(0.0)),
            cov_2s_unc: obs2,
            cov_3s_unc: obs2.map(|v| (v + 0.1).min(1.0)),
            cov_1s_at_unc: obs2.map(|v| (v - 0.2).max(0.0)),
            cov_2s_at_unc: obs2,
            cov_3s_at_unc: obs2.map(|v| (v + 0.05).min(1.0)),
            cov_1s_ct_unc: obs2.map(|v| (v - 0.2).max(0.0)),
            cov_2s_ct_unc: obs2,
            cov_3s_ct_unc: obs2.map(|v| (v + 0.05).min(1.0)),
            cov_2s_row_unc: obs2.map(|v| (v + 0.03).min(1.0)),
            cov_2s_at_row_unc: obs2,
            cov_2s_ct_row_unc: obs2,
            n_rows,
            n_rows_pd,
            n_rows_unc,
            n_rows_pd_unc,
            n_objects_all: n_all,
            n_objects_trusted: n_trusted,
        }
    }

    /// A synthetic walk aggregate exercising every 4.2 sub-panel: a well-sampled
    /// cell, a thin (n < 3) cell, two arms, a per-object walk (one all-failed),
    /// an empty calibration family, and a one-object timeline.
    fn synthetic_predict_aggregates(snapshot: &str) -> PredictAggregates {
        // Mirrors production arm naming: rust arms are dot/plus-delimited (never
        // "default"), and the single "default" arm belongs to another tool — so
        // (rust, "default") does not co-occur, exactly the shape that emptied the
        // default surface, the marginals and the per-object matrix before the
        // primary-series fix. The full arm axis (4) also exceeds the rust arm count
        // (3), exercising the 4.2C/4.2E reconciliation, and the '.'/'+' slugs
        // exercise the main-effects tokeniser.
        PredictAggregates {
            snapshot_id: snapshot.into(),
            profile: "ci".into(),
            cells: vec![
                // Primary rust arm, two dt bins, both n >= 3 (4.2B populates).
                pw_surface_cell(
                    ("NEA", "rust", "efcc.adap.nonight"),
                    (1.0, 3.0),
                    (0.3, 1.0),
                    5,
                    (Some(0.8), Some(1.0)),
                ),
                pw_surface_cell(
                    ("NEA", "rust", "efcc.adap.nonight"),
                    (3.0, 10.0),
                    (1.0, 3.0),
                    5,
                    (Some(8.0), Some(12.0)),
                ),
                // Below the three-object floor: must hatch, never pool.
                pw_surface_cell(
                    ("NEA", "rust", "efcc.adap.nonight"),
                    (1.0, 3.0),
                    (1.0, 3.0),
                    2,
                    (Some(0.5), Some(0.3)),
                ),
                // Two more rust arms (a '.'-only and a '+'-transport slug) so the
                // main-effects tokeniser has factor tokens shared across '.' and '+'.
                pw_surface_cell(
                    ("NEA", "rust", "nodeb.adap.nonight+mc"),
                    (1.0, 3.0),
                    (0.3, 1.0),
                    4,
                    (Some(1.2), Some(2.0)),
                ),
                pw_surface_cell(
                    ("NEA", "rust", "efcc.adap.vfc+mc"),
                    (1.0, 3.0),
                    (0.3, 1.0),
                    4,
                    (Some(1.5), Some(0.6)),
                ),
                // Another tool's single "default" arm: present in the cells, so the
                // full arm axis (4) exceeds the rust arm count (3); 4.2C/4.2E render
                // it as an explicit hatched row, never dropped.
                pw_surface_cell(
                    ("NEA", "findorb", "default"),
                    (1.0, 3.0),
                    (0.3, 1.0),
                    5,
                    (Some(2.0), Some(1.5)),
                ),
            ],
            coverage_cells: vec![
                // rust at its primary arm: a short-arc and a long-arc row with
                // numeric cells (short-arc the more over-confident), a Σ_pred-share
                // hatch (dt 1–3 d short-arc), and a thin hatch (dt 1–3 d long-arc).
                pw_coverage_cell(
                    ("NEA", "rust", "efcc.adap.nonight"),
                    true,
                    (0.3, 1.0),
                    (5, 3),
                    Some(0.78),
                    Some(0.90),
                    0.60,
                ),
                pw_coverage_cell(
                    ("NEA", "rust", "efcc.adap.nonight"),
                    false,
                    (0.3, 1.0),
                    (4, 4),
                    Some(0.40),
                    Some(0.42),
                    0.50,
                ),
                pw_coverage_cell(
                    ("NEA", "rust", "efcc.adap.nonight"),
                    true,
                    (1.0, 3.0),
                    (5, 3),
                    Some(0.80),
                    Some(0.88),
                    0.05,
                ),
                pw_coverage_cell(
                    ("NEA", "rust", "efcc.adap.nonight"),
                    false,
                    (1.0, 3.0),
                    (2, 2),
                    Some(0.70),
                    Some(0.72),
                    0.50,
                ),
                // find_orb at its default arm.
                pw_coverage_cell(
                    ("NEA", "findorb", "default"),
                    true,
                    (0.3, 1.0),
                    (4, 4),
                    Some(0.70),
                    Some(0.70),
                    0.30,
                ),
                pw_coverage_cell(
                    ("NEA", "findorb", "default"),
                    false,
                    (0.3, 1.0),
                    (3, 3),
                    Some(0.99),
                    Some(0.99),
                    0.40,
                ),
            ],
            per_object: vec![
                ObjectWalkSummary {
                    object: "Apophis".into(),
                    class: "NEA".into(),
                    tool: "rust".into(),
                    config_arm: "efcc.adap.nonight".into(),
                    n_windows_expected: 4,
                    n_windows_converged: 3,
                    n_windows_failed: 1,
                    n_predictions: 16,
                    med_d2_norm: Some(1.1),
                    n_predictions_incl_flagged: 0,
                    med_d2_norm_incl_flagged: None,
                    flag_counts: Default::default(),
                    trust_reasons: Default::default(),
                    med_sep_arcsec: Some(0.9),
                    cov_1s: Some(0.40),
                    cov_2s: Some(0.85),
                    cov_3s: Some(0.97),
                    cov_1s_at: Some(0.64),
                    cov_2s_at: Some(0.93),
                    cov_3s_at: Some(0.99),
                    cov_1s_ct: Some(0.70),
                    cov_2s_ct: Some(0.95),
                    cov_3s_ct: Some(0.99),
                    n_nights: 8,
                    med_z2_at_norm: Some(1.2),
                    med_z2_ct_norm: Some(0.9),
                    n_z_at: 8,
                    n_z_ct: 8,
                },
                // An object no window converged on: the hatched row.
                ObjectWalkSummary {
                    object: "Bennu".into(),
                    class: "NEA".into(),
                    tool: "rust".into(),
                    config_arm: "efcc.adap.nonight".into(),
                    n_windows_expected: 4,
                    n_windows_converged: 0,
                    n_windows_failed: 4,
                    n_predictions: 0,
                    med_d2_norm: None,
                    n_predictions_incl_flagged: 0,
                    med_d2_norm_incl_flagged: None,
                    flag_counts: Default::default(),
                    trust_reasons: Default::default(),
                    med_sep_arcsec: None,
                    cov_1s: None,
                    cov_2s: None,
                    cov_3s: None,
                    cov_1s_at: None,
                    cov_2s_at: None,
                    cov_3s_at: None,
                    cov_1s_ct: None,
                    cov_2s_ct: None,
                    cov_3s_ct: None,
                    n_nights: 0,
                    med_z2_at_norm: None,
                    med_z2_ct_norm: None,
                    n_z_at: 0,
                    n_z_ct: 0,
                },
                // The other rust arms carry a per-object realism so the main-effects
                // sweep marginalizes real numbers across shared factor tokens.
                ObjectWalkSummary {
                    object: "Apophis".into(),
                    class: "NEA".into(),
                    tool: "rust".into(),
                    config_arm: "nodeb.adap.nonight+mc".into(),
                    n_windows_expected: 4,
                    n_windows_converged: 4,
                    n_windows_failed: 0,
                    n_predictions: 16,
                    med_d2_norm: Some(1.3),
                    n_predictions_incl_flagged: 0,
                    med_d2_norm_incl_flagged: None,
                    flag_counts: Default::default(),
                    trust_reasons: Default::default(),
                    med_sep_arcsec: Some(1.0),
                    cov_1s: Some(0.41),
                    cov_2s: Some(0.87),
                    cov_3s: Some(0.98),
                    cov_1s_at: Some(0.67),
                    cov_2s_at: Some(0.95),
                    cov_3s_at: Some(0.99),
                    cov_1s_ct: Some(0.69),
                    cov_2s_ct: Some(0.95),
                    cov_3s_ct: Some(0.99),
                    n_nights: 8,
                    med_z2_at_norm: Some(1.4),
                    med_z2_ct_norm: Some(1.0),
                    n_z_at: 8,
                    n_z_ct: 8,
                },
                ObjectWalkSummary {
                    object: "Apophis".into(),
                    class: "NEA".into(),
                    tool: "rust".into(),
                    config_arm: "efcc.adap.vfc+mc".into(),
                    n_windows_expected: 4,
                    n_windows_converged: 4,
                    n_windows_failed: 0,
                    n_predictions: 16,
                    med_d2_norm: Some(0.7),
                    n_predictions_incl_flagged: 0,
                    med_d2_norm_incl_flagged: None,
                    flag_counts: Default::default(),
                    trust_reasons: Default::default(),
                    med_sep_arcsec: Some(0.8),
                    cov_1s: Some(0.38),
                    cov_2s: Some(0.84),
                    cov_3s: Some(0.97),
                    cov_1s_at: Some(0.63),
                    cov_2s_at: Some(0.92),
                    cov_3s_at: Some(0.98),
                    cov_1s_ct: Some(0.72),
                    cov_2s_ct: Some(0.96),
                    cov_3s_ct: Some(0.995),
                    n_nights: 8,
                    med_z2_at_norm: Some(0.8),
                    med_z2_ct_norm: Some(0.6),
                    n_z_at: 8,
                    n_z_ct: 8,
                },
                // The non-rust "default" arm's walk: present, but 4.2C/4.2E key on
                // rust, so this arm renders hatched there, not from this row.
                ObjectWalkSummary {
                    object: "Apophis".into(),
                    class: "NEA".into(),
                    tool: "findorb".into(),
                    config_arm: "default".into(),
                    n_windows_expected: 4,
                    n_windows_converged: 4,
                    n_windows_failed: 0,
                    n_predictions: 16,
                    med_d2_norm: Some(1.8),
                    n_predictions_incl_flagged: 0,
                    med_d2_norm_incl_flagged: None,
                    flag_counts: Default::default(),
                    trust_reasons: Default::default(),
                    med_sep_arcsec: Some(2.0),
                    cov_1s: Some(0.35),
                    cov_2s: Some(0.80),
                    cov_3s: Some(0.95),
                    cov_1s_at: Some(0.60),
                    cov_2s_at: Some(0.90),
                    cov_3s_at: Some(0.98),
                    cov_1s_ct: Some(0.66),
                    cov_2s_ct: Some(0.93),
                    cov_3s_ct: Some(0.99),
                    n_nights: 8,
                    med_z2_at_norm: Some(1.9),
                    med_z2_ct_norm: Some(1.3),
                    n_z_at: 8,
                    n_z_ct: 8,
                },
            ],
            d2_histograms: Vec::new(),
            per_window: vec![crate::predict_schema::WalkTimeline {
                tool: "rust".into(),
                config_arm: "efcc.adap.nonight".into(),
                object: "Apophis".into(),
                class: "NEA".into(),
                window_index: vec![0, 1, 2],
                arc_days: vec![1.0, 3.0, 10.0],
                converged: vec![true, true, false],
                med_d2_norm: vec![Some(1.0), Some(1.2), None],
                med_sep_arcsec: vec![Some(0.8), Some(0.9), None],
                n_preds: vec![4, 5, 0],
            }],
            reduced_chi2: Vec::new(),
            per_window_note: None,
            // Empty: the D-003 family has no data until a Student-t arm runs.
            calibration: Vec::new(),
            calibration_note: None,
            // Empty: the production synthetic walk records no engine version,
            // so the fixture exercises the "engine not recorded" marked state.
            engine_versions: Vec::new(),
        }
    }

    /// A minimal per-object walk summary for the run-block fixture.
    fn run_ows(object: &str, arm: &str, med_d2: f64) -> ObjectWalkSummary {
        ObjectWalkSummary {
            object: object.into(),
            class: "NEA".into(),
            tool: "rust".into(),
            config_arm: arm.into(),
            n_windows_expected: 4,
            n_windows_converged: 4,
            n_windows_failed: 0,
            n_predictions: 16,
            med_d2_norm: Some(med_d2),
            n_predictions_incl_flagged: 0,
            med_d2_norm_incl_flagged: None,
            flag_counts: Default::default(),
            trust_reasons: Default::default(),
            med_sep_arcsec: Some(0.9),
            cov_1s: Some(0.39),
            cov_2s: Some(0.86),
            cov_3s: Some(0.98),
            cov_1s_at: Some(0.66),
            cov_2s_at: Some(0.94),
            cov_3s_at: Some(0.99),
            cov_1s_ct: Some(0.71),
            cov_2s_ct: Some(0.96),
            cov_3s_ct: Some(0.99),
            n_nights: 8,
            med_z2_at_norm: Some(med_d2),
            med_z2_ct_norm: Some(med_d2 * 0.8),
            n_z_at: 8,
            n_z_ct: 8,
        }
    }

    /// A (class = all) calibration cell for the run-block fixture, at a given
    /// stratum / trace band.
    #[allow(clippy::too_many_arguments)]
    fn run_cal(
        arm: &str,
        law: &str,
        ref_law: &str,
        mls: f64,
        se: f64,
        stratum: &str,
        band: Option<&str>,
    ) -> crate::predict_schema::CalibrationCell {
        crate::predict_schema::CalibrationCell {
            tool: "rust".into(),
            config_arm: arm.into(),
            sigma_table: "pinned".into(),
            class: "all".into(),
            scoring_law: law.into(),
            reference_law: ref_law.into(),
            stratum: stratum.into(),
            arc_lo_days: None,
            arc_hi_days: None,
            dt_lo_days: None,
            dt_hi_days: None,
            trace_band: band.map(Into::into),
            n_rows: 1000,
            n_nights: 500,
            mean_log_score: Some(mls),
            mean_log_score_se: Some(se),
            baseline_series: Some("efcc.adap.nonight.n".into()),
            paired_dlog_score: None,
            paired_dlog_score_se: None,
            n_paired_nights: None,
            n_paired_rows: None,
            cvm_all_rows: None,
            n_pit: 0,
            cvm_per_night: None,
            n_pit_nights: 0,
            pit_hist: Vec::new(),
            cov_obs_1: None,
            cov_obs_4: None,
            cov_obs_9: None,
            cov_exp_1: None,
            cov_exp_4: None,
            cov_exp_9: None,
            n_cov: 0,
            cov_obs_1_at: None,
            cov_obs_4_at: None,
            cov_obs_9_at: None,
            cov_obs_1_ct: None,
            cov_obs_4_ct: None,
            cov_obs_9_ct: None,
            cov_exp_1_dir: None,
            cov_exp_4_dir: None,
            cov_exp_9_dir: None,
            med_z2_at_norm: None,
            med_z2_ct_norm: None,
            n_cov_at: 0,
            n_cov_ct: 0,
        }
    }

    /// A synthetic additional-RUN aggregate: three arms over the error-model
    /// (normal / Student-t ν=4) × rejection (adaptive / blunder) × information
    /// (observed / expected) axes, each scored under its own law, with a
    /// populated calibration ranking. `engine` sets the recorded engine
    /// version(s) (None → the walk recorded none).
    fn synthetic_run_aggregate(engine: Option<&str>) -> PredictAggregates {
        // (arm, law, ref_law, overall mLS, overall SE, obs-dominated mLS, obs SE).
        let arms = [
            (
                "efcc.adap.nonight.n",
                "normal",
                "chi2_2",
                -5.69f64,
                0.38,
                -3.06,
                0.23,
            ),
            (
                "efcc.blun.nonight.t4",
                "student-t",
                "student_t(nu=4)",
                -2.70,
                0.11,
                -1.66,
                0.03,
            ),
            (
                "efcc.blun.nonight.exp.t4",
                "student-t",
                "student_t(nu=4)",
                -2.67,
                0.12,
                -1.71,
                0.03,
            ),
        ];
        let mut cells = Vec::new();
        let mut per_object = Vec::new();
        let mut calibration = Vec::new();
        for (i, (arm, law, ref_law, mls, se, od_mls, od_se)) in arms.iter().enumerate() {
            // One well-populated surface cell per arm (n ≥ 3) so coverage reads.
            cells.push(pw_surface_cell(
                ("NEA", "rust", arm),
                (1.0, 3.0),
                (0.3, 1.0),
                5,
                (Some(0.9), Some(1.0 + i as f64 * 0.1)),
            ));
            // Three objects per arm so the realism median has support.
            for obj in ["Apophis", "Bennu", "Eros"] {
                per_object.push(run_ows(obj, arm, 1.0 + i as f64 * 0.1));
            }
            // Both strata of the ranking: all held-out rows (overall) and the
            // observation-dominated rows (trace ratio < 0.1).
            calibration.push(run_cal(arm, law, ref_law, *mls, *se, "overall", None));
            calibration.push(run_cal(
                arm,
                law,
                ref_law,
                *od_mls,
                *od_se,
                "trace_band",
                Some("<0.1"),
            ));
        }
        let engine_versions = engine
            .map(|v| {
                arms.iter()
                    .map(|(arm, ..)| crate::predict_schema::SeriesEngineVersions {
                        tool: "rust".into(),
                        config_arm: (*arm).into(),
                        versions: vec![v.to_string()],
                    })
                    .collect()
            })
            .unwrap_or_default();
        PredictAggregates {
            snapshot_id: "2026-07-29-75ab459bc8b2".into(),
            profile: "ci".into(),
            cells,
            coverage_cells: Vec::new(),
            per_object,
            d2_histograms: Vec::new(),
            per_window: Vec::new(),
            reduced_chi2: Vec::new(),
            per_window_note: None,
            calibration,
            calibration_note: None,
            engine_versions,
        }
    }

    #[test]
    fn error_model_vocabulary_maps_tokens_to_reader_words() {
        // The new factor tokens decode to reader words; the raw tokens never
        // escape the mapping functions.
        assert!(pw_is_student_t_token("t4"));
        assert!(pw_is_student_t_token("t12"));
        assert!(!pw_is_student_t_token("nonight"));
        assert!(!pw_is_student_t_token("t"));
        assert_eq!(
            pw_error_model_word("efcc.adap.nonight.n").as_deref(),
            Some("normal")
        );
        assert_eq!(
            pw_error_model_word("efcc.blun.nonight.t4").as_deref(),
            Some("Student-t (ν=4)")
        );
        assert_eq!(
            pw_error_model_word("efcc.blun.nonight.t6").as_deref(),
            Some("Student-t (ν=6)")
        );
        assert_eq!(pw_error_model_word("efcc.adap.vfc"), None);
        assert_eq!(
            pw_information_word("efcc.blun.nonight.exp.t4"),
            Some("expected information")
        );
        assert_eq!(
            pw_information_word("efcc.blun.nonight.t4"),
            Some("observed information")
        );
        assert_eq!(pw_information_word("efcc.adap.vfc"), None);
        assert_eq!(pw_token_factor("n"), "error model");
        assert_eq!(pw_token_factor("t4"), "error model");
        assert_eq!(pw_token_factor("exp"), "information");
        assert_eq!(pw_token_factor("blun"), "rejection");
        assert_eq!(pw_factor_word(1, "blun"), "blunder");
    }

    #[test]
    fn varying_factors_include_error_model_and_information_not_constants() {
        let arms = [
            "efcc.adap.nonight.n",
            "efcc.blun.nonight.t4",
            "efcc.blun.nonight.exp.t4",
        ];
        let varying = pw_varying_factors(&arms);
        // debias (all efcc) and nightly (all nonight) and transport (all linear)
        // are constant → not swept. Rejection, error model, information vary.
        assert!(varying.contains(&PwFactor::Rejection));
        assert!(varying.contains(&PwFactor::ErrorModel));
        assert!(varying.contains(&PwFactor::Information));
        assert!(!varying.contains(&PwFactor::Debias));
        assert!(!varying.contains(&PwFactor::Nightly));
        assert!(!varying.contains(&PwFactor::Transport));
    }

    #[test]
    fn run_block_renders_its_own_block_with_vocabulary_and_ranking() {
        let rows = vec![synthetic_rust_prop_row("Apophis", 0.0)];
        let agg = synthetic_predict_aggregates("2026-07-29-75ab459bc8b2");
        let run = synthetic_run_aggregate(None);
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(
            &rows,
            &[],
            &out,
            None,
            Some(&agg),
            std::slice::from_ref(&run),
        )
        .unwrap();
        let html = std::fs::read_to_string(&out).unwrap();

        // The block renders as its own section with a head line.
        assert!(
            html.contains(">Additional run</div>"),
            "run block panel-title"
        );
        // Head line fields, each a named value (engine not recorded by this walk).
        assert!(
            html.contains("engine not recorded"),
            "a walk with no recorded engine version states the named absent state"
        );
        assert!(
            html.contains("snapshot 2026-07-29-75ab459bc8b2"),
            "head states the snapshot"
        );
        assert!(html.contains("3 arms"), "head states the arm count");
        assert!(
            html.contains("rows · all populations"),
            "head names the pooled population scope (every population, class = all)"
        );
        // Error-model + information vocabulary on the page; never the raw tokens.
        assert!(
            html.contains("Student-t (ν=4)"),
            "the error-model word renders"
        );
        assert!(
            html.contains("expected information"),
            "the information word renders"
        );
        assert!(
            html.contains("observed information"),
            "the default information word renders"
        );
        assert!(
            html.contains("blunder"),
            "the blunder rejection word renders"
        );
        // The populated D-003 ranking: both stratum columns named, the
        // difference-of-means Δ column, and the baseline mark.
        assert!(
            html.contains("all held-out rows"),
            "the ranking names the all-held-out-rows regime"
        );
        assert!(
            html.contains("observation-dominated rows (trace ratio &lt; 0.1)"),
            "the ranking names the observation-dominated regime"
        );
        assert!(
            html.contains("Δ vs baseline"),
            "the ranking carries the difference-vs-baseline column"
        );
        assert!(
            html.contains("difference of means; SE in quadrature"),
            "the Δ header hover states it is a difference of means, not paired"
        );
        assert!(
            html.contains(">baseline</span>"),
            "the baseline arm's Δ cell is marked, not blank"
        );
        assert!(
            !html.contains("awaiting Student-t chain</td><td"),
            "the run block's ranking is populated, not the marked-absent state"
        );

        // No raw arm slug leaks onto the page (text or hover) for the run arms.
        for slug in [
            "efcc.blun.nonight.exp.t4",
            "efcc.blun.nonight.t4",
            "efcc.adap.nonight.n",
        ] {
            assert!(
                !html.contains(slug),
                "the raw arm slug {slug} must never appear on the page"
            );
        }
    }

    #[test]
    fn run_block_prints_recorded_engine_version() {
        let rows = vec![synthetic_rust_prop_row("Apophis", 0.0)];
        let agg = synthetic_predict_aggregates("2026-07-29-75ab459bc8b2");
        let run = synthetic_run_aggregate(Some("empyrean 0.10.0"));
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(
            &rows,
            &[],
            &out,
            None,
            Some(&agg),
            std::slice::from_ref(&run),
        )
        .unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        assert!(
            html.contains("engine empyrean 0.10.0"),
            "a walk that recorded its engine version prints it in the head line"
        );
        assert!(
            !html.contains("engine not recorded"),
            "the recorded-engine head must not also state the absent marker"
        );
    }

    /// True when the page carries a "D-00<digit>" ruling id anywhere.
    fn contains_ruling_id(html: &str) -> bool {
        html.match_indices("D-00").any(|(i, _)| {
            html[i + 4..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit())
        })
    }

    #[test]
    fn predict_aggregates_profile_defaults_to_unstated() {
        // An aggregate file written before the profile field parses with the
        // marked-absent label, never a guessed profile.
        let json = r#"{"snapshot_id":"snap","cells":[],"per_object":[]}"#;
        let agg: PredictAggregates = serde_json::from_str(json).unwrap();
        assert_eq!(agg.profile, "unstated");
    }

    #[test]
    fn realism_bin_is_diverging_about_one() {
        assert_eq!(realism_bin(1.0).0, "rv0", "calibrated is the centre");
        assert_eq!(realism_bin(0.1).1, "over-conservative", "m<1 is blue");
        assert_eq!(realism_bin(10.0).1, "over-confident", "m>1 is red");
        assert_eq!(realism_bin(0.01).0, "rvb3", "far below is the deepest blue");
        assert_eq!(realism_bin(100.0).0, "rvr3", "far above is the deepest red");
    }

    #[test]
    fn report_renders_predictive_power_with_a_walk() {
        let rows = vec![synthetic_rust_prop_row("Apophis", 0.0)];
        let agg = synthetic_predict_aggregates("2026-07-29-snap");
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&rows, &[], &out, None, Some(&agg), &[]).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        assert!(html.contains("id=\"s15\""), "missing 4.2 anchor");
        assert!(html.contains("class=\"agrid pw-grid\""), "no 4.2 grids");
        assert!(html.contains("class=\"gc rv"), "no diverging realism cells");
        assert!(
            html.contains("class=\"gc gb"),
            "no decade-ladder accuracy cells"
        );
        assert!(
            html.contains("log₂ · diverging"),
            "the realism key must state the exception"
        );
        assert!(
            !contains_ruling_id(&html),
            "a ruling id shipped in the page"
        );
        assert!(
            html.contains("id=\"pw-timeline-object\""),
            "4.2F must carry its object select"
        );
        // Provenance (profile) is baked into the server-rendered reading line.
        assert!(html.contains("profile ci"), "profile must be stated");
    }

    #[test]
    fn predictive_power_matrices_populate_on_production_like_arms() {
        // With production-like rust arms (dot/plus-delimited, never "default"), the
        // single-arm views (4.2B marginals, 4.2D per-object) and the main-effects
        // sub-matrix must each render real numbers. Before the primary-series fix
        // they selected the never-co-occurring (rust, "default") slice and rendered
        // all-hatched; before the token-split fix the sub-matrix was empty — both
        // while the gate stayed green. Each assertion fails on the broken render.
        let rows = vec![synthetic_rust_prop_row("Apophis", 0.0)];
        let agg = synthetic_predict_aggregates("snap");
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&rows, &[], &out, None, Some(&agg), &[]).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();

        // Count the numeric value spans (`<span class="cn">…`) inside one panel's
        // slice of the page; an all-hatched matrix carries none.
        let numeric_cells = |start: &str, end: &str| -> usize {
            let s = html
                .find(start)
                .unwrap_or_else(|| panic!("missing section marker: {start}"));
            let e = html[s..].find(end).map(|i| s + i).unwrap_or(html.len());
            html[s..e].matches("<span class=\"cn\">").count()
        };
        // 4.2A coverage leads and renders real inside-ellipse rates.
        assert!(
            numeric_cells("Coverage</div>", "Surfaces</div>") > 0,
            "4.2A coverage must render numeric inside-ellipse rates, not an all-hatched matrix"
        );
        assert!(
            numeric_cells("Horizon marginals</div>", "Configuration arms</div>") > 0,
            "4.2C marginals must render numeric cells, not an all-hatched matrix"
        );
        assert!(
            numeric_cells("Configuration arms</div>", "Per-object walk</div>") > 0,
            "4.2D main-effects table (arms + calibration merged in S3) must render at least one numeric row"
        );
        // The merged arms+calibration section carries the Calibration content (the
        // 1σ/2σ columns and the log-score mark) without a separate Calibration
        // panel-title, so the per-object walk is now bounded by the timeline.
        assert!(
            numeric_cells("Per-object walk</div>", "Walk timeline</div>") > 0,
            "4.2E per-object matrix must render at least one numeric row"
        );
        assert!(
            !html.contains(">Calibration</div>"),
            "Calibration folds into the merged Configuration arms section (S3 item 2)"
        );
        assert!(
            html.contains(">log-score absent</div>"),
            "the merged section keeps the log-score-absent mark"
        );
        // The full arm axis (4) exceeds the rust arm count (3); the surfaces
        // provenance (folded into the panel-title hover) states 4 arms, and the
        // non-rust arm is hatched with its reason in the arm grid, never dropped.
        assert!(
            html.contains("4 arms, 1 classes, profile ci"),
            "the surfaces provenance must count the full arm axis (4)"
        );
        assert!(
            html.contains("no rust walk for this arm"),
            "an arm no rust walk produced must render hatched with its reason"
        );
    }

    #[test]
    fn coverage_headline_anchors_render_on_the_face() {
        // 4.2A is the owner's answer-bearing panel: it must state, without a hover,
        // the diverging scale and expected anchors (ABOVE the matrices, not below),
        // that the cell is the observed 2σ inside-ellipse percent, the dof of each
        // direction, the horizon unit, and the covariance arm each tool row is
        // scored on. All but the two key labels ride table-exempt header text, so
        // this adds no visible-word-budget cost beyond the two labels.
        let rows = vec![synthetic_rust_prop_row("Apophis", 0.0)];
        let agg = synthetic_predict_aggregates("snap");
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&rows, &[], &out, None, Some(&agg), &[]).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();

        let cs = html.find(">Coverage</div>").expect("4.2A Coverage panel");
        let ce = html[cs..]
            .find("Surfaces</div>")
            .map(|i| cs + i)
            .expect("end of 4.2A block");
        let cov = &html[cs..ce];

        // Finding 3: the key (scale + expected anchors) leads, above the first matrix.
        let key_at = cov
            .find("class=\"grid-key\"")
            .expect("coverage key present");
        let grid_at = cov
            .find("<table class=\"agrid pw-grid\" data-grid=\"predict\">")
            .expect("coverage matrix present");
        assert!(
            key_at < grid_at,
            "the coverage key must precede the first matrix, not sit below all three"
        );

        // Finding 4: each direction's dof is on its (exempt) corner title, and the
        // expected triples carry a visible joint / axis label (not hover-only).
        assert!(
            cov.contains("joint · 2 dof"),
            "joint matrix must state 2 dof"
        );
        assert!(
            cov.contains("along-track · 1 dof"),
            "along-track matrix must state 1 dof"
        );
        assert!(
            cov.contains("cross-track · 1 dof"),
            "cross-track matrix must state 1 dof"
        );
        assert!(
            cov.contains(">joint 39/86/98.9<"),
            "the joint expected triple must carry a visible label"
        );
        assert!(
            cov.contains(">axis 68/95/99.7<"),
            "the 1-dof expected triple must carry a visible label"
        );

        // Finding 5 / v2-#6 (S4): the cell scope — trust-off observed 2σ percent on
        // Σ_pred-dominated rows only, the tool ╲ dt layout and the day horizon — is
        // the one load-bearing caveat. S4 moved it off the per-matrix corner header
        // onto the always-visible scope key line under the Coverage summary.
        assert!(
            cov.contains("trust-off · σ_pred-only 2σ% · tool ╲ dt · days"),
            "the scope key must state trust-off, the σ_pred-only scope, the 2σ unit and the day horizon"
        );
        assert!(
            cov.contains("σ_pred-only"),
            "the Σ_pred-dominated scope must ride the always-visible corner header"
        );

        // Finding 6: the covariance arm rides visibly beside the tool group name.
        // Vocabulary (S3 item 7): the group header reads the product name
        // `empyrean`, not the `rust` channel slug, and the covariance arm renders
        // as factor words (`EFCC · adaptive · per-obs · linear`) with the raw id
        // on hover only.
        assert!(
            cov.contains(
                ">empyrean <span class=\"pwarm\" title=\"arm efcc.adap.nonight\">EFCC · adaptive · per-obs · linear</span>"
            ),
            "the coverage group header must name empyrean and render the arm as factor words with the id on hover"
        );
    }

    #[test]
    fn coverage_matrix_splits_arc_and_carries_the_v2_figures() {
        // 4.2A v2: two arc rows per class, the cell the trust-unconditioned rate,
        // the trust-gated and per-row figures on hover, and a Σ_pred-share hatch
        // with its reason. Each assertion fails on the pre-v2 single-row render.
        let rows = vec![synthetic_rust_prop_row("Apophis", 0.0)];
        let agg = synthetic_predict_aggregates("snap");
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&rows, &[], &out, None, Some(&agg), &[]).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        let cs = html.find(">Coverage</div>").expect("4.2A Coverage panel");
        let ce = html[cs..].find("Surfaces</div>").map(|i| cs + i).unwrap();
        let cov = &html[cs..ce];

        // Both arc rows are present for the class.
        assert!(
            cov.contains("short-arc"),
            "4.2A must carry the short-arc rows"
        );
        assert!(
            cov.contains("long-arc"),
            "4.2A must carry the long-arc rows"
        );
        // The cell is the trust-unconditioned rate (78%); the hover carries the
        // trust-gated and per-row figures the reader compares it against.
        assert!(
            cov.contains("<span class=\"cn\">78%</span>"),
            "the cell must show the trust-unconditioned 2σ rate"
        );
        assert!(
            cov.contains("trust-gated 90%"),
            "the hover must carry the trust-gated figure"
        );
        assert!(
            cov.contains("per-row 81%"),
            "the hover must carry the per-row figure"
        );
        // A cell below the Σ_pred-dominated share floor is hatched with its reason.
        assert!(
            cov.contains("share below 10%"),
            "a cell below the Σ_pred-dominated share floor must hatch with its reason"
        );
        assert!(
            cov.contains("σ_pred-dominated 5% of rows"),
            "the share hatch must state the measured share"
        );
        // v2-#6: the numeric cell's hover states the Σ_pred share as a FILTER on
        // the row population (reads as "computed on these rows only"), not as a
        // bare descriptor. The short-arc dt 0.3–1 d cell is 60 % Σ_pred-dominated.
        assert!(
            cov.contains("computed on the 60% of rows where σ_pred dominates"),
            "the numeric cell hover must read the Σ_pred share as a row filter"
        );
    }

    #[test]
    fn coverage_folds_always_empty_class_rows_into_one_counted_line() {
        // v2-#8: a (class, arc-side) no tool reached in any direction is not
        // drawn as a full hatched row under every tool; it is folded into one
        // consolidated "no predictive windows" line per matrix, the classes
        // named with their 0-object count on hover (marked and counted). Here
        // MainBelt has a surface cell (so it is a class) but no coverage row, so
        // both its arc sides fold; NEA keeps its per-tool short/long rows.
        let rows = vec![synthetic_rust_prop_row("Apophis", 0.0)];
        let mut agg = synthetic_predict_aggregates("snap");
        agg.cells.push(pw_surface_cell(
            ("MainBelt", "rust", "efcc.adap.nonight"),
            (1.0, 3.0),
            (0.3, 1.0),
            5,
            (Some(1.0), Some(1.0)),
        ));
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&rows, &[], &out, None, Some(&agg), &[]).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        let cs = html.find(">Coverage</div>").expect("4.2A Coverage panel");
        let ce = html[cs..].find("Surfaces</div>").map(|i| cs + i).unwrap();
        let cov = &html[cs..ce];

        // The consolidated line is present and names the empty class on its hover.
        assert!(
            cov.contains("no predictive windows"),
            "always-empty class rows must fold into one consolidated line"
        );
        assert!(
            cov.contains("MainBelt · short-arc") && cov.contains("MainBelt · long-arc"),
            "the consolidated line must name the empty class × arc combinations"
        );
        // MainBelt is NOT drawn as a per-tool data row (that is what folded).
        assert!(
            !cov.contains("<td class=\"gtool\">MainBelt"),
            "the empty class must not keep a full hatched per-tool row"
        );
        // NEA keeps its per-tool short/long rows (it has coverage data).
        assert!(
            cov.contains("<td class=\"gtool\">NEA · short-arc"),
            "a class with data keeps its per-tool arc rows"
        );
    }

    #[test]
    fn production_arm_is_featured_over_a_more_populated_control_arm() {
        // Without the shipped arm in the data, the most-populated pair leads
        // (the fixture's rust arms tie at one object; the alphabetical
        // tie-break picks the nonight control).
        let mut agg = synthetic_predict_aggregates("snap");
        assert_eq!(pw_primary_series(&agg), ("rust", "efcc.adap.nonight"));

        // A second nonight object makes the control the most-populated arm.
        let mut didymos = agg.per_object[0].clone();
        didymos.object = "Didymos".into();
        agg.per_object.push(didymos);

        // The shipped arm present but unscored (no converged window) does not
        // feature: a headline needs data behind it.
        let mut unscored = agg.per_object[1].clone();
        unscored.config_arm = PRODUCTION_ARM.1.into();
        agg.per_object.push(unscored);
        assert_eq!(pw_primary_series(&agg), ("rust", "efcc.adap.nonight"));

        // One scored object on the shipped arm features it over the control's
        // two, for the headline and the per-tool coverage arm alike.
        let mut scored = agg.per_object[0].clone();
        scored.config_arm = PRODUCTION_ARM.1.into();
        agg.per_object.push(scored);
        assert_eq!(pw_primary_series(&agg), PRODUCTION_ARM);
        assert_eq!(pw_coverage_arm_for(&agg, "rust"), Some(PRODUCTION_ARM.1));
        assert_eq!(pw_coverage_arm_for(&agg, "findorb"), Some("default"));
    }

    #[test]
    fn realism_surface_hover_carries_the_sigma_pred_share() {
        // Item 3: the 4.2B realism cell hover gains the Σ_pred-dominated share of
        // rows (n_rows_pd / n_rows); the fixture's well-sampled cells carry 75%.
        let rows = vec![synthetic_rust_prop_row("Apophis", 0.0)];
        let agg = synthetic_predict_aggregates("snap");
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&rows, &[], &out, None, Some(&agg), &[]).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        assert!(
            html.contains("σ_pred-dominated 75% of rows"),
            "a 4.2B realism cell hover must carry the Σ_pred-dominated share"
        );
    }

    #[test]
    fn diverging_ramp_text_clears_wcag_aa_in_both_themes() {
        // Every 4.2A diverging cell carries a number (shownums) inked
        // --ed-text-primary over a color-mix of the theme accent/error on the theme
        // bg. The deepest blue (rvb3) failed 4.5:1 in the dark theme at 78% (3.58:1)
        // and is now 60%. This recomputes the WCAG contrast of every step in BOTH
        // themes from the emitted percentages and the page's own tokens, so a
        // percentage or token regression below AA fails the gate (it fails at 78%).
        let html = render_rows(&pair_test_rows());
        let css = realism_css();

        fn hex(s: &str) -> (f64, f64, f64) {
            let s = s.trim_start_matches('#');
            (
                u8::from_str_radix(&s[0..2], 16).unwrap() as f64,
                u8::from_str_radix(&s[2..4], 16).unwrap() as f64,
                u8::from_str_radix(&s[4..6], 16).unwrap() as f64,
            )
        }
        fn tok(html: &str, from: usize, needle: &str) -> (f64, f64, f64) {
            let i = html[from..]
                .find(needle)
                .map(|k| from + k)
                .unwrap_or_else(|| panic!("token {needle} not found"))
                + needle.len();
            hex(&html[i..i + 7])
        }
        fn pct(css: &str, cls: &str) -> f64 {
            let anchor = format!(".{cls} {{");
            let seg = &css[css.find(&anchor).expect("ramp class in realism_css")..];
            let p = seg.find('%').expect("percent in ramp rule");
            let b = seg.as_bytes();
            let mut j = p;
            while j > 0 && b[j - 1].is_ascii_digit() {
                j -= 1;
            }
            seg[j..p].parse::<f64>().unwrap()
        }
        fn lin(c: f64) -> f64 {
            let c = c / 255.0;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }
        fn lum(c: (f64, f64, f64)) -> f64 {
            0.2126 * lin(c.0) + 0.7152 * lin(c.1) + 0.0722 * lin(c.2)
        }
        fn contrast(a: (f64, f64, f64), b: (f64, f64, f64)) -> f64 {
            let (l1, l2) = (lum(a), lum(b));
            let (hi, lo) = if l1 > l2 { (l1, l2) } else { (l2, l1) };
            (hi + 0.05) / (lo + 0.05)
        }
        fn mix(x: (f64, f64, f64), bg: (f64, f64, f64), p: f64) -> (f64, f64, f64) {
            let q = p / 100.0;
            (
                x.0 * q + bg.0 * (1.0 - q),
                x.1 * q + bg.1 * (1.0 - q),
                x.2 * q + bg.2 * (1.0 - q),
            )
        }

        // Dark tokens are the first occurrence; light tokens come after the light
        // --ed-bg anchor, with --ed-error inherited from dark (light does not set it).
        let d_accent = tok(&html, 0, "--ed-accent: ");
        let d_error = tok(&html, 0, "--ed-error: ");
        let d_bg = tok(&html, 0, "--ed-bg: ");
        let d_text = tok(&html, 0, "--ed-text-primary: ");
        let l0 = html.find("--ed-bg: #f6f8fb").expect("light theme block");
        let l_accent = tok(&html, l0, "--ed-accent: ");
        let l_bg = tok(&html, l0, "--ed-bg: ");
        let l_text = tok(&html, l0, "--ed-text-primary: ");
        let l_error = d_error;

        for (cls, base_d, base_l) in [
            ("rvb1", d_accent, l_accent),
            ("rvb2", d_accent, l_accent),
            ("rvb3", d_accent, l_accent),
            ("rvr1", d_error, l_error),
            ("rvr2", d_error, l_error),
            ("rvr3", d_error, l_error),
        ] {
            let p = pct(&css, cls);
            let cd = contrast(d_text, mix(base_d, d_bg, p));
            let cl = contrast(l_text, mix(base_l, l_bg, p));
            assert!(cd >= 4.5, "dark {cls} at {p}% = {cd:.2}:1 (< 4.5 AA)");
            assert!(cl >= 4.5, "light {cls} at {p}% = {cl:.2}:1 (< 4.5 AA)");
        }
    }

    #[test]
    fn export_identity_names_the_timeline_object_not_none() {
        let html = render_rows(&pair_test_rows());
        // The 4.2F walk timeline is per-object; its PNG stamp and filename must name
        // the selected object (the variant slot), not the pair-less 'none' every
        // object collapsed to before.
        assert!(
            html.contains("gd.id === 'pw-timeline-chart'"),
            "the export identity must special-case the walk-timeline chart"
        );
        assert!(
            html.contains("getElementById('pw-timeline-object')")
                && html.contains("pair = os.value"),
            "the timeline export must read the selected object into the variant slot"
        );
    }

    #[test]
    fn surfaces_arm_selector_is_scoped_to_the_tool() {
        let html = render_rows(&pair_test_rows());
        // The arm <select> is repopulated on tool change to the arms that exist for
        // that tool, so a reader never picks a rust-only arm under find_orb / layup
        // and lands on an all-hatched grid that reads as a broken tool.
        assert!(
            html.contains("armsByTool[p.dataset.tool]"),
            "the surfaces wiring must index arms by tool from the pair panels"
        );
        assert!(
            html.contains("function repopulateArms()"),
            "the surfaces wiring must repopulate the arm options per tool"
        );
        assert!(
            html.contains("repopulateArms(); apply();"),
            "a tool change must repopulate the arm options before re-applying"
        );
    }

    #[test]
    fn report_without_a_walk_renders_the_empty_state() {
        let rows = vec![synthetic_rust_prop_row("Apophis", 0.0)];
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&rows, &[], &out, None, None, &[]).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        assert!(html.contains("id=\"s15\""), "the 4.2 shell always renders");
        assert!(
            html.contains("No walk-forward data"),
            "empty state must show its message"
        );
        assert!(
            !html.contains("class=\"agrid pw-grid\""),
            "the empty state must render no grids"
        );
        assert!(
            html.contains("const PREDICT_DESCRIPTOR = null;"),
            "no walk means a null descriptor"
        );
        assert!(
            !predict_path_for(&out).exists(),
            "no walk means no predict sidecar on disk"
        );
    }

    #[test]
    fn predictive_power_hatches_a_thin_cell_never_pools_it() {
        let rows = vec![synthetic_rust_prop_row("Apophis", 0.0)];
        let agg = synthetic_predict_aggregates("snap");
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&rows, &[], &out, None, Some(&agg), &[]).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        // The n=2 thin cell renders hatched with its count + reason, never as a
        // coloured value (its 0.3 realism / 0.5 sep must not appear as a cell).
        assert!(
            html.contains("n=2 · fewer than three objects"),
            "a thin cell must be hatched with n and reason"
        );
    }

    #[test]
    fn predictive_calibration_marks_log_score_absent_when_empty() {
        let rows = vec![synthetic_rust_prop_row("Apophis", 0.0)];
        let agg = synthetic_predict_aggregates("snap"); // calibration is empty
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&rows, &[], &out, None, Some(&agg), &[]).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        assert!(
            html.contains("awaiting Student-t chain"),
            "the empty log-score family must be marked absent"
        );
    }

    #[test]
    fn predict_sidecar_is_written_and_matches_its_descriptor() {
        let rows = vec![synthetic_rust_prop_row("Apophis", 0.0)];
        let agg = synthetic_predict_aggregates("snap");
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&rows, &[], &out, None, Some(&agg), &[]).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        // The large per-object family must not ride inline on the page.
        assert!(
            !html.contains("\"per_window\""),
            "per_window must travel as a sidecar, not inline"
        );
        let sidecar = predict_path_for(&out);
        let blob = std::fs::read(&sidecar).expect("sidecar must be written");
        let hash = fnv1a32_hex(&blob);
        assert!(
            html.contains(&format!("\"bytes\":{}", blob.len())),
            "descriptor byte length must match the sidecar"
        );
        assert!(
            html.contains(&format!("\"hash\":\"{hash}\"")),
            "descriptor FNV must match the sidecar"
        );
        let parsed: serde_json::Value = serde_json::from_slice(&blob).unwrap();
        assert_eq!(
            parsed["per_window"].as_array().unwrap().len(),
            agg.per_window.len(),
            "sidecar must carry the per_window family"
        );
    }

    #[test]
    fn predictive_report_is_byte_reproducible_including_the_sidecar() {
        let rows = vec![synthetic_rust_prop_row("Apophis", 0.0)];
        let agg = synthetic_predict_aggregates("snap");
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.html");
        let b = dir.path().join("b.html");
        generate_report(&rows, &[], &a, None, Some(&agg), &[]).unwrap();
        generate_report(&rows, &[], &b, None, Some(&agg), &[]).unwrap();
        let sa = std::fs::read(predict_path_for(&a)).unwrap();
        let sb = std::fs::read(predict_path_for(&b)).unwrap();
        assert_eq!(sa, sb, "the predict sidecar must be byte-reproducible");
    }

    #[test]
    fn merge_predict_aggregates_concatenates_and_guards_snapshot_and_profile() {
        let a = synthetic_predict_aggregates("snap");
        let mut b = synthetic_predict_aggregates("snap");
        b.profile = "full".into();
        let merged = merge_predict_aggregates(vec![a.clone(), b])
            .unwrap()
            .unwrap();
        assert_eq!(
            merged.cells.len(),
            a.cells.len() * 2,
            "cells must concatenate"
        );
        assert_eq!(
            merged.profile, "unstated",
            "disagreeing profiles collapse to the marked state"
        );
        let mut c = synthetic_predict_aggregates("other-snap");
        c.snapshot_id = "other-snap".into();
        assert!(
            merge_predict_aggregates(vec![a, c]).is_err(),
            "a snapshot mismatch must be refused"
        );
        assert!(merge_predict_aggregates(vec![]).unwrap().is_none());
    }

    // The population order is one canonical switch (POPULATION_ORDER) applied to
    // every grid. Guards the constant (it names exactly the 13 catalog
    // populations, nested in Part-4 class order, no duplicate) and proves the
    // switch is wired into the agreement-grid spine: objects fed worst-value
    // first and out of population order still render their population blocks in
    // POPULATION_ORDER. The render half can fail — revert prop_like_ordered_blocks
    // to a worst-descending block sort and NEO/Comet flip.
    #[test]
    fn population_order_is_the_single_canonical_switch() {
        use std::collections::{BTreeSet, HashMap};
        // (1) The constant names exactly the catalog's populations, with no
        // duplicate, and each maps to one of the six Part-4 classes.
        let in_order: BTreeSet<&str> = POPULATION_ORDER.iter().copied().collect();
        assert_eq!(
            in_order.len(),
            POPULATION_ORDER.len(),
            "POPULATION_ORDER must not repeat a population"
        );
        let catalog_pops: BTreeSet<&str> = crate::catalog::all_objects()
            .iter()
            .map(|o| o.population)
            .collect();
        assert_eq!(
            in_order, catalog_pops,
            "POPULATION_ORDER must name exactly the catalog's populations"
        );
        for &p in &POPULATION_ORDER {
            // Panics if a population is unmapped — the constant stays in sync
            // with the six-class taxonomy.
            let _ = crate::predict_schema::classes::from_population(p);
        }
        // population_rank is the identity on the slice; an unknown sorts last.
        for (i, &p) in POPULATION_ORDER.iter().enumerate() {
            assert_eq!(
                population_rank(p),
                i,
                "population_rank must match the slice for {p}"
            );
        }
        assert_eq!(
            population_rank("not a population"),
            POPULATION_ORDER.len(),
            "an unmapped population must sort last"
        );
        // class_rank is non-decreasing along the slice (classes stay contiguous).
        let mut last = 0usize;
        for &p in &POPULATION_ORDER {
            let r = class_rank(crate::predict_schema::classes::from_population(p));
            assert!(
                r >= last,
                "class_rank must be non-decreasing along POPULATION_ORDER (contiguous classes)"
            );
            last = r;
        }

        // (2) The switch is wired into the agreement-grid spine. Under the old
        // worst-descending policy Comet (worst 10 km) would sort ahead of NEO
        // (worst 1 km); the canonical order must still place NEO first.
        let objects = [("Comet obj", "Comet"), ("NEO obj", "NEO")];
        let dts = [0i64, 30];
        let mut vals: HashMap<(String, i64), f64> = HashMap::new();
        vals.insert(("Comet obj".to_string(), 0), 10.0);
        vals.insert(("Comet obj".to_string(), 30), 8.0);
        vals.insert(("NEO obj".to_string(), 0), 1.0);
        vals.insert(("NEO obj".to_string(), 30), 0.5);
        let blocks = prop_like_ordered_blocks(&objects, &dts, &vals);
        let order: Vec<&str> = blocks.iter().map(|(p, _)| *p).collect();
        assert_eq!(
            order,
            vec!["NEO", "Comet"],
            "the canonical order must place NEO before Comet despite Comet's larger worst value"
        );
    }

    // ───────────────────── S3 de-duplication tests ─────────────────────
    // One per S3 item; each fails on the pre-S3 structure.

    #[test]
    fn s3_item1_timing_grids_fold_behind_one_per_object_disclosure() {
        let html = render_rows(&timing_fixture());
        // S7: the propagation cost (per-row + by-population) and its per-object G1
        // grid live in §2.1's C slot. Scope to §2.1 (s-prop .. s-eph).
        let spos = html.find("id=\"s-prop\"").expect("2.1 Propagation section");
        let send = html[spos..].find("id=\"s-eph\"").map(|i| spos + i).unwrap();
        let after = &html[spos..send];
        let perrow = after
            .find(">Per row</div>")
            .expect("the per-row table stays open");
        // The C-slot per-object fold is the `per object` disclosure that follows
        // the per-row table (the A slot is also a `per object` fold, before it).
        let fold = perrow
            + after[perrow..]
                .find("<details class=\"more-tables\"><summary class=\"more-sum\">per object</summary>")
                .expect("2.1 C per-object fold present");
        let g1 = after.find("id=\"g1-prop-time\"").expect("G1 grid present");
        assert!(
            perrow < fold,
            "the per-row (and by-population) cost tables stay open, before the fold"
        );
        assert!(
            fold < g1,
            "the per-object G1 grid folds behind one `per object` disclosure"
        );
    }

    // ───────────────────── S7 Part-2 reshape tests ─────────────────────
    // One per item; each fails on the pre-S7 (s-ranking / s-timing) structure.

    #[test]
    fn s7_part2_is_three_sections_with_slot_anchors() {
        let html = render_rows(&timing_fixture());
        // The old two-section split (s-ranking / s-timing) is gone.
        assert!(
            !html.contains("id=\"s-ranking\"") && !html.contains("id=\"s-timing\""),
            "the old 2.1 Side-by-side / 2.2 Wall-clock split is gone"
        );
        // Three axis sections, each with the A · B · C slot anchors the renderer
        // emits (the scroll-spy targets).
        for (sec, title) in [
            ("s-prop", "Propagation"),
            ("s-eph", "Ephemeris"),
            ("s-od", "Orbit determination"),
        ] {
            assert!(
                html.contains(&format!("<div class=\"section\" id=\"{sec}\">")),
                "missing Part-2 section {sec}"
            );
            for slot in ["a", "b", "c"] {
                assert!(
                    html.contains(&format!(
                        "class=\"slot-eyebrow spy-anchor\" id=\"{sec}-{slot}\""
                    )),
                    "section {sec} missing slot anchor {sec}-{slot}"
                );
            }
            // The section-title div for this section carries the axis name.
            let tid = format!("id=\"{sec}-title\"");
            let t = html
                .find(&tid)
                .unwrap_or_else(|| panic!("section {sec} has no title div"));
            let close = html[t..].find("</div>").map(|i| t + i).unwrap();
            assert!(
                html[t..close].contains(title),
                "section {sec} title div missing axis name {title}"
            );
        }
    }

    #[test]
    fn s7_b_by_population_slot_exists_for_all_three_axes() {
        let html = render_rows(&timing_fixture());
        // Every axis section has a B · by population slot (today only propagation
        // did); fails on the old structure where ephemeris/OD had no marginal.
        for sec in ["s-prop", "s-eph", "s-od"] {
            assert!(
                html.contains(&format!("id=\"{sec}-b\"")),
                "section {sec} missing its B · by population slot"
            );
        }
        // The ephemeris B is a population × tool matrix (empyrean + find_orb).
        let se = html.find("id=\"s-eph-b\"").unwrap();
        let se_end = html[se..].find("id=\"s-eph-c\"").map(|i| se + i).unwrap();
        let eph_b = &html[se..se_end];
        assert!(
            eph_b.contains("<th class=\"gobj\">population</th>")
                && eph_b.contains("<th>empyrean</th>")
                && eph_b.contains("<th>find_orb</th>"),
            "the ephemeris B states each tool's separation by population"
        );
        // The OD B is a population × tool matrix (empyrean σ_eq + find_orb conv).
        let so = html.find("id=\"s-od-b\"").unwrap();
        let so_end = html[so..].find("id=\"s-od-c\"").map(|i| so + i).unwrap();
        let od_b = &html[so..so_end];
        assert!(
            od_b.contains("empyrean <span class=\"u\">σ_eq</span>")
                && od_b.contains(">find_orb conv.</th>"),
            "the OD B states empyrean σ_eq and find_orb convergence by population"
        );
    }

    #[test]
    fn s7_parts_3_and_4_heads_have_titles_and_no_lone_letters() {
        let html = render_rows(&timing_fixture());
        // 4.1 and 4.2 now render an on-page title (today they render none).
        assert!(
            html.contains(">Consistency with JPL at epoch</div>"),
            "4.1 renders its on-page title"
        );
        assert!(
            html.contains(">Predictive power</div>"),
            "4.2 renders its on-page title"
        );
        // The P.n section numbers carry no lone slot letter any more.
        for n in ["3.1", "3.2", "3.3", "4.1"] {
            assert!(
                html.contains(&format!("<div class=\"section-num\">{n}</div>")),
                "section number {n} present without a lone letter"
            );
        }
        for lone in [">3.1A<", ">3.2A<", ">3.3A<", ">4.1A<"] {
            assert!(
                !html.contains(lone),
                "lone-letter section number {lone} survived"
            );
        }
    }

    #[test]
    fn s7_rail_is_generated_for_every_part() {
        let rail = build_rail_html();
        // A rail-part header for each of the five parts.
        for p in [
            PART_NAME_1,
            PART_NAME_2,
            PART_NAME_3,
            PART_NAME_4,
            PART_NAME_APPENDIX,
        ] {
            assert!(
                rail.contains(&format!("<div class=\"rail-part\">{p}</div>")),
                "rail missing part header {p}"
            );
        }
        // Part 2 is three fixed-slot entries, each with its A · B · C sub-entries.
        for sec in ["s-prop", "s-eph", "s-od"] {
            assert!(
                rail.contains(&format!("data-target=\"{sec}\"")),
                "rail missing Part-2 section {sec}"
            );
            for slot in ["a", "b", "c"] {
                assert!(
                    rail.contains(&format!("data-target=\"{sec}-{slot}\"")),
                    "rail missing {sec}-{slot} sub-entry"
                );
            }
        }
        // Parts 3–4 and the appendix are flat entries, in the tree order.
        for sec in [
            "s10", "s05", "s09b", "s12", "s15", "s01", "s01b", "s05c", "s13",
        ] {
            assert!(
                rail.contains(&format!("data-target=\"{sec}\"")),
                "rail missing entry {sec}"
            );
        }
        // No pre-S7 ranking/timing rail entries.
        assert!(
            !rail.contains("s-ranking") && !rail.contains("s-timing"),
            "the pre-S7 rail entries are gone"
        );
    }

    #[test]
    fn s7_dom_order_equals_visual_order() {
        let html = render_rows(&timing_fixture());
        // No flex `order` reorders the content column: byte position IS the order.
        assert!(!html.contains("{ order:"), "no flex order rule may survive");
        let at = |id: &str| -> usize {
            html.find(&format!("id=\"{id}\""))
                .unwrap_or_else(|| panic!("missing #{id}"))
        };
        let seq = [
            "part1-bar",
            "s-prop",
            "s-eph",
            "s-od",
            "part3",
            "s10",
            "s05",
            "s09b",
            "part4",
            "s12",
            "s15",
            "appendix",
            "s01",
            "s01b",
            "s05c",
            "s13",
        ];
        let mut prev = 0usize;
        for id in seq {
            let p = at(id);
            assert!(p > prev, "#{id} is out of DOM/visual order");
            prev = p;
        }
    }

    #[test]
    fn s3_item2_arms_and_calibration_merge_into_one_factor_level_table() {
        let rows = vec![synthetic_rust_prop_row("Apophis", 0.0)];
        let agg = synthetic_predict_aggregates("snap");
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&rows, &[], &out, None, Some(&agg), &[]).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        // One merged section: no separate Calibration panel-title.
        assert!(
            html.contains(">Configuration arms</div>") && !html.contains(">Calibration</div>"),
            "Configuration arms and Calibration merge into one section"
        );
        // One factor × level header carries realism AND 1σ/2σ coverage together.
        let m = html
            .find(">marginal realism</span>")
            .expect("marginal realism column");
        let hdr = &html[m..m + html[m..].find("</tr>").expect("header row end")];
        assert!(
            hdr.contains(">1σ obs</span>") && hdr.contains(">2σ obs</span>"),
            "the merged factor-level header pairs marginal realism with 1σ/2σ coverage"
        );
        // One of each fold, and the log-score-absent mark stays.
        assert_eq!(
            html.matches("<summary>full factor grid</summary>").count(),
            1,
            "one full factor grid fold"
        );
        assert_eq!(
            html.matches("<summary>full arm list</summary>").count(),
            1,
            "one full arm list fold"
        );
        assert!(
            html.contains(">log-score absent</div>"),
            "the log-score-absent mark stays"
        );
    }

    #[test]
    fn s3_item3_nongrav_one_row_per_coeff_with_agreement_and_c_hatched() {
        let html = render_synthetic_html();
        assert!(
            html.contains("rowspan=\"${coeffs.length}\"") && !html.contains("const totalSubRows"),
            "one row per (object, coefficient), not per channel × coefficient"
        );
        let ng = {
            let s = html.find("id=\"s09b\"").expect("3.3 non-grav section");
            let e = html[s..]
                .find("id=\"part4\"")
                .map(|i| s + i)
                .unwrap_or(html.len());
            &html[s..e]
        };
        assert!(
            ng.contains(">channels</th>")
                && ng.contains(">c</th>")
                && !ng.contains(">Channel</th>"),
            "rust/cli/python collapse to one agreement column; c is its own column"
        );
        assert!(
            html.contains("c (C ABI) emitted no non-grav row this run"),
            "the absent c channel is one hatched column with its reason"
        );
    }

    #[test]
    fn s3_item4_outliers_fold_into_1_2a_and_section_and_rail_are_gone() {
        let html = render_rows(&pair_test_rows());
        assert!(
            !html.contains("id=\"s07\"") && !html.contains("data-target=\"s07\""),
            "the standalone 1.2D Outliers section and its rail entry are gone"
        );
        // S6: the merged 1.2 section runs from s06 to the merged 1.3 section s08b.
        let s06 = html.find("id=\"s06\"").expect("1.2 section");
        let s06end = html[s06..].find("id=\"s08b\"").map(|i| s06 + i).unwrap();
        let sec = &html[s06..s06end];
        assert!(
            sec.contains("<summary class=\"more-sum\">outliers</summary>")
                && sec.contains("<tbody id=\"eph-outliers\">"),
            "the outliers table folds into §1.2 as the `outliers` disclosure"
        );
        assert!(
            sec.contains("class=\"rwn\"") && sec.contains("runaway</span>"),
            "the §1.2A summary line gains the runaway count element (JS fills the number)"
        );
    }

    #[test]
    fn s3_item5_assist_capped_moves_into_2_2_n_and_a3_timing_table_gone() {
        // Two timed ASSIST default rows plus one capped (null wall clock).
        let mut rows = Vec::new();
        for dt in [0.0_f64, 30.0, 60.0] {
            let mut r = synthetic_rust_prop_row("Apophis", dt);
            r.channel = "assist".into();
            r.propagation_uncertainty = Some("assist_default_variational_off".into());
            r.assist_time_ms = if dt == 60.0 { None } else { Some(6.0) };
            r.emp_time_ms = None;
            rows.push(r);
        }
        let html = render_rows(&rows);
        assert!(
            !html.contains(">Per-horizon median timing</div>")
                && !html.contains("id=\"assist-arms-timing\""),
            "the §A.3 per-horizon timing panel-title and its mount are removed"
        );
        assert!(
            html.contains("(capped 1)"),
            "the capped (loud-FAIL) count moves into §2.2's n column, so nothing is lost"
        );
    }

    #[test]
    fn s3_item6_sigma_eq_owned_by_1_3a_trimmed_from_2_1_and_4_1a() {
        let (rows, cmp) = radar_scenario();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&rows, &cmp, &out, None, None, &[]).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();

        // 2.1 "By object and fitter": the empyrean σ_eq column is dropped (1.3A
        // owns it); the external fitter stays and the panel links to §1.3A.
        let s2 = html
            .find("By object and fitter")
            .expect("2.1 by-object-and-fitter table");
        let t2 = &html[s2..s2 + html[s2..].find("</table>").expect("2.1 table end")];
        assert!(
            t2.contains("<th>find_orb</th>") && !t2.contains("<th>empyrean</th>"),
            "2.1 keeps the external fitter column and drops the duplicate empyrean σ_eq column"
        );
        assert!(
            html[s2..].contains("href=\"#s08b\""),
            "2.1 links to §1.3A for empyrean's per-object σ_eq"
        );

        // 4.1A "Complete per-object table": the duplicate σ_eq column is dropped;
        // pathology, the paired χ²r, the σ ratio and the JPL arc stay.
        let s4 = html
            .find("Complete per-object table")
            .expect("4.1A per-object table");
        let t4 = &html[s4..s4 + html[s4..].find("</table>").expect("4.1A table end")];
        assert!(
            !t4.contains("<span class=\"u\">σ_eq</span>"),
            "4.1A drops the duplicate per-object σ_eq column"
        );
        assert!(
            t4.contains(">pathology</th>")
                && t4.contains("χ²ᵣ emp / JPL")
                && t4.contains("σ ratio (a)")
                && t4.contains("arc (days)"),
            "4.1A keeps pathology, the paired χ²r, the σ ratio and the JPL arc"
        );
    }

    #[test]
    fn s3_item7_coverage_group_header_names_empyrean_and_factor_words() {
        let rows = vec![synthetic_rust_prop_row("Apophis", 0.0)];
        let agg = synthetic_predict_aggregates("snap");
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&rows, &[], &out, None, Some(&agg), &[]).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        assert!(
            html.contains(">empyrean <span class=\"pwarm\" title=\"arm efcc.adap.nonight\">EFCC · adaptive · per-obs · linear</span>"),
            "the 4.2 coverage group header reads empyrean + the arm as factor words"
        );
        assert!(
            !html.contains("<span class=\"pwarm\">efcc.adap.nonight</span>"),
            "the raw arm id no longer renders as the chip text (it rides the hover)"
        );
    }

    // ── S5 (P4 remainder) ───────────────────────────────────────────────────

    #[test]
    fn s5_item1_part_heads_are_eyebrow_title_and_part1_carries_the_controls() {
        let html = render_rows(&pair_test_rows());
        // Part heads are a mono eyebrow over a Syne title (both classes present,
        // split from "Part N · Title"). Fails on the old single-string head.
        assert!(
            html.contains("<span class=\"ph-eyebrow\">Part 1</span><span class=\"ph-title\">Reference agreement</span>"),
            "the Part 1 head splits into a mono eyebrow + Syne title"
        );
        assert!(
            html.contains("class=\"ph-eyebrow\">Part 2<")
                && html.contains("class=\"ph-eyebrow\">Part 4<"),
            "every Part head carries the eyebrow + title shape"
        );
        // The part-head is flush on the content edge — no centring box.
        let ph = html.find(".part-head {").expect("part-head rule");
        let rule = &html[ph..ph + html[ph..].find('}').unwrap()];
        assert!(
            rule.contains("max-width: none") && !rule.contains("1400px"),
            "the Part head is left-aligned on the content edge, not a centred 1400px box: {rule}"
        );
        // The Compare controls ride the Part 1 head line: the sticky bar carries
        // the head stack AND the two selects, and the separate #part1 head is gone.
        assert!(
            html.contains("<div class=\"part1-bar\" id=\"part1-bar\">")
                && html.contains("<div class=\"ph-head\"><span class=\"ph-eyebrow\">Part 1</span>"),
            "the sticky Compare bar is the Part 1 head line"
        );
        assert!(
            !html.contains("\"part-head\" id=\"part1\">"),
            "the separate #part1 part-head is merged into the sticky bar"
        );
    }

    #[test]
    fn s5_item2_one_null_token_in_the_error_formatter() {
        let html = render_rows(&pair_test_rows());
        // The km error formatter returns the one null token (—), not the legacy
        // triple-hyphen variant. Fails on the old '---' return.
        assert!(
            html.contains("if (km == null) return '—';"),
            "the error formatter returns the one null token"
        );
        assert!(
            !html.contains("return '---'"),
            "the legacy triple-hyphen null variant is gone"
        );
    }

    #[test]
    fn s5_item3_keys_collapse_to_chip_and_grid_key_no_plotly_legend() {
        let html = render_rows(&pair_test_rows());
        // The inline value chip carries its decade on a data attribute, so every
        // chip shares one class signature — the `.lchip.gbN` form is gone.
        assert!(
            html.contains(".lchip[data-gb=\"1\"]") && html.contains(".lchip[data-gb=\"7\"]"),
            "the ladder chip ink is keyed on data-gb, not a gbN class"
        );
        assert!(
            !html.contains(".lchip.gb") && !html.contains("class=\"lchip gb"),
            "no chip carries a gbN class any more"
        );
        // The rvruler mini-key is folded away (renamed off the key selector).
        assert!(
            html.contains(".rvnote {")
                && !html.contains(".rvruler {")
                && !html.contains("class=\"rvruler\""),
            "the rvruler key component is gone"
        );
        // The remaining Plotly legends (3.2, 4.1) are gone in favour of direct
        // labels — the shared COV_LEGEND placement no longer exists.
        assert!(
            !html.contains("COV_LEGEND"),
            "the covariance-panel Plotly legend is gone (direct labels replace it)"
        );
        assert!(
            html.contains("const chiAnnos =") && html.contains("const d3Annos ="),
            "the chi² and derivatives panels build direct-label annotations"
        );
        // The growth end labels are vertically dodged where lines converge.
        assert!(
            html.contains("const dodged = popLabels.map(") && html.contains("MINSEP"),
            "the growth direct labels are dodged"
        );
    }

    #[test]
    fn s5_item4_a1_mono_line_no_chart_cards_modebar_download_only() {
        let html = render_rows(&pair_test_rows());
        // A.1 is one mono line of counts — no stat cards, no dot legends.
        assert!(
            html.contains("class=\"grid-summary run-counts\"")
                && html.contains(" objects · ")
                && html.contains(" channels</div>"),
            "A.1 renders a mono run-counts line (recognised as a summary line)"
        );
        assert!(
            !html.contains("class=\"summary-card\"") && !html.contains("<div class=\"legend\">"),
            "A.1 drops the seven stat cards and the two dot legends"
        );
        // The modebar carries only the one branded download button.
        assert!(
            html.contains("modeBarButtons: [[{ name: 'downloadPng'"),
            "the modebar is a single-group list with only the download button"
        );
        assert!(
            !html.contains("modeBarButtonsToRemove") && !html.contains("modeBarButtonsToAdd"),
            "no default-tool add/remove lists survive"
        );
        // Every axis uses power exponents; charts carry no card (border + inset).
        assert!(
            html.contains("exponentformat: 'power'"),
            "axes use power exponents"
        );
        let cc = html
            .find(".chart-container {")
            .expect("chart-container rule");
        let rule = &html[cc..cc + html[cc..].find('}').unwrap()];
        assert!(
            rule.contains("border: none") && rule.contains("background: transparent"),
            "charts carry no card (no border, no inset fill): {rule}"
        );
    }

    #[test]
    fn s5_item5_colours_kit_accent_and_unified_error_ink() {
        let html = render_rows(&pair_test_rows());
        // The light-theme text accent is the kit value #2d6da3.
        assert!(
            html.contains("--ed-accent: #2d6da3"),
            "the light accent is the kit text-accent #2d6da3"
        );
        // The two freelanced error reds collapse to the one error token.
        assert!(
            !html.contains("#e0654a"),
            "the second error red is unified onto the error token"
        );
        // The masthead dim ink reads a token, not a bespoke rgba.
        assert!(
            html.contains(".masthead h1 .mh-dim { color: var(--ed-text-muted); }")
                && !html.contains("rgba(232,232,236,0.38)"),
            "the dim masthead ink reads a token"
        );
        // The compare-bar tool categorical colours are gone (names only).
        assert!(
            !html.contains("style=\"color:var(--tool-empyrean)\""),
            "the compare-bar selects carry no per-tool categorical colour"
        );
    }

    #[test]
    fn s5_item6_timing_key_line_and_a2_absent_line_is_data_driven() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&timing_fixture(), &[], &out, None, None, &[]).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        // The 2.2 timing tables carry ONE visible key line; the header-row hover
        // sentence is gone from the speed matrix. Delta 9: the key line is a terse
        // unit key, not a sentence (reading guidance moved to the Method fold).
        assert!(
            html.contains("<div class=\"grid-summary\">ms · p5 p50 p95 · n · by boundary</div>"),
            "the speed matrix states its reading in one terse unit key line"
        );
        assert!(
            html.contains("id=\"t1-speed-matrix\"><thead><tr><th class=\"gobj\">arm</th>"),
            "the speed-matrix header row no longer carries a hover sentence"
        );
        // The A.2 "planned / opt-in" line is data-driven: the old static long
        // hover naming all three tools is gone.
        assert!(
            !html.contains("OrbFit Consortium / IAU MPC (CMC2003 rejection)"),
            "the A.2 planned line is computed from the run, not a static enumeration"
        );
    }

    /// Delta 9 item 3: a key line (`.grid-summary`, or the readout unit key) names
    /// units/axes in the terse `·` idiom and must never carry a sentence. A
    /// "sentence" here is a period followed by whitespace and a capital (an
    /// embedded sentence boundary), OR a run of prose — more than 8 words with no
    /// `·` field separator. Readout SUMMARY lines (id `…-summary`) are exempt:
    /// the brief budgets them at ≤25 words of numbers. Fails on the pre-delta
    /// lines ("Wall clock … Read within a boundary." / "Median … read down a
    /// column."); the self-checks below prove the detector can fail.
    #[test]
    fn delta9_key_lines_are_terse_not_sentences() {
        fn visible(inner: &str) -> String {
            let mut out = String::new();
            let mut depth = 0i32;
            for ch in inner.chars() {
                match ch {
                    '<' => depth += 1,
                    '>' => depth = (depth - 1).max(0),
                    _ if depth == 0 => out.push(ch),
                    _ => {}
                }
            }
            out.split_whitespace().collect::<Vec<_>>().join(" ")
        }
        fn alpha_words(s: &str) -> usize {
            s.split_whitespace()
                .filter(|w| w.chars().any(|c| c.is_ascii_alphabetic()))
                .count()
        }
        fn period_then_capital(s: &str) -> bool {
            let c: Vec<char> = s.chars().collect();
            for i in 0..c.len() {
                if c[i] == '.' {
                    let mut j = i + 1;
                    while j < c.len() && c[j].is_whitespace() {
                        j += 1;
                    }
                    if j < c.len() && c[j].is_ascii_uppercase() {
                        return true;
                    }
                }
            }
            false
        }
        fn is_sentence(s: &str) -> bool {
            period_then_capital(s) || (!s.contains('·') && alpha_words(s) > 8)
        }
        // The detector proves it can fail — pre-delta sentences flag, terse keys
        // and short numeric readouts do not.
        assert!(is_sentence(
            "Median propagation ms per population; read down a column."
        ));
        assert!(is_sentence(
            "Wall clock (ms) per arm; p5 · p50 · p95 · n, grouped by boundary. Read within a boundary."
        ));
        assert!(!is_sentence("ms · p5 p50 p95 · n · by boundary"));
        assert!(!is_sentence("median ms · by population"));
        assert!(!is_sentence("km"));
        assert!(!is_sentence("No walk-forward data in this report."));

        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&timing_fixture(), &[], &out, None, None, &[]).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        // The key lines this delta governs are the Part 1 and Part 2 unit keys
        // (the C·cost timing keys and the Part-2 readout keys km/mas/σ_eq); scan
        // up to Part 3. Downstream parts carry ≤25-word numeric readout summaries
        // (e.g. §4.1's "Median σ_eq … too tight.") that are not unit keys.
        let scope_end = html.find("id=\"part3\"").unwrap_or(html.len());
        for (attrs, inner) in elements_by_class(&html[..scope_end], "grid-summary") {
            // Readout summary lines carry the section's numbers at the ≤25-word
            // budget, not a unit key — exempt (id `…-summary`).
            if attrs.contains("-summary\"") {
                continue;
            }
            let text = visible(inner);
            assert!(
                !is_sentence(&text),
                "grid-summary key line is a sentence: {text:?} (attrs {attrs:?})"
            );
        }
    }

    /// Delta 9 item 1: the masthead no longer states the channel under test. No
    /// "RUST"/"Rust channel under test" (nor any "Rust") survives in the
    /// masthead; the statement moved once to Part 3's scope line (eyebrow role).
    #[test]
    fn delta9_masthead_has_no_rust_channel_under_test() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&timing_fixture(), &[], &out, None, None, &[]).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        let mstart = html.find("<div class=\"masthead\">").expect("masthead");
        let mend = mstart
            + html[mstart..]
                .find("<div class=\"layout-wrap\">")
                .expect("masthead end");
        let masthead = &html[mstart..mend];
        assert!(
            !masthead.contains("RUST"),
            "masthead must not carry upper-case RUST: {masthead:?}"
        );
        assert!(
            !masthead.contains("Rust channel under test"),
            "the masthead 'Rust channel under test' subtitle is gone"
        );
        assert!(
            !masthead.contains("Rust"),
            "the masthead carries no 'Rust' at all: {masthead:?}"
        );
        // The statement moved once to Part 3's scope line.
        let p3 = html.find("id=\"part3\"").expect("part3 head");
        let p3win = &html[p3..(p3 + 800).min(html.len())];
        assert!(
            p3win.contains("data-k=\"channels\"") && p3win.contains("rust (under test)"),
            "Part 3 states the channel under test once in its eyebrow scope line: {p3win:?}"
        );
    }
    // ── Per-method widening panels ──────────────────────────────────

    /// A `detection_on` method row for the matrix / outcome / disagreement
    /// tests: a propagation row on `channel` under `method`, with a nominal
    /// position so cross-channel parity has something to compare.
    fn method_row(object: &str, channel: &str, method: &str, test_type: &str) -> ValidationResult {
        use crate::schema::uncertainty_modes as um;
        let mut r = ValidationResult::empty();
        r.object = object.to_string();
        r.population = "NEO".to_string();
        r.epoch_mjd_tdb = 61000.0;
        r.dt_days = 30.0;
        r.t_mjd_tdb = 61030.0;
        r.force_model = "standard".to_string();
        r.test_type = test_type.to_string();
        r.channel = channel.to_string();
        r.emp_pos_au = Some([1.0, 2.0, 3.0]);
        r.propagation_uncertainty = Some(um::compose(method, um::DETECTION_ON));
        r.timestamp = "2026-04-29T00:00:00Z".to_string();
        r
    }

    #[test]
    fn method_matrix_emits_container_and_sidecar_carries_its_fields() {
        use crate::schema::uncertainty_modes as um;
        let mut rows = Vec::new();
        for &m in &um::METHODS {
            rows.push(method_row("Apophis", "rust", m, "propagation"));
            if m != um::GAUSSIAN_MIXTURE {
                rows.push(method_row("Apophis", "core", m, "propagation"));
            }
        }
        let html = build_method_matrix_html(&rows);
        // The matrix is rendered client-side (renderMethodMatrix) into this
        // container; the server emits the container, the `method` header, and
        // every engine channel as a column — a channel absent this run still
        // gets a column in the client, never a dropped one.
        assert!(
            html.contains("id=\"method-matrix-body\""),
            "matrix body container missing"
        );
        assert!(html.contains(">method</th>"), "method header missing");
        for ch in ["rust", "core", "c", "python", "cli"] {
            assert!(
                html.contains(&format!(">{ch}</th>")),
                "channel column {ch} missing"
            );
        }
        // Data contract: a row carrying every field the matrix client reads to
        // classify a cell (parity, refusal, auto resolved rung) serializes them
        // to the sidecar, so a renamed or dropped field surfaces here.
        let mut contract = method_row("Apophis", "rust", "auto", "propagation");
        contract.resolved_method = Some("first_order_detection_on".to_string());
        contract.orbit_delivered = Some(false);
        contract.orbit_status = Some("failed:curvature_exceeded".to_string());
        let json = serde_json::to_string(&contract).unwrap();
        for field in [
            "propagation_uncertainty",
            "emp_pos_au",
            "resolved_method",
            "orbit_delivered",
            "orbit_status",
            "object",
            "channel",
            "test_type",
            "population",
            "force_model",
            "dt_days",
        ] {
            assert!(
                json.contains(&format!("\"{field}\"")),
                "sidecar row missing {field}, which the matrix client reads"
            );
        }
        // E1 contract: the FULL status message (which the client puts in the
        // cell's title attribute, never in the cell text — the cell shows a
        // compact "refused (n)" badge) round-trips verbatim through the sidecar.
        let long_status = "orbit determination failed for Apophis: differential correction \
             failed: uncertainty method SecondOrder has no honest implementation on \
             determine_object";
        let mut refused = method_row("Apophis", "rust", "second_order", "orbit_determination");
        refused.orbit_delivered = Some(false);
        refused.orbit_status = Some(long_status.to_string());
        assert!(
            serde_json::to_string(&refused)
                .unwrap()
                .contains(long_status),
            "the full status message must reach the sidecar verbatim for the cell hover"
        );
    }

    #[test]
    fn method_matrix_auto_resolved_rung_reaches_the_sidecar() {
        // The auto cell is keyed on the resolved rung (never a numeric parity
        // verdict); the resolved method the client keys on must reach the
        // sidecar, distinct per channel so a mismatch is visible.
        let mut rust = method_row("Bennu", "rust", "auto", "propagation");
        rust.resolved_method = Some("first_order_detection_on".to_string());
        let mut core = method_row("Bennu", "core", "auto", "propagation");
        core.resolved_method = Some("second_order_detection_on".to_string());
        let jr = serde_json::to_string(&rust).unwrap();
        let jc = serde_json::to_string(&core).unwrap();
        let html = build_method_matrix_html(&[rust, core]);
        assert!(
            html.contains("id=\"method-matrix-body\""),
            "matrix container missing"
        );
        assert!(
            jr.contains("\"resolved_method\":\"first_order_detection_on\""),
            "rust resolved rung missing from the sidecar row"
        );
        assert!(
            jc.contains("\"resolved_method\":\"second_order_detection_on\""),
            "core resolved rung missing from the sidecar row"
        );
    }

    #[test]
    fn disagreement_series_reads_sky_sigma_off_the_row() {
        // σ_RA from sky-cov[0][0], σ_Dec from [1][1] — distinct values so a
        // swap is caught.
        let mut r = method_row("Apophis", "rust", "first_order", "ephemeris");
        r.emp_radec_cov_arcsec2 = Some([[4.0, 0.0], [0.0, 9.0]]);
        let series = disagreement_sigma_series(&[r]);
        let p = series
            .iter()
            .find(|p| p.object == "Apophis" && p.method == "first_order")
            .expect("first_order point missing");
        assert!(
            (p.sigma_ra_arcsec.unwrap() - 2.0).abs() < 1e-12,
            "σ_RA must come from sky-cov[0][0] (=2.0), got {:?}",
            p.sigma_ra_arcsec
        );
        assert!(
            (p.sigma_dec_arcsec.unwrap() - 3.0).abs() < 1e-12,
            "σ_Dec must come from sky-cov[1][1] (=3.0), got {:?}",
            p.sigma_dec_arcsec
        );
    }

    #[test]
    fn disagreement_sigma_pos_reads_lower_triangular_position_block() {
        // The packed joint is lower-triangular: the 3×3 position-block diagonal
        // is at indices [0], [2], [5] — NOT the upper-triangular [0], [3], [5].
        // cov_tri = [v00, v10, v11, v20, v21, v22]; distinct off-diagonals make
        // the two readings diverge (correct trace 1+4+9=14, upper read 1+7+9=17).
        let mut r = method_row("Apophis", "rust", "second_order", "propagation");
        r.emp_pos_cov_au2 = None; // force the packed-joint path
        r.cov_tri = Some(vec![1.0, 0.0, 4.0, 7.0, 0.0, 9.0]);
        let series = disagreement_sigma_series(&[r]);
        let p = series
            .iter()
            .find(|p| p.object == "Apophis" && p.method == "second_order")
            .expect("second_order point missing");
        let expect = (1.0_f64 + 4.0 + 9.0).sqrt() * AU_KM;
        let got = p.sigma_pos_km.expect("σ_pos missing");
        assert!(
            (got - expect).abs() / expect < 1e-12,
            "σ_pos must read the lower-triangular diagonal (√14·AU_KM = {expect}), got {got}"
        );
    }

    #[test]
    fn outcome_table_emits_container_and_carries_mixture_fields() {
        let mut mix = method_row("Apophis", "rust", "gaussian_mixture", "propagation");
        mix.orbit_delivered = Some(true);
        mix.orbit_status = Some("delivered".to_string());
        mix.cov_kind = Some(3);
        mix.cov_joint_width = Some(6);
        mix.mix_n_components_total = Some(5);
        mix.mix_weight_delivered = Some(0.97);
        mix.mix_n_failed = Some(1);
        mix.mix_n_curvature_refused = Some(2);
        mix.mix_n_sky_linearization_refused = Some(3);
        let json = serde_json::to_string(&mix).unwrap();
        let html = build_method_outcome_html(&[mix]);
        // A run that holds an outcome row emits the container (not the empty
        // state); the client (renderMethodOutcome) fills these tally columns.
        assert!(
            html.contains("id=\"method-outcome-body\""),
            "outcome body container missing"
        );
        for hcol in ["mixture", "cov kind", "joint W", "resolved"] {
            assert!(
                html.contains(&format!(">{hcol}</th>")),
                "outcome header {hcol} missing"
            );
        }
        // Data contract: the mixture tallies and covariance descriptors the
        // client renders reach the sidecar row.
        for field in [
            "mix_n_components_total",
            "mix_weight_delivered",
            "mix_n_failed",
            "mix_n_curvature_refused",
            "mix_n_sky_linearization_refused",
            "cov_kind",
            "cov_joint_width",
            "orbit_delivered",
            "orbit_status",
        ] {
            assert!(
                json.contains(&format!("\"{field}\"")),
                "sidecar row missing {field}, which the outcome client reads"
            );
        }
    }

    #[test]
    fn outcome_rows_exclude_bare_covariance_prop_and_eph() {
        // Against the packed-joint distribution every covariance-bearing row
        // carries a cov_kind; that alone must NOT make an ephemeris or
        // propagation row an outcome row (the over-match that ballooned the
        // table). An OD-family transport row with a covariance IS one, and a
        // mixture product row is one regardless of its test type.
        let mut eph = method_row("Apophis", "rust", "first_order", "ephemeris");
        eph.cov_kind = Some(3);
        assert!(
            !is_outcome_row(&eph),
            "an ephemeris row carrying only a packed-joint covariance is not an outcome row"
        );
        let mut prop = method_row("Apophis", "rust", "second_order", "propagation");
        prop.cov_kind = Some(3);
        assert!(
            !is_outcome_row(&prop),
            "a propagation row carrying only a packed-joint covariance is not an outcome row"
        );
        let mut trans = method_row(
            "Apophis",
            "rust",
            "first_order",
            "orbit_determination_transport",
        );
        trans.cov_kind = Some(3);
        assert!(
            is_outcome_row(&trans),
            "an OD post-fit transport row (with a covariance) is an outcome row"
        );
        let mut mix = method_row("Apophis", "rust", "gaussian_mixture", "propagation");
        mix.mix_n_components_total = Some(5);
        assert!(
            is_outcome_row(&mix),
            "a mixture product row is an outcome row regardless of test type"
        );
        // A DELIVERED propagation/ephemeris product is an ordinary product row
        // (the per-method matrix's job), not an outcome.
        let mut delivered = method_row("Apophis", "rust", "first_order", "ephemeris");
        delivered.orbit_delivered = Some(true);
        delivered.orbit_status = Some(DELIVERED_STATUS.to_string());
        delivered.cov_kind = Some(3);
        assert!(
            !is_outcome_row(&delivered),
            "a delivered ephemeris product row is not an outcome row"
        );
        // An orbit the engine did NOT deliver is an outcome row.
        let mut undelivered = method_row("Apophis", "rust", "first_order", "ephemeris");
        undelivered.orbit_delivered = Some(false);
        undelivered.orbit_status = Some("failed:curvature_exceeded".to_string());
        assert!(
            is_outcome_row(&undelivered),
            "an undelivered ephemeris row is an outcome row"
        );
        // A row carrying only a failure/withheld status (no delivery flag) is an
        // outcome row; the delivered status alone is not.
        let mut withheld = method_row("Apophis", "rust", "first_order", "ephemeris");
        withheld.orbit_status = Some("withheld: residual-only evaluation".to_string());
        assert!(
            is_outcome_row(&withheld),
            "a row carrying only a non-delivered status is an outcome row"
        );
        let mut ok_status_only = method_row("Apophis", "rust", "first_order", "ephemeris");
        ok_status_only.orbit_status = Some(DELIVERED_STATUS.to_string());
        assert!(
            !is_outcome_row(&ok_status_only),
            "the delivered status alone does not make a row an outcome row"
        );
    }

    #[test]
    fn convergence_matrix_shows_iterations_and_status() {
        let mut rust = method_row("Apophis", "rust", "first_order", "orbit_determination");
        rust.od_converged = Some(true);
        rust.od_iterations = Some(7);
        let mut core = method_row("Apophis", "core", "first_order", "orbit_determination");
        core.od_converged = Some(false);
        core.notes = "singular normal matrix".to_string();
        // find_orb folds onto the reference (core) row; low coverage reads as
        // its placeholder fallback, not a fit — an external-tool column with
        // its own status.
        core.findorb_rms_residual = Some(1.5);
        core.findorb_n_obs_used = Some(10);
        core.findorb_n_obs_rejected = Some(90);
        let html = build_convergence_matrix_html(&[rust, core]);
        assert!(
            html.contains("converged in 7 iterations") && html.contains("&#10003; 7"),
            "converged cell did not show its iteration count"
        );
        assert!(
            html.contains("did not converge — singular normal matrix"),
            "a non-converged fit did not render its status"
        );
        // Every tool the plan fits with is a column: the external find_orb
        // column is present and renders its own status.
        assert!(
            html.contains(">find_orb</th>"),
            "the external find_orb column is missing"
        );
        assert!(
            html.contains("below the 50% coverage floor"),
            "the external fitter's status was not rendered"
        );
    }

    #[test]
    fn convergence_matrix_calls_out_a_channel_with_no_od_rows() {
        // A channel present in the run (here python, a propagation row) but with
        // no OD rows has no column; the absence is called out as text, not left
        // to be noticed.
        let mut rust = method_row("Apophis", "rust", "first_order", "orbit_determination");
        rust.od_converged = Some(true);
        let python = method_row("Apophis", "python", "first_order", "propagation");
        let html = build_convergence_matrix_html(&[rust, python]);
        assert!(
            html.contains("Present in this run with no orbit-determination rows at all"),
            "the no-OD-rows channel call-out is missing"
        );
        assert!(html.contains("python"), "the absent channel is not named");
    }

    #[test]
    fn dataset_descriptor_matches_sidecar_with_method_rows() {
        // The page-side verifier, run in Rust: byte length, row count and
        // FNV-1a-32 of the written sidecar must all match the inline
        // descriptor — WITH the widened method / mixture rows present.
        let mut mix = method_row("Apophis", "rust", "gaussian_mixture", "propagation");
        mix.mix_n_components_total = Some(4);
        mix.mix_n_sky_linearization_refused = Some(2);
        mix.cov_tri = Some(vec![1.0, 0.0, 1.0, 0.0, 0.0, 1.0]);
        let rows = vec![
            synthetic_rust_prop_row("Apophis", 0.0),
            method_row("Apophis", "rust", "second_order", "propagation"),
            mix,
        ];
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.html");
        generate_report(&rows, &[], &out, None, None, &[]).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        let data_bytes = std::fs::read(dir.path().join("report.data.json")).unwrap();
        // The new fields reached the sidecar.
        let data_str = String::from_utf8(data_bytes.clone()).unwrap();
        assert!(data_str.contains("second_order_detection_on"));
        assert!(data_str.contains("mix_n_sky_linearization_refused"));
        // Parse the inline descriptor and verify it against the sidecar the
        // way the page's loadDataset() does.
        let marker = "\"file\":\"report.data.json\"";
        let i = html.find(marker).expect("inline descriptor missing");
        let start = html[..i].rfind('{').unwrap();
        let end = start + html[start..].find('}').unwrap() + 1;
        let desc: serde_json::Value = serde_json::from_str(&html[start..end]).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&data_bytes).unwrap();
        assert_eq!(desc["bytes"].as_u64().unwrap() as usize, data_bytes.len());
        assert_eq!(
            desc["rows"].as_u64().unwrap() as usize,
            parsed.as_array().unwrap().len()
        );
        assert_eq!(desc["rows"].as_u64().unwrap() as usize, rows.len());
        assert_eq!(desc["hash"].as_str().unwrap(), fnv1a32_hex(&data_bytes));
    }
}
