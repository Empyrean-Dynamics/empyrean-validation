//! The one registry of the tools the report compares.
//!
//! Every place the report names a tool for the Part 1 Compare bar reads this
//! table: the Tool and Reference selects, the pair panels' builders, the page's
//! chart colours and `--tool-<slug>` tokens, the client's row-field readers, the
//! axis coverage, and the server-side "is this tool in the run" probe. A new
//! comparator is added here once; nothing else lists tools by hand.
//!
//! A field is declared once as a [`Field`]: the row-field name the page's
//! JavaScript reads and the Rust accessor the server-side builders read are
//! produced by the same macro token, so the two cannot name different fields.

use crate::schema::ValidationResult;

/// One row field a tool contributes: its serialized name (what the page reads)
/// and the accessor the server-side builders read it with.
#[derive(Clone, Copy)]
pub struct Field {
    pub name: &'static str,
    pub get: fn(&ValidationResult) -> Option<f64>,
}

/// An `Option<f64>` row field.
macro_rules! fld {
    ($f:ident) => {
        Some(Field {
            name: stringify!($f),
            get: |r: &ValidationResult| r.$f,
        })
    };
}

/// An `Option<u32>` row field, read as a number.
macro_rules! fldu {
    ($f:ident) => {
        Some(Field {
            name: stringify!($f),
            get: |r: &ValidationResult| r.$f.map(f64::from),
        })
    };
}

/// When a tool's chi-square is a fit of the whole arc.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Chi2Rule {
    /// Always.
    Always,
    /// Only where layup's flag says a fit of the whole arc converged
    /// ([`ValidationResult::layup_chi2_is_a_fit`]); the page mirrors it as
    /// `layupFitOfArc`.
    LayupWholeArc,
}

/// One tool the Compare bar can name.
pub struct ToolSpec {
    /// The selector value, `data-tool` / `data-ref` and the tool id elsewhere.
    pub slug: &'static str,
    /// The label the selects and panels show.
    pub label: &'static str,
    /// The chart colour (a literal hex: Plotly cannot resolve a CSS variable).
    pub color: &'static str,
    /// The theme-aware `--tool-<slug>` text colours, (dark, light), for tools
    /// whose name is shown as DOM text in their own colour.
    pub token: Option<(&'static str, &'static str)>,
    /// The reference everything is compared against: its position and sky
    /// offset are the origin (zero) of the Horizons-anchored fields.
    pub truth: bool,
    /// Propagation position difference from Horizons, km.
    pub prop_vs_jpl: Option<Field>,
    /// Propagation position difference from empyrean, km.
    pub prop_vs_emp: Option<Field>,
    /// Ephemeris sky separation from Horizons, arcsec (a scalar: no direction).
    pub eph_sep: Option<Field>,
    /// Signed ephemeris offset from Horizons, dRA cos(dec) and dDec, arcsec. A
    /// tool with both can form a pair with any other tool that has both.
    pub eph_d_ra: Option<Field>,
    pub eph_d_dec: Option<Field>,
    /// Wall clock, ms.
    pub time: Option<Field>,
    /// Orbit determination: post-fit rms (arcsec), reduced chi-square, and the
    /// observations used.
    pub od_rms: Option<Field>,
    pub od_chi2: Option<Field>,
    pub od_nobs: Option<Field>,
    pub chi2_rule: Chi2Rule,
}

impl ToolSpec {
    /// The axes this tool covers, derived from the fields it declares: no axis is
    /// claimed that no field backs.
    pub fn axes(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.truth || self.prop_vs_jpl.is_some() || self.prop_vs_emp.is_some() {
            v.push("prop_pos");
        }
        if self.truth || (self.eph_d_ra.is_some() && self.eph_d_dec.is_some()) {
            v.push("eph");
        }
        if self.od_rms.is_some() {
            v.push("od_rms");
        }
        if self.od_chi2.is_some() {
            v.push("od_chi2");
        }
        if self.od_nobs.is_some() {
            v.push("od_nobs");
        }
        if self.time.is_some() {
            v.push("time");
        }
        v
    }

    /// Every field this tool declares, for the presence probe and the dataset
    /// projection.
    pub fn fields(&self) -> Vec<Field> {
        [
            self.prop_vs_jpl,
            self.prop_vs_emp,
            self.eph_sep,
            self.eph_d_ra,
            self.eph_d_dec,
            self.time,
            self.od_rms,
            self.od_chi2,
            self.od_nobs,
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// The reduced chi-square of `r` for this tool, under its own rule for when
    /// the number is a fit of the whole arc.
    pub fn chi2(&self, r: &ValidationResult) -> Option<f64> {
        let v = self.od_chi2.and_then(|f| (f.get)(r))?;
        match self.chi2_rule {
            Chi2Rule::Always => Some(v),
            Chi2Rule::LayupWholeArc => r.layup_chi2_is_a_fit().then_some(v),
        }
    }

    /// Does `r` carry anything this tool measured?
    pub fn has_any(&self, r: &ValidationResult) -> bool {
        self.fields().iter().any(|f| (f.get)(r).is_some())
    }
}

/// The tools, in the order the selects offer them. empyrean leads the Tool
/// select and JPL the Reference select; the rest follow in a stable order.
pub static TOOLS: &[ToolSpec] = &[
    ToolSpec {
        slug: "empyrean",
        label: "empyrean",
        color: "#5b9bd5",
        token: Some(("#5b9bd5", "#2f6a9e")),
        truth: false,
        prop_vs_jpl: fld!(emp_vs_horizons_km),
        prop_vs_emp: None,
        eph_sep: fld!(separation_arcsec),
        eph_d_ra: fld!(d_ra_arcsec),
        eph_d_dec: fld!(d_dec_arcsec),
        time: fld!(emp_time_ms),
        od_rms: fld!(od_rms_combined_arcsec),
        od_chi2: fld!(od_reduced_chi2),
        od_nobs: fldu!(n_obs_used),
        chi2_rule: Chi2Rule::Always,
    },
    ToolSpec {
        slug: "jpl",
        label: "JPL",
        color: "#3d9a6d",
        token: Some(("#3d9a6d", "#2c7550")),
        truth: true,
        prop_vs_jpl: None,
        prop_vs_emp: None,
        eph_sep: None,
        eph_d_ra: None,
        eph_d_dec: None,
        time: None,
        od_rms: None,
        od_chi2: fld!(ref_od_reduced_chi2),
        od_nobs: fldu!(ref_od_n_obs_used),
        chi2_rule: Chi2Rule::Always,
    },
    ToolSpec {
        slug: "assist",
        label: "ASSIST",
        color: "#c77dff",
        token: Some(("#c77dff", "#7c3aed")),
        truth: false,
        prop_vs_jpl: fld!(assist_vs_horizons_km),
        prop_vs_emp: fld!(emp_vs_assist_km),
        eph_sep: None,
        eph_d_ra: None,
        eph_d_dec: None,
        time: fld!(assist_time_ms),
        od_rms: None,
        od_chi2: None,
        od_nobs: None,
        chi2_rule: Chi2Rule::Always,
    },
    ToolSpec {
        slug: "findorb",
        label: "find_orb",
        color: "#d05040",
        token: Some(("#dd6154", "#b83a2c")),
        truth: false,
        prop_vs_jpl: fld!(findorb_vs_horizons_km),
        prop_vs_emp: fld!(emp_vs_findorb_km),
        eph_sep: fld!(findorb_separation_arcsec),
        eph_d_ra: fld!(findorb_d_ra_arcsec),
        eph_d_dec: fld!(findorb_d_dec_arcsec),
        time: fld!(findorb_time_ms),
        od_rms: fld!(findorb_rms_residual),
        od_chi2: None,
        od_nobs: fldu!(findorb_n_obs_used),
        chi2_rule: Chi2Rule::Always,
    },
    ToolSpec {
        slug: "oorb",
        label: "OpenOrb",
        color: "#e8a040",
        token: None,
        truth: false,
        prop_vs_jpl: fld!(oorb_vs_horizons_km),
        prop_vs_emp: fld!(emp_vs_oorb_km),
        eph_sep: fld!(oorb_separation_arcsec),
        eph_d_ra: fld!(oorb_d_ra_arcsec),
        eph_d_dec: fld!(oorb_d_dec_arcsec),
        time: fld!(oorb_time_ms),
        od_rms: None,
        od_chi2: None,
        od_nobs: None,
        chi2_rule: Chi2Rule::Always,
    },
    ToolSpec {
        slug: "orbfit",
        label: "OrbFit",
        color: "#7dd3c0",
        token: None,
        truth: false,
        prop_vs_jpl: None,
        prop_vs_emp: None,
        eph_sep: None,
        eph_d_ra: None,
        eph_d_dec: None,
        time: fld!(orbfit_time_ms),
        od_rms: fld!(orbfit_rms_arcsec),
        od_chi2: None,
        od_nobs: fldu!(orbfit_n_obs_used),
        chi2_rule: Chi2Rule::Always,
    },
    ToolSpec {
        slug: "layup",
        label: "layup",
        color: "#e07bc0",
        token: None,
        truth: false,
        prop_vs_jpl: None,
        prop_vs_emp: None,
        eph_sep: None,
        eph_d_ra: None,
        eph_d_dec: None,
        time: fld!(layup_time_ms),
        od_rms: None,
        od_chi2: fld!(layup_reduced_chi2),
        od_nobs: fldu!(layup_n_obs_used),
        chi2_rule: Chi2Rule::LayupWholeArc,
    },
    ToolSpec {
        slug: "grss",
        label: "GRSS",
        color: "#8fbf5f",
        token: None,
        truth: false,
        prop_vs_jpl: fld!(grss_vs_horizons_km),
        prop_vs_emp: fld!(emp_vs_grss_km),
        eph_sep: fld!(grss_separation_arcsec),
        eph_d_ra: fld!(grss_d_ra_arcsec),
        eph_d_dec: fld!(grss_d_dec_arcsec),
        time: fld!(grss_time_ms),
        od_rms: fld!(grss_rms_arcsec),
        od_chi2: fld!(grss_reduced_chi2),
        od_nobs: fldu!(grss_n_obs_used),
        chi2_rule: Chi2Rule::Always,
    },
    // adam-core, the Python package, and adam-core (Rust), the Rust API: one
    // release, two surfaces, one field shape. No emp-vs-adam-core position
    // difference is stored, so propagation pairs it with JPL only.
    ToolSpec {
        slug: "adam-core",
        label: "adam-core",
        color: "#3f9fb0",
        token: Some(("#4fb3c4", "#1f6f7e")),
        truth: false,
        prop_vs_jpl: fld!(adam_core_vs_horizons_km),
        prop_vs_emp: None,
        eph_sep: fld!(adam_core_separation_arcsec),
        eph_d_ra: fld!(adam_core_d_ra_arcsec),
        eph_d_dec: fld!(adam_core_d_dec_arcsec),
        time: fld!(adam_core_time_ms),
        od_rms: None,
        od_chi2: fld!(adam_core_reduced_chi2),
        od_nobs: fldu!(adam_core_n_obs_used),
        chi2_rule: Chi2Rule::Always,
    },
    ToolSpec {
        slug: "adam-core-rust",
        label: "adam-core (Rust)",
        color: "#8fd9e6",
        token: Some(("#7fcfdc", "#176070")),
        truth: false,
        prop_vs_jpl: fld!(adam_core_rust_vs_horizons_km),
        prop_vs_emp: None,
        eph_sep: fld!(adam_core_rust_separation_arcsec),
        eph_d_ra: fld!(adam_core_rust_d_ra_arcsec),
        eph_d_dec: fld!(adam_core_rust_d_dec_arcsec),
        time: fld!(adam_core_rust_time_ms),
        od_rms: None,
        od_chi2: fld!(adam_core_rust_reduced_chi2),
        od_nobs: fldu!(adam_core_rust_n_obs_used),
        chi2_rule: Chi2Rule::Always,
    },
    // kete and jorbit propagate and time; they store no position difference, so
    // they appear on the timing slots only.
    ToolSpec {
        slug: "kete",
        label: "kete",
        color: "#e6c84a",
        token: Some(("#d9b93a", "#7a6410")),
        truth: false,
        prop_vs_jpl: None,
        prop_vs_emp: None,
        eph_sep: None,
        eph_d_ra: None,
        eph_d_dec: None,
        time: fld!(kete_time_ms),
        od_rms: None,
        od_chi2: None,
        od_nobs: None,
        chi2_rule: Chi2Rule::Always,
    },
    ToolSpec {
        slug: "jorbit",
        label: "jorbit",
        color: "#c49a6c",
        token: Some(("#c49a6c", "#7a5428")),
        truth: false,
        prop_vs_jpl: None,
        prop_vs_emp: None,
        eph_sep: None,
        eph_d_ra: None,
        eph_d_dec: None,
        time: fld!(jorbit_time_ms),
        od_rms: None,
        od_chi2: None,
        od_nobs: None,
        chi2_rule: Chi2Rule::Always,
    },
];

/// Comparators that can carry rows in a run yet form no Part 1 pair, with the
/// reason. Every comparator in [`TOOLS`] is offered by a select or is named here;
/// a test enforces it. Empty today: kete and jorbit pair on timing.
pub static NOT_PAIRABLE: &[(&str, &str)] = &[];

/// The tool with this slug.
pub fn tool(slug: &str) -> Option<&'static ToolSpec> {
    TOOLS.iter().find(|t| t.slug == slug)
}

/// The tools other than empyrean and the reference, in select order.
pub fn externals() -> impl Iterator<Item = &'static ToolSpec> {
    TOOLS.iter().filter(|t| t.slug != "empyrean" && !t.truth)
}

/// The registry as the page's JSON: the one table the client builds its tool
/// map, axis coverage and row-field readers from.
pub fn registry_json() -> String {
    let name = |f: Option<Field>| f.map(|f| serde_json::Value::from(f.name));
    let v: Vec<serde_json::Value> = TOOLS
        .iter()
        .map(|t| {
            serde_json::json!({
                "slug": t.slug,
                "label": t.label,
                "color": t.color,
                "truth": t.truth,
                "axes": t.axes(),
                "prop_vs_jpl": name(t.prop_vs_jpl),
                "prop_vs_emp": name(t.prop_vs_emp),
                "eph_sep": name(t.eph_sep),
                "eph_d_ra": name(t.eph_d_ra),
                "eph_d_dec": name(t.eph_d_dec),
                "time": name(t.time),
                "od_rms": name(t.od_rms),
                "od_chi2": name(t.od_chi2),
                "od_nobs": name(t.od_nobs),
                "chi2_rule": match t.chi2_rule {
                    Chi2Rule::Always => "always",
                    Chi2Rule::LayupWholeArc => "layup",
                },
            })
        })
        .collect();
    serde_json::to_string(&v).unwrap_or_default()
}

/// Every row field the registry names, for the dataset projection and the tests.
pub fn all_field_names() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = TOOLS
        .iter()
        .flat_map(|t| t.fields().into_iter().map(|f| f.name))
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// The `--tool-<slug>` tokens: one `(slug, dark, light)` per tool that has one.
pub fn color_tokens() -> Vec<(&'static str, &'static str, &'static str)> {
    TOOLS
        .iter()
        .filter_map(|t| t.token.map(|(d, l)| (t.slug, d, l)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_unique_and_lead_with_empyrean_then_jpl() {
        let mut s: Vec<&str> = TOOLS.iter().map(|t| t.slug).collect();
        assert_eq!(&s[..2], ["empyrean", "jpl"]);
        s.sort_unstable();
        let n = s.len();
        s.dedup();
        assert_eq!(s.len(), n, "a tool slug is listed twice");
    }

    #[test]
    fn every_field_name_is_the_name_its_accessor_reads() {
        // Set each registered name in the serialized row, read the row back through
        // the accessor: the page's name and the builders' accessor are one field.
        for t in TOOLS {
            for f in t.fields() {
                let mut v = serde_json::to_value(ValidationResult::empty()).unwrap();
                v[f.name] = serde_json::json!(7);
                let row: ValidationResult = serde_json::from_value(v)
                    .unwrap_or_else(|e| panic!("{} on {}: {e}", f.name, t.slug));
                assert_eq!((f.get)(&row), Some(7.0), "{} on {}", f.name, t.slug);
            }
        }
    }

    #[test]
    fn axes_are_backed_by_fields() {
        let layup = tool("layup").unwrap();
        assert_eq!(layup.axes(), ["od_chi2", "od_nobs", "time"]);
        let kete = tool("kete").unwrap();
        assert_eq!(kete.axes(), ["time"]);
        let ac = tool("adam-core").unwrap();
        assert!(ac.axes().contains(&"eph") && ac.axes().contains(&"prop_pos"));
    }

    #[test]
    fn layup_chi2_follows_its_own_whole_arc_rule() {
        let l = tool("layup").unwrap();
        let mut r = ValidationResult::empty();
        r.layup_reduced_chi2 = Some(1.5);
        r.layup_flag = Some(0);
        assert_eq!(l.chi2(&r), Some(1.5));
        r.layup_flag = Some(1);
        assert_eq!(l.chi2(&r), None, "a failed candidate's chi2 is not a fit");
    }

    #[test]
    fn not_pairable_names_registered_tools_with_a_reason() {
        for (slug, why) in NOT_PAIRABLE {
            assert!(tool(slug).is_some(), "{slug} is not a registered tool");
            assert!(!why.is_empty());
        }
    }
}
