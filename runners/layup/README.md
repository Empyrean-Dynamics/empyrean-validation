# layup external-reference runner

External orbit-determination reference for the empyrean validation
suite. Compares empyrean's OD pipeline against
[layup](https://github.com/Smithsonian/layup), Matthew Holman's
(Smithsonian / CfA) orbit-fitting package.

layup is a useful comparison point because:

- It is **ASSIST-backed** — its N-body fit integrates with ASSIST over
  DE441 + the SB441-N16 asteroid set, the same dynamical model the
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

The runner fits every `fixtures/psv/*.psv` fixture with
`layup orbitfit … ADES_psv` and emits, per object, the layup fit's
quality metrics. Radar-augmented OD (`fixtures/psv-radar/`) is out of
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

**Astrometric weighting (missing errors).** The OD fixtures carry blank
`rmsRA` / `rmsDec`. layup never imputes a missing error; its behavior is
set by the weighting mode:

- *stock default* (`--no-weight-data`): a single flat σ from layup's C++
  core on **every** observation — uniform weighting, not comparable to a
  real per-observation OD scheme;
- *Veres 2017* (`--weight-data`): a principled per-observatory σ (Vereš et
  al. 2017 — a per-station lookup, 0.1″–1.5″, refined by epoch / catalog /
  program; default 1.5″/1.0″ for unknown codes);
- *supplied* (layup API only, not the CLI): the per-obs `rmsRA` / `rmsDec`
  directly, but NaN/≤0 rows fall back to the flat default.

Because the fixtures have no reported errors, **this runner defaults to
Veres 2017** (`--weight-data` ON) so layup's χ² is a principled weighted χ²
rather than a flat-σ figure. The mode is recorded per row as
`layup_weighting`. The choice shifts reduced-χ² materially (≈2× on some
short arcs), so it is a deliberate, documented default — pass
`--no-weight-data` for stock layup.

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
`sorcha` from git and `jax`). `setup.sh` clones a pinned layup ref
recursively, builds it into the venv, and runs `layup bootstrap` to
download the ephemeris + reference data.

```bash
./setup.sh                        # pinned ref (LAYUP_REF to override)
```

Prerequisites: python ≥3.11, git, a C compiler, cmake.

## Usage

```bash
# Fit every optical OD fixture through layup.
./.venv/bin/python run_layup.py ../../fixtures/psv \
    --output ../../results/validation_layup.json

# Subset / tune:
#   --only Apophis,Eros     fit only these objects (fixture stems, `/`→`_`)
#   --timeout 900           per-fixture fit timeout (seconds)
#   --ar-data-path DIR      ASSIST+Rebound bootstrap dir (passthrough to --ar)
```

The output JSON is consumed by `empyrean-validation merge-external
--layup`, which folds the `layup_*` fields onto the
`orbit_determination` rows of the reference channel and then feeds the
HTML report.

layup is **opt-in** in the pipeline (gated behind `WITH_LAYUP=1`, like
OrbFit) — it does not run as part of `make all`:

```bash
make setup-layup             # one-time
make run-layup               # produce results/validation_layup.json
make all WITH_LAYUP=1        # fold layup into the merged report
```

## License + attribution

layup is MIT-licensed. This runner shells out to the upstream `layup`
CLI in an isolated venv; no layup code is linked into or imported by
empyrean.

- Source: <https://github.com/Smithsonian/layup>
- Docs: <https://layup.readthedocs.io>
- ASSIST (the force model layup integrates with): Holman et al. (2023),
  *ASSIST: A Fast Ephemeris-based Test-particle Integrator*.
