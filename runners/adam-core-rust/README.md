# adam-core-rust external-reference runner

Runs the empyrean validation plan through **adam-core's released public Rust
API**: tool id `adam-core-rust`. It is the same adam-core release as the
[`adam-core/`](../adam-core/) channel (the PyPI wheels), reached through its Rust
crates instead of Python, so the suite compares one release through two public
surfaces, the way empyrean itself is run through its core, python, c and cli
channels.

## License

The runner's source is BSD-3-Clause, like the rest of the suite. Its binary
links two Asteroid Institute crates from crates.io: `adam_core` (MIT) and
`adam_assist` (GPL-3.0, which vendors ASSIST and REBOUND, both GPL-3.0). The
binary is therefore GPL-3.0 as built. All of that source is public. The binary
is built and run inside the validation job and is never distributed, and
nothing here is linked into empyrean. This crate has its own `Cargo.lock` and
`[workspace]`, so the suite's crate at the repository root never adopts it.

## What it runs

| Axis | adam-core call |
|---|---|
| propagation | `adam_assist::AssistPropagator::from_paths(de440, sb441-n16)` at its public default IAS15 integrator (epsilon 1e-6, min_dt 1e-9, initial_dt 1e-6, adaptive mode Global, the Python channel's settings), then `Propagator::propagate` on the plan IC (SSB, equatorial, TDB) to `t_mjd_tdb` |
| ephemeris | `AssistPropagator::generate_ephemeris` with `EphemerisOptions::default()`. adam-core's typed ephemeris takes ecliptic Cartesian orbits and observers, so the IC is rotated with adam-core's own `rotate_frame`; observers come from `AdamCoreSpiceBackend::observer_states_from_codes` (ecliptic, Sun-centred, as `Observers.from_codes`) |
| orbit determination | `adam_core::run_od` under the public `CORE_OD_V1` profile (EFCC18 debias, then nightly-batch deweighting with a cap of four), bound to the registered EFCC18 table, with ASSIST as the ABI-v2 provider (`AssistPropagator::external_propagator`) |

adam-core's own crates carry no N-body integrator. Their only built-in
propagator is two-body, which this channel never uses. The Python channel
likewise refuses `run_od(propagator=None)`. `adam_assist` 0.7.0 is the only
released ASSIST provider for `adam_core` 0.8.0 (it requires the `^0.8` crates),
and both adam-core channels run the same release pair.

### OD input

- **ADES PSV.** It is read by adam-core's Rust reader
  (`ades_string_to_observations`) after stripping each cell's fixed-width
  padding, the same normalization the Python channel applies before its reader.
  The Rust reader drops the last character of `obsTime` unconditionally (the
  `Z`), so a padded `...Z   ` would not parse.
- **Stations adam-core cannot place.** These are stations with no ground site
  in the registered observatory table that are also not SPICE bodies: the
  space-based codes C51, C53, C57 and the like. They are dropped and counted
  (`adam_core_rust_n_obs_dropped_spacebased`, `adam_core_rust_dropped_stations`).
- **Missing sigma.** `CORE_OD_V1` has no fill stage and refuses a non-finite
  variance. The runner fills only the missing sigmas from the suite's shared
  public Vereš et al. (2017) model before the fit, through
  [`sigmafill/fill_json.py`](../sigmafill/fill_json.py). That is the same
  `fill_missing` the Python runners import: one implementation, the same labels
  (`adam_core_rust_sigma_source`), and a reported sigma is never overwritten.
- **Observation covariance.** It is built by adam-core's ADES route,
  `ades_angular_covariance_flat`: lon sigma rmsRA / cos(dec), lat sigma rmsDec,
  and a null rmsCorr counts as zero correlation. The Python channel hands its
  `SourceCatalog` the same values (the RA-axis sigma rmsRA / cos(dec), a zero
  null correlation), so both channels weight the observations alike; the
  recorded `observation_covariance` setting names the route.

## Data: the registered set, by explicit path

Every file comes from the `adam-core-rust/` folder of the third-party tool set
(`third-party-tools-2026-11-11`). The names and pinned versions are adam-core's
own (`adam_core_rs_kernel_data::KERNEL_SPECS`):

- **SPICE kernels.** The six default kernels are furnished in `setup_SPICE`
  order: `latest_leapseconds.tls`, `de440.bsp`,
  `earth_200101_990827_predict.bpc`, `earth_620120_240827.bpc`,
  `earth_latest_high_prec.bpc` and `earth_assoc_itrf93.tf`.
- **Observatory table.** `obscodes_extended.json` (mpc-obscodes 2026.4.23).
- **ASSIST pair.** `de440.bsp` and `sb441-n16.bsp`.
- **EFCC18 table.** `bias.dat`, which `CORE_OD_V1` verifies by sha256 and size.

adam-core's resolver is never called. It would look at an environment override,
then probe a `python3`, then `~/.cache`, then fetch a wheel from PyPI. The
runner also sets `ADAM_CORE_KERNEL_OFFLINE=1`, so a stray call fails instead of
fetching. A missing file stops the run, with every missing file named.

The Python channel reads Earth orientation and observatory codes from whatever
PyPI data packages pip resolves at setup. Those versions float: on 2026-11-11
pip resolved naif-eop-high-prec 2026.11.11, naif-eop-historical 2026.8.8,
naif-eop-predict 2026.8.7 and mpc-obscodes 2026.11.7. This channel reads the
versions the crate pins. Each channel records every such file's name, package
version and sha256 in its settings, under the same fields (`earth_orientation`,
`leapseconds`, `obscodes`), so the Configurations appendix shows which files
each read. The Python channel also records `spice_planets_kernel`: its SPICE
reads DE440 from a PyPI package while its ASSIST opens the registered copy (the
same bytes). Here SPICE and ASSIST open the one registered `de440.bsp`
(`planets_kernel`).

The two sets of files differ in coverage:

- **This channel.** The pinned high-precision kernel ends on 2026-07-26 (last
  datum 2026-04-29). After that date, Earth orientation comes from the pinned
  low-accuracy long-term predict, whose last datum is 2024-08-27.
- **The Python channel.** Its predict file's last datum is 2026-08-06.

On the plan's post-2026 ephemeris rows the two channels' topocentric range
differs by up to about 55 m, and RA by up to about 0.1 mas; the geocentre (500)
agrees exactly, and propagation agrees bit for bit (the same ASSIST backend).
This was measured on Achilles, Apophis and 2020 CD3: running this channel on
the Python channel's Earth-orientation files makes every ephemeris row
identical to the Python channel's, and swapping the observatory table alone
changes nothing.

## Recorded on every row

`adam_core_rust_version` (the adam_core version) and `adam_core_rust_settings`
are recorded. The settings are read from the crates and the files, never typed:

- the API and source commit;
- `CORE_OD_V1` with its definition sha256, and its pipeline, IOD and refinement
  declarations as adam-core's canonical JSON;
- the propagator and its integrator settings, and ASSIST's data fingerprint;
- the ephemeris options;
- every registered file with its pinned package version and sha256
  (`planets_kernel`, `asteroid_kernel`, `earth_orientation`, `leapseconds`,
  `obscodes`, `debias_table`);
- the sigma-fill table hash and the observation-covariance route;
- the self-perturber, non-grav and space-based-observer policies;
- the OD wall cap.

`merge-external` folds these onto the merged rows under the tool id, and the
report's Configurations appendix prints them.

## No hidden fallbacks

- **Self-perturbers.** An object whose plan row names a SB441-N16 body in
  `excluded_perturbers_naif` (Iris, Vesta, Pallas and Hygiea in the catalog) is
  a named unsupported row in `adam_core_rust_unsupported` on every axis. The
  decision reads that field, never the object's name. adam_assist hard-codes
  `ASSIST_FORCES_DEFAULT` in a private function, and its `IntegratorConfig`
  carries only dt, min_dt, epsilon and adaptive_mode, so it cannot leave a body
  out of its own forces.
- **Non-grav DT.** adam-core's non-gravitational parameters have no Marsden DT
  term, so a row with a non-zero `ic_non_grav_dt` runs without it and is
  flagged `nongrav dt`.
- **Wall cap.** Each object's fit runs in its own child process under a wall
  cap (`--timeout-min`, default 30, recorded as `od_timeout_min`). A capped
  object is a named row (`adam_core_rust_timed_out`, its reason), never a drop.
  `make` passes `ADAM_CORE_TIMEOUT_MIN`, the one cap both adam-core channels
  take; the Python channel kills its child at the same cap, records it in the
  same words and names a capped object with the same reason.
- **Fits without an orbit.** A refused `run_od` carries adam-core's own error.
  A fit with no orbit carries adam-core's stage, code and message, as an
  unsupported reason. A fit whose refinement did not converge but kept a best
  state is `converged: false` with that state's statistics and adam-core's
  reason in `adam_core_rust_failure`, as the Python channel reports it.
- **Progress.** One progress line per OD object before the fit and one after,
  with its wall time, so a log shows where the time went.

## Usage

```bash
# One-time: cargo build of the pinned crates (Cargo.lock committed).
./runners/adam-core-rust/setup.sh        # binary: runners/adam-core-rust/build/release/run-adam-core-rust

make run-adam-core-rust                    # or, by hand:
runners/adam-core-rust/build/release/run-adam-core-rust \
    --plan results/validation_plan.json --output results/validation_adam-core-rust.json \
    --data-dir "$DATA_DIR/third-party/adam-core-rust" --fixtures-dir fixtures/psv \
    --sigmafill runners/sigmafill/fill_json.py --python python3 \
    [--only propagation,ephemeris,orbit_determination] [--objects A,B] [--timeout-min 30]

# Tests: no kernel needed; the sigma-fill test runs runners/sigmafill through python3.
cd runners/adam-core-rust && cargo test --locked
```

In the workflow, OD is priced per object (`fixtures/external-costs.json`) and
run in object legs. `EXT_LEG` gives the leg's objects to `--objects`, and
reduce concatenates the legs (`scripts/channel-chunks.py concat-external`).

## Pins

| Crate | Pin | Notes |
|---|---|---|
| adam_core | `=0.8.0` (crates.io, MIT) | the release the Python channel runs |
| adam_assist | `=0.7.0` (crates.io, GPL-3.0) | `default-features = false`, so adam-core's resolver feature is off |
| libassist-sys, librebound-sys | `=1.2.1`, `=4.6.0` (via adam_assist) | vendored ASSIST and REBOUND C, compiled by `cc`; no build-time fetch |
