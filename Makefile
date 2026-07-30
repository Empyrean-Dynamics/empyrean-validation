# empyrean validation suite — top-level orchestrator.
#
# Lives in the empyrean-validation repo because the validation framework
# owns the pipeline. The per-channel runner *implementations* still live
# in the repos whose code they exercise — empyrean (rust / python / c /
# cli channels) and empyrean-core (core channel) — because they each
# directly link or import the surface they're validating. This Makefile
# just orchestrates them.
#
# Sibling-checkout layout assumed (matches the canonical empyrean monorepo):
#
#   <parent>/
#     ├── empyrean/                 ← rust / python / c / cli channels
#     ├── empyrean-core/            ← core channel (validate-core binary)
#     ├── empyrean-validation/      ← this repo (Makefile + framework + externals)
#     ├── (empyrean-core's private engine deps as siblings — core channel only)
#
# Usage:
#   make setup     # one-time: install ASSIST venv + DE440, build find_orb
#   make build     # build the rust runner + C runner + wheel + core + validation CLI
#   make run       # run all channels (rust / python / c / cli / core / assist / findorb)
#   make report    # merge + generate HTML report + ci-summary JSON
#   make all       # build + run + report
#   make clean     # remove derived report/summary/plan; KEEP channel results
#   make archive   # snapshot a completed run to results/archive/<timestamp>/
#   make clean-results  # delete channel results (requires an archive first)
#
# Override defaults:
#   make all OBJECTS="Apophis,Bennu" TIERS="standard"
#   make all DATA_DIR=/custom/data CACHE_DIR=/custom/cache
#
# CI entrypoint: `make setup all` from a fresh clone (with sibling
# repos checked out as above).

# ── Configuration ──────────────────────────────────────────
ROOT := $(abspath $(dir $(MAKEFILE_LIST)))
# Sibling repos. `empyrean` ships the distribution channels — the
# wrapper crate, libempyrean.dylib, empyrean-cli binary, and the
# empyrean Python wheel — that the per-channel runners consume.
# `empyrean-core` ships the validate-core binary that bypasses FFI
# for the "core" reference channel.
EMPYREAN_ROOT := $(abspath $(ROOT)/../empyrean)
EMPYREAN_CORE_ROOT := $(abspath $(ROOT)/../empyrean-core)
EMPYREAN_VALIDATION_ROOT := $(ROOT)

OBJECTS ?=
TIERS ?= standard
# Matrix-CI plumbing. When PLAN_PREBUILT=1 the plan is assumed to have been
# produced by an upstream `prep` job and dropped into $(RESULTS_DIR) as an
# artifact, so the plan target only asserts its presence instead of rebuilding
# it from the rust channel. This lets the replay + external channels run in
# isolated matrix legs that never build or run the rust reference themselves.
PLAN_PREBUILT ?=
DATA_DIR ?= $(HOME)/.empyrean/data
CACHE_DIR ?= $(HOME)/.empyrean/cache
RESULTS_DIR := $(ROOT)/results
FIXTURES_PSV := $(ROOT)/fixtures/psv
# Radar-augmented fixtures (optical + ADES <radar> table) for the objects with
# radar astrometry; drives the find_orb radar-OD reference pass.
FIXTURES_PSV_RADAR := $(ROOT)/fixtures/psv-radar
# The fixture DATA is not in git: fixtures/manifest.json (tracked) pins an
# immutable public-read GCS snapshot, and the fetch script materializes +
# verifies it. See fixtures/README.md for the contract.
FETCH_FIXTURES := $(ROOT)/scripts/fetch-fixtures.sh

# Per-channel runners (rust / python / c / cli) live in this repo's
# runners/ directory alongside the external-reference runners. They
# consume empyrean's distribution artifacts — wrapper crate,
# libempyrean.dylib, empyrean-cli binary, empyrean Python wheel —
# from the sibling empyrean checkout, but link / import those
# artifacts by path, so they carry no build-graph coupling that
# would force an empyrean publish-tag-bump on every wrapper change.
EMPYREAN_RUNNERS := $(ROOT)/runners
WHEEL_VENV := $(EMPYREAN_ROOT)/empyrean-py/.venv
WHEEL_PY := $(WHEEL_VENV)/bin/python
RUST_BIN := $(EMPYREAN_RUNNERS)/rust/target/release/validate
C_BIN := $(EMPYREAN_RUNNERS)/c/runner
CLI_BIN := $(EMPYREAN_RUNNERS)/cli/target/release/empyrean-cli-runner

# External-reference runners (assist / findorb / kete / oorb / jorbit / grss)
# live under this repo's runners/ directory.
EMP_VAL_BIN := $(EMPYREAN_VALIDATION_ROOT)/target/release/empyrean-validation
EMP_VAL_RUNNERS := $(EMPYREAN_VALIDATION_ROOT)/runners
ASSIST_VENV := $(EMP_VAL_RUNNERS)/assist/.venv
ASSIST_PY := $(ASSIST_VENV)/bin/python
FO_BIN := $(EMP_VAL_RUNNERS)/findorb/install/bin/fo

# Empyrean-core "core" channel: bypasses FFI/wrapper, calls
# empyrean_core directly. The binary lives in
# `empyrean-core/src/bin/validate-core.rs` behind the `validate`
# feature flag.
CORE_BIN := $(EMPYREAN_CORE_ROOT)/target/release/validate-core
WITH_CORE := $(if $(wildcard $(EMPYREAN_CORE_ROOT)/Cargo.toml),1,)

# OpenOrb: DEFAULT-OFF, and that is a decision, not an oversight.
#
# run_oorb.py needs ref_sun_pos_au / ref_sun_vel_au_d on each row to convert
# OpenOrb's heliocentric convention into the plan's SSB frame. Only
# `empyrean-validation plan` populates those; the plan CI and this Makefile
# actually use is stripped from the rust runner's output, which never does.
# Measured: 0 of 652 rust rows and 0 of 448 plan rows carry the key, so 100%
# of oorb's rows skip. The runner now exits nonzero on that instead of writing
# a pass-through of untouched plan rows and calling it a green channel — which
# means running it today is a guaranteed, permanent failure, not a flaky one.
#
# It cannot simply be plumbed: ref_sun_* are not among the 70 keys of the
# v0.7.0 schema empyrean-core pins, so adding them to PLAN_CARRIED_KEYS
# reintroduces the deny_unknown_fields break the whitelist strip exists to
# kill. Sequencing (empyrean-jg3o): tag the validation schema -> re-pin
# empyrean-core -> populate ref_sun_* -> set WITH_OORB=1 here and restore the
# `oorb` matrix leg in .github/workflows/validation.yml.
#
# Turning it off rather than leaving it red is the point: a leg that is always
# red teaches people to ignore red, and a leg that skips in silence is the
# defect this whole branch exists to eliminate. Set WITH_OORB=1 to run it
# anyway — the runner's honest nonzero exit is deliberately intact, so a
# premature re-enable fails immediately and says why.
#
# Defined here, above every target that references it: GNU make expands a
# prerequisite list when the rule is READ, so a flag defined below `setup`
# reads as empty there no matter what it is later assigned.
WITH_OORB ?=
# Why the OpenOrb channel is absent, in one line, for the reduce log. Empty
# when WITH_OORB is set, so a re-enabled leg stops claiming to be disabled.
OORB_DISABLED_REASON := $(if $(WITH_OORB),,the plan carries no ref_sun_pos_au/ref_sun_vel_au_d and cannot until the validation schema is tagged and empyrean-core re-pinned (empyrean-jg3o); leg turned off in .github/workflows/validation.yml and behind WITH_OORB here)

# DYLD path for runtime linking against libempyrean.dylib (built by
# empyrean-c into the empyrean repo's target/release).
DYLD := DYLD_LIBRARY_PATH="$(EMPYREAN_ROOT)/target/release"

# CLI args
ONLY_FLAG := $(if $(OBJECTS),--only "$(OBJECTS)",)

# Output JSONs
#
# `validation_plan.json` is the canonical test fixture every replay channel
# (python / c / cli / core / assist / findorb / kete / grss) consumes — it carries
# the test grid, fetched initial conditions, and Horizons reference values
# but no channel-specific results. It is derived from the rust output by
# stripping channel-specific fields, so the IC/ref data is shared via the
# Horizons disk cache rather than re-fetched per channel.
PLAN := $(RESULTS_DIR)/validation_plan.json
RUST_PROPEPH := $(RESULTS_DIR)/validation_rust_propeph.json
RUST_OD := $(RESULTS_DIR)/validation_rust_od.json
RUST := $(RESULTS_DIR)/validation_rust.json
PYTHON := $(RESULTS_DIR)/validation_python.json
C_OUT := $(RESULTS_DIR)/validation_c.json
CLI_OUT := $(RESULTS_DIR)/validation_cli.json
CORE_OUT := $(RESULTS_DIR)/validation_core.json
ASSIST_OUT := $(RESULTS_DIR)/validation_assist.json
FINDORB_OUT := $(RESULTS_DIR)/validation_findorb.json
# find_orb radar-augmented OD reference (second pass over the psv-radar fixtures);
# its rows attach to the `orbit_determination_radar` OD rows in the merge.
FINDORB_RADAR_OUT := $(RESULTS_DIR)/validation_findorb_radar.json
OORB_OUT := $(RESULTS_DIR)/validation_oorb.json
ORBFIT_OUT := $(RESULTS_DIR)/validation_orbfit.json
KETE_OUT := $(RESULTS_DIR)/validation_kete.json
# GRSS external reference (Makadia et al.) — propagation + ephemeris + OD, and
# with find_orb one of only two radar-capable references in the suite. Two
# passes, mirroring find_orb: the plan-driven one and a radar pass over the
# psv-radar fixtures whose rows attach to the `orbit_determination_radar` rows.
GRSS_OUT := $(RESULTS_DIR)/validation_grss.json
GRSS_RADAR_OUT := $(RESULTS_DIR)/validation_grss_radar.json
JORBIT_OUT := $(RESULTS_DIR)/validation_jorbit.json
# layup external OD reference (opt-in, WITH_LAYUP=1); its rows attach to the
# `orbit_determination` OD rows in the merge, alongside find_orb + OrbFit.
LAYUP_OUT := $(RESULTS_DIR)/validation_layup.json
# Merged-with-external version of the rust unified file. ASSIST and find_orb
# references attach onto the rust prop+eph and OD rows in a single pass.
RUST_MERGED := $(RESULTS_DIR)/validation_rust_merged.json
# `core` is the canonical reference channel: empyrean-core direct (no
# FFI). The merged-with-externals JSON is keyed off core rows so the
# report's external-references panel pairs ASSIST / find_orb / OpenOrb /
# OrbFit comparisons to the same physical computation that the cross-
# channel parity panel uses as its reference.
CORE_MERGED := $(RESULTS_DIR)/validation_core_merged.json
REPORT := $(RESULTS_DIR)/validation_report.html
SUMMARY := $(RESULTS_DIR)/validation_summary.json

# Channel JSON list fed to `validate report`. Comma-joined; empyrean-core
# only appended when its sibling tree is present (WITH_CORE). Each channel
# contributes exactly one file.
comma := ,
# Report inputs: rust is now included WITHOUT externals folded in (just
# the bare $(RUST) file); externals are folded onto the `core` reference
# channel via $(CORE_MERGED). When `core` isn't present, fall back to
# $(RUST_MERGED) so the report still has external comparison data
# (degraded — externals will only pair against rust rows in that mode).
REPORT_INPUTS := $(if $(WITH_CORE),$(RUST)$(comma)$(PYTHON)$(comma)$(C_OUT)$(comma)$(CLI_OUT)$(comma)$(CORE_MERGED),$(RUST_MERGED)$(comma)$(PYTHON)$(comma)$(C_OUT)$(comma)$(CLI_OUT))

# ── Targets ────────────────────────────────────────────────
.PHONY: all setup build run report clean clean-results clean-all archive help fixtures check-fixtures \
        setup-assist setup-findorb setup-oorb setup-orbfit setup-kete setup-jorbit setup-layup \
        setup-grss run-grss \
        build-empyrean-c build-rust build-c build-cli build-wheel build-core build-empyrean-validation \
        run-rust run-python run-c run-cli run-assist run-findorb run-oorb run-orbfit \
        run-core run-kete run-jorbit run-layup \
        merge-external plan check-plan reduce

help:
	@echo "Targets:"
	@echo "  make setup     # one-time: ASSIST venv + DE440 + find_orb"
	@echo "  make build     # rust + C + CLI + wheel$(if $(WITH_CORE), + core,) + validation CLI"
	@echo "  make run       # all channels$(if $(WITH_CORE), (incl. core),)"
	@echo "  make report    # merged HTML + summary"
	@echo "  make all       # build + run + report"
	@echo "  make clean     # remove derived report/summary/plan (results kept)"
	@echo "  make archive   # snapshot a completed run to results/archive/<ts>/"
	@echo "  make clean-results  # delete channel results (archive required)"
	@echo
	@echo "Channels: rust, python, c, cli$(if $(WITH_CORE), + core,)"
	@echo "Variables:  OBJECTS='A,B' TIERS=standard DATA_DIR=$(DATA_DIR)"
	@echo
	@echo "Layout: empyrean=$(EMPYREAN_ROOT)"
	@echo "        empyrean-core=$(EMPYREAN_CORE_ROOT)"
	@echo "        empyrean-validation=$(EMPYREAN_VALIDATION_ROOT)"

all: build run report

# ── Fixtures ───────────────────────────────────────────────
# Every OD channel reads $(FIXTURES_PSV); the find_orb radar pass reads
# $(FIXTURES_PSV_RADAR). The data lives in an immutable public-read GCS
# snapshot pinned bit-exactly by fixtures/manifest.json (tracked); the
# fetch script downloads whatever is missing/stale over plain HTTPS and
# verifies EVERY file's sha256 on EVERY invocation — so no target below
# can ever fit a partial, stale, or contaminated set, and it exits
# nonzero naming each offending file when it cannot deliver that.
#
# Phony on purpose: the verify must run every time. Order-only
# (`| fixtures`) on the file targets: the guard must run before them, but
# a phony prerequisite must never mark a completed multi-hour channel
# output as out of date.
fixtures:
	@$(FETCH_FIXTURES)

# Back-compat alias — CI steps and muscle memory both know this name.
check-fixtures: fixtures

# ── Setup (one-time) ───────────────────────────────────────
# OrbFit's runner is gated on WITH_ORBFIT (see `run`), so only set it up
# when it will actually run — otherwise `make setup` pulls a Docker image
# for a comparator that never executes (and fails the setup if Docker is
# unavailable).
setup: setup-assist setup-findorb setup-kete setup-jorbit setup-grss $(if $(WITH_OORB),setup-oorb,) $(if $(WITH_ORBFIT),setup-orbfit,) $(if $(WITH_LAYUP),setup-layup,)
	@echo
	@echo "External dependencies installed."

setup-assist: $(ASSIST_VENV)/bin/python
$(ASSIST_VENV)/bin/python:
	@echo "──── Setting up ASSIST venv + DE440 ────────────────────"
	@cd $(EMP_VAL_RUNNERS)/assist && ./setup.sh

setup-findorb: $(FO_BIN)
$(FO_BIN):
	@echo "──── Building find_orb from source ─────────────────────"
	@cd $(EMP_VAL_RUNNERS)/findorb && ./setup.sh

# ── Build ──────────────────────────────────────────────────
# `build-empyrean-c` runs first because every other channel runner links
# to `libempyrean.dylib` produced by empyrean-c. The empyrean-sys build
# script doesn't trigger an empyrean-c rebuild — it just links the
# existing artifact — so without this step a stale dylib silently
# survives header-shape changes and crashes the runners with
# struct-layout segfaults.
build: build-empyrean-c build-rust build-c build-cli build-wheel build-empyrean-validation $(if $(WITH_CORE),build-core,)

# Exactly what `shard-reference` needs, and nothing else: the rust runner
# ($(RUST_BIN)), the harness CLI ($(EMP_VAL_BIN)), and libempyrean, which the
# runner links against via $(DYLD). Measured on a reference leg, the full
# `build` took 375s — most of it building the C harness, the CLI and the
# maturin wheel that a reference shard never invokes. Multiplied by one leg per
# catalog object that is over five hours of billed time per run spent compiling
# artifacts nobody in that job uses.
build-reference: build-empyrean-c build-rust build-empyrean-validation

.PHONY: build-reference

# PHONY: cargo's incremental build is cheap when nothing changed and
# this is the only way to guarantee `libempyrean.dylib` matches the
# header that bindgen was compiled against.
build-empyrean-c:
	@echo "──── Building empyrean-c (libempyrean.dylib) ───────────"
	@cd $(EMPYREAN_ROOT) && cargo build --release -p empyrean-c

# All cargo-based build steps below are PHONY so cargo's incremental
# fingerprinting decides whether to rebuild. A bare file target with no
# source prerequisites would only trigger the first build; later edits
# to sibling crates wouldn't rebuild the linked binary because make sees
# the file already exists.
build-rust:
	@echo "──── Building rust runner ──────────────────────────────"
	@cd $(EMPYREAN_RUNNERS)/rust && cargo build --release

build-c:
	@echo "──── Building C runner ─────────────────────────────────"
	@cd $(EMPYREAN_RUNNERS)/c && $(MAKE)

build-cli:
	@echo "──── Building CLI runner ───────────────────────────────"
	@cd $(EMPYREAN_RUNNERS)/cli && cargo build --release

# The python / c / cli channels all run through this venv's interpreter,
# and the python channel imports the empyrean extension that maturin
# compiles into it here. Nothing else creates the venv, so bootstrap it
# (with maturin) as a prerequisite.
$(WHEEL_VENV)/bin/maturin:
	@echo "──── Creating empyrean-py build venv (maturin) ─────────"
	@python3 -m venv $(WHEEL_VENV)
	@$(WHEEL_PY) -m pip install --quiet --upgrade pip maturin

build-wheel: $(WHEEL_VENV)/bin/maturin
	@echo "──── Building empyrean-py wheel ────────────────────────"
	@# Build the wheel and pip-install it, rather than `maturin develop`.
	@# develop resolves the project's dev dependency-groups (which include
	@# the private empyrean-sphinx-theme docs dep) and fails offline; a
	@# plain wheel install pulls only the public runtime deps from the
	@# wheel metadata — PEP 735 groups are never in wheel metadata.
	@rm -rf $(WHEEL_VENV)/wheelhouse
	@cd $(EMPYREAN_ROOT)/empyrean-py && \
	    $(WHEEL_VENV)/bin/maturin build --release --out $(WHEEL_VENV)/wheelhouse
	@$(WHEEL_PY) -m pip install --quiet --force-reinstall $(WHEEL_VENV)/wheelhouse/*.whl

# Optional: empyrean-core "core" channel runner. Only invoked when the
# sibling empyrean-core tree exists (WITH_CORE auto-detected above).
# The validate-core binary lives in empyrean-core/src/bin/, gated on
# the `validate` feature so the lib's dep tree stays clean for
# distribution.
build-core:
	@echo "──── Building empyrean-core direct runner ──────────────"
	@cd $(EMPYREAN_CORE_ROOT) && cargo build --release --bin validate-core --features validate

# `empyrean-validation` CLI: meta operations (plan / merge-external /
# report / ci-check) shared across every empyrean repo. Built from this
# repo's own crate.
build-empyrean-validation:
	@echo "──── Building empyrean-validation CLI ──────────────────"
	@cd $(EMPYREAN_VALIDATION_ROOT) && cargo build --release --bin empyrean-validation

# File target so rules that need the binary (plan strip, merge, report) can
# depend on it directly instead of failing with "No rule to make target"
# when it hasn't been built yet. Delegates to the phony recipe above so
# cargo's fingerprinting still decides whether to rebuild.
$(EMP_VAL_BIN):
	@$(MAKE) --no-print-directory build-empyrean-validation

# ── Run pipeline ───────────────────────────────────────────
# Channel order: rust first (produces the unified $(RUST) input every other
# channel consumes), then the four replay channels (python / c / cli / core),
# then the externals. Each non-rust channel reads exactly one file and writes
# exactly one file.
# Default external set: ASSIST + OpenOrb + find_orb + kete + jorbit +
# layup + GRSS all run as part of `make run` and fold into the merged report.
#
# OrbFit runs its runner via the MPC's Docker container (neofit2.x): it
# refits each OD object from empyrean's IC as a heliocentric Cartesian
# seed and folds the post-fit RMS + observation counts onto the OD rows.
# Default-on. On Apple Silicon the amd64-only image runs under qemu
# (5-10x slower per fit — patience, not a hang); native amd64 CI is fast.
# Set WITH_ORBFIT= (empty) to skip on hosts without Docker. Deep-encounter
# impactors (e.g. 2008 TC3, 2024 BX1) can overflow neofit2.x's encounter
# propagation and are surfaced as per-object errors, never silently.
WITH_ORBFIT ?= 1
# layup: OD reference from ADES PSV astrometry (Smithsonian, ASSIST-backed).
# Default-on; set WITH_LAYUP= (empty) to skip on hosts where its heavy
# C-extension venv is unavailable.
WITH_LAYUP ?= 1
# OpenOrb is gated on WITH_OORB, defined above with the reason it is off.
run: run-rust run-python run-c run-cli run-assist run-findorb \
     run-kete run-jorbit run-grss \
     $(if $(WITH_OORB),run-oorb,) \
     $(if $(WITH_ORBFIT),run-orbfit,) \
     $(if $(WITH_LAYUP),run-layup,) \
     $(if $(WITH_CORE),run-core,)

# Rust channel: two binary subcommands (`validate run` for prop+eph,
# `validate od` for orbit determination) feed into one unified output.
# These are file targets, not PHONY — once written they're reused by every
# other channel without re-running the rust pipeline. To force a fresh
# rust validation, `make clean` (or delete the per-channel JSONs).
run-rust: $(RUST)

$(RUST_PROPEPH): $(RUST_BIN)
	@echo "──── Rust channel: prop + ephemeris ────────────────────"
	@mkdir -p $(RESULTS_DIR)
	@$(DYLD) $(RUST_BIN) run $(ONLY_FLAG) --tiers $(TIERS) \
	    --data-dir $(DATA_DIR) --cache-dir $(CACHE_DIR) \
	    --output $(RUST_PROPEPH)

# `validate od` writes TWO JSONL sidecars beside $(RUST_OD), named from its
# stem: $(RUST_OD_ORBITS) (per-object fitted + propagated orbit + covariance)
# and $(RUST_OD_COMPARE) (fitted-vs-reference Keplerian Mahalanobis
# comparisons, the sole input to the report's §12). They are not channel
# results and no make rule names them as outputs, which is how CI came to
# stage the four channel JSONs and leave both sidecars behind in the prep
# job's workspace — §12 was empty in every published report. Declared here so
# the coupling is visible: the prep upload in .github/workflows/validation.yml
# lists both by name, and `report` now fails rather than rendering an
# unexplained empty §12 when OD rows arrive without them.
RUST_OD_ORBITS := $(RESULTS_DIR)/validation_rust_od_orbits.jsonl
RUST_OD_COMPARE := $(RESULTS_DIR)/validation_rust_od_compare.jsonl

$(RUST_OD): $(RUST_BIN) | fixtures
	@echo "──── Rust channel: orbit determination ─────────────────"
	@$(DYLD) $(RUST_BIN) od $(ONLY_FLAG) --tier $(TIERS) \
	    --data-dir $(DATA_DIR) \
	    --fixtures-dir $(FIXTURES_PSV) \
	    --output $(RUST_OD)
	@for f in $(RUST_OD_ORBITS) $(RUST_OD_COMPARE); do \
	    test -f "$$f" || { \
	        echo "ERROR: $(RUST_BIN) od did not write its sidecar $$f."; \
	        echo "       The report's §12 (fitted orbit + covariance vs references)"; \
	        echo "       has no other input and would render empty."; \
	        exit 1; }; \
	 done

ifeq ($(REFERENCE_ASSEMBLED),1)
# Matrix-CI assemble job: the unified reference was produced by
# `assemble-reference` from the per-object shards, and this job carries NO
# engine — no runners/rust binary, no wheel venv. Assert its presence loudly
# instead of letting make walk the $(RUST_PROPEPH)/$(RUST_OD) chain, which
# names $(RUST_BIN) as a prerequisite that only the phony `build-rust` produces:
# make would abort with "No rule to make target .../validate" before running a
# single recipe. Same shape as PLAN_PREBUILT=1 below, one level up the graph.
$(RUST):
	@test -f $(RUST) || { echo "ERROR: REFERENCE_ASSEMBLED=1 but $(RUST) is missing — run assemble-reference first."; exit 1; }
	@echo "──── Rust channel: using assembled reference $(RUST) ───"
else
$(RUST): $(RUST_PROPEPH) $(RUST_OD)
	@echo "──── Rust channel: merge prop+eph + OD into unified ────"
	@python3 -c "import json,sys; \
a=json.load(open('$(RUST_PROPEPH)')); \
b=json.load(open('$(RUST_OD)')); \
sys.exit('ERROR: $(RUST_OD) carries zero OD rows. The rust OD pass produced nothing — a runner that emits no rows at all is a dead channel, not a passing one. Check the fixture fetch (make fixtures) and the runner log above.') if not b else None; \
json.dump(a+b, open('$(RUST)','w'), indent=2, default=str); \
print(f'Wrote {len(a)+len(b)} unified rust rows ({len(a)} prop+eph, {len(b)} OD) to $(RUST)')"
endif

# ── Per-object reference shards (CI matrix fan-out) ─────────
# The rust reference is the expensive half of this suite: running all three
# axes over the full catalog in one process took 176 min and blew the CI job's
# 180-minute ceiling, taking ~3 hours of completed fits with it (empyrean-qhhw).
# The work is embarrassingly parallel per object, so CI fans it out one leg per
# object and reassembles here.
#
# Why per-object rather than N balanced shards: cost is NOT predictable from
# any cheap proxy — Didymos fits 6,034 observations in 25.4 min while Eros
# fits 13,150 in 47 s — so a static shard table would need measured weights
# and would silently unbalance whenever the engine or catalog moved. One leg
# per object is generated from the catalog, self-balancing forever, and isolates
# a crash or timeout to the single object that caused it.
SHARD_DIR := $(RESULTS_DIR)/shards

# Same filters as ONLY_FLAG, for the catalog enumeration that drives the matrix.
LIST_ONLY_FLAG := $(if $(OBJECTS),--only "$(OBJECTS)",)

# Emit the CI matrix: one entry per object, each with its catalog name and a
# filesystem-safe slug. `list-objects` shares its filter resolution with
# `plan`, so the matrix and the shards cannot disagree about which objects
# are in scope.
# Writes the matrix to a FILE rather than stdout, and the CI step reads the
# file. Piping `make -s list-objects` into fromJson() looked cleaner and was a
# latent trap: whenever $(EMP_VAL_BIN) had to be built first, make's own
# "──── Building empyrean-validation CLI ────" banner went to stdout ahead of
# the JSON, so the matrix expression received "──── Building…" and the entire
# fan-out failed to expand. A file has no such coupling to make's chatter.
OBJECTS_JSON := $(RESULTS_DIR)/objects.json

list-objects: $(OBJECTS_JSON)

$(OBJECTS_JSON): $(EMP_VAL_BIN)
	@mkdir -p $(RESULTS_DIR)
	@$(EMP_VAL_BIN) list-objects $(LIST_ONLY_FLAG) --format json > $(OBJECTS_JSON)
	@python3 -c "import json,sys; d=json.load(open('$(OBJECTS_JSON)')); \
sys.exit('ERROR: $(OBJECTS_JSON) is empty — no objects to validate.') if not d else None; \
print(f'Wrote {len(d)} objects to $(OBJECTS_JSON)')"

# One object's slice of the reference. OBJECT is the catalog name
# ("46P/Wirtanen"), SLUG its safe token ("46P_Wirtanen") — both come from
# `list-objects` so they always correspond.
#
# `od` derives its two JSONL sidecars from the --output stem, so pointing
# --output into the shard directory lands them there too, with no second path
# to keep in sync.
shard-reference: $(RUST_BIN) $(EMP_VAL_BIN) | fixtures
	@test -n "$(OBJECT)" || { echo "ERROR: shard-reference needs OBJECT=<catalog name>."; exit 1; }
	@test -n "$(SLUG)"   || { echo "ERROR: shard-reference needs SLUG=<safe token>."; exit 1; }
	@mkdir -p $(SHARD_DIR)/$(SLUG)
	@echo "──── Reference shard: $(OBJECT) [prop + ephemeris] ─────"
	@$(DYLD) $(RUST_BIN) run --only "$(OBJECT)" --tiers $(TIERS) \
	    --data-dir $(DATA_DIR) --cache-dir $(CACHE_DIR) \
	    --output $(SHARD_DIR)/$(SLUG)/validation_rust_propeph.json
	@echo "──── Reference shard: $(OBJECT) [orbit determination] ──"
	@$(DYLD) $(RUST_BIN) od --only "$(OBJECT)" --tier $(TIERS) \
	    --data-dir $(DATA_DIR) \
	    --fixtures-dir $(FIXTURES_PSV) \
	    --output $(SHARD_DIR)/$(SLUG)/validation_rust_od.json
	@python3 -c "$$ASSERT_SHARD_COMPLETE" $(SHARD_DIR)/$(SLUG) "$(OBJECT)"

# Reassemble every per-object shard into the canonical reference, then let the
# existing $(RUST) merge + strip-plan rules take over unchanged.
#
# This is the fan-in, and fan-ins are where a parallel pipeline goes quietly
# wrong: a leg that died leaves its object simply absent, and a report rendered
# from 49 of 50 objects looks exactly like a healthy one. So the shard set is
# checked against the catalog enumeration and any missing object is named and
# fatal — never warned about, never skipped.
assemble-reference: $(EMP_VAL_BIN)
	@echo "──── Reference: reassemble per-object shards ───────────"
	@mkdir -p $(SHARD_DIR)
	@$(EMP_VAL_BIN) list-objects $(LIST_ONLY_FLAG) --format lines > $(SHARD_DIR)/.expected
	@python3 -c "$$ASSEMBLE_SHARDS" $(SHARD_DIR) $(SHARD_DIR)/.expected \
	    $(RUST_PROPEPH) $(RUST_OD) $(RUST_OD_ORBITS) $(RUST_OD_COMPARE) $(RUST)

.PHONY: list-objects shard-reference assemble-reference lint-workflows

# Static-check the workflows. Worth a target of its own because the fan-out
# introduced a class of bug that nothing else here catches and a green run hides:
# repointing a job's `needs:` without updating the `if:` that reads
# `needs.<job>.result`. The `needs` context holds ONLY direct dependencies, so a
# stale reference reads null, the condition is false forever, and the job is
# SKIPPED — which does not fail the run. That is how `reduce` came to be silently
# skipped on every trigger, dropping the report, the ci-check row floors and the
# publish while the suite reported success. actionlint reports it as
# `property "prep" is not defined in object type {...}`.
lint-workflows:
	@command -v actionlint >/dev/null 2>&1 || { \
	    echo "ERROR: actionlint is not installed — cannot verify the workflows."; \
	    echo "       brew install actionlint   (or see github.com/rhysd/actionlint)"; \
	    exit 1; }
	@echo "──── actionlint: .github/workflows ─────────────────────"
	@actionlint -shellcheck= .github/workflows/*.yml
	@echo "  workflows OK."

# A shard must contain both halves and be parseable. Zero OD rows is legal for
# one object (not every catalog entry need carry fittable astrometry) but zero
# prop+eph rows means the run produced nothing for an object CI believes it
# covered — the silent-empty shape, caught here at the shard rather than after
# 50 of them have been concatenated into an innocuous-looking whole.
define ASSERT_SHARD_COMPLETE
import json, sys, pathlib
d, obj = pathlib.Path(sys.argv[1]), sys.argv[2]
propeph, od = d / "validation_rust_propeph.json", d / "validation_rust_od.json"
problems = []
counts = {}
for f in (propeph, od):
    if not f.is_file():
        problems.append(f"{f.name}: missing")
        continue
    try:
        counts[f.name] = len(json.load(open(f)))
    except Exception as exc:
        problems.append(f"{f.name}: unparseable ({exc})")
for side in ("validation_rust_od_orbits.jsonl", "validation_rust_od_compare.jsonl"):
    if counts.get("validation_rust_od.json") and not (d / side).is_file():
        problems.append(f"{side}: missing, but the OD pass emitted rows")
if counts.get("validation_rust_propeph.json") == 0:
    problems.append("validation_rust_propeph.json: zero rows")
if problems:
    print(f"ERROR: reference shard for {obj!r} is not usable:", file=sys.stderr)
    for p in problems:
        print(f"  - {p}", file=sys.stderr)
    print("       A shard that is short here is an object silently absent from", file=sys.stderr)
    print("       the assembled reference and from the report.", file=sys.stderr)
    raise SystemExit(1)
print(f"  shard OK: {obj} — {counts.get('validation_rust_propeph.json', 0)} prop+eph rows, "
      f"{counts.get('validation_rust_od.json', 0)} OD rows")
endef
export ASSERT_SHARD_COMPLETE

define ASSEMBLE_SHARDS
import json, pathlib, sys
shard_dir = pathlib.Path(sys.argv[1])
expected_file = pathlib.Path(sys.argv[2])
out_propeph, out_od, out_orbits, out_compare, out_unified = (
    pathlib.Path(p) for p in sys.argv[3:8]
)

expected = []
for line in expected_file.read_text().splitlines():
    if line.strip():
        slug, _, name = line.partition("\t")
        expected.append((slug, name))
if not expected:
    raise SystemExit("ERROR: catalog enumeration produced zero objects; refusing to assemble.")

def shard_path(slug):
    """Locate one object's shard directory, whichever layout staged it.

    A local run writes results/shards/<slug>/ directly. CI downloads the
    per-object artifacts, and actions/download-artifact nests each one under a
    directory named after the artifact — results/shards/reference-shard-<slug>/.
    Accepting both means this does not depend on that action's nesting behavior,
    which cannot be verified outside a real run; a shell step that flattened one
    layout into the other would silently produce nothing if the assumption were
    wrong, and every object would then read as missing.
    """
    for candidate in (shard_dir / slug, shard_dir / f"reference-shard-{slug}"):
        if (candidate / "validation_rust_propeph.json").is_file():
            return candidate
    return shard_dir / slug  # canonical path, for the error message

propeph, od, orbits, compare, missing = [], [], [], [], []
for slug, name in expected:
    d = shard_path(slug)
    p, o = d / "validation_rust_propeph.json", d / "validation_rust_od.json"
    if not (p.is_file() and o.is_file()):
        missing.append(f"{name} (slug {slug}): "
                       f"{'propeph' if not p.is_file() else ''}"
                       f"{' and ' if not p.is_file() and not o.is_file() else ''}"
                       f"{'od' if not o.is_file() else ''} shard absent")
        continue
    propeph += json.load(open(p))
    od += json.load(open(o))
    for src, dst in ((d / "validation_rust_od_orbits.jsonl", orbits),
                     (d / "validation_rust_od_compare.jsonl", compare)):
        if src.is_file():
            dst += [ln for ln in src.read_text().splitlines() if ln.strip()]

if missing:
    print(f"ERROR: {len(missing)} of {len(expected)} reference shards did not arrive:",
          file=sys.stderr)
    for m in missing:
        print(f"  - {m}", file=sys.stderr)
    print("       Every object in the catalog gets a matrix leg; a leg that failed or", file=sys.stderr)
    print("       was cancelled leaves its object ABSENT, and a report built from a", file=sys.stderr)
    print("       partial reference is indistinguishable from a complete one.", file=sys.stderr)
    print("       Re-run the failed leg(s); do not assemble around them.", file=sys.stderr)
    raise SystemExit(1)

# Unlike a single object's shard, the assembled whole having no OD rows means
# the entire OD axis is dead — the empyrean-7wo5 shape the suite exists to catch.
if not od:
    raise SystemExit("ERROR: assembled reference carries zero OD rows across all "
                     f"{len(expected)} objects. The OD axis produced nothing, which is a "
                     "dead axis, not a passing one. Check the fixture fetch (make fixtures).")

# The unified reference is written HERE, not by the $(RUST) rule, because the
# assemble job carries no engine: that rule's prerequisites name $(RUST_BIN),
# which only the phony `build-rust` produces, so make would abort before
# running any recipe. Both halves are already in memory, so emit it directly
# and let `make plan REFERENCE_ASSEMBLED=1` treat it as given.
for path, rows in ((out_propeph, propeph), (out_od, od), (out_unified, propeph + od)):
    path.parent.mkdir(parents=True, exist_ok=True)
    json.dump(rows, open(path, "w"), indent=2, default=str)
for path, lines in ((out_orbits, orbits), (out_compare, compare)):
    path.write_text("".join(ln + "\n" for ln in lines))

print(f"Assembled {len(expected)} shards -> {len(propeph)} prop+eph rows, {len(od)} OD rows, "
      f"{len(orbits)} orbit sidecar rows, {len(compare)} compare sidecar rows; "
      f"unified {len(propeph) + len(od)} rows -> {out_unified}")
endef
export ASSEMBLE_SHARDS

# ── Test plan ──────────────────────────────────────────────
# The plan is the canonical test fixture: same row schema as a channel
# output, but with channel-specific result fields nulled out (emp_*, od_*,
# separation_arcsec, channel set to "plan"). Every replay channel reads
# the plan instead of validation_rust.json so they don't depend on rust's
# results.
#
# A plan with zero OD rows is not a smaller plan — it is a plan that
# silently deletes an entire test axis from every downstream channel.
# That is exactly what shipped: the plan carried 0 orbit_determination
# rows, so python / c / cli / core / find_orb / OrbFit / layup each
# replayed nothing on the OD axis and reported success. Assert the count
# on BOTH plan paths — the one that generates it and the one that
# receives it as a prep artifact — so no leg can replay a gutted plan.
#
# OD_TEST_TYPES mirrors empyrean_validation::schema::test_types::
# ORBIT_DETERMINATION_FAMILY, which cannot be imported here: the external
# matrix legs run this assertion under a bare python3 with no cargo toolchain
# and no built harness. The unit test
# `makefile_od_test_type_list_matches_the_schema` reads this very line and
# fails if the two ever disagree, so the duplication is checked rather than
# trusted.
#
# Counted by NAME rather than as "not propagation and not ephemeris". The
# complement counts a typo'd test type as OD, and it counts the narrower
# recovery axes as if they were the OD axis — a plan of nothing but
# `non_grav_recovery` rows satisfied the old form while carrying not one row
# of the axis this assertion protects. So both halves are asserted: some OD
# row must exist, and `orbit_determination` itself must be among them.
OD_TEST_TYPES := orbit_determination,orbit_determination_radar,non_grav_recovery,dt_recovery,photometry_recovery,thrust_recovery
define ASSERT_PLAN_HAS_OD
import json, sys
from collections import Counter

path, od_types = sys.argv[1], sys.argv[2].split(",")
rows = json.load(open(path))
od = [r for r in rows if r.get("test_type") in od_types]
if not od:
    seen = ", ".join(f"{n} {t}" for t, n in sorted(Counter(r.get("test_type") for r in rows).items()))
    sys.exit(
        f"ERROR: {path} carries ZERO orbit-determination rows "
        f"({len(rows)} rows total; test types present: {seen}).\n"
        "       Every OD consumer downstream (python / c / cli / core replay, "
        "find_orb, OrbFit, layup,\n"
        "       SBDB merge, the report's OD section) would no-op and report "
        "success on an untested axis.\n"
        "       Run `make fixtures` and re-read the rust OD runner log."
    )
if not any(r.get("test_type") == "orbit_determination" for r in od):
    sys.exit(
        f"ERROR: {path} carries {len(od)} orbit-determination-family rows but ZERO "
        "`orbit_determination` rows.\n"
        "       The recovery axes (non_grav / dt / photometry / thrust) are checks "
        "layered on top of the\n"
        "       optical fit, not substitutes for it — a plan with only those deletes "
        "the OD axis proper\n"
        "       while still looking non-empty."
    )
c = Counter(r["test_type"] for r in od)
print("  plan OD rows: " + ", ".join(f"{n} {t}" for t, n in sorted(c.items())))
endef
export ASSERT_PLAN_HAS_OD

plan: $(PLAN) check-plan
ifeq ($(PLAN_PREBUILT),1)
# Matrix-CI replay/external leg: the plan was produced by the prep job and
# staged here as an artifact. Assert its presence loudly rather than silently
# rebuilding it (which would need the rust reference this leg deliberately
# does not carry).
$(PLAN):
	@test -f $(PLAN) || { echo "ERROR: PLAN_PREBUILT=1 but $(PLAN) is missing — the prep job's plan artifact was not staged into $(RESULTS_DIR)."; exit 1; }
	@echo "──── Plan: using prebuilt artifact $(PLAN) ─────────────"
else
$(PLAN): $(RUST) $(EMP_VAL_BIN)
	@echo "──── Plan: strip rust unified down to the plan contract ────"
	@$(EMP_VAL_BIN) strip-plan --input $(RUST) --output $(PLAN)
endif

# Phony so the assertion runs on EVERY invocation, not just the one that
# built the plan file. `plan` and every plan-consuming target depend on
# this, so a leg handed a gutted prep artifact refuses to replay it —
# python3 rather than $(WHEEL_PY) because the external legs carry no
# wheel venv.
check-plan: $(PLAN)
	@python3 -c "$$ASSERT_PLAN_HAS_OD" $(PLAN) $(OD_TEST_TYPES)

run-python: $(PLAN) check-plan fixtures
	@echo "──── Python channel: replay plan ───────────────────────"
	@$(WHEEL_PY) $(EMPYREAN_RUNNERS)/python/run.py \
	    --input $(PLAN) --output $(PYTHON) \
	    --fixtures-dir $(FIXTURES_PSV) \
	    --data-dir $(DATA_DIR)

run-c: $(PLAN) $(C_BIN) check-plan fixtures
	@echo "──── C channel: replay plan (prop / eph / OD) ──────────"
	@$(WHEEL_PY) $(EMPYREAN_RUNNERS)/c/drive.py \
	    --input $(PLAN) \
	    --output $(C_OUT) --runner $(C_BIN) \
	    --fixtures-dir $(FIXTURES_PSV) \
	    $(if $(filter-out $(HOME)/.empyrean/data,$(DATA_DIR)),--data-dir $(DATA_DIR),)

run-cli: $(PLAN) $(CLI_BIN) check-plan fixtures
	@echo "──── CLI channel: fork-exec one binary per plan row ────"
	@$(DYLD) $(WHEEL_PY) $(EMPYREAN_RUNNERS)/cli/drive.py \
	    --input $(PLAN) \
	    --output $(CLI_OUT) --runner $(CLI_BIN) \
	    --fixtures-dir $(FIXTURES_PSV) \
	    $(if $(filter-out $(HOME)/.empyrean/data,$(DATA_DIR)),--data-dir $(DATA_DIR),)

run-assist: $(PLAN) check-plan $(ASSIST_PY)
	@echo "──── ASSIST: external propagator reference ─────────────"
	@$(ASSIST_PY) $(EMP_VAL_RUNNERS)/assist/run_assist.py $(PLAN) \
	    --output $(ASSIST_OUT) \
	    --horizons-cache $(CACHE_DIR)/horizons \
	    --data-dir $(DATA_DIR)

# find_orb's binary is an optional, non-fatal BUILD (see findorb/setup.sh) —
# but a missing binary at RUN time is a failure, not a skip. This used to
# `echo '[]' > $(FINDORB_OUT)` and succeed, which handed the merge a
# syntactically valid file asserting "find_orb compared zero objects" and
# produced a green leg that ran no comparator at all. Fail with the fix
# instead; the matrix's fail-fast:false keeps the other comparators alive and
# reduce then omits find_orb honestly.
run-findorb: $(ASSIST_PY) check-plan fixtures
	@test -x "$(FO_BIN)" || { \
	    echo "ERROR: find_orb binary not built: $(FO_BIN)"; \
	    echo "       Build it with: make setup-findorb"; \
	    echo "       (An empty result file would report 'find_orb compared nothing' as a pass.)"; \
	    exit 1; }
	@echo "──── find_orb: external OD reference ───────────────────"
	@$(ASSIST_PY) $(EMP_VAL_RUNNERS)/findorb/run_findorb.py $(FIXTURES_PSV) \
	    --output $(FINDORB_OUT) --fo-binary $(FO_BIN) \
	    --data-dir $(DATA_DIR) --plan $(PLAN)
	@echo "──── find_orb: radar-augmented OD reference (psv-radar) ─"
	@$(ASSIST_PY) $(EMP_VAL_RUNNERS)/findorb/run_findorb.py $(FIXTURES_PSV_RADAR) \
	    --output $(FINDORB_RADAR_OUT) --fo-binary $(FO_BIN) \
	    --data-dir $(DATA_DIR) --test-type orbit_determination_radar

# ── OpenOrb (oorb) external comparison — propagation + ephemeris ─
# Independent Fortran implementation (Granvik et al., University of
# Helsinki). Covers the same propagation + ephemeris axes ASSIST does,
# from a different code base. Setup builds oorb from source via the
# bundled setup.sh; subsequent runs reuse the installed binary.
OORB_BIN := $(EMP_VAL_RUNNERS)/oorb/install/bin/oorb

setup-oorb: $(OORB_BIN)
$(OORB_BIN):
	@echo "──── Building OpenOrb from source ──────────────────────"
	@cd $(EMP_VAL_RUNNERS)/oorb && ./setup.sh

run-oorb: $(OORB_OUT)
# Same rule as find_orb: an optional BUILD, but a missing binary at RUN time
# is a failure. The `echo '[]'` fallback made a leg that compared nothing
# indistinguishable from a leg that compared everything and agreed.
$(OORB_OUT): $(PLAN) $(ASSIST_PY) | check-plan
	@test -x "$(OORB_BIN)" || { \
	    echo "ERROR: OpenOrb binary not built: $(OORB_BIN)"; \
	    echo "       Build it with: make setup-oorb"; \
	    exit 1; }
	@echo "──── OpenOrb: external prop + ephemeris reference ──────"
	@$(ASSIST_PY) $(EMP_VAL_RUNNERS)/oorb/run_oorb.py \
	    --input $(PLAN) --output $(OORB_OUT) \
	    --prefix $(EMP_VAL_RUNNERS)/oorb/install

# ── OrbFit external comparison — orbit determination ─────────────
# OrbFit Consortium (University of Pisa) / IAU Minor Planet Center.
# Canonical implementation of CMC2003 χ²-with-hysteresis rejection.
# Setup pulls the MPC's Docker container; runner shells out via docker.
setup-orbfit:
	@echo "──── Pulling OrbFit container ──────────────────────────"
	@cd $(EMP_VAL_RUNNERS)/orbfit && ./setup.sh

run-orbfit: $(ORBFIT_OUT)
$(ORBFIT_OUT): $(PLAN) | check-plan fixtures
	@echo "──── OrbFit: external OD reference (neofit2.x via docker) ──"
	@$(EMP_VAL_RUNNERS)/orbfit/run_orbfit.py \
	    --plan $(PLAN) --output $(ORBFIT_OUT) --psv-dir $(FIXTURES_PSV)

# Optional: empyrean-core direct (no FFI). Replays the plan in-process so
# the report's Section 09 can show binding-translation drift (rust wrapper
# vs the core baseline) alongside the C / CLI / Python channels.
run-core: $(if $(WITH_CORE),$(CORE_OUT),)
ifneq ($(WITH_CORE),1)
	@echo "──── Core channel skipped (empyrean-core not found) ────"
endif

$(CORE_OUT): $(PLAN) $(CORE_BIN) | check-plan fixtures
	@echo "──── Core channel: replay plan via empyrean-core ───────"
	@$(DYLD) $(CORE_BIN) --input $(PLAN) --output $(CORE_OUT) \
	    --fixtures-dir $(FIXTURES_PSV)

# ── Kete external comparison (standalone, not in report) ───
# kete is Dar Dahlen's open-source NEO toolkit (originally developed
# at Caltech IPAC for NEO Surveyor mission simulation work; now an
# independent personal project at github.com/dahlend/kete). An
# independent Rust+Python implementation of N-body propagation,
# ephemeris, and OD. Used as a sanity check parallel to ASSIST /
# find_orb but covering all three test types.
KETE_VENV := $(EMP_VAL_RUNNERS)/kete/.venv
KETE_PY := $(KETE_VENV)/bin/python

setup-kete: $(KETE_PY)
$(KETE_PY):
	@echo "──── Setting up kete venv ──────────────────────────────"
	@cd $(EMP_VAL_RUNNERS)/kete && ./setup.sh

run-kete: $(KETE_OUT)
$(KETE_OUT): $(PLAN) $(KETE_PY) | check-plan fixtures
	@echo "──── Kete: external all-test-types reference ──────────"
	@$(KETE_PY) $(EMP_VAL_RUNNERS)/kete/run_kete.py \
	    --input $(PLAN) --output $(KETE_OUT) \
	    --fixtures-dir $(FIXTURES_PSV)

# ── GRSS external comparison — propagation + ephemeris + OD ──
# The Gauss-Radau Small-body Simulator (Makadia et al.;
# github.com/rahil-makadia/grss): a C++ propagation/OD core behind a Python
# interface, installed from PyPI into its own venv and never linked into
# empyrean. The widest external reference in the suite — the only one covering
# all three axes — and, with find_orb, one of only two that ingests radar
# astrometry, which is what turns radar OD from a single-witness comparison
# into a cross-check.
#
# Two passes, mirroring run-findorb: the plan-driven pass (propagation +
# ephemeris + optical OD) and a radar pass over $(FIXTURES_PSV_RADAR) whose
# rows attach to the `orbit_determination_radar` OD rows. GRSS's LSQ is seeded
# from the plan's IC, so the radar pass reads the plan too.
GRSS_VENV := $(EMP_VAL_RUNNERS)/grss/.venv
GRSS_PY := $(GRSS_VENV)/bin/python

setup-grss: $(GRSS_PY)
$(GRSS_PY):
	@echo "──── Setting up grss venv + SPICE kernels ──────────────"
	@cd $(EMP_VAL_RUNNERS)/grss && ./setup.sh

run-grss: $(GRSS_OUT)
# Same rule as find_orb / layup: the venv is an optional BUILD, but a missing
# interpreter at RUN time is a failure, not a skip. An empty result file would
# report a comparator that never executed as a successful empty comparison.
$(GRSS_OUT): $(PLAN) | check-plan fixtures
	@test -x "$(GRSS_PY)" || { \
	    echo "ERROR: grss venv not built: $(GRSS_PY)"; \
	    echo "       Build it with: make setup-grss"; \
	    echo "       (An empty result file would report 'GRSS compared nothing' as a pass.)"; \
	    exit 1; }
	@echo "──── GRSS: external prop + eph + OD reference ──────────"
	@$(GRSS_PY) $(EMP_VAL_RUNNERS)/grss/run_grss.py \
	    --input $(PLAN) --output $(GRSS_OUT) \
	    --fixtures-dir $(FIXTURES_PSV) \
	    $(if $(OBJECTS),--objects "$(OBJECTS)",)
	@echo "──── GRSS: radar-augmented OD reference (psv-radar) ────"
	@$(GRSS_PY) $(EMP_VAL_RUNNERS)/grss/run_grss.py --radar \
	    --input $(PLAN) --output $(GRSS_RADAR_OUT) \
	    --fixtures-dir $(FIXTURES_PSV_RADAR) \
	    $(if $(OBJECTS),--objects "$(OBJECTS)",)

# ── jorbit (opt-in, parallel to kete) ────────────────────────
# JAX-based propagator + OD (independent — affiliation pending
# verification). Out of the public-report headline set; invoke
# explicitly via `make run-jorbit` and `make report INCLUDE_OPTIONAL=1`.
JORBIT_VENV := $(EMP_VAL_RUNNERS)/jorbit/.venv
JORBIT_PY := $(JORBIT_VENV)/bin/python

setup-jorbit: $(JORBIT_PY)
$(JORBIT_PY):
	@echo "──── Setting up jorbit venv ────────────────────────────"
	@cd $(EMP_VAL_RUNNERS)/jorbit && ./setup.sh

run-jorbit: $(JORBIT_OUT)
$(JORBIT_OUT): $(PLAN) $(JORBIT_PY) | check-plan
	@echo "──── jorbit: external (opt-in) reference ───────────────"
	@$(JORBIT_PY) $(EMP_VAL_RUNNERS)/jorbit/run_jorbit.py \
	    --input $(PLAN) --output $(JORBIT_OUT)

# ── layup (opt-in external OD reference) ─────────────────────
# MIT-licensed, ASSIST-backed orbit fitter (Smithsonian / CfA; Matthew
# Holman et al.). Fits the optical OD fixtures directly from ADES PSV and
# folds χ² / reduced-χ² / n_obs / convergence onto the OD rows alongside
# find_orb + OrbFit. Opt-in via WITH_LAYUP=1 (heavy C-extension build).
LAYUP_VENV := $(EMP_VAL_RUNNERS)/layup/.venv
LAYUP_PY := $(LAYUP_VENV)/bin/python

setup-layup: $(LAYUP_PY)
$(LAYUP_PY):
	@echo "──── Setting up layup venv (clone + build + bootstrap) ─"
	@cd $(EMP_VAL_RUNNERS)/layup && ./setup.sh

run-layup: $(LAYUP_OUT)
# layup's venv is an optional, heavy BUILD (see layup/setup.sh), but it is
# only ever *run* when WITH_LAYUP is set — so at this point the caller has
# asked for layup and a missing venv is a failure. The `echo '[]'` fallback
# reported a comparator that never executed as a successful empty comparison.
# Opt out with WITH_LAYUP= rather than by silently producing nothing.
$(LAYUP_OUT): | fixtures
	@test -x "$(LAYUP_PY)" || { \
	    echo "ERROR: layup venv not built: $(LAYUP_PY)"; \
	    echo "       Build it with: make setup-layup, or opt out with WITH_LAYUP="; \
	    exit 1; }
	@echo "──── layup: external OD reference (ADES PSV) ───────────"
	@mkdir -p $(RESULTS_DIR)
	@$(LAYUP_PY) $(EMP_VAL_RUNNERS)/layup/run_layup.py $(FIXTURES_PSV) \
	    --output $(LAYUP_OUT)

# ── Merge external + report ────────────────────────────────
# Channel-agnostic meta operations live in this repo's CLI binary. It
# owns the schema, so its merge / report / ci-check stays in lockstep
# with the row format every channel runner emits.
#
# Headline externals folded onto rust rows by default: ASSIST + find_orb
# + OpenOrb + OrbFit + GRSS. kete + jorbit are opt-in — their per-row data is
# present in their own JSONs but not folded onto the rust rows unless
# the user explicitly opts in via the recipe below (see
# INCLUDE_OPTIONAL).
INCLUDE_OPTIONAL ?=
# Fold external references (ASSIST / find_orb / OpenOrb / [OrbFit]) onto
# the canonical `core` reference channel's rows. The report consumes
# $(CORE_MERGED) and the report's external-references panel reads the
# `*_*` fields off core rows, matching the parity-comparison reference
# choice. If `core` isn't present (WITH_CORE not set), fall back to
# folding onto rust rows for backward compat.
merge-external: $(if $(WITH_CORE),$(CORE_MERGED),$(RUST_MERGED))
$(CORE_MERGED): $(CORE_OUT) $(ASSIST_OUT) $(FINDORB_OUT) $(FINDORB_RADAR_OUT) $(if $(WITH_OORB),$(OORB_OUT),) \
                $(KETE_OUT) $(JORBIT_OUT) $(GRSS_OUT) \
                $(if $(WITH_ORBFIT),$(ORBFIT_OUT),) $(if $(WITH_LAYUP),$(LAYUP_OUT),) $(EMP_VAL_BIN)
	@echo "──── Merge ASSIST + find_orb$(if $(WITH_OORB), + OpenOrb,) + kete + jorbit + GRSS$(if $(WITH_ORBFIT), + OrbFit,)$(if $(WITH_LAYUP), + layup,) into core ──"
	@$(EMP_VAL_BIN) merge-external -i $(CORE_OUT) -o $(CORE_MERGED) \
	    --assist $(ASSIST_OUT) --findorb $(FINDORB_OUT) \
	    --findorb-radar $(FINDORB_RADAR_OUT) \
	    $(if $(WITH_OORB),--oorb $(OORB_OUT),) \
	    --kete $(KETE_OUT) --jorbit $(JORBIT_OUT) \
	    --grss $(GRSS_OUT) --grss-radar $(GRSS_RADAR_OUT) \
	    --jpl-sbdb-cache $(CACHE_DIR)/sbdb \
	    $(if $(WITH_ORBFIT),--orbfit $(ORBFIT_OUT),) \
	    $(if $(WITH_LAYUP),--layup $(LAYUP_OUT),)
$(RUST_MERGED): $(RUST) $(ASSIST_OUT) $(FINDORB_OUT) $(FINDORB_RADAR_OUT) $(if $(WITH_OORB),$(OORB_OUT),) \
                $(KETE_OUT) $(JORBIT_OUT) $(GRSS_OUT) \
                $(if $(WITH_ORBFIT),$(ORBFIT_OUT),) $(if $(WITH_LAYUP),$(LAYUP_OUT),) $(EMP_VAL_BIN)
	@echo "──── Merge ASSIST + find_orb$(if $(WITH_OORB), + OpenOrb,) + kete + jorbit + GRSS$(if $(WITH_ORBFIT), + OrbFit,)$(if $(WITH_LAYUP), + layup,) into rust (fallback) ──"
	@$(EMP_VAL_BIN) merge-external -i $(RUST) -o $(RUST_MERGED) \
	    --assist $(ASSIST_OUT) --findorb $(FINDORB_OUT) \
	    --findorb-radar $(FINDORB_RADAR_OUT) \
	    $(if $(WITH_OORB),--oorb $(OORB_OUT),) \
	    --kete $(KETE_OUT) --jorbit $(JORBIT_OUT) \
	    --grss $(GRSS_OUT) --grss-radar $(GRSS_RADAR_OUT) \
	    --jpl-sbdb-cache $(CACHE_DIR)/sbdb \
	    $(if $(WITH_ORBFIT),--orbfit $(ORBFIT_OUT),) \
	    $(if $(WITH_LAYUP),--layup $(LAYUP_OUT),)

report: $(RUST) $(PYTHON) $(C_OUT) $(CLI_OUT) $(if $(WITH_CORE),$(CORE_MERGED),$(RUST_MERGED)) $(EMP_VAL_BIN)
	@echo "──── Generating combined HTML report ───────────────────"
	@$(EMP_VAL_BIN) report \
	    --results $(REPORT_INPUTS) \
	    --output $(REPORT) \
	    --summary $(SUMMARY)
	@echo
	@echo "Report: $(REPORT)"
	@echo "Summary: $(SUMMARY)"
	@echo "Channels: rust, python, c, cli$(if $(WITH_CORE), + core,)"

# ── Matrix-CI reduce ───────────────────────────────────────
# Merge external references + render the report from channel JSONs that
# upstream matrix legs produced and staged into $(RESULTS_DIR). Unlike the
# `report` target this does NOT go through the per-channel file-target rules
# (those would try to rebuild missing binaries), so it never rebuilds a
# channel — the legs own that. It builds only the validation harness itself,
# then folds in whatever external channels are actually present: a leg that
# failed under the matrix's fail-fast:false leaves its JSON absent, and reduce
# skips it *loudly* (printed to the log and, by its absence, to the report)
# rather than faking or defaulting it. The empyrean replay channels
# (rust/python/c/cli/core) come from a single upstream job that succeeds or
# fails atomically, so REPORT_INPUTS still lists them directly.
#
# `add` takes a third argument: the reason the channel is deliberately off, or
# empty when it should have been there. The message used to read "leg failed
# or was disabled" for both cases, which is unreadable — the next person
# cannot tell a policy decision from a breakage, and both look like the
# channel merely went missing. A DISABLED line names the decision and the
# bead; a MISSING line says the leg was expected and did not deliver.
reduce: build-empyrean-validation
	@echo "──── Reduce: merge external references + render report ─"
	@ref=""; merged=""; \
	if [ -f "$(CORE_OUT)" ]; then ref="$(CORE_OUT)"; merged="$(CORE_MERGED)"; \
	elif [ -f "$(RUST)" ]; then ref="$(RUST)"; merged="$(RUST_MERGED)"; \
	else echo "ERROR: reduce found neither $(CORE_OUT) nor $(RUST) — no reference channel was staged."; exit 1; fi; \
	echo "Reference channel: $$ref  →  $$merged"; \
	flags=""; \
	add() { \
	    if [ -f "$$2" ]; then \
	        if [ -n "$$3" ]; then \
	            echo "  NOTE $$1 — marked DISABLED ($$3) yet $$2 IS staged; folding it in anyway. Someone re-enabled the leg without clearing the note."; \
	        fi; \
	        flags="$$flags $$1 $$2"; \
	    elif [ -n "$$3" ]; then \
	        echo "  skip $$1 — DISABLED by decision: $$3"; \
	    else \
	        echo "  skip $$1 — MISSING: $$2 was not staged. Its matrix leg was expected to run and did not deliver (fail-fast:false keeps the rest alive); the report omits this comparator."; \
	    fi; }; \
	add --assist        "$(ASSIST_OUT)"          ""; \
	add --findorb       "$(FINDORB_OUT)"         ""; \
	add --findorb-radar "$(FINDORB_RADAR_OUT)"   ""; \
	add --oorb          "$(OORB_OUT)"            "$(OORB_DISABLED_REASON)"; \
	add --kete          "$(KETE_OUT)"            ""; \
	add --jorbit        "$(JORBIT_OUT)"          ""; \
	add --grss          "$(GRSS_OUT)"             ""; \
	add --grss-radar    "$(GRSS_RADAR_OUT)"       ""; \
	add --orbfit        "$(ORBFIT_OUT)"          ""; \
	add --layup         "$(LAYUP_OUT)"           ""; \
	if [ -d "$(CACHE_DIR)/sbdb" ]; then flags="$$flags --jpl-sbdb-cache $(CACHE_DIR)/sbdb"; fi; \
	$(EMP_VAL_BIN) merge-external -i "$$ref" -o "$$merged" $$flags
	@$(EMP_VAL_BIN) report \
	    --results $(REPORT_INPUTS) \
	    --output $(REPORT) \
	    --summary $(SUMMARY)
	@echo "Report: $(REPORT)"
	@echo "Summary: $(SUMMARY)"

# ── Cleanup ────────────────────────────────────────────────
# Completed runs archive here, one timestamped directory per `make archive`.
ARCHIVE_DIR := $(RESULTS_DIR)/archive

# `clean` no longer removes results/. It used to be `rm -rf $(RESULTS_DIR)`,
# which on 2026-07-23 destroyed a seven-hour local validation run — the
# obvious, muscle-memory command silently deleting the single most expensive
# artifact in the repo. Results are not build output: a full run costs hours of
# CPU and cannot be regenerated from source alone (it depends on the JPL
# responses cached at the time). Removing them is now something you have to ask
# for by name.
clean:
	@rm -f $(REPORT) $(SUMMARY) $(RUST_MERGED) $(CORE_MERGED) $(PLAN)
	@echo "Removed the derived report / summary / merge / plan from $(RESULTS_DIR)."
	@echo "Per-channel results are PRESERVED — 'make clean-results' removes those,"
	@echo "'make archive' snapshots them to $(ARCHIVE_DIR)/<timestamp>/ first."

# The explicit one. Named so it cannot be typed by accident, and it refuses to
# run without an archived copy — the whole point is that hours of compute are
# never one keystroke from gone.
clean-results:
	@test -d "$(ARCHIVE_DIR)" && [ -n "`ls -1 $(ARCHIVE_DIR) 2>/dev/null`" ] || { \
	    echo "ERROR: refusing to delete $(RESULTS_DIR) with nothing in $(ARCHIVE_DIR)."; \
	    echo "       A full run is hours of CPU and is not reproducible from source"; \
	    echo "       alone (it depends on the JPL responses cached at the time)."; \
	    echo "       Run 'make archive' first, or 'rm -rf $(RESULTS_DIR)' if you"; \
	    echo "       really mean it."; \
	    exit 1; }
	@find $(RESULTS_DIR) -mindepth 1 -maxdepth 1 ! -name archive -exec rm -rf {} +
	@echo "Removed the channel outputs in $(RESULTS_DIR). $(ARCHIVE_DIR) preserved."

# Snapshot a completed run. Timestamped so consecutive runs accumulate instead
# of overwriting, which is what makes a run comparable to the one before it.
archive:
	@test -d "$(RESULTS_DIR)" || { echo "Nothing to archive: $(RESULTS_DIR) does not exist."; exit 1; }
	@stamp=`date -u +%Y%m%d_%H%M%S`; dest="$(ARCHIVE_DIR)/$$stamp"; \
	 mkdir -p "$$dest"; \
	 n=0; \
	 for f in $(RESULTS_DIR)/*; do \
	     [ -e "$$f" ] || continue; \
	     case "$$f" in $(ARCHIVE_DIR)) continue ;; esac; \
	     cp -R "$$f" "$$dest"/ && n=$$((n+1)); \
	 done; \
	 test "$$n" -gt 0 || { echo "Nothing to archive: $(RESULTS_DIR) is empty."; rmdir "$$dest"; exit 1; }; \
	 echo "Archived $$n result artifact(s) to $$dest"

clean-all: clean clean-results
	rm -rf $(ASSIST_VENV) $(KETE_VENV) $(GRSS_VENV)
	rm -rf $(EMP_VAL_RUNNERS)/findorb/build $(EMP_VAL_RUNNERS)/findorb/install
	rm -f $(C_BIN)
	rm -rf $(EMPYREAN_RUNNERS)/cli/target $(EMPYREAN_RUNNERS)/rust/target
	@echo "Removed all generated artifacts."
