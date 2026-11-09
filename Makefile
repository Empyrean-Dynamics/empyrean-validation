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
#   make all DATA_DIR=/custom/data
#
# The JPL and MPC inputs (SBDB, Horizons vectors + ephemerides, ADES PSVs)
# are NOT fetched live: `fixtures/manifest.json` pins one immutable snapshot
# minted by the manual mint-inputs workflow, and `make fixtures` materializes
# and sha256-verifies it under `fixtures/` (CACHE_DIR defaults there). Every
# consumer — the plan builder, the reference runner, ASSIST, merge-external —
# reads that snapshot and nothing else; a developer's ~/.empyrean/cache is no
# longer consulted. See fixtures/README.md for the contract.
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

# PREBUILT_BIN=1: the compiled binaries (the rust / CLI / C runners, the harness,
# validate-core, the engine's C library and the wheel) were built ONCE, in prep,
# and staged into $(PREBUILT_DIR) by the prebuilt-bin artifact (scripts/prebuilt.py
# unpack puts each where the build would have). In this mode every `build-*`
# target below ASSERTS its artifact exists and still hashes to the manifest, and
# NEVER compiles: a missing or altered binary fails the leg by name instead of
# being silently rebuilt into something other than what prep measured.
PREBUILT_BIN ?=
PREBUILT_DIR := $(ROOT)/prebuilt
PREBUILT_VERIFY := python3 $(ROOT)/scripts/prebuilt.py verify --manifest $(PREBUILT_DIR)/PREBUILT.json

OBJECTS ?=
TIERS ?= standard
# Matrix-CI plumbing. When PLAN_PREBUILT=1 the plan is assumed to have been
# produced by an upstream `prep` job and dropped into $(RESULTS_DIR) as an
# artifact, so the plan target only asserts its presence instead of rebuilding
# it from the rust channel. This lets the replay + external channels run in
# isolated matrix legs that never build or run the rust reference themselves.
# With LEG set as well (a channel leg of results/schedule.json), the four
# distribution replay channels read only that leg's rows of the plan; see
# channel_plan below.
PLAN_PREBUILT ?=
DATA_DIR ?= $(HOME)/.empyrean/data
# The JPL cache (SBDB + Horizons) is the pinned inputs snapshot on disk, not a
# developer's personal cache: every consumer reads fixtures/{sbdb,horizons} and
# nothing else, so a cache miss fails loudly instead of silently fetching.
CACHE_DIR ?= $(ROOT)/fixtures
RESULTS_DIR := $(ROOT)/results
FIXTURES_PSV := $(ROOT)/fixtures/psv
# Radar-augmented fixtures (optical + ADES <radar> table) for the objects with
# radar astrometry; drives the find_orb radar-OD reference pass.
FIXTURES_PSV_RADAR := $(ROOT)/fixtures/psv-radar
# The inputs snapshot (SBDB, Horizons, ADES PSVs) is not in git:
# fixtures/manifest.json (tracked) pins one immutable snapshot in the suite's
# public validation bucket (gs://empyrean-validation/inputs/<id>/), and
# scripts/inputs.py materializes + sha256-verifies it over plain HTTPS (no
# credential). See fixtures/README.md for the contract.
# INPUTS_OFFLINE=1 verifies the on-disk snapshot without touching the network
# — the reader path in CI, where prep is the sole fetcher.
INPUTS_PIN := $(ROOT)/fixtures/manifest.json
INPUTS_TOOL := python3 $(ROOT)/scripts/inputs.py
INPUTS_OFFLINE ?=

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
WITH_CORE := $(if $(or $(wildcard $(EMPYREAN_CORE_ROOT)/Cargo.toml),$(and $(PREBUILT_BIN),$(wildcard $(CORE_BIN)))),1,)

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
# The plan one distribution replay channel (python / c / cli / core) reads.
# A CI channel leg runs with PLAN_PREBUILT=1 and LEG=<id>: scripts/schedule.py
# packed the plan's rows into cost-balanced legs (the schedule job, from the
# committed cost table), and the leg replays only its rows, the prebuilt plan
# narrowed by scripts/channel-chunks.py select into
# $(RESULTS_DIR)/validation_plan.leg-<channel>.json. Legs used to take whole
# objects, and the one object holding the 42-minute row outran the job limit
# (run 42506609035). With LEG empty there is no leg and the channel reads
# $(PLAN) whole, as a local run does. The external comparators read $(PLAN) in
# every mode.
LEG ?=
SCHEDULE ?= $(RESULTS_DIR)/schedule.json
LEG_MATRIX := $(RESULTS_DIR)/leg-matrix.json
COSTS := $(ROOT)/fixtures/costs.json
# Total channel legs over python / c / cli / core: what is left of the
# account's 60 concurrent jobs after the external legs and a reserve.
CHANNEL_LEG_BUDGET ?= 50
SCHEDULED_CHANNELS := python,c,cli,core
# A row whose single timing run costs this many seconds or more fans its timing
# repetitions out to distinct legs (the cost table marks the rows, today the
# one 42-minute 2020 CD3 propagation; this lowers the bar so a cheap row fans
# out in a rehearsal). Empty = the cost table's own marks only.
HEAVY_MIN_S ?=
HEAVY_FLAG := $(if $(HEAVY_MIN_S),--heavy-min-s $(HEAVY_MIN_S),)
channel_plan = $(if $(LEG),$(RESULTS_DIR)/validation_plan.leg-$(1).json,$(PLAN))
# The side file of a leg's per-row timing repetition overrides: a replica leg of
# the heavy row runs it once. Never a plan key, which would flow into outputs.
channel_reps = $(if $(LEG),--reps-override $(RESULTS_DIR)/validation_plan.leg-$(1).reps.json,)
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
# adam-core external reference (propagation + ephemeris + OD): the released
# adam-core 0.7.0 / adam-assist 0.6.0. Its rows attach to the prop / eph / OD rows.
ADAM_CORE_OUT := $(RESULTS_DIR)/validation_adam-core.json
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
# The renderer writes the page's dataset beside the report, named from the
# report's stem. The page fetches it, so it travels with the report everywhere.
REPORT_DATA := $(RESULTS_DIR)/validation_report.data.json
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
        setup-assist setup-findorb setup-oorb setup-orbfit setup-kete setup-jorbit setup-layup setup-adam-core \
        setup-grss run-grss \
        build-empyrean-c build-rust build-c build-cli build-wheel build-core build-empyrean-validation \
        run-rust run-python run-c run-cli run-assist run-findorb run-oorb run-orbfit \
        run-core run-kete run-jorbit run-layup run-adam-core \
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
# $(FIXTURES_PSV_RADAR); the plan builder and reference runner read
# $(CACHE_DIR)/{sbdb,horizons}. All four product directories are one
# immutable snapshot pinned bit-exactly by fixtures/manifest.json (tracked),
# in the suite's public validation bucket. `scripts/inputs.py fetch`
# materializes whatever is missing/stale over plain HTTPS (atomic
# temp-then-rename, no credential), verifies EVERY file's sha256 on EVERY
# invocation, prunes any snapshot-shaped file (*.psv, *.json in the product
# dirs) the manifest does not list, and exits nonzero
# naming each offending file — so no target below can ever fit a partial,
# stale, or contaminated set. INPUTS_OFFLINE=1 does the verify pass without the
# network: the CI reader path, where prep is the sole fetcher. Files outside
# the four product dirs (fixtures/windows.json,
# fixtures/README.md) are never touched.
#
# Phony on purpose: the verify must run every time. Order-only
# (`| fixtures`) on the file targets: the guard must run before them, but
# a phony prerequisite must never mark a completed multi-hour channel
# output as out of date.
fixtures:
	@$(INPUTS_TOOL) fetch --pin $(INPUTS_PIN) --dest $(ROOT)/fixtures --prune $(if $(INPUTS_OFFLINE),--offline,)

# Back-compat alias — CI steps and muscle memory both know this name.
check-fixtures: fixtures

# ── Setup (one-time) ───────────────────────────────────────
# OrbFit's runner is gated on WITH_ORBFIT (see `run`), so only set it up
# when it will actually run — otherwise `make setup` pulls a Docker image
# for a comparator that never executes (and fails the setup if Docker is
# unavailable).
setup: setup-assist setup-findorb setup-kete setup-jorbit setup-grss $(if $(WITH_OORB),setup-oorb,) $(if $(WITH_ORBFIT),setup-orbfit,) $(if $(WITH_LAYUP),setup-layup,) $(if $(WITH_ADAM_CORE),setup-adam-core,)
	@echo
	@echo "External dependencies installed."

# ASSIST's two ephemerides (the binary DE440 linux_p1550p2650.440 and
# sb441-n16.bsp) come from its folder of the third-party tool set, synced under
# $(DATA_DIR)/third-party; never downloaded.
ASSIST_DATA_DIR ?= $(DATA_DIR)/third-party/assist
setup-assist: $(ASSIST_VENV)/bin/python
$(ASSIST_VENV)/bin/python:
	@echo "──── Setting up ASSIST venv (ephemerides from the third-party set) ─"
	@cd $(EMP_VAL_RUNNERS)/assist && ./setup.sh "$(ASSIST_DATA_DIR)"

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

# Which empyrean release the committed manifests pin. Read from the rust
# runner's manifest rather than spelled again here: two spellings of one
# fact are two facts that can disagree, and this one decides which engine
# the whole run measures.
PINNED_EMPYREAN := $(shell sed -n 's/^empyrean = "=\(.*\)"/\1/p' $(EMPYREAN_RUNNERS)/rust/Cargo.toml)
# The version of the sibling checkout the c / python channels build from.
SIBLING_EMPYREAN := $(shell sed -n '1,10s/^version = "\(.*\)"/\1/p' $(EMPYREAN_ROOT)/empyrean-c/Cargo.toml 2>/dev/null)

# Assert the sibling checkout is the release the manifests pin.
#
# The channels do NOT all get their engine the same way, and the split is
# invisible at run time:
#
#   - rust / cli link `empyrean` from crates.io, whose `empyrean-sys`
#     resolves a CHECKSUM-PINNED prebuilt engine for exactly the pinned
#     version (downloaded into ~/.cache/empyrean). They measure the
#     published artifact no matter what this checkout contains.
#   - c / python build from $(EMPYREAN_ROOT) — this checkout's header,
#     dylib and wheel.
#
# So a sibling checkout on a different release does not fail; it quietly
# makes half the channels measure a different engine than the other half,
# and every cross-channel fidelity diff in the report then attributes an
# engine difference to binding drift. The C runner's ABI handshake catches
# the subset where the ABI version also moved, but two builds of the SAME
# ABI with different physics pass it. Check the version here, where the
# fix is one `git checkout` away.
.PHONY: check-engine-version
check-engine-version:
ifeq ($(PREBUILT_BIN),1)
	@test -n "$(PINNED_EMPYREAN)" || { \
	    echo "ERROR: could not read the pinned empyrean version from $(EMPYREAN_RUNNERS)/rust/Cargo.toml."; \
	    exit 1; }
	@$(PREBUILT_VERIFY) --component engine-c --engine-version "$(PINNED_EMPYREAN)"
	@echo "engine version OK: $(PINNED_EMPYREAN) (pinned == the prebuilt binaries' engine)"
else
	@test -n "$(PINNED_EMPYREAN)" || { \
	    echo "ERROR: could not read the pinned empyrean version from $(EMPYREAN_RUNNERS)/rust/Cargo.toml."; \
	    exit 1; }
	@test -n "$(SIBLING_EMPYREAN)" || { \
	    echo "ERROR: no empyrean checkout at $(EMPYREAN_ROOT) (looked for empyrean-c/Cargo.toml)."; \
	    echo "       The c and python channels build their header, dylib and wheel from it."; \
	    exit 1; }
	@test "$(PINNED_EMPYREAN)" = "$(SIBLING_EMPYREAN)" || { \
	    echo "ERROR: engine version mismatch."; \
	    echo "       manifests pin:            $(PINNED_EMPYREAN)"; \
	    echo "       $(EMPYREAN_ROOT) is: $(SIBLING_EMPYREAN)"; \
	    echo "       The rust and cli channels use the checksum-pinned $(PINNED_EMPYREAN)"; \
	    echo "       engine; the c and python channels would build $(SIBLING_EMPYREAN) from"; \
	    echo "       that checkout. The run would compare two different engines and read"; \
	    echo "       the difference as binding drift."; \
	    echo "       Check out v$(PINNED_EMPYREAN) in $(EMPYREAN_ROOT), or point"; \
	    echo "       EMPYREAN_ROOT at a checkout of it."; \
	    exit 1; }
	@echo "engine version OK: $(PINNED_EMPYREAN) (pinned == $(EMPYREAN_ROOT))"
endif

# PHONY: cargo's incremental build is cheap when nothing changed and
# this is the only way to guarantee `libempyrean.dylib` matches the
# header that bindgen was compiled against.
build-empyrean-c: check-engine-version
ifeq ($(PREBUILT_BIN),1)
	@$(PREBUILT_VERIFY) --component engine-c
else
	@echo "──── Building empyrean-c (libempyrean.dylib) ───────────"
	@cd $(EMPYREAN_ROOT) && cargo build --release -p empyrean-c
endif

# All cargo-based build steps below are PHONY so cargo's incremental
# fingerprinting decides whether to rebuild. A bare file target with no
# source prerequisites would only trigger the first build; later edits
# to sibling crates wouldn't rebuild the linked binary because make sees
# the file already exists.
build-rust:
ifeq ($(PREBUILT_BIN),1)
	@$(PREBUILT_VERIFY) --component rust-runner
else
	@echo "──── Building rust runner ──────────────────────────────"
	@cd $(EMPYREAN_RUNNERS)/rust && cargo build --release
endif

# Gated on the version check: this compiles against $(EMPYREAN_ROOT)'s
# empyrean.h and links its dylib, so a mismatched checkout produces a wall
# of struct / signature errors whose cause is one line of `make` output
# away. Fail with the cause instead.
build-c: check-engine-version
ifeq ($(PREBUILT_BIN),1)
	@$(PREBUILT_VERIFY) --component c-runner
else
	@echo "──── Building C runner ─────────────────────────────────"
	@cd $(EMPYREAN_RUNNERS)/c && $(MAKE) EMPYREAN_ROOT="$(EMPYREAN_ROOT)"
endif

build-cli:
ifeq ($(PREBUILT_BIN),1)
	@$(PREBUILT_VERIFY) --component cli-runner
else
	@echo "──── Building CLI runner ───────────────────────────────"
	@cd $(EMPYREAN_RUNNERS)/cli && cargo build --release
endif

# The python / c / cli channels all run through this venv's interpreter,
# and the python channel imports the empyrean extension that maturin
# compiles into it here. Nothing else creates the venv, so bootstrap it
# (with maturin) as a prerequisite.
$(WHEEL_VENV)/bin/maturin:
	@echo "──── Creating empyrean-py build venv (maturin) ─────────"
	@python3 -m venv $(WHEEL_VENV)
	@$(WHEEL_PY) -m pip install --quiet --upgrade pip maturin

# Same gate as build-c: the wheel is compiled from $(EMPYREAN_ROOT)'s
# empyrean-py, so a mismatched checkout ships the python channel a
# different engine than the rust and cli channels resolve.
build-wheel: check-engine-version $(if $(PREBUILT_BIN),,$(WHEEL_VENV)/bin/maturin)
ifeq ($(PREBUILT_BIN),1)
	@# The wheel was built once in prep; this leg only installs those bytes
	@# (after verifying them) into its own venv.
	@$(PREBUILT_VERIFY) --component wheel
	@echo "──── Installing the prebuilt empyrean-py wheel ─────────"
	@test -x $(WHEEL_PY) || python3 -m venv $(WHEEL_VENV)
	@$(WHEEL_PY) -m pip install --quiet --force-reinstall $(WHEEL_VENV)/wheelhouse/*.whl
else
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
endif

# Optional: empyrean-core "core" channel runner. Only invoked when the
# sibling empyrean-core tree exists (WITH_CORE auto-detected above).
# The validate-core binary lives in empyrean-core/src/bin/, gated on
# the `validate` feature so the lib's dep tree stays clean for
# distribution.
build-core:
ifeq ($(PREBUILT_BIN),1)
	@$(PREBUILT_VERIFY) --component core-runner
else
	@echo "──── Building empyrean-core direct runner ──────────────"
	@cd $(EMPYREAN_CORE_ROOT) && cargo build --release --bin validate-core --features validate
endif

# `empyrean-validation` CLI: meta operations (plan / merge-external /
# report / ci-check) shared across every empyrean repo. Built from this
# repo's own crate.
build-empyrean-validation:
ifeq ($(PREBUILT_BIN),1)
	@$(PREBUILT_VERIFY) --component harness
else
	@echo "──── Building empyrean-validation CLI ──────────────────"
	@cd $(EMPYREAN_VALIDATION_ROOT) && cargo build --release --bin empyrean-validation
endif

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
# adam-core: the released adam-core / adam-assist (PyPI wheels, no private data).
# Default-on; set WITH_ADAM_CORE= (empty) to skip.
WITH_ADAM_CORE ?= 1
# OpenOrb is gated on WITH_OORB, defined above with the reason it is off.
run: run-rust run-python run-c run-cli run-assist run-findorb \
     run-kete run-jorbit run-grss \
     $(if $(WITH_OORB),run-oorb,) \
     $(if $(WITH_ORBFIT),run-orbfit,) \
     $(if $(WITH_LAYUP),run-layup,) \
     $(if $(WITH_ADAM_CORE),run-adam-core,) \
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

# ── Covariance realism (walk-forward family) — LOCAL-ONLY for now ────────
# Expanding per-night windows; predictions scored vs their predicted covariance. Profiles:
# full (the surface run) / ci (iteration + config sweeps) / ladder (smoke).
# The window manifest is the single source every runner consumes; regenerate
# it only against the fixture snapshot on disk.
WALK_PROFILE ?= ci
WALK_MANIFEST := $(ROOT)/fixtures/windows.json
WALK := $(RESULTS_DIR)/validation_rust_walk.json
WALK_WINDOWS := $(RESULTS_DIR)/validation_rust_walk_windows.jsonl
WALK_PREDICTIONS := $(RESULTS_DIR)/validation_rust_walk_predictions.jsonl
WALK_SCORED := $(RESULTS_DIR)/validation_predict_scored.jsonl
WALK_AGG := $(RESULTS_DIR)/validation_predict_agg.json

.PHONY: walk-manifest walk walk-score walk-report
walk-manifest: | fixtures
	@echo "──── Covariance realism: window manifest ───────────────"
	@cargo run --release --bin empyrean-validation -- windows 	    --fixtures-dir $(FIXTURES_PSV) --output $(WALK_MANIFEST) 	    --debias-dir $(DATA_DIR)

walk: $(RUST_BIN) | fixtures
	@echo "──── Covariance realism: rust walk ($(WALK_PROFILE)) ───"
	@test -f $(WALK_MANIFEST) || { 	    echo "ERROR: no window manifest at $(WALK_MANIFEST) — run 'make walk-manifest'."; 	    exit 1; }
	@$(DYLD) $(RUST_BIN) walk --manifest $(WALK_MANIFEST) 	    --profile $(WALK_PROFILE) 	    --fixtures-dir $(FIXTURES_PSV) --data-dir $(DATA_DIR) 	    --output $(WALK)

walk-score:
	@echo "──── Covariance realism: scoring ───────────────────────"
	@cargo run --release --bin empyrean-validation -- score-predictions 	    --manifest $(WALK_MANIFEST) 	    --predictions $(WALK_PREDICTIONS) --windows $(WALK_WINDOWS) 	    --out-scored $(WALK_SCORED) --out-agg $(WALK_AGG)


# Render the report with Part 4.2 fed from this walk's aggregate.
walk-report: $(EMP_VAL_BIN)
	@echo "──── Covariance realism: report (Part 4.2) ─────────────"
	@test -f $(WALK_AGG) || { \
	    echo "ERROR: no walk aggregate at $(WALK_AGG) — run 'make walk-score'."; \
	    exit 1; }
	@$(EMP_VAL_BIN) report \
	    --results $(REPORT_INPUTS) \
	    --output $(REPORT) \
	    --summary $(SUMMARY) \
	    --predict-agg $(WALK_AGG)

# ── Synthetic (perfect-model) lane of the covariance-realism family ──────
# One truth fit + truth ephemeris per object, then per noise law a fixture
# set whose rows carry the injected σ as rmsRA/rmsDec (astCat=Gaia2 so the
# EFCC debias is identically zero). Fits run sigma_policy=reported so the
# 0.2″ fiducial is not floored by the VFCC2017 preset. Local-only, like
# the rest of the family; results/ is ignored.
SYNTH_ROOT := $(RESULTS_DIR)/synthetic
# Injection laws (the law the DATA follow). Tags: gaussian, t$(SYNTH_NU)
# (per-axis Student-t), t$(SYNTH_NU)j (JOINT elliptical Student-t — the law
# the engine's robust fit assumes, so the matched heavy-tailed lane).
SYNTH_LAWS ?= gaussian,student-t,student-t-joint
SYNTH_NU ?= 4
SYNTH_SEED ?= 1
SYNTH_FIDUCIAL ?= 0.2
# The lane a walk / score targets (a law tag: gaussian | t$(SYNTH_NU) |
# t$(SYNTH_NU)j).
SYNTH_LANE ?= gaussian
SYNTH_DIR := $(SYNTH_ROOT)/$(SYNTH_LANE)
SYNTH_WALK := $(SYNTH_DIR)/validation_rust_walk_synthetic.json
SYNTH_WINDOWS := $(SYNTH_DIR)/validation_rust_walk_synthetic_windows.jsonl
SYNTH_PREDICTIONS := $(SYNTH_DIR)/validation_rust_walk_synthetic_predictions.jsonl
SYNTH_SCORED := $(SYNTH_DIR)/validation_predict_scored_synthetic.jsonl
SYNTH_AGG := $(SYNTH_DIR)/validation_predict_agg_synthetic.json
# Debias is inert on Gaia2 rows, so the grid collapses to rejection × nightly.
SYNTH_ARMS ?= efcc.adap.vfc,efcc.adap.nonight,efcc.cmc.vfc,efcc.cmc.nonight,efcc.norej.vfc,efcc.norej.nonight
SYNTH_MODES ?= first-order,sigma-points
# The ASSUMED fit-side error law (the law the estimator assumes — distinct
# from the injection law above). Empty leaves every arm at its own value
# (normal for the grid). Set SYNTH_ERROR_MODEL=student-t + SYNTH_FIT_NU to
# fit the whole grid under a Student-t law; SYNTH_COV_INFO picks the
# covariance information matrix (observed | expected). For a MIXED matrix of
# laws / rejection gates (incl. the blunder gate) in one walk, point
# SYNTH_CONFIG_SET at an arm JSON file instead of using the grid.
SYNTH_ERROR_MODEL ?=
SYNTH_FIT_NU ?=
SYNTH_COV_INFO ?=
SYNTH_CONFIG_SET ?=
SYNTH_ARM_SOURCE := $(if $(SYNTH_CONFIG_SET),--config-set $(SYNTH_CONFIG_SET),--grid --arms $(SYNTH_ARMS))
SYNTH_EM_FLAGS := $(if $(SYNTH_ERROR_MODEL),--error-model $(SYNTH_ERROR_MODEL),) $(if $(SYNTH_FIT_NU),--nu $(SYNTH_FIT_NU),) $(if $(SYNTH_COV_INFO),--covariance-information $(SYNTH_COV_INFO),)

.PHONY: synth synth-walk synth-score
synth: $(RUST_BIN) | fixtures
	@echo "──── Covariance realism: synthetic fixtures ($(SYNTH_LAWS)) ───"
	@$(DYLD) $(RUST_BIN) synthesize --out-root $(SYNTH_ROOT) \
	    --noise $(SYNTH_LAWS) --nu $(SYNTH_NU) --seed $(SYNTH_SEED) \
	    --fiducial-sigma-arcsec $(SYNTH_FIDUCIAL) \
	    --fixtures-dir $(FIXTURES_PSV) --debias-dir $(DATA_DIR) --data-dir $(DATA_DIR)

synth-walk: $(RUST_BIN)
	@echo "──── Covariance realism: synthetic walk [$(SYNTH_LANE)] ($(WALK_PROFILE)) ───"
	@test -f $(SYNTH_DIR)/windows.json || { \
	    echo "ERROR: no synthetic manifest at $(SYNTH_DIR)/windows.json — run 'make synth'."; \
	    exit 1; }
	@$(DYLD) $(RUST_BIN) walk --manifest $(SYNTH_DIR)/windows.json \
	    --profile $(WALK_PROFILE) $(SYNTH_ARM_SOURCE) $(SYNTH_EM_FLAGS) \
	    --sigma-policy reported --uncertainty-modes $(SYNTH_MODES) \
	    --fixtures-dir $(SYNTH_DIR)/psv --data-dir $(DATA_DIR) \
	    --output $(SYNTH_WALK)

synth-score:
	@echo "──── Covariance realism: synthetic scoring [$(SYNTH_LANE)] ───"
	@cargo run --release --bin empyrean-validation -- score-predictions \
	    --manifest $(SYNTH_DIR)/windows.json \
	    --predictions $(SYNTH_PREDICTIONS) --windows $(SYNTH_WINDOWS) \
	    --out-scored $(SYNTH_SCORED) --out-agg $(SYNTH_AGG)

$(RUST_OD): $(RUST_BIN) | fixtures
	@echo "──── Rust channel: orbit determination ─────────────────"
	@$(DYLD) $(RUST_BIN) od $(ONLY_FLAG) --tier $(TIERS) \
	    --data-dir $(DATA_DIR) --cache-dir $(CACHE_DIR) \
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

# ── Reference legs (CI matrix fan-out) ──────────────────────
# The rust reference is the expensive half of this suite: running all three
# axes over the full catalog in one process took 176 min and blew the CI job's
# 180-minute ceiling, taking ~3 hours of completed fits with it (empyrean-qhhw).
# CI therefore fans it out in legs and reassembles here. The legs used to be one
# per object (an object is a self-balancing unit: fit cost is NOT predictable
# from any cheap proxy, Didymos fits 6,034 observations in 25.4 min while Eros
# fits 13,150 in 47 s), but one object holds the single row that costs 42
# minutes per run, timed best of three, so a leg now takes ROWS, packed by the
# committed cost table (see schedule-reference below).
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

# The reference is computed in cost-balanced LEGS, not one job per object: the
# one object holding the 42-minute 2020 CD3 propagation row (timed best of three
# in every channel) outran every job budget, and an object-level split cannot
# cut it. A candidate row is a propagation or ephemeris row the rust runner
# requests; `validate run --list-rows` prints them in emission order and needs
# no engine call, so prep lists them, packs them (with one OD unit per object)
# into legs by cost (scripts/schedule.py pack-reference), and each leg runs
# `validate run --select` over its rows and `validate od --only` over the
# objects whose OD run it owns.
REFERENCE_CANDIDATES := $(RESULTS_DIR)/reference-candidates.json
REFERENCE_SCHEDULE := $(RESULTS_DIR)/reference-schedule.json
REFERENCE_LEG_MATRIX := $(RESULTS_DIR)/reference-leg-matrix.json
REFERENCE_LEG_BUDGET ?= 16
# The heavy propagation rows (the cost table marks them; HEAVY_MIN_S adds any
# row whose single timing run costs that much in any channel) are DECOUPLED:
# they run on legs of their own from prep, in parallel with the light reference
# (scripts/schedule.py, "heavy decoupling"). prep writes the heavy file, builds
# their plan rows with no engine call (`validate run --emit-plan-rows`), and
# schedules their rust replicas (the heavy reference schedule) and their channel
# replicas (the heavy channel schedule, over the heavy plan).
HEAVY := $(RESULTS_DIR)/heavy.json
HEAVY_SELECT := $(RESULTS_DIR)/heavy-select.json
HEAVY_PLAN := $(RESULTS_DIR)/heavy-plan.json
REFERENCE_HEAVY_SCHEDULE := $(RESULTS_DIR)/reference-heavy-schedule.json
REFERENCE_HEAVY_LEG_MATRIX := $(RESULTS_DIR)/reference-heavy-leg-matrix.json
HEAVY_SCHEDULE := $(RESULTS_DIR)/schedule-heavy.json
HEAVY_LEG_MATRIX := $(RESULTS_DIR)/heavy-leg-matrix.json
# The schedule a reference leg reads its rows from: the light one, or the heavy
# one for a reference-heavy leg (LEG=h01 ...).
REFERENCE_LEG_SCHEDULE ?= $(REFERENCE_SCHEDULE)
# The decoupled rows the final assemble found the engine refused after the plan
# was published; reduce discards them from every channel and external.
HEAVY_DROPPED := $(RESULTS_DIR)/validation_rust_heavy_dropped.json
PLAN_FINAL := $(RESULTS_DIR)/validation_plan.final.json

.PHONY: list-reference-rows schedule-reference
list-reference-rows: $(RUST_BIN)
	@mkdir -p $(RESULTS_DIR)
	@$(DYLD) $(RUST_BIN) run --list-rows $(ONLY_FLAG) --tiers $(TIERS) \
	    --data-dir $(DATA_DIR) --cache-dir $(CACHE_DIR) > $(REFERENCE_CANDIDATES)
	@python3 -c "import json,sys; d=json.load(open('$(REFERENCE_CANDIDATES)')); \
sys.exit('ERROR: $(REFERENCE_CANDIDATES) is empty: no candidate rows.') if not d else None; \
print(f'Wrote {len(d)} candidate reference rows to $(REFERENCE_CANDIDATES)')"

# The reference schedule: a pure function of the candidate rows, the object
# list, the committed cost table and the leg budget. Writes the schedule and the
# leg matrix, prints each leg's predicted wall, names every row the cost table
# does not know.
schedule-reference: $(OBJECTS_JSON) list-reference-rows
	@echo "──── Heavy rows: decouple them and emit their plan rows ─"
	@python3 $(ROOT)/scripts/schedule.py heavy --candidates $(REFERENCE_CANDIDATES) --costs $(COSTS) \
	    --channels $(SCHEDULED_CHANNELS) $(HEAVY_FLAG) --out $(HEAVY) --select-out $(HEAVY_SELECT)
	@if [ "$$(python3 -c "import json;print(len(json.load(open('$(HEAVY_SELECT)'))))")" != "0" ]; then \
	    $(DYLD) $(RUST_BIN) run --select $(HEAVY_SELECT) --emit-plan-rows $(ONLY_FLAG) --tiers $(TIERS) \
	        --data-dir $(DATA_DIR) --cache-dir $(CACHE_DIR) --output $(HEAVY_PLAN) || exit 1; \
	else \
	    echo "[]" > $(HEAVY_PLAN); echo "[]" > $(HEAVY_PLAN).dropped.json; \
	fi
	@echo "──── Schedule: pack the reference into cost-balanced legs ───"
	@python3 $(ROOT)/scripts/schedule.py pack-reference --candidates $(REFERENCE_CANDIDATES) \
	    --objects $(OBJECTS_JSON) --costs $(COSTS) --legs $(REFERENCE_LEG_BUDGET) $(HEAVY_FLAG) \
	    --heavy $(HEAVY) --heavy-out $(REFERENCE_HEAVY_SCHEDULE) --heavy-matrix $(REFERENCE_HEAVY_LEG_MATRIX) \
	    --out $(REFERENCE_SCHEDULE) --matrix $(REFERENCE_LEG_MATRIX)
	@echo "──── Schedule: the heavy rows' channel replicas ────────"
	@python3 $(ROOT)/scripts/schedule.py pack-heavy --heavy-plan $(HEAVY_PLAN) --heavy $(HEAVY) \
	    --costs $(COSTS) --channels $(SCHEDULED_CHANNELS) --out $(HEAVY_SCHEDULE) --matrix $(HEAVY_LEG_MATRIX)

# One reference leg. LEG is the leg id from $(REFERENCE_SCHEDULE).
#
# `od` derives its two JSONL sidecars from the --output stem, so pointing
# --output into the leg directory lands them there too, with no second path to
# keep in sync. A leg always writes validation_rust_propeph.json (with its
# .dropped.json) and validation_rust_od.json, empty lists when it has nothing of
# that kind, so the assemble step finds the same files in every leg.
shard-reference: $(RUST_BIN) $(EMP_VAL_BIN) | fixtures
	@test -n "$(LEG)" || { echo "ERROR: shard-reference needs LEG=<leg id> (see $(REFERENCE_SCHEDULE))."; exit 1; }
	@mkdir -p $(SHARD_DIR)/$(LEG)
	@python3 $(ROOT)/scripts/reference-legs.py select --candidates $(REFERENCE_CANDIDATES) \
	    --schedule $(REFERENCE_LEG_SCHEDULE) --leg "$(LEG)" \
	    --rows-out $(SHARD_DIR)/$(LEG)/leg.rows.json --reps-out $(SHARD_DIR)/$(LEG)/leg.reps.json \
	    --objects-out $(SHARD_DIR)/$(LEG)/leg.objects
	@if [ -s $(SHARD_DIR)/$(LEG)/leg.rows.json ]; then \
	    echo "──── Reference leg $(LEG) [prop + ephemeris rows] ────────"; \
	    $(DYLD) $(RUST_BIN) run --select $(SHARD_DIR)/$(LEG)/leg.rows.json \
	        --reps-override $(SHARD_DIR)/$(LEG)/leg.reps.json $(ONLY_FLAG) --tiers $(TIERS) \
	        --data-dir $(DATA_DIR) --cache-dir $(CACHE_DIR) \
	        --output $(SHARD_DIR)/$(LEG)/validation_rust_propeph.json || exit 1; \
	else \
	    echo "Reference leg $(LEG): no propagation or ephemeris rows."; \
	    echo "[]" > $(SHARD_DIR)/$(LEG)/validation_rust_propeph.json; \
	    echo "[]" > $(SHARD_DIR)/$(LEG)/validation_rust_propeph.json.dropped.json; \
	fi
	@if [ -s $(SHARD_DIR)/$(LEG)/leg.objects ]; then \
	    echo "──── Reference leg $(LEG) [orbit determination: $$(cat $(SHARD_DIR)/$(LEG)/leg.objects)] ──"; \
	    $(DYLD) $(RUST_BIN) od --only "$$(cat $(SHARD_DIR)/$(LEG)/leg.objects)" --tier $(TIERS) \
	        --data-dir $(DATA_DIR) --cache-dir $(CACHE_DIR) \
	        --fixtures-dir $(FIXTURES_PSV) \
	        --output $(SHARD_DIR)/$(LEG)/validation_rust_od.json || exit 1; \
	else \
	    echo "Reference leg $(LEG): no orbit-determination objects."; \
	    echo "[]" > $(SHARD_DIR)/$(LEG)/validation_rust_od.json; \
	fi
	@# The rows file is tested by the shell, not $(wildcard): make caches directory
	@# listings, and a file written earlier in this same recipe reads as absent, which
	@# would run the check against an empty selection and refuse every result row.
	@selection=""; if [ -s $(SHARD_DIR)/$(LEG)/leg.rows.json ]; then selection="--rows $(SHARD_DIR)/$(LEG)/leg.rows.json"; fi; \
	python3 $(ROOT)/scripts/reference-legs.py check --dir $(SHARD_DIR)/$(LEG) $$selection \
	    --objects "$$(cat $(SHARD_DIR)/$(LEG)/leg.objects 2>/dev/null)"

# Reassemble every leg into the canonical reference, then let the existing
# $(RUST) merge + strip-plan rules take over unchanged (scripts/assemble-reference.py
# says what it checks: every candidate row a result or named as dropped by the leg
# that ran it, replicas folded, every object's OD run delivered once).
#
# Two steps since the heavy rows decoupled. `assemble-reference` (the assemble
# job) takes the decoupled rows from the plan rows prep emitted for them: the
# reference it writes is provisional and exists to be stripped into the plan the
# channels and externals replay while the heavy legs still run. `assemble-final`
# (the assemble-final job) takes them from the heavy reference legs, writes the
# canonical reference, strips it and checks the strip equals the published plan
# apart from the timestamp, failing closed by row and field; a decoupled row the
# engine refused goes to $(HEAVY_DROPPED).
ASSEMBLE_LEGS = --schedule $(REFERENCE_SCHEDULE) --candidates $(REFERENCE_CANDIDATES) --legs-dir $(SHARD_DIR) \
    --out-propeph $(RUST_PROPEPH) --out-od $(RUST_OD) --out-orbits $(RUST_OD_ORBITS) \
    --out-compare $(RUST_OD_COMPARE) --out-unified $(RUST) \
    --out-dropped $(RESULTS_DIR)/validation_rust_dropped.json
assemble-reference:
	@echo "──── Reference: reassemble the light legs (decoupled rows from their emitted plan rows) ─"
	@mkdir -p $(SHARD_DIR)
	@python3 $(ROOT)/scripts/assemble-reference.py $(ASSEMBLE_LEGS) \
	    $(if $(wildcard $(HEAVY)),--decoupled-plan $(HEAVY_PLAN) --heavy $(HEAVY),)

assemble-final:
	@echo "──── Reference: reassemble every leg, heavy legs included ─"
	@mkdir -p $(SHARD_DIR)
	@python3 $(ROOT)/scripts/assemble-reference.py $(ASSEMBLE_LEGS) \
	    --heavy-schedule $(REFERENCE_HEAVY_SCHEDULE)
	@$(EMP_VAL_BIN) strip-plan --input $(RUST) --output $(PLAN_FINAL)
	@python3 $(ROOT)/scripts/assemble-reference.py check-final --published $(PLAN) --final $(PLAN_FINAL) \
	    --heavy $(HEAVY) --dropped $(RESULTS_DIR)/validation_rust_dropped.json \
	    --out-heavy-dropped $(HEAVY_DROPPED)

.PHONY: list-objects shard-reference assemble-reference assemble-final lint-workflows

# Mint a validation inputs snapshot LOCALLY into a scratch tree, no upload:
# the one online path (.github/workflows/mint-inputs.yml) by hand, minus the
# bucket, the pin, and the provenance CI records. Query-only: the harness
# `plan` subcommand runs against an EMPTY cache dir, so every SBDB and Horizons
# response (vectors, observer ephemerides, Sun states) is fetched LIVE and
# written in the engine's own disk-cache format under MINT_DIR/tree/{horizons,
# sbdb} — it fetches, it does not propagate. The pinned PSVs are attached from
# the fixtures tree; scripts/inputs.py writes the manifest and prints the
# summary. OBJECTS= mints a partial tree, named as such in its manifest.
# Developers mint locally; `make fixtures` fetches the pinned snapshot instead.
# This reaches JPL and the MPC — run it deliberately.
MINT_DIR ?= $(ROOT)/mint
mint-inputs: $(EMP_VAL_BIN) | fixtures
	@echo "──── Mint (local, query-only): plan fetches JPL/MPC into $(MINT_DIR)/tree ──"
	@rm -rf $(MINT_DIR)
	@mkdir -p $(MINT_DIR)/tree
	@$(DYLD) $(EMP_VAL_BIN) plan $(ONLY_FLAG) --tiers $(TIERS) \
	    --cache-dir $(MINT_DIR)/tree --output $(MINT_DIR)/plan-from-scratch.json
	@mkdir -p $(MINT_DIR)/tree/psv $(MINT_DIR)/tree/psv-radar
	@cp $(FIXTURES_PSV)/*.psv $(MINT_DIR)/tree/psv/
	@cp $(FIXTURES_PSV_RADAR)/*.psv $(MINT_DIR)/tree/psv-radar/
	@printf '%s' '{"catalog_commit":"local","distribution_version":"local","workflow_run":"local","endpoints":["https://ssd-api.jpl.nasa.gov/sbdb.api","https://ssd.jpl.nasa.gov/api/horizons.api"],"psv_source":"local:fixtures","curation":{"psv":"set of record, unchanged"}}' > $(MINT_DIR)/provenance.json
	@python3 $(ROOT)/scripts/inputs.py manifest --tree $(MINT_DIR)/tree --provenance $(MINT_DIR)/provenance.json $(if $(OBJECTS),--partial,)
	@python3 $(ROOT)/scripts/inputs.py summary --tree $(MINT_DIR)/tree
	@echo "Local mint complete in $(MINT_DIR)/tree (no upload)."

.PHONY: mint-inputs

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
OD_TEST_TYPES := orbit_determination,orbit_determination_radar,non_grav_recovery,dt_recovery,photometry_recovery,thrust_recovery,covariance_realism
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

# A heavy leg (empyrean-heavy) replays the HEAVY plan: the emitted plan rows of
# the decoupled rows and their warm-up leaders, every one a propagation row. It
# holds no OD row by construction, so it is checked for what it is instead.
define ASSERT_HEAVY_PLAN
import json, sys

path = sys.argv[1]
rows = json.load(open(path))
if not rows:
    sys.exit(f"ERROR: {path} is empty: a heavy leg has nothing to replay.")
bad = sorted({r.get("test_type") for r in rows} - {"propagation"})
if bad:
    sys.exit(f"ERROR: {path} holds {bad} rows; the heavy plan holds propagation rows only.")
print(f"  heavy plan: {len(rows)} propagation rows")
endef
export ASSERT_HEAVY_PLAN
# `full` (the plan assemble published) or `heavy` (the heavy plan prep emitted).
PLAN_KIND ?= full

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
ifeq ($(PLAN_KIND),heavy)
	@python3 -c "$$ASSERT_HEAVY_PLAN" $(PLAN)
else ifeq ($(PLAN_KIND),full)
	@python3 -c "$$ASSERT_PLAN_HAS_OD" $(PLAN) $(OD_TEST_TYPES)
else
	@echo "ERROR: PLAN_KIND=$(PLAN_KIND) is neither full nor heavy."; exit 1
endif

# One channel leg's rows of the prebuilt plan (see channel_plan).
#
# check-plan runs on the FULL prebuilt plan, before the selection, and never on
# a leg. Its assertion is about the plan the assemble job staged (some OD row,
# and some `orbit_determination` row among them); a leg of a few propagation
# rows holds none, so a per-leg assertion would fail a correct leg, and
# asserting it on the full plan first means no leg replays a slice of a gutted
# plan. What a leg can get wrong on its own the select step refuses by name: a
# schedule built for another plan, an unknown channel or leg, an empty leg.
#
# Every leg file is remade on every invocation (check-plan is phony), so a leg
# left by an earlier LEG is never reused.
$(RESULTS_DIR)/validation_plan.leg-%.json: $(PLAN) check-plan
	@echo "──── Plan: select this leg's rows of the prebuilt plan ──"
	@test -n "$(LEG)" || { echo "ERROR: $@ needs LEG=<leg id> (see $(SCHEDULE))."; exit 1; }
	@python3 $(ROOT)/scripts/channel-chunks.py select --plan $(PLAN) \
	    --schedule $(SCHEDULE) --channel $* --leg "$(LEG)" --output $@ \
	    --reps-output $(RESULTS_DIR)/validation_plan.leg-$*.reps.json

# The cost-balanced schedule of the channel legs: a pure function of the plan,
# the committed cost table and the leg budget. Writes $(SCHEDULE) and the leg
# matrix, prints each channel's predicted leg walls, and names every row the
# cost table does not know.
.PHONY: schedule refresh-costs
schedule: $(PLAN) check-plan
	@echo "──── Schedule: pack the plan into cost-balanced legs ────"
	@python3 $(ROOT)/scripts/schedule.py pack --plan $(PLAN) --costs $(COSTS) \
	    --channels $(SCHEDULED_CHANNELS) --budget $(CHANNEL_LEG_BUDGET) $(HEAVY_FLAG) \
	    $(if $(wildcard $(HEAVY)),--heavy $(HEAVY),) \
	    --out $(SCHEDULE) --matrix $(LEG_MATRIX)

# The external legs (S7): one leg per tool, or, for a tool the external cost
# table prices per object (jorbit, OrbFit), object legs packed longest object
# first so no leg replays the whole catalog, each with a timeout sized to its
# predicted wall (scripts/schedule.py pack-external). EXTERNAL_TOOLS names the
# tools that run; EXTERNAL_LEG_TARGET_MIN overrides the table's target leg wall
# (a rehearsal lowers it to cut two objects into two legs).
EXTERNAL_COSTS := $(ROOT)/fixtures/external-costs.json
EXTERNAL_SCHEDULE ?= $(RESULTS_DIR)/external-schedule.json
EXTERNAL_LEG_MATRIX := $(RESULTS_DIR)/external-leg-matrix.json
EXTERNAL_TOOLS ?= assist,findorb,kete,jorbit,layup,orbfit,grss
EXTERNAL_LEG_TARGET_MIN ?=
# One external leg's id in $(EXTERNAL_SCHEDULE); empty runs the tool over the
# whole plan, as before the chunking.
EXT_LEG ?=
.PHONY: schedule-external
schedule-external: $(PLAN) check-plan
	@echo "──── Schedule: the external legs ───────────────────────"
	@python3 $(ROOT)/scripts/schedule.py pack-external --plan $(PLAN) --costs $(EXTERNAL_COSTS) \
	    --tools $(EXTERNAL_TOOLS) $(if $(EXTERNAL_LEG_TARGET_MIN),--target-leg-min $(EXTERNAL_LEG_TARGET_MIN),) \
	    --out $(EXTERNAL_SCHEDULE) --matrix $(EXTERNAL_LEG_MATRIX)

# Rebuild the committed cost table from a named run's results: FROM is a
# directory holding that run's validation_plan.json, validation_rust.json and
# each channel's output (whole, or its leg files below it). The new table goes
# in through a PR, with the run it came from named in the table itself.
refresh-costs:
	@test -n "$(FROM)" -a -n "$(RUN_ID)" -a -n "$(ENGINE_PIN)" || { \
	    echo "usage: make refresh-costs FROM=<run dir> RUN_ID=<run id> ENGINE_PIN=<engine pin>"; exit 1; }
	@python3 $(ROOT)/scripts/schedule.py refresh-costs --from "$(FROM)" \
	    --run-id "$(RUN_ID)" --engine-pin "$(ENGINE_PIN)" --out $(COSTS)

run-python: $(call channel_plan,python) check-plan fixtures
	@echo "──── Python channel: replay plan ───────────────────────"
	@$(WHEEL_PY) $(EMPYREAN_RUNNERS)/python/run.py \
	    --input $(call channel_plan,python) --context $(PLAN) --output $(PYTHON) \
	    $(call channel_reps,python) \
	    --fixtures-dir $(FIXTURES_PSV) \
	    --data-dir $(DATA_DIR)

run-c: $(call channel_plan,c) $(C_BIN) check-plan fixtures
	@echo "──── C channel: replay plan (prop / eph / OD) ──────────"
	@$(WHEEL_PY) $(EMPYREAN_RUNNERS)/c/drive.py \
	    --input $(call channel_plan,c) --context $(PLAN) $(call channel_reps,c) \
	    --output $(C_OUT) --runner $(C_BIN) \
	    --fixtures-dir $(FIXTURES_PSV) \
	    $(if $(filter-out $(HOME)/.empyrean/data,$(DATA_DIR)),--data-dir $(DATA_DIR),)

run-cli: $(call channel_plan,cli) $(CLI_BIN) check-plan fixtures
	@echo "──── CLI channel: fork-exec one binary per plan row ────"
	@$(DYLD) $(WHEEL_PY) $(EMPYREAN_RUNNERS)/cli/drive.py \
	    --input $(call channel_plan,cli) --context $(PLAN) $(call channel_reps,cli) \
	    --output $(CLI_OUT) --runner $(CLI_BIN) \
	    --fixtures-dir $(FIXTURES_PSV) \
	    $(if $(filter-out $(HOME)/.empyrean/data,$(DATA_DIR)),--data-dir $(DATA_DIR),)

run-assist: $(PLAN) check-plan $(ASSIST_PY)
	@echo "──── ASSIST: external propagator reference ─────────────"
	@$(ASSIST_PY) $(EMP_VAL_RUNNERS)/assist/run_assist.py $(PLAN) \
	    --output $(ASSIST_OUT) \
	    --horizons-cache $(CACHE_DIR)/horizons \
	    --data-dir $(ASSIST_DATA_DIR)

# find_orb's build fails loudly (see findorb/setup.sh), and a missing binary
# at RUN time is a failure too, not a skip. This used to
# `echo '[]' > $(FINDORB_OUT)` and succeed, which handed the merge a
# syntactically valid file asserting "find_orb compared zero objects" and
# produced a green leg that ran no comparator at all. Fail with the fix
# instead; the matrix's fail-fast:false keeps the other comparators alive and
# reduce then omits find_orb honestly.
# fo asserts without ~/.find_orb (miscell.cpp); a cached build restores the
# binary but not $HOME, so the config dir is installed here when missing.
# find_orb's binary DE440 comes from its folder of the third-party tool set.
FINDORB_DATA_DIR ?= $(DATA_DIR)/third-party/findorb
run-findorb: $(ASSIST_PY) check-plan fixtures
	@test -f "$(HOME)/.find_orb/cospar.txt" || { \
	    echo "──── Installing find_orb config directory (cached build) ───"; \
	    cd $(EMP_VAL_RUNNERS)/findorb && ./setup.sh --config-only; }
	@test -x "$(FO_BIN)" || { \
	    echo "ERROR: find_orb binary not built: $(FO_BIN)"; \
	    echo "       Build it with: make setup-findorb"; \
	    echo "       (An empty result file would report 'find_orb compared nothing' as a pass.)"; \
	    exit 1; }
	@# find_orb links its binary DE440 only when the file is there and otherwise
	@# runs without it, so its absence is refused here rather than discovered in
	@# the residuals.
	@test -s "$(FINDORB_DATA_DIR)/linux_p1550p2650.440" || { \
	    echo "ERROR: $(FINDORB_DATA_DIR)/linux_p1550p2650.440 is missing: find_orb's DE440 comes from"; \
	    echo "       the findorb/ folder of the third-party tool set (synced under $(DATA_DIR)/third-party)."; \
	    exit 1; }
	@echo "──── find_orb: external OD reference ───────────────────"
	@$(ASSIST_PY) $(EMP_VAL_RUNNERS)/findorb/run_findorb.py $(FIXTURES_PSV) \
	    --output $(FINDORB_OUT) --fo-binary $(FO_BIN) \
	    --data-dir $(FINDORB_DATA_DIR) --plan $(PLAN)
	@echo "──── find_orb: radar-augmented OD reference (psv-radar) ─"
	@$(ASSIST_PY) $(EMP_VAL_RUNNERS)/findorb/run_findorb.py $(FIXTURES_PSV_RADAR) \
	    --output $(FINDORB_RADAR_OUT) --fo-binary $(FO_BIN) \
	    --data-dir $(FINDORB_DATA_DIR) --test-type orbit_determination_radar

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
# find_orb's RUN-time rule (OpenOrb's BUILD stays optional): a missing binary
# at RUN time is a failure. The `echo '[]'` fallback made a leg that compared nothing
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
# EXT_LEG runs one object leg of the external schedule; its "every object
# failed" verdict is then taken over the merged legs (channel-chunks.py
# concat-external), so a leg whose few objects all fail does not hide the rest.
$(ORBFIT_OUT): $(PLAN) | check-plan fixtures
	@echo "──── OrbFit: external OD reference (neofit2.x via docker) ──"
	@set --; \
	if [ -n "$(EXT_LEG)" ]; then \
	    only="$$(python3 $(ROOT)/scripts/channel-chunks.py external-objects --plan $(PLAN) \
	        --schedule $(EXTERNAL_SCHEDULE) --tool orbfit --leg "$(EXT_LEG)")" || exit 1; \
	    set -- --defer-all-failed; \
	    if [ -n "$$only" ]; then echo "  leg $(EXT_LEG): $$only"; set -- --defer-all-failed --only "$$only"; fi; \
	fi; \
	$(EMP_VAL_RUNNERS)/orbfit/run_orbfit.py \
	    --plan $(PLAN) --output $(ORBFIT_OUT) --psv-dir $(FIXTURES_PSV) "$$@"

# Optional: empyrean-core direct (no FFI). Replays the plan in-process so
# the report's Section 09 can show binding-translation drift (rust wrapper
# vs the core baseline) alongside the C / CLI / Python channels.
run-core: $(if $(WITH_CORE),$(CORE_OUT),)
ifneq ($(WITH_CORE),1)
	@echo "──── Core channel skipped (empyrean-core not found) ────"
endif

# A replica leg of the heavy row (a leg with repetition overrides) runs the rows
# that carry one in their own validate-core process with --n-timing-runs 1, the
# rest as usual, and joins the two outputs: validate-core has no per-row
# repetition input, and none is needed.
$(CORE_OUT): $(call channel_plan,core) $(CORE_BIN) | check-plan fixtures
	@echo "──── Core channel: replay plan via empyrean-core ───────"
	@if [ -n "$(LEG)" ] && [ "$$(python3 -c "import json;print(len(json.load(open('$(RESULTS_DIR)/validation_plan.leg-core.reps.json'))))")" != "0" ]; then \
	    python3 $(ROOT)/scripts/channel-chunks.py partition --input $(call channel_plan,core) \
	        --reps $(RESULTS_DIR)/validation_plan.leg-core.reps.json \
	        --override-out $(RESULTS_DIR)/validation_plan.leg-core.once.json \
	        --rest-out $(RESULTS_DIR)/validation_plan.leg-core.rest.json || exit 1; \
	    echo "[]" > $(CORE_OUT).rest; \
	    if [ "$$(python3 -c "import json;print(len(json.load(open('$(RESULTS_DIR)/validation_plan.leg-core.rest.json'))))")" != "0" ]; then \
	        $(DYLD) $(CORE_BIN) --input $(RESULTS_DIR)/validation_plan.leg-core.rest.json --output $(CORE_OUT).rest \
	            --fixtures-dir $(FIXTURES_PSV) || exit 1; \
	    fi; \
	    $(DYLD) $(CORE_BIN) --n-timing-runs 1 --input $(RESULTS_DIR)/validation_plan.leg-core.once.json \
	        --output $(CORE_OUT).once --fixtures-dir $(FIXTURES_PSV) || exit 1; \
	    python3 $(ROOT)/scripts/channel-chunks.py join --out $(CORE_OUT) $(CORE_OUT).rest $(CORE_OUT).once || exit 1; \
	else \
	    $(DYLD) $(CORE_BIN) --input $(call channel_plan,core) --output $(CORE_OUT) \
	        --fixtures-dir $(FIXTURES_PSV); \
	fi

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
	    --fixtures-dir $(FIXTURES_PSV) --data-dir $(DATA_DIR)

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

# GRSS's seven SPICE kernels come from its folder of the third-party tool set,
# synced under $(DATA_DIR)/third-party; none is downloaded.
GRSS_DATA_DIR ?= $(DATA_DIR)/third-party/grss
setup-grss: $(GRSS_PY)
$(GRSS_PY):
	@echo "──── Setting up grss venv + SPICE kernels ──────────────"
	@cd $(EMP_VAL_RUNNERS)/grss && GRSS_DATA_DIR="$(GRSS_DATA_DIR)" ./setup.sh

run-grss: $(GRSS_OUT)
# find_orb's and layup's RUN-time rule (the venv BUILD stays optional): a
# missing interpreter at RUN time is a failure, not a skip. An empty result file would
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

# Offline: jorbit's DE440 and SB441-N16 are staged from the registered kernel
# set in DATA_DIR (runners/jorbit/jorbit_offline.py), never downloaded.
setup-jorbit: $(JORBIT_PY)
$(JORBIT_PY):
	@echo "──── Setting up jorbit venv ────────────────────────────"
	@cd $(EMP_VAL_RUNNERS)/jorbit && DATA_DIR="$(DATA_DIR)" ./setup.sh

run-jorbit: $(JORBIT_OUT)
# EXT_LEG runs one object leg of the external schedule (`--only` its objects);
# jorbit still reads the whole plan, so its geocentre ephemeris spans every
# geocentric row exactly as an unchunked run's does.
$(JORBIT_OUT): $(PLAN) $(JORBIT_PY) | check-plan
	@echo "──── jorbit: external (opt-in) reference ───────────────"
	@set --; \
	if [ -n "$(EXT_LEG)" ]; then \
	    only="$$(python3 $(ROOT)/scripts/channel-chunks.py external-objects --plan $(PLAN) \
	        --schedule $(EXTERNAL_SCHEDULE) --tool jorbit --leg "$(EXT_LEG)")" || exit 1; \
	    if [ -n "$$only" ]; then echo "  leg $(EXT_LEG): $$only"; set -- --only "$$only"; fi; \
	fi; \
	$(JORBIT_PY) $(EMP_VAL_RUNNERS)/jorbit/run_jorbit.py \
	    --input $(PLAN) --output $(JORBIT_OUT) --data-dir $(DATA_DIR) "$$@"

# ── layup (opt-in external OD reference) ─────────────────────
# MIT-licensed, ASSIST-backed orbit fitter (Smithsonian / CfA; Matthew
# Holman et al.). Fits the optical OD fixtures directly from ADES PSV and
# folds χ² / reduced-χ² / n_obs / convergence onto the OD rows alongside
# find_orb + OrbFit. Opt-in via WITH_LAYUP=1 (heavy C-extension build).
LAYUP_VENV := $(EMP_VAL_RUNNERS)/layup/.venv
LAYUP_PY := $(LAYUP_VENV)/bin/python
# layup's data directory (its --ar): every kernel, the observatory codes and
# the debiasing table under layup's own names, staged by stage_data.py from
# the layup folder of the third-party tool set (dataset
# third-party-tools-2026-11-08, synced under $(DATA_DIR)/third-party): the
# folder lists every file layup reads, shared kernels included, so layup needs
# nothing from the engine kernel set. layup never downloads at run time.
LAYUP_DATA_DIR ?= $(DATA_DIR)/third-party/layup
LAYUP_DATA := $(EMP_VAL_RUNNERS)/layup/data

setup-layup: $(LAYUP_PY)
$(LAYUP_PY):
	@echo "──── Setting up layup venv (pinned release + staged data) ─"
	@cd $(EMP_VAL_RUNNERS)/layup && DATA_DIR="$(LAYUP_DATA_DIR)" ./setup.sh

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
	@# Re-staged on every run: a venv restored from the tool cache skips
	@# setup.sh, and the data must be the set this run synced.
	@$(LAYUP_PY) $(EMP_VAL_RUNNERS)/layup/stage_data.py \
	    --data-dir "$(LAYUP_DATA_DIR)" --dest "$(LAYUP_DATA)"
	@mkdir -p $(RESULTS_DIR)
	@$(LAYUP_PY) $(EMP_VAL_RUNNERS)/layup/run_layup.py $(FIXTURES_PSV) \
	    --ar-data-path "$(LAYUP_DATA)" --output $(LAYUP_OUT) \
	    $(if $(OBJECTS),--only "$(OBJECTS)",)

# ── adam-core (external propagation + ephemeris + OD reference) ─
# The released adam-core 0.7.0 with adam-assist 0.6.0 (PyPI wheels, own venv).
# OD runs the public CORE_OD_V1 profile; its missing per-observation sigma comes
# from the suite's shared public Veres 2017 model (runners/sigmafill). It reads
# DE440 and SB441-N16 from the adam-core folder of the third-party tool set
# (dataset third-party-tools-2026-11-08, synced under $(DATA_DIR)/third-party)
# and passes them to adam-assist explicitly; it never falls back to the
# install-time PyPI kernels and fetches nothing at run time.
ADAM_CORE_VENV := $(EMP_VAL_RUNNERS)/adam-core/.venv
ADAM_CORE_PY := $(ADAM_CORE_VENV)/bin/python
ADAM_CORE_DATA_DIR ?= $(DATA_DIR)/third-party/adam-core

setup-adam-core: $(ADAM_CORE_PY)
$(ADAM_CORE_PY):
	@echo "──── Setting up adam-core venv (adam-core 0.7.0 + adam-assist 0.6.0) ─"
	@cd $(EMP_VAL_RUNNERS)/adam-core && ./setup.sh

run-adam-core: $(ADAM_CORE_OUT)
$(ADAM_CORE_OUT): $(PLAN) | check-plan fixtures
	@test -x "$(ADAM_CORE_PY)" || { \
	    echo "ERROR: adam-core venv not built: $(ADAM_CORE_PY)"; \
	    echo "       Build it with: make setup-adam-core, or opt out with WITH_ADAM_CORE="; \
	    exit 1; }
	@echo "──── adam-core: external prop + eph + OD reference ─────"
	@mkdir -p $(RESULTS_DIR)
	@$(ADAM_CORE_PY) $(EMP_VAL_RUNNERS)/adam-core/run_adam_core.py \
	    --input $(PLAN) --output $(ADAM_CORE_OUT) \
	    --fixtures-dir $(FIXTURES_PSV) --data-dir "$(ADAM_CORE_DATA_DIR)" \
	    --registered-obscodes "$(DATA_DIR)/obscodes_extended.json" \
	    $(if $(OBJECTS),--objects "$(OBJECTS)",)

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
                $(if $(WITH_ORBFIT),$(ORBFIT_OUT),) $(if $(WITH_LAYUP),$(LAYUP_OUT),) $(if $(WITH_ADAM_CORE),$(ADAM_CORE_OUT),) $(EMP_VAL_BIN)
	@echo "──── Merge ASSIST + find_orb$(if $(WITH_OORB), + OpenOrb,) + kete + jorbit + GRSS$(if $(WITH_ORBFIT), + OrbFit,)$(if $(WITH_LAYUP), + layup,)$(if $(WITH_ADAM_CORE), + adam-core,) into core ──"
	@$(EMP_VAL_BIN) merge-external -i $(CORE_OUT) -o $(CORE_MERGED) \
	    --assist $(ASSIST_OUT) --findorb $(FINDORB_OUT) \
	    --findorb-radar $(FINDORB_RADAR_OUT) \
	    $(if $(WITH_OORB),--oorb $(OORB_OUT),) \
	    --kete $(KETE_OUT) --jorbit $(JORBIT_OUT) \
	    --grss $(GRSS_OUT) --grss-radar $(GRSS_RADAR_OUT) \
	    --jpl-sbdb-cache $(CACHE_DIR)/sbdb \
	    $(if $(WITH_ORBFIT),--orbfit $(ORBFIT_OUT),) \
	    $(if $(WITH_LAYUP),--layup $(LAYUP_OUT),) \
	    $(if $(WITH_ADAM_CORE),--adam-core $(ADAM_CORE_OUT),)
$(RUST_MERGED): $(RUST) $(ASSIST_OUT) $(FINDORB_OUT) $(FINDORB_RADAR_OUT) $(if $(WITH_OORB),$(OORB_OUT),) \
                $(KETE_OUT) $(JORBIT_OUT) $(GRSS_OUT) \
                $(if $(WITH_ORBFIT),$(ORBFIT_OUT),) $(if $(WITH_LAYUP),$(LAYUP_OUT),) $(if $(WITH_ADAM_CORE),$(ADAM_CORE_OUT),) $(EMP_VAL_BIN)
	@echo "──── Merge ASSIST + find_orb$(if $(WITH_OORB), + OpenOrb,) + kete + jorbit + GRSS$(if $(WITH_ORBFIT), + OrbFit,)$(if $(WITH_LAYUP), + layup,)$(if $(WITH_ADAM_CORE), + adam-core,) into rust (fallback) ──"
	@$(EMP_VAL_BIN) merge-external -i $(RUST) -o $(RUST_MERGED) \
	    --assist $(ASSIST_OUT) --findorb $(FINDORB_OUT) \
	    --findorb-radar $(FINDORB_RADAR_OUT) \
	    $(if $(WITH_OORB),--oorb $(OORB_OUT),) \
	    --kete $(KETE_OUT) --jorbit $(JORBIT_OUT) \
	    --grss $(GRSS_OUT) --grss-radar $(GRSS_RADAR_OUT) \
	    --jpl-sbdb-cache $(CACHE_DIR)/sbdb \
	    $(if $(WITH_ORBFIT),--orbfit $(ORBFIT_OUT),) \
	    $(if $(WITH_LAYUP),--layup $(LAYUP_OUT),) \
	    $(if $(WITH_ADAM_CORE),--adam-core $(ADAM_CORE_OUT),)

report: $(RUST) $(PYTHON) $(C_OUT) $(CLI_OUT) $(if $(WITH_CORE),$(CORE_MERGED),$(RUST_MERGED)) $(EMP_VAL_BIN)
	@echo "──── Generating combined HTML report ───────────────────"
	@$(EMP_VAL_BIN) report \
	    --results $(REPORT_INPUTS) \
	    --output $(REPORT) \
	    --summary $(SUMMARY) \
	    $(if $(wildcard $(WALK_AGG)),--predict-agg $(WALK_AGG),)
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
	: > "$(RESULTS_DIR)/comparators-absent.txt"; \
	flags=""; \
	add() { \
	    if [ -f "$$2" ]; then \
	        if [ -n "$$3" ]; then \
	            echo "  NOTE $$1 — marked DISABLED ($$3) yet $$2 IS staged; folding it in anyway. Someone re-enabled the leg without clearing the note."; \
	        fi; \
	        flags="$$flags $$1 $$2"; \
	    elif [ -f "$$2.absent" ]; then \
	        echo "  skip $$1 — NOT MERGED from its legs: $$(head -1 "$$2.absent")"; \
	        printf '%s=%s\n' "$${1#--}" "its legs could not be merged into one file: $$(head -1 "$$2.absent")" >> "$(RESULTS_DIR)/comparators-absent.txt"; \
	    elif [ -n "$$3" ]; then \
	        echo "  skip $$1 — DISABLED by decision: $$3"; \
	    else \
	        echo "  skip $$1 — MISSING: $$2 was not staged. Its matrix leg was expected to run and did not deliver (fail-fast:false keeps the rest alive); the report omits this comparator."; \
	        printf '%s=%s\n' "$${1#--}" "its leg staged no output ($$(basename $$2) is missing), so it is absent from this report" >> "$(RESULTS_DIR)/comparators-absent.txt"; \
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
	add --adam-core      "$(ADAM_CORE_OUT)"        ""; \
	if [ -d "$(CACHE_DIR)/sbdb" ]; then flags="$$flags --jpl-sbdb-cache $(CACHE_DIR)/sbdb"; fi; \
	if [ -s "$(RESULTS_DIR)/comparators-absent.txt" ]; then flags="$$flags --comparator-absent-file $(RESULTS_DIR)/comparators-absent.txt"; fi; \
	$(EMP_VAL_BIN) merge-external -i "$$ref" -o "$$merged" $$flags
	@$(EMP_VAL_BIN) report \
	    --results $(REPORT_INPUTS) \
	    --output $(REPORT) \
	    --summary $(SUMMARY) \
	    $(if $(wildcard $(WALK_AGG)),--predict-agg $(WALK_AGG),)
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
	@rm -f $(REPORT) $(REPORT_DATA) $(SUMMARY) $(RUST_MERGED) $(CORE_MERGED) $(PLAN) $(wildcard $(RESULTS_DIR)/validation_plan.leg-*.json) $(wildcard $(RESULTS_DIR)/*_merged_comparators.json) $(wildcard $(RESULTS_DIR)/comparators-absent.txt)
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
