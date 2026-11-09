# adam-core external-reference runner

Standalone runner that exercises the **Asteroid Institute's adam\_core**
orbit-determination stack with the **adam-assist** ASSIST propagator against the
empyrean validation plan, **run exactly as it ships publicly**. Like every other
runner under `runners/`, it imports no empyrean code and reads nothing from the
parent repo except the canonical plan, the ADES PSV fixtures, and the window
manifest.

adam\_core covers all three axes — propagation, ephemeris, and orbit
determination — through one Rust-native pipeline, and its OD pipeline is the one
the Institute ships for its own studies. That makes it the closest-to-upstream
reference for empyrean's uncertainty-era OD work.

## The channel

One channel, tool id `adam-core`: **the released PyPI packages
`adam-core==0.8.0` and `adam-assist==0.7.0`**, installed from the published
binary wheels (no cargo build, no git checkout, no private data package).

* **Propagation / ephemeris** go through `adam_assist.ASSISTPropagator` at its
  shipped integrator defaults (`epsilon=1e-6`, `min_dt=1e-9`, `adaptive_mode=1`,
  `initial_dt=1e-6`), the same native ASSIST library the suite's `assist` runner
  uses, on the **registered kernels**: `de440.bsp` and `sb441-n16.bsp` from the
  `adam-core/` folder of the third-party tool set, passed explicitly as
  `planets_path` / `asteroids_path` (the engine set's bytes, by hash). The runner
  refuses to start without them and never falls back to the install-time PyPI
  kernel packages. Those settings are recorded on every row (`adam_core_epsilon`,
  `adam_core_adaptive_mode`), and the propagator is always passed explicitly —
  `run_od(propagator=None)` would silently fall back to adam\_core's built-in
  two-body provider, so the resolved backend is asserted on every row
  (`adam_core_propagator`).
* **Orbit determination** goes through `run_od` under the **public `CORE_OD_V1`
  profile**, bound with `core_od_v1_resource_bindings()`. Its observation
  pipeline is

  > `Efcc18CatalogBiasCorrection` → `NightlyBatchDeweighting(cap=4)`

  on the **public** EFCC18 star-catalog bias table that ships as the
  `jpl-debias-2018` PyPI package. There is **no SigmaFill and no
  empirical-covariance stage**, and the proprietary `ASTEROID_INSTITUTE_OD_V1`
  profile (which fills missing sigma from the private `v2_sigma_fill` table and
  adds the `v2_full` residual-covariance table) is **not used**. No private
  Asteroid-Institute table is installed, imported, or referenced anywhere.

Every output row is a plan row plus `adam_core_*` fields; `channel` is `adam-core`.
An OD row carries the profile (`adam_core_profile = CORE_OD_V1`), the model-stack
NAMES (`adam_core_models`), the adam\_core version (`adam_core_version`), the force
model (`adam_core_force_model`), the propagator assertion (`adam_core_propagator`),
the integrator settings (`adam_core_epsilon`, `adam_core_adaptive_mode`) and the
measured weight provenance (`adam_core_sigma_source`); on a refusal/failure,
adam-core's own reason. The fitted epoch (`adam_core_epoch_mjd_tdb`) is converted
to TDB with adam-core's own `Timestamp.rescale`: a fit comes back in the
observations' UTC.

## Observation uncertainties: adam-core's ADES route

ADES `rmsRA` is the uncertainty of RA times cos(Dec). adam-core 0.8.0's ADES route
for orbit determination (`OrbitDeterminationObservations.from_ades`, whose
covariance is the Rust kernel `ades_angular_covariance_flat`) takes the RA-axis
sigma as `rmsRA / cos(Dec)` and a null `rmsCorr` as a zero correlation. `run_od`
takes a `SourceCatalog`, which reads `ra_sigma` as the RA-axis sigma and a null
correlation as NaN, so the runner hands it those same values. A test pins the
catalog's covariance against `from_ades` itself, and the setting
`observation_covariance` names the route. The `adam-core-rust` channel builds the
same covariance through adam-core's Rust reader.

## Missing uncertainties: the shared public Vereš 2017 sigma-fill

`CORE_OD_V1` has no sigma-fill stage and **requires finite, positive input
astrometric variances**: `run_od` refuses a fit if any observation lacks a
reported uncertainty (naming the private `FillMissingAstrometricUncertainty`
table as the remedy). No catalog object in the suite carries a complete set of
reported per-observation uncertainties, so the runner fills the missing sigmas —
**before** handing the rows to adam-core — from the suite's **shared public**
Vereš, Farnocchia, Chesley & Chamberlin (2017) station model
(`runners/sigmafill`), never adam-core's private table (owner ruling 2026-11-08).
This is the same fill the suite applies to every tool that refuses missing sigma
(layup next), which is why it is a shared helper rather than adam-core-only code.

Discipline (no hidden fallbacks): a **reported** sigma is never overwritten, and
every **filled** observation is labelled by fill level on the OD row —
`adam_core_sigma_source = {reported, filled, filled_by_level: {station, catalog,
no_catalog, catalog_unrecognized}, total, table_sha256}`, with `adam_core_sigma_fill`
naming the model. A catalog code the table does not name takes the conservative
unknown-catalog weight and is labelled `catalog_unrecognized`; there is no invented
global value. See `runners/sigmafill/README.md` for the paper sentence behind each
level (Icarus 296, 139-149; Tables 2-3 + Section 3) and the transcription re-check.
`count_fill_branches.py` counts how many fixture observations hit each branch.

## Recorded on every row

`adam_core_version` (adam-core) and `adam_core_settings`, a name-to-string map read
from the bound profile and the installed packages: the profile and its definition
hash, the observation pipeline, the nightly-batch cap, the debias table and its
hash, the IOD and refinement settings, the propagator and its integrator settings,
the sha256 of each registered kernel, the sigma-fill table hash, the observation
covariance route, the per-object cap (`od_timeout_min`), and the non-grav and
self-perturber policies. adam\_core reads Earth orientation, the leap seconds, the
SPICE planetary kernel and the observatory codes from its PyPI data packages and
accepts no path, so the channel runs them as pip resolved them at setup and records
each file's name, package version and sha256 (`earth_orientation`, `leapseconds`,
`spice_planets_kernel`, `obscodes`), the fields and format the `adam-core-rust`
channel uses for its pinned copies. Those versions float with each setup, and they
are what explains the topocentric difference between the two channels. The row
also states the registered `obscodes_extended.json` hash and whether it is
identical to the installed one, rather than patching the tool. `merge-external`
folds these onto the merged rows and the report's Configurations appendix prints
them.

## Self-perturbers (no planets-only force model in adam-assist 0.7.0)

Four catalog objects — **Iris, Vesta, Pallas, Hygiea** — are themselves in
ASSIST's SB441-n16 massive-asteroid perturber set, so the full force model makes
the object perturb *itself* (a 1/r² self-singularity, ~1e9 km at 1 yr). The
suite's own ASSIST runner avoids this by dropping asteroid forces (a planets-only
`Ephem`). **adam-assist 0.7.0 exposes no equivalent**:

- its `adam_assist.perturbers` module only *detects* self-perturbers (to attach a
  warning); it does not change the force model;
- `asteroids_path=None` resolves the default SB441-n16 (still the full model);
- a non-existent `asteroids_path` now **raises** at construction (strict DAF
  binding — the October runner's missing-file trick is dead; proven at run time
  by `adam_core_common.missing_asteroid_path_raises`);
- pointing `asteroids_path` at a planets kernel constructs but integrates garbage
  (~1e9 km over 10 yr).

So these four objects **cannot** be given a planets-only force model through the
public wrapper. Their propagation, ephemeris and OD rows are emitted as a NAMED
unsupported row (`adam_core_unsupported`, `adam_core_force_model` naming the
reason), never a self-singular state. **This is an open owner decision** (the
options — accept the gap, build a self-excluded SB441 kernel, or ask upstream for
a force mask — are in the builder report), not a settled runner behaviour.

## No hidden fallbacks

Every capability gap is counted on the row, never silently substituted:

- `adam_core_sigma_source` — reported vs filled (by level); the fill is the public
  Vereš 2017 model, never a private table, and a reported sigma is never touched.
- `adam_core_n_obs_dropped_spacebased` — space-based / unplaceable stations
  (satellites: `C51`, `C57`, `250`, …) cannot be placed as ground observatories,
  so their rows are dropped from the fit and counted; the ephemeris channel marks
  such observers `adam_core_unsupported="space-based observer"`.
- `adam_core_unsupported="nongrav dt"` — adam\_core's `NonGravitationalParameters`
  carries A1/A2/A3 **and** the g(r) terms (ALN, R0, NM, NN, NK) but has **no**
  Marsden time-delay (DT) term, so an object with a non-zero `ic_non_grav_dt`
  (only 67P in the catalog) is modelled without its DT; the result is still
  emitted, flagged.
- `adam_core_timed_out=true` with `adam_core_unsupported="timed out after 30 min"`
  and its wall time: an OD fit that reaches the `--timeout-min` cap (each object
  is fit in its own child process, killed at the cap). The log prints a line
  before and after each object, the second with its wall time.
- `adam_core_converged=false` + an `adam_core_unsupported` reason — a fit `run_od`
  could not produce (an IOD/refinement failure carries adam-core's own
  stage/code/message; a self-perturber carries the planets-only reason above).

## Usage

```bash
# One-time. Installs adam-core 0.8.0 + adam-assist 0.7.0 from PyPI wheels.
./runners/adam-core/setup.sh

PY=runners/adam-core/.venv/bin/python
# The registered kernels: the adam-core/ folder of the third-party tool set.
export ADAM_CORE_DATA_DIR=~/.empyrean/data/third-party/adam-core

# Propagation / ephemeris / OD (plan in, rows out)
$PY runners/adam-core/run_adam_core.py --input results/validation_plan.json \
    --fixtures-dir fixtures/psv --output results/validation_adam-core.json

# Walk-forward (window manifest in, windows + predictions JSONL out)
$PY runners/adam-core/run_adam_core_walk.py --manifest fixtures/windows.json \
    --profile ci --fixtures-dir fixtures/psv --cov-method sigma-point \
    --out-windows results/validation_adam-core_walk_windows.jsonl \
    --out-predictions results/validation_adam-core_walk_predictions.jsonl

# Tests (inside the venv)
$PY -m pytest runners/adam-core/tests runners/sigmafill/tests -q
```

`run_adam_core.py` flags: `--data-dir` (default `$ADAM_CORE_DATA_DIR`),
`--registered-obscodes` (compared, not used), `--objects A,B` (an object leg's
objects, refused by name when the plan lacks one),
`--only propagation,ephemeris,orbit_determination`, `--timeout-min` (default 30),
`--epsilon`, `--adaptive-mode` (default to the shipped `1e-6` / `1`).

## Pins

| Package | Pin | Notes |
|---|---|---|
| adam\_core | `0.8.0` (PyPI) | Rust-owned orbit determination, CPython 3.11 to 3.14; wheel-only |
| adam-assist | `0.7.0` (PyPI) | native ASSIST provider, capsule `adam_core.propagator.v2`; wheel-only |
| jpl-debias-2018 | PyPI dep | EFCC18 bias table (`CORE_OD_V1` resource); SHA-256 verified by adam\_core when bound |
| de440.bsp, sb441-n16.bsp | registered third-party set, `adam-core/` | passed to adam-assist as `planets_path` / `asteroids_path`; sha256 recorded on every row |
| naif-eop-* + mpc-obscodes | PyPI deps | Earth orientation and observatory codes read from the installed packages (adam\_core accepts no path for either) |
| Python | 3.12 | |

The kernels come from the registered set; the EFCC18 table, Earth orientation and
observatory codes ship as PyPI packages resolved from site-packages. A fit reads
nothing over the network (install-time downloads from PyPI are fine; a run-time
fetch would be a defect); the unit proof runs it under a sandbox that denies
network access. `setup.sh` writes `.venv/adam-core-versions.json` (package versions, the
adam-assist native backend, every data package's version, and the bound
`CORE_OD_V1` pipeline with the declared-vs-measured EFCC18 SHA-256); the runner
stamps the adam\_core version on each OD row.
