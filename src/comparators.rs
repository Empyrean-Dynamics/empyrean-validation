//! Per-comparator isolation for `merge-external`.
//!
//! Each external tool's file is loaded and folded independently. A file that is
//! empty, truncated or not the shape the merge reads fails **that tool only**:
//! the tool is named with its reason, recorded in a sidecar beside the merged
//! output, and the merge, the report and `ci-check` go on. Nothing disappears
//! silently: the sidecar is read by `report`, which prints every failed
//! comparator in the Configurations appendix and the CI summary.
//!
//! The same shape rule is mirrored in `scripts/validate_external.py`, which each
//! external leg runs before upload (the legs carry no harness binary), so a
//! malformed file fails its own leg instead of reaching `reduce`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Whether a comparator's file was folded in or skipped.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ComparatorStatus {
    /// The file was loaded and folded (possibly onto zero rows).
    Merged,
    /// The file was absent, unreadable, malformed, or its merge failed; the
    /// comparator was skipped and `reason` says why.
    Failed,
}

/// One merge path of a comparator and the rows it landed on.
///
/// A comparator can fold onto the report along several paths that fail
/// independently: find_orb's OD fits, its propagation and ephemeris references
/// (the one that needs an engine context), ASSIST's folded reference rows and
/// its standalone per-arm rows. The outcome's headline `rows` cannot say which
/// of them is empty, so each path's count is recorded by name and carried into
/// the CI summary, where `ci-check` can require it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MergePath {
    /// The path's name (`od`, `propagation`, `ephemeris`, `radar`, `rows`,
    /// `folded`, `standalone`, `timing`, `od-reference`, ...).
    pub path: String,
    /// Rows the path landed on or appended.
    pub rows: usize,
}

/// What one comparator's fold produced: the headline row count (unchanged
/// from before per-path counts) and every merge path's count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folded {
    pub rows: usize,
    pub paths: Vec<MergePath>,
}

impl Folded {
    /// A comparator with one merge path: its headline count is the path's.
    pub fn single(path: &str, rows: usize) -> Self {
        Self::with_paths(rows, &[(path, rows)])
    }

    /// A headline count and its paths, in the order the merge ran them.
    pub fn with_paths(rows: usize, paths: &[(&str, usize)]) -> Self {
        Self {
            rows,
            paths: paths
                .iter()
                .map(|(path, rows)| MergePath {
                    path: (*path).to_string(),
                    rows: *rows,
                })
                .collect(),
        }
    }
}

/// One comparator's outcome in a `merge-external` run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ComparatorOutcome {
    /// The merge flag the comparator arrived on, without the dashes
    /// (`assist`, `findorb-radar`, `grss`, ...).
    pub tool: String,
    pub status: ComparatorStatus,
    /// Rows the comparator was folded onto (0 for a failed comparator).
    pub rows: usize,
    /// Why it failed; `None` when merged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Every merge path's row count, by name (empty for a failed comparator,
    /// and for a sidecar written before per-path counts existed).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<MergePath>,
}

impl ComparatorOutcome {
    /// A comparator that merged with only its headline count (no per-path
    /// counts); the production merges use [`ComparatorOutcome::folded`].
    pub fn merged(tool: &str, rows: usize) -> Self {
        Self {
            tool: tool.to_string(),
            status: ComparatorStatus::Merged,
            rows,
            reason: None,
            paths: Vec::new(),
        }
    }

    /// A comparator that merged, with every merge path's count.
    pub fn folded(tool: &str, folded: Folded) -> Self {
        Self {
            tool: tool.to_string(),
            status: ComparatorStatus::Merged,
            rows: folded.rows,
            reason: None,
            paths: folded.paths,
        }
    }

    pub fn failed(tool: &str, reason: impl Into<String>) -> Self {
        Self {
            tool: tool.to_string(),
            status: ComparatorStatus::Failed,
            rows: 0,
            reason: Some(reason.into()),
            paths: Vec::new(),
        }
    }

    pub fn is_failed(&self) -> bool {
        self.status == ComparatorStatus::Failed
    }

    /// `failed: <reason>`, the one spelling the log, the report and the summary
    /// share.
    pub fn failure_line(&self) -> Option<String> {
        self.reason.as_ref().map(|r| format!("failed: {r}"))
    }
}

/// The sidecar `merge-external` writes beside its merged output and `report`
/// reads beside each results file: `<stem>_comparators.json`.
pub fn sidecar_path(results: &Path) -> PathBuf {
    let stem = results
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    results.with_file_name(format!("{stem}_comparators.json"))
}

/// Write the outcomes (always, even when every comparator merged, so a stale
/// sidecar from an earlier run can never describe this one).
pub fn write_sidecar(results: &Path, outcomes: &[ComparatorOutcome]) -> Result<(), String> {
    let path = sidecar_path(results);
    let text = serde_json::to_string_pretty(outcomes).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Read the sidecar beside `results`, if one exists. A sidecar that is present
/// but unreadable is an error: it is this suite's own file, so damage to it is a
/// defect to name, not a comparator to skip.
pub fn read_sidecar(results: &Path) -> Result<Option<Vec<ComparatorOutcome>>, String> {
    let path = sidecar_path(results);
    if !path.exists() {
        return Ok(None);
    }
    let raw =
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|e| format!("parse {}: {e}", path.display()))
}

/// Load one external tool's per-row JSON and check the minimal shape every merge
/// reads: a JSON array whose elements are objects carrying a string `object`.
/// The error names the path and what was wrong, so it can stand as the
/// comparator's `failed:` reason unchanged.
pub fn read_tool_records(path: &Path) -> Result<Vec<serde_json::Value>, String> {
    let txt = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    if txt.trim().is_empty() {
        return Err(format!("{}: file is empty", path.display()));
    }
    let value: serde_json::Value =
        serde_json::from_str(&txt).map_err(|e| format!("parse {}: {e}", path.display()))?;
    let serde_json::Value::Array(records) = value else {
        return Err(format!(
            "{}: expected a JSON array of per-row records, found {}",
            path.display(),
            json_kind(&value)
        ));
    };
    for (i, rec) in records.iter().enumerate() {
        if !rec["object"].is_string() {
            return Err(format!(
                "{}: record {i} is not an object with a string `object` field",
                path.display()
            ));
        }
    }
    Ok(records)
}

fn json_kind(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn an_empty_file_is_refused_by_name() {
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), "assist.json", "");
        let e = read_tool_records(&p).unwrap_err();
        assert!(
            e.contains("file is empty") && e.contains("assist.json"),
            "{e}"
        );
        let p = write(d.path(), "ws.json", " \n\t ");
        assert!(read_tool_records(&p).unwrap_err().contains("file is empty"));
    }

    #[test]
    fn a_truncated_or_non_array_file_is_refused_with_the_parse_error() {
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), "t.json", r#"[{"object":"Eros","test_ty"#);
        let e = read_tool_records(&p).unwrap_err();
        assert!(e.starts_with(&format!("parse {}", p.display())), "{e}");
        let p = write(d.path(), "o.json", r#"{"object":"Eros"}"#);
        assert!(
            read_tool_records(&p)
                .unwrap_err()
                .contains("found an object")
        );
    }

    #[test]
    fn a_record_without_an_object_name_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let p = write(
            d.path(),
            "r.json",
            r#"[{"object":"Eros"},{"test_type":"x"}]"#,
        );
        let e = read_tool_records(&p).unwrap_err();
        assert!(e.contains("record 1"), "{e}");
        let p = write(d.path(), "n.json", r#"[1]"#);
        assert!(read_tool_records(&p).unwrap_err().contains("record 0"));
    }

    #[test]
    fn a_well_formed_file_loads_and_an_empty_array_is_allowed() {
        let d = tempfile::tempdir().unwrap();
        let p = write(
            d.path(),
            "ok.json",
            r#"[{"object":"Eros"},{"object":"Bennu"}]"#,
        );
        assert_eq!(read_tool_records(&p).unwrap().len(), 2);
        let p = write(d.path(), "e.json", "[]");
        assert!(read_tool_records(&p).unwrap().is_empty());
    }

    #[test]
    fn the_sidecar_round_trips_and_names_the_failed_reason() {
        let d = tempfile::tempdir().unwrap();
        let merged = d.path().join("validation_core_merged.json");
        assert_eq!(read_sidecar(&merged).unwrap(), None);
        let outcomes = vec![
            ComparatorOutcome::merged("assist", 12),
            ComparatorOutcome::failed("kete", "parse k.json: EOF while parsing a value"),
        ];
        write_sidecar(&merged, &outcomes).unwrap();
        assert_eq!(
            sidecar_path(&merged).file_name().unwrap(),
            "validation_core_merged_comparators.json"
        );
        let back = read_sidecar(&merged).unwrap().unwrap();
        assert_eq!(back, outcomes);
        assert_eq!(
            back[1].failure_line().unwrap(),
            "failed: parse k.json: EOF while parsing a value"
        );
        assert!(!back[0].is_failed() && back[1].is_failed());
    }

    #[test]
    fn a_damaged_sidecar_is_an_error_not_a_skip() {
        let d = tempfile::tempdir().unwrap();
        let merged = d.path().join("m.json");
        std::fs::write(sidecar_path(&merged), "not json").unwrap();
        assert!(read_sidecar(&merged).unwrap_err().contains("parse"));
    }

    #[test]
    fn the_sidecar_carries_every_merge_path_and_reads_an_older_one_without() {
        let d = tempfile::tempdir().unwrap();
        let merged = d.path().join("m.json");
        let outcomes = vec![ComparatorOutcome::folded(
            "findorb",
            Folded::with_paths(354, &[("od", 354), ("propagation", 800), ("ephemeris", 0)]),
        )];
        write_sidecar(&merged, &outcomes).unwrap();
        let back = read_sidecar(&merged).unwrap().unwrap();
        assert_eq!(back, outcomes);
        assert_eq!(back[0].rows, 354);
        assert_eq!(
            back[0].paths,
            vec![
                MergePath {
                    path: "od".into(),
                    rows: 354
                },
                MergePath {
                    path: "propagation".into(),
                    rows: 800
                },
                MergePath {
                    path: "ephemeris".into(),
                    rows: 0
                },
            ]
        );
        // A sidecar from before per-path counts still reads, with none.
        std::fs::write(
            sidecar_path(&merged),
            r#"[{"tool":"layup","status":"merged","rows":354}]"#,
        )
        .unwrap();
        let old = read_sidecar(&merged).unwrap().unwrap();
        assert_eq!(old, vec![ComparatorOutcome::merged("layup", 354)]);
        assert!(old[0].paths.is_empty());
    }
}
