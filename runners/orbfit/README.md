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

| Image | `minorplanetcenter/orbfit:latest` |
|-------|-----------------------------------|
| Source | Docker Hub, official MPC org |
| Compressed size | ~3.1 GB |
| Disk size | ~6 GB |
| Maintainer | Federica Spoto et al., IAU Minor Planet Center |
| License | GPL (OrbFit is free software; never linked into empyrean) |

## Status

🚧 **Investigation complete; implementation deferred.** See
[`TODO.md`](TODO.md) for the full investigation summary and the
`neofit2.x` + Cartesian-seed plan that the next session should
pick up.

Short version: OrbFit's binaries (`neocp_prelim.x`, `fitobs.x` Gauss
IOD, `neofit2.x`) all assume a prior orbit exists somewhere — the
MPC workflow chains discovery IOD with subsequent refits. None of
them do blind IOD on a 13-year arc cleanly. The right path is to
write a Cartesian seed at `epoch/<desig>.eq0` from empyrean's plan
row IC (ICRF→ECLM rotation) and drive `neofit2.x` with it.

What's in place (won't break anything in `make all`):

- [x] `setup.sh` — pulls + smoke-tests the container
- [x] `run_orbfit.py` — docker plumbing, scratch-dir setup, `.fel` /
      `.rwo` parsers; **`process_od_row` short-circuits with an
      `orbfit_error` until the seed-writer path is wired**
- [x] Schema extension: `orbfit_*` fields on `ValidationResult`
- [x] CLI extension: `--orbfit` flag on `merge-external`
- [x] Makefile wiring: gated behind `WITH_ORBFIT=1` so the runner
      does not execute as part of `make all`

What's pending (see [`TODO.md`](TODO.md) for the worked-out plan):

- [ ] Cartesian-seed `.eq0` writer (ICRF→ECLM rotation; obliquity
      constants already imported in `run_orbfit.py`)
- [ ] Swap `process_od_row_real` back to `process_od_row` and update
      the docker exec to invoke `neofit2.x < input`
- [ ] Sanity-check against the container's bundled `2021UA12` /
      `3200` NEA test cases before pointing at empyrean's catalog
- [ ] Report integration alongside ASSIST + OpenOrb + find_orb

## Usage (once setup is filled in)

```bash
# One-time: pull the MPC container
./setup.sh

# Per-run: replay every OD row from the canonical plan through OrbFit
python3 run_orbfit.py \
    --plan ../../results/validation_plan.json \
    --output ../../results/validation_orbfit.json
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
