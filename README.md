<img src="docs/empyrean-dynamics-icon.png" width="140" alt="empyrean-validation">

# empyrean-validation
Cross-channel validation framework for the empyrean astrodynamics stack

<a href="https://claude.ai"><img src="https://img.shields.io/badge/Built%20with-Claude%20Code-D97757?logo=anthropic&logoColor=white&style=flat-square" alt="Built with Claude Code"></a>
<br>
<a href="https://www.empyrean-dynamics.com"><img src="https://img.shields.io/badge/Website-empyrean--dynamics.com-1a1a2e?logo=data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHdpZHRoPSIyNCIgaGVpZ2h0PSIyNCIgdmlld0JveD0iMCAwIDI0IDI0IiBmaWxsPSJub25lIiBzdHJva2U9IndoaXRlIiBzdHJva2Utd2lkdGg9IjIiIHN0cm9rZS1saW5lY2FwPSJyb3VuZCIgc3Ryb2tlLWxpbmVqb2luPSJyb3VuZCI+PGNpcmNsZSBjeD0iMTIiIGN5PSIxMiIgcj0iMTAiLz48bGluZSB4MT0iMiIgeTE9IjEyIiB4Mj0iMjIiIHkyPSIxMiIvPjxwYXRoIGQ9Ik0xMiAyYTE1LjMgMTUuMyAwIDAgMSA0IDEwIDE1LjMgMTUuMyAwIDAgMS00IDEwIDE1LjMgMTUuMyAwIDAgMS00LTEwIDE1LjMgMTUuMyAwIDAgMSA0LTEweiIvPjwvc3ZnPg==&logoColor=white&style=flat-square" alt="Website"></a>
<a href="https://github.com/Empyrean-Dynamics"><img src="https://img.shields.io/badge/GitHub-Empyrean--Dynamics-1a1a2e?logo=github&logoColor=white&style=flat-square" alt="GitHub"></a>

---

empyrean-validation is the contract between every repo in the empyrean tree
and the validation pipeline that exercises them. It owns:

- The **shared row schema** every channel runner emits and every consumer
  deserializes (`ValidationResult`, `ValidationPlan`)
- The **curated test catalog** — 43 NEAs, comets, Trojans, KBOs, ISOs,
  and atmospheric impactors with per-object `dt` grids, public so the
  catalog is auditable + community-contributable
- The **canonical-plan generator** that fetches initial conditions from
  JPL SBDB and reference values from JPL Horizons, producing the
  `validation_plan.json` every replay channel consumes
- The **HTML report renderer** that turns channel result JSONs into the
  branded report shipped to the website
- The **external-reference runners** under `runners/` that compare the
  empyrean stack against ASSIST, find_orb, and kete

## Architecture

```
                    villeneuve  scott  nolan
                          │       │     │
                          └───────┼─────┘
                                  ▼
                            empyrean-core
                                  ▲
                          ┌───────┼─────────────┐
                          │       │             │
                          │       ▼             ▼
                          │  empyrean    empyrean-validation
                          │       ▲             ▲
                          │       │             │
                          │  ┌────┼─────┐       │
                          │  │    │     │       │
                          │  ▼    ▼     ▼       │
                          │  rust py   c/cli    │
                          │   runners (in empyrean)
                          │                     │
                          └────── core runner ──┘
                            (in empyrean-core)
```

empyrean-validation has no dependency on empyrean-core or empyrean. The
direction of the graph runs the other way: each channel-owning repo
(empyrean-core for the core channel, empyrean for the four distribution
channels) consumes empyrean-validation's schema and catalog, runs its
own channel against the canonical plan, and uploads results to the
shared GCS bucket. This crate then renders the unified report.

## Status

Initial commit lays out the module skeleton; the schema, catalog, plan
generator, and report renderer migrate into this repo from
`empyrean/validation/runners/rust/src/` over the next few PRs. See
issue tracker for the migration plan.

## Build

```sh
cargo build
cargo test
cargo doc --no-deps --open
```

empyrean-validation depends on
[villeneuve](https://github.com/Empyrean-Dynamics/villeneuve) for the
SBDB / Horizons clients used by the plan generator. It does not depend
on scott, empyrean-core, or empyrean — those are the things being
validated.
