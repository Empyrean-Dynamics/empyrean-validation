# External Reference Runners

Standalone runners that exercise independent astrodynamics
implementations (ASSIST, find_orb, OpenOrb, OrbFit, kete, jorbit, layup)
against the same canonical
validation plan and emit JSON conforming to the
[`ValidationResult`](../src/schema.rs) schema. The empyrean validation
report folds these into a multi-channel comparison so each empyrean
distribution channel can be benchmarked against an independent
reference.

Each runner ships its own `setup.sh` (installs the upstream dependency
into a per-runner virtualenv or build tree) and a single `run_*.py`
entry point. They're intentionally self-contained — none of them
import any empyrean code, none of them reach back into the parent
repository, and each can be invoked directly with `--help` to see its
flags.

## Runners

| Directory | Upstream | License | What it exercises |
|---|---|---|---|
| [`assist/`](assist/) | [ASSIST](https://github.com/matthewholman/assist) (REBOUND-based N-body propagator) | GPL-3.0 | Propagation. ASSIST is GPL and never linked into empyrean — runs in its own venv. |
| [`layup/`](layup/) | [layup](https://github.com/Smithsonian/layup) (Holman / Smithsonian, ASSIST-backed orbit fitter) | MIT | Orbit determination from ADES PSV astrometry. Runs by default (`WITH_LAYUP=` to skip); folds χ² / reduced-χ² / n_obs / convergence onto the OD rows. Runs in its own venv (heavy C-extension + `sorcha`/`jax` dep graph). |
| [`findorb/`](findorb/) | [find_orb](https://github.com/Bill-Gray/find_orb) (Bill Gray) | GPL-2.0 | Orbit determination from MPC astrometry; with `--plan`, its fitted orbit is also propagated by fo itself to the plan's epochs (state vectors + per-site RA/Dec — fit-then-propagate, not an IC replay). Built from source, runs as a CLI subprocess. |
| [`orbfit/`](orbfit/) | [OrbFit](https://adams.dm.unipi.it/orbfit/) (OrbFit Consortium / IAU Minor Planet Center, `neofit2.x`) | GPL-3.0 | Orbit determination — the MPC's production NEO refit. Seeds neofit2.x with empyrean's IC as a heliocentric Cartesian orbit (`epoch/<desig>.eq0`), then runs its differential correction against the ADES astrometry (a *refit from empyrean's seed*, not a blind IOD — OrbFit's workflow always refines a prior orbit). Canonical implementation of CMC2003 χ²-with-hysteresis rejection; folds post-fit RMS + used/rejected counts onto the OD rows. Runs via the MPC's Docker container (`minorplanetcenter/orbfit`, amd64), never linked into empyrean. Default-on (`WITH_ORBFIT=` to skip). |
| [`oorb/`](oorb/) | [OpenOrb](https://github.com/oorb/oorb) (Granvik et al., Fortran orbit-computation library) | GPL-3.0 | Propagation and ephemeris generation — full n-body (Bulirsch–Stoer, planets + Moon + Pluto, relativity; the stock conf's 2-body default is force-patched, and the plan's SSB states are converted to/from OpenOrb's heliocentric convention using the Sun state carried on plan rows). Built from upstream Fortran source via `setup.sh`; the runner shells out to the `oorb` CLI binary. The pip-installable Python wrapper (`pyoorb`) is intentionally not used — it has the same Fortran build dependency anyway and its sdist breaks on Python 3.12. OD skipped (Ranging/LSL doesn't fit per-row replay). |
| [`kete/`](kete/) | [kete](https://github.com/dahlend/kete) (Dahl & friends) | BSD-3-Clause | Propagation, ephemeris generation, and OD. Full Marsden non-grav (A1/A2/A3 + g(r) + dt) via `NonGravModel`; astrometric RA/Dec with an explicit light-time iteration; self-perturbers in kete's massive-asteroid set (Vesta/Pallas/Hygiea) run planets-only. Pure Python (rebuild-from-PyPI). |
| [`jorbit/`](jorbit/) | [jorbit](https://github.com/ben-cassese/jorbit) (Cassese, JAX-based N-body integrator) | GPL-3.0 | Propagation and ephemeris generation (gravity-only — the non-grav signal remains in comet residuals, stated loudly; self-perturbers run the planets-only `gr planets` preset). jorbit is GPL and never linked into empyrean — runs in its own venv (also keeps the JAX dep tree off the rest of the suite). OD skipped. |

## Workflow

The runners assume a [validation plan JSON](../src/plan.rs) has
already been generated (typically by the empyrean rust runner or the
empyrean-validation `plan` subcommand once it exists). Each runner
reads the plan, replays the test cases through its upstream
implementation, and writes its own per-channel JSON.

```bash
# One-time per runner.
./assist/setup.sh
./findorb/setup.sh
./kete/setup.sh
./oorb/setup.sh
./jorbit/setup.sh
./layup/setup.sh

# Run.
./assist/.venv/bin/python assist/run_assist.py   --plan  validation_plan.json --output validation_assist.json
./findorb/install/bin/fo                                                                  # find_orb is invoked from run_findorb.py
./kete/.venv/bin/python   kete/run_kete.py       --input validation_plan.json --output validation_kete.json
python3                   oorb/run_oorb.py       --input validation_plan.json --output validation_oorb.json
./jorbit/.venv/bin/python jorbit/run_jorbit.py   --input validation_plan.json --output validation_jorbit.json
./layup/.venv/bin/python  layup/run_layup.py     ../fixtures/psv           --output validation_layup.json
```

Outputs are merged by the empyrean-validation report renderer into a
single multi-channel HTML page.

## Add a runner

To plug another implementation into the suite, copy the shape of the
existing runners — `kete/` is the simplest full-featured template
(propagation + ephemeris + OD), `jorbit/` the simplest minimal one
(propagation + ephemeris only):

1. **Create `runners/<tool>/`** with a `setup.sh` that installs the
   upstream tool into a self-contained environment (its own venv or
   build tree — GPL tools stay subprocess/venv-isolated and are never
   linked), and a single `run_<tool>.py` entry point. Any language
   works; the contract is JSON in, JSON out.
2. **Read the plan** (`results/validation_plan.json`). Each row is one
   test case: `test_type` is `propagation` (propagate the
   initial condition to `t_mjd_tdb`), `ephemeris` (also compute the
   observed RA/Dec/range for `observer` at that epoch), or
   `orbit_determination` (fit the object's MPC astrometry from
   `fixtures/psv/`). Initial conditions are Cartesian SSB-centered
   ICRF, in au and au/day (`ic_pos_au` / `ic_vel_au_d`), with Marsden
   non-grav parameters (`ic_a1`…`ic_g_k`, `ic_non_grav_dt`) when the
   object has them — no network access needed to replay a row.
   Skip row kinds your tool doesn't support (several runners skip OD).
3. **Emit one JSON array of rows** in the
   [`ValidationResult`](../src/schema.rs) shape, with `channel` set to
   your tool's name and the fields you can't populate left `null`.
   Copy the reference values (`ref_*`) through from the plan row so
   your rows are self-contained.
4. **Render**: pass your output alongside the other channel JSONs —
   `empyrean-validation report --results validation_rust.json
   validation_<tool>.json --output report.html` — and your tool shows
   up as another column in the comparison. The CLI installs with
   `cargo install --git https://github.com/Empyrean-Dynamics/empyrean-validation`
   (no private dependencies).
