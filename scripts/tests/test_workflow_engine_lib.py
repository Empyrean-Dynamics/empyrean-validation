"""No job in the validation workflows can reach the public engine-release URL,
and every job that builds or runs against the engine says where it got it.

empyrean-sys's build script resolves EMPYREAN_LIB_DIR before it would download,
so the structure that keeps every job off the public URL is: the variable is set
for the whole run, exactly one place populates the directory (the engine-lib
action: authenticated API in prep, offline-verified cache everywhere else), and
every job that compiles or runs engine-linked code populates it first. A job
that forgets panics with "libempyrean not found" rather than downloading. These
tests read the workflow text (no YAML dependency) and fail on each way to break
that.
"""

import pathlib
import re

ROOT = pathlib.Path(__file__).resolve().parents[2]
WF = ROOT / ".github" / "workflows"
ACTIONS = ROOT / ".github" / "actions"
VALIDATION = (WF / "validation.yml").read_text()

# Jobs that compile or run engine-linked code (the harness, the runners).
ENGINE_JOBS = ["prep", "reference", "reference-heavy", "assemble", "assemble-final", "empyrean", "empyrean-heavy", "reduce"]
# Jobs that must not: they run third-party tools in a secretless sandbox.
NON_ENGINE_JOBS = ["precheck", "external"]


def jobs(text):
    """{job name: its text} for the top-level `jobs:` mapping."""
    body = text.split("\njobs:\n", 1)[1]
    parts = re.split(r"^  ([A-Za-z0-9_-]+):\n", body, flags=re.MULTILINE)
    return {parts[i]: parts[i + 1] for i in range(1, len(parts), 2)}


def code_lines(text):
    return [ln for ln in text.splitlines() if not ln.lstrip().startswith("#")]


def first_index(text, needles):
    hits = [text.find(n) for n in needles if n in text]
    return min(hits) if hits else -1


ENGINE_USE = ["actions/engine-lib", "actions/engine-setup"]
ENGINE_WORK = ["make ", "cargo ", "prebuilt.py unpack", "scripts/prebuilt.py"]


def test_the_engine_library_dir_is_set_once_for_the_whole_run():
    top = VALIDATION.split("\njobs:\n", 1)[0]
    assert re.search(r"^  EMPYREAN_LIB_DIR: \$\{\{ github\.workspace \}\}/engine-lib$", top, re.MULTILINE)
    for name, text in jobs(VALIDATION).items():
        assert "EMPYREAN_LIB_DIR" not in "\n".join(code_lines(text)), f"job {name} overrides EMPYREAN_LIB_DIR"
        assert "EMPYREAN_FORCE_DOWNLOAD" not in text, f"job {name} forces empyrean-sys's download"


def test_no_workflow_or_action_names_the_public_release_url():
    for path in [*WF.glob("*.yml"), *ACTIONS.rglob("*.yml")]:
        for ln in code_lines(path.read_text()):
            assert "releases/download" not in ln, f"{path.name}: {ln.strip()}"


def test_every_engine_job_populates_the_library_before_it_builds_or_runs_anything():
    all_jobs = jobs(VALIDATION)
    for name in ENGINE_JOBS:
        text = "\n".join(code_lines(all_jobs[name]))
        use = first_index(text, ENGINE_USE)
        assert use >= 0, f"job {name} builds or runs engine code but never restores the engine library"
        work = first_index(text, ENGINE_WORK)
        assert work < 0 or use < work, f"job {name} builds or runs before the engine library is in place"


def test_the_jobs_that_run_third_party_tools_load_no_engine():
    all_jobs = jobs(VALIDATION)
    for name in NON_ENGINE_JOBS:
        text = "\n".join(code_lines(all_jobs[name]))
        for bad in ("cargo ", "PREBUILT_BIN", "engine-lib", "engine-setup", "prebuilt.py", "gh release"):
            assert bad not in text, f"job {name} must stay engine-free but contains `{bad}`"


def test_only_prep_writes_the_engine_library_and_only_with_a_token():
    for name, text in jobs(VALIDATION).items():
        code = "\n".join(code_lines(text))
        writes = "role: writer" in code
        assert writes == (name == "prep"), f"job {name}: the writer role belongs to prep alone"
    setup = (ACTIONS / "engine-setup" / "action.yml").read_text()
    assert re.search(r"engine-lib\n\s+with:\n\s+role: \$\{\{ inputs\.role \}\}\n\s+token: \$\{\{ inputs\.gh-pat \}\}", setup)


def test_the_engine_lib_action_fetches_through_the_script_verifies_and_logs():
    act = "\n".join(code_lines((ACTIONS / "engine-lib" / "action.yml").read_text()))
    assert "scripts/engine_lib.py fetch" in act and "scripts/engine_lib.py restore" in act
    assert "GH_TOKEN: ${{ inputs.token }}" in act
    assert "inputs.role != 'writer' && steps.cache.outputs.cache-hit != 'true'" in act, "a reader must refuse a cache miss"
    assert "EMPYREAN_LIB=$lib" in act and 'echo "engine-lib: EMPYREAN_LIB_DIR=' in act
    assert "EMPYREAN_LIB_DIR is not set" in act, "the action must refuse to run if the workflow stops setting the directory"
    assert "curl" not in act and "wget" not in act


def test_engine_setup_skips_every_compile_prerequisite_when_asked():
    setup = (ACTIONS / "engine-setup" / "action.yml").read_text()
    steps = re.split(r"^    - ", setup.split("steps:\n", 1)[1], flags=re.MULTILINE)
    gated = [s for s in steps if "inputs.skip-build != 'true'" in s]
    text = "\n".join(gated)
    for needed in ("Checkout empyrean (sibling)", "Checkout empyrean-core (sibling)", "Checkout nolan", "setup-rust-toolchain", "Cache cargo state"):
        assert needed in text, f"{needed} must be skipped under skip-build"
    ungated = "\n".join(s for s in steps if "inputs.skip-build != 'true'" not in s)
    assert "actions/engine-lib" in ungated, "the engine library is set up whether or not the job builds"


def test_the_reference_job_compiles_nothing_and_the_others_only_assert_prebuilt():
    all_jobs = jobs(VALIDATION)
    for name in ("reference", "reference-heavy"):
        assert 'skip-build: "true"' in "\n".join(code_lines(all_jobs[name])), f"{name} compiles"
    for name in ("reference", "reference-heavy", "assemble", "assemble-final", "empyrean", "empyrean-heavy", "reduce"):
        code = "\n".join(code_lines(all_jobs[name]))
        assert "cargo " not in code, f"{name} compiles"
        assert "setup-rust-toolchain" not in code, f"{name} installs a toolchain"
        for line in code.splitlines():
            if re.search(r"\bmake\b.*\b(build|build-\w+)\b", line) and "PREBUILT_BIN=1" not in line:
                raise AssertionError(f"{name}: `{line.strip()}` would compile")
    prep = "\n".join(code_lines(all_jobs["prep"]))
    assert "make build\n" in prep and "prebuilt.py pack" in prep and "name: prebuilt-bin" in prep


def test_the_mint_workflow_also_sets_the_directory_because_it_builds_the_harness():
    mint = (WF / "mint-inputs.yml").read_text()
    top = mint.split("\njobs:\n", 1)[0]
    assert re.search(r"^  EMPYREAN_LIB_DIR: \$\{\{ github\.workspace \}\}/engine-lib$", top, re.MULTILINE)
    assert "engine-setup" in mint, "the mint's engine library comes from engine-setup's engine-lib steps"
