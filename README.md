<img src="docs/empyrean-dynamics-icon.png" width="140" alt="empyrean-validation">

# empyrean-validation
Cross-channel and external-reference validation for the empyrean astrodynamics stack

<a href="https://github.com/Empyrean-Dynamics/empyrean-validation/actions/workflows/rust.yml"><img src="https://github.com/Empyrean-Dynamics/empyrean-validation/actions/workflows/rust.yml/badge.svg" alt="CI"></a>
<a href="https://github.com/Empyrean-Dynamics/empyrean-validation/actions/workflows/validation.yml"><img src="https://github.com/Empyrean-Dynamics/empyrean-validation/actions/workflows/validation.yml/badge.svg" alt="Validation Suite"></a>
<a href="https://github.com/Empyrean-Dynamics/empyrean/releases/tag/v0.11.0-rc.1"><img src="https://img.shields.io/badge/validates-empyrean%200.11.0--rc.1-1a1a2e?style=flat-square" alt="validates empyrean 0.11.0-rc.1"></a>
<br>
<a href="LICENSE"><img src="https://img.shields.io/badge/license-BSD--3--Clause-blue.svg?style=flat-square" alt="License"></a>
<a href="https://doi.org/10.5281/zenodo.21315119"><img src="https://zenodo.org/badge/1225313390.svg" alt="DOI"></a>
<br>
<a href="https://claude.ai"><img src="https://img.shields.io/badge/Built%20with-Claude%20Code-D97757?logo=anthropic&logoColor=white&style=flat-square" alt="Built with Claude Code"></a>
<a href="https://www.empyrean-dynamics.com"><img src="https://img.shields.io/badge/Website-empyrean--dynamics.com-1a1a2e?logo=data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHdpZHRoPSIyNCIgaGVpZ2h0PSIyNCIgdmlld0JveD0iMCAwIDI0IDI0IiBmaWxsPSJub25lIiBzdHJva2U9IndoaXRlIiBzdHJva2Utd2lkdGg9IjIiIHN0cm9rZS1saW5lY2FwPSJyb3VuZCIgc3Ryb2tlLWxpbmVqb2luPSJyb3VuZCI+PGNpcmNsZSBjeD0iMTIiIGN5PSIxMiIgcj0iMTAiLz48bGluZSB4MT0iMiIgeTE9IjEyIiB4Mj0iMjIiIHkyPSIxMiIvPjxwYXRoIGQ9Ik0xMiAyYTE1LjMgMTUuMyAwIDAgMSA0IDEwIDE1LjMgMTUuMyAwIDAgMS00IDEwIDE1LjMgMTUuMyAwIDAgMS00LTEwIDE1LjMgMTUuMyAwIDAgMSA0LTEweiIvPjwvc3ZnPg==&logoColor=white&style=flat-square" alt="Website"></a>
<a href="https://github.com/Empyrean-Dynamics"><img src="https://img.shields.io/badge/GitHub-Empyrean--Dynamics-1a1a2e?logo=github&logoColor=white&style=flat-square" alt="GitHub"></a>

---

empyrean-validation answers one question continuously: **does every way of
reaching the empyrean physics produce the same numbers, and do those numbers
agree with independent implementations and JPL references?**

The live answer is the report:
<https://validation.empyrean-dynamics.com/latest/validation_report.html>.
Cite that URL, not the bare domain, which lists the bucket.

The suite replays one canonical test plan through every distribution channel
of the stack and through independent external tools, and renders everything
into a single HTML report: cross-channel parity at bit and ULP resolution,
accuracy against JPL Horizons, orbit-determination quality against external
fitters, fitted orbit and covariance comparisons (per-element Δ, σ ratios,
Mahalanobis distances), and timing.

## The catalog

The plan covers 50 Solar System objects in 13 populations, each object with
its own time grid. `empyrean-validation list-objects` prints the catalog
(`--only` and `--populations` narrow it).

| Population | Objects |
|------------|---------|
| Near-Earth | Apophis, Bennu, Didymos, Duende, Eros, Toutatis, Farnocchia, 2020 AV2, 2021 PH27 (a general-relativity stress orbit), 2024 YR4 (the 2032 lunar encounter), Phaethon (a rock comet) |
| Short-arc near-Earth | 2026 FQ12, 2026 FO12, 2026 DA |
| Impactors | 2008 TC3, 2023 CX1, 2024 BX1, 2014 AA, 2018 LA |
| Temporarily captured | 2020 CD3, 2024 PT5 |
| Main belt | Lutetia, Nysa, Holman, Parijskij |
| Self-perturbers | Iris, Vesta, Pallas, Hygiea (members of the perturber set, so each fit excludes the body itself) |
| Comets | 67P, 2P/Encke, 103P/Hartley 2, 46P/Wirtanen, 96P/Machholz, C/2023 A3 |
| Centaur | Chiron |
| Jupiter Trojans | Hektor, Patroclus, Achilles |
| Neptune Trojans | 2001 QR322, 2005 TN53, 2008 LC18 |
| Earth Trojans | 2010 TK7, 2020 XL5 |
| Trans-Neptunian | Eris, Makemake, Varuna |
| Interstellar | 1I/'Oumuamua, 2I/Borisov, 3I/ATLAS |

## What it compares

**Distribution channels**: the same physics reached through every binding.
`core` is the reference channel the others are compared against, and the
`rust` runner's pass over the catalog seeds the plan every other channel
replays.

| Channel | Path into the stack | Runner |
|---------|---------------------|--------|
| `core` | the `validate-core` binary, linking empyrean-core directly with no FFI: **the reference channel** | sibling `empyrean-core` |
| `rust` | the `empyrean` wrapper crate | [![rs](https://img.shields.io/badge/rs-B7410E?style=flat-square&logo=rust&logoColor=white)](runners/rust/) |
| `python` | the `empyrean` Python wheel (PyO3) | [![py](https://img.shields.io/badge/py-3776AB?style=flat-square&logo=python&logoColor=white)](runners/python/) |
| `c` | the `libempyrean` C ABI | [![c](https://img.shields.io/badge/c-555555?style=flat-square)](runners/c/) |
| `cli` | the `empyrean-cli` binary | [![cli](https://img.shields.io/badge/cli-1a1a2e?style=flat-square)](runners/cli/) |

**External references**: independent implementations run against the same
plan; their results are folded onto the core channel's rows.

| Tool | Origin | Axes compared | In a default run |
|------|--------|---------------|------------------|
| [JPL](https://ssd.jpl.nasa.gov/) | NASA JPL SSD: Horizons and SBDB, one solution in two views | propagation and ephemeris (Horizons truth); orbit determination: JPL's reported fit quality (normalized rms as reduced χ², n_obs) and the fitted orbit and covariance (SBDB) | always |
| [ASSIST](https://github.com/matthewholman/assist) | Holman et al., ephemeris-driven REBOUND (pinned 1.2.3, rebound 4.6.0) | propagation (f64 and first-order STM modes), timing | yes |
| [find_orb](https://github.com/Bill-Gray/find_orb) | Bill Gray / Project Pluto | orbit determination and fitted orbits; propagation and ephemeris of its own fitted orbit (fit, then propagate); **radar OD** | yes |
| [GRSS](https://github.com/rahil-makadia/grss) | Makadia et al., small-body propagation and OD with radar | propagation, ephemeris, orbit determination (χ², reduced χ², rms, n_obs), **radar OD** (delay and Doppler residuals) | yes |
| [layup](https://github.com/Smithsonian/layup) | Holman / Smithsonian, an ASSIST-backed orbit fitter | orbit determination (χ², reduced χ², n_obs, convergence) | yes (`WITH_LAYUP=` skips it) |
| [OrbFit](http://adams.dm.unipi.it/orbfit/) | OrbFit Consortium / MPC, `neofit2.x` from the MPC's Docker image | orbit determination with CMC2003 rejection, refit from the plan's initial condition | yes (`WITH_ORBFIT=` skips it) |
| [kete](https://github.com/dahlend/kete) | Dar Dahlen | propagation, ephemeris, orbit determination | yes |
| [jorbit](https://github.com/ben-cassese/jorbit) | Ben Cassese, JAX-based | propagation and geocentric ephemeris (gravity only) | yes |
| [OpenOrb](https://github.com/oorb/oorb) | Granvik et al., University of Helsinki | propagation, ephemeris | no (`WITH_OORB=1`): the plan does not carry the Sun state its heliocentric conversion needs |

**JPL is one source of truth in two views.** SBDB supplies the initial
conditions, the fitted orbit and covariance, and JPL's own reported fit
quality (normalized rms, n_obs, radar counts, data arc, condition code);
Horizons supplies the reference states and observed quantities that same
solution propagates to. "Empyrean vs JPL" is therefore a complete comparison
across propagation, ephemeris and orbit determination. ASSIST runs only
first-order variational (STM) mode: it does not encode second-order
derivatives, so empyrean's second-order rows have no external counterpart.

**Uncertainty methods**: the plan carries a method axis so the replay
channels reproduce every way the engine propagates uncertainty. Each row's
`propagation_uncertainty` is a composite `<method>_<arm>` tag. The methods are
`none` (no covariance), first order (STM), second order (STT), `auto` (the
engine resolves the method per epoch), sigma point, seeded Monte Carlo (100
samples) and, on the close-approach objects, a Gaussian mixture; the arm is a
detection or timing arm. Every propagation and ephemeris row is emitted under
every method's `detection_on` arm, and the `detection_off` and
prebuilt-system timing arms are derived from that grid (for the `none` and
first-order methods) by the `arm-plan` command. Orbit determination carries
the axis on two row kinds, the fit and the post-fit transport of its
covariance, under the same methods. The plan is the contract the channels
consume; the external tools stay first-order comparators.

## The gate

`empyrean-validation ci-check` reads the summary the report writes and fails
the run, naming what failed, when:

- a strict channel (`python` and `core` in CI) is absent, or any of its
  compared rows differs from the reference beyond 1e-10. An orbit both sides
  decline is parity; an orbit one side delivers and the other declines (a fit
  refused for its method, or a propagation or ephemeris the engine declined),
  and a pair the comparison cannot judge, are each named by object, axis,
  offset, observer and method.
- a strict channel has no row for a reference row of an axis it produces.
  Each missing row is named the same way. The axes a channel does not produce
  at this pin (the per-method orbit-determination transport rows on python, c
  and cli) are declared in `src/schema.rs`, counted and not required.
- an axis falls below its row floor. On a full-catalog run the floors come
  from the catalog itself (`--catalog-floors`): one `orbit_determination` row
  per object, and one `orbit_determination_radar` row per object with a
  pinned radar fixture. A floor checks rows produced and rows converged, and
  rows compared on a strict channel.
- an object the plan names has no reference row (`--plan`): the reference
  delivered nothing for it, so no other channel's rows for it were compared.
  It fails as `reference leg missing: <objects>`. Rows a channel emits on an
  axis the reference does not run are reported per axis as unreferenced and
  do not fail the run.

## How CI runs it

The [Validation Suite](.github/workflows/validation.yml) workflow runs when a
push to `main` moves the empyrean pin, and on manual dispatch (optionally with
an `objects` subset and a `tier`):

```text
precheck ─► prep ─► reference ×50 ─► assemble ─┬─► empyrean (4 channels × object chunks) ─┐
                    (one leg per object)        │                                         ├─► reduce
                                                └─► external (one leg per tool) ──────────┘
```

- **prep** builds the engine and the harness, warms the shared caches,
  fetches the pinned inputs snapshot into them, and deals the catalog into the
  per-object matrix and the channel chunks
  ([`scripts/channel-chunks.py`](scripts/channel-chunks.py) `split`).
- **reference** runs the rust reference (propagation, ephemeris, orbit
  determination) for one object per leg, so a slow or failing object costs
  only its own leg.
- **assemble** reassembles the per-object shards into the reference, fails
  naming any object whose shard did not arrive, and strips the reference into
  the plan.
- **empyrean** replays the plan through the python, c, cli and core channels,
  one leg per channel per object chunk. Each leg replays only its chunk:
  `make run-<channel> PLAN_PREBUILT=1 OBJECTS=<chunk>` checks the full plan,
  filters it to the chunk's objects (`channel-chunks.py filter`, which prints
  the per-object and total row counts) and hands the runner that slice.
- **external** replays the whole plan through one comparator per leg.
- **reduce** reassembles each channel from its chunks (`channel-chunks.py
  concat`, which refuses a missing or duplicate chunk), folds the external
  references onto the core rows, renders the report, runs the gate, and
  publishes the report from a full-catalog run on `main`.

Every job that replays or renders seals itself from JPL and the MPC
([`scripts/seal-upstream.sh`](scripts/seal-upstream.sh)) before it runs, so
an input missing from the snapshot fails instead of being fetched live. The
manual [`mint-inputs`](.github/workflows/mint-inputs.yml) workflow is the one
path that queries JPL and the MPC. One CI leg reproduces locally, given a
plan in `results/`:

```sh
make run-core PLAN_PREBUILT=1 OBJECTS="Eris,Hektor"   # the core channel over two objects' rows
```

## What lives here

- `src/`: the shared row schema (`ValidationResult` / `ValidationPlan`) every
  runner emits, the object catalog, the plan generator, the orbit and
  covariance comparison kernel, the report renderer and the
  `empyrean-validation` CLI (`empyrean-validation --help`).
- `runners/`: the per-channel runners and the external-reference runners
  (see [runners/README.md](runners/README.md)).
- `scripts/`: the inputs snapshot tool (`inputs.py`), the channel chunking
  tool (`channel-chunks.py`) and the network seal (`seal-upstream.sh`).
- `fixtures/`: the pin of the inputs snapshot (`manifest.json`; the data
  itself is fetched, see [fixtures/README.md](fixtures/README.md)).
- `Makefile`: the orchestrator every local run and every CI job goes through.

## Run it

```sh
make setup     # one-time: external-tool venvs and builds
make build     # every channel and the validation CLI
make run       # replay the plan through every channel and external tool
make report    # the HTML report and the CI summary
make help      # targets and variables
```

`make all OBJECTS="Apophis,Bennu"` runs a subset of the catalog, and
`TIERS=` picks the model tier (`standard` by default). The channel
repositories sit as siblings of this checkout (`../empyrean`,
`../empyrean-core`; `EMPYREAN_ROOT` and `EMPYREAN_CORE_ROOT` point
elsewhere), and the core channel joins the run whenever the
`empyrean-core` sibling is present.

The JPL and MPC inputs (SBDB, Horizons vectors and ephemerides, ADES
astrometry) are never queried during a run: `fixtures/manifest.json` pins one
immutable snapshot in the suite's public bucket, and `make fixtures` fetches
it over plain HTTPS and verifies every sha256. `make plan` strips the rust
reference run over that snapshot into the canonical plan, and
`make mint-inputs` mints a new snapshot into a local scratch tree (it queries
JPL and the MPC; nothing is uploaded).

`cargo test` covers the schema, the catalog, the plan, the comparison kernel,
the report renderer and the gate; it reads the fixture snapshot, so run
`make fixtures` first. `python3 -m pytest scripts/tests` covers the inputs
and chunking scripts.

Want to benchmark another implementation against the same plan? See
[Add a runner](runners/README.md#add-a-runner): external runners are
self-contained (read the plan JSON, emit rows in the shared schema), and the
eight existing ones are working templates.

The harness itself consumes only **public crates**: the published `empyrean`
wrapper supplies the SBDB and Horizons query clients the plan generator uses,
and `hyperjet` supplies the linear-algebra kernels the covariance comparison
uses. It has no private dependencies; the private repositories enter only as
the optional `core` reference channel (built from a sibling `empyrean-core`
checkout when present) and as the things being validated.

## Versioning and local development overrides

This harness is versioned in lockstep with the distribution release it
validates: checking out validation `vX.Y.Z` and running it reproduces the
validation of record for empyrean `X.Y.Z`. The `validates empyrean X.Y.Z`
badge above moves with the same pin bump, so it always states the published
release the committed manifests point at. The committed manifests pin only
published, tagged artifacts: the runners consume `empyrean` from crates.io
(whose `empyrean-sys` downloads the checksum-pinned engine of that release),
and the plan generator's JPL query clients come from the same published
crate. The core reference channel (`validate-core`) builds from the
`empyrean-core` tag of that generation.

Validating unreleased work is a deliberate, **uncommitted** local override;
never commit these:

```toml
# In the relevant Cargo.toml(s), temporarily:
empyrean = { path = "../../../empyrean/empyrean" }   # runner: local wrapper

[patch.crates-io]
empyrean = { path = "../empyrean/empyrean" }          # harness: local wrapper
```

Cargo honors `[patch]` only from the build-root manifest, and a path patch to
a missing sibling breaks every cargo command in that checkout, so keep
overrides scoped to the manifest you are building from and revert them before
committing.

### What the current pins validate

The committed manifests pin **empyrean `0.11.0-rc.1`** (crates.io and PyPI)
and **hyperjet `1.15.0`**, against the **empyrean-core `0.11.1`** reference
channel (villeneuve 1.26.5, scott 1.19.5). Four properties of that
generation shape how a run is set up:

- **Every time crosses the distribution's API with its scale.** The Rust
  surface takes and returns `Epoch` values, the Python tables carry `Epochs`
  sub-tables, and the CLI requires `--scale` on an MJD argument. The suite's
  inputs are MJD TDB throughout, so the rust and cli runners build
  `Epoch::from_mjd_tdb` for every instant the plan carries (the SBDB epoch,
  the offset grid, the Horizons queries, the weighting-layer windows, the
  walk's output epoch) and read `mjd_tdb()` back, so the rows and the result
  files stay MJD TDB. The Python channel's low-level entry points take and
  return MJD TDB arrays.
- **The reference channel is the engine the distribution binds.** The
  workflow reads the `empyrean-core` tag from the pinned distribution's C ABI
  manifest and checks the reference out at that tag, and every channel's
  recorded `source_version` names the engine releases it ran.
- **The C ABI handshake is an equality check.** `EMPYREAN_ABI_VERSION` is
  `1100`; the C-channel runner asserts it at startup against the loaded
  `libempyrean` and refuses to run on a mismatch, because the version encodes
  struct layout: a stale library is not a degraded run but a
  reinterpretation of every struct crossing the boundary. The library, the
  header and the wheel come from the sibling `../empyrean` checkout
  (`EMPYREAN_ROOT`), so that checkout has to be on the release the manifests
  pin.
- **`determine` is batch-first.** It groups observations by ADES object
  identifier and returns one entry per group, so a failed fit arrives as a
  non-delivered *entry* rather than as a raised error. Every channel runner
  reduces that to the single fit its per-object fixture asks for and refuses
  anything else (zero groups, several groups, or a failed one) rather than
  choosing an entry.
