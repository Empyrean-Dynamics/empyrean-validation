# adam-core external-reference runner

Standalone runner that exercises the **Asteroid Institute's adam\_core**
orbit-determination stack with the **adam-assist** ASSIST propagator against the
empyrean validation plan. Like every other runner under `runners/`, it imports
no empyrean code and reads nothing from the parent repo except the canonical
plan, the ADES PSV fixtures, and the window manifest.

adam\_core covers all three axes — propagation, ephemeris, and orbit
determination — through one Rust-native pipeline, and its OD pipeline is the one
the Institute ships for its own walk-forward studies. That makes it the
closest-to-upstream reference for empyrean's uncertainty-era OD work.

## The channel (local-only)

One channel, tool id `adamcore`: **adam-core with its own shipped defaults**,
`run_od(models="default")` on the Asteroid Institute's observatory-uncertainty
tables. The model stack is

> `SigmaFill(v2_sigma_fill)` → `EFCC18DebiasModel` →
> `EmpiricalCovariance(v2_full, add)` → `NightBatchDeweightingModel(cap=4)`

with fitter `NativeOrbitFitter(ASSISTPropagator())` (Gauss IOD → staged
differential correction, `outlier_rejection="cmc2003"`) and
`propagator=ASSISTPropagator()` with adam-assist's defaults (`epsilon=1e-6,
min_dt=1e-9, adaptive_mode=1, initial_dt=1e-6`) — bit-for-bit the suite's
`assist_asteroid_institute` ASSIST arm. The weight tables come from the
**private** `adam-observatory-uncertainties` package, so **this runner is
local-only**: CI skips it and its results are not published. There is no public
or alternate weighting channel.

Every output row is a plan row plus `adamcore_*` fields; `channel` is `adamcore`.
What a row carries as metadata — and nothing more — is:

- `adamcore_models` — the model-stack NAMES (e.g. `SigmaFillModel(v2_sigma_fill)`).
- `adamcore_version` — the adam\_core version; `adamcore_data_version` — the
  `adam-observatory-uncertainties` package version string.
- `adamcore_force_model` — `planets+SB441-n16`, or `planets-only (…)` for a
  self-perturber (see below).
- `adamcore_sigma_source` — the measured per-observation weight provenance:
  `reported` / `filled` with per-level COUNTS in `filled_by_level`
  (station / catalog / global) / `defaulted_by_adamcore` / `total`.

**No sigma value or table row from the private tables ever leaves the venv onto a
row** — only model names, the data-package version string, and fill *counts*.

### What each model does

- **SigmaFillModel** (`v2_sigma_fill`) — fills *only missing* per-observation
  sigmas from a (station, catalog) lookup with a global fallback. Reported sigmas
  (rows that carry `rmsRA`/`rmsDec`) are left **untouched**; the runner counts
  reported vs filled-by-level per fit (`adamcore_sigma_source`), never the values.
- **EFCC18DebiasModel** — removes the Eggl–Farnocchia–Chesley–Chamberlin (2018)
  star-catalog position biases, from JPL's public `bias.dat` (fetched once by
  `setup.sh`, SHA-256 verified — see Pins).
- **EmpiricalCovarianceModel** (`v2_full`) — adds the per-station empirical
  residual covariance to each observation's covariance.
- **NightBatchDeweightingModel(cap=4)** — inflates sigmas by `sqrt(N/4)` for more
  than 4 observations from a station in one night.

(`models=None` with missing sigmas is refused by `run_od`, so the fill table is
required; it is the Institute's private `v2_sigma_fill`, which is why the runner
is local-only. A paper-transcribed public VFC2017 table was prototyped earlier and
is retained for reference outside the runner at
`.claude/drafts/adam-core-runner/vfc2017_sigma_table.csv`; it is NOT used here.)

## Self-perturbers (planets-only force model)

Four objects in the catalog — **Iris, Vesta, Pallas, Hygiea** — are themselves in
ASSIST's SB441-n16 massive-asteroid perturber set, so the full force model makes
the object perturb *itself* (a 1/r² self-singularity: ~1e9 km error at 1 yr).
ASSIST has no per-body exclusion, so — exactly as the suite's ASSIST runner does
— the runner integrates these four **planets-only**. The adam-assist route (no
patching): construct `ASSISTPropagator(asteroids_path="<non-existent path>")`,
which makes ASSIST disable asteroid forces (it prints *"(ASSIST) The JPL asteroid
ephemeris file has not been found. Asteroid forces have been disabled."*).
`asteroids_path=None` does **not** disable — it resolves the default SB441-n16.

Every row carries `adamcore_force_model`: `"planets-only (self-perturber;
SB441-n16 includes the object)"` for the four, `"planets+SB441-n16"` for all
others. The planets-only route is **asserted** at run time
(`verify_planets_only_active`): a synthetic belt orbit propagated 10 yr under the
planets-only and the default propagator must diverge (~19 km measured); a silent
revert to the full model would give 0 and raises. It was verified empirically
that adam-assist @ce52ce1e holds its ASSIST ephemeris **per instance** (not
process-shared), so the two force models coexist in one process and the runner
simply selects the propagator per object — propagation, ephemeris, OD and the
walk all route self-perturbers to the planets-only propagator. (The walk's JSONL
records follow the scorer's `deny_unknown_fields` schema, so they cannot carry
`adamcore_force_model`; the self-perturber force model is applied to the fit and
noted here and in the run summary.)

Parity: planets-only propagation of the four self-perturbers matches the ASSIST
runner's own planets-only rows for the same objects
(`assist_asteroid_institute_variational_off` in that runner's output) to within
**0.10 km** worst-case over all dt (68 rows), well under 1 km.

## No hidden fallbacks

Every capability gap is counted on the row, never silently substituted:

- `adamcore_sigma_source = {reported, filled, filled_by_level, defaulted_by_adamcore,
  total}` — the fit's per-observation weight provenance. **adam_core's
  `OrbitDeterminationObservations.from_ades` produces a NaN lon/lat variance for a
  null `rmsRA`/`rmsDec` — it injects no default sigma** (it is documented to require
  a model to fill the sigmas before fitting), so this is counted straight off the
  original observations: `reported` = observations with a finite variance after
  `from_ades` that carried an ADES rms; `filled` = those with a NaN variance, which
  SigmaFill supplies, broken out in `filled_by_level` by the matching table level
  (`station+catalog` / `station` / `catalog` / `global` / `model_fallback_0.75`);
  `defaulted_by_adamcore` = observations with a finite variance but NO ADES rms (an
  injected default). The last is **measured**, not assumed: the runner cross-checks
  each observation's ADES `rmsRACosDec`/`rmsDec` against its post-`from_ades`
  variance, counts any finite-variance / null-rms observation as defaulted, and logs
  a WARNING naming the count (measured 0 on this build — a future silent default
  would surface here, never relabelled as reported). All counts come from the
  observations' covariance, never from the fitter's members (whose
  `original_astrometry` carries a value for every used row).
- `adamcore_n_obs_dropped_spacebased` — space-based / unplaceable stations
  (satellites: `C51`, `C57`, `250`, …) cannot be placed as ground observatories,
  so their rows are dropped from the fit and counted; the ephemeris channel marks
  such observers `adamcore_unsupported="space-based observer"`.
- `adamcore_unsupported="nongrav dt"` — adam\_core's `NonGravitationalParameters`
  carries A1/A2/A3 **and** the g(r) terms (ALN, R0, NM, NN, NK) but has **no**
  Marsden time-delay (DT) term, so an object with a non-zero `ic_non_grav_dt`
  (only 67P in the catalog) is modelled without its DT; the result is still
  emitted, flagged.
- `adamcore_timed_out=true` — an OD fit that exceeds `--timeout-min` (each
  object is fit in its own child process so the timeout is a hard kill).
- `adamcore_converged=false` + an `adamcore_unsupported` reason — a fit that
  `run_od` could not produce (e.g. an impactor whose arc is below the IOD
  `min_arc_length`).

## Usage

```bash
# One-time. Installs the PRIVATE adam-observatory-uncertainties tables via git+ssh
# (required; fails loudly if your SSH key has no access — the runner is local-only).
./runners/adamcore/setup.sh

PY=runners/adamcore/.venv/bin/python

# Propagation / ephemeris / OD (plan in, rows out)
$PY runners/adamcore/run_adamcore.py --input results/validation_plan.json \
    --fixtures-dir fixtures/psv --output results/validation_adamcore.json

# Walk-forward (window manifest in, windows + predictions JSONL out)
$PY runners/adamcore/run_adamcore_walk.py --manifest fixtures/windows.json \
    --profile ci --fixtures-dir fixtures/psv --cov-method sigma-point \
    --out-windows results/validation_adamcore_walk_windows.jsonl \
    --out-predictions results/validation_adamcore_walk_predictions.jsonl

# Tests (inside the venv)
$PY -m pytest runners/adamcore/tests -q
```

`run_adamcore.py` flags: `--objects A,B`,
`--only propagation,ephemeris,orbit_determination`, `--timeout-min` (default 30).

## Pins

| Package | Pin | Notes |
|---|---|---|
| adam\_core | git `3d01b9a4` (PR #217 `od-obs-uncertainty-rust`) | maturin build, needs cargo (rust-version 1.87) |
| adam-assist | git `ce52ce1e` (branch `kk/native-od-propagator`) | ASSIST as a native external propagator (`propagator_capsule` ABI) + STMs; installed with a uv `--override` for its `adam-core>=0.5.7,<0.6` pin |
| adam-observatory-uncertainties | git+ssh `v0.3.0` | PRIVATE / PROPRIETARY; `ADAMCORE_B612=1` only |
| EFCC18 `bias.dat` | JPL public archive | `download_efcc18_bias_table()` verifies the SHA-256 itself (see below) |
| Python | 3.12 (uv) | |

adam-assist's `kk/native-od-propagator` branch exposes ASSIST over adam\_core's
new external-propagator C ABI (`ExternalPropagatorV1`, capsule
`adam_core.propagator.v1`), which is required because adam\_core PR #217 refuses
Python-only propagators for OD. `setup.sh` writes
`.venv/adamcore-versions.json` (package versions + resolved git commits + EFCC18
SHA + data-package version); the runner stamps the adam\_core version
(`adamcore_version`) and the data-package version (`adamcore_data_version`) on
each OD row.

**ASSIST / REBOUND versions**: adam-assist @ce52ce1e exposes **no** Python
ASSIST/REBOUND version (it vendors them natively), so `versions.json`'s `assist`
and `rebound` stay `null`. The underlying versions are the Rust crates in
adam-assist's `rust/adam_assist_rs/Cargo.lock` at the pinned commit — **libassist-sys
1.2.1** (ASSIST) and **librebound-sys 4.6.0** (REBOUND) — which `write_versions.py`
fetches read-only from GitHub and records under `assist_native_crates`.

**EFCC18 SHA-256**: adam\_core's `download_efcc18_bias_table(verify_checksum=True,
the default)` verifies the downloaded `bias.dat` against its own
`EFCC18_BIAS_DAT_SHA256` constant and raises on mismatch — the verification is
adam\_core's, not the runner's. `setup.sh` additionally re-asserts the materialized
`bias.dat` against that same exposed constant and fails loudly on mismatch, and
stamps the measured SHA into `versions.json`.

### Fallback (STOP-1)

If adam\_core `@3d01b9a4` and adam-assist `@ce52ce1e` disagree at the
capsule/ABI or adapter-namespace level, re-run with
`ADAMCORE_REV=e856436c ./setup.sh` (the adam\_core rev adam-assist built its Rust
against) and report the fallback. **Neither package is patched.** On this build
no fallback was needed.

## STOP-step results

Measured 2026-10-16 (macOS, Python 3.12.13, adam\_core 0.5.8 `@3d01b9a4`,
adam-assist 0.4.1 `@ce52ce1e`); see the builder report for the full tables.

- **STOP-1 (capsule/ABI)** — PASS, no fallback. `ASSISTPropagator().native_backend`
  = `NativeBackend(name='adam_assist', version='0.4.1', work_units={'propagator_capsule'})`;
  `NativeOrbitFitter(ASSISTPropagator())` constructs.
- **STOP-2 (sigma-table cross-check vs scott)** — ran during development against a
  paper-transcribed public VFC2017 table (now RETIRED — the single channel uses the
  Institute's private `v2_sigma_fill`, and the prototype CSV lives outside the runner
  at `.claude/drafts/adam-core-runner/vfc2017_sigma_table.csv`). Historical result:
  matched scott on G96/F51/C51; 703/691 differed by epoch convention; 704/G45 were
  scott re-tunings. The paper was the authority; scott was not edited.
- **STOP-3 (propagation parity)** — PASS. 51 propagation rows over Apophis (NEO,
  non-grav), 103P/Hartley 2 (comet, non-grav), 2010 TK7 (Trojan, gravity-only):
  worst |Δ| vs the `assist_asteroid_institute_variational_off` arm = 0.67 km (< 1 km).
  Non-grav A1/A2/A3 + g(r) must be attached — gravity-only propagation of a
  non-grav object disagrees by ~1 km at 1 yr.
- **STOP-4 (OD timing)** — PASS. All three converged, max wall-clock 14.6 min
  (< 20 min): 2026 DA (short-arc NEA) 1.3 s (rχ²=0.83, 5 sigmas filled),
  Holman (MBA) 50.4 s (rχ²=0.38, 125 space-based dropped), 67P (comet) 874.6 s
  (rχ²=65.6, 756 rejected).
- **STOP-5 (walk score + ci estimate)** — PASS. One object's first 5 ci windows
  (sigma-point) produced 98 predictions with 2×2 deg²/native-RA covariance;
  `score-predictions --scoring-law normal` accepted all 98 (98 scored, 5 surface
  cells) with no cov_units/cov_basis error. Monte-carlo confirmed (`default+mc`).
  The full ci-profile walk is **not run**: estimated ≈ 14.6 h (2,795 windows,
  comets/ISOs dominate), well over the ~3 h budget.
