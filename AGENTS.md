# AGENTS.md

Guidance for coding agents working in this repository.

## What this repository is

- The validation harness for the empyrean stack: one canonical plan (a
  curated catalogue of Solar System objects with per-object time grids)
  replayed through every distribution channel and through independent
  external tools, rendered into one HTML report with cross-channel parity
  at bit resolution, accuracy against JPL Horizons, and orbit-determination
  quality against external fitters.
- The report is the product. The current one is at
  <https://validation.empyrean-dynamics.com/latest/validation_report.html>;
  archived runs sit under `archive/` in the same bucket and locally under
  `results/archive/`, written by `make archive`. Cite the report URL,
  never the bare domain.
- The harness consumes only public crates and the public `empyrean` package;
  the channel repositories sit as siblings of a checkout.

## Work here

- `make setup`, `make build`, `make run WITH_CORE=1`, `make report`; `cargo
  test` covers the schema, the comparison kernel and the renderer. Opt-in
  external tools are gated by `WITH_*` variables (see the README).
- The plan is the contract: a change to a row kind, a method tag or a
  tolerance is a schema change and lands with its renderer and runner
  updates together. Never edit a result file by hand.
- A disagreement with another tool or a published reference at a level you
  consider unacceptable is a *validation finding*: open it through the issue
  templates with the object, the row and the numbers.

## For agents using empyrean rather than changing it

The `empyrean` skill states what the package does, what it is not for and
how this validation is to be read; install it with
`claude plugin marketplace add Empyrean-Dynamics/empyrean` and
`claude plugin install empyrean@empyrean-dynamics`, or
`npx skills add Empyrean-Dynamics/empyrean`. Package docs:
<https://docs.empyrean-dynamics.com> (plain text: `/llms.txt`).
