# OpenOrb external-reference runner

Propagation and ephemeris through [OpenOrb](https://github.com/oorb/oorb)
(Granvik et al.), the Fortran orbit-computation library, driven as its `oorb`
CLI in a subprocess. OpenOrb is GPL-3.0 and is never linked into empyrean.

## Build

`setup.sh` clones OpenOrb, checks out the pinned commit `02278424` (master of
2026-08-20) and builds the binary with gfortran (`./configure gfortran opt`,
`make`). It builds nothing else: `make ephem`, which downloads JPL's ASCII
ephemeris, is never run. A failed build fails the script. The commit is
written to `install/COMMIT` and stamped on every row.

## Data: the oorb folder of the third-party tool set

Every file OpenOrb opens comes from the `oorb/` folder of the third-party tool
set (`--data-dir`, the Makefile's `OORB_DATA_DIR`), linked into a private
`OORB_DATA` directory; the runner refuses to start when one is missing:

| File | What it is |
|---|---|
| `de440.dat` | DE440 in OpenOrb's binary form (built by OpenOrb's own `make ephem` from JPL's ASCII DE440) |
| `OBSCODE.dat`, `ET-UT.dat`, `TAI-UTC.dat` | observatory codes and time tables, as shipped in the OpenOrb repository at `02278424` |
| `asteroid_indices.txt`, `asteroid_ephemeris.txt`, `asteroid_masses.txt` | the BC430 asteroid perturbers, read when `perturber.asteroids: T` |
| `de440.bsp` | the Sun's barycentric state for the frame conversion (`runners/sunstate`) |

## Model

The shipped `oorb.conf` with its dynamical model set to n-body (it ships
2-body) and `perturber.asteroids: T`: planets, Moon, Pluto, relativity and
every BC430 asteroid the indices file lists (all 300), integrated with the
shipped Bulirsch-Stoer, with as many oorb processes at once as there are CPUs
(`--jobs`; each call costs about 2 s with the 300 perturbers). Every row runs
that one force model. An object that is itself a BC430 perturber would perturb
itself (measured: Eros run with the asteroids on is 1.8e8 km from Horizons at
p50), and OpenOrb's conf switches the asteroids for every body at once, with no
way to leave one out. So each of its rows is a named unsupported row, never
run: it carries the reason in `oorb_unsupported` ("self-perturber: BC430
includes the object ...") and no position, the rule the adam-core and layup
runners follow. Which objects those are comes from the data: the fixture's
`permID` against the active lines of the indices file. In this catalog they
are seven, Eros, Lutetia, Nysa, Iris, Vesta, Pallas and Hygiea, and
`oorb_settings.self_perturbers` names them. OpenOrb models no
non-gravitational acceleration, so rows whose plan carries A1/A2/A3 say so in
`oorb_model_note` (Apophis drifts to 1.2e5 km after its 2029 encounter).
OpenOrb works in heliocentric coordinates: the plan's barycentric states are
converted with the Sun's state from the folder's `de440.bsp` at the initial
and target epochs, never from the plan. Every row carries `oorb_version` and
`oorb_settings`.

## Credit

The BC430 asteroid ephemerides and masses: Baer, J. & Chesley, S. R. (2017),
"BC430 Asteroid Ephemerides", Zenodo, doi:10.5281/zenodo.3558850, licensed
CC-BY-4.0 (redistribution with attribution).
