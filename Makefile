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
.PHONY: all setup build run report clean clean-results clean-all archive help fixtures check-fixtures \
        setup-assist setup-findorb setup-oorb setup-orbfit setup-kete setup-jorbit setup-layup \
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
setup: setup-assist setup-findorb setup-kete setup-jorbit $(if $(WITH_OORB),setup-oorb,) $(if $(WITH_ORBFIT),setup-orbfit,) $(if $(WITH_LAYUP),setup-layup,)
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
# layup all run as part of `make run` and fold into the merged report.
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
     run-kete run-jorbit \
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

$(RUST): $(RUST_PROPEPH) $(RUST_OD)
	@echo "──── Rust channel: merge prop+eph + OD into unified ────"
	@$(WHEEL_PY) -c "import json,sys; \
a=json.load(open('$(RUST_PROPEPH)')); \
b=json.load(open('$(RUST_OD)')); \
sys.exit('ERROR: $(RUST_OD) carries zero OD rows. The rust OD pass produced nothing — a runner that emits no rows at all is a dead channel, not a passing one. Check the fixture fetch (make fixtures) and the runner log above.') if not b else None; \
json.dump(a+b, open('$(RUST)','w'), indent=2, default=str); \
print(f'Wrote {len(a)+len(b)} unified rust rows ({len(a)} prop+eph, {len(b)} OD) to $(RUST)')"

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
$(CORE_MERGED): $(CORE_OUT) $(ASSIST_OUT) $(FINDORB_OUT) $(FINDORB_RADAR_OUT) $(if $(WITH_OORB),$(OORB_OUT),) \
                $(KETE_OUT) $(JORBIT_OUT) \
                $(if $(WITH_ORBFIT),$(ORBFIT_OUT),) $(if $(WITH_LAYUP),$(LAYUP_OUT),) $(EMP_VAL_BIN)
	@echo "──── Merge ASSIST + find_orb$(if $(WITH_OORB), + OpenOrb,) + kete + jorbit$(if $(WITH_ORBFIT), + OrbFit,)$(if $(WITH_LAYUP), + layup,) into core ──"
	@$(EMP_VAL_BIN) merge-external -i $(CORE_OUT) -o $(CORE_MERGED) \
	    --assist $(ASSIST_OUT) --findorb $(FINDORB_OUT) \
	    --findorb-radar $(FINDORB_RADAR_OUT) \
	    $(if $(WITH_OORB),--oorb $(OORB_OUT),) \
	    --kete $(KETE_OUT) --jorbit $(JORBIT_OUT) \
	    --jpl-sbdb-cache $(CACHE_DIR)/sbdb \
	    $(if $(WITH_ORBFIT),--orbfit $(ORBFIT_OUT),) \
	    $(if $(WITH_LAYUP),--layup $(LAYUP_OUT),)
$(RUST_MERGED): $(RUST) $(ASSIST_OUT) $(FINDORB_OUT) $(FINDORB_RADAR_OUT) $(if $(WITH_OORB),$(OORB_OUT),) \
                $(KETE_OUT) $(JORBIT_OUT) \
                $(if $(WITH_ORBFIT),$(ORBFIT_OUT),) $(if $(WITH_LAYUP),$(LAYUP_OUT),) $(EMP_VAL_BIN)
	@echo "──── Merge ASSIST + find_orb$(if $(WITH_OORB), + OpenOrb,) + kete + jorbit$(if $(WITH_ORBFIT), + OrbFit,)$(if $(WITH_LAYUP), + layup,) into rust (fallback) ──"
	@$(EMP_VAL_BIN) merge-external -i $(RUST) -o $(RUST_MERGED) \
	    --assist $(ASSIST_OUT) --findorb $(FINDORB_OUT) \
	    --findorb-radar $(FINDORB_RADAR_OUT) \
	    $(if $(WITH_OORB),--oorb $(OORB_OUT),) \
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
	rm -rf $(ASSIST_VENV) $(KETE_VENV)
	rm -rf $(EMP_VAL_RUNNERS)/findorb/build $(EMP_VAL_RUNNERS)/findorb/install
	rm -f $(C_BIN)
	rm -rf $(EMPYREAN_RUNNERS)/cli/target $(EMPYREAN_RUNNERS)/rust/target
	@echo "Removed all generated artifacts."
