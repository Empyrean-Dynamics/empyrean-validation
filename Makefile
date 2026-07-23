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
#   make clean     # remove generated JSONs and HTML; keep external installs
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
DATA_DIR ?= $(HOME)/.empyrean/data
CACHE_DIR ?= $(HOME)/.empyrean/cache
RESULTS_DIR := $(ROOT)/results
FIXTURES_PSV := $(ROOT)/fixtures/psv
# Radar-augmented fixtures (optical + ADES <radar> table) for the objects with
# radar astrometry; drives the find_orb radar-OD reference pass.
FIXTURES_PSV_RADAR := $(ROOT)/fixtures/psv-radar

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

# External-reference runners (assist / findorb / kete / oorb / jorbit)
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

# DYLD path for runtime linking against libempyrean.dylib (built by
# empyrean-c into the empyrean repo's target/release).
DYLD := DYLD_LIBRARY_PATH="$(EMPYREAN_ROOT)/target/release"

# CLI args
ONLY_FLAG := $(if $(OBJECTS),--only "$(OBJECTS)",)

# Output JSONs
#
# `validation_plan.json` is the canonical test fixture every replay channel
# (python / c / cli / core / assist / findorb / kete) consumes — it carries
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
.PHONY: all setup build run report clean help \
        setup-assist setup-findorb setup-oorb setup-orbfit setup-kete setup-jorbit setup-layup \
        build-empyrean-c build-rust build-c build-cli build-wheel build-core build-empyrean-validation \
        run-rust run-python run-c run-cli run-assist run-findorb run-oorb run-orbfit \
        run-core run-kete run-jorbit run-layup \
        merge-external plan

help:
	@echo "Targets:"
	@echo "  make setup     # one-time: ASSIST venv + DE440 + find_orb"
	@echo "  make build     # rust + C + CLI + wheel$(if $(WITH_CORE), + core,) + validation CLI"
	@echo "  make run       # all channels$(if $(WITH_CORE), (incl. core),)"
	@echo "  make report    # merged HTML + summary"
	@echo "  make all       # build + run + report"
	@echo "  make clean     # remove generated JSONs + HTML"
	@echo
	@echo "Channels: rust, python, c, cli$(if $(WITH_CORE), + core,)"
	@echo "Variables:  OBJECTS='A,B' TIERS=standard DATA_DIR=$(DATA_DIR)"
	@echo
	@echo "Layout: empyrean=$(EMPYREAN_ROOT)"
	@echo "        empyrean-core=$(EMPYREAN_CORE_ROOT)"
	@echo "        empyrean-validation=$(EMPYREAN_VALIDATION_ROOT)"

all: build run report

# ── Setup (one-time) ───────────────────────────────────────
# OrbFit's runner is gated on WITH_ORBFIT (see `run`), so only set it up
# when it will actually run — otherwise `make setup` pulls a Docker image
# for a comparator that never executes (and fails the setup if Docker is
# unavailable).
setup: setup-assist setup-findorb setup-oorb setup-kete setup-jorbit $(if $(WITH_ORBFIT),setup-orbfit,) $(if $(WITH_LAYUP),setup-layup,)
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

# ── Run pipeline ───────────────────────────────────────────
# Channel order: rust first (produces the unified $(RUST) input every other
# channel consumes), then the four replay channels (python / c / cli / core),
# then the externals. Each non-rust channel reads exactly one file and writes
# exactly one file.
# Public-report channel set: ASSIST + OrbFit + OpenOrb + find_orb are
# the four canonical externals surfaced in the headline report. kete +
# jorbit stay as opt-in runners — invoke `make run-kete` / `make run-jorbit`
# explicitly to produce their JSONs, and pass `INCLUDE_OPTIONAL=1` to
# the `report` target to surface them in the rendered HTML.
#
# OrbFit ships its runner via Docker but is not yet end-to-end
# (Cartesian seed orbit + equinoctial→Cartesian conversion pending).
# Set `WITH_ORBFIT=1` to opt in once those are in place.
WITH_ORBFIT ?=
# layup is opt-in too (WITH_LAYUP=1) — brand-new upstream (v0.0.1) with a heavy
# C-extension build, so it stays out of the default `make all` path, same as
# OrbFit.
WITH_LAYUP ?=
run: run-rust run-python run-c run-cli run-assist run-findorb run-oorb \
     run-kete run-jorbit \
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

$(RUST_OD): $(RUST_BIN)
	@echo "──── Rust channel: orbit determination ─────────────────"
	@$(DYLD) $(RUST_BIN) od $(ONLY_FLAG) --tier $(TIERS) \
	    --data-dir $(DATA_DIR) \
	    --fixtures-dir $(FIXTURES_PSV) \
	    --output $(RUST_OD)

$(RUST): $(RUST_PROPEPH) $(RUST_OD)
	@echo "──── Rust channel: merge prop+eph + OD into unified ────"
	@$(WHEEL_PY) -c "import json; \
a=json.load(open('$(RUST_PROPEPH)')); \
b=json.load(open('$(RUST_OD)')); \
json.dump(a+b, open('$(RUST)','w'), indent=2, default=str); \
print(f'Wrote {len(a)+len(b)} unified rust rows ({len(a)} prop+eph, {len(b)} OD) to $(RUST)')"

# ── Test plan ──────────────────────────────────────────────
# The plan is the canonical test fixture: same row schema as a channel
# output, but with channel-specific result fields nulled out (emp_*, od_*,
# separation_arcsec, channel set to "plan"). Every replay channel reads
# the plan instead of validation_rust.json so they don't depend on rust's
# results.
plan: $(PLAN)
$(PLAN): $(RUST)
	@echo "──── Plan: strip channel-specific fields from rust unified ─"
	@$(WHEEL_PY) -c "import json; \
rows=json.load(open('$(RUST)')); \
clear_fields=['emp_pos_au','emp_time_ms','emp_vs_horizons_km','separation_arcsec','d_ra_arcsec','d_dec_arcsec','d_rho_km','d_light_time_s','od_iterations','od_converged','od_rms_ra_arcsec','od_rms_dec_arcsec','od_rms_combined_arcsec','od_chi2','od_reduced_chi2','od_a1','od_a2','od_a3','od_a1_sigma','od_a2_sigma','od_a3_sigma','assist_vs_horizons_km','emp_vs_assist_km','assist_time_ms','speed_ratio','findorb_rms_residual','findorb_n_obs_used','findorb_n_obs_rejected']; \
rows=[r for r in rows if r.get('propagation_uncertainty') not in ('auto', 'second_order_with_cov')]; \
[r.update({f: None for f in clear_fields}) for r in rows]; \
[r.update(channel='plan') for r in rows]; \
json.dump(rows, open('$(PLAN)','w'), indent=2, default=str); \
print(f'Wrote {len(rows)} plan rows to $(PLAN) (auto + second-order excluded — rust-only axes)')"

run-python: $(PLAN)
	@echo "──── Python channel: replay plan ───────────────────────"
	@$(WHEEL_PY) $(EMPYREAN_RUNNERS)/python/run.py \
	    --input $(PLAN) --output $(PYTHON) \
	    --fixtures-dir $(FIXTURES_PSV) \
	    --data-dir $(DATA_DIR)

run-c: $(PLAN) $(C_BIN)
	@echo "──── C channel: replay plan (prop / eph / OD) ──────────"
	@$(WHEEL_PY) $(EMPYREAN_RUNNERS)/c/drive.py \
	    --input $(PLAN) \
	    --output $(C_OUT) --runner $(C_BIN) \
	    --fixtures-dir $(FIXTURES_PSV) \
	    $(if $(filter-out $(HOME)/.empyrean/data,$(DATA_DIR)),--data-dir $(DATA_DIR),)

run-cli: $(PLAN) $(CLI_BIN)
	@echo "──── CLI channel: fork-exec one binary per plan row ────"
	@$(DYLD) $(WHEEL_PY) $(EMPYREAN_RUNNERS)/cli/drive.py \
	    --input $(PLAN) \
	    --output $(CLI_OUT) --runner $(CLI_BIN) \
	    --fixtures-dir $(FIXTURES_PSV) \
	    $(if $(filter-out $(HOME)/.empyrean/data,$(DATA_DIR)),--data-dir $(DATA_DIR),)

run-assist: $(PLAN) $(ASSIST_PY)
	@echo "──── ASSIST: external propagator reference ─────────────"
	@$(ASSIST_PY) $(EMP_VAL_RUNNERS)/assist/run_assist.py $(PLAN) \
	    --output $(ASSIST_OUT) \
	    --horizons-cache $(CACHE_DIR)/horizons \
	    --data-dir $(DATA_DIR)

# find_orb's binary is an optional, non-fatal build (see findorb/setup.sh).
# When it's absent, skip the comparison and emit empty result files so the
# merge step still has valid (empty) inputs — the missing comparator is
# surfaced here and by its absence from the report, never silently faked.
run-findorb: $(ASSIST_PY)
	@if [ -x "$(FO_BIN)" ]; then \
	    echo "──── find_orb: external OD reference ───────────────────"; \
	    $(ASSIST_PY) $(EMP_VAL_RUNNERS)/findorb/run_findorb.py $(FIXTURES_PSV) \
	        --output $(FINDORB_OUT) --fo-binary $(FO_BIN) \
	        --data-dir $(DATA_DIR) --plan $(PLAN); \
	    echo "──── find_orb: radar-augmented OD reference (psv-radar) ─"; \
	    $(ASSIST_PY) $(EMP_VAL_RUNNERS)/findorb/run_findorb.py $(FIXTURES_PSV_RADAR) \
	        --output $(FINDORB_RADAR_OUT) --fo-binary $(FO_BIN) \
	        --data-dir $(DATA_DIR) --test-type orbit_determination_radar; \
	else \
	    echo "──── find_orb: SKIPPED (binary not built — see setup warning) ──"; \
	    echo '[]' > $(FINDORB_OUT); \
	    echo '[]' > $(FINDORB_RADAR_OUT); \
	fi

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
# oorb's binary is an optional, non-fatal build (see oorb/setup.sh). When
# it's absent, skip the comparison and emit an empty result file so the
# merge step still has a valid input — the missing comparator is surfaced
# here and by its absence from the report, never silently faked.
$(OORB_OUT): $(PLAN) $(ASSIST_PY)
	@if [ -x "$(OORB_BIN)" ]; then \
	    echo "──── OpenOrb: external prop + ephemeris reference ──────"; \
	    $(ASSIST_PY) $(EMP_VAL_RUNNERS)/oorb/run_oorb.py \
	        --input $(PLAN) --output $(OORB_OUT) \
	        --prefix $(EMP_VAL_RUNNERS)/oorb/install; \
	else \
	    echo "──── OpenOrb: SKIPPED (binary not built — see setup warning) ──"; \
	    echo '[]' > $(OORB_OUT); \
	fi

# ── OrbFit external comparison — orbit determination ─────────────
# OrbFit Consortium (University of Pisa) / IAU Minor Planet Center.
# Canonical implementation of CMC2003 χ²-with-hysteresis rejection.
# Setup pulls the MPC's Docker container; runner shells out via docker.
setup-orbfit:
	@echo "──── Pulling OrbFit container ──────────────────────────"
	@cd $(EMP_VAL_RUNNERS)/orbfit && ./setup.sh

run-orbfit: $(ORBFIT_OUT)
$(ORBFIT_OUT): $(PLAN)
	@echo "──── OrbFit: external OD reference (via docker) ────────"
	@$(EMP_VAL_RUNNERS)/orbfit/run_orbfit.py \
	    --plan $(PLAN) --output $(ORBFIT_OUT)

# Optional: empyrean-core direct (no FFI). Replays the plan in-process so
# the report's Section 09 can show binding-translation drift (rust wrapper
# vs the core baseline) alongside the C / CLI / Python channels.
run-core: $(if $(WITH_CORE),$(CORE_OUT),)
ifneq ($(WITH_CORE),1)
	@echo "──── Core channel skipped (empyrean-core not found) ────"
endif

$(CORE_OUT): $(PLAN) $(CORE_BIN)
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
$(KETE_OUT): $(PLAN) $(KETE_PY)
	@echo "──── Kete: external all-test-types reference ──────────"
	@$(KETE_PY) $(EMP_VAL_RUNNERS)/kete/run_kete.py \
	    --input $(PLAN) --output $(KETE_OUT) \
	    --fixtures-dir $(FIXTURES_PSV)

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
$(JORBIT_OUT): $(PLAN) $(JORBIT_PY)
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
# layup's venv is an optional, heavy build (see layup/setup.sh). When it's
# absent, skip the fit and emit an empty result file so the merge step still
# has a valid input — the missing comparator is surfaced here and by its
# absence from the report, never silently faked.
$(LAYUP_OUT): $(FIXTURES_PSV)
	@if [ -x "$(LAYUP_PY)" ]; then \
	    echo "──── layup: external OD reference (ADES PSV) ───────────"; \
	    $(LAYUP_PY) $(EMP_VAL_RUNNERS)/layup/run_layup.py $(FIXTURES_PSV) \
	        --output $(LAYUP_OUT); \
	else \
	    echo "──── layup: SKIPPED (venv not built — run 'make setup-layup') ──"; \
	    mkdir -p $(RESULTS_DIR); \
	    echo '[]' > $(LAYUP_OUT); \
	fi

# ── Merge external + report ────────────────────────────────
# Channel-agnostic meta operations live in this repo's CLI binary. It
# owns the schema, so its merge / report / ci-check stays in lockstep
# with the row format every channel runner emits.
#
# Headline externals folded onto rust rows by default: ASSIST + find_orb
# + OpenOrb + OrbFit. kete + jorbit are opt-in — their per-row data is
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
$(CORE_MERGED): $(CORE_OUT) $(ASSIST_OUT) $(FINDORB_OUT) $(FINDORB_RADAR_OUT) $(OORB_OUT) \
                $(KETE_OUT) $(JORBIT_OUT) \
                $(if $(WITH_ORBFIT),$(ORBFIT_OUT),) $(if $(WITH_LAYUP),$(LAYUP_OUT),) $(EMP_VAL_BIN)
	@echo "──── Merge ASSIST + find_orb + OpenOrb + kete + jorbit$(if $(WITH_ORBFIT), + OrbFit,)$(if $(WITH_LAYUP), + layup,) into core ──"
	@$(EMP_VAL_BIN) merge-external -i $(CORE_OUT) -o $(CORE_MERGED) \
	    --assist $(ASSIST_OUT) --findorb $(FINDORB_OUT) \
	    --findorb-radar $(FINDORB_RADAR_OUT) \
	    --oorb $(OORB_OUT) \
	    --kete $(KETE_OUT) --jorbit $(JORBIT_OUT) \
	    --jpl-sbdb-cache $(CACHE_DIR)/sbdb \
	    $(if $(WITH_ORBFIT),--orbfit $(ORBFIT_OUT),) \
	    $(if $(WITH_LAYUP),--layup $(LAYUP_OUT),)
$(RUST_MERGED): $(RUST) $(ASSIST_OUT) $(FINDORB_OUT) $(FINDORB_RADAR_OUT) $(OORB_OUT) \
                $(KETE_OUT) $(JORBIT_OUT) \
                $(if $(WITH_ORBFIT),$(ORBFIT_OUT),) $(if $(WITH_LAYUP),$(LAYUP_OUT),) $(EMP_VAL_BIN)
	@echo "──── Merge ASSIST + find_orb + OpenOrb + kete + jorbit$(if $(WITH_ORBFIT), + OrbFit,)$(if $(WITH_LAYUP), + layup,) into rust (fallback) ──"
	@$(EMP_VAL_BIN) merge-external -i $(RUST) -o $(RUST_MERGED) \
	    --assist $(ASSIST_OUT) --findorb $(FINDORB_OUT) \
	    --findorb-radar $(FINDORB_RADAR_OUT) \
	    --oorb $(OORB_OUT) \
	    --kete $(KETE_OUT) --jorbit $(JORBIT_OUT) \
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

# ── Cleanup ────────────────────────────────────────────────
clean:
	rm -rf $(RESULTS_DIR)
	@echo "Removed $(RESULTS_DIR). External deps preserved."

clean-all: clean
	rm -rf $(ASSIST_VENV) $(KETE_VENV)
	rm -rf $(EMP_VAL_RUNNERS)/findorb/build $(EMP_VAL_RUNNERS)/findorb/install
	rm -f $(C_BIN)
	rm -rf $(EMPYREAN_RUNNERS)/cli/target $(EMPYREAN_RUNNERS)/rust/target
	@echo "Removed all generated artifacts."
