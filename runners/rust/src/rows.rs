//! Row identity, selection, repetition overrides and the dropped-row file for
//! the propagation / ephemeris runner.
//!
//! The reference used to be computed one whole object per CI leg, so the one
//! object holding the 42-minute 2020 CD3 propagation row outran every budget.
//! A leg now takes ROWS: `validate run --list-rows` prints the candidate rows
//! in the order the run emits them, scripts/schedule.py packs them into legs
//! by cost, and `validate run --select <rows.json>` computes only the listed
//! rows. A selected row the engine refuses produces no result row (the run has
//! always dropped those); the dropped-row file names each one and gives the
//! engine's reason, so the assemble step can tell a row the engine refused
//! from a row lost to a dead leg.
//!
//! The row identity is the harness-wide key `(object, test_type,
//! propagation_uncertainty, dt_days, observer, force_model)` that
//! scripts/schedule.py uses for every channel. The three files this module
//! reads and writes share one JSON shape, a list of objects carrying those
//! six fields (extra fields are ignored, so a plan row is a valid selection
//! entry):
//!
//! * the `--select` file: the rows to compute;
//! * the `--reps-override` file: the same entries with a `reps` field, the
//!   timing repetitions that row runs instead of the runner's default;
//! * the `<output>.dropped.json` file: the same entries with a `reason`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One propagation or ephemeris row's identity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RowId {
    pub object: String,
    pub test_type: String,
    pub propagation_uncertainty: String,
    pub dt_days: f64,
    pub observer: Option<String>,
    pub force_model: String,
}

impl RowId {
    /// A propagation row: no observer, the force-model tier of the sweep.
    pub fn propagation(object: &str, tag: &str, tier: &str, dt_days: f64) -> Self {
        Self {
            object: object.to_string(),
            test_type: "propagation".to_string(),
            propagation_uncertainty: tag.to_string(),
            dt_days,
            observer: None,
            force_model: tier.to_string(),
        }
    }

    /// An ephemeris row: one observing site, always the `standard` tier.
    pub fn ephemeris(object: &str, tag: &str, observer: &str, dt_days: f64) -> Self {
        Self {
            object: object.to_string(),
            test_type: "ephemeris".to_string(),
            propagation_uncertainty: tag.to_string(),
            dt_days,
            observer: Some(observer.to_string()),
            force_model: "standard".to_string(),
        }
    }

    /// The identity as one string. `{:?}` of an `f64` always carries the
    /// decimal point (`-5475.0`), so the same value written by Python or by
    /// this runner keys identically.
    pub fn key(&self) -> String {
        format!(
            "{}|{}|{}|{:?}|{}|{}",
            self.object,
            self.test_type,
            self.propagation_uncertainty,
            self.dt_days,
            self.observer.as_deref().unwrap_or(""),
            self.force_model
        )
    }
}

/// The rows a leg computes.
#[derive(Debug, Default)]
pub struct RowSelection {
    keys: HashSet<String>,
}

impl RowSelection {
    pub fn from_rows(rows: &[RowId]) -> Self {
        Self {
            keys: rows.iter().map(RowId::key).collect(),
        }
    }

    pub fn contains(&self, id: &RowId) -> bool {
        self.keys.contains(&id.key())
    }
}

/// Per-row timing repetitions that replace the runner's default.
#[derive(Debug, Default)]
pub struct RepsOverride {
    reps: HashMap<String, usize>,
}

impl RepsOverride {
    pub fn get(&self, id: &RowId) -> Option<usize> {
        self.reps.get(&id.key()).copied()
    }
}

#[derive(Deserialize)]
struct RepsEntry {
    #[serde(flatten)]
    id: RowId,
    reps: usize,
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Read a `--select` file: a JSON list of row identities.
pub fn load_rows(path: &Path) -> Result<Vec<RowId>, String> {
    let rows: Vec<RowId> = serde_json::from_str(&read(path)?)
        .map_err(|e| format!("{}: not a list of row identities: {e}", path.display()))?;
    let mut seen = HashSet::new();
    for r in &rows {
        if !seen.insert(r.key()) {
            return Err(format!(
                "{}: the row {} is listed twice",
                path.display(),
                r.key()
            ));
        }
    }
    Ok(rows)
}

/// Read a `--reps-override` file: a JSON list of row identities with `reps`.
/// A zero count is refused: a row that runs no timing repetition has no result.
pub fn load_reps_override(path: &Path) -> Result<RepsOverride, String> {
    let entries: Vec<RepsEntry> = serde_json::from_str(&read(path)?).map_err(|e| {
        format!(
            "{}: not a list of row identities with `reps`: {e}",
            path.display()
        )
    })?;
    let mut reps = HashMap::new();
    for e in entries {
        if e.reps == 0 {
            return Err(format!(
                "{}: row {} asks for 0 repetitions",
                path.display(),
                e.id.key()
            ));
        }
        if reps.insert(e.id.key(), e.reps).is_some() {
            return Err(format!(
                "{}: the row {} is listed twice",
                path.display(),
                e.id.key()
            ));
        }
    }
    Ok(RepsOverride { reps })
}

/// One selected row that produced no result row, and why.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DroppedRow {
    #[serde(flatten)]
    pub id: RowId,
    pub reason: String,
}

/// `<output>.dropped.json` beside the main output.
pub fn dropped_path(output: &Path) -> PathBuf {
    let mut name = output
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".dropped.json");
    output.with_file_name(name)
}

/// Every selected row is either emitted or dropped, never both and never
/// neither, and the emitted rows keep the candidate order. A violation means
/// the candidate list and the run disagree about what a row is, which would
/// let a leg silently lose or invent rows; the run refuses to write.
pub fn check_accounted(
    candidates: &[RowId],
    selection: Option<&RowSelection>,
    emitted: &[String],
    dropped: &[DroppedRow],
) -> Result<(), String> {
    let order: HashMap<String, usize> = candidates
        .iter()
        .enumerate()
        .map(|(i, r)| (r.key(), i))
        .collect();
    let mut last: Option<usize> = None;
    let mut seen = HashSet::new();
    for k in emitted {
        let Some(&i) = order.get(k) else {
            return Err(format!(
                "the run emitted a row that is not a candidate: {k}"
            ));
        };
        if !seen.insert(k.clone()) {
            return Err(format!("the run emitted the row {k} twice"));
        }
        if last.is_some_and(|l| i < l) {
            return Err(format!(
                "the run emitted {k} out of the candidate order (--list-rows disagrees with the run)"
            ));
        }
        last = Some(i);
    }
    for d in dropped {
        let k = d.id.key();
        if !order.contains_key(&k) {
            return Err(format!("a dropped row is not a candidate: {k}"));
        }
        if !seen.insert(k.clone()) {
            return Err(format!(
                "the row {k} is both emitted and dropped, or dropped twice"
            ));
        }
    }
    let wanted = |c: &RowId| selection.is_none_or(|s| s.contains(c));
    let missing: Vec<&RowId> = candidates
        .iter()
        .filter(|c| wanted(c) && !seen.contains(&c.key()))
        .collect();
    if let Some(m) = missing.first() {
        return Err(format!(
            "{} selected row(s) are neither emitted nor dropped (first: {})",
            missing.len(),
            m.key()
        ));
    }
    if let Some(stray) = candidates
        .iter()
        .find(|c| !wanted(c) && seen.contains(&c.key()))
    {
        return Err(format!(
            "the run touched a row outside the selection: {}",
            stray.key()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(dt: f64) -> RowId {
        RowId::propagation("A", "auto_detection_on", "standard", dt)
    }

    #[test]
    fn key_carries_the_decimal_point_so_python_and_rust_agree() {
        assert_eq!(
            p(-5475.0).key(),
            "A|propagation|auto_detection_on|-5475.0||standard"
        );
        let e = RowId::ephemeris("A", "none_detection_on", "W84", 30.0);
        assert_eq!(e.key(), "A|ephemeris|none_detection_on|30.0|W84|standard");
    }

    #[test]
    fn selection_round_trips_through_json_and_ignores_extra_fields() {
        let dir = tempdir();
        let f = dir.join("sel.json");
        std::fs::write(
            &f,
            r#"[{"object":"A","test_type":"propagation","propagation_uncertainty":"auto_detection_on",
                 "dt_days":-5475,"observer":null,"force_model":"standard","emp_time_ms":null,"extra":1}]"#,
        )
        .unwrap();
        let rows = load_rows(&f).unwrap();
        assert_eq!(rows, vec![p(-5475.0)]);
        assert!(RowSelection::from_rows(&rows).contains(&p(-5475.0)));
        assert!(!RowSelection::from_rows(&rows).contains(&p(-3650.0)));
    }

    #[test]
    fn a_row_listed_twice_is_refused() {
        let dir = tempdir();
        let f = dir.join("dup.json");
        let one = r#"{"object":"A","test_type":"propagation","propagation_uncertainty":"x","dt_days":1.0,"observer":null,"force_model":"standard"}"#;
        std::fs::write(&f, format!("[{one},{one}]")).unwrap();
        let err = load_rows(&f).unwrap_err();
        assert!(err.contains("listed twice"), "{err}");
    }

    #[test]
    fn reps_override_reads_counts_and_refuses_zero() {
        let dir = tempdir();
        let f = dir.join("reps.json");
        std::fs::write(
            &f,
            r#"[{"object":"A","test_type":"propagation","propagation_uncertainty":"auto_detection_on","dt_days":-5475.0,"observer":null,"force_model":"standard","reps":1}]"#,
        )
        .unwrap();
        let o = load_reps_override(&f).unwrap();
        assert_eq!(o.get(&p(-5475.0)), Some(1));
        assert_eq!(o.get(&p(-3650.0)), None);
        std::fs::write(
            &f,
            r#"[{"object":"A","test_type":"propagation","propagation_uncertainty":"auto_detection_on","dt_days":-5475.0,"observer":null,"force_model":"standard","reps":0}]"#,
        )
        .unwrap();
        assert!(
            load_reps_override(&f)
                .unwrap_err()
                .contains("0 repetitions")
        );
    }

    #[test]
    fn dropped_path_sits_beside_the_output() {
        assert_eq!(
            dropped_path(Path::new("results/shards/x/validation_rust_propeph.json")),
            PathBuf::from("results/shards/x/validation_rust_propeph.json.dropped.json")
        );
    }

    #[test]
    fn dropped_row_serializes_flat_with_its_reason() {
        let d = DroppedRow {
            id: p(1.0),
            reason: "engine FAIL (x)".into(),
        };
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(v["object"], "A");
        assert_eq!(v["reason"], "engine FAIL (x)");
        let back: DroppedRow = serde_json::from_value(v).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn accounting_accepts_emitted_plus_dropped_in_candidate_order() {
        let c = vec![p(1.0), p(2.0), p(3.0)];
        let sel = RowSelection::from_rows(&c);
        let dropped = vec![DroppedRow {
            id: p(2.0),
            reason: "r".into(),
        }];
        check_accounted(&c, Some(&sel), &[p(1.0).key(), p(3.0).key()], &dropped).unwrap();
    }

    #[test]
    fn accounting_refuses_a_selected_row_that_is_neither_emitted_nor_dropped() {
        let c = vec![p(1.0), p(2.0)];
        let sel = RowSelection::from_rows(&c);
        let err = check_accounted(&c, Some(&sel), &[p(1.0).key()], &[]).unwrap_err();
        assert!(err.contains("neither emitted nor dropped"), "{err}");
    }

    #[test]
    fn accounting_refuses_a_row_both_emitted_and_dropped() {
        let c = vec![p(1.0)];
        let dropped = vec![DroppedRow {
            id: p(1.0),
            reason: "r".into(),
        }];
        let err = check_accounted(&c, None, &[p(1.0).key()], &dropped).unwrap_err();
        assert!(err.contains("both emitted and dropped"), "{err}");
    }

    #[test]
    fn accounting_refuses_an_emission_out_of_candidate_order() {
        let c = vec![p(1.0), p(2.0)];
        let err = check_accounted(&c, None, &[p(2.0).key(), p(1.0).key()], &[]).unwrap_err();
        assert!(err.contains("out of the candidate order"), "{err}");
    }

    #[test]
    fn accounting_refuses_an_emitted_row_that_is_not_a_candidate() {
        let c = vec![p(1.0)];
        let err = check_accounted(&c, None, &[p(9.0).key()], &[]).unwrap_err();
        assert!(err.contains("not a candidate"), "{err}");
    }

    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let d = std::env::temp_dir().join(format!(
            "rows-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}
