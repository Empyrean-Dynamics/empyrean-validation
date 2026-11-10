# layup external-reference runner

External orbit-determination reference for the empyrean validation
suite. Compares empyrean's OD pipeline against
[layup](https://github.com/Smithsonian/layup), Matthew Holman's
(Smithsonian / CfA) orbit-fitting package.

layup is a useful comparison point because:

- It is **ASSIST-backed** — its N-body fit integrates with ASSIST over
  DE440 + the SB441-N16 asteroid set, the same dynamical model the
  validation suite's ASSIST propagation channel uses. So layup and
  empyrean fit the same physics through independent code, and a
  disagreement isolates the *fitter*, not the force model.
- It reads **ADES PSV** directly — the exact format the suite's OD
  fixtures (`fixtures/psv/`) are in.
- It is MIT-licensed and actively developed for LSST-scale orbit
  fitting.

## What it exercises

Optical **orbit determination** only. layup's natural domain is per-arc
fitting from astrometry; propagation and ephemeris rows are skipped,
matching the pattern of `run_findorb.py` / `run_orbfit.py`. ASSIST is
the external reference for propagation; Horizons for ephemeris.

The runner fits every `fixtures/psv/*.psv` fixture through layup's
Python API (`fit_one.py`, one process per object) and emits, per object,
the layup fit's quality metrics. Radar-augmented OD (`fixtures/psv-radar/`) is out of
scope for this first integration.

## χ², not arcsec RMS

layup reports a post-fit **χ²** (`csq`) and its degrees of freedom, not a
residual RMS in arcsec. Rather than fabricate an arcsec figure from χ²
(which would need the per-observation weights layup does not emit), the
runner folds the quantities layup actually reports:

| Folded field | layup source | empyrean counterpart |
|---|---|---|
| `layup_chi2` | `csq` | `od_chi2` |
| `layup_reduced_chi2` | `csq / ndof` | `od_reduced_chi2` |
| `layup_n_obs_used` | `nobs_fit` | `n_obs_used` |
| `layup_converged` | `flag == 0` | `od_converged` |
| `layup_time_ms` | wall clock | `emp_time_ms` |

**Comparability caveat.** `csq` depends on each tool's observation
weighting, debiasing, and astrometric error model, so it is not strictly
apples-to-apples with empyrean's `od_chi2` (nor find_orb's / OrbFit's
RMS). **reduced-χ²** (≈1 for a consistent fit) is the most tool-agnostic
metric. Likewise `nobs_fit` is layup's own count and may not match
empyrean/find_orb's observation-count convention.

**Settings (layup at its own documented best practice, owner ruling D4).** Every
fit runs through `fit_one.py`, which calls `layup.orbitfit.orbitfit` the way
layup's own `orbitfit_cli` does; the CLI cannot set any of this beyond the
booleans `--weight-data` and `--debias`. Each setting cites layup 1.1.0:

| Setting | Value | Where layup documents it |
|---|---|---|
| `weight_data` | `"supplied"`: each observation's `rmsRA` / `rmsDec` used directly | `orbitfit` docstring: "uses the per-observation `rmsRA` / `rmsDec` columns (arcseconds) directly, e.g. ADES-reported uncertainties" |
| sigma fill | reported sigma kept; a missing one filled by the shared public Vereš 2017 model ([`../sigmafill`](../sigmafill/)) | same docstring: a NaN or nonpositive supplied value "falls back to the default", layup's flat C++ sigma, which the fill keeps any observation from reaching |
| `debias` | `True`: EFCC18 star-catalog debiasing | `orbitfit` docstring ("apply debiasing corrections"); `generate_bias_dict` reads the high-resolution `bias.dat` staged from the registered `debias_hires2018.tgz` |
| `fit_nongrav` | `"auto"` at the default `NongravAutoThresholds` | `orbitfit` docstring and issue #357: the gravity-only fit is kept unless its reduced chi-square exceeds 1.5; otherwise A2, A1, A3, then A1+A2, then A1+A2+A3 are tried, and the most parsimonious model with delta chi-square at least 9 per parameter and every term at 3 sigma is adopted |
| `nongrav_gr` | layup's default, asteroidal (r/r0)^-2, for every object | `orbitfit` docstring: "For a comet, pass `constants.MARSDEN_1973_GOFR`" |
| `iod`, `engine` | `"auto"` (Gauss, BK-IOD fallback), `"cartesian"` | layup's defaults (`orbitfit` signature) |

About a third of the optical rows (58,764 of 183,464) carry a reported sigma; the
rest are filled, and `layup_sigma_source` counts reported, filled, half-reported
and the fill level of each filled observation. layup's flat default and its own
Vereš-table mode (`weight_data=True`) are therefore never used: the suite's runners
share one fill. The identification, photometry and free-text columns the fit
does not use (`provID`, `trkSub`, `prog`, `band`, `photCat`, `notes`, `remarks`)
are dropped from the prepared input, because layup's `np.sort(data,
order="obsTime")` breaks timestamp ties on the remaining fields and a string/NaN
mix there raised `TypeError` on Apophis.

**Model differences, stated on every row.** The force model is ASSIST over DE440
and SB441-N16 from the registered `layup/` folder, the engine set's own bytes. The
non-grav model is layup's, chosen from the data by its auto ladder; the comet
g(r) is not selected, because choosing a law by designation is barred, so a comet
that adopts a non-grav term fits it with the asteroidal law. The adopted terms are
in `layup_nongrav_adopted` (`{}`: gravity only).

**Self-perturbers.** Iris, Vesta, Pallas and Hygiea are themselves in SB441-N16,
and layup's ASSIST force model takes the whole set with no way to leave a body
out, so a fit would pull each toward its own ephemeris. Fitted anyway, none
converges (flag 3 for Vesta and Hygiea, flag 4 for Iris and Pallas). Their rows
are named unsupported
(`layup_converged: false`, `layup_unsupported` with the reason) and never fitted,
the rule the adam-core runner follows for the same reason.

Every row carries `layup_settings` (the table above, `LAYUP_SETTINGS` in
`run_layup.py`) and `layup_version`. A fit that fails or times out is a named row
with `layup_converged: false` and `layup_error`, never a dropped object; a fit
that does not converge keeps layup's own `layup_flag`.

The walk and Monte-Carlo runners (`run_layup_walk.py`, `run_layup_mc_predict.py`)
fit every window through the same driver and the same input adapter, so they run
at the same settings; a warm window passes the previous state as layup's
`initial_guess` (state only).

The fitted barycentric-Cartesian state + epoch are also carried in the
per-object JSON (`layup_state_bcart_eq`, `layup_epoch_mjd_tdb`) for
provenance, but are not folded onto the `ValidationResult` rows — a
raw state diff against another fitter needs a shared output epoch, which
this first pass does not enforce.

## Input handling

The ADES PSV fixtures are space-padded and lead with a `permID` column
that is blank for many comets / recently-designated objects, so layup's
convention (the first column is the consistently-populated primary id)
does not hold out of the box. Because each fixture file is exactly one
object, the runner strips the padding and injects a constant `layupID`
first column, guaranteeing every observation groups into a single fit.

## One fit per subprocess (not layup's native batch)

layup is built to fit **many** objects per process (one input file grouped
by primary id, fanned out over `--num-workers` in `--chunksize` blocks),
amortizing its heavy ~3 s cold start (import jax/numba/sorcha, JIT,
SPICE-kernel load, ASSIST init). This runner deliberately does **not** batch:

- **Individual fit times** — layup's output has no per-object time column,
  so a batch yields only a wall-clock total; per-fixture subprocesses give a
  real `layup_time_ms` per object (the project rule: batch only where
  per-fit times stay calculable — layup can't, so it stays per-object).
- **Crash isolation** — layup fits via `concurrent.futures` +
  `np.concatenate([f.result() …])`, so one bad object (an internal `np.sort`
  type error, a missing-obscode or satellite-`sys` ingest error) aborts the
  whole batch; separate subprocesses contain the failure to that object.

The trade-off is the repeated cold start, so `layup_time_ms` is
cold-subprocess wall clock (startup-dominated), **not** a fit-kernel
benchmark and not comparable to empyrean's warm in-process `emp_time_ms`.
Batching layup — recovering per-object timing, e.g. via an upstream
per-object time column — is tracked as a follow-up.

## Setup

layup runs in its own venv (heavy dep graph: a C extension built via
scikit-build-core against the `assist` + `rebound` C libraries, plus
`sorcha` and `jax`). `setup.sh` installs the pinned PyPI release
(`layup==1.1.0`, released 2026-09-21; layup publishes an sdist only, so
the install compiles the extension), then stages layup's data directory.

```bash
./setup.sh                        # pinned release (LAYUP_VERSION to override)
DATA_DIR=/path/to/layup-folder ./setup.sh  # stage from another data folder
```

Prerequisites: python ≥3.11, a C compiler, cmake.

### Data: staged from the third-party tool set, never downloaded

layup reads every kernel, the observatory codes and its debiasing table
from one directory (`--ar`), under the names its own `layup bootstrap`
downloads, and its fit path downloads any name that is missing, from the
address the release names. Those addresses move: NAIF's renaming of its
dated Earth-orientation kernels made every fit fail with a 404. So the
suite never bootstraps. `stage_data.py` builds the directory (`data/`
here) from the `layup/` folder of the published third-party tool set
(`third-party-tools-2026-11-10` in `gs://empyrean-data`), which lists every
file layup opens (10 files, shared kernels included), so layup needs nothing
from the engine kernel set. Sync it with the bucket's `empyrean-data.py`
(`sync third-party-tools-2026-11-10 $DATA_DIR/third-party`); the Makefile
variable `LAYUP_DATA_DIR` (default `$(DATA_DIR)/third-party/layup`) names the
folder `stage_data.py --data-dir` reads. It takes layup's file names from the
installed layup itself:

| layup reads | staged from the folder |
|---|---|
| `de440s.bsp` | `de440.bsp` (full DE440, a superset of the short file) |
| Earth orientation: historical, predicted, high precision | `earth_620120_*.bpc`, `earth_*_predict.bpc`, `earth_latest_high_prec.bpc` |
| `linux_p1550p2650.440`, `sb441-n16.bsp` (ASSIST) | same names |
| `naif0012.tls`, `pck00010.pck` | `naif0012.tls`, `pck00010.tpc` |
| `ObsCodes.json` (+ `.gz`) | `obscodes_extended.json` (and its gzip) |
| `debias_hires2018.tgz`, `bias.dat` | `debias_hires2018.tgz` (and the table extracted from it) |
| `meta_kernel.txt` | written: loads the data directory's own kernel files |

Each staged file links to exactly one data-directory file; no match, two
matches (a set synced without `--prune`) or an empty file stops staging
with every such file named. The set's own `bias.dat` is the
low-resolution debiasing table and is never staged under layup's name,
which indexes the high-resolution one. `make run-layup` re-stages before
every run (layup removes the staged archive after extracting the table),
and `make run-layup OBJECTS=A,B` fits only those objects. `--ar` moves only
the ASSIST data: layup 1.1 reads `naif0012.tls` and the other SPICE kernels
from its cache directory in several fit paths, so `run_layup.py` also sets
`LAYUP_CACHE_DIR` to the staged directory (a run on a machine whose per-user
cache is empty, such as a CI runner, otherwise fails with "SPICE kernel file
'naif0012.tls' not found"). `run_layup.py` refuses to start when any file layup checks
for is missing from its data directory.

## Usage

```bash
# Fit every optical OD fixture through layup.
./.venv/bin/python stage_data.py --data-dir "$DATA_DIR/third-party/layup" --dest data
./.venv/bin/python run_layup.py ../../fixtures/psv \
    --ar-data-path data --output ../../results/validation_layup.json

# Subset / tune:
#   --only Apophis,Eros     fit only these objects (fixture stems, `/`→`_`)
#   --timeout 900           per-fixture fit timeout (seconds)
#   --ar-data-path DIR      layup's staged data dir (passthrough to --ar; default data/)
```

The output JSON is consumed by `empyrean-validation merge-external
--layup`, which folds the `layup_*` fields onto the
`orbit_determination` rows of the reference channel and then feeds the
HTML report.

layup runs by default in the pipeline (`WITH_LAYUP=1`); `WITH_LAYUP=` skips it:

```bash
make setup-layup             # one-time
make run-layup               # produce results/validation_layup.json
make all WITH_LAYUP=         # the merged report without layup
```

## License + attribution

layup is MIT-licensed. This runner calls layup's Python API in a
subprocess under an isolated venv; no layup code is linked into or
imported by empyrean.

- Source: <https://github.com/Smithsonian/layup>
- Docs: <https://layup.readthedocs.io>
- ASSIST (the force model layup integrates with): Holman et al. (2023),
  *ASSIST: A Fast Ephemeris-based Test-particle Integrator*.
