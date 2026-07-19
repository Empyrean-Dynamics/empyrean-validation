<img src="docs/empyrean-dynamics-icon.png" width="140" alt="empyrean-validation">

# empyrean-validation
Cross-channel and external-reference validation for the empyrean astrodynamics stack

<a href="https://github.com/Empyrean-Dynamics/empyrean-validation/actions/workflows/rust.yml"><img src="https://github.com/Empyrean-Dynamics/empyrean-validation/actions/workflows/rust.yml/badge.svg" alt="CI"></a>
<a href="https://github.com/Empyrean-Dynamics/empyrean-validation/actions/workflows/validation.yml"><img src="https://github.com/Empyrean-Dynamics/empyrean-validation/actions/workflows/validation.yml/badge.svg" alt="Validation Suite"></a>
<a href="https://github.com/Empyrean-Dynamics/empyrean/releases/tag/v0.8.1"><img src="https://img.shields.io/badge/validates-empyrean%200.8.1-1a1a2e?style=flat-square" alt="validates empyrean 0.8.1"></a>
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

It replays one canonical test plan — a curated catalog of 43 solar-system
objects (NEAs including impactors and temporarily-captured objects, comets,
Trojans, KBOs, interstellar objects) with per-object time grids — through
every distribution channel of the stack, runs the same plan through
independent external tools, and renders everything into a single HTML
report: cross-channel parity at bit / ULP resolution, accuracy against JPL
Horizons, orbit-determination quality against external fitters, fitted
orbit + covariance comparisons (per-element Δ, σ ratios, Mahalanobis
distances), and timing.

## What it compares

**Distribution channels** — the same physics reached through every binding;
`core` is the reference channel the others are compared against:

| Channel | Path into the stack | Runner |
|---------|---------------------|--------|
| `core` | `validate-core` binary linking empyrean-core directly (no FFI) — **the reference channel** | sibling `empyrean-core` |
| `rust` | the `empyrean` wrapper crate | [![rs](https://img.shields.io/badge/rs-B7410E?style=flat-square&logo=rust&logoColor=white)](runners/rust/) |
| `python` | the `empyrean` Python wheel (PyO3) | [![py](https://img.shields.io/badge/py-3776AB?style=flat-square&logo=python&logoColor=white)](runners/python/) |
| `c` | `libempyrean` C ABI | [![c](https://img.shields.io/badge/c-555555?style=flat-square)](runners/c/) |
| `cli` | the `empyrean-cli` binary | [![cli](https://img.shields.io/badge/cli-1a1a2e?style=flat-square)](runners/cli/) |

**External references** — independent implementations run against the same
plan; results are folded onto the core channel's rows:

| Tool | Origin | Axes compared |
|------|--------|---------------|
| [JPL](https://ssd.jpl.nasa.gov/) | NASA JPL SSD — Horizons + SBDB (one solution, two views) | propagation, ephemeris (Horizons truth); orbit determination — JPL's reported fit quality (normalized rms → reduced-χ² + n_obs) and fitted orbit + covariance (SBDB) |
| [ASSIST](https://github.com/matthewholman/assist) | Holman et al. — ephemeris-driven REBOUND (pinned 1.2.3 / rebound 4.6.0) | propagation (f64 and first-order STM modes), timing |
| [layup](https://github.com/Smithsonian/layup) | Matthew Holman / Smithsonian — ASSIST-backed orbit fitter (opt-in, `WITH_LAYUP=1`) | orbit determination (χ² / reduced-χ² / n_obs / convergence) |
| [find_orb](https://github.com/Bill-Gray/find_orb) | Bill Gray / Project Pluto | orbit determination, fitted orbits |
| [OrbFit](http://adams.dm.unipi.it/orbfit/) | OrbFit Consortium / MPC (opt-in, `WITH_ORBFIT=1`) | orbit determination (CMC2003 rejection) |
| [OpenOrb](https://github.com/oorb/oorb) | Granvik et al., University of Helsinki | propagation, ephemeris |
| [kete](https://github.com/dahlend/kete) | Dar Dahlen (opt-in) | propagation, ephemeris, OD sanity check |
| [jorbit](https://github.com/ben-cassese/jorbit) | Ben Cassese — JAX-based (opt-in) | propagation, OD sanity check |

**JPL is one source of truth, two views:** SBDB supplies initial conditions,
the fitted orbit + covariance, and JPL's own reported orbit-fit quality
(normalized rms, n_obs, radar counts, data-arc, condition code); Horizons
supplies the reference states and observed quantities that same solution
propagates to. So "Empyrean vs JPL" is a complete comparison across
propagation, ephemeris, and orbit determination in one selection. ASSIST runs
only first-order variational (STM) mode — it does not encode second-order
derivatives, so empyrean's second-order rows have no external counterpart.

## What lives here

The shared row schema (`ValidationResult` / `ValidationPlan`) every runner
emits, the object catalog, the canonical-plan generator, the orbit +
covariance comparison kernel, the per-channel and external runners under
`runners/`, and the report renderer / CLI (`empyrean-validation`).

## Run it

```sh
make setup            # one-time: external-tool venvs, builds, data files
make build            # build every channel + the validation CLI
make run WITH_CORE=1  # replay the plan through all channels + externals
make report           # render the unified HTML report
```

Channel repos sit as siblings of this checkout (`../empyrean`,
`../empyrean-core`, …); `make plan` regenerates the canonical plan from
SBDB / Horizons. `cargo test` covers the schema, comparison kernel, and
report rendering.

Want to benchmark another implementation against the same plan? See
[Add a runner](runners/README.md#add-a-runner) — external runners are
self-contained (read the plan JSON, emit rows in the shared schema) and
the seven existing ones are working templates.

The harness itself consumes only **public crates**: the published
`empyrean` wrapper supplies the SBDB / Horizons query clients the plan
generator uses, and `hyperjet` supplies the linear-algebra kernels the
covariance comparison uses. It has no private dependencies — the private
repos enter only as the optional `core` reference channel (built from a
sibling `empyrean-core` checkout when present) and as the things being
validated.

## Versioning and local development overrides

This harness is versioned in lockstep with the distribution release it
validates: checking out validation `vX.Y.Z` and running it reproduces the
validation-of-record for empyrean `X.Y.Z`. The `validates empyrean X.Y.Z`
badge above is part of the same atomic pin-bump artifact — it always
states the published release the committed manifests point at. The committed manifests
therefore pin only published, tagged artifacts — the runners consume
`empyrean` from crates.io (whose `empyrean-sys` downloads the
checksum-pinned engine of that release), and the plan generator's JPL
query clients come from the same published crate. The core reference channel
(`validate-core`) builds from the `empyrean-core` tag of that generation.

Validating unreleased work is a deliberate, **uncommitted** local
override — never commit these:

```toml
# In the relevant Cargo.toml(s), temporarily:
empyrean = { path = "../../../empyrean/empyrean" }   # runner: local wrapper

[patch.crates-io]
empyrean = { path = "../empyrean/empyrean" }          # harness: local wrapper
```

Note that Cargo honors `[patch]` only from the build-root manifest, and a
path patch to a missing sibling breaks every cargo command in that
checkout — keep overrides scoped to the manifest you are actually
building from, and revert them before committing.
