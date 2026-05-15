//! Cross-channel validation framework for the empyrean astrodynamics stack.
//!
//! This crate is the contract between every repo in the empyrean tree and
//! the validation pipeline that exercises them:
//!
//! - **Schema** — [`schema`] holds [`ValidationResult`] and [`ValidationPlan`],
//!   the shared row shape that every channel runner emits and every consumer
//!   (report renderer, website, CI gate) deserializes.
//! - **Catalog** — [`catalog`] holds the curated set of test objects (NEAs,
//!   comets, Trojans, KBOs, ISOs, atmospheric impactors) plus their per-object
//!   `dt` grids. Public-facing so the catalog is auditable + community-
//!   contributable.
//! - **Plan generation** — [`plan`] fetches initial conditions from JPL SBDB
//!   and reference values from JPL Horizons, producing the canonical
//!   `validation_plan.json` that every channel runner consumes.
//! - **Report rendering** — [`report`] turns a set of channel result JSONs
//!   into the branded HTML report shipped to the website.
//! - **Compare** — [`compare`] holds the metric helpers (position-error-km,
//!   angular-separation-arcsec, format helpers) shared by every channel
//!   runner so they all measure the same thing the same way.
//!
//! Each empyrean repo wires its own channel runners on top of this crate:
//!
//! - `empyrean-core` — replays the plan in-process via empyrean-core directly
//! - `empyrean` — replays the plan through the four distribution channels
//!   (rust wrapper, python wheel, C ABI, CLI binary)
//! - `empyrean-validation` (this repo) — drives the external-reference
//!   runners under [`runners/`](https://github.com/Empyrean-Dynamics/empyrean-validation/tree/main/runners)
//!   (ASSIST, find_orb, kete) and owns the canonical CI workflow that
//!   coordinates the cross-repo run.

pub mod catalog;
pub mod compare;
pub mod orbit_compare;
pub mod plan;
pub mod report;
pub mod schema;
