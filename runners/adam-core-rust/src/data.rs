//! The registered data folder (`adam-core-rust/` of the third-party tool set)
//! and the settings record stamped on every row.
//!
//! The file names and their pinned versions are read from adam-core's own
//! manifest (`adam_core::kernel_data::KERNEL_SPECS` and
//! `DEFAULT_SPICE_KERNEL_IDS`), never typed here, and every file is opened by
//! the explicit path in the folder. adam-core's resolver (environment
//! override, a `python3` probe, `~/.cache`, then a fetch from PyPI) is never
//! called.

use adam_core::kernel_data::{DEFAULT_SPICE_KERNEL_IDS, KERNEL_SPECS, KernelSpec};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

/// The EFCC18 star-catalog bias table `CORE_OD_V1` declares by sha256.
pub const BIAS_TABLE: &str = "bias.dat";

/// adam-core's manifest entry for one resolver id.
pub fn spec(id: &str) -> &'static KernelSpec {
    KERNEL_SPECS
        .iter()
        .find(|s| s.id == id)
        .unwrap_or_else(|| panic!("adam_core_rs_kernel_data has no kernel id {id}"))
}

/// The PyPI package and version adam-core pins a file to, from the wheel URL in
/// its manifest (`.../naif_eop_predict-2024.8.28.1-py3-none-any.whl` gives
/// `naif-eop-predict 2024.8.28.1`).
pub fn pinned_package(spec: &KernelSpec) -> String {
    let wheel = spec.wheel_url.rsplit('/').next().unwrap_or(spec.wheel_url);
    let mut parts = wheel.splitn(3, '-');
    let name = parts.next().unwrap_or("").replace('_', "-");
    let version = parts.next().unwrap_or("unknown");
    format!("{name} {version}")
}

/// Every file the runner opens, in the order it opens them: the six default
/// SPICE kernels (setup_SPICE order), the observatory table, the ASSIST pair and
/// the bias table. Names are adam-core's own cache file names.
pub fn required_files() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = DEFAULT_SPICE_KERNEL_IDS
        .iter()
        .map(|id| spec(id).filename)
        .collect();
    for id in ["mpc_obscodes", "de440", "sb441_n16"] {
        let name = spec(id).filename;
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out.push(BIAS_TABLE);
    out
}

/// The registered folder, checked once: every required file present.
#[derive(Debug, Clone)]
pub struct DataDir {
    pub root: PathBuf,
}

impl DataDir {
    /// Refuse to start without the folder, naming every missing file; there is
    /// no fallback to adam-core's resolver or the PyPI data packages.
    pub fn open(root: &Path) -> Result<Self, String> {
        if !root.is_dir() {
            return Err(format!(
                "no adam-core-rust data directory at {}. Pass --data-dir <third-party>/adam-core-rust \
                 (make run-adam-core-rust stages it); the runner reads every file from it.",
                root.display()
            ));
        }
        let missing: Vec<String> = required_files()
            .into_iter()
            .map(|n| root.join(n))
            .filter(|p| !p.is_file())
            .map(|p| p.display().to_string())
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "registered file(s) missing: {}. Refusing to fall back to adam-core's resolver.",
                missing.join(", ")
            ));
        }
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    pub fn by_id(&self, id: &str) -> PathBuf {
        self.path(spec(id).filename)
    }
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut f =
        std::fs::File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}

/// `name package version (sha256 ...)` for one registered file.
pub fn describe(data: &DataDir, id: &str) -> Result<String, String> {
    let s = spec(id);
    Ok(format!(
        "{} {} (sha256 {})",
        s.filename,
        pinned_package(s),
        sha256_file(&data.path(s.filename))?
    ))
}

/// Every setting that shaped the run, as strings, read from the crates and the
/// files (nothing hand-typed that the tool can report). `facts` carries the
/// values only the caller knows (integrator, profile, sigma-fill table).
pub fn settings_record(
    data: &DataDir,
    facts: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, String> {
    let mut rec = facts.clone();
    rec.insert("planets_kernel".into(), describe(data, "de440")?);
    rec.insert("asteroid_kernel".into(), describe(data, "sb441_n16")?);
    rec.insert(
        "earth_orientation".into(),
        [
            "eop_high_prec",
            "eop_historical",
            "eop_predict",
            "earth_itrf93",
        ]
        .iter()
        .map(|id| describe(data, id))
        .collect::<Result<Vec<_>, _>>()?
        .join("; "),
    );
    rec.insert("leapseconds".into(), describe(data, "leapseconds")?);
    rec.insert("obscodes".into(), describe(data, "mpc_obscodes")?);
    rec.insert(
        "debias_table".into(),
        format!(
            "{BIAS_TABLE} (sha256 {}), bound to CORE_OD_V1's efcc18_catalog_bias_table slot",
            sha256_file(&data.path(BIAS_TABLE))?
        ),
    );
    rec.insert(
        "kernel_source".into(),
        "registered third-party set, adam-core-rust/ folder; every path passed explicitly \
         (furnsh, load_mpc_obscodes, AssistPropagator::from_paths, ResourceBindings); \
         adam-core's resolver is never called (ADAM_CORE_KERNEL_OFFLINE=1)"
            .into(),
    );
    Ok(rec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_required_files_come_from_adam_cores_manifest_in_setup_spice_order() {
        assert_eq!(
            required_files(),
            vec![
                "latest_leapseconds.tls",
                "de440.bsp",
                "earth_200101_990827_predict.bpc",
                "earth_620120_240827.bpc",
                "earth_latest_high_prec.bpc",
                "earth_assoc_itrf93.tf",
                "obscodes_extended.json",
                "sb441-n16.bsp",
                "bias.dat",
            ]
        );
    }

    #[test]
    fn the_pinned_package_is_read_from_the_wheel_url() {
        assert_eq!(
            pinned_package(spec("eop_high_prec")),
            "naif-eop-high-prec 2026.4.30"
        );
        assert_eq!(
            pinned_package(spec("mpc_obscodes")),
            "mpc-obscodes 2026.4.23"
        );
    }

    #[test]
    fn a_folder_missing_files_is_refused_naming_every_one() {
        let dir = std::env::temp_dir().join(format!("acr-data-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for n in required_files() {
            if n != "bias.dat" && n != "obscodes_extended.json" {
                std::fs::write(dir.join(n), b"x").unwrap();
            }
        }
        let err = DataDir::open(&dir).unwrap_err();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(
            err.contains("obscodes_extended.json") && err.contains("bias.dat"),
            "{err}"
        );
        assert!(!err.contains("de440.bsp"), "{err}");
        assert!(
            DataDir::open(Path::new("/nonexistent-acr"))
                .unwrap_err()
                .contains("--data-dir")
        );
    }
}
