# External Reference Runners

Standalone runners that exercise independent astrodynamics
implementations (ASSIST, find_orb, OpenOrb, OrbFit, kete, jorbit, layup,
GRSS) against the same canonical
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
| [`layup/`](layup/) | [layup](https://github.com/Smithsonian/layup) (Holman / Smithsonian, ASSIST-backed orbit fitter) | MIT | Orbit determination from ADES PSV astrometry. Runs by default (`WITH_LAYUP=` to skip); folds χ² / reduced-χ² / n_obs / convergence onto the OD rows. Runs layup's Python API at its own best practice: reported sigma kept, missing sigma filled by the shared public Vereš 2017 model ([`sigmafill/`](sigmafill/)), EFCC18 debiasing on, adaptive non-grav (`fit_nongrav="auto"`); every row records its settings, and the walk runner fits at the same ones. Self-perturbers (Iris/Vesta/Pallas/Hygiea) are named unsupported: layup cannot leave a body out of SB441-N16. Runs in its own venv (heavy C-extension + `sorcha`/`jax` dep graph). |
| [`adam-core/`](adam-core/) | [adam_core](https://github.com/B612-Asteroid-Institute/adam_core) + [adam-assist](https://github.com/B612-Asteroid-Institute/adam-assist) (Asteroid Institute; Rust-backed, ASSIST propagation) | MIT (adam_core) / GPL-3.0 (adam-assist, venv-isolated) | Propagation, ephemeris and orbit determination from ADES PSV, plus the walk. One public channel `adam-core`: the released PyPI packages `adam-core==0.7.0` + `adam-assist==0.6.0`, OD through the public `CORE_OD_V1` profile (EFCC18 debias on the public `jpl-debias-2018` table → nightly-batch deweighting) with `ASSISTPropagator`. CORE_OD_V1 has no sigma-fill, so missing per-observation uncertainties are filled by the suite's shared public Vereš 2017 model ([`sigmafill/`](sigmafill/)); no private Asteroid-Institute table is used. Self-perturbers (Iris/Vesta/Pallas/Hygiea) are reported unsupported — adam-assist 0.6.0 has no planets-only force model. |
| [`sunstate/`](sunstate/) | JPL DE440 (NAIF SPK, type 2) | n/a (reads the registered file) | Shared dependency-free helper: the Sun's barycentric state from the DE440 a tool's third-party folder lists, for the runners that convert to heliocentric coordinates (OrbFit, OpenOrb). Identical to SPICE's `spkezr` positions. Not a tool — a shared data-prep helper. |
| [`sigmafill/`](sigmafill/) | Vereš, Farnocchia, Chesley & Chamberlin (2017), Icarus 296 (public) | n/a (paper transcription) | Shared public per-observation astrometric sigma-fill. Fills ONLY missing sigmas, labelled by level (station / catalog / global); a reported sigma is never overwritten. Imported by every runner whose tool refuses a missing uncertainty or would give it a flat default (`adam-core`, `layup`). Not a tool — a shared data-prep helper. |
| [`findorb/`](findorb/) | [find_orb](https://github.com/Bill-Gray/find_orb) (Bill Gray) | GPL-2.0 | Orbit determination from MPC astrometry; with `--plan`, its fitted orbit is also propagated by fo itself to the plan's epochs (state vectors + per-site RA/Dec — fit-then-propagate, not an IC replay). Built from source at the commit set pinned in `setup.sh` (find_orb with its lunar, jpl_eph and sat_code libraries), runs as a CLI subprocess. Runs at find_orb's own documented settings (`findorb_common.py`, shared by the OD and walk runners): EFCC18 debiasing on, Cowell integrator, BC-405 asteroid perturbers with find_orb's own proximity culling and self-exclusion. `bc405.dat`, `bias.dat` and the DE file come from the third-party set's `findorb/` folder, staged into a private config dir; the runner refuses to start without them. Every row states "asteroid perturbers BC-405 (300, proximity-culled), not SB441-N16". |
| [`orbfit/`](orbfit/) | [OrbFit](https://adams.dm.unipi.it/orbfit/) (OrbFit Consortium / IAU Minor Planet Center, `neofit2.x`) | GPL-3.0 | Orbit determination — the MPC's production NEO refit. Seeds neofit2.x with empyrean's IC as a heliocentric Cartesian orbit (`epoch/<desig>.eq0`), then runs its differential correction against the ADES astrometry (a *refit from empyrean's seed*, not a blind IOD — OrbFit's workflow always refines a prior orbit). Canonical implementation of CMC2003 χ²-with-hysteresis rejection; folds post-fit RMS + used/rejected counts onto the OD rows. Runs via the MPC's Docker container (`minorplanetcenter/orbfit`, amd64) pinned by digest, with no network, never linked into empyrean. The heliocentric seed subtracts the Sun's state from the `orbfit/` folder's DE440 ([`sunstate/`](sunstate/)); there is no raw-SSB fallback. Its force model holds the image's 17 AST17 massive asteroids; an AST17 body the plan excludes (`excluded_perturbers_naif`, e.g. Vesta) is left out by OrbFit's own self-perturbation check, the object run under that body's AST17 name, confirmed from neofit2.x's output; a row excluding two AST17 bodies is a named unsupported row (`orbfit_unsupported`). Default-on (`WITH_ORBFIT=` to skip). |
| [`oorb/`](oorb/) | [OpenOrb](https://github.com/oorb/oorb) (Granvik et al., Fortran orbit-computation library) | GPL-3.0 | Propagation and ephemeris generation — full n-body (Bulirsch–Stoer, planets + Moon + Pluto + the 300 BC430 asteroid perturbers, relativity; the stock conf's 2-body default is set to n-body and `perturber.asteroids` on; an object that is itself a BC430 perturber, by its fixture permID (Eros, Lutetia, Nysa, Iris, Vesta, Pallas, Hygiea), is a named unsupported row with the reason in `oorb_unsupported`, as adam-core and layup do; the absence of non-grav is stated per row). Every file it reads comes from the `oorb/` folder of the third-party tool set; the plan's SSB states are converted to/from OpenOrb's heliocentric convention with the Sun from that folder's DE440 ([`sunstate/`](sunstate/)). Built from upstream Fortran source at commit `02278424` via `setup.sh`; the runner shells out to the `oorb` CLI binary. BC430: Baer & Chesley (2017), CC-BY-4.0. The pip-installable Python wrapper (`pyoorb`) is intentionally not used — it has the same Fortran build dependency anyway and its sdist breaks on Python 3.12. OD skipped (Ranging/LSL doesn't fit per-row replay). |
| [`kete/`](kete/) | [kete](https://github.com/dahlend/kete) (Dahl & friends) | BSD-3-Clause | Propagation, ephemeris generation, and OD. Full Marsden non-grav (A1/A2/A3 + g(r) + dt) via `NonGravModel`; astrometric RA/Dec with an explicit light-time iteration; the force model carries the sixteen SB441-N16 asteroid masses on every row (kete's own five, plus the other eleven registered from kete's mass table with their states from `sb441-n16.bsp`), and an object that is one of the sixteen (Iris/Vesta/Pallas/Hygiea) is a named unsupported row with the reason in `kete_unsupported`: kete cannot leave a body out of its own forces. Everything kete loads comes from the `kete/` folder of the third-party tool set, linked into a private `KETE_CACHE_DIR` before kete is imported, so its import makes no network call. Pure Python (rebuild-from-PyPI). |
| [`grss/`](grss/) | [GRSS](https://github.com/rahil-makadia/grss) (Rahil Makadia et al., Gauss-Radau Small-body Simulator) | GPL-3.0 | Propagation, ephemeris **and** orbit determination — the only external reference covering all three axes, and with find_orb one of only two that ingests **radar** astrometry (delay + Doppler), which is what makes radar OD a cross-check instead of a single witness. Reports the suite's only post-fit radar residual RMS. GPL, so — like ASSIST / jorbit / OrbFit — never linked into empyrean: a C++ core behind a Python interface, installed from PyPI into its own venv and driven only as a subprocess. Runs two passes like find_orb: plan-driven (prop + eph + optical OD), then `--radar` over `fixtures/psv-radar/`. Its OD is a **refit seeded from the plan's IC** (like OrbFit, unlike find_orb / layup), holding the plan's Marsden non-grav parameters fixed. `setup.sh` fetches only the DE440-case SPICE kernels (~235 MB) rather than the 4.4 GB set `import grss` would pull, and stamps the kernel version into every row's `source_version`; `GRSS_DE_KERNEL=441` + `--de-kernel 441` opts into the full DE441 set. Known force-model difference, carried on every affected row as `grss_model_note` and re-stated in the runner's summary: GRSS's `NongravParameters` has **no Marsden DT term**, so the plan's `ic_non_grav_dt` is not modelled — 67P, 103P/Hartley 2, 46P/Wirtanen, 2I/Borisov and 3I/ATLAS are compared under a different force model and their offsets are km-to-10⁴-km rather than the 4 m median of the rest of the catalog. |
| [`jorbit/`](jorbit/) | [jorbit](https://github.com/ben-cassese/jorbit) (Cassese, JAX-based N-body integrator) | GPL-3.0 | Propagation and geocentric ephemeris generation (gravity-only — the non-grav signal remains in comet residuals, stated loudly; self-perturbers run the planets-only `gr planets` preset). Runs offline: its DE440 and SB441-N16 come from the `jorbit/` folder of the third-party tool set. Each object replays in a worker process of its own, so the leg's memory is bounded by the largest object, not the catalog; a crashed worker marks only its object's rows with `jorbit_error`. jorbit's only observer model for a ground site is a live Horizons query, so ground-site ephemeris rows are not attempted and each object carries one explicit row saying so. jorbit is GPL and never linked into empyrean — runs in its own venv (also keeps the JAX dep tree off the rest of the suite). OD skipped. |

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
./adam-core/setup.sh                      # installs adam-core 0.7.0 + adam-assist 0.6.0 from PyPI wheels (public; no private data)
./grss/setup.sh

# Fixtures. The ADES PSV astrometry is NOT in the checkout — it lives in the
# GCS snapshot pinned by fixtures/manifest.json. Any runner that reads
# ../fixtures/psv (layup below, find_orb internally) has nothing to fit until
# this has been run and exited 0. `make` targets depend on it automatically;
# the direct invocations below do not.
make -C .. fixtures

# Run.
./assist/.venv/bin/python assist/run_assist.py   --plan  validation_plan.json --output validation_assist.json
./findorb/install/bin/fo                                                                  # find_orb is invoked from run_findorb.py
./kete/.venv/bin/python   kete/run_kete.py       --input validation_plan.json --output validation_kete.json --data-dir ~/.empyrean/data/third-party/kete
./assist/.venv/bin/python oorb/run_oorb.py       --input validation_plan.json --output validation_oorb.json --data-dir ~/.empyrean/data/third-party/oorb --fixtures-dir ../fixtures/psv
./jorbit/.venv/bin/python jorbit/run_jorbit.py   --input validation_plan.json --output validation_jorbit.json --data-dir ~/.empyrean/data/third-party/jorbit
./layup/.venv/bin/python  layup/run_layup.py     ../fixtures/psv           --output validation_layup.json
./adam-core/.venv/bin/python adam-core/run_adam_core.py --input validation_plan.json --fixtures-dir ../fixtures/psv --output validation_adam-core.json
./grss/.venv/bin/python   grss/run_grss.py       --input validation_plan.json --output validation_grss.json
./grss/.venv/bin/python   grss/run_grss.py --radar --input validation_plan.json --output validation_grss_radar.json
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
   `fixtures/psv/`), or `orbit_determination_radar` (fit the
   radar-augmented `fixtures/psv-radar/` — optical table plus an ADES
   `<radar>` table). Note that `strip-plan` removes the radar rows while
   radar OD is rust-only, so a radar pass is driven by the fixture
   directory rather than by plan rows: see `findorb/` and `grss/`.
   Initial conditions are Cartesian SSB-centered
   ICRF, in au and au/day (`ic_pos_au` / `ic_vel_au_d`), with Marsden
   non-grav parameters (`ic_a1`…`ic_g_k`, `ic_non_grav_dt`) when the
   object has them — no network access needed to replay a row.
   Skip row kinds your tool doesn't support (several runners skip OD).
3. **Emit one JSON array of rows** in the
   [`ValidationResult`](../src/schema.rs) shape, with `channel` set to
   your tool's name and the fields you can't populate left `null`.
   Copy the reference values (`ref_*`) through from the plan row so
   your rows are self-contained. Stamp `<tool>_version` (a string) and
   `<tool>_settings` (an object of setting to value, each value saying
   where the tool documents it) on every row, and `<tool>_unsupported` (the
   reason, in the tool's terms) on a row the tool cannot run: `merge-external`
   folds all three onto the merged rows under the tool id, and the report
   renders exactly what was recorded (settings in the Configurations appendix,
   each unsupported reason beside its tool there and on the cell it leaves
   empty).
4. **Render**: pass your output alongside the other channel JSONs —
   `empyrean-validation report --results validation_rust.json
   validation_<tool>.json --output report.html` — and your tool shows
   up as another column in the comparison. The CLI installs with
   `cargo install --git https://github.com/Empyrean-Dynamics/empyrean-validation`
   (no private dependencies).
