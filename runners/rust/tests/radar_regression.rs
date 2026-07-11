//! End-to-end radar OD regression through the empyrean channel.
//!
//! Loads the suite's full multi-apparition Apophis fixture (9520 optical
//! 2004–2021 + 50 JPL `sb_radar` records) and runs `ctx.determine` — the same
//! wrapper → C-ABI → empyrean-core path the python / c / cli channels use.
//! In earlier engine releases, a cold IOD-seeded joint optical+radar fit on
//! this arc STALLED (`NotConverged`); the engine now seeds the joint fit from
//! an optical-only orbit so it converges. This test guards that fix end-to-end
//! across the distribution chain. Gated on the data tier being available.

use empyrean::{Context, ODConfig};

const APOPHIS_RADAR_PSV: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/psv-radar/Apophis.psv"
);

#[test]
fn apophis_optical_plus_radar_converges_through_channel() {
    let ctx = match Context::from_data_dir(None) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("SKIP: ephemeris data tier unavailable ({e})");
            return;
        }
    };

    let psv = std::fs::read_to_string(APOPHIS_RADAR_PSV).expect("read Apophis radar fixture");
    let observations = ctx
        .read_ades(&psv)
        .expect("parse optical+radar PSV through the channel");

    // The fixture must actually carry radar, or this would silently degrade to
    // an optical-only fit and not exercise the seeding fix.
    assert_eq!(
        observations.radar_len(),
        50,
        "expected 50 radar records to flow through ctx.read_ades"
    );
    assert!(
        observations.len() > 9000,
        "expected the dense multi-apparition optical arc"
    );

    // The fit must converge (it stalled before the optical-first seeding fix).
    let result = ctx
        .determine(&observations, None, &ODConfig::default())
        .expect("optical+radar determine must converge through the channel");
    assert!(
        result.converged,
        "Apophis multi-apparition optical+radar fit must converge"
    );
}
