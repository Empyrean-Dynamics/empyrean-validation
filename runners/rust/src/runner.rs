//! Validation runner: propagate orbits and compare to JPL Horizons.
//!
//! Channel: rust, going through the empyrean safe wrapper (same C ABI
//! that python / c / cli ride) — so Section 09 fidelity diffs reflect
//! only binding-translation drift, not propagator-path drift.
//!
//! # Uncertainty-method axis and cross-channel reproducibility
//!
//! Every (object, tier, dt) propagation and every (object, site, dt)
//! ephemeris row is swept under the engine's uncertainty methods — see
//! [`build_uncertainty_axes`]. Under the *same* method, these rows are
//! reproducible across channels as follows:
//!
//! - `none`, `first_order`, `second_order` and
//!   `sigma_point` are **deterministic**: identical inputs give
//!   identical outputs, so a same-method cross-channel diff is expected to
//!   be zero within the suite's `1e-10` fidelity band (SPICE-backed
//!   magnitudes stay band-compared, never bit-pinned).
//! - `monte_carlo` is **seeded** with the single suite-wide
//!   `MONTE_CARLO_SEED`. The engine owns the RNG and the sample order, so a
//!   fixed seed makes the sample moment bit-reproducible across runs and
//!   channels too — a bit check as well as a moment check.
//! - `gaussian_mixture` (close-approach objects only) is
//!   **deterministic**: the engine splits the object's own covariance with
//!   no RNG, and this runner compares the moment-collapsed covariance it
//!   returns.
//! - `auto` is bit-reproducible once the resolved rung agrees; the rung the
//!   engine resolved to is recorded per row in `resolved_method`, so the
//!   cross-channel compare keys on the outcome rather than the request.
//!
//! No row this runner emits is moment-only — every swept method above is
//! deterministic or seeded.
//!
//! ## Ephemeris sky covariance
//!
//! The ephemeris sky covariance has two sources: the harness projection of
//! the input covariance through the ephemeris Jacobian (the first-order sky
//! covariance) and the engine's delivered per-method covariance. On
//! `first_order` / `none` rows the **published** value stays
//! the harness projection — the pinned first-order golden — and the delivered
//! covariance is recorded only as a diagnostic (its RA/Dec σ difference, in
//! `notes`), because switching the published value on these rows is a golden
//! move that waits on a measured table from the first local run. On every
//! other method the delivered covariance is the published one (there is no
//! prior golden). See [`published_sky_covariance`].
//!
//! ## 0.11 per-method products, and what the 0.11 wrapper still does not carry
//!
//! Bound to the local `empyrean 0.11.0` distribution (an uncommitted path
//! override — see the README's "Local development overrides"), this runner
//! reads the per-method products off the wrapper rather than recomputing them:
//!
//! - **Propagation rows** carry the delivered packed joint (`cov_kind`, the
//!   wire discriminant via [`cov_kind_wire`]; `cov_joint_width`; `cov_tri`, the
//!   packed lower triangle), the per-orbit delivery outcome
//!   (`orbit_delivered` / `orbit_status` off `outcomes[0]` — the sole
//!   discriminator, never a row count), and, on a row the engine split, the
//!   six Gaussian-mixture tallies off the retained-component status table (see
//!   [`populate_mixture_tallies`]). The 6×6 the harness compares is still the
//!   wrapper's state covariance.
//! - **Ephemeris rows** carry the per-orbit outcome (`outcomes[0]`).
//! - **OD transport rows** (`orbit_determination_transport`) carry, per method,
//!   the post-fit transport of the fitted covariance, reading the same wrapper
//!   products (delivered packed joint, resolved kind, per-orbit outcome,
//!   mixture tallies, position moment view) through the same shared readback the
//!   propagation sweep uses ([`read_prop_products`]) — the delivered per-state
//!   packed joint, which the wrapper carries for every kind, so a sampling
//!   method (SigmaPoint / MonteCarlo), whose sample covariance lives on the
//!   propagated state, delivers its joint here exactly as the sweep's sampling
//!   rows do. The
//!   OD method axis rides this transport, not the fit (design ruling 9): the
//!   fit stays first-order (the OD-method gap below), but propagating the
//!   fitted covariance runs every method, so SecondOrder / SigmaPoint /
//!   MonteCarlo / GaussianMixture produce a joint on the OD seam here. The plan
//!   row carries no transport target, so the transport is taken at the FIT
//!   EPOCH (dt = 0) and named so in `notes`; the dispatch and delivered kinds
//!   are real while the cross-method numeric diagnostic is degenerate at a zero
//!   offset (design ruling 13). See [`od_transport_rows`].
//!
//! Three gaps at this pin, each left `None` or recorded by name rather than
//! back-filled — three products the 0.11 wrapper does **not** expose:
//!
//! - **Ephemeris resolved kind / packed joint.** The ephemeris seam
//!   (`EphemerisEntry` / `EphemerisResult`) flattens the delivered sky
//!   covariance to a bare 6×6 with no resolved-kind tag and no packed joint, so
//!   an ephemeris row's `resolved_method` / `cov_kind` / `cov_joint_*` cannot
//!   be read off the delivered row; back-filling them from the request would be
//!   a silent substitution, so they stay `None`. (The sky covariance itself is
//!   still published per the rule above.)
//! - **OD method axis.** `ODConfig` carries no `uncertainty_method` at this
//!   distribution revision (being added to the wrapper separately), so the OD
//!   fit runs method-free: no OD method axis, no per-fit packed joint. Every OD
//!   fit row records [`OD_METHOD_AXIS_NOT_PRODUCED`] in `notes` rather than a
//!   blank or a first-order default.
//! - **Pre-retention mixture tallies.** The wrapper exposes only the retained
//!   component status table, so the mixture tallies here count retained
//!   components; the core channel reads villeneuve's pre-aggregated tallies
//!   (which count sub-Gaussians before retention). A cross-channel compare
//!   attributes that difference to the wrapper surface, not physics.

use std::collections::HashMap;
use std::time::Instant;

use rayon::prelude::*;

use empyrean::{
    Context, CoordinateState, CovarianceKind, EphemerisConfig, EphemerisEntry, Epoch,
    ForceModelTier, Frame, ODConfig, Orbit, Origin, PropagationConfig, Representation,
    UncertaintyMethod,
};
use empyrean_validation::catalog::{
    DEFAULT_DT_DAYS, FORCE_MODEL_TIERS, ValidationObject, is_close_approach,
};
use empyrean_validation::compare;
use empyrean_validation::orbit_compare::compare_orbits;
use empyrean_validation::schema::{
    CapturedOrbit, OD_METHOD_AXIS_NOT_PRODUCED, OrbitComparison, ValidationResult, orbit_sources,
};

/// Runner config.
pub struct ValidateConfig {
    pub tiers: Vec<String>,
    pub n_timing_runs: usize,
    /// Attach a synthetic Cartesian covariance to every input orbit so
    /// the propagator dispatches to Jet1 (STM-bearing) integration —
    /// the production hot path. Set to false to drop covariance and
    /// exercise the f64-only path (useful for head-to-head timing vs
    /// external propagators that don't propagate uncertainty).
    pub attach_covariance: bool,
}

impl Default for ValidateConfig {
    fn default() -> Self {
        Self {
            tiers: FORCE_MODEL_TIERS.iter().map(|s| s.to_string()).collect(),
            n_timing_runs: 3,
            attach_covariance: true,
        }
    }
}

// ── Observation-sensitivity row order ───────────────────────────────
//
// The engine's observation Jacobian is `[6][n_params]` row-major, and
// its six output rows are the spherical topocentric observable in this
// order. Every layer between the engine and this runner (empyrean-core,
// the C ABI, the safe wrapper) marshals it through unreordered, so these
// indices are the contract on both sides of the FFI boundary.
//
// The angle rows (RA, Dec, and their rates) arrive in degrees per input
// unit; the range rows arrive in AU per input unit. Reading row 0 as RA
// therefore yields neither the right observable nor the right unit
// (empyrean-9666l: this runner published a range/RA covariance under the
// `emp_radec_cov_arcsec2` name until the pin test below was added).
//
// These were local copies while the pin sat at 0.9.0, whose wrapper did
// not export them. It does now, so they come from the crate: the row
// order is the FFI contract, and a local copy of a contract is a second
// place for it to drift. (`SENSITIVITY_ROW_RANGE` is imported by the pin
// test alone — it names the row that was being misread, and the
// projection itself never touches it.)
use empyrean::{SENSITIVITY_ROW_DEC, SENSITIVITY_ROW_RA};

/// Project the input-state covariance onto the sky plane through the
/// ephemeris Jacobian: \\(C_\text{radec} = J C_\text{in} J^\top\\) over
/// the RA and Dec rows, returned as a 2×2 in arcsec² with the RA
/// row/column scaled by cosδ so it matches `d_ra_arcsec`.
///
/// `jacobian` is the row-major `[6][n_params]` block as the wrapper
/// hands it over; only the first six columns are projected, because
/// `cin` is the 6×6 state covariance and the remaining columns (when a
/// wide chain carried non-gravitational or other solved-for parameters)
/// have no counterpart in it.
///
/// Returns `None` when the block is too short or too narrow to carry the
/// rows this needs, rather than indexing past its end.
fn project_sky_covariance(
    jacobian: &[f64],
    n_params: usize,
    cin: &[[f64; 6]; 6],
    dec_rad: f64,
) -> Option<[[f64; 2]; 2]> {
    if n_params < 6 || jacobian.len() < (SENSITIVITY_ROW_DEC + 1) * n_params {
        return None;
    }
    // Row stride is n_params, and each row is truncated to its 6 state
    // columns.
    let state_row = |r: usize| &jacobian[r * n_params..r * n_params + 6];
    let hra = state_row(SENSITIVITY_ROW_RA);
    let hdec = state_row(SENSITIVITY_ROW_DEC);

    let quad = |ha: &[f64], hb: &[f64]| {
        let mut s = 0.0;
        for i in 0..6 {
            for j in 0..6 {
                s += ha[i] * cin[i][j] * hb[j];
            }
        }
        s
    };
    let cosd = dec_rad.cos();
    let deg2_to_arcsec2 = 3600.0_f64 * 3600.0;
    let c_ra_ra = quad(hra, hra) * cosd * cosd * deg2_to_arcsec2;
    let c_ra_dec = quad(hra, hdec) * cosd * deg2_to_arcsec2;
    let c_dec_dec = quad(hdec, hdec) * deg2_to_arcsec2;
    Some([[c_ra_ra, c_ra_dec], [c_ra_dec, c_dec_dec]])
}

/// Append [`OD_METHOD_AXIS_NOT_PRODUCED`] to an OD fit row's base note so the
/// row records the missing method axis by name — never a blank cell, never a
/// silent first-order default. An empty base yields the marker alone.
fn with_od_method_note(base: String) -> String {
    if base.is_empty() {
        OD_METHOD_AXIS_NOT_PRODUCED.to_string()
    } else {
        format!("{base}; {OD_METHOD_AXIS_NOT_PRODUCED}")
    }
}

fn tier_from_str(s: &str) -> ForceModelTier {
    match s {
        "approximate" => ForceModelTier::Approximate,
        "basic" => ForceModelTier::Basic,
        // Standard is the v0.7.0 facade default. "Full" was the legacy
        // default but is excluded in v0.7.0; map it to Standard.
        _ => ForceModelTier::Standard,
    }
}

/// The resolved uncertainty method to report for a propagation row: the
/// tag of the covariance **kind the engine delivered** at the compared
/// epoch, for every row that carried a covariance.
///
/// On an `auto` row this names the rung auto resolved to. On an explicit
/// method it equals the requested method when the engine honoured the
/// request — the engine runs the requested rung end to end and refuses by
/// name rather than substitute (the no-silent-substitution invariant) — and
/// if a *different* kind was delivered under an explicit request this
/// reports the **delivered** kind, never the request, so the cross-channel
/// compare catches the silent substitution. `None` only when the row
/// carried no covariance (`none`).
///
/// `_requested_tag` is the method the row asked for. It is kept in the
/// signature so the call site and the tests pair a request with its
/// delivered kind, but the reported tag is purely a function of what was
/// delivered — the requested method already rides
/// [`ValidationResult::propagation_uncertainty`].
fn resolved_method_for(_requested_tag: &str, delivered: Option<CovarianceKind>) -> Option<String> {
    use empyrean_validation::schema::uncertainty_modes as um;
    let tag = match delivered? {
        CovarianceKind::Linear => um::FIRST_ORDER,
        CovarianceKind::SecondOrder => um::SECOND_ORDER,
        CovarianceKind::Mixture => um::GAUSSIAN_MIXTURE,
        CovarianceKind::MonteCarlo => um::MONTE_CARLO,
        CovarianceKind::SigmaPoint => um::SIGMA_POINT,
    };
    Some(tag.to_string())
}

/// The C-ABI wire discriminant for a delivered covariance kind — the `cov_kind`
/// the schema carries. Matches the `EMPYREAN_COVARIANCE_KIND_*` tags the core
/// channel also emits via `kind.wire_discriminant()` (linear 0, second-order 1,
/// mixture 3, monte-carlo 4, sigma-point 5), so the two channels record the
/// same discriminant for the same kind.
///
/// The 0.11 wrapper keeps its `CovarianceKind` ↔ wire-tag map `pub(crate)`, so
/// the FFI contract is restated here rather than read off the wrapper; it is
/// pinned against the tag constants by the
/// `cov_kind_wire_matches_the_c_abi_tags` test so the restated copy cannot
/// drift silently.
fn cov_kind_wire(kind: CovarianceKind) -> u8 {
    match kind {
        CovarianceKind::Linear => 0,
        CovarianceKind::SecondOrder => 1,
        CovarianceKind::Mixture => 3,
        CovarianceKind::MonteCarlo => 4,
        CovarianceKind::SigmaPoint => 5,
    }
}

/// The per-orbit delivery outcome for a row, read off the wrapper's
/// [`OrbitOutcome`](empyrean::OrbitOutcome) — the sole delivery discriminator,
/// never a row count. Returns `(orbit_delivered, orbit_status)`:
///
/// - `Delivered` with no withheld reason → `(true, "delivered")`.
/// - `Delivered` but a covariance was expected and could not be read back →
///   `(true, "cov_withheld:<reason>")`: the orbit delivered its state but the
///   engine published no covariance for it, carried by name rather than as a
///   blank cell.
/// - `Failed` → `(false, "failed:<variant>")` with the engine's
///   `EMPYREAN_PROPAGATE_FAILURE_*` classification named.
fn orbit_outcome_channel(
    outcome: &empyrean::OrbitOutcome,
    withheld_reason: Option<&str>,
) -> (bool, String) {
    use empyrean::OrbitOutcome;
    match outcome {
        OrbitOutcome::Delivered { .. } => match withheld_reason {
            Some(reason) => (true, format!("cov_withheld:{reason}")),
            None => (true, "delivered".to_string()),
        },
        OrbitOutcome::Failed { code, message } => (
            false,
            format!("failed:{}", propagate_failure_variant(*code, message)),
        ),
    }
}

/// Name an `EMPYREAN_PROPAGATE_FAILURE_*` classification code. The integer
/// codes are the C-ABI contract (header `EMPYREAN_PROPAGATE_FAILURE_*`); an
/// unrecognized code falls back to the engine's own message so no failure is
/// ever reported as a bare number.
fn propagate_failure_variant(code: i32, message: &str) -> String {
    match code {
        1 => "integration".to_string(),
        2 => "kepler_dt_backprop".to_string(),
        3 => "transform".to_string(),
        4 => "covariance_input".to_string(),
        5 => "sigma_point".to_string(),
        6 => "sampled_parameter".to_string(),
        7 => "ensemble_member".to_string(),
        8 => "output_assembly".to_string(),
        99 => "other".to_string(),
        other => format!("code_{other}({message})"),
    }
}

/// Fill a row's six Gaussian-mixture tallies from the wrapper's retained
/// mixture components — the per-component status table that is the 0.11
/// product. The counts are over the components the engine retained (one entry
/// per surviving sub-Gaussian), tallied by `ComponentStatus`: the
/// curvature-refused, unresolved, failed, and sky-linearization-refused
/// (status code 4, the 0.11 addition) counts, plus the retained component
/// count and the delivered mass.
///
/// An empty component set leaves every tally unset — never a fabricated zero —
/// matching the core channel's `populate_mixture_tallies` on an unsplit
/// (SecondOrder-delivered) row.
///
/// Cross-channel note: the core channel reads villeneuve's pre-aggregated
/// `MixtureComponents` tallies, which count sub-Gaussians *before* retention
/// (including those dropped before marshaling). The 0.11 wrapper exposes only
/// the retained-component status table, so these counts are over retained
/// components; a report comparing the two channels attributes any difference
/// to that surface, not to physics. See the module doc.
fn populate_mixture_tallies(
    components: &[empyrean::propagate::MixtureComponent],
    row: &mut ValidationResult,
) {
    use empyrean::propagate::ComponentStatus;
    if components.is_empty() {
        return;
    }
    let (mut n_failed, mut n_unresolved, mut n_curvature, mut n_sky) = (0u32, 0u32, 0u32, 0u32);
    let mut weight_delivered = 0.0;
    for c in components {
        weight_delivered += c.weight;
        match c.status {
            ComponentStatus::Resolved => {}
            ComponentStatus::CurvatureRefused { .. } => n_curvature += 1,
            ComponentStatus::Unresolved => n_unresolved += 1,
            ComponentStatus::Failed => n_failed += 1,
            ComponentStatus::SkyLinearizationRefused { .. } => n_sky += 1,
        }
    }
    row.mix_n_components_total = Some(components.len() as u32);
    row.mix_weight_delivered = Some(weight_delivered);
    row.mix_n_failed = Some(n_failed);
    row.mix_n_unresolved = Some(n_unresolved);
    row.mix_n_curvature_refused = Some(n_curvature);
    row.mix_n_sky_linearization_refused = Some(n_sky);
}

/// The 0.11 per-method propagation products read off a delivered
/// [`PropagationResult`](empyrean::propagate::PropagationResult) for its first
/// orbit — the single readback the propagation sweep and the post-fit OD
/// transport leg both call, so the two sites cannot drift.
///
/// The delivered covariance is the orbit's **per-state packed joint**
/// (`states[0].joint`) — the engine's delivered row, present for every kind:
/// the sensitivity-chain kinds (Linear / SecondOrder) and the SAMPLING kinds
/// (SigmaPoint / MonteCarlo), whose sample covariance lives on the propagated
/// state and is *not* reachable through the sensitivity-chain point accessor
/// [`covariance_at_cartesian`](empyrean::propagate::PropagationResult::covariance_at_cartesian)
/// (which returns a sensitivity-chain error for a sampled kind). The joint's
/// [`state_block`](empyrean::PackedJoint::state_block) is the 6×6 moment view and
/// its `kind` is the delivered kind, so the one joint carries both — reading the
/// chain accessor would add nothing it lacks. A covariance is `withheld` only
/// when the per-state joint is absent though one was expected (`attach_cov`),
/// and then it is carried by name with the engine's own reason (the point
/// accessor's error) rather than as a blank cell.
struct PropProducts {
    /// The delivered covariance kind (the joint's `kind`); `None` when no
    /// covariance was delivered.
    resolved_kind: Option<CovarianceKind>,
    /// The position 3×3 moment view (AU²), read off the joint's state block.
    emp_pos_cov: Option<[[f64; 3]; 3]>,
    /// The delivered packed joint.
    cov_joint: Option<empyrean::PackedJoint>,
    /// The per-orbit delivery outcome `(orbit_delivered, orbit_status)` off
    /// `outcomes[0]`.
    outcome_channel: Option<(bool, String)>,
    /// The retained Gaussian-mixture components (empty off a non-mixture row).
    mixture_components: Vec<empyrean::propagate::MixtureComponent>,
}

/// Read the delivered per-method products off `result`'s first orbit — see
/// [`PropProducts`] for which covariance surface this reads and why.
fn read_prop_products(
    result: &empyrean::propagate::PropagationResult,
    attach_cov: bool,
) -> PropProducts {
    let mut resolved_kind: Option<CovarianceKind> = None;
    let mut emp_pos_cov: Option<[[f64; 3]; 3]> = None;
    let mut cov_joint: Option<empyrean::PackedJoint> = None;
    // The delivered covariance IS the per-state packed joint, read for every
    // kind (sampling kinds included). The sensitivity-chain point accessor is
    // consulted only to name the engine's reason when the joint is absent.
    let mut withheld: Option<String> = None;
    match result.states.first().and_then(|s| s.joint.clone()) {
        Some(joint) => {
            resolved_kind = Some(joint.kind);
            let m = joint.state_block();
            emp_pos_cov = Some([
                [m[0][0], m[0][1], m[0][2]],
                [m[1][0], m[1][1], m[1][2]],
                [m[2][0], m[2][1], m[2][2]],
            ]);
            cov_joint = Some(joint);
        }
        None => {
            if attach_cov {
                withheld = Some(match result.covariance_at_cartesian(0, 0) {
                    Err(e) => e.to_string(),
                    Ok(_) => "covariance absent from the propagated state".to_string(),
                });
            }
        }
    }
    // Per-orbit delivery outcome — the sole discriminator, read off
    // `outcomes[0]`, never a row count.
    let outcome_channel = result
        .outcomes
        .first()
        .map(|oc| orbit_outcome_channel(oc, withheld.as_deref()));
    // Retained mixture components for this orbit (empty for every non-mixture
    // delivery, so the tally only fills on a row the engine actually split).
    let mixture_components = match result.mixtures.first() {
        Some(chain) => chain.components.iter().flatten().cloned().collect(),
        None => Vec::new(),
    };
    PropProducts {
        resolved_kind,
        emp_pos_cov,
        cov_joint,
        outcome_channel,
        mixture_components,
    }
}

/// Extract the (RA·cosδ, Dec) sky-plane 2×2 covariance in arcsec² from the
/// engine-delivered 6×6 ephemeris covariance.
///
/// The delivered matrix is ordered (rho, RA, Dec, vrho, vRA, vDec) in
/// (AU, deg), so the sky block is rows/columns 1 (RA) and 2 (Dec) read in
/// deg². The RA row and column are scaled by cosδ so the result matches
/// `d_ra_arcsec`, and deg² is converted to arcsec² — the same convention
/// [`project_sky_covariance`] applies to the harness projection, so a
/// first-order row reads the same quantity whether it comes from the
/// engine's delivered covariance or from the projection fallback.
fn delivered_sky_covariance(cov6: &[[f64; 6]; 6], dec_rad: f64) -> [[f64; 2]; 2] {
    const RA: usize = 1;
    const DEC: usize = 2;
    let cosd = dec_rad.cos();
    let deg2_to_arcsec2 = 3600.0_f64 * 3600.0;
    let c_ra_ra = cov6[RA][RA] * cosd * cosd * deg2_to_arcsec2;
    let c_ra_dec = cov6[RA][DEC] * cosd * deg2_to_arcsec2;
    let c_dec_dec = cov6[DEC][DEC] * deg2_to_arcsec2;
    [[c_ra_ra, c_ra_dec], [c_ra_dec, c_dec_dec]]
}

/// The RA/Dec σ difference (delivered − projection, arcsec) recorded as a
/// diagnostic note on a first-order ephemeris row whose published sky
/// covariance stays the harness projection. `None` when either covariance
/// is absent (there is nothing to compare).
fn sky_covariance_diagnostic(
    projected: Option<[[f64; 2]; 2]>,
    delivered: Option<[[f64; 2]; 2]>,
) -> Option<String> {
    let (p, d) = (projected?, delivered?);
    // Diagonal σ in arcsec (the 2×2 is a variance block in arcsec²).
    let sigma = |m: [[f64; 2]; 2], i: usize| m[i][i].max(0.0).sqrt();
    let d_sigma_ra = sigma(d, 0) - sigma(p, 0);
    let d_sigma_dec = sigma(d, 1) - sigma(p, 1);
    Some(format!(
        "sky_cov_diag(delivered-projection): d_sigma_ra_arcsec={d_sigma_ra:.6e} d_sigma_dec_arcsec={d_sigma_dec:.6e}"
    ))
}

/// Choose the ephemeris row's **published** sky covariance and its
/// diagnostic note from the harness projection and the engine-delivered
/// covariance.
///
/// On a first-order / f64 row the published value stays the harness
/// projection — the pinned first-order golden — and the engine's delivered
/// covariance is recorded only as a diagnostic (its RA/Dec σ against the
/// projection, via [`sky_covariance_diagnostic`]): switching the published
/// value on these rows is a golden move that waits on a measured table from
/// the first local run. On every other method the delivered covariance is
/// the published one (there is no prior golden), with the projection as the
/// fallback when the engine returned no sky covariance for the row.
fn published_sky_covariance(
    uncertainty_tag: &str,
    projected: Option<[[f64; 2]; 2]>,
    delivered: Option<[[f64; 2]; 2]>,
) -> (Option<[[f64; 2]; 2]>, Option<String>) {
    use empyrean_validation::schema::uncertainty_modes as um;
    // The first-order golden is kept on the covariance-free (`none`) and
    // `first_order` rows; keyed on the tag's method PREFIX so it holds under
    // any arm of those two methods.
    let method = um::method_of(uncertainty_tag);
    let first_order_pinned = method == Some(um::FIRST_ORDER) || method == Some(um::NONE);
    if first_order_pinned {
        (projected, sky_covariance_diagnostic(projected, delivered))
    } else {
        (delivered.or(projected), None)
    }
}

/// One requested uncertainty method in the per-row sweep.
struct UncertaintyAxis {
    tag: &'static str,
    attach: bool,
    method: UncertaintyMethod,
    /// Timing repetitions (best-of-N). The sampling methods cost
    /// ~100-120 propagations per call, so they measure once.
    timing_runs: usize,
}

/// The uncertainty-method axis each (object, tier, dt) propagation and each
/// (object, site, dt) ephemeris row is swept under.
///
/// With covariance attached (the production default) every object is swept
/// under the engine's six production uncertainty surfaces; with covariance
/// dropped only the covariance-free `f64` method runs (benchmark mode for a
/// head-to-head against external propagators that carry no uncertainty).
///
/// A close-approach object (`is_close_approach`) additionally carries the
/// `gaussian_mixture` arm: the engine splits the object's own
/// covariance into a mixture and returns the moment-collapsed covariance (the
/// mixture side table — component count, survivors, tallies — is a 0.11
/// product, so those schema fields stay `None` here). Non-close-approach
/// objects get no mixture row, matching the plan.
/// `CLOSE_APPROACH_OBJECTS` in `empyrean_validation::catalog` is the source
/// of truth for which objects those are.
///
/// `monte_carlo` is pinned to the suite-wide sample count and
/// seed (`MONTE_CARLO_SAMPLE_COUNT` / `MONTE_CARLO_SEED` in
/// `empyrean_validation::schema::uncertainty_modes`),
/// **not** [`UncertaintyMethod::monte_carlo`] (whose fixed seed is a
/// per-call convenience, not the validation-of-record seed): one suite-wide
/// seed makes a seeded Monte-Carlo row a cross-channel bit check as well as
/// a moment check.
fn build_uncertainty_axes(
    attach_covariance: bool,
    is_close_approach: bool,
) -> Vec<UncertaintyAxis> {
    use empyrean_validation::schema::uncertainty_modes as um;
    if !attach_covariance {
        return vec![UncertaintyAxis {
            tag: um::NONE,
            attach: false,
            method: UncertaintyMethod::FirstOrder,
            timing_runs: 0,
        }];
    }
    let mut axes = vec![
        UncertaintyAxis {
            tag: um::FIRST_ORDER,
            attach: true,
            method: UncertaintyMethod::FirstOrder,
            timing_runs: 0,
        },
        UncertaintyAxis {
            tag: um::NONE,
            attach: false,
            method: UncertaintyMethod::FirstOrder,
            timing_runs: 0,
        },
        UncertaintyAxis {
            tag: um::SECOND_ORDER,
            attach: true,
            method: UncertaintyMethod::SecondOrder,
            timing_runs: 0,
        },
        UncertaintyAxis {
            tag: um::AUTO,
            attach: true,
            method: UncertaintyMethod::auto(),
            timing_runs: 0,
        },
        // The full uncertainty ladder, for the report's performance strip:
        // sigma-point (120 samples at the wrapper defaults) and seeded
        // Monte Carlo at the suite-wide N and seed. Sampling methods cost
        // ~100-120 propagations per call — timing_runs = 1.
        UncertaintyAxis {
            tag: um::SIGMA_POINT,
            attach: true,
            method: UncertaintyMethod::sigma_point(),
            timing_runs: 1,
        },
        UncertaintyAxis {
            tag: um::MONTE_CARLO,
            attach: true,
            method: UncertaintyMethod::MonteCarlo {
                n_samples: um::MONTE_CARLO_SAMPLE_COUNT as usize,
                seed: Some(um::MONTE_CARLO_SEED),
            },
            timing_runs: 1,
        },
    ];
    if is_close_approach {
        // Close-approach objects only: the engine splits the object's own
        // covariance into a Gaussian mixture (no caller input) and returns
        // the moment-collapsed covariance. Component-splitting costs several
        // propagations per call — timing_runs = 1.
        axes.push(UncertaintyAxis {
            tag: um::GAUSSIAN_MIXTURE,
            attach: true,
            method: UncertaintyMethod::gaussian_mixture(),
            timing_runs: 1,
        });
    }
    axes
}

/// Run propagation + ephemeris validation against Horizons reference.
pub fn run_propagation_validation(
    ctx: &Context,
    objs: &[&ValidationObject],
    config: &ValidateConfig,
    horizons_cache_dir: &std::path::Path,
    sbdb_cache_dir: &std::path::Path,
    num_threads: Option<usize>,
) -> Vec<ValidationResult> {
    let timestamp = chrono::Utc::now().to_rfc3339();
    let channel = "rust".to_string();
    // Provenance: the exact empyrean engine (empyrean-core / villeneuve /
    // scott / nolan) this channel exercises. Stamped on every row so the
    // merged report records which code produced the numbers. `None` only if
    // the version FFI fails — same accessor the CapturedOrbit sidecar uses,
    // so a row and its sidecar always agree.
    let engine_version = empyrean::version_string().ok();

    eprintln!("Fetching initial conditions...");

    struct ObjData {
        name: String,
        population: String,
        notes: String,
        /// NAIF ids this object must not be perturbed by — its own, for the
        /// SB441-N16 self-perturbers. Derived from the catalog entry via
        /// `plan::self_perturber_naif_ids` so the prop/eph rows and the plan
        /// cannot disagree about the force model.
        excluded_naif: Vec<i32>,
        epoch: f64,
        ic_pos: [f64; 3],
        ic_vel: [f64; 3],
        a1: f64,
        a2: f64,
        a3: f64,
        ng_alpha: f64,
        ng_r0: f64,
        ng_m: f64,
        ng_n: f64,
        ng_k: f64,
        ng_dt: Option<f64>,
        dt_list: &'static [f64],
        horizons_vectors: HashMap<i64, ([f64; 3], [f64; 3])>,
        horizons_ephemeris: HashMap<(&'static str, i64), EphemerisEntry>,
    }

    let obs_codes = empyrean_validation::catalog::OBSERVER_CODES;
    let mut obj_data: Vec<ObjData> = Vec::new();

    for obj in objs {
        let sbdb = match empyrean::query_sbdb(&[obj.sbdb_query], Some(sbdb_cache_dir)) {
            Ok(b) if !b.orbits.is_empty() => b,
            Ok(_) => {
                eprintln!("  {}: SKIP (SBDB: empty result)", obj.name);
                continue;
            }
            Err(e) => {
                eprintln!("  {}: SKIP (SBDB: {e})", obj.name);
                continue;
            }
        };
        let epoch = match sbdb.orbits[0].state.epoch.mjd_tdb() {
            Ok(t) => t,
            Err(e) => {
                eprintln!("  {}: SKIP (SBDB epoch: {e})", obj.name);
                continue;
            }
        };

        let (hor_pos, hor_vel) = match empyrean::query_horizons_vectors(
            obj.horizons_command,
            epoch,
            Some(horizons_cache_dir),
        ) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("  {}: SKIP (Horizons IC: {e})", obj.name);
                continue;
            }
        };

        // Extract Marsden non-grav from SBDB and pass through with the
        // explicit g(r) parameters so the C ABI builds the correct
        // model. SBDB defaults to inverse_square for asteroids and
        // water-ice for comets. `dt` is the SBDB time-delay (days)
        // applied to g(r) — non-zero for Jupiter-family comets and
        // some interstellar objects (67P=+45.7d, 2I/Borisov=−65.1d).
        let (a1, a2, a3, ng_alpha, ng_r0, ng_m, ng_n, ng_k, ng_dt) = {
            let o = &sbdb.orbits[0];
            // The wrapper carries the Marsden g(r) parameters as flat
            // fields with an all-zero sentinel for the inverse-square
            // default; record the canonical inverse-square constants
            // (α=1, r0=1, m=2, n=0, k=0) in that case.
            let has_g = o.ng_alpha != 0.0
                || o.ng_r0 != 0.0
                || o.ng_m != 0.0
                || o.ng_n != 0.0
                || o.ng_k != 0.0;
            let (ga, gr0, gm, gn, gk) = if has_g {
                (o.ng_alpha, o.ng_r0, o.ng_m, o.ng_n, o.ng_k)
            } else {
                (1.0, 1.0, 2.0, 0.0, 0.0)
            };
            (o.a1, o.a2, o.a3, ga, gr0, gm, gn, gk, o.non_grav_dt)
        };

        eprintln!(
            "  {}: epoch={:.1} MJD TDB (Horizons IC, a1={:.2e}, dt={:?})",
            obj.name, epoch, a1, ng_dt
        );

        let dt_list = obj.dt_days.unwrap_or(DEFAULT_DT_DAYS);
        let mut horizons_vectors: HashMap<i64, ([f64; 3], [f64; 3])> = HashMap::new();
        for &dt in dt_list {
            let target = epoch + dt;
            match empyrean::query_horizons_vectors(
                obj.horizons_command,
                target,
                Some(horizons_cache_dir),
            ) {
                Ok(h) => {
                    horizons_vectors.insert(dt as i64, h);
                }
                Err(e) => {
                    eprintln!("  {}: dt={dt:+.0}d Horizons SKIP ({e})", obj.name);
                }
            }
        }

        // Ephemeris (RA/Dec) is observer-dependent — fetch from every site so
        // the report can average the sky-plane separation over the sites.
        let mut horizons_ephemeris: HashMap<(&'static str, i64), EphemerisEntry> = HashMap::new();
        for &obs_code in obs_codes {
            for &dt in dt_list {
                let target = epoch + dt;
                match empyrean::query_horizons(
                    &[obj.horizons_command],
                    obs_code,
                    &[target],
                    Some(horizons_cache_dir),
                ) {
                    Ok(r) if !r.is_empty() => {
                        horizons_ephemeris
                            .insert((obs_code, dt as i64), r.into_iter().next().unwrap());
                    }
                    Ok(_) => {
                        eprintln!(
                            "  {}: {obs_code} dt={dt:+.0}d ephemeris SKIP (empty)",
                            obj.name
                        );
                    }
                    Err(e) => {
                        eprintln!(
                            "  {}: {obs_code} dt={dt:+.0}d ephemeris SKIP ({e})",
                            obj.name
                        );
                    }
                }
            }
        }

        obj_data.push(ObjData {
            name: obj.name.to_string(),
            population: obj.population.to_string(),
            notes: obj.notes.to_string(),
            excluded_naif: empyrean_validation::plan::self_perturber_naif_ids(obj),
            epoch,
            ic_pos: hor_pos,
            ic_vel: hor_vel,
            a1,
            a2,
            a3,
            ng_alpha,
            ng_r0,
            ng_m,
            ng_n,
            ng_k,
            ng_dt,
            dt_list,
            horizons_vectors,
            horizons_ephemeris,
        });
    }

    eprintln!();
    eprintln!(
        "Running propagation + ephemeris validation ({} objects, {} threads)...",
        obj_data.len(),
        num_threads.unwrap_or(0),
    );

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(num_threads.unwrap_or(0))
        .build()
        .expect("failed to build thread pool");

    // Uncertainty-method axis. When config.attach_covariance is true (the
    // default) every (object, tier, dt) row is propagated under each of the
    // engine's production uncertainty surfaces so the report can compare
    // timing + accuracy across methods head-to-head; the ephemeris seam
    // sweeps the same axis. When attach_covariance is false only the
    // covariance-free f64 row is emitted (benchmark mode for a head-to-head
    // against external propagators that carry no uncertainty). The axis and
    // its cross-channel reproducibility are documented on
    // `build_uncertainty_axes` and in the module doc.
    //
    // Built per object: a close-approach object additionally carries the
    // Gaussian-mixture arm (see `build_uncertainty_axes`).
    let all_results: Vec<Vec<ValidationResult>> = pool.install(|| {
        obj_data
            .par_iter()
            .map(|data| {
                use empyrean_validation::schema::uncertainty_modes as um;
                let mut results: Vec<ValidationResult> = Vec::new();
                let modes = build_uncertainty_axes(
                    config.attach_covariance,
                    is_close_approach(&data.name),
                );

                for axis in &modes {
                // Synthetic typical-NEO 6×6 Cartesian covariance:
                //   1 km position σ, 1 mm/s velocity σ (uncorrelated).
                //
                // This is what triggers empyrean's Jet1 / Auto STM
                // dispatch in the propagator — empyrean is uncertainty-
                // first, so any orbit with a covariance attached
                // propagates STM by default. Numbers are placeholder
                // physical scales; covariance-accuracy validation
                // against Monte Carlo is a separate (future) test.
                let covariance = if axis.attach {
                    let pos_var_au = (1.0 / 149_597_870.700_f64).powi(2);
                    let vel_var_au_d = (1e-6 / 149_597_870.700_f64 * 86_400.0).powi(2);
                    let mut c = [[0.0_f64; 6]; 6];
                    c[0][0] = pos_var_au;
                    c[1][1] = pos_var_au;
                    c[2][2] = pos_var_au;
                    c[3][3] = vel_var_au_d;
                    c[4][4] = vel_var_au_d;
                    c[5][5] = vel_var_au_d;
                    Some(c)
                } else {
                    None
                };
                // The row's composite tag: this axis's method under the
                // detection_on arm (the rust runner measures detection on only;
                // the detection_off / tolerance arms are derived by `arm-plan`).
                let uncertainty_tag = um::compose(axis.tag, um::DETECTION_ON);
                let state = CoordinateState {
                    epoch: Epoch::from_mjd_tdb(data.epoch),
                    elements: [
                        data.ic_pos[0],
                        data.ic_pos[1],
                        data.ic_pos[2],
                        data.ic_vel[0],
                        data.ic_vel[1],
                        data.ic_vel[2],
                    ],
                    covariance,
                    representation: Representation::Cartesian,
                    frame: Frame::ICRF,
                    origin: Origin::SSB,
                };
                let mut orbit = Orbit::new(state);
                if data.a1 != 0.0 || data.a2 != 0.0 || data.a3 != 0.0 {
                    orbit = orbit
                        .with_nongrav(data.a1, data.a2, data.a3)
                        .with_g_function(
                            data.ng_alpha,
                            data.ng_r0,
                            data.ng_m,
                            data.ng_n,
                            data.ng_k,
                        )
                        .with_non_grav_dt(data.ng_dt);
                }

                for tier_str in &config.tiers {
                    let tier = tier_from_str(tier_str);

                    for &dt in data.dt_list {
                        let Some(&hor) = data.horizons_vectors.get(&(dt as i64)) else {
                            continue;
                        };
                        let target = Epoch::from_mjd_tdb(data.epoch + dt);

                        // Take the body out of its own perturber set. This
                        // also takes the engine's ephemeris-overlap
                        // short-circuit out of play, so the row measures a
                        // real integration rather than a resample of the
                        // body's own SPK — see `naif_to_origins`.
                        let excluded = match naif_to_origins(&data.excluded_naif) {
                            Ok(o) => o,
                            Err(e) => {
                                eprintln!("  {}: {tier_str} SKIP ({e})", data.name);
                                continue;
                            }
                        };
                        let prop_config = PropagationConfig {
                            force_model: tier,
                            excluded_perturbers: excluded,
                            uncertainty_method: axis.method.clone(),
                            frame: Frame::ICRF,
                            ..PropagationConfig::default()
                        };

                        let mut emp_times = Vec::new();
                        let mut emp_pos_cov: Option<[[f64; 3]; 3]> = None;
                        // The covariance kind the engine resolved to at the
                        // compared epoch, read off the delivered per-state
                        // joint. Only `auto` turns this into a
                        // `resolved_method`; for an explicit method the
                        // request is the outcome (`resolved_method_for`).
                        let mut resolved_kind: Option<CovarianceKind> = None;
                        // The 0.11 per-method products, read off the wrapper
                        // (never recomputed): the delivered packed joint behind
                        // the collapsed moment views, the per-orbit delivery
                        // outcome, and the retained Gaussian-mixture components.
                        let mut cov_joint: Option<empyrean::PackedJoint> = None;
                        let mut outcome_channel: Option<(bool, String)> = None;
                        let mut mixture_components: Vec<empyrean::propagate::MixtureComponent> =
                            Vec::new();
                        let mut emp_state: Option<[f64; 3]> = None;
                        let mut failed = false;

                        let runs = if axis.timing_runs > 0 {
                            axis.timing_runs
                        } else {
                            config.n_timing_runs
                        };
                        for _ in 0..runs {
                            let t0 = Instant::now();
                            match ctx.propagate(&[orbit.clone()], &[target], &prop_config) {
                                Ok(result) => {
                                    let ms = t0.elapsed().as_secs_f64() * 1000.0;
                                    emp_times.push(ms);
                                    if !result.states.is_empty() {
                                        emp_state = Some(result.states[0].position);
                                        // The 0.11 per-method products — the
                                        // delivered per-state packed joint (the
                                        // engine's delivered row, for every kind
                                        // including the sampling kinds), the
                                        // resolved kind, the per-orbit outcome,
                                        // and the retained mixture components —
                                        // read off the result by the shared
                                        // readback the OD transport leg also
                                        // uses. A covariance expected but absent
                                        // from the state is named on the outcome.
                                        let products = read_prop_products(&result, axis.attach);
                                        resolved_kind = products.resolved_kind;
                                        emp_pos_cov = products.emp_pos_cov;
                                        cov_joint = products.cov_joint;
                                        outcome_channel = products.outcome_channel;
                                        mixture_components = products.mixture_components;
                                    } else {
                                        eprintln!(
                                            "  {} {tier_str} dt={dt:+.0}d {} empyrean Ok but states.len()=0 (likely AGM mixture-only return; skipping row)",
                                            data.name, axis.tag,
                                        );
                                    }
                                }
                                Err(e) => {
                                    eprintln!(
                                        "  {} {tier_str} dt={dt:+.0}d {} empyrean FAIL ({e})",
                                        data.name, axis.tag,
                                    );
                                    failed = true;
                                    break;
                                }
                            }
                        }
                        if failed || emp_state.is_none() {
                            continue;
                        }
                        let emp_pos = emp_state.unwrap();
                        let emp_ms = emp_times.iter().copied().fold(f64::INFINITY, f64::min);
                        let emp_vs_hor = compare::position_error_km(&emp_pos, &hor.0);

                        eprintln!(
                            "  {} {:>12} dt={:>+6.0}d  e-h={}  {:.1}ms",
                            data.name,
                            tier_str,
                            dt,
                            compare::fmt_km(emp_vs_hor),
                            emp_ms,
                        );

                        let mut prop_row = ValidationResult {
                            object: data.name.clone(),
                            population: data.population.clone(),
                            epoch_mjd_tdb: data.epoch,
                            dt_days: dt,
                            t_mjd_tdb: data.epoch + dt,
                            force_model: tier_str.clone(),
                            test_type: "propagation".to_string(),
                            channel: channel.clone(),
                            observer: None,
                            emp_vs_horizons_km: Some(emp_vs_hor),
                            emp_pos_au: Some(emp_pos),
                            emp_pos_cov_au2: emp_pos_cov,
                            emp_stm: None,
                            emp_time_ms: Some(emp_ms),
                            separation_arcsec: None,
                            d_ra_arcsec: None,
                            d_dec_arcsec: None,
                            emp_radec_cov_arcsec2: None,
                            d_rho_km: None,
                            d_light_time_s: None,
                            ic_pos_au: Some(data.ic_pos),
                            ic_vel_au_d: Some(data.ic_vel),
                            ic_a1: Some(data.a1),
                            ic_a2: Some(data.a2),
                            ic_a3: Some(data.a3),
                            ic_g_alpha: Some(data.ng_alpha),
                            ic_g_r0: Some(data.ng_r0),
                            ic_g_m: Some(data.ng_m),
                            ic_g_n: Some(data.ng_n),
                            ic_g_k: Some(data.ng_k),
                            ic_non_grav_dt: data.ng_dt,
                            ref_pos_au: Some(hor.0),
                            ref_vel_au_d: Some(hor.1),
                            ref_ra_rad: None,
                            ref_dec_rad: None,
                            ref_rho_au: None,
                            ref_light_time_d: None,
                            ref_sun_pos_au: None,
                            ref_sun_vel_au_d: None,
                            ref_od_rms_normalized: None,
                            ref_od_reduced_chi2: None,
                            ref_od_n_obs_used: None,
                            ref_od_n_del_obs_used: None,
                            ref_od_n_dop_obs_used: None,
                            ref_od_data_arc_days: None,
                            ref_od_condition_code: None,
                            ref_od_soln_date: None,
                            ref_od_pe_used: None,
                            ref_od_sb_used: None,
                            n_obs_used: None,
                            od_iterations: None,
                            od_converged: None,
                            od_rms_ra_arcsec: None,
                            od_rms_dec_arcsec: None,
                            od_rms_combined_arcsec: None,
                            od_chi2: None,
                            od_reduced_chi2: None,
                            od_a1: None,
                            od_a2: None,
                            od_a3: None,
                            od_a1_sigma: None,
                            od_a2_sigma: None,
                            od_a3_sigma: None,
                            od_dt: None,
                            od_dt_sigma: None,
                            od_h: None,
                            od_h_sigma: None,
                            od_g1: None,
                            od_g1_sigma: None,
                            od_g2: None,
                            od_g2_sigma: None,
                            od_photometry_model: None,
                            od_photometry_reduced_chi2: None,
                            od_thrust_dv_m_per_s: Vec::new(),
                            od_thrust_dv_sigma_m_per_s: Vec::new(),
                            excluded_perturbers_naif: data.excluded_naif.clone(),
                            // Prop / eph rows run no fit, so there is no
                            // disposition to report. `None` reads as "not
                            // applicable", never as "fixed".
                            od_disposition_marsden: None,
                            od_disposition_dt: None,
                            od_disposition_amrat: None,
                            od_solve_for_used: None,
                            od_joint_covariance_width: None,
                            od_disposition_thrust: Vec::new(),
                            od_warnings: Vec::new(),
                            propagation_uncertainty: Some(uncertainty_tag.to_string()),
                            // Per-method uncertainty output, read off the 0.11
                            // wrapper. `resolved_method` is the tag of the
                            // covariance kind the engine delivered on every
                            // covariance-bearing row (the rung an `auto` row
                            // chose; the honoured request on an explicit row;
                            // the delivered kind, never the request, on a silent
                            // substitution the compare catches); `None` on an
                            // `none` row. `cov_kind` / `cov_joint_width` /
                            // `cov_tri` are the delivered packed joint; the
                            // outcome channel is `outcomes[0]`. The mixture
                            // side-table tallies are populated below when the
                            // engine delivered a Mixture.
                            resolved_method: resolved_method_for(axis.tag, resolved_kind)
                                .map(|m| um::compose(&m, um::DETECTION_ON)),
                            cov_kind: cov_joint.as_ref().map(|j| cov_kind_wire(j.kind)),
                            cov_joint_width: cov_joint.as_ref().map(|j| j.width as u32),
                            cov_tri: cov_joint.as_ref().map(|j| j.tri.clone()),
                            orbit_delivered: outcome_channel.as_ref().map(|(d, _)| *d),
                            orbit_status: outcome_channel.as_ref().map(|(_, s)| s.clone()),
                            mix_n_components_total: None,
                            mix_weight_delivered: None,
                            mix_n_failed: None,
                            mix_n_unresolved: None,
                            mix_n_curvature_refused: None,
                            mix_n_sky_linearization_refused: None,
                            assist_vs_horizons_km: None,
                            emp_vs_assist_km: None,
                            assist_time_ms: None,
                            assist_call_time_ms: None,
                            assist_stm: None,
                            fd_stm: None,
                            var_stm_fixed: None,
                            speed_ratio: None,
                            findorb_rms_residual: None,
                            findorb_n_obs_used: None,
                            findorb_n_obs_rejected: None,
                            findorb_vs_horizons_km: None,
                            emp_vs_findorb_km: None,
                            findorb_separation_arcsec: None,
                            findorb_d_ra_arcsec: None,
                            findorb_d_dec_arcsec: None,
                            findorb_d_rho_km: None,
                            findorb_time_ms: None,
                            kete_time_ms: None,
                            jorbit_time_ms: None,
                            // External-reference fields populated by merge-external:
                            // OpenOrb (prop + ephemeris) and OrbFit (OD).
                            oorb_vs_horizons_km: None,
                            emp_vs_oorb_km: None,
                            oorb_time_ms: None,
                            oorb_separation_arcsec: None,
                            oorb_d_ra_arcsec: None,
                            oorb_d_dec_arcsec: None,
                            oorb_d_rho_km: None,
                            orbfit_rms_arcsec: None,
                            orbfit_n_obs_used: None,
                            orbfit_n_obs_rejected: None,
                            orbfit_time_ms: None,
                            orbfit_error: None,
                            layup_chi2: None,
                            layup_reduced_chi2: None,
                            layup_n_obs_used: None,
                            layup_converged: None,
                            layup_time_ms: None,
                            // GRSS is an external comparator; the reference channel never fills these.
                            grss_vs_horizons_km: None,
                            emp_vs_grss_km: None,
                            grss_time_ms: None,
                            grss_separation_arcsec: None,
                            grss_d_ra_arcsec: None,
                            grss_d_dec_arcsec: None,
                            grss_d_rho_km: None,
                            grss_rms_arcsec: None,
                            grss_chi2: None,
                            grss_reduced_chi2: None,
                            grss_converged: None,
                            grss_n_obs_used: None,
                            grss_n_obs_rejected: None,
                            grss_n_obs_unsupported: None,
                            grss_rms_delay_us: None,
                            grss_rms_doppler_hz: None,
                            grss_n_delay_used: None,
                            grss_n_doppler_used: None,
                            grss_sigma_pos_km: None,
                            grss_model_note: None,
                            grss_error: None,
                            source_version: engine_version.clone(),
                            timestamp: timestamp.clone(),
                            notes: data.notes.clone(),
                        };
                        // Gaussian-mixture side table: populate the six tallies
                        // only on a row the engine actually split (delivered a
                        // Mixture); an unsplit row (SecondOrder collapse) keeps
                        // every tally `None`, matching the core channel.
                        if resolved_kind == Some(CovarianceKind::Mixture) {
                            populate_mixture_tallies(&mixture_components, &mut prop_row);
                        }
                        results.push(prop_row);
                    }
                }

                // Ephemeris tests (Standard tier) — one row per observing site,
                // swept under the full method axis (ruling 1: every method on
                // every ephemeris row). The requested method is threaded into
                // the ephemeris propagation config below, and the engine's
                // per-method sky covariance is read off the delivered entry.
                for &obs_code in obs_codes {
                for &dt in data.dt_list {
                    let Some(hor) = data.horizons_ephemeris.get(&(obs_code, dt as i64)) else {
                        continue;
                    };
                    // Reference entries carry degrees over the wrapper
                    // surface; comparisons below are in radians.
                    let hor_ra_rad = hor.ra_deg.to_radians();
                    let hor_dec_rad = hor.dec_deg.to_radians();
                    let hor_light_time_d =
                        (!hor.light_time_days.is_nan()).then_some(hor.light_time_days);
                    let target = Epoch::from_mjd_tdb(data.epoch + dt);

                    // (ICRF, SSB) is the construction basis — observers come
                    // back exactly as built, untransformed, which is what
                    // ephemeris generation requires and what this call got
                    // implicitly before the basis became explicit.
                    let observers = match ctx.get_observers(
                        &[obs_code],
                        &[target],
                        Frame::ICRF,
                        Origin::SSB,
                    ) {
                        Ok(o) => o,
                        Err(e) => {
                            eprintln!("  {} dt={dt:+.0}d SKIP (observer: {e})", data.name);
                            continue;
                        }
                    };
                    if observers.is_empty() {
                        continue;
                    }

                    let mut eph_config =
                        EphemerisConfig::with_force_model(ForceModelTier::Standard);
                    // Live as of the 0.10 ABI. This was inert through 0.9.x —
                    // `empyrean-c::ephemeris::build_ephemeris_config_from_c`
                    // read only {force_model, frame, uncertainty_method} off
                    // the embedded propagation config and dropped the rest —
                    // and it now carries `excluded_perturbers` through
                    // field-by-field with no `..Default` tail.
                    //
                    // So the ephemeris rows for the Self-Perturber population
                    // move at this release: the exclusion they always recorded
                    // now actually takes effect, and the body stops both
                    // pulling on itself and short-circuiting the integration
                    // into a resample of its own SPK.
                    eph_config.propagation.excluded_perturbers =
                        match naif_to_origins(&data.excluded_naif) {
                            Ok(o) => o,
                            Err(e) => {
                                eprintln!("  {} dt={dt:+.0}d SKIP ({e})", data.name);
                                continue;
                            }
                        };
                    // Thread the row's requested method, exactly as the
                    // propagation sweep does — the engine delivers the
                    // ephemeris (and its sky covariance) under this method.
                    eph_config.propagation.uncertainty_method = axis.method.clone();
                    // Time the ephemeris-generation call per row with the same
                    // stopwatch boundary as the core runner's `replay_ephemeris`
                    // (and the CLI channel): the stopwatch wraps exactly the
                    // `generate_ephemeris` call for this one object / observing
                    // site / epoch, with orbit, observer, and config construction
                    // outside the timed region. Best-of the runner's
                    // `n_timing_runs` like the propagation rows above (core times a
                    // single call; the boundary — what the stopwatch wraps — is
                    // identical). `generate_ephemeris` is deterministic run to run,
                    // so the delivered sky position is bit-identical to the
                    // pre-timing single call.
                    let eph_runs = if axis.timing_runs > 0 {
                        axis.timing_runs
                    } else {
                        config.n_timing_runs
                    };
                    let mut eph_times = Vec::new();
                    let mut eph_out = None;
                    let mut eph_failed = false;
                    for _ in 0..eph_runs {
                        let t0 = Instant::now();
                        match ctx.generate_ephemeris(
                            std::slice::from_ref(&orbit),
                            &observers,
                            &eph_config,
                        ) {
                            Ok(eph) => {
                                eph_times.push(t0.elapsed().as_secs_f64() * 1000.0);
                                eph_out = Some(eph);
                            }
                            Err(e) => {
                                eprintln!("  {} dt={dt:+.0}d FAIL ({e})", data.name);
                                eph_failed = true;
                                break;
                            }
                        }
                    }
                    if eph_failed {
                        continue;
                    }
                    let emp_eph_ms = eph_times.iter().copied().fold(f64::INFINITY, f64::min);
                    match eph_out {
                        Some(eph) => {
                            let Some(entry) = eph.entries.first() else {
                                continue;
                            };
                            // Per-orbit delivery outcome for the ephemeris row,
                            // read off `outcomes[0]` (never a row count). The
                            // ephemeris seam carries no withheld-covariance
                            // channel, so the delivered row is simply
                            // "delivered" or the engine's failure variant.
                            let eph_outcome =
                                eph.outcomes.first().map(|oc| orbit_outcome_channel(oc, None));
                            // Wrapper returns degrees; compare in radians.
                            let emp_ra_rad = entry.ra_deg.to_radians();
                            let emp_dec_rad = entry.dec_deg.to_radians();

                            let sep = compare::angular_separation_arcsec(
                                emp_ra_rad,
                                emp_dec_rad,
                                hor_ra_rad,
                                hor_dec_rad,
                            );
                            // Wrap the RA difference to [-π, π] so an object
                            // near RA = 0 / 2π doesn't produce a spurious ~2π
                            // residual. (Dec needs no wrap; separation below is
                            // great-circle and already wrap-safe.)
                            let mut d_ra_wrapped =
                                (emp_ra_rad - hor_ra_rad).rem_euclid(std::f64::consts::TAU);
                            if d_ra_wrapped > std::f64::consts::PI {
                                d_ra_wrapped -= std::f64::consts::TAU;
                            }
                            let d_ra = d_ra_wrapped * emp_dec_rad.cos();
                            let d_dec = emp_dec_rad - hor_dec_rad;
                            let d_ra_arcsec = d_ra.to_degrees() * 3600.0;
                            let d_dec_arcsec = d_dec.to_degrees() * 3600.0;

                            // Sky-plane 2×2 covariance (arcsec², RA·cosδ). Two
                            // sources: the harness projection of the input
                            // covariance through the ephemeris Jacobian (the
                            // FIRST-ORDER sky covariance) and the engine's
                            // delivered per-method covariance (what the requested
                            // method actually produced). On first-order / f64
                            // rows the projection stays the PUBLISHED golden and
                            // the delivered covariance is a diagnostic only; on
                            // every other method the delivered covariance is
                            // published — see `published_sky_covariance`.
                            let projected: Option<[[f64; 2]; 2]> =
                                match (&covariance, eph.sensitivity.first()) {
                                    (Some(cin), Some(sens)) => project_sky_covariance(
                                        &sens.jacobian,
                                        sens.n_params as usize,
                                        cin,
                                        emp_dec_rad,
                                    ),
                                    _ => None,
                                };
                            let delivered: Option<[[f64; 2]; 2]> = entry
                                .covariance
                                .map(|c| delivered_sky_covariance(&c, emp_dec_rad));
                            let (emp_radec_cov, sky_cov_diag) =
                                published_sky_covariance(&uncertainty_tag, projected, delivered);
                            // Carry the object's free-form notes, plus the
                            // first-order delivered-vs-projection σ diagnostic
                            // when one was recorded (first-order rows only).
                            let eph_notes = match &sky_cov_diag {
                                Some(diag) if data.notes.is_empty() => diag.clone(),
                                Some(diag) => format!("{}; {diag}", data.notes),
                                None => data.notes.clone(),
                            };

                            let d_rho_km = Some((entry.rho_au - hor.rho_au) * compare::AU_KM);
                            let d_lt_s = if entry.light_time_days.is_finite() {
                                hor_light_time_d
                                    .map(|h| (entry.light_time_days - h) * 86400.0)
                            } else {
                                None
                            };

                            eprintln!(
                                "  {} dt={dt:>+6.0}d  sep={sep:.1}mas  dRA={d_ra_arcsec:.1}mas  dDec={d_dec_arcsec:.1}mas",
                                data.name,
                            );

                            results.push(ValidationResult {
                                object: data.name.clone(),
                                population: data.population.clone(),
                                epoch_mjd_tdb: data.epoch,
                                dt_days: dt,
                                t_mjd_tdb: data.epoch + dt,
                                force_model: "standard".to_string(),
                                test_type: "ephemeris".to_string(),
                                channel: channel.clone(),
                                observer: Some(obs_code.to_string()),
                                emp_vs_horizons_km: None,
                                emp_pos_au: None,
                                emp_pos_cov_au2: None,
                                emp_stm: None,
                                emp_time_ms: Some(emp_eph_ms),
                                separation_arcsec: Some(sep),
                                d_ra_arcsec: Some(d_ra_arcsec),
                                d_dec_arcsec: Some(d_dec_arcsec),
                                emp_radec_cov_arcsec2: emp_radec_cov,
                                d_rho_km,
                                d_light_time_s: d_lt_s,
                                ic_pos_au: Some(data.ic_pos),
                                ic_vel_au_d: Some(data.ic_vel),
                                ic_a1: Some(data.a1),
                                ic_a2: Some(data.a2),
                                ic_a3: Some(data.a3),
                                ic_g_alpha: Some(data.ng_alpha),
                                ic_g_r0: Some(data.ng_r0),
                                ic_g_m: Some(data.ng_m),
                                ic_g_n: Some(data.ng_n),
                                ic_g_k: Some(data.ng_k),
                                ic_non_grav_dt: data.ng_dt,
                                ref_pos_au: None,
                                ref_vel_au_d: None,
                                ref_ra_rad: Some(hor_ra_rad),
                                ref_dec_rad: Some(hor_dec_rad),
                                ref_rho_au: Some(hor.rho_au),
                                ref_light_time_d: hor_light_time_d,
                                ref_sun_pos_au: None,
                                ref_sun_vel_au_d: None,
                                ref_od_rms_normalized: None,
                                ref_od_reduced_chi2: None,
                                ref_od_n_obs_used: None,
                                ref_od_n_del_obs_used: None,
                                ref_od_n_dop_obs_used: None,
                                ref_od_data_arc_days: None,
                                ref_od_condition_code: None,
                                ref_od_soln_date: None,
                                ref_od_pe_used: None,
                                ref_od_sb_used: None,
                                n_obs_used: None,
                                od_iterations: None,
                                od_converged: None,
                                od_rms_ra_arcsec: None,
                                od_rms_dec_arcsec: None,
                                od_rms_combined_arcsec: None,
                                od_chi2: None,
                                od_reduced_chi2: None,
                                od_a1: None,
                                od_a2: None,
                                od_a3: None,
                                od_a1_sigma: None,
                                od_a2_sigma: None,
                                od_a3_sigma: None,
                                od_dt: None,
                                od_dt_sigma: None,
                                od_h: None,
                                od_h_sigma: None,
                                od_g1: None,
                                od_g1_sigma: None,
                                od_g2: None,
                                od_g2_sigma: None,
                                od_photometry_model: None,
                                od_photometry_reduced_chi2: None,
                                od_thrust_dv_m_per_s: Vec::new(),
                                od_thrust_dv_sigma_m_per_s: Vec::new(),
                                excluded_perturbers_naif: data.excluded_naif.clone(),
                                // Prop / eph rows run no fit, so there is no
                                // disposition to report. `None` reads as "not
                                // applicable", never as "fixed".
                                od_disposition_marsden: None,
                                od_disposition_dt: None,
                                od_disposition_amrat: None,
                                od_solve_for_used: None,
                                od_joint_covariance_width: None,
                                od_disposition_thrust: Vec::new(),
                                od_warnings: Vec::new(),
                                propagation_uncertainty: Some(uncertainty_tag.to_string()),
                                // Per-method output. STOP (0.11 wrapper gap):
                                // the ephemeris seam (`EphemerisEntry` /
                                // `EphemerisResult`) flattens the delivered sky
                                // covariance to a bare 6×6 and carries NO
                                // resolved-kind tag and NO packed joint, so
                                // `resolved_method` / `cov_kind` / `cov_joint_*`
                                // cannot be read off the delivered ephemeris row
                                // at this pin — left `None` rather than
                                // back-filled from the request (which would be a
                                // silent substitution). The sky covariance is
                                // the harness projection on first-order rows
                                // (the delivered covariance a `notes`
                                // diagnostic) and the engine's delivered
                                // per-method product on every other method — see
                                // `published_sky_covariance`. The per-orbit
                                // outcome channel IS carried (`outcomes[0]`).
                                // See the module doc.
                                resolved_method: None,
                                cov_kind: None,
                                cov_joint_width: None,
                                cov_tri: None,
                                orbit_delivered: eph_outcome.as_ref().map(|(d, _)| *d),
                                orbit_status: eph_outcome.as_ref().map(|(_, s)| s.clone()),
                                mix_n_components_total: None,
                                mix_weight_delivered: None,
                                mix_n_failed: None,
                                mix_n_unresolved: None,
                                mix_n_curvature_refused: None,
                                mix_n_sky_linearization_refused: None,
                                assist_vs_horizons_km: None,
                                emp_vs_assist_km: None,
                                assist_time_ms: None,
                                assist_call_time_ms: None,
                                assist_stm: None,
                                fd_stm: None,
                                var_stm_fixed: None,
                                speed_ratio: None,
                                findorb_rms_residual: None,
                                findorb_n_obs_used: None,
                                findorb_n_obs_rejected: None,
                                findorb_vs_horizons_km: None,
                                emp_vs_findorb_km: None,
                                findorb_separation_arcsec: None,
                                findorb_d_ra_arcsec: None,
                                findorb_d_dec_arcsec: None,
                                findorb_d_rho_km: None,
                                findorb_time_ms: None,
                                kete_time_ms: None,
                                jorbit_time_ms: None,
                                oorb_vs_horizons_km: None,
                                emp_vs_oorb_km: None,
                                oorb_time_ms: None,
                                oorb_separation_arcsec: None,
                                oorb_d_ra_arcsec: None,
                                oorb_d_dec_arcsec: None,
                                oorb_d_rho_km: None,
                                orbfit_rms_arcsec: None,
                                orbfit_n_obs_used: None,
                                orbfit_n_obs_rejected: None,
                                orbfit_time_ms: None,
                            orbfit_error: None,
                                layup_chi2: None,
                                layup_reduced_chi2: None,
                                layup_n_obs_used: None,
                                layup_converged: None,
                                layup_time_ms: None,
                                // GRSS is an external comparator; the reference channel never fills these.
                                grss_vs_horizons_km: None,
                                emp_vs_grss_km: None,
                                grss_time_ms: None,
                                grss_separation_arcsec: None,
                                grss_d_ra_arcsec: None,
                                grss_d_dec_arcsec: None,
                                grss_d_rho_km: None,
                                grss_rms_arcsec: None,
                                grss_chi2: None,
                                grss_reduced_chi2: None,
                                grss_converged: None,
                                grss_n_obs_used: None,
                                grss_n_obs_rejected: None,
                                grss_n_obs_unsupported: None,
                                grss_rms_delay_us: None,
                                grss_rms_doppler_hz: None,
                                grss_n_delay_used: None,
                                grss_n_doppler_used: None,
                                grss_sigma_pos_km: None,
                                grss_model_note: None,
                                grss_error: None,
                                source_version: engine_version.clone(),
                                timestamp: timestamp.clone(),
                                notes: eph_notes,
                            });
                        }
                        None => continue,
                    }
                }
                } // end for &obs_code (observing sites)
                } // end for &attach

                results
            })
            .collect()
    });

    all_results.into_iter().flatten().collect()
}

/// Result of [`run_od_validation`] — the existing per-row metric records
/// plus a per-object orbit + covariance sidecar and the bidirectional
/// orbit comparisons used by the orbit-comparison panel.
pub struct OdValidationOutput {
    /// Per-row metric records (existing `validation_rust_od.json` shape).
    pub results: Vec<ValidationResult>,
    /// Per-object fitted orbit + covariance, in native + ICRF-Cartesian
    /// + ecliptic-Keplerian representations. Includes both the
    ///   at-native-epoch records and any propagated records used as inputs
    ///   to the comparison kernel.
    pub captured_orbits: Vec<CapturedOrbit>,
    /// Bidirectional comparisons: per (empyrean_od, reference) pair, one
    /// comparison at the fit epoch and one at the reference epoch.
    pub orbit_comparisons: Vec<OrbitComparison>,
}

/// The solve-for metadata a fit reports about itself, in the schema's
/// wire shape.
///
/// Read off the *result*, never off the config: under
/// `SolveForParams::Auto` the two differ by design, and the whole point of
/// recording dispositions is to capture the width the fit actually ran at.
struct SolveMetadata {
    marsden: Option<String>,
    dt: Option<String>,
    amrat: Option<String>,
    thrust: Vec<String>,
    solve_for_used: Option<String>,
    warnings: Vec<String>,
    joint_width: Option<u32>,
}

impl SolveMetadata {
    fn from_fit(dr: &empyrean::DetermineResult) -> Self {
        let d = &dr.dispositions;
        // Trailing all-fixed entries carry no information — every orbit
        // declares MAX_THRUST_SEGMENTS slots whether or not it has burns —
        // so trim to the last non-fixed entry. An orbit with no thrust at
        // all reports an empty list rather than a run of "fixed".
        let last_active = d
            .thrust
            .iter()
            .rposition(|p| !matches!(p, empyrean::ParamDisposition::Fixed));
        let thrust = match last_active {
            Some(i) => d.thrust[..=i]
                .iter()
                .map(|p| p.as_tag().to_string())
                .collect(),
            None => Vec::new(),
        };
        let solve_for_used = Some(
            match dr.solve_for_used {
                empyrean::SolveForParams::StateOnly => "state_only",
                empyrean::SolveForParams::StateAndNonGrav => "state_and_nongrav",
                empyrean::SolveForParams::Auto => "auto",
                empyrean::SolveForParams::Explicit(_) => "explicit",
            }
            .to_string(),
        );
        Self {
            marsden: Some(d.marsden.as_tag().to_string()),
            dt: Some(d.dt.as_tag().to_string()),
            amrat: Some(d.amrat.as_tag().to_string()),
            thrust,
            solve_for_used,
            warnings: dr.warnings.clone(),
            // The go-forward joint. A state-only fit carries no
            // `solved_covariance`, and its joint is the 6×6 — reported as
            // width 6 rather than left absent, so "state-only fit" and
            // "channel does not report a width" stay distinguishable.
            joint_width: Some(dr.solved_covariance.as_ref().map_or(6, |c| c.width as u32)),
        }
    }
}

/// Resolve NAIF ids from a plan row's `excluded_perturbers_naif` into the
/// origins the engine takes off the perturber set.
///
/// An id the engine does not recognise is an error, not a body to skip. The
/// old OD path silently mapped an unparseable designation to "exclude
/// nothing", which is the worst of the three outcomes: the object runs *with*
/// the self-perturbation, and the row still reads as a self-perturber row.
pub(crate) fn naif_to_origins(naif: &[i32]) -> Result<Vec<Origin>, String> {
    naif.iter()
        .map(|&id| {
            Origin::from_naif_id(id)
                .ok_or_else(|| format!("unknown NAIF id in excluded_perturbers_naif: {id}"))
        })
        .collect()
}

/// Build an OD **failure row**: a `ValidationResult` that records *why* no fit
/// was produced, in the same row stream a successful fit would have joined.
///
/// The `determine()` error path already emitted one of these (empyrean-8l28);
/// this lifts that pattern out so every way an OD can fail to produce a number
/// goes through it. A fixture that is missing, unreadable, unparseable, or
/// empty is a validation *failure*, not an absence — the project rule is "no
/// hidden fallbacks in scientific code: never silently substitute defaults,
/// drop observations, or degrade quality; every mismatch must surface loudly."
///
/// The distinction is not cosmetic. A row carrying `od_converged: false` and
/// the reason in `notes` reaches the report and the row counts; a row that was
/// never emitted is indistinguishable downstream from "this object was never
/// part of the run". That is precisely how the OD channel stayed dead in CI
/// for the life of this repo: fifty missing fixtures produced fifty `eprintln`
/// lines nobody reads and an empty JSON array that every consumer treated as
/// "nothing to do". Fifty failure rows in the report would have said it out
/// loud on the first run.
///
/// Callers fill in whatever they know (`n_obs_used`, `emp_time_ms`,
/// `od_iterations`); everything else stays `None`, which reads as "not
/// measured" rather than a fabricated zero.
fn od_failure_row(
    obj: &ValidationObject,
    test_type: &str,
    channel: &str,
    tier_str: &str,
    excluded_naif: &[i32],
    engine_version: Option<String>,
    note: String,
) -> ValidationResult {
    let mut row = ValidationResult::empty();
    row.object = obj.name.to_string();
    row.population = obj.population.to_string();
    row.test_type = test_type.to_string();
    row.channel = channel.to_string();
    row.force_model = tier_str.to_string();
    row.od_converged = Some(false);
    row.excluded_perturbers_naif = excluded_naif.to_vec();
    row.source_version = engine_version;
    row.timestamp = chrono::Utc::now().to_rfc3339();
    row.notes = note;
    row
}

/// Second OD pass: **optical + radar**, for the objects that have a
/// radar-augmented fixture in `fixtures/psv-radar/`.
///
/// Reads the same optical arc plus the ADES `<radar>` delay/Doppler table and
/// runs a second `determine` with the same `od_config`, emitting an
/// `orbit_determination_radar` row so the report and the find_orb merge
/// cross-check the radar-tightened orbit exactly as they do the optical-only
/// fit.
///
/// **Independent of the optical fixture by construction.** This used to be
/// nested inside the optical fit's success path, so the five tracked radar
/// fixtures — the only OD fixtures that were ever committed to this repo —
/// could not run at all while `fixtures/psv/` was absent: the optical branch
/// returned first. find_orb still spent CI time on its radar pass, merging
/// onto rows that could not exist. Reachability is the whole point of pulling
/// it out here.
///
/// Objects with no radar fixture (45 of 50) return no rows — that is an
/// absence, not a failure. Every other outcome is a row: a fixture present but
/// unreadable, unparseable, carrying no radar records, or failing to converge
/// is a failure with a reason, never a log line.
#[allow(clippy::too_many_arguments)]
fn run_radar_od(
    ctx: &Context,
    obj: &ValidationObject,
    fixtures_dir: &std::path::Path,
    od_config: &ODConfig,
    channel: &str,
    tier_str: &str,
    engine_version: Option<String>,
    timestamp: &str,
) -> Vec<ValidationResult> {
    let mut results: Vec<ValidationResult> = Vec::new();
    let excluded_naif_r: Vec<i32> = od_config
        .excluded_perturbers
        .iter()
        .copied()
        .map(Origin::naif_id)
        .collect();
    let radar_failure = |note: String| -> ValidationResult {
        od_failure_row(
            obj,
            empyrean_validation::schema::test_types::ORBIT_DETERMINATION_RADAR,
            channel,
            tier_str,
            &excluded_naif_r,
            engine_version.clone(),
            note,
        )
    };

    let Some(radar_psv) = fixtures_dir
        .parent()
        .map(|p| p.join("psv-radar"))
        .into_iter()
        .flat_map(|d| {
            [
                d.join(format!("{}.psv", obj.name)),
                d.join(format!("{}.psv", obj.name.replace('/', "_"))),
                d.join(format!("{}.psv", obj.mpc_designation)),
            ]
        })
        .find(|p| p.exists())
    else {
        // No radar fixture for this object — the normal case for the bulk of
        // the catalog. Nothing to report.
        return results;
    };

    let psv_r = match std::fs::read_to_string(&radar_psv) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "  {}: radar FAIL (read PSV: {e}) — emitting failure row",
                obj.name
            );
            results.push(radar_failure(format!(
                "read radar PSV {}: {e}",
                radar_psv.display()
            )));
            return results;
        }
    };
    let obs_r = match ctx.read_ades(&psv_r) {
        Ok(o) => o,
        Err(e) => {
            eprintln!(
                "  {}: radar FAIL (parse PSV: {e}) — emitting failure row",
                obj.name
            );
            results.push(radar_failure(format!(
                "parse radar PSV {}: {e}",
                radar_psv.display()
            )));
            return results;
        }
    };
    if obs_r.radar_len() == 0 {
        eprintln!(
            "  {}: radar FAIL (fixture carries zero radar records) — emitting failure row",
            obj.name
        );
        let mut row = radar_failure(format!(
            "radar PSV {} parsed to {} observations but zero <radar> records",
            radar_psv.display(),
            obs_r.len()
        ));
        row.n_obs_used = Some(obs_r.len() as u32);
        results.push(row);
        return results;
    }

    eprintln!(
        "  {}: + radar OD ({} obs incl {} radar)...",
        obj.name,
        obs_r.len(),
        obs_r.radar_len()
    );
    let t0r = std::time::Instant::now();
    // determine is batch-first: it groups the observations by ADES object
    // identifier and returns one entry per group, so a fit that fails is a
    // failed *entry* inside an `Ok` batch rather than an `Err` from the call.
    // `into_single` collapses both hazards onto this row's existing failure
    // path — it refuses a batch that is not exactly one delivered fit, naming
    // the objects when a fixture grouped into several rather than picking one.
    let dr = match ctx
        .determine(&obs_r, None, od_config)
        .and_then(|batch| batch.into_single())
    {
        Ok(dr) => dr,
        Err(e) => {
            let ms_fail = t0r.elapsed().as_secs_f64() * 1000.0;
            eprintln!("  {}: radar OD FAIL ({e}) — emitting failure row", obj.name);
            let mut row = radar_failure(format!("radar determine FAIL: {e}"));
            // `n_obs_used` and `od_iterations` are left EMPTY, not filled with
            // stand-ins. The fit failed, so it used no observations and ran an
            // unknown number of iterations; the only honest value is "no
            // value".
            //
            // What used to be here: `n_obs_used = obs_r.len()` (the fixture's
            // observation count, never a fitted selection) and
            // `od_iterations = od_config.max_iterations` (the CAP, echoed back
            // as though the solver had reached it). Both read as measurements
            // in the results JSON and in the report, and both are fiction.
            // Measured cost of that: Bennu's radar row reported "100
            // iterations, 603 observations" for a solve that stopped at 27
            // iterations on damping exhaustion having selected nothing — which
            // is a completely different failure, with a completely different
            // fix, from the "hit the iteration cap on a 603-observation arc"
            // the row described. The real cause is in `error`, which carries
            // the solver's own stop reason; nothing needs to be invented
            // alongside it.
            row.emp_time_ms = Some(ms_fail);
            results.push(row);
            return results;
        }
    };
    let ms_r = t0r.elapsed().as_secs_f64() * 1000.0;
    let orbit_r = dr.state();
    let meta_r = SolveMetadata::from_fit(&dr);
    eprintln!(
        "    radar: converged={} rms_combined={:.4} ({:.0}ms)",
        dr.converged, dr.summary.rms_combined_arcsec, ms_r
    );
    results.push(ValidationResult {
        object: obj.name.to_string(),
        population: obj.population.to_string(),
        epoch_mjd_tdb: orbit_r.epoch.mjd_tdb().unwrap_or(f64::NAN),
        dt_days: 0.0,
        t_mjd_tdb: orbit_r.epoch.mjd_tdb().unwrap_or(f64::NAN),
        force_model: tier_str.to_string(),
        test_type: empyrean_validation::schema::test_types::ORBIT_DETERMINATION_RADAR.to_string(),
        channel: channel.to_string(),
        observer: None,
        emp_vs_horizons_km: None,
        emp_pos_au: Some(orbit_r.position),
        emp_pos_cov_au2: None,
        emp_stm: None,
        emp_time_ms: Some(ms_r),
        separation_arcsec: None,
        d_ra_arcsec: None,
        d_dec_arcsec: None,
        emp_radec_cov_arcsec2: None,
        d_rho_km: None,
        d_light_time_s: None,
        ic_pos_au: None,
        ic_vel_au_d: None,
        ic_a1: None,
        ic_a2: None,
        ic_a3: None,
        ic_g_alpha: None,
        ic_g_r0: None,
        ic_g_m: None,
        ic_g_n: None,
        ic_g_k: None,
        ic_non_grav_dt: None,
        ref_pos_au: None,
        ref_vel_au_d: None,
        ref_ra_rad: None,
        ref_dec_rad: None,
        ref_rho_au: None,
        ref_light_time_d: None,
        ref_sun_pos_au: None,
        ref_sun_vel_au_d: None,
        ref_od_rms_normalized: None,
        ref_od_reduced_chi2: None,
        ref_od_n_obs_used: None,
        ref_od_n_del_obs_used: None,
        ref_od_n_dop_obs_used: None,
        ref_od_data_arc_days: None,
        ref_od_condition_code: None,
        ref_od_soln_date: None,
        ref_od_pe_used: None,
        ref_od_sb_used: None,
        n_obs_used: Some(dr.summary.num_selected as u32),
        od_iterations: Some(dr.iterations),
        od_converged: Some(dr.converged),
        od_rms_ra_arcsec: Some(dr.summary.rms_ra_arcsec),
        od_rms_dec_arcsec: Some(dr.summary.rms_dec_arcsec),
        od_rms_combined_arcsec: Some(dr.summary.rms_combined_arcsec),
        od_chi2: Some(dr.summary.chi2),
        od_reduced_chi2: Some(dr.summary.reduced_chi2),
        od_a1: None,
        od_a2: None,
        od_a3: None,
        od_a1_sigma: None,
        od_a2_sigma: None,
        od_a3_sigma: None,
        od_dt: None,
        od_dt_sigma: None,
        od_h: None,
        od_h_sigma: None,
        od_g1: None,
        od_g1_sigma: None,
        od_g2: None,
        od_g2_sigma: None,
        od_photometry_model: None,
        od_photometry_reduced_chi2: None,
        od_thrust_dv_m_per_s: Vec::new(),
        od_thrust_dv_sigma_m_per_s: Vec::new(),
        excluded_perturbers_naif: excluded_naif_r,
        od_disposition_marsden: meta_r.marsden,
        od_disposition_dt: meta_r.dt,
        od_disposition_amrat: meta_r.amrat,
        od_disposition_thrust: meta_r.thrust,
        od_solve_for_used: meta_r.solve_for_used,
        od_warnings: meta_r.warnings,
        od_joint_covariance_width: meta_r.joint_width,
        propagation_uncertainty: None,
        // Per-method uncertainty output — NOT PRODUCED at this pin. The OD fit
        // runs method-free: ae00643's `ODConfig` carries no `uncertainty_method`
        // (being added to the wrapper separately), so there is no OD method
        // axis and no per-fit packed joint to read. Recorded by name in `notes`
        // rather than left silently blank or defaulted to first order.
        resolved_method: None,
        cov_kind: None,
        cov_joint_width: None,
        cov_tri: None,
        orbit_delivered: None,
        orbit_status: None,
        mix_n_components_total: None,
        mix_weight_delivered: None,
        mix_n_failed: None,
        mix_n_unresolved: None,
        mix_n_curvature_refused: None,
        mix_n_sky_linearization_refused: None,
        assist_vs_horizons_km: None,
        emp_vs_assist_km: None,
        assist_time_ms: None,
        assist_call_time_ms: None,
        assist_stm: None,
        fd_stm: None,
        var_stm_fixed: None,
        speed_ratio: None,
        findorb_rms_residual: None,
        findorb_n_obs_used: None,
        findorb_n_obs_rejected: None,
        findorb_vs_horizons_km: None,
        emp_vs_findorb_km: None,
        findorb_separation_arcsec: None,
        findorb_d_ra_arcsec: None,
        findorb_d_dec_arcsec: None,
        findorb_d_rho_km: None,
        findorb_time_ms: None,
        kete_time_ms: None,
        jorbit_time_ms: None,
        oorb_vs_horizons_km: None,
        emp_vs_oorb_km: None,
        oorb_time_ms: None,
        oorb_separation_arcsec: None,
        oorb_d_ra_arcsec: None,
        oorb_d_dec_arcsec: None,
        oorb_d_rho_km: None,
        orbfit_rms_arcsec: None,
        orbfit_n_obs_used: None,
        orbfit_n_obs_rejected: None,
        orbfit_time_ms: None,
        orbfit_error: None,
        layup_chi2: None,
        layup_reduced_chi2: None,
        layup_n_obs_used: None,
        layup_converged: None,
        layup_time_ms: None,
        // GRSS is an external comparator; the reference channel never fills these.
        grss_vs_horizons_km: None,
        emp_vs_grss_km: None,
        grss_time_ms: None,
        grss_separation_arcsec: None,
        grss_d_ra_arcsec: None,
        grss_d_dec_arcsec: None,
        grss_d_rho_km: None,
        grss_rms_arcsec: None,
        grss_chi2: None,
        grss_reduced_chi2: None,
        grss_converged: None,
        grss_n_obs_used: None,
        grss_n_obs_rejected: None,
        grss_n_obs_unsupported: None,
        grss_rms_delay_us: None,
        grss_rms_doppler_hz: None,
        grss_n_delay_used: None,
        grss_n_doppler_used: None,
        grss_sigma_pos_km: None,
        grss_model_note: None,
        grss_error: None,
        source_version: engine_version.clone(),
        timestamp: timestamp.to_string(),
        notes: with_od_method_note(format!("optical+radar ({} radar obs)", obs_r.radar_len())),
    });
    results
}

/// The note every `orbit_determination_transport` row carries. The plan row
/// carries no transport target (its `t_mjd_tdb` is a `0.0` placeholder — the
/// far-epoch target is runtime-derived), so the post-fit covariance is
/// transported AT THE FIT EPOCH (dt = 0). Named on the row so a reader never
/// mistakes the degenerate (zero-offset) cross-method diagnostic for a
/// far-epoch transport — design ruling 13 — and matched to the core channel's
/// `replay_od_transport` caveat so rust and core transport rows read alike.
const OD_TRANSPORT_FIT_EPOCH_NOTE: &str = "transport at the fit epoch (plan row carries no target; \
     the far-epoch target is the OD runner's paired-orbit epoch)";

/// The post-fit OD transport leg (design ruling 9): the OD method axis rides
/// the *transport* of the fitted covariance, not the fit. The wrapper's
/// `ODConfig` refuses a non-first-order *fit* by name (so the OD fit rows carry
/// [`OD_METHOD_AXIS_NOT_PRODUCED`]), but a *propagation* of the fitted
/// covariance runs every method — so this is where SecondOrder / SigmaPoint /
/// MonteCarlo / GaussianMixture produce a joint on the OD seam, closing the
/// fourth gap commit 3 named.
///
/// For one fitted orbit it emits one `orbit_determination_transport` row per
/// method the plan carries ([`build_uncertainty_axes`], the same axis the
/// propagation sweep uses — identical method mapping and the suite-wide
/// Monte-Carlo N / seed), propagating the fitted orbit *with its solved
/// covariance* to the FIT EPOCH under each method and reading the propagation
/// products off the delivered result exactly as the propagation sweep does (no
/// recompute): [`resolved_method_for`] the delivered kind, the delivered packed
/// joint ([`cov_kind_wire`] / width / lower triangle), the per-orbit delivery
/// outcome off `outcomes[0]` ([`orbit_outcome_channel`] — never a row count),
/// the position-covariance moment view, and the retained Gaussian-mixture
/// tallies ([`populate_mixture_tallies`]) on a row the engine split.
///
/// The transport target is the fit epoch (dt = 0): the plan row carries no
/// target and a per-fit leg has no paired far-epoch orbit, so the leg matches
/// the core channel at this pin. The dispatch and the delivered kinds are real;
/// the cross-method *numeric* diagnostic is degenerate at a zero offset and is
/// captioned as such in [`OD_TRANSPORT_FIT_EPOCH_NOTE`].
#[allow(clippy::too_many_arguments)]
fn od_transport_rows(
    ctx: &Context,
    fitted: &Orbit,
    channel: &str,
    object: &str,
    population: &str,
    tier: ForceModelTier,
    tier_str: &str,
    excluded: &[Origin],
    is_close_approach: bool,
    base_notes: &str,
    engine_version: Option<String>,
    timestamp: &str,
) -> Vec<ValidationResult> {
    // The transport target: the fit epoch, read off the fitted orbit's state
    // (dt = 0). The plan row's `t_mjd_tdb` is a `0.0` placeholder, so the
    // target is derived here, never read from the row.
    let fit_epoch = fitted.state.epoch;
    let fit_epoch_mjd = fit_epoch.mjd_tdb().unwrap_or(f64::NAN);
    // The requested-perturber NAIF ids for the row, derived from the Origins
    // the fit used (never read off a moved-out catalog field).
    let excluded_naif: Vec<i32> = excluded.iter().copied().map(Origin::naif_id).collect();
    // Same axis as the propagation sweep and the plan's transport rows
    // (`plan_methods_for_object`): a close-approach object adds the mixture arm.
    use empyrean_validation::schema::uncertainty_modes as um;
    let axes = build_uncertainty_axes(true, is_close_approach);
    let mut rows: Vec<ValidationResult> = Vec::with_capacity(axes.len());

    for axis in &axes {
        let prop_config = PropagationConfig {
            force_model: tier,
            excluded_perturbers: excluded.to_vec(),
            uncertainty_method: axis.method.clone(),
            frame: Frame::ICRF,
            ..PropagationConfig::default()
        };

        // The 0.11 per-method products, read off the delivered propagation
        // exactly as the propagation sweep reads them (never recomputed).
        let mut resolved_kind: Option<CovarianceKind> = None;
        let mut emp_pos_cov: Option<[[f64; 3]; 3]> = None;
        let mut cov_joint: Option<empyrean::PackedJoint> = None;
        let mut outcome_channel: Option<(bool, String)> = None;
        let mut mixture_components: Vec<empyrean::propagate::MixtureComponent> = Vec::new();
        let mut emp_pos: Option<[f64; 3]> = None;

        let t0 = Instant::now();
        let emp_ms = match ctx.propagate(std::slice::from_ref(fitted), &[fit_epoch], &prop_config) {
            Ok(result) => {
                let ms = t0.elapsed().as_secs_f64() * 1000.0;
                if let Some(st) = result.states.first() {
                    emp_pos = Some(st.position);
                    // The per-method products — the delivered per-state packed
                    // joint (for every kind, sampling kinds included), the
                    // resolved kind, the per-orbit outcome, and the retained
                    // mixture components — read off the result by the same
                    // shared readback the propagation sweep uses (no copy).
                    let products = read_prop_products(&result, axis.attach);
                    resolved_kind = products.resolved_kind;
                    emp_pos_cov = products.emp_pos_cov;
                    cov_joint = products.cov_joint;
                    outcome_channel = products.outcome_channel;
                    mixture_components = products.mixture_components;
                }
                Some(ms)
            }
            Err(e) => {
                // A hard propagation error is still a reported row (with the
                // products unset), never a dropped row.
                eprintln!("  {object} OD transport {} FAIL ({e})", axis.tag);
                None
            }
        };

        // The transport row mirrors the plan's `orbit_determination_transport`
        // row shape (identity + method tag), filled with the fit-epoch target
        // and the delivered products. It is not a propagation/ephemeris row, so
        // it carries no IC / ref values — `empty()` is the plan's own base.
        let mut row = ValidationResult::empty();
        row.object = object.to_string();
        row.population = population.to_string();
        row.epoch_mjd_tdb = fit_epoch_mjd;
        row.dt_days = 0.0;
        row.t_mjd_tdb = fit_epoch_mjd;
        row.force_model = tier_str.to_string();
        row.test_type =
            empyrean_validation::schema::test_types::ORBIT_DETERMINATION_TRANSPORT.to_string();
        row.channel = channel.to_string();
        row.emp_pos_au = emp_pos;
        row.emp_pos_cov_au2 = emp_pos_cov;
        row.emp_time_ms = emp_ms;
        row.excluded_perturbers_naif = excluded_naif.clone();
        row.propagation_uncertainty = Some(um::compose(axis.tag, um::DETECTION_ON));
        // Per-method products, read off the 0.11 wrapper (never recomputed).
        // resolved_method is the delivered kind's method, composed with the
        // row's detection arm.
        row.resolved_method =
            resolved_method_for(axis.tag, resolved_kind).map(|m| um::compose(&m, um::DETECTION_ON));
        row.cov_kind = cov_joint.as_ref().map(|j| cov_kind_wire(j.kind));
        row.cov_joint_width = cov_joint.as_ref().map(|j| j.width as u32);
        row.cov_tri = cov_joint.as_ref().map(|j| j.tri.clone());
        row.orbit_delivered = outcome_channel.as_ref().map(|(d, _)| *d);
        row.orbit_status = outcome_channel.as_ref().map(|(_, s)| s.clone());
        row.source_version = engine_version.clone();
        row.timestamp = timestamp.to_string();
        row.notes = if base_notes.is_empty() {
            OD_TRANSPORT_FIT_EPOCH_NOTE.to_string()
        } else {
            format!("{base_notes}; {OD_TRANSPORT_FIT_EPOCH_NOTE}")
        };
        // Gaussian-mixture side table: populate only on a row the engine
        // actually split (delivered a Mixture); an unsplit row keeps every
        // tally `None`, matching the propagation sweep and the core channel.
        if resolved_kind == Some(CovarianceKind::Mixture) {
            populate_mixture_tallies(&mixture_components, &mut row);
        }
        rows.push(row);
    }
    rows
}

/// Run orbit-determination validation: load PSV from
/// `validation/fixtures/psv/{name}.psv`, run `ctx.determine`, emit one
/// `ValidationResult` row per object with `test_type =
/// "orbit_determination"`. The fitted Cartesian state at the OD epoch
/// is stored in `emp_pos_au` so Section 09 can compute cross-channel
/// fidelity for OD the same way it does for propagation.
///
/// Also emits [`CapturedOrbit`] sidecar records for the
/// orbit-comparison panel:
/// - one per successful OD fit (`source = "empyrean_od"`), and
/// - one per object that has a JPL SBDB entry (`source = "sbdb"`).
///
/// The comparison kernel pairs these by `object` to produce the
/// per-object Δstate + Mahalanobis distances + σ ratios shown in the
/// "Fitted orbit + covariance vs references" report panel.
pub fn run_od_validation(
    ctx: &Context,
    objs: &[&ValidationObject],
    fixtures_dir: &std::path::Path,
    max_iterations: u32,
    tier: ForceModelTier,
    sbdb_cache_dir: Option<&std::path::Path>,
) -> OdValidationOutput {
    let timestamp = chrono::Utc::now().to_rfc3339();
    let channel = "rust".to_string();
    // Provenance: the exact empyrean engine (empyrean-core / villeneuve /
    // scott / nolan) this channel exercises. Stamped on every row (including
    // the OD-failure row) so the merged report records which code produced —
    // or failed to produce — the fit. `None` only if the version FFI fails;
    // same accessor the CapturedOrbit sidecar uses, so a row and its sidecar
    // always agree.
    let engine_version = empyrean::version_string().ok();
    let tier_str = match tier {
        ForceModelTier::Approximate => "approximate",
        ForceModelTier::Basic => "basic",
        ForceModelTier::Standard => "standard",
    }
    .to_string();

    // Parallelize across catalog objects. Each fit is independent (no
    // shared mutable state in the engine's determine pipeline); empyrean::Context
    // is Send + Sync (see empyrean/src/context.rs:22-23 — "concurrent
    // propagation calls are safe") so the same `&ctx` is safely shared
    // across Rayon workers. Per-object output is collected into a Vec of
    // (results, captured_orbits, orbit_comparisons) triples then flattened
    // back into the top-level accumulators after `.collect()`.
    //
    // Per-object stdout interleaves under parallel execution; every line
    // includes the object name so the log stays readable. Per-call
    // `emp_time_ms` is wall-clock cost of one ctx.determine() call (under
    // N-way contention with concurrent fits) — cross-channel comparability
    // is preserved because every channel measures the same per-call shape.
    let per_object: Vec<(
        Vec<ValidationResult>,
        Vec<CapturedOrbit>,
        Vec<OrbitComparison>,
    )> = objs
        .par_iter()
        .map(|obj| {
            let mut results: Vec<ValidationResult> = Vec::new();
            let mut captured_orbits: Vec<CapturedOrbit> = Vec::new();
            let mut orbit_comparisons: Vec<OrbitComparison> = Vec::new();
            // SB441-N16 self-perturbers: exclude the body's own gravity from
            // the perturber set during fitting. Without this the integrator
            // self-pulls and converges to junk fixed points (Pallas RMS 8000″,
            // Iris RMS 149″, etc.). The validation catalog tags these with
            // population = "Self-Perturber"; mpc_designation carries the
            // asteroid number for Origin::Asteroid construction.
            //
            // Derived from the catalog entry alone, so it is hoisted above the
            // fixture load: a failure row for a fixture that never loaded still
            // records which perturber set the fit *would* have used.
            let excluded_naif = empyrean_validation::plan::self_perturber_naif_ids(obj);
            let excluded_origins: Vec<Origin> = match naif_to_origins(&excluded_naif) {
                Ok(o) => o,
                Err(e) => {
                    eprintln!("  {}: OD SKIP ({e})", obj.name);
                    return (Vec::new(), Vec::new(), Vec::new());
                }
            };
            // Fit configuration. Hoisted above both OD passes because it
            // depends only on the catalog entry — the radar pass below must be
            // able to build it without the optical fixture having loaded.
            let od_config = ODConfig {
                force_model: tier,
                max_iterations,
                excluded_perturbers: excluded_origins,
                ..ODConfig::default()
            };

            // ── Radar OD (independent of the optical fixture) ──────────────
            // Runs FIRST and unconditionally. It used to be nested inside the
            // optical fit's success path, which made the five tracked radar
            // fixtures unreachable whenever the (untracked) optical fixtures were
            // absent — i.e. in every CI run this repo has ever done.
            results.extend(run_radar_od(
                ctx,
                obj,
                fixtures_dir,
                &od_config,
                &channel,
                &tier_str,
                engine_version.clone(),
                &timestamp,
            ));

            // Emit a failure row for any way the optical fixture fails to become
            // observations. See `od_failure_row` for why these are rows and not
            // log lines.
            let fixture_failure = |note: String| -> ValidationResult {
                od_failure_row(
                    obj,
                    empyrean_validation::schema::test_types::ORBIT_DETERMINATION,
                    &channel,
                    &tier_str,
                    &excluded_naif,
                    engine_version.clone(),
                    note,
                )
            };

            // Try common PSV filename variants. Object names with a "/"
            // (the comets — "2P/Encke", "103P/Hartley 2", the interstellars)
            // are stored with the slash rewritten to "_" so the name is not
            // read as a path separator; try that sanitized form too.
            let candidates = [
                fixtures_dir.join(format!("{}.psv", obj.name)),
                fixtures_dir.join(format!("{}.psv", obj.name.replace('/', "_"))),
                fixtures_dir.join(format!("{}.psv", obj.mpc_designation)),
            ];
            let path = candidates.iter().find(|p| p.exists());
            let Some(path) = path else {
                eprintln!(
                    "  {}: FAIL (no PSV at {}) — emitting failure row",
                    obj.name,
                    candidates[0].display()
                );
                results.push(fixture_failure(format!(
                    "no PSV fixture: none of {} exist",
                    candidates
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
                return (results, captured_orbits, orbit_comparisons);
            };

            let psv = match std::fs::read_to_string(path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!(
                        "  {}: FAIL (read PSV: {e}) — emitting failure row",
                        obj.name
                    );
                    results.push(fixture_failure(format!("read PSV {}: {e}", path.display())));
                    return (results, captured_orbits, orbit_comparisons);
                }
            };

            let observations = match ctx.read_ades(&psv) {
                Ok(o) => o,
                Err(e) => {
                    eprintln!(
                        "  {}: FAIL (parse PSV: {e}) — emitting failure row",
                        obj.name
                    );
                    results.push(fixture_failure(format!(
                        "parse PSV {}: {e}",
                        path.display()
                    )));
                    return (results, captured_orbits, orbit_comparisons);
                }
            };

            if observations.is_empty() {
                eprintln!(
                    "  {}: FAIL (zero observations) — emitting failure row",
                    obj.name
                );
                let mut row = fixture_failure(format!(
                    "PSV {} parsed to zero observations",
                    path.display()
                ));
                row.n_obs_used = Some(0);
                results.push(row);
                return (results, captured_orbits, orbit_comparisons);
            }

            let n_obs = observations.len();
            eprintln!(
                "  {}: {} observations, running determine{}...",
                obj.name,
                n_obs,
                if excluded_naif.is_empty() {
                    String::new()
                } else {
                    format!(" (excluded perturbers: {excluded_naif:?})")
                },
            );

            let t0 = std::time::Instant::now();
            // Batch-first determine collapsed to this fixture's single object;
            // see the radar pass for why `into_single` and not `iter().next()`.
            let determine_result = match ctx
                .determine(&observations, None, &od_config)
                .and_then(|batch| batch.into_single())
            {
                Ok(r) => r,
                Err(e) => {
                    // empyrean-8l28: emit an explicit failure row so the
                    // downstream report sees the failure rather than the
                    // fixture silently disappearing. Project rule:
                    // "no hidden fallbacks in scientific code — every
                    // mismatch must surface loudly."
                    let ms_fail = t0.elapsed().as_secs_f64() * 1000.0;
                    eprintln!(
                        "  {}: determine FAIL ({e}) — emitting failure row",
                        obj.name
                    );
                    let mut row = fixture_failure(format!("determine FAIL: {e}"));
                    row.n_obs_used = Some(n_obs as u32);
                    row.od_iterations = Some(max_iterations);
                    row.emp_time_ms = Some(ms_fail);
                    results.push(row);
                    return (results, captured_orbits, orbit_comparisons);
                }
            };
            let ms = t0.elapsed().as_secs_f64() * 1000.0;

            eprintln!(
                "    converged={} iterations={} rms_ra={:.4} rms_dec={:.4} chi2={:.2} ({:.0}ms)",
                determine_result.converged,
                determine_result.iterations,
                determine_result.summary.rms_ra_arcsec,
                determine_result.summary.rms_dec_arcsec,
                determine_result.summary.chi2,
                ms
            );

            // `DetermineResult.orbit` is now a re-feedable `Orbit`; take the
            // bare state snapshot (epoch/position/velocity/covariance/frame/
            // origin) the validation channel records.
            let orbit = determine_result.state();
            let meta = SolveMetadata::from_fit(&determine_result);

            // Capture the fitted state + cov in three coordinate views
            // (native Cartesian, Sun-centered ICRF Cartesian, Sun-centered
            // ecliptic-J2000 Keplerian) for the orbit-comparison panel.
            // Transformation via `ctx.transform` propagates covariance
            // through the Jacobian.
            let fit_native = propagated_state_to_coord(&orbit);
            let empy_version = engine_version.clone();
            let fit_captured = match capture_orbit(
                ctx,
                obj.name,
                orbit_sources::EMPYREAN_OD,
                empy_version.clone(),
                &fit_native,
            ) {
                Ok(captured) => Some(captured),
                Err(e) => {
                    eprintln!("  {}: fit orbit-capture transform FAIL ({e})", obj.name);
                    None
                }
            };
            if let Some(c) = &fit_captured {
                captured_orbits.push(c.clone());
            }

            // Capture SBDB's published orbit (if any) so the comparison
            // kernel can pair empyrean_od ↔ sbdb. SBDB returns
            // CometaryCoordinates with covariance for objects that have
            // a published solution; short-arc impactors typically do not.
            // `sbdb_nongrav` carries SBDB's published Marsden (A1, A2, A3) for
            // this object — the reference signal the non-grav-recovery second
            // pass below compares against. `None` when SBDB has no orbit.
            let (sbdb_native, sbdb_captured, sbdb_nongrav) =
                match empyrean::query_sbdb(&[obj.sbdb_query], sbdb_cache_dir) {
                    Ok(batch) if !batch.orbits.is_empty() => {
                        let sbdb_state = batch.orbits[0].state;
                        let sbdb_orbit_id = batch.orbit_ids.first().cloned();
                        let sbdb_ng = (batch.orbits[0].a1, batch.orbits[0].a2, batch.orbits[0].a3);
                        let captured = match capture_orbit(
                            ctx,
                            obj.name,
                            orbit_sources::SBDB,
                            sbdb_orbit_id,
                            &sbdb_state,
                        ) {
                            Ok(c) => {
                                captured_orbits.push(c.clone());
                                Some(c)
                            }
                            Err(e) => {
                                eprintln!(
                                    "  {}: SBDB orbit-capture transform FAIL ({e})",
                                    obj.name,
                                );
                                None
                            }
                        };
                        (Some(sbdb_state), captured, Some(sbdb_ng))
                    }
                    Ok(_) => {
                        eprintln!("  {}: SBDB returned empty batch", obj.name);
                        (None, None, None)
                    }
                    Err(e) => {
                        eprintln!("  {}: SBDB SKIP ({e})", obj.name);
                        (None, None, None)
                    }
                };

            // Bidirectional orbit-vs-orbit comparison. For each (fit,
            // sbdb) pair, propagate one side to the other's epoch and
            // compare in Keplerian space. This produces two rows per pair
            // — one at the fit epoch, one at the sbdb epoch — so the
            // report can show how much each side's uncertainty inflates
            // under propagation.
            if let (Some(fit_c), Some(sbdb_native), Some(sbdb_c)) =
                (&fit_captured, &sbdb_native, &sbdb_captured)
            {
                let prop_cfg = empyrean::PropagationConfig {
                    force_model: tier,
                    uncertainty_method: empyrean::UncertaintyMethod::FirstOrder,
                    frame: empyrean::Frame::ICRF,
                    ..empyrean::PropagationConfig::default()
                };

                // Direction A: bring sbdb to fit's epoch; compare at fit's epoch.
                match propagate_and_capture(
                    ctx,
                    obj.name,
                    "sbdb_at_fit_epoch",
                    empy_version.clone(),
                    sbdb_native,
                    fit_c.epoch_mjd_tdb,
                    &prop_cfg,
                ) {
                    Ok(sbdb_at_fit) => {
                        captured_orbits.push(sbdb_at_fit.clone());
                        // Re-tag as canonical SBDB so the kernel pairs it
                        // with empyrean_od (kernel only matches the two
                        // canonical source tags).
                        let mut sbdb_at_fit_as_sbdb = sbdb_at_fit.clone();
                        sbdb_at_fit_as_sbdb.source = orbit_sources::SBDB.to_string();
                        let mut rows = compare_orbits(&[fit_c.clone(), sbdb_at_fit_as_sbdb], 1.0);
                        for r in rows.iter_mut() {
                            r.common_epoch_source = "fit".to_string();
                            r.notes.push(
                                "reference (SBDB) propagated to fit epoch via STM".to_string(),
                            );
                        }
                        orbit_comparisons.extend(rows);
                    }
                    Err(e) => {
                        eprintln!("  {}: propagate SBDB→fit_epoch FAIL ({e})", obj.name,);
                    }
                }

                // Direction B: bring fit to sbdb's epoch; compare at sbdb's epoch.
                match propagate_and_capture(
                    ctx,
                    obj.name,
                    "fit_at_sbdb_epoch",
                    empy_version.clone(),
                    &fit_native,
                    sbdb_c.epoch_mjd_tdb,
                    &prop_cfg,
                ) {
                    Ok(fit_at_sbdb) => {
                        captured_orbits.push(fit_at_sbdb.clone());
                        // Manually pair: kernel only pairs empyrean_od ↔
                        // sbdb|findorb, so we synthesize a comparison
                        // record by feeding an empyrean_od-tagged copy of the
                        // propagated state.
                        let mut fit_at_sbdb_as_fit = fit_at_sbdb.clone();
                        fit_at_sbdb_as_fit.source = orbit_sources::EMPYREAN_OD.to_string();
                        let mut rows = compare_orbits(&[fit_at_sbdb_as_fit, sbdb_c.clone()], 1.0);
                        for r in rows.iter_mut() {
                            r.common_epoch_source = "sbdb".to_string();
                            r.notes
                                .push("fit propagated to SBDB epoch via STM".to_string());
                        }
                        orbit_comparisons.extend(rows);
                    }
                    Err(e) => {
                        eprintln!("  {}: propagate fit→sbdb_epoch FAIL ({e})", obj.name,);
                    }
                }
            }
            results.push(ValidationResult {
                object: obj.name.to_string(),
                population: obj.population.to_string(),
                epoch_mjd_tdb: orbit.epoch.mjd_tdb().unwrap_or(f64::NAN),
                dt_days: 0.0,
                t_mjd_tdb: orbit.epoch.mjd_tdb().unwrap_or(f64::NAN),
                force_model: tier_str.clone(),
                test_type: "orbit_determination".to_string(),
                channel: channel.clone(),
                observer: None,
                emp_vs_horizons_km: None,
                emp_pos_au: Some(orbit.position),
                emp_pos_cov_au2: None,
                emp_stm: None,
                emp_time_ms: Some(ms),
                separation_arcsec: None,
                d_ra_arcsec: None,
                d_dec_arcsec: None,
                emp_radec_cov_arcsec2: None,
                d_rho_km: None,
                d_light_time_s: None,
                ic_pos_au: None,
                ic_vel_au_d: None,
                ic_a1: None,
                ic_a2: None,
                ic_a3: None,
                ic_g_alpha: None,
                ic_g_r0: None,
                ic_g_m: None,
                ic_g_n: None,
                ic_g_k: None,
                ic_non_grav_dt: None,
                ref_pos_au: None,
                ref_vel_au_d: None,
                ref_ra_rad: None,
                ref_dec_rad: None,
                ref_rho_au: None,
                ref_light_time_d: None,
                ref_sun_pos_au: None,
                ref_sun_vel_au_d: None,
                ref_od_rms_normalized: None,
                ref_od_reduced_chi2: None,
                ref_od_n_obs_used: None,
                ref_od_n_del_obs_used: None,
                ref_od_n_dop_obs_used: None,
                ref_od_data_arc_days: None,
                ref_od_condition_code: None,
                ref_od_soln_date: None,
                ref_od_pe_used: None,
                ref_od_sb_used: None,
                n_obs_used: Some(determine_result.summary.num_selected as u32),
                od_iterations: Some(determine_result.iterations),
                od_converged: Some(determine_result.converged),
                od_rms_ra_arcsec: Some(determine_result.summary.rms_ra_arcsec),
                od_rms_dec_arcsec: Some(determine_result.summary.rms_dec_arcsec),
                od_rms_combined_arcsec: Some(determine_result.summary.rms_combined_arcsec),
                od_chi2: Some(determine_result.summary.chi2),
                od_reduced_chi2: Some(determine_result.summary.reduced_chi2),
                od_a1: None,
                od_a2: None,
                od_a3: None,
                od_a1_sigma: None,
                od_a2_sigma: None,
                od_a3_sigma: None,
                od_dt: None,
                od_dt_sigma: None,
                od_h: None,
                od_h_sigma: None,
                od_g1: None,
                od_g1_sigma: None,
                od_g2: None,
                od_g2_sigma: None,
                od_photometry_model: None,
                od_photometry_reduced_chi2: None,
                od_thrust_dv_m_per_s: Vec::new(),
                od_thrust_dv_sigma_m_per_s: Vec::new(),
                excluded_perturbers_naif: excluded_naif,
                od_disposition_marsden: meta.marsden,
                od_disposition_dt: meta.dt,
                od_disposition_amrat: meta.amrat,
                od_disposition_thrust: meta.thrust,
                od_solve_for_used: meta.solve_for_used,
                od_warnings: meta.warnings,
                od_joint_covariance_width: meta.joint_width,
                propagation_uncertainty: None,
                // Per-method uncertainty output — NOT PRODUCED at this pin on
                // OD fit rows: ae00643's `ODConfig` carries no
                // `uncertainty_method` (being added to the wrapper separately),
                // so the fit is method-free with no OD method axis and no
                // per-fit packed joint to read. Recorded by name in `notes`
                // (OD_METHOD_AXIS_NOT_PRODUCED), never silently blank or
                // defaulted to first order.
                resolved_method: None,
                cov_kind: None,
                cov_joint_width: None,
                cov_tri: None,
                orbit_delivered: None,
                orbit_status: None,
                mix_n_components_total: None,
                mix_weight_delivered: None,
                mix_n_failed: None,
                mix_n_unresolved: None,
                mix_n_curvature_refused: None,
                mix_n_sky_linearization_refused: None,
                assist_vs_horizons_km: None,
                emp_vs_assist_km: None,
                assist_time_ms: None,
                assist_call_time_ms: None,
                assist_stm: None,
                fd_stm: None,
                var_stm_fixed: None,
                speed_ratio: None,
                findorb_rms_residual: None,
                findorb_n_obs_used: None,
                findorb_n_obs_rejected: None,
                findorb_vs_horizons_km: None,
                emp_vs_findorb_km: None,
                findorb_separation_arcsec: None,
                findorb_d_ra_arcsec: None,
                findorb_d_dec_arcsec: None,
                findorb_d_rho_km: None,
                findorb_time_ms: None,
                kete_time_ms: None,
                jorbit_time_ms: None,
                oorb_vs_horizons_km: None,
                emp_vs_oorb_km: None,
                oorb_time_ms: None,
                oorb_separation_arcsec: None,
                oorb_d_ra_arcsec: None,
                oorb_d_dec_arcsec: None,
                oorb_d_rho_km: None,
                orbfit_rms_arcsec: None,
                orbfit_n_obs_used: None,
                orbfit_n_obs_rejected: None,
                orbfit_time_ms: None,
                orbfit_error: None,
                layup_chi2: None,
                layup_reduced_chi2: None,
                layup_n_obs_used: None,
                layup_converged: None,
                layup_time_ms: None,
                // GRSS is an external comparator; the reference channel never fills these.
                grss_vs_horizons_km: None,
                emp_vs_grss_km: None,
                grss_time_ms: None,
                grss_separation_arcsec: None,
                grss_d_ra_arcsec: None,
                grss_d_dec_arcsec: None,
                grss_d_rho_km: None,
                grss_rms_arcsec: None,
                grss_chi2: None,
                grss_reduced_chi2: None,
                grss_converged: None,
                grss_n_obs_used: None,
                grss_n_obs_rejected: None,
                grss_n_obs_unsupported: None,
                grss_rms_delay_us: None,
                grss_rms_doppler_hz: None,
                grss_n_delay_used: None,
                grss_n_doppler_used: None,
                grss_sigma_pos_km: None,
                grss_model_note: None,
                grss_error: None,
                source_version: empy_version.clone(),
                timestamp: timestamp.clone(),
                notes: with_od_method_note(obj.notes.to_string()),
            });

            // ── Post-fit transport under every method (design ruling 9) ──
            // The OD method axis rides the transport of the fitted covariance,
            // not the fit: `ODConfig` refuses a non-first-order fit by name
            // (the fit rows above), but propagating the fitted covariance runs
            // every method. One `orbit_determination_transport` row per method,
            // at the fit epoch (dt = 0) — see `od_transport_rows`.
            results.extend(od_transport_rows(
                ctx,
                &determine_result.orbit,
                &channel,
                obj.name,
                obj.population,
                tier,
                &tier_str,
                &od_config.excluded_perturbers,
                is_close_approach(obj.name),
                obj.notes,
                empy_version.clone(),
                &timestamp,
            ));

            // ── Third OD: non-grav recovery (objects with an SBDB A2 signal) ──
            // For objects whose JPL SBDB reference carries a non-zero
            // transverse non-grav coefficient (Yarkovsky NEOs like Apophis /
            // Bennu and the comets), re-fit the SAME optical arc with
            // `solve_for = StateAndNonGrav` and emit a separate
            // `non_grav_recovery` row carrying the FITTED A1/A2/A3 ± 1σ so the
            // report can compare fitted-vs-JPL in σ. The 1σ comes from the
            // fitted 9×9 (state + A1/A2/A3) covariance diagonal: σ_aᵢ =
            // sqrt(C9x9[6+i][6+i]).
            //
            // Loud-failure rule: if the fit did NOT actually recover non-grav
            // — the 9×9 is absent (`covariance_9x9 == None`) or a fitted a-value
            // is non-finite — that axis is emitted as `None` (never 0, never
            // NaN) so a missing value reads as "non-grav not recovered". (The
            // engine currently has a bug where StateAndNonGrav can silently
            // fall back to a 6-param state-only fit, so most of these rows
            // legitimately come back `None` for now — that is correct.)
            if let Some((ref_a1, ref_a2, ref_a3)) = sbdb_nongrav
                && ref_a2 != 0.0
            {
                eprintln!(
                    "  {}: + non-grav recovery OD (SBDB A2={ref_a2:.3e})...",
                    obj.name
                );
                let ng_config = ODConfig {
                    solve_for: empyrean::SolveForParams::StateAndNonGrav,
                    ..od_config.clone()
                };
                let t0n = std::time::Instant::now();
                match ctx
                    .determine(&observations, None, &ng_config)
                    .and_then(|batch| batch.into_single())
                {
                    Ok(dr) => {
                        let ms_n = t0n.elapsed().as_secs_f64() * 1000.0;
                        let orbit_n = dr.state();
                        let meta_n = SolveMetadata::from_fit(&dr);
                        // Per-axis sigma from the 9×9 covariance diagonal,
                        // present only when non-grav was actually solved.
                        // sqrt() of a non-finite / negative variance yields
                        // NaN, which the guard below maps back to None.
                        let sigma = |i: usize| -> Option<f64> {
                            dr.covariance_9x9
                                .map(|c| c[6 + i][6 + i].sqrt())
                                .filter(|s| s.is_finite())
                        };
                        // Fitted a-value → Some only when finite; otherwise
                        // None ("non-grav not recovered"). Pair each fitted
                        // value with its sigma so a None value never carries
                        // a stray sigma.
                        let fitted = |v: f64, i: usize| -> (Option<f64>, Option<f64>) {
                            if v.is_finite() {
                                (Some(v), sigma(i))
                            } else {
                                (None, None)
                            }
                        };
                        let (od_a1, od_a1_sigma) = fitted(dr.orbit.a1, 0);
                        let (od_a2, od_a2_sigma) = fitted(dr.orbit.a2, 1);
                        let (od_a3, od_a3_sigma) = fitted(dr.orbit.a3, 2);
                        eprintln!(
                            "    non-grav: converged={} 9x9={} a2_fit={:?} σ_a2={:?} ({:.0}ms)",
                            dr.converged,
                            dr.covariance_9x9.is_some(),
                            od_a2,
                            od_a2_sigma,
                            ms_n
                        );
                        let excluded_naif_n: Vec<i32> = ng_config
                            .excluded_perturbers
                            .iter()
                            .copied()
                            .map(Origin::naif_id)
                            .collect();
                        results.push(ValidationResult {
                            object: obj.name.to_string(),
                            population: obj.population.to_string(),
                            epoch_mjd_tdb: orbit_n.epoch.mjd_tdb().unwrap_or(f64::NAN),
                            dt_days: 0.0,
                            t_mjd_tdb: orbit_n.epoch.mjd_tdb().unwrap_or(f64::NAN),
                            force_model: tier_str.clone(),
                            test_type: empyrean_validation::schema::test_types::NON_GRAV_RECOVERY
                                .to_string(),
                            channel: channel.clone(),
                            observer: None,
                            emp_vs_horizons_km: None,
                            emp_pos_au: Some(orbit_n.position),
                            emp_pos_cov_au2: None,
                            emp_stm: None,
                            emp_time_ms: Some(ms_n),
                            separation_arcsec: None,
                            d_ra_arcsec: None,
                            d_dec_arcsec: None,
                            emp_radec_cov_arcsec2: None,
                            d_rho_km: None,
                            d_light_time_s: None,
                            ic_pos_au: None,
                            ic_vel_au_d: None,
                            ic_a1: Some(ref_a1),
                            ic_a2: Some(ref_a2),
                            ic_a3: Some(ref_a3),
                            ic_g_alpha: None,
                            ic_g_r0: None,
                            ic_g_m: None,
                            ic_g_n: None,
                            ic_g_k: None,
                            ic_non_grav_dt: None,
                            ref_pos_au: None,
                            ref_vel_au_d: None,
                            ref_ra_rad: None,
                            ref_dec_rad: None,
                            ref_rho_au: None,
                            ref_light_time_d: None,
                            ref_sun_pos_au: None,
                            ref_sun_vel_au_d: None,
                            ref_od_rms_normalized: None,
                            ref_od_reduced_chi2: None,
                            ref_od_n_obs_used: None,
                            ref_od_n_del_obs_used: None,
                            ref_od_n_dop_obs_used: None,
                            ref_od_data_arc_days: None,
                            ref_od_condition_code: None,
                            ref_od_soln_date: None,
                            ref_od_pe_used: None,
                            ref_od_sb_used: None,
                            n_obs_used: Some(dr.summary.num_selected as u32),
                            od_iterations: Some(dr.iterations),
                            od_converged: Some(dr.converged),
                            od_rms_ra_arcsec: Some(dr.summary.rms_ra_arcsec),
                            od_rms_dec_arcsec: Some(dr.summary.rms_dec_arcsec),
                            od_rms_combined_arcsec: Some(dr.summary.rms_combined_arcsec),
                            od_chi2: Some(dr.summary.chi2),
                            od_reduced_chi2: Some(dr.summary.reduced_chi2),
                            od_a1,
                            od_a2,
                            od_a3,
                            od_a1_sigma,
                            od_a2_sigma,
                            od_a3_sigma,
                            od_dt: None,
                            od_dt_sigma: None,
                            od_h: None,
                            od_h_sigma: None,
                            od_g1: None,
                            od_g1_sigma: None,
                            od_g2: None,
                            od_g2_sigma: None,
                            od_photometry_model: None,
                            od_photometry_reduced_chi2: None,
                            od_thrust_dv_m_per_s: Vec::new(),
                            od_thrust_dv_sigma_m_per_s: Vec::new(),
                            excluded_perturbers_naif: excluded_naif_n,
                            od_disposition_marsden: meta_n.marsden,
                            od_disposition_dt: meta_n.dt,
                            od_disposition_amrat: meta_n.amrat,
                            od_disposition_thrust: meta_n.thrust,
                            od_solve_for_used: meta_n.solve_for_used,
                            od_warnings: meta_n.warnings,
                            od_joint_covariance_width: meta_n.joint_width,
                            propagation_uncertainty: None,
                            // Per-method uncertainty output — NOT PRODUCED at
                            // this pin on OD fit rows: ae00643's `ODConfig`
                            // carries no `uncertainty_method` (being added to the
                            // wrapper separately), so the fit is method-free with
                            // no OD method axis and no per-fit packed joint.
                            // Recorded by name in `notes`, never silently blank
                            // or defaulted to first order.
                            resolved_method: None,
                            cov_kind: None,
                            cov_joint_width: None,
                            cov_tri: None,
                            orbit_delivered: None,
                            orbit_status: None,
                            mix_n_components_total: None,
                            mix_weight_delivered: None,
                            mix_n_failed: None,
                            mix_n_unresolved: None,
                            mix_n_curvature_refused: None,
                            mix_n_sky_linearization_refused: None,
                            assist_vs_horizons_km: None,
                            emp_vs_assist_km: None,
                            assist_time_ms: None,
                            assist_call_time_ms: None,
                            assist_stm: None,
                            fd_stm: None,
                            var_stm_fixed: None,
                            speed_ratio: None,
                            findorb_rms_residual: None,
                            findorb_n_obs_used: None,
                            findorb_n_obs_rejected: None,
                            findorb_vs_horizons_km: None,
                            emp_vs_findorb_km: None,
                            findorb_separation_arcsec: None,
                            findorb_d_ra_arcsec: None,
                            findorb_d_dec_arcsec: None,
                            findorb_d_rho_km: None,
                            findorb_time_ms: None,
                            kete_time_ms: None,
                            jorbit_time_ms: None,
                            oorb_vs_horizons_km: None,
                            emp_vs_oorb_km: None,
                            oorb_time_ms: None,
                            oorb_separation_arcsec: None,
                            oorb_d_ra_arcsec: None,
                            oorb_d_dec_arcsec: None,
                            oorb_d_rho_km: None,
                            orbfit_rms_arcsec: None,
                            orbfit_n_obs_used: None,
                            orbfit_n_obs_rejected: None,
                            orbfit_time_ms: None,
                            orbfit_error: None,
                            layup_chi2: None,
                            layup_reduced_chi2: None,
                            layup_n_obs_used: None,
                            layup_converged: None,
                            layup_time_ms: None,
                            // GRSS is an external comparator; the reference channel never fills these.
                            grss_vs_horizons_km: None,
                            emp_vs_grss_km: None,
                            grss_time_ms: None,
                            grss_separation_arcsec: None,
                            grss_d_ra_arcsec: None,
                            grss_d_dec_arcsec: None,
                            grss_d_rho_km: None,
                            grss_rms_arcsec: None,
                            grss_chi2: None,
                            grss_reduced_chi2: None,
                            grss_converged: None,
                            grss_n_obs_used: None,
                            grss_n_obs_rejected: None,
                            grss_n_obs_unsupported: None,
                            grss_rms_delay_us: None,
                            grss_rms_doppler_hz: None,
                            grss_n_delay_used: None,
                            grss_n_doppler_used: None,
                            grss_sigma_pos_km: None,
                            grss_model_note: None,
                            grss_error: None,
                            source_version: empy_version.clone(),
                            timestamp: timestamp.clone(),
                            notes: with_od_method_note(format!(
                                "non-grav recovery (solve_for=StateAndNonGrav, 9x9={})",
                                dr.covariance_9x9.is_some()
                            )),
                        });
                    }
                    Err(e) => {
                        eprintln!("  {}: non-grav recovery OD FAIL ({e})", obj.name);
                    }
                }
            }

            (results, captured_orbits, orbit_comparisons)
        })
        .collect();

    // Flatten per-object triples back into the function-level accumulators.
    // Input order is preserved by `.par_iter().map().collect()` (Rayon's
    // contract — collect returns results in the input slice's order).
    let mut results: Vec<ValidationResult> = Vec::new();
    let mut captured_orbits: Vec<CapturedOrbit> = Vec::new();
    let mut orbit_comparisons: Vec<OrbitComparison> = Vec::new();
    for (r, c, p) in per_object {
        results.extend(r);
        captured_orbits.extend(c);
        orbit_comparisons.extend(p);
    }

    OdValidationOutput {
        results,
        captured_orbits,
        orbit_comparisons,
    }
}

/// Propagate a native CoordinateState (any representation) to
/// `target_epoch_mjd_tdb` via `ctx.propagate` with the supplied config
/// and capture the result as a [`CapturedOrbit`] tagged with `source`.
/// Covariance is propagated through the STM by the propagator (when
/// the input state carries one).
fn propagate_and_capture(
    ctx: &Context,
    object: &str,
    source: &str,
    source_version: Option<String>,
    native: &CoordinateState,
    target_epoch_mjd_tdb: f64,
    prop_cfg: &empyrean::PropagationConfig,
) -> Result<CapturedOrbit, empyrean::Error> {
    let orbit = empyrean::Orbit::new(*native);
    let target = Epoch::from_mjd_tdb(target_epoch_mjd_tdb);
    let result = ctx.propagate(&[orbit], &[target], prop_cfg)?;
    let propagated = result.states.first().ok_or_else(|| empyrean::Error {
        code: -4,
        message: "propagation returned no states (AGM mixture-only return or empty result)"
            .to_string(),
        // Only a strict-offline context construction populates this; a
        // propagation that returned no states names no absent files.
        missing_data_files: Vec::new(),
    })?;
    let propagated_coord = propagated_state_to_coord(propagated);
    capture_orbit(ctx, object, source, source_version, &propagated_coord)
}

/// Promote an `empyrean::PropagatedState` (OD output shape) to a full
/// [`CoordinateState`] so it can flow through `ctx.transform`. fit's
/// OD output is always Cartesian.
pub(crate) fn propagated_state_to_coord(orbit: &empyrean::PropagatedState) -> CoordinateState {
    CoordinateState {
        epoch: orbit.epoch,
        elements: [
            orbit.position[0],
            orbit.position[1],
            orbit.position[2],
            orbit.velocity[0],
            orbit.velocity[1],
            orbit.velocity[2],
        ],
        covariance: orbit.covariance,
        // 0.11 `CoordinateState` is state-only (6×6); the state↔parameter
        // border now lives on the engine-side packed joint, not on the input
        // state, so there is no cross block to carry across this round-trip.
        representation: Representation::Cartesian,
        frame: orbit.frame,
        origin: orbit.origin,
    }
}

/// Transform any source's native orbit + covariance into the three
/// canonical views used by the orbit-comparison panel: the native view
/// (as-supplied), Sun-centered ICRF Cartesian, and Sun-centered
/// ecliptic-J2000 Keplerian. Covariance is propagated through the
/// Jacobian by `ctx.transform`.
pub(crate) fn capture_orbit(
    ctx: &Context,
    object: &str,
    source: &str,
    source_version: Option<String>,
    native: &CoordinateState,
) -> Result<CapturedOrbit, empyrean::Error> {
    let epoch_mjd_tdb = native.epoch.mjd_tdb().unwrap_or(f64::NAN);
    let native_repr = match native.representation {
        Representation::Cartesian => "cartesian",
        Representation::Keplerian => "keplerian",
        Representation::Cometary => "cometary",
        Representation::Spherical => "spherical",
    }
    .to_string();
    let native_frame = match native.frame {
        Frame::ICRF => "icrf",
        Frame::EclipticJ2000 => "ecliptic_j2000",
        Frame::ITRF93 => "itrf93",
    }
    .to_string();
    let native_origin_naif = native.origin.naif_id();

    // Sun-centered ICRF Cartesian. `transform` split into a batch form and
    // this single-state one at 0.10; one state at a time is what this
    // per-object capture has, and the single form is the same computation.
    let cart_sun = ctx.transform_coordinates_single(
        native,
        Representation::Cartesian,
        Frame::ICRF,
        Origin::SUN,
    )?;

    // Sun-centered ecliptic-J2000 Keplerian.
    let kep_sun = ctx.transform_coordinates_single(
        native,
        Representation::Keplerian,
        Frame::EclipticJ2000,
        Origin::SUN,
    )?;

    Ok(CapturedOrbit {
        object: object.to_string(),
        source: source.to_string(),
        source_version,
        epoch_mjd_tdb,
        native_repr,
        native_frame,
        native_origin_naif,
        native_state: native.elements,
        native_cov_6x6: native.covariance,
        state_cart_icrf_sun: cart_sun.elements,
        cov_cart_icrf_sun_6x6: cart_sun.covariance,
        state_kep_ecliptic_sun: kep_sun.elements,
        cov_kep_ecliptic_sun_6x6: kep_sun.covariance,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    // Only the pin test reads this row constant; it names the row the
    // projection must NOT read.
    use empyrean::SENSITIVITY_ROW_RANGE;

    /// A `[6][n_params]` block whose every row is a constant, distinct
    /// value — row `r` is filled with `r + 1` — so the projection's
    /// output identifies which rows it read. Columns at and beyond index
    /// 6 (present only on wide chains) are poisoned: they have no
    /// counterpart in the 6×6 state covariance and must never be
    /// projected.
    fn row_labelled_jacobian(n_params: usize) -> Vec<f64> {
        const POISON: f64 = 1.0e6;
        let mut j = vec![0.0; 6 * n_params];
        for r in 0..6 {
            for c in 0..n_params {
                j[r * n_params + c] = if c < 6 { (r + 1) as f64 } else { POISON };
            }
        }
        j
    }

    fn identity6() -> [[f64; 6]; 6] {
        let mut c = [[0.0; 6]; 6];
        for (i, row) in c.iter_mut().enumerate() {
            row[i] = 1.0;
        }
        c
    }

    /// With C_in = I₆ and every entry of row `r` equal to `r + 1`, the
    /// quadratic form over rows `a`, `b` is `6·(a+1)·(b+1)`. That makes
    /// the expected 2×2 a closed-form function of WHICH rows were read,
    /// which is the whole point of the fixture.
    fn expect_from_rows(row_a: usize, row_b: usize, dec_rad: f64) -> [[f64; 2]; 2] {
        let cosd = dec_rad.cos();
        let a2 = 3600.0_f64 * 3600.0;
        let q = |x: usize, y: usize| 6.0 * ((x + 1) as f64) * ((y + 1) as f64);
        [
            [
                q(row_a, row_a) * cosd * cosd * a2,
                q(row_a, row_b) * cosd * a2,
            ],
            [q(row_a, row_b) * cosd * a2, q(row_b, row_b) * a2],
        ]
    }

    fn assert_close(got: [[f64; 2]; 2], want: [[f64; 2]; 2]) {
        for i in 0..2 {
            for j in 0..2 {
                let rel = (got[i][j] - want[i][j]).abs() / want[i][j].abs().max(1.0);
                assert!(
                    rel < 1e-12,
                    "[{i}][{j}]: got {}, want {}",
                    got[i][j],
                    want[i][j]
                );
            }
        }
    }

    /// The contract this whole test module exists for (empyrean-9666l):
    /// the six Jacobian rows are `[range, RA, Dec, v_range, v_RA, v_Dec]`,
    /// so the sky-plane projection reads rows 1 and 2. Reading rows 0 and
    /// 1 — the defect — publishes a range/RA covariance under the
    /// `emp_radec_cov_arcsec2` name, in AU·deg rather than deg², and
    /// nothing about the resulting number looks wrong on inspection.
    #[test]
    fn sky_covariance_projects_the_ra_and_dec_rows_not_range() {
        let dec_rad = 0.4;
        let j = row_labelled_jacobian(6);
        let got = project_sky_covariance(&j, 6, &identity6(), dec_rad).expect("6-column block");

        assert_close(
            got,
            expect_from_rows(SENSITIVITY_ROW_RA, SENSITIVITY_ROW_DEC, dec_rad),
        );

        // And explicitly NOT the rows the defect read.
        let wrong = expect_from_rows(SENSITIVITY_ROW_RANGE, SENSITIVITY_ROW_RA, dec_rad);
        assert!(
            (got[0][0] - wrong[0][0]).abs() > 1.0,
            "projection matched the (range, RA) rows — the empyrean-9666l defect is back"
        );
    }

    /// A wide chain strides by `n_params`, not by 6. If the stride were
    /// hard-coded the rows would silently shift, and the poisoned
    /// non-state columns would land in the quadratic form.
    #[test]
    fn sky_covariance_strides_by_n_params_on_a_wide_chain() {
        let dec_rad = -0.9;
        for n_params in [6usize, 9, 12] {
            let j = row_labelled_jacobian(n_params);
            let got = project_sky_covariance(&j, n_params, &identity6(), dec_rad)
                .unwrap_or_else(|| panic!("n_params={n_params} block"));
            assert_close(
                got,
                expect_from_rows(SENSITIVITY_ROW_RA, SENSITIVITY_ROW_DEC, dec_rad),
            );
        }
    }

    /// Rows the projection does not read cannot influence it — the
    /// range and rate rows are inert even when they dominate the block.
    #[test]
    fn sky_covariance_ignores_the_range_and_rate_rows() {
        let dec_rad = 0.1;
        let mut j = row_labelled_jacobian(6);
        for r in [SENSITIVITY_ROW_RANGE, 3, 4, 5] {
            for c in 0..6 {
                j[r * 6 + c] = 1.0e9;
            }
        }
        let got = project_sky_covariance(&j, 6, &identity6(), dec_rad).expect("6-column block");
        assert_close(
            got,
            expect_from_rows(SENSITIVITY_ROW_RA, SENSITIVITY_ROW_DEC, dec_rad),
        );
    }

    /// A block that cannot carry the Dec row is declined, not indexed
    /// past the end.
    #[test]
    fn sky_covariance_declines_a_block_too_short_or_too_narrow() {
        let dec_rad = 0.0;
        let full = row_labelled_jacobian(6);

        // Two rows only — enough for the old (rows 0,1) read, not for
        // this one. Declining is the correct answer.
        assert!(project_sky_covariance(&full[..12], 6, &identity6(), dec_rad).is_none());
        // Fewer than 6 state columns: the quadratic form has no 6-vector
        // to contract the covariance against.
        assert!(
            project_sky_covariance(&row_labelled_jacobian(5), 5, &identity6(), dec_rad).is_none()
        );
    }

    /// The engine-delivered ephemeris covariance is ordered
    /// (rho, RA, Dec, vrho, vRA, vDec) in (AU, deg): the sky block is rows
    /// and columns 1 (RA) and 2 (Dec), RA scaled by cos²δ, Dec unscaled,
    /// deg² → arcsec². The range diagonal (row 0) must never surface — the
    /// same hazard the projection pin guards, now on the
    /// delivered read.
    #[test]
    fn delivered_sky_covariance_reads_the_ra_dec_block_scaled_by_cosd() {
        let mut c = [[0.0f64; 6]; 6];
        c[0][0] = 7.0; // rho — must be ignored
        c[1][1] = 2.0; // RA, deg²
        c[2][2] = 3.0; // Dec, deg²
        c[1][2] = 0.5; // RA/Dec cross, deg²
        c[2][1] = 0.5;
        let dec_rad = 60.0_f64.to_radians(); // cosδ = 0.5
        let cosd = dec_rad.cos();
        let a2 = 3600.0_f64 * 3600.0;
        let got = delivered_sky_covariance(&c, dec_rad);
        let close = |x: f64, y: f64| (x - y).abs() / y.abs().max(1.0) < 1e-12;
        assert!(close(got[0][0], 2.0 * cosd * cosd * a2), "RA·cosδ variance");
        assert!(close(got[1][1], 3.0 * a2), "Dec variance");
        assert!(close(got[0][1], 0.5 * cosd * a2), "RA/Dec cross");
        assert!(close(got[1][0], got[0][1]), "symmetry");
        // The range diagonal (7.0 deg²·a2) must not land anywhere.
        assert!(got[0][0] < 7.0 * a2, "range row leaked into the sky block");
    }

    /// A first-order / f64 ephemeris row PUBLISHES the harness projection —
    /// the pinned first-order golden — and records the engine's delivered
    /// covariance only as a diagnostic; switching the published value on
    /// these rows waits on a measured table. Every other method publishes the
    /// delivered covariance, with the projection as the fallback.
    #[test]
    fn first_order_ephemeris_sky_covariance_is_the_projection_not_the_delivered() {
        use empyrean_validation::schema::uncertainty_modes as um;
        let proj = [[1.0, 0.0], [0.0, 2.0]];
        let deliv = [[9.0, 0.0], [0.0, 16.0]];

        // First-order: the projection is published; the delivered covariance
        // is a diagnostic only (its σ difference against the projection).
        let (published, diag) =
            published_sky_covariance(um::FIRST_ORDER_DETECTION_ON, Some(proj), Some(deliv));
        assert_eq!(published, Some(proj));
        assert!(
            diag.is_some(),
            "the delivered-vs-projection σ diagnostic is recorded"
        );

        // A non-first-order method publishes the delivered covariance, no
        // diagnostic.
        let (published, diag) =
            published_sky_covariance(um::SECOND_ORDER_DETECTION_ON, Some(proj), Some(deliv));
        assert_eq!(published, Some(deliv));
        assert!(diag.is_none());

        // A non-first-order method with no delivered covariance falls back to
        // the projection.
        let (published, _) =
            published_sky_covariance(um::SIGMA_POINT_DETECTION_ON, Some(proj), None);
        assert_eq!(published, Some(proj));

        // none row carries no delivered covariance: the projection (itself
        // None without an attached covariance) is published, no diagnostic.
        let (published, diag) = published_sky_covariance(um::NONE_DETECTION_ON, None, None);
        assert_eq!(published, None);
        assert!(diag.is_none());
    }

    /// `resolved_method` is the tag of the covariance KIND the engine
    /// delivered, on every covariance-bearing row. An `auto` row reports the
    /// rung auto chose; an honoured explicit row reports its own tag; a
    /// silent substitution reports the DELIVERED kind, never the request, so
    /// the cross-channel compare catches it; a row with no covariance
    /// resolves to nothing.
    #[test]
    fn resolved_method_reports_the_delivered_kind_on_every_covariance_row() {
        use empyrean_validation::schema::uncertainty_modes as um;
        // Auto resolves to the delivered rung.
        assert_eq!(
            resolved_method_for(um::AUTO, Some(CovarianceKind::SecondOrder)),
            Some(um::SECOND_ORDER.to_string())
        );
        assert_eq!(
            resolved_method_for(um::AUTO, Some(CovarianceKind::Linear)),
            Some(um::FIRST_ORDER.to_string())
        );
        assert_eq!(
            resolved_method_for(um::AUTO, Some(CovarianceKind::Mixture)),
            Some(um::GAUSSIAN_MIXTURE.to_string())
        );
        assert_eq!(resolved_method_for(um::AUTO, None), None);
        // An honoured explicit request reports its own tag (the delivered
        // kind equals the request) — the purity check the compare pairs with
        // `propagation_uncertainty`.
        assert_eq!(
            resolved_method_for(um::FIRST_ORDER, Some(CovarianceKind::Linear)),
            Some(um::FIRST_ORDER.to_string())
        );
        assert_eq!(
            resolved_method_for(um::SECOND_ORDER, Some(CovarianceKind::SecondOrder)),
            Some(um::SECOND_ORDER.to_string())
        );
        // A silent substitution — explicit SecondOrder requested but Linear
        // delivered — reports the Linear tag, never the request, so the
        // cross-channel compare flags the mismatch.
        assert_eq!(
            resolved_method_for(um::SECOND_ORDER, Some(CovarianceKind::Linear)),
            Some(um::FIRST_ORDER.to_string())
        );
        // No covariance delivered → nothing resolved.
        assert_eq!(resolved_method_for(um::FIRST_ORDER, None), None);
    }

    /// With covariance attached a non-close-approach object is swept under
    /// the six production uncertainty methods; a close-approach object adds a
    /// seventh, `gaussian_mixture` (the mixture plan rows the runner
    /// would otherwise never produce). With covariance dropped only the
    /// covariance-free f64 method runs, close-approach or not. The ephemeris
    /// seam iterates the same axis, so a method present here is a method the
    /// ephemeris path now produces a row for (this is what the removed
    /// non-first-order ephemeris skip used to suppress for all but the first
    /// two).
    #[test]
    fn every_method_is_swept_under_covariance_and_only_f64_without() {
        use empyrean_validation::schema::uncertainty_modes as um;
        // Non-close-approach object: the six production surfaces, no mixture.
        let tags: Vec<&str> = build_uncertainty_axes(true, false)
            .iter()
            .map(|a| a.tag)
            .collect();
        assert_eq!(
            tags,
            vec![
                um::FIRST_ORDER,
                um::NONE,
                um::SECOND_ORDER,
                um::AUTO,
                um::SIGMA_POINT,
                um::MONTE_CARLO,
            ]
        );
        // Close-approach object: the same six plus the Gaussian-mixture arm.
        let ca_tags: Vec<&str> = build_uncertainty_axes(true, true)
            .iter()
            .map(|a| a.tag)
            .collect();
        assert_eq!(
            ca_tags,
            vec![
                um::FIRST_ORDER,
                um::NONE,
                um::SECOND_ORDER,
                um::AUTO,
                um::SIGMA_POINT,
                um::MONTE_CARLO,
                um::GAUSSIAN_MIXTURE,
            ]
        );
        assert_eq!(build_uncertainty_axes(true, true).len(), 7);
        assert_eq!(build_uncertainty_axes(true, false).len(), 6);
        // Benchmark mode is the covariance-free f64 row only, close-approach
        // or not (no mixture without covariance).
        for ca in [false, true] {
            let bench: Vec<&str> = build_uncertainty_axes(false, ca)
                .iter()
                .map(|a| a.tag)
                .collect();
            assert_eq!(bench, vec![um::NONE]);
        }
    }

    /// The Monte-Carlo axis carries the one suite-wide seed and sample
    /// count, not `UncertaintyMethod::monte_carlo`'s per-call convenience
    /// seed — the property that makes a seeded Monte-Carlo row a
    /// cross-channel bit check.
    #[test]
    fn monte_carlo_axis_carries_the_suite_seed_and_sample_count() {
        use empyrean_validation::schema::uncertainty_modes as um;
        let axes = build_uncertainty_axes(true, false);
        let mc = axes
            .iter()
            .find(|a| a.tag == um::MONTE_CARLO)
            .expect("monte carlo axis present");
        match mc.method {
            UncertaintyMethod::MonteCarlo { n_samples, seed } => {
                assert_eq!(n_samples, um::MONTE_CARLO_SAMPLE_COUNT as usize);
                assert_eq!(seed, Some(um::MONTE_CARLO_SEED));
            }
            _ => panic!("the monte_carlo axis must carry UncertaintyMethod::MonteCarlo"),
        }
    }

    /// A minimal width-6 derived packed joint for building synthetic mixture
    /// components; only `kind` / `width` / `tri` are read by the code under
    /// test, but every field is set so the value is a real `PackedJoint`.
    fn synthetic_joint(kind: CovarianceKind) -> empyrean::PackedJoint {
        empyrean::PackedJoint {
            layout: empyrean::AxisLayout {
                present: 0,
                considered: 0,
                marginalized: 0,
            },
            width: 6,
            tri: vec![0.0; 6 * 7 / 2],
            kind,
            quality: empyrean::JointQuality::PositiveDefinite,
            functional: empyrean::JointFunctional::State,
            provenance: empyrean::JointProvenance::Derived,
            asserted_source: None,
        }
    }

    fn mix_component(
        weight: f64,
        status: empyrean::propagate::ComponentStatus,
    ) -> empyrean::propagate::MixtureComponent {
        empyrean::propagate::MixtureComponent {
            weight,
            mean: [0.0; 6],
            joint: synthetic_joint(CovarianceKind::Mixture),
            status,
            frame: Frame::ICRF,
            origin: Origin::SSB,
        }
    }

    /// The `cov_kind` the schema carries is the C-ABI wire discriminant, and it
    /// must match the `EMPYREAN_COVARIANCE_KIND_*` tags the core channel also
    /// emits (linear 0, second-order 1, mixture 3, monte-carlo 4, sigma-point
    /// 5). This pins the restated map so it cannot drift from the FFI contract.
    #[test]
    fn cov_kind_wire_matches_the_c_abi_tags() {
        assert_eq!(cov_kind_wire(CovarianceKind::Linear), 0);
        assert_eq!(cov_kind_wire(CovarianceKind::SecondOrder), 1);
        assert_eq!(cov_kind_wire(CovarianceKind::Mixture), 3);
        assert_eq!(cov_kind_wire(CovarianceKind::MonteCarlo), 4);
        assert_eq!(cov_kind_wire(CovarianceKind::SigmaPoint), 5);
    }

    /// The per-orbit outcome channel is the sole delivery discriminator: a
    /// delivered orbit is `delivered`, a delivered orbit whose covariance was
    /// withheld carries the reason by name, and a failed orbit names its
    /// classification — never a row count, never a blank cell. (Mutation: a
    /// call site that set `orbit_delivered` from `states.len()` would report a
    /// delivered-but-empty orbit as not delivered — the property this channel
    /// exists to prevent.)
    #[test]
    fn orbit_outcome_channel_names_delivery_withholding_and_failure() {
        use empyrean::OrbitOutcome;
        let delivered = OrbitOutcome::Delivered {
            first_row: 0,
            num_rows: 1,
        };
        assert_eq!(
            orbit_outcome_channel(&delivered, None),
            (true, "delivered".to_string())
        );
        // Delivered but the covariance was expected and unreadable → withheld,
        // still a delivered orbit, with the reason by name.
        assert_eq!(
            orbit_outcome_channel(&delivered, Some("non-PSD")),
            (true, "cov_withheld:non-PSD".to_string())
        );
        // Failed → not delivered, the EMPYREAN_PROPAGATE_FAILURE_* code named.
        assert_eq!(
            orbit_outcome_channel(
                &OrbitOutcome::Failed {
                    code: 1,
                    message: "integrator step failed".to_string(),
                },
                None,
            ),
            (false, "failed:integration".to_string())
        );
        // An unrecognized code falls back to the engine message, never a bare
        // number.
        assert_eq!(
            orbit_outcome_channel(
                &OrbitOutcome::Failed {
                    code: 77,
                    message: "unexpected".to_string(),
                },
                None,
            ),
            (false, "failed:code_77(unexpected)".to_string())
        );
    }

    /// The six Gaussian-mixture tallies are counted off the retained-component
    /// status table — the 0.11 product — including the sky-linearization-
    /// refused count (status code 4). An unsplit row (no components) leaves
    /// every tally unset, never a fabricated zero. (Mutation: blanking the
    /// tallies, or dropping the sky-refusal arm, fails the counts below.)
    #[test]
    fn mixture_tallies_count_the_retained_component_status_table() {
        use empyrean::propagate::ComponentStatus as CS;
        let components = vec![
            mix_component(0.40, CS::Resolved),
            mix_component(0.30, CS::CurvatureRefused { rho: 2.0 }),
            mix_component(0.10, CS::Unresolved),
            mix_component(0.10, CS::Failed),
            mix_component(0.05, CS::SkyLinearizationRefused { extent: 0.01 }),
        ];
        let mut row = ValidationResult::empty();
        populate_mixture_tallies(&components, &mut row);
        assert_eq!(row.mix_n_components_total, Some(5));
        assert_eq!(row.mix_n_curvature_refused, Some(1));
        assert_eq!(row.mix_n_unresolved, Some(1));
        assert_eq!(row.mix_n_failed, Some(1));
        assert_eq!(row.mix_n_sky_linearization_refused, Some(1));
        let w = row.mix_weight_delivered.expect("weight delivered");
        assert!((w - 0.95).abs() < 1e-12, "summed retained weight");

        // Unsplit row: no components → every tally unset, never a zero.
        let mut blank = ValidationResult::empty();
        populate_mixture_tallies(&[], &mut blank);
        assert_eq!(blank.mix_n_components_total, None);
        assert_eq!(blank.mix_weight_delivered, None);
        assert_eq!(blank.mix_n_sky_linearization_refused, None);
    }

    /// Every OD fit row records the missing OD method axis by name, never a
    /// blank cell — appended after any base note. (Mutation: returning the
    /// base note unchanged drops the marker and fails the asserts below.)
    #[test]
    fn od_fit_rows_record_the_missing_method_axis_by_name() {
        // Empty base → the marker stands alone (never an empty note).
        assert_eq!(
            with_od_method_note(String::new()),
            OD_METHOD_AXIS_NOT_PRODUCED
        );
        // A base note keeps its text and gains the marker.
        let noted = with_od_method_note("optical+radar (50 radar obs)".to_string());
        assert!(noted.starts_with("optical+radar (50 radar obs); "));
        assert!(noted.contains(OD_METHOD_AXIS_NOT_PRODUCED));
        assert!(OD_METHOD_AXIS_NOT_PRODUCED.contains("ODConfig.uncertainty_method"));
    }

    /// A usable `Context` from the local data tier, or `None` to skip — the
    /// same gate the `radar_regression` integration test uses, so a missing
    /// data dir skips rather than fails.
    fn test_ctx() -> Option<Context> {
        match Context::from_data_dir(None) {
            Ok(c) => Some(c),
            Err(e) => {
                eprintln!("SKIP: ephemeris data tier unavailable ({e})");
                None
            }
        }
    }

    /// A synthetic fitted orbit carrying the same typical-NEO prior the
    /// propagation sweep attaches — 1 km position σ, 1 mm/s velocity σ,
    /// uncorrelated, at a valid epoch — the covariance a fit leaves on its
    /// orbit, which the transport then carries.
    fn synthetic_neo_orbit() -> Orbit {
        let pos_var_au = (1.0 / 149_597_870.700_f64).powi(2);
        let vel_var_au_d = (1e-6 / 149_597_870.700_f64 * 86_400.0).powi(2);
        let mut c = [[0.0_f64; 6]; 6];
        c[0][0] = pos_var_au;
        c[1][1] = pos_var_au;
        c[2][2] = pos_var_au;
        c[3][3] = vel_var_au_d;
        c[4][4] = vel_var_au_d;
        c[5][5] = vel_var_au_d;
        let state = CoordinateState {
            epoch: Epoch::from_mjd_tdb(59000.0),
            elements: [1.0, 0.1, 0.05, -0.002, 0.017, 0.001],
            covariance: Some(c),
            representation: Representation::Cartesian,
            frame: Frame::ICRF,
            origin: Origin::SSB,
        };
        Orbit::new(state)
    }

    /// Run the post-fit transport leg against the local data tier for the
    /// synthetic fitted orbit. Returns `None` when the data tier is
    /// unavailable, so the test skips. With the data dir present (the suite's
    /// offline fixtures) the propagation runs for real, so the mutations below
    /// are provably caught.
    fn run_transport_leg_for_test() -> Option<Vec<ValidationResult>> {
        let ctx = test_ctx()?;
        let fitted = synthetic_neo_orbit();
        Some(od_transport_rows(
            &ctx,
            &fitted,
            "rust",
            "SyntheticFit",
            "test",
            ForceModelTier::Standard,
            "standard",
            &[],
            // Close-approach: exercise the full axis including the mixture arm,
            // so the row count equals the widest plan axis.
            true,
            "base note",
            Some("test-engine".to_string()),
            "ts",
        ))
    }

    /// Propagate the synthetic fitted orbit to `target` under the plan axis
    /// whose tag is `tag`, returning the delivered result together with the
    /// axis (so a caller can read `attach`). `None` to skip off a missing data
    /// tier. The config mirrors the propagation sweep (method, frame, tier).
    fn propagate_under_axis(
        tag: &str,
        target: Epoch,
    ) -> Option<(empyrean::propagate::PropagationResult, bool)> {
        let ctx = test_ctx()?;
        let fitted = synthetic_neo_orbit();
        let axes = build_uncertainty_axes(true, true);
        let axis = axes
            .iter()
            .find(|a| a.tag == tag)
            .unwrap_or_else(|| panic!("no {tag} axis in the plan"));
        let config = PropagationConfig {
            force_model: ForceModelTier::Standard,
            uncertainty_method: axis.method.clone(),
            frame: Frame::ICRF,
            ..PropagationConfig::default()
        };
        let result = ctx
            .propagate(std::slice::from_ref(&fitted), &[target], &config)
            .expect("propagation under the requested method");
        Some((result, axis.attach))
    }

    /// The post-fit transport leg emits one `orbit_determination_transport` row
    /// per plan method, each carrying its method tag, and the SecondOrder row
    /// resolves to the SecondOrder kind — the engine ran the requested rung on
    /// the transport (ruling 9), not a silent first-order substitution.
    /// Mutation: drop the method threading (hardcode `FirstOrder` in the leg's
    /// `PropagationConfig`) and the SecondOrder row delivers Linear → resolves
    /// `first_order` → red.
    #[test]
    fn od_transport_emits_one_row_per_method_resolved_to_the_delivered_kind() {
        use empyrean_validation::schema::test_types as tt;
        use empyrean_validation::schema::uncertainty_modes as um;
        let Some(rows) = run_transport_leg_for_test() else {
            return;
        };
        // One row per plan method (close-approach object → the widest axis).
        assert_eq!(rows.len(), build_uncertainty_axes(true, true).len());
        for r in &rows {
            assert_eq!(r.test_type, tt::ORBIT_DETERMINATION_TRANSPORT);
            assert_eq!(r.channel, "rust");
            assert_eq!(r.dt_days, 0.0);
            assert_eq!(
                r.epoch_mjd_tdb, r.t_mjd_tdb,
                "dt = 0: target is the fit epoch"
            );
            assert!(
                r.propagation_uncertainty.is_some(),
                "every row carries its requested method tag"
            );
        }
        let second = rows
            .iter()
            .find(|r| r.propagation_uncertainty.as_deref() == Some(um::SECOND_ORDER_DETECTION_ON))
            .expect("a second_order transport row");
        assert_eq!(
            second.resolved_method.as_deref(),
            Some(um::SECOND_ORDER_DETECTION_ON),
            "SecondOrder transport must deliver the SecondOrder kind, not Linear"
        );
        assert_eq!(
            second.cov_kind,
            Some(cov_kind_wire(CovarianceKind::SecondOrder))
        );
        assert!(second.cov_joint_width.is_some() && second.cov_tri.is_some());
        assert_eq!(second.orbit_delivered, Some(true));
    }

    /// Every transport row names the fit-epoch (dt = 0) target in `notes`, so a
    /// reader never mistakes the degenerate cross-method diagnostic for a
    /// far-epoch transport, and keeps the object's base note. Mutation: blank
    /// `OD_TRANSPORT_FIT_EPOCH_NOTE` (or drop the append) → red.
    #[test]
    fn od_transport_rows_note_the_fit_epoch_transport() {
        let Some(rows) = run_transport_leg_for_test() else {
            return;
        };
        assert!(!rows.is_empty());
        for r in &rows {
            assert!(
                r.notes.contains("transport at the fit epoch"),
                "row notes must name the fit-epoch transport, got {:?}",
                r.notes
            );
            assert!(r.notes.starts_with("base note; "), "the base note is kept");
        }
    }

    /// The per-orbit outcome is read off `outcomes[0]`, never fabricated from
    /// the presence of a row: every transport orbit delivers its state at the
    /// fit epoch (`orbit_delivered = true`), with the status named — a clean
    /// "delivered" on every row, sampling kinds included, because the delivered
    /// covariance is the per-state joint the wrapper carries for every kind
    /// (not the sensitivity-chain accessor that withholds the sampled rows).
    /// The leg emits one row per plan method. Mutation: drop the
    /// `result.outcomes.first()` read (so the outcome channel stays unset) →
    /// `orbit_delivered` / `orbit_status` both `None` → red.
    #[test]
    fn od_transport_outcome_comes_from_outcomes_zero_not_a_row_count() {
        use empyrean_validation::schema::uncertainty_modes as um;
        let Some(rows) = run_transport_leg_for_test() else {
            return;
        };
        assert_eq!(rows.len(), build_uncertainty_axes(true, true).len());
        for r in &rows {
            assert_eq!(
                r.orbit_delivered,
                Some(true),
                "{:?} must deliver its state",
                r.propagation_uncertainty
            );
            assert!(
                r.orbit_status.is_some(),
                "status is named off outcomes[0], never blank"
            );
        }
        // A deterministic (sensitivity-chain) row reads a clean "delivered".
        let first = rows
            .iter()
            .find(|r| r.propagation_uncertainty.as_deref() == Some(um::FIRST_ORDER_DETECTION_ON))
            .expect("a first_order transport row");
        assert_eq!(first.orbit_status.as_deref(), Some("delivered"));
    }

    /// The first-order transport row carries the delivered packed joint read
    /// off the wrapper: a width-`w` joint has exactly `w(w+1)/2` lower-triangle
    /// cells, `cov_kind` is the wire tag, and the position moment view is
    /// populated. Mutation: drop the width / tri reads (leave them `None`) → red.
    #[test]
    fn od_transport_first_order_carries_the_delivered_packed_joint() {
        use empyrean_validation::schema::uncertainty_modes as um;
        let Some(rows) = run_transport_leg_for_test() else {
            return;
        };
        let first = rows
            .iter()
            .find(|r| r.propagation_uncertainty.as_deref() == Some(um::FIRST_ORDER_DETECTION_ON))
            .expect("a first_order transport row");
        assert_eq!(
            first.resolved_method.as_deref(),
            Some(um::FIRST_ORDER_DETECTION_ON)
        );
        assert_eq!(first.cov_kind, Some(cov_kind_wire(CovarianceKind::Linear)));
        let w = first.cov_joint_width.expect("a delivered joint width") as usize;
        assert!(w >= 6, "the state block is present at minimum");
        let tri = first.cov_tri.as_ref().expect("the packed lower triangle");
        assert_eq!(
            tri.len(),
            w * (w + 1) / 2,
            "lower triangle is w(w+1)/2 cells"
        );
        assert!(
            first.emp_pos_cov_au2.is_some(),
            "position moment view populated"
        );
    }

    /// The shared propagation readback ([`read_prop_products`]) — the covariance
    /// surface of both the propagation sweep and the OD transport leg — delivers
    /// the SAMPLING kinds off the per-state packed joint, not `cov_withheld`. A
    /// SigmaPoint propagation delivers a width-6 joint (21-cell lower triangle)
    /// tagged kind 5; a MonteCarlo one tagged kind 4; both read
    /// `orbit_status == "delivered"`. The sensitivity-chain
    /// `covariance_at_cartesian` accessor returns an error for these kinds, so
    /// the pre-fix readback marked them `cov_withheld` — this is the defect that
    /// silently downgraded two methods against the core and python channels.
    /// Mutation: read the chain accessor only in `read_prop_products` (drop the
    /// per-state-joint read) → `cov_joint` `None`, status `cov_withheld:…` → red.
    #[test]
    fn sampled_rows_deliver_the_per_state_joint_not_cov_withheld() {
        use empyrean_validation::schema::uncertainty_modes as um;
        // A non-zero offset — the sweep propagates to real epochs; the sampled
        // covariance is delivered off the state regardless of the offset.
        let target = Epoch::from_mjd_tdb(59_030.0);
        for (tag, want_wire) in [
            (um::SIGMA_POINT, cov_kind_wire(CovarianceKind::SigmaPoint)),
            (um::MONTE_CARLO, cov_kind_wire(CovarianceKind::MonteCarlo)),
        ] {
            let Some((result, attach)) = propagate_under_axis(tag, target) else {
                return;
            };
            let products = read_prop_products(&result, attach);
            let joint = products.cov_joint.as_ref().unwrap_or_else(|| {
                panic!("{tag}: the sampled covariance is delivered off the per-state joint")
            });
            assert_eq!(
                cov_kind_wire(joint.kind),
                want_wire,
                "{tag}: delivered kind"
            );
            assert_eq!(joint.width, 6, "{tag}: state-only joint width");
            assert_eq!(joint.tri.len(), 21, "{tag}: packed 6×6 lower triangle");
            assert_eq!(
                products.resolved_kind.map(cov_kind_wire),
                Some(want_wire),
                "{tag}: resolved kind reads off the delivered joint"
            );
            let (delivered, status) = products
                .outcome_channel
                .unwrap_or_else(|| panic!("{tag}: an outcome off outcomes[0]"));
            assert!(delivered, "{tag}: the orbit delivered its state");
            assert_eq!(status, "delivered", "{tag}: delivered, never cov_withheld");
        }
    }

    /// The post-fit OD transport leg delivers the SAMPLING kinds too: the
    /// SigmaPoint transport row carries its per-state joint (kind 5, width 6, a
    /// 21-cell triangle) with `orbit_status == "delivered"`, not `cov_withheld`
    /// — the leg and the sweep share the readback, so this is the leg-side proof
    /// of the same fix. Mutation: chain-accessor-only in `read_prop_products` →
    /// the SigmaPoint row withholds its covariance → red.
    #[test]
    fn od_transport_sampled_row_delivers_the_per_state_joint() {
        use empyrean_validation::schema::uncertainty_modes as um;
        let Some(rows) = run_transport_leg_for_test() else {
            return;
        };
        let sp = rows
            .iter()
            .find(|r| r.propagation_uncertainty.as_deref() == Some(um::SIGMA_POINT_DETECTION_ON))
            .expect("a sigma_point transport row");
        assert_eq!(
            sp.resolved_method.as_deref(),
            Some(um::SIGMA_POINT_DETECTION_ON),
            "the SigmaPoint transport delivers the SigmaPoint kind"
        );
        assert_eq!(sp.cov_kind, Some(cov_kind_wire(CovarianceKind::SigmaPoint)));
        assert_eq!(sp.cov_joint_width, Some(6));
        assert_eq!(sp.cov_tri.as_ref().map(|t| t.len()), Some(21));
        assert_eq!(sp.orbit_delivered, Some(true));
        assert_eq!(
            sp.orbit_status.as_deref(),
            Some("delivered"),
            "the sampled row delivers, never cov_withheld"
        );
        assert!(
            sp.emp_pos_cov_au2.is_some(),
            "position moment view populated"
        );
    }

    /// Switching the covariance readback from the sensitivity-chain point
    /// accessor (`covariance_at_cartesian`) to the per-state packed joint
    /// (`states[0].joint`) is byte-neutral on a deterministic row: the two are
    /// the SAME delivered covariance read two ways, so their 6×6 state blocks
    /// are bit-identical. This is a within-run cross-accessor identity, not a
    /// value pin — it asserts no SPICE-backed magnitude, only that the two
    /// accessors agree bit-for-bit on whatever the propagation computed. If this
    /// ever diverged, the accessor switch would not be byte-neutral and the
    /// readback change would have to stop; `read_prop_products` reads the joint.
    #[test]
    fn first_order_joint_matches_the_chain_accessor_to_the_bit() {
        use empyrean_validation::schema::uncertainty_modes as um;
        let target = Epoch::from_mjd_tdb(59_030.0);
        let Some((result, attach)) = propagate_under_axis(um::FIRST_ORDER, target) else {
            return;
        };
        let chain = result
            .covariance_at_cartesian(0, 0)
            .expect("the chain accessor delivers on a first-order row");
        let chain_block = chain.matrix();
        let state_joint = result
            .states
            .first()
            .and_then(|s| s.joint.clone())
            .expect("the per-state joint is delivered on a first-order row");
        let state_block = state_joint.state_block();
        for i in 0..6 {
            for j in 0..6 {
                assert_eq!(
                    chain_block[i][j].to_bits(),
                    state_block[i][j].to_bits(),
                    "[{i}][{j}]: chain accessor and per-state joint differ — the readback \
                     switch is not byte-neutral"
                );
            }
        }
        // The shared readback both sites use reads that same delivered joint.
        let read = read_prop_products(&result, attach)
            .cov_joint
            .expect("read_prop_products delivers the joint");
        assert_eq!(
            read.tri, state_joint.tri,
            "read_prop_products reads the per-state joint's triangle"
        );
        assert_eq!(read.width, state_joint.width);
        assert_eq!(
            read.kind,
            chain.kind(),
            "the delivered kind matches the chain accessor's"
        );
    }
}
