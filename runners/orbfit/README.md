# OrbFit external-reference runner

External orbit-determination reference for the empyrean validation
suite. Compares empyrean's OD pipeline against OrbFit, the orbit
determination code developed by the OrbFit Consortium (University of
Pisa) and adopted by the IAU Minor Planet Center as their production
fitting pipeline.

OrbFit is particularly valuable as a comparison point because:

- The MPC uses it for the orbital elements catalogues that empyrean
  queries via `query_sbdb` / `query_horizons`. Validating against
  OrbFit directly closes the loop on the catalog → fit → catalog
  round-trip.
- It is the canonical implementation of **Carpino-Milani-Chesley
  (2003) χ²-with-hysteresis rejection**, which empyrean implements as
  `RejectionKind::CMC2003`. Per-row rejection-decision parity against
  OrbFit is a strong test of that path.
- Federica Spoto (MPC) maintains the public Docker image, so the
  runner has a stable upstream to track.

## Container

| Image | `minorplanetcenter/orbfit@sha256:0a72ed7c2c528e0e3fe24a0f280a251f6ab610eb3d5e1d63dbf0f2bd20104f64` (what `:latest` resolved to on 2026-11-09; the digest also pins OrbFit's own data inside the image, `AST17.bai` / `AST17.bep` and the neofit options) |
|-------|-----------------------------------|
| Network | none: every container runs with `--network none` |
| Source | Docker Hub, official MPC org |
| Compressed size | ~3.1 GB |
| Disk size | ~6 GB |
| Maintainer | Federica Spoto et al., IAU Minor Planet Center |
| License | GPL (OrbFit is free software; never linked into empyrean) |

## How it works

OrbFit's production workflow (the one the MPC uses to maintain its NEO
catalog) never does blind IOD on a long arc — it always **refines a
prior orbit** against the astrometry. `neofit2.x` is that refit step.
This runner drives it exactly as the MPC does, but seeds the prior orbit
from **empyrean's initial condition** instead of a previous MPC solution
(a Cartesian-seeded refit, not an independent initial-orbit
determination):

1. **Seed.** Each OD object's IC (Cartesian state at `epoch_mjd_tdb`,
   from the plan's propagation/ephemeris rows — ICRF, SSB-centered) is
   converted to OrbFit's heliocentric ecliptic-J2000 (`ECLM J2000`)
   frame — subtract the Sun's SSB state at the IC epoch, rotate ICRF
   → ecliptic — and written as an OEF2.0 `CAR` record at
   `epoch/<desig>.eq0`. The Sun's state comes from the registered DE440
   (`--de440`, the `orbfit/` folder of the third-party tool set) through
   the shared helper [`../sunstate`](../sunstate/); the plan the suite
   runs carries none, and there is no raw-SSB fallback: the runner
   refuses to start without the file. The SSB→heliocentric shift
   matters: for deep-Earth-approaching NEOs (Apophis's 0.029 AU 2020
   encounter) the ~0.007 AU Sun–SSB offset is a large fraction of the
   encounter distance and, left in, corrupts neofit2.x's two-body
   encounter segments (`ever_pitkin` overflow).
2. **Fit.** `neofit2.x < input` with the MPC's production option file
   (`neofit.nop.std`, or `neofit.nop.ngr` when the plan row carries a
   non-gravitational signal — `ic_a1`/`ic_a2`/`ic_a3` — as for Apophis,
   Phaethon, 2024 YR4). Observations come from the ADES PSV fixtures
   (`mpcobs/<desig>.psv`, ingested directly), fit with the `gaiaDR2_mix`
   error model + CMC2003 outlier rejection.
   The force model holds the planets and the massive asteroids the option
   file names (`.iast=17`, `.filbe='AST17'`): the first 17 entries of the
   image's `ast_files/AST17.bai` (Ceres, Pallas, Juno, Vesta, ... Pluto),
   each named by its number, which the runner reads from the image at
   start. neofit2.x leaves one of them out by its own self-perturbation
   check (`selpert` in `src/propag/force_model.f90`, called at
   `src/neodys/neofit2.f90:277`): the entry whose name equals the name the
   object runs under. So when the plan excludes an AST17 body from the
   object's forces (`excluded_perturbers_naif` names 2000000+n), the object
   runs under that body's name (Vesta as `4`) and the row fails unless
   neofit2.x prints `self perturbation of <n> avoided`; the rows record it
   in `orbfit_left_out_naif`. The decision reads the row only, never the
   designation. An excluded body outside AST17 needs nothing. A row that
   excludes two AST17 bodies cannot run (selpert leaves out only one), so
   it is a named unsupported row with the reason in `orbfit_unsupported`.
   Run under its own name, Vesta perturbed itself: 48 of 3709 observations
   kept at 1.23″; left out, it keeps 3559 at 0.84″.
3. **Parse.** The fitted Cartesian state + covariance from the `CAR`
   record of `epoch/<desig>.eq0_postfit`; the weighted RMS + per-obs
   SEL flags (used/rejected) from `mpcobs/<desig>.rwo`. The state is
   rotated ecliptic → ICRF so emitted rows carry ICRF (heliocentric).

Reproduces the container's own bundled unit tests (`2021UA12`, `3200` —
`rms_ast < 2″`) and fits empyrean's catalog from its ICs (Eros 0.59″,
Apophis with non-gravs 0.50″). Deep-encounter impactors whose arcs run
into Earth (2008 TC3, 2024 BX1, …) can overflow neofit2.x's encounter
propagation; those surface as per-object `orbfit_error`, never a silent
skip.

The runner is **default-on** in `make all` (`WITH_ORBFIT=1`). On Apple
Silicon the amd64-only image runs under qemu, ~5–10× slower per fit
(tens of seconds each) — slow is expected, not a hang; native amd64 CI
is fast.

## Usage

```bash
# One-time: pull the MPC container (~3.85 GB).
./setup.sh

# Per-run: refit every OD row from the canonical plan through neofit2.x.
./run_orbfit.py \
    --plan ../../results/validation_plan.json \
    --output ../../results/validation_orbfit.json \
    --psv-dir ../../fixtures/psv

# Test subset (a few objects, e.g. a plain NEO + a non-grav one):
./run_orbfit.py --plan ../../results/validation_plan.json \
    --output /tmp/orbfit_test.json --only Eros,Apophis
```

The output JSON is consumed by `empyrean-validation merge-external
--orbfit` and then fed to the HTML report alongside the other channel
JSONs.

## Coverage

OrbFit's natural domain is **orbit determination** (per-arc fitting
from astrometry). The runner emits `orbfit_*` fields on the OD rows
only; propagation and ephemeris rows are skipped, matching the
pattern of `run_findorb.py` (find_orb is also OD-only here).

For propagation comparison, the validation suite uses **ASSIST** as
the external N-body reference. For ephemeris, the JPL Horizons
reference baked into the plan is the comparison point.

## License + attribution

OrbFit is GPL-3.0 licensed. This runner shells out to the upstream
container; OrbFit is never linked into empyrean's binary distribution.
References:

- Milani et al. (2004), "OrbFit Software System and Manual",
  University of Pisa Celestial Mechanics Group.
- Carpino, Milani & Chesley (2003), "Error statistics of asteroid
  optical astrometric observations", *Icarus* 166, 248-270.
- Minor Planet Center, "OrbFit at the MPC: a new tool to compare
  the MPC orbits to NEODyS, AstDyS and JPL", ACM 2023.
- Container: <https://hub.docker.com/r/minorplanetcenter/orbfit>
- Source: <https://adams.dm.unipi.it/orbfit/>
