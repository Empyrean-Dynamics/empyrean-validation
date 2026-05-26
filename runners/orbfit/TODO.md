# OrbFit runner — TODO

**Status:** scaffolding in place, **end-to-end fit not yet working**.
Investigation captured below so the next session can pick up without
re-discovering. Runner is gated behind `WITH_ORBFIT=1` in the
top-level Makefile so it does not run as part of `make all`.

## What's confirmed working

- `minorplanetcenter/orbfit:latest` container is the right artifact —
  MPC-maintained, 3.1 GB, source is `github.com/Smithsonian/mpc-orbfit`
- `setup.sh` pulls + smoke-tests the container
- Docker plumbing inside `run_orbfit.py`: scratch dir setup, volume
  mount at `/sa/god_fit/work`, `AST17.{bai,bep}` perturber symlinks,
  PSV observations under `mpcobs/<desig>.psv` are all correct
- `.fel` and `.rwo` output parsers are written and unit-test-clean
- `cargo build --release --bin empyrean-validation` compiles with the
  new `orbfit_*` schema fields + `merge-external --orbfit` flag

## What is NOT yet working

**Producing a real OD fit on empyrean's catalog.** None of the OrbFit
binaries' default workflows handle our use case (known multi-year-arc
NEAs) cleanly without further setup.

## OrbFit's binaries — what each one is actually for

| Binary | Purpose | Seed orbit required? | Status against our catalog |
|--------|---------|---------------------:|----------------------------|
| `neocp_prelim.x` | NEOCP fresh-discovery IOD (days-to-weeks arc) | No (does IOD) | ❌ "arc longer than 1 year, grid will not work" on multi-year arcs; "no good points" on hours-long impactor arcs. Wrong tool for the catalog. |
| `fitobs.x` menu `2` (Gauss IOD) | IOD on selected arc segments + DC | No, in theory | ❌ Filtered all 10 candidate orbits as "bizarre orbits" on 13-year 2010 TK7 arc. Gauss method numerically unstable on long arcs with widely-spaced obs. |
| **`neofit2.x`** | **MPC production NEO refit (`mpc-orbfit/src/neodys`)** | **Yes** (`epoch/<desig>.eq0`) | ✅ Reaches "Processing 2010TK7 number 1" then fails with "cannot OPEN epoch/2010TK7.eq0" |
| `fitobs.x` menu `3` (DC) | Refine existing orbit | Yes (same .eq0) | ✅ Works on bundled `unit_tests/105.rnc` which ships an `epoch/105.eq0` |
| `comets_od.x` | Comet OD | Yes | Not investigated — same architecture as fitobs |

**Conclusion: OrbFit's production workflow assumes a prior orbit always
exists.** The MPC catalog is maintained by chaining `neocp_prelim.x`
(at discovery) → `neofit2.x` (at every subsequent obs update). There is
no "IOD a 13-year arc from scratch" path — that's not how OrbFit is
used.

## The path forward (when the next session picks this up)

**Use `neofit2.x` with a Cartesian seed orbit written from empyrean's
plan row IC.** Steps:

1. **Write a Cartesian seed `epoch/<desig>.eq0` from the plan row's IC**

   The plan row carries `ic_pos_au` + `ic_vel_au_d` (ICRF, AU/AU·day⁻¹)
   and `epoch_mjd_tdb` (MJD TDB). OrbFit needs OEF2.0 format with
   `refsys = ECLM J2000`. Rotation: `R_x(-ε)` with ε = J2000 obliquity
   (IAU 1976, 23.439291° rad). The `_COS_OBL` / `_SIN_OBL` constants
   are already defined in `run_orbfit.py`.

   File format (`<desig>.eq0`):

   ```
   format  = 'OEF2.0'       ! file format
   rectype = 'ML'           ! record type
   refsys  = ECLM J2000     ! default reference system
   END_OF_HEADER
   <desig>
    CAR  <x_ecl> <y_ecl> <z_ecl> <vx_ecl> <vy_ecl> <vz_ecl>
    MJD  <epoch_mjd_tdb>  TDT
    MAG  0.000  0.150
   ```

   The MJD-TDB / MJD-TDT difference is < 1 ms at solar-system scales
   and below OrbFit's time-quantum sensitivity, so the TDB/TDT label
   mismatch is safe in practice.

2. **Replace the runner's docker exec with `neofit2.x`**

   ```python
   cmd = [
       "docker", "run", "--rm", "--platform", "linux/amd64",
       "-v", f"{scratch}:/sa/god_fit/work",
       "minorplanetcenter/orbfit:latest",
       "-c", (
           "cd /sa/god_fit/work && "
           "cp ../unit_tests/neofit.nop.std neofit.nop && "
           "ln -sf ../ast_files/AST17.bai AST17.bai && "
           "ln -sf ../ast_files/AST17.bep AST17.bep && "
           "ln -sf ../bin/neofit2.x . && "
           "./neofit2.x < input"
       ),
   ]
   ```

   Where `scratch` has the layout (mirroring `unit_tests/`):

   ```
   scratch/
   ├── input              (echo <desig> > input)
   ├── mpcobs/
   │   └── <desig>.psv   (or .obs / .ades)
   ├── epoch/
   │   └── <desig>.eq0   ← Cartesian seed (the writer from step 1)
   ├── err/              (mkdir; neofit2 writes warning log here)
   └── war/              (mkdir; neofit2 writes warning detail here)
   ```

   The `.nop` config copied in is `neofit.nop.std` from
   `/sa/god_fit/unit_tests/` — production MPC NEO options
   (gaiaDR2_mix error model, ecclim 0.9999, samin 0.3, samax 4000, 17
   asteroid perturbers including AST17, .ngr_opt off). For non-grav
   cases (e.g. Apophis, Phaethon, 2024 YR4) use `neofit.nop.ngr`
   instead — selectable from the plan row's `ic_a*` populated state.

3. **Parse the post-fit Cartesian state from `epoch/<desig>.eq0_postfit`**

   Same OEF2.0 format as the input; the `CAR` record line has the
   fitted Cartesian + epoch, followed by `RMS`, `EIG`, `WEA`, `COV`,
   `NOR` blocks (same layout as the `.fel` file parser in
   `_parse_orbfit_outputs` already handles).

   For the `merge-external` row population, we want:
     - Cartesian fitted state (already rotated back ECLM → ICRF at
       merge time, OR rotated here so the row carries ICRF)
     - RMS from `mpcobs/<desig>.rwo` header (`RMSast`)
     - Per-obs SEL flags for `n_obs_used` / `n_obs_rejected`

4. **Sanity-check against a bundled test case**

   Use `2021UA12` (NEA) or `3200` (Phaethon, with non-grav) from the
   container's `unit_tests/`. The `test_neofit.py` runs these and
   asserts `rms_ast < 2 arcsec`. If our runner reproduces that on the
   same observations + .eq0 file, we know the harness is correct.
   Then move to empyrean's catalog with empyrean-IC seeds.

## Reference points

- Container source: <https://github.com/Smithsonian/mpc-orbfit>
- OrbFit Consortium: <http://adams.dm.unipi.it/orbfit/>
- IAU MPC: <https://minorplanetcenter.net/>
- Federica Spoto (MPC, primary container maintainer) — ACM 2023:
  <https://www.hou.usra.edu/meetings/acm2023/pdf/2434.pdf>
- Bundled in container: `/sa/god_fit/CHOCHA-PLAN.md` (planned f90wrap
  Python binding for `coo_cha` — not shipped), `/sa/god_fit/PYTHON-PLAN.md`
  (broader Python wrapper plan — also not shipped)

## Estimated effort

- Steps 1–4 above: ~4 hours of focused work on a Linux/amd64 host
  (Apple Silicon's qemu emulation makes each docker run 5–10× slower,
  so do this work on the validation CI machine if possible).
- Add `--platform linux/amd64` automatically in the runner for Apple
  Silicon hosts; native amd64 hosts can drop the flag.

## What's already wired downstream of the runner

These do not block the runner's IOD work — they're ready to consume
the output JSON whenever the runner starts producing one:

- `ValidationResult` schema: `orbfit_rms_arcsec`, `orbfit_n_obs_used`,
  `orbfit_n_obs_rejected`, `orbfit_time_ms` (skip_serializing_if so
  current channel JSONs still round-trip)
- `empyrean-validation merge-external --orbfit <path>` flag +
  `merge_orbfit()` impl in `src/bin/cli.rs`
- `Makefile`: `setup-orbfit`, `run-orbfit` targets; runner in default
  `make run` aggregate gated on `WITH_ORBFIT=1`; `merge-external`
  recipe conditionally folds `--orbfit` when `WITH_ORBFIT=1`
