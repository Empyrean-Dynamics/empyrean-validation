//! Curated validation object catalog.
//!
//! Each object carries SBDB and Horizons query strings, MPC designation,
//! population classification, optional custom time grid, and OD-skip
//! flag. Lifted verbatim from the original
//! `empyrean/validation/runners/rust/src/objects.rs` so this crate is the
//! single source of truth for what we test.
//!
//! # Populations
//!
//! - **NEO** — Apophis, Bennu, Didymos, Duende, Eros, Farnocchia, 2020 AV2
//! - **MBA** — Lutetia, Nysa, Holman, Parijskij
//! - **Self-Perturber** — Iris, Vesta, Pallas, Hygiea (SB441-N16 bodies that
//!   appear in the perturber set; their OD must exclude themselves)
//! - **TNO** — Eris, Makemake, Varuna
//! - **Comet** — 67P, 2P/Encke, 103P/Hartley 2, 46P/Wirtanen
//! - **Centaur** — Chiron
//! - **Jupiter Trojan** — Hektor, Patroclus, Achilles
//! - **Neptune Trojan** — 2001 QR322, 2005 TN53, 2008 LC18
//! - **Earth Trojan** — 2010 TK7, 2020 XL5
//! - **ISO** — 1I/'Oumuamua, 2I/Borisov
//! - **TCO** — 2020 CD3, 2024 PT5
//! - **Impactor** — 2008 TC3, 2023 CX1, 2024 BX1, 2014 AA, 2018 LA
//! - **Short-arc NEO** — 2026 FQ12, 2026 FO12, 2026 DA
//!
//! Total: 43 objects across 13 populations.

use serde::Serialize;

/// A celestial object for validation (propagation, ephemeris, and/or OD).
///
/// Catalog entries are `&'static` so the whole catalog lives in `.rodata`
/// at zero runtime cost. `Deserialize` is intentionally not derived —
/// the catalog is the source of truth, not a sink. Generate
/// `catalog.json` for external consumers via `serde_json::to_string`.
#[derive(Debug, Clone, Serialize)]
pub struct ValidationObject {
    /// Display name. Matches `ValidationResult::object` in the result schema.
    pub name: &'static str,
    /// JPL SBDB query string for the initial-condition fetch.
    pub sbdb_query: &'static str,
    /// JPL Horizons command string (e.g., `"99942;"` for numbered, or
    /// `"DES=2020 AV2;"` for unnumbered designations).
    pub horizons_command: &'static str,
    /// MPC designation for fetching observations (used by OD validation).
    pub mpc_designation: &'static str,
    /// One of the population strings (`"NEO"`, `"Comet"`, …).
    pub population: &'static str,
    /// Custom time offsets in days from orbit epoch. `None` → use
    /// [`DEFAULT_DT_DAYS`].
    pub dt_days: Option<&'static [f64]>,
    /// Free-form notes carried into the validation report's per-row
    /// hover text.
    pub notes: &'static str,
    /// If true, orbit must be sourced from Horizons (not SBDB). Used for
    /// objects with custom epochs that diverge from SBDB's fit.
    pub horizons_only: bool,
    /// Custom epoch (MJD TDB) for `horizons_only` objects.
    pub epoch_mjd: Option<f64>,
    /// Skip OD validation for this object (e.g., self-perturbers whose
    /// OD requires special handling, or objects with insufficient
    /// observation arcs).
    pub skip_od: bool,
}

// ── Default time grid ──────────────────────────────────────

/// Default propagation `dt` grid: ±15 yr from epoch sampled densely near
/// epoch and sparsely at the wings. Captures both short-baseline ULP-grade
/// agreement and long-baseline integrator drift.
pub const DEFAULT_DT_DAYS: &[f64] = &[
    -5475.0, -3650.0, -1825.0, -1095.0, -365.0, -180.0, -90.0, -30.0, 0.0, 30.0, 90.0, 180.0,
    365.0, 1095.0, 1825.0, 3650.0, 5475.0,
];

// ── Force model tiers ──────────────────────────────────────

/// Force-model tiers exercised by the validation suite. Today the suite
/// runs only Standard ("full" string survives for backward compatibility
/// with archived JSON files in GCS); Approximate / Basic / Standard are
/// the variants empyrean-core exposes.
pub const FORCE_MODEL_TIERS: &[&str] = &["full"];

// ── Observer codes ─────────────────────────────────────────

/// MPC observer codes exercised for ephemeris-side validation.
///
/// - `W84` — CTIO 4m
/// - `F51` — Pan-STARRS 1
/// - `X05` — La Silla
/// - `500` — Geocenter (sanity check; no parallax)
/// - `I41` — ATLAS Mauna Loa
pub const OBSERVER_CODES: &[&str] = &["W84", "F51", "X05", "500", "I41"];

// ── Population colors (brand guide) ────────────────────────

/// Population color from the brand palette.
///
/// Returns the hex string used by the validation report's heatmap +
/// time-series traces. Mirrors the spielberg chart palette
/// (`variables.css`) so reports and the web frontend stay visually
/// consistent.
pub fn population_color(pop: &str) -> &'static str {
    match pop {
        "NEO" => "#5b9bd5",            // Arctic Blue (accent)
        "MBA" => "#e8a040",            // Celestial Amber (event)
        "Self-Perturber" => "#a070d0", // Violet
        "TNO" => "#40c0c0",            // Teal
        "Comet" => "#60c060",          // Green
        "Centaur" => "#d070a0",        // Pink
        "Jupiter Trojan" => "#80b040", // Olive
        "Neptune Trojan" => "#3d7ab8", // Secondary blue
        "Earth Trojan" => "#c8a040",   // Warning amber
        "ISO" => "#f04060",            // Red (error)
        "TCO" => "#7bb8e8",            // Light arctic blue
        "Impactor" => "#d05040",       // Deep red
        "Short-arc NEO" => "#4a88c2",  // Tertiary blue
        _ => "#8b9198",                // Text secondary
    }
}

// ── Impact locations ───────────────────────────────────────

/// Geographic location where a confirmed impactor entered the atmosphere.
/// Used by the validation report to overlay impact corridors.
#[derive(Debug, Clone)]
pub struct ImpactLocation {
    pub name: &'static str,
    pub lat: f64,
    pub lon: f64,
    pub location: &'static str,
}

/// Confirmed atmospheric-impact locations for impactor-population objects.
pub const IMPACT_LOCATIONS: &[ImpactLocation] = &[
    ImpactLocation {
        name: "2008 TC3",
        lat: 20.75,
        lon: 32.19,
        location: "Nubian Desert, Sudan",
    },
    ImpactLocation {
        name: "2023 CX1",
        lat: 49.9,
        lon: 0.8,
        location: "English Channel/Normandy",
    },
    ImpactLocation {
        name: "2024 BX1",
        lat: 52.5,
        lon: 13.4,
        location: "Near Berlin, Germany",
    },
    ImpactLocation {
        name: "2014 AA",
        lat: 12.0,
        lon: -44.0,
        location: "Mid-Atlantic Ocean (estimated)",
    },
    ImpactLocation {
        name: "2018 LA",
        lat: -21.5,
        lon: 27.8,
        location: "Botswana/South Africa border",
    },
];

// ── Object catalog ─────────────────────────────────────────

/// Impactors can only be propagated backward from epoch (they hit the
/// Earth, so any forward propagation would integrate through the
/// collision).
static IMPACTOR_DT: &[f64] = &[
    -5475.0, -3650.0, -1825.0, -1095.0, -365.0, -180.0, -90.0, -30.0, 0.0,
];

const NEOS: &[ValidationObject] = &[
    ValidationObject {
        name: "Apophis",
        sbdb_query: "Apophis",
        horizons_command: "99942;",
        mpc_designation: "99942",
        population: "NEO",
        dt_days: None,
        notes: "2029 Earth CA at dt~+1239d",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
    ValidationObject {
        name: "Bennu",
        sbdb_query: "Bennu",
        horizons_command: "101955;",
        mpc_designation: "101955",
        population: "NEO",
        dt_days: None,
        notes: "OSIRIS-REx target",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
    ValidationObject {
        name: "Didymos",
        sbdb_query: "Didymos",
        horizons_command: "65803;",
        mpc_designation: "65803",
        population: "NEO",
        dt_days: None,
        notes: "DART target",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
    ValidationObject {
        name: "Duende",
        sbdb_query: "Duende",
        horizons_command: "367943;",
        mpc_designation: "367943",
        population: "NEO",
        dt_days: None,
        notes: "2013 Earth CA at 34,050 km",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
    ValidationObject {
        name: "Eros",
        sbdb_query: "Eros",
        horizons_command: "433;",
        mpc_designation: "433",
        population: "NEO",
        dt_days: None,
        notes: "NEAR target, Mars-crosser",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
    ValidationObject {
        name: "Farnocchia",
        sbdb_query: "Farnocchia",
        horizons_command: "84100;",
        mpc_designation: "84100",
        population: "NEO",
        dt_days: None,
        notes: "ASSIST primary validation object",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
    ValidationObject {
        name: "2020 AV2",
        sbdb_query: "2020 AV2",
        horizons_command: "DES=2020 AV2;",
        mpc_designation: "2020 AV2",
        population: "NEO",
        dt_days: None,
        notes: "Vatira, orbit entirely inside Venus",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
];

const MBAS: &[ValidationObject] = &[
    ValidationObject {
        name: "Lutetia",
        sbdb_query: "Lutetia",
        horizons_command: "21;",
        mpc_designation: "21",
        population: "MBA",
        dt_days: None,
        notes: "Rosetta flyby target",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
    ValidationObject {
        name: "Nysa",
        sbdb_query: "Nysa",
        horizons_command: "44;",
        mpc_designation: "44",
        population: "MBA",
        dt_days: None,
        notes: "S-type MBA",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
    ValidationObject {
        name: "Holman",
        sbdb_query: "Holman",
        horizons_command: "3666;",
        mpc_designation: "3666",
        population: "MBA",
        dt_days: None,
        notes: "ASSIST primary test object",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
    ValidationObject {
        name: "Parijskij",
        sbdb_query: "Parijskij",
        horizons_command: "5303;",
        mpc_designation: "5303",
        population: "MBA",
        dt_days: None,
        notes: "ASSIST test case: close encounter with Ceres over ~10 yr",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
];

/// Self-Perturbers — SB441-N16 bodies that appear in the perturber set.
/// Their OD must exclude themselves to avoid the "body pulled by its own
/// gravity" artifact.
const SELF_PERTURBERS: &[ValidationObject] = &[
    ValidationObject {
        name: "Iris",
        sbdb_query: "7 Iris",
        horizons_command: "7;",
        mpc_designation: "7",
        population: "Self-Perturber",
        dt_days: None,
        notes: "SB441-N16 perturber #7, catastrophic self-perturbation",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: true,
    },
    ValidationObject {
        name: "Vesta",
        sbdb_query: "Vesta",
        horizons_command: "4;",
        mpc_designation: "4",
        population: "Self-Perturber",
        dt_days: None,
        notes: "SB441-N16 perturber, Dawn target",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: true,
    },
    ValidationObject {
        name: "Pallas",
        sbdb_query: "Pallas",
        horizons_command: "2;",
        mpc_designation: "2",
        population: "Self-Perturber",
        dt_days: None,
        notes: "SB441-N16 perturber, 2nd most massive",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: true,
    },
    ValidationObject {
        name: "Hygiea",
        sbdb_query: "Hygiea",
        horizons_command: "10;",
        mpc_designation: "10",
        population: "Self-Perturber",
        dt_days: None,
        notes: "SB441-N16 perturber, 4th most massive",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: true,
    },
];

const TNOS: &[ValidationObject] = &[
    ValidationObject {
        name: "Eris",
        sbdb_query: "Eris",
        horizons_command: "136199;",
        mpc_designation: "136199",
        population: "TNO",
        dt_days: None,
        notes: "Most massive known TNO",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
    ValidationObject {
        name: "Makemake",
        sbdb_query: "Makemake",
        horizons_command: "136472;",
        mpc_designation: "136472",
        population: "TNO",
        dt_days: None,
        notes: "Classical KBO",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
    ValidationObject {
        name: "Varuna",
        sbdb_query: "Varuna",
        horizons_command: "20000;",
        mpc_designation: "20000",
        population: "TNO",
        dt_days: None,
        notes: "Hot classical KBO",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
];

const COMETS: &[ValidationObject] = &[
    ValidationObject {
        name: "67P",
        sbdb_query: "67P",
        horizons_command: "DES=67P;CAP;NOFRAG",
        mpc_designation: "67P",
        population: "Comet",
        dt_days: None,
        notes: "Rosetta target, well-characterized non-grav",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
    ValidationObject {
        name: "2P/Encke",
        sbdb_query: "2P",
        horizons_command: "DES=2P;CAP;NOFRAG",
        mpc_designation: "2P",
        population: "Comet",
        dt_days: None,
        notes: "Shortest period comet, strong non-grav",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
    ValidationObject {
        name: "103P/Hartley 2",
        sbdb_query: "103P",
        horizons_command: "DES=103P;CAP;NOFRAG",
        mpc_designation: "103P",
        population: "Comet",
        dt_days: None,
        notes: "EPOXI target, well-characterized non-grav",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
    ValidationObject {
        name: "46P/Wirtanen",
        sbdb_query: "46P",
        horizons_command: "DES=46P;CAP;NOFRAG",
        mpc_designation: "46P",
        population: "Comet",
        dt_days: None,
        notes: "2018 close Earth approach",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
];

const CENTAURS: &[ValidationObject] = &[ValidationObject {
    name: "Chiron",
    sbdb_query: "Chiron",
    horizons_command: "2060;",
    mpc_designation: "2060",
    population: "Centaur",
    dt_days: None,
    notes: "Between Jupiter and Neptune",
    horizons_only: false,
    epoch_mjd: None,
    skip_od: false,
}];

const JUPITER_TROJANS: &[ValidationObject] = &[
    ValidationObject {
        name: "Hektor",
        sbdb_query: "Hektor",
        horizons_command: "624;",
        mpc_designation: "624",
        population: "Jupiter Trojan",
        dt_days: None,
        notes: "L4, largest Trojan, contact binary",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
    ValidationObject {
        name: "Patroclus",
        sbdb_query: "Patroclus",
        horizons_command: "617;",
        mpc_designation: "617",
        population: "Jupiter Trojan",
        dt_days: None,
        notes: "L5, first known binary Trojan",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
    ValidationObject {
        name: "Achilles",
        sbdb_query: "Achilles",
        horizons_command: "588;",
        mpc_designation: "588",
        population: "Jupiter Trojan",
        dt_days: None,
        notes: "L4, first Trojan discovered (1906)",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
];

const NEPTUNE_TROJANS: &[ValidationObject] = &[
    ValidationObject {
        name: "2001 QR322",
        sbdb_query: "2001 QR322",
        horizons_command: "DES=2001 QR322;",
        mpc_designation: "2001 QR322",
        population: "Neptune Trojan",
        dt_days: None,
        notes: "L4, first Neptune Trojan discovered",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
    ValidationObject {
        name: "2005 TN53",
        sbdb_query: "2005 TN53",
        horizons_command: "DES=2005 TN53;",
        mpc_designation: "2005 TN53",
        population: "Neptune Trojan",
        dt_days: None,
        notes: "L4, high-inclination (25 deg)",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
    ValidationObject {
        name: "2008 LC18",
        sbdb_query: "2008 LC18",
        horizons_command: "DES=2008 LC18;",
        mpc_designation: "2008 LC18",
        population: "Neptune Trojan",
        dt_days: None,
        notes: "L5, first L5 Neptune Trojan found",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
];

const EARTH_TROJANS: &[ValidationObject] = &[
    ValidationObject {
        name: "2010 TK7",
        sbdb_query: "2010 TK7",
        horizons_command: "DES=2010 TK7;",
        mpc_designation: "2010 TK7",
        population: "Earth Trojan",
        dt_days: None,
        notes: "L4, first confirmed Earth Trojan",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
    ValidationObject {
        name: "2020 XL5",
        sbdb_query: "2020 XL5",
        horizons_command: "DES=2020 XL5;",
        mpc_designation: "2020 XL5",
        population: "Earth Trojan",
        dt_days: None,
        notes: "L4, ~1.2 km",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
];

const ISOS: &[ValidationObject] = &[
    ValidationObject {
        name: "1I/'Oumuamua",
        sbdb_query: "1I",
        horizons_command: "DES=1I;",
        mpc_designation: "1I",
        population: "ISO",
        dt_days: None,
        notes: "e=1.2, custom g(r) non-grav from SBDB",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
    ValidationObject {
        name: "2I/Borisov",
        sbdb_query: "2I",
        horizons_command: "DES=2I;",
        mpc_designation: "2I",
        population: "ISO",
        dt_days: None,
        notes: "e=3.4, interstellar comet",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
];

const TCOS: &[ValidationObject] = &[
    ValidationObject {
        name: "2020 CD3",
        sbdb_query: "2020 CD3",
        horizons_command: "DES=2020 CD3;",
        mpc_designation: "2020 CD3",
        population: "TCO",
        dt_days: None,
        notes: "Earth capture ~2018-2020",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
    ValidationObject {
        name: "2024 PT5",
        sbdb_query: "2024 PT5",
        horizons_command: "DES=2024 PT5;",
        mpc_designation: "2024 PT5",
        population: "TCO",
        dt_days: None,
        notes: "Earth capture Sep-Nov 2024",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
];

const IMPACTORS: &[ValidationObject] = &[
    ValidationObject {
        name: "2008 TC3",
        sbdb_query: "2008 TC3",
        horizons_command: "DES=2008 TC3;",
        mpc_designation: "2008 TC3",
        population: "Impactor",
        dt_days: Some(IMPACTOR_DT),
        notes: "Sudan, meteorites recovered (Almahata Sitta)",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
    ValidationObject {
        name: "2023 CX1",
        sbdb_query: "2023 CX1",
        horizons_command: "DES=2023 CX1;",
        mpc_designation: "2023 CX1",
        population: "Impactor",
        dt_days: Some(IMPACTOR_DT),
        notes: "Normandy coast, meteorites recovered",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
    ValidationObject {
        name: "2024 BX1",
        sbdb_query: "2024 BX1",
        horizons_command: "DES=2024 BX1;",
        mpc_designation: "2024 BX1",
        population: "Impactor",
        dt_days: Some(IMPACTOR_DT),
        notes: "Near Berlin, meteorites recovered",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
    ValidationObject {
        name: "2014 AA",
        sbdb_query: "2014 AA",
        horizons_command: "DES=2014 AA;",
        mpc_designation: "2014 AA",
        population: "Impactor",
        dt_days: Some(IMPACTOR_DT),
        notes: "Mid-Atlantic Ocean",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
    ValidationObject {
        name: "2018 LA",
        sbdb_query: "2018 LA",
        horizons_command: "DES=2018 LA;",
        mpc_designation: "2018 LA",
        population: "Impactor",
        dt_days: Some(IMPACTOR_DT),
        notes: "Botswana/South Africa border, meteorites recovered",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
];

/// Recently-discovered NEOs whose observation arc is too short for
/// long-baseline propagation tests but still useful for OD validation.
const SHORT_ARC_NEOS: &[ValidationObject] = &[
    ValidationObject {
        name: "2026 FQ12",
        sbdb_query: "2026 FQ12",
        horizons_command: "DES=2026 FQ12;",
        mpc_designation: "2026 FQ12",
        population: "Short-arc NEO",
        dt_days: None,
        notes: "Recent discovery Mar 2026",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
    ValidationObject {
        name: "2026 FO12",
        sbdb_query: "2026 FO12",
        horizons_command: "DES=2026 FO12;",
        mpc_designation: "2026 FO12",
        population: "Short-arc NEO",
        dt_days: None,
        notes: "Recent discovery Mar 2026",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
    ValidationObject {
        name: "2026 DA",
        sbdb_query: "2026 DA",
        horizons_command: "DES=2026 DA;",
        mpc_designation: "2026 DA",
        population: "Short-arc NEO",
        dt_days: None,
        notes: "2026 discovery",
        horizons_only: false,
        epoch_mjd: None,
        skip_od: false,
    },
];

/// Returns the full validation object catalog (43 objects across 13
/// populations). Order matches the populations enumerated in the module
/// docstring.
pub fn all_objects() -> Vec<&'static ValidationObject> {
    let mut out = Vec::new();
    for group in &[
        NEOS,
        MBAS,
        SELF_PERTURBERS,
        TNOS,
        COMETS,
        CENTAURS,
        JUPITER_TROJANS,
        NEPTUNE_TROJANS,
        EARTH_TROJANS,
        ISOS,
        TCOS,
        IMPACTORS,
        SHORT_ARC_NEOS,
    ] {
        out.extend(group.iter());
    }
    out
}

/// Filter objects by population names (case-insensitive).
pub fn filter_by_population(pops: &[&str]) -> Vec<&'static ValidationObject> {
    let pops_lower: Vec<String> = pops.iter().map(|p| p.to_lowercase()).collect();
    all_objects()
        .into_iter()
        .filter(|o| pops_lower.contains(&o.population.to_lowercase()))
        .collect()
}

/// Filter objects by name (case-insensitive).
pub fn filter_by_name(names: &[&str]) -> Vec<&'static ValidationObject> {
    let names_lower: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
    all_objects()
        .into_iter()
        .filter(|o| names_lower.contains(&o.name.to_lowercase()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_43_objects() {
        assert_eq!(all_objects().len(), 43);
    }

    #[test]
    fn catalog_has_13_populations() {
        let mut pops: Vec<&str> = all_objects().iter().map(|o| o.population).collect();
        pops.sort();
        pops.dedup();
        assert_eq!(pops.len(), 13);
    }

    #[test]
    fn no_duplicate_object_names() {
        let mut names: Vec<&str> = all_objects().iter().map(|o| o.name).collect();
        names.sort();
        let n = names.len();
        names.dedup();
        assert_eq!(names.len(), n, "duplicate object name in catalog");
    }

    #[test]
    fn self_perturbers_skip_od_by_default() {
        let sp = filter_by_population(&["Self-Perturber"]);
        assert_eq!(sp.len(), 4);
        assert!(sp.iter().all(|o| o.skip_od));
    }

    #[test]
    fn impactors_use_backward_only_dt_grid() {
        let imp = filter_by_population(&["Impactor"]);
        assert_eq!(imp.len(), 5);
        assert!(imp.iter().all(|o| {
            o.dt_days
                .map(|grid| grid.iter().all(|&dt| dt <= 0.0))
                .unwrap_or(false)
        }));
    }

    #[test]
    fn population_color_returns_hex() {
        for pop in [
            "NEO",
            "MBA",
            "Self-Perturber",
            "TNO",
            "Comet",
            "Centaur",
            "Jupiter Trojan",
            "Neptune Trojan",
            "Earth Trojan",
            "ISO",
            "TCO",
            "Impactor",
            "Short-arc NEO",
        ] {
            let c = population_color(pop);
            assert!(c.starts_with('#'), "{pop} → {c}");
            assert_eq!(c.len(), 7, "{pop} → {c}");
        }
    }

    #[test]
    fn unknown_population_falls_back_to_default() {
        assert_eq!(population_color("???"), "#8b9198");
    }

    #[test]
    fn filter_by_name_finds_apophis() {
        let r = filter_by_name(&["Apophis"]);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].mpc_designation, "99942");
    }

    #[test]
    fn filter_by_name_is_case_insensitive() {
        let r = filter_by_name(&["apophis"]);
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn observer_codes_are_valid_mpc_format() {
        for code in OBSERVER_CODES {
            assert!(code.len() == 3 || code.len() == 4, "{code}");
        }
    }
}
