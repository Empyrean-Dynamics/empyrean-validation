//! Fatal errors for a sealed-snapshot input miss.
//!
//! The validation suite reads every JPL and MPC product from the pinned
//! inputs snapshot on disk and from nowhere else: CI points the JPL and MPC
//! hosts at the loopback address before running, so a query that would reach
//! the network fails at the socket. A client that misses the on-disk snapshot
//! is therefore reading a gap in the pinned inputs, not surviving a transient
//! outage, and must fail the leg loudly rather than drop the object (the old
//! `SKIP (...)` path silently shrank the reference). [`missing_input`] builds
//! the one message every consumer raises — the plan builder, the reference
//! runner and `merge-external` here, and the ASSIST runner's Python twin —
//! naming the object, the product, the lookup key and the directory searched,
//! so a re-mint can target exactly that gap.

use std::path::Path;

/// A JPL/MPC product the suite reads from the pinned snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputProduct {
    /// SBDB record (orbit, covariance, non-grav and physical parameters).
    Sbdb,
    /// Horizons state vectors (the IC, each per-`dt` vector, the Sun's state).
    HorizonsVectors,
    /// Horizons observer ephemeris (RA/Dec, light-time corrected).
    HorizonsEphemeris,
}

impl InputProduct {
    /// The product label carried in the error message.
    pub fn label(self) -> &'static str {
        match self {
            InputProduct::Sbdb => "SBDB",
            InputProduct::HorizonsVectors => "Horizons vectors",
            InputProduct::HorizonsEphemeris => "Horizons ephemeris",
        }
    }
}

/// The Horizons state-vector lookup key (`<command>|<epoch MJD TDB>`), named
/// in the error so a re-mint can reproduce exactly the missing response. This
/// is the semantic key, not the on-disk file name; [`cache_file_name`] maps a
/// key to the file the snapshot holds it in.
pub fn vectors_key(command: &str, epoch_mjd_tdb: f64) -> String {
    format!("{command}|{epoch_mjd_tdb:.8}")
}

/// The Horizons ephemeris request, named as the suite issued it
/// (`<command>|<observer>|<time MJD TDB>`). The engine's own key for this
/// request carries the time as the Horizons TLIST it sends (a Julian date),
/// so this names the request, not the engine's key text; a re-mint
/// reproduces it from these three values.
pub fn ephemeris_key(command: &str, observer: &str, time_mjd_tdb: f64) -> String {
    format!("{command}|{observer}|{time_mjd_tdb:.8} MJD TDB")
}

/// The snapshot file a cache key lives in: `<sanitized key>.json`.
///
/// The snapshot's `sbdb/` and `horizons/` files are the engine's own disk
/// cache: the mint's `plan --cache-dir` hands each key (an SBDB entry's key is
/// the catalog's `sbdb_query`) to the engine, whose disk cache names the file
/// by keeping every ASCII letter, digit, `_`, `-` and `.` and writing `_` for
/// every other character (`/`, space, `|`, `;`, `=`, `'`, ...). This is that
/// rule, for the reader here that opens a snapshot file by key instead of
/// asking the engine (`merge-external --jpl-sbdb-cache`). A reader with its
/// own rule reports an input missing that the snapshot holds: replacing only
/// spaces turned `C/2023 A3` into `C/2023_A3.json`, while the snapshot holds
/// `C_2023_A3.json`.
pub fn cache_file_name(key: &str) -> String {
    let stem: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{stem}.json")
}

/// Build the fatal message for a product the pinned snapshot does not carry.
///
/// `cause` is whatever the client reported (an empty result, a query error);
/// it is appended so the surface reason survives, but the leg fails either
/// way — the snapshot is sealed, so there is no fallback to the network.
pub fn missing_input(
    object: &str,
    product: InputProduct,
    key: &str,
    cache_dir: &Path,
    cause: &str,
) -> String {
    format!(
        "{object}: {} not in the pinned inputs snapshot (request `{key}` under {}): \
         {cause}. The validation inputs are sealed from JPL and the MPC; re-mint \
         the snapshot to add this input.",
        product.label(),
        cache_dir.display(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_input_names_object_product_key_and_dir() {
        let msg = missing_input(
            "Apophis",
            InputProduct::HorizonsVectors,
            "DES=99942;|60000.00000000",
            Path::new("fixtures/horizons"),
            "empty result",
        );
        // Every field the re-mint needs is on the one line.
        assert!(msg.contains("Apophis"), "names the object: {msg}");
        assert!(msg.contains("Horizons vectors"), "names the product: {msg}");
        assert!(
            msg.contains("DES=99942;|60000.00000000"),
            "names the key: {msg}"
        );
        assert!(msg.contains("fixtures/horizons"), "names the dir: {msg}");
        assert!(msg.contains("empty result"), "keeps the cause: {msg}");
        assert!(
            msg.contains("sealed"),
            "states the inputs are sealed: {msg}"
        );
    }

    #[test]
    fn product_labels_match_the_contract() {
        assert_eq!(InputProduct::Sbdb.label(), "SBDB");
        assert_eq!(InputProduct::HorizonsVectors.label(), "Horizons vectors");
        assert_eq!(
            InputProduct::HorizonsEphemeris.label(),
            "Horizons ephemeris"
        );
    }

    #[test]
    fn cache_file_name_is_the_engine_disk_cache_rule() {
        // File names the pinned snapshot carries, from the keys stored inside
        // those files.
        // A comet: the slash and the space both become `_`.
        assert_eq!(cache_file_name("C/2023 A3"), "C_2023_A3.json");
        // A provisional designation with a space.
        assert_eq!(cache_file_name("2024 YR4"), "2024_YR4.json");
        // Numbered bodies, bare and with a name.
        assert_eq!(cache_file_name("3200"), "3200.json");
        assert_eq!(cache_file_name("7 Iris"), "7_Iris.json");
        // A Horizons key: `;`, `=` and `|` become `_`; `.` is kept.
        assert_eq!(
            cache_file_name("DES=103P;CAP;NOFRAG|500|2451677.5000000000"),
            "DES_103P_CAP_NOFRAG_500_2451677.5000000000.json"
        );
        assert_eq!(
            cache_file_name("10;|500|2455725.5000000000"),
            "10__500_2455725.5000000000.json"
        );
        // An apostrophe becomes `_` like any other character outside the
        // kept set. (The catalog queries SBDB for 1I/'Oumuamua as `1I`, so
        // this name never reaches the SBDB cache; the rule still covers it.)
        assert_eq!(cache_file_name("1I/'Oumuamua"), "1I__Oumuamua.json");
        assert_eq!(cache_file_name("1I"), "1I.json");
    }

    /// Every catalog object's SBDB file is where the reader looks for it, and
    /// every SBDB file in the snapshot is named from its stored key by the
    /// same rule. Reads the fetched snapshot (`make fixtures`, pinned by
    /// fixtures/manifest.json), as the radar-fixture catalog test does.
    #[test]
    fn every_catalog_sbdb_query_names_a_snapshot_file() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/sbdb");
        for o in crate::catalog::all_objects() {
            let p = dir.join(cache_file_name(o.sbdb_query));
            assert!(
                p.is_file(),
                "{}: no SBDB snapshot file at {} for query `{}`",
                o.name,
                p.display(),
                o.sbdb_query
            );
        }
        let entries =
            std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
        let mut n = 0;
        for e in entries {
            let p = e.unwrap().path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
            let key = v["key"]
                .as_str()
                .unwrap_or_else(|| panic!("{}: no key", p.display()));
            assert_eq!(
                cache_file_name(key),
                p.file_name().unwrap().to_str().unwrap(),
                "key `{key}`"
            );
            n += 1;
        }
        assert_eq!(n, crate::catalog::all_objects().len());
    }

    #[test]
    fn keys_carry_the_command_and_eight_decimal_epoch() {
        assert_eq!(vectors_key("10", 60000.5), "10|60000.50000000");
        assert_eq!(
            ephemeris_key("DES=99942;", "W84", 60000.25),
            "DES=99942;|W84|60000.25000000 MJD TDB"
        );
    }
}
