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
/// is the semantic key, not the on-disk file name — the key→file-name
/// sanitize rule lives in the engine (and in the two existing readers) and is
/// deliberately not re-spelled here.
pub fn vectors_key(command: &str, epoch_mjd_tdb: f64) -> String {
    format!("{command}|{epoch_mjd_tdb:.8}")
}

/// The Horizons ephemeris lookup key (`<command>|<observer>|<time MJD TDB>`).
pub fn ephemeris_key(command: &str, observer: &str, time_mjd_tdb: f64) -> String {
    format!("{command}|{observer}|{time_mjd_tdb:.8}")
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
        "{object}: {} not in the pinned inputs snapshot (key `{key}` under {}): \
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
    fn keys_carry_the_command_and_eight_decimal_epoch() {
        assert_eq!(vectors_key("10", 60000.5), "10|60000.50000000");
        assert_eq!(
            ephemeris_key("DES=99942;", "W84", 60000.25),
            "DES=99942;|W84|60000.25000000"
        );
    }
}
