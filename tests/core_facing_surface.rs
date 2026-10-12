//! Compile-time guard on the consumer-facing surface.
//!
//! `empyrean-core`'s `validate` channel consumes this crate as a LIBRARY, not
//! through the distribution wrapper, so every item it uses must stay reachable
//! with the `distribution` feature OFF. If one of them ever grows an
//! `empyrean::` dependency (directly, or by being moved behind the feature
//! gate), core's `validate` build would be forced to pull the `empyrean`
//! wrapper again — a crate published only AFTER core is released, which is the
//! circular dependency this split exists to break.
//!
//! The whole file is `cfg(not(feature = "distribution"))`: with the default
//! feature on it compiles to an empty test binary. With `--no-default-features`
//! it names each core-facing item below, so such a leak turns the
//! no-default-features build RED here, by name, instead of silently
//! reintroducing the dependency.
//!
//! The surface is taken from `empyrean-core`'s `validate-core` binary, which
//! uses `empyrean_validation::{compare::AU_KM, schema::ValidationResult,
//! schema::ValidationPlan, schema::test_types::*, schema::uncertainty_modes::*}`.

#![cfg(not(feature = "distribution"))]

use empyrean_validation::compare::AU_KM;
use empyrean_validation::schema::{
    ValidationPlan, ValidationResult, test_types as tt, uncertainty_modes as um,
};

#[test]
fn core_facing_metric_helper_is_reachable() {
    // `compare::AU_KM` — the AU→km scale factor validate-core multiplies by.
    // `black_box` keeps this a runtime read, not a const assertion.
    assert!(std::hint::black_box(AU_KM).is_finite());
}

#[test]
fn core_facing_schema_types_are_reachable() {
    // `ValidationResult` (built via `empty`, with its `notes` field and
    // `propagation_uncertainty`) and the `ValidationPlan` alias: the row shape
    // validate-core emits and the plan it replays.
    let mut r = ValidationResult::empty();
    r.notes = "guard".to_string();
    r.propagation_uncertainty = Some(um::NONE_DETECTION_ON.to_string());
    let plan: ValidationPlan = vec![r];
    assert_eq!(plan.len(), 1);
}

#[test]
fn core_facing_test_type_tags_are_reachable() {
    // Every test-type tag validate-core dispatches on.
    let _tags: [&str; 5] = [
        tt::PROPAGATION,
        tt::EPHEMERIS,
        tt::ORBIT_DETERMINATION,
        tt::ORBIT_DETERMINATION_TRANSPORT,
        tt::NON_GRAV_RECOVERY,
    ];
}

#[test]
fn core_facing_uncertainty_vocabulary_is_reachable() {
    // The composite method/arm vocabulary validate-core composes a row's
    // resolved_method from: the method and arm tables, the two decomposers, and
    // `compose` (delivered method composed with the row's arm).
    let _methods: [&str; 7] = um::METHODS;
    let _arms: [&str; 3] = um::ARMS;
    let tag = um::compose(um::FIRST_ORDER, um::DETECTION_ON);
    assert_eq!(um::method_of(&tag), Some(um::FIRST_ORDER));
    assert_eq!(um::arm_of(&tag), Some(um::DETECTION_ON));
    // Monte-Carlo sampling knobs the core channel reads back.
    let _samples: u32 = um::MONTE_CARLO_SAMPLE_COUNT;
    let _seed: u64 = um::MONTE_CARLO_SEED;
}
