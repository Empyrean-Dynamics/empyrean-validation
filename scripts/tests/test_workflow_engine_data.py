"""Every job that builds an engine context stages the engine kernel set through
the ONE shared action, and a hosted cache miss refuses by name.

Hosted run 44355534056 (full catalog, 112/112 legs green) lost find_orb from the
report: `reduce` merges find_orb's geocentric propagation vectors through
`empyrean::Context::from_data_dir`, but never staged the kernel set, and the
workflow runs offline (EMPYREAN_OFFLINE=1) and sealed, so the engine refused
with "Missing data files" and merge-external isolated the comparator out of the
report. These tests read the workflow text (no YAML dependency) and fail on each
way to bring that back: a job that runs the engine without the action, a job
carrying its own copy of the restore, a cache miss that passes silently, a
fallback to the host mount on a hosted run, and a verification that no longer
names what the engine requires.
"""

import pathlib
import re

ROOT = pathlib.Path(__file__).resolve().parents[2]
WF = ROOT / ".github" / "workflows"
ACTION = ROOT / ".github" / "actions" / "engine-data" / "action.yml"
VALIDATION = (WF / "validation.yml").read_text()

# Jobs that build an engine context from the data directory at run time: the
# channel legs and the heavy legs replay through the engine, the external legs
# read the shared kernels from the same directory, and reduce's merge-external
# converts find_orb's geocentric vectors with an engine context. (prep writes
# the set through engine-setup; reference and assemble legs restore it there.)
DATA_JOBS = ["empyrean", "empyrean-heavy", "external", "reduce"]
USE = "actions/engine-data"

# The names the engine gives when it refuses an offline context (the hosted
# reduce log, run 44355534056): a verification that drops one stops proving the
# set is what the engine needs.
ENGINE_REQUIRED = [
    "de440.bsp",
    "gm_de440.tpc",
    "earth_latest_high_prec.bpc",
    "earth_620120_260806.bpc",
    "earth_2026_260806_2126_predict.bpc",
    "moon_pa_de440_200625.bpc",
    "sb441-n16.bsp",
    "obscodes_extended.json",
    "bias.dat",
]


def jobs(text):
    body = text.split("\njobs:\n", 1)[1]
    parts = re.split(r"^  ([A-Za-z0-9_-]+):\n", body, flags=re.MULTILINE)
    return {parts[i]: parts[i + 1] for i in range(1, len(parts), 2)}


def code(text):
    return "\n".join(ln for ln in text.splitlines() if not ln.lstrip().startswith("#"))


def test_every_job_that_builds_an_engine_context_uses_the_shared_kernel_step():
    all_jobs = jobs(VALIDATION)
    for name in DATA_JOBS:
        text = code(all_jobs[name])
        assert text.count(USE) == 1, f"job {name} must stage the engine kernel set through {USE} exactly once"
        assert "dataset: ${{ env.EMPYREAN_DATASET }}" in text, f"job {name} must pass the registered dataset slug"


def test_the_kernel_step_runs_before_the_job_runs_the_engine():
    all_jobs = jobs(VALIDATION)
    for name in DATA_JOBS:
        text = code(all_jobs[name])
        use = text.index(USE)
        # The first `make`/unpack that can build an engine context. The third-party
        # restore and the act copy of the external job come after the kernel step too.
        for work in ("make reduce", "make fixtures", "make test", "make run", "prebuilt.py unpack"):
            if work in text:
                assert use < text.index(work), f"job {name}: `{work}` runs before the kernel set is staged"


def test_reduce_stages_the_kernel_set_before_the_merge_that_needs_it():
    text = code(jobs(VALIDATION)["reduce"])
    assert text.index(USE) < text.index("make reduce"), "merge-external needs the kernels for find_orb's propagation vectors"


def test_no_job_carries_its_own_copy_of_the_kernel_restore():
    for name, text in jobs(VALIDATION).items():
        body = code(text)
        if name == "prep" or name in ("reference", "reference-heavy"):
            # engine-setup (the writer in prep, the readers in reference) owns these.
            assert "empyrean-data-" not in body, f"job {name} spells the kernel cache key itself"
            continue
        assert "empyrean-data-${{ env.EMPYREAN_DATASET }}-set-only" not in body, f"job {name} restores the kernel cache itself"
        assert "ACT_DATA_DIR" not in body and "/host-data" not in body, f"job {name} copies the host kernel mount itself"
        assert "id: cache-data" not in body, f"job {name} carries its own kernel restore step"


def test_the_external_jobs_third_party_restore_is_left_alone():
    ext = code(jobs(VALIDATION)["external"])
    assert "empyrean-third-party-${{ env.EMPYREAN_DATASET }}-${{ env.THIRD_PARTY_DATASET }}" in ext
    assert "fail-on-cache-miss: true" in ext


def test_reduce_stays_credential_free_beyond_the_publish_grant():
    r = jobs(VALIDATION)["reduce"]
    perms = r.split("permissions:", 1)[1].split("steps:", 1)[0]
    assert re.findall(r"^\s+([a-z-]+): (read|write)$", perms, flags=re.MULTILINE) == [
        ("contents", "read"),
        ("id-token", "write"),
    ]
    action = code(ACTION.read_text())
    for secret in ("secrets.", "GH_TOKEN", "gcloud", "google-github-actions/auth", "id-token"):
        assert secret not in action, f"the kernel step must hold no credential, found `{secret}`"


def test_a_hosted_cache_miss_fails_by_name_and_act_alone_may_use_the_host_mount():
    action = code(ACTION.read_text())
    steps = re.split(r"^    - ", action.split("steps:\n", 1)[1], flags=re.MULTILINE)[1:]
    miss = next(s for s in steps if "Require a warm kernel cache" in s)
    assert "env.ACT != 'true' && steps.cache.outputs.cache-hit != 'true'" in miss
    assert "exit 1" in miss and "empyrean-data-${{ inputs.dataset }}-set-only" in miss, "the miss must exit and name the key"
    copy = next(s for s in steps if "host mount" in s and "(act)" in s)
    assert "env.ACT == 'true' && steps.cache.outputs.cache-hit != 'true'" in copy
    assert "ACT_DATA_DIR:-/host-data" in copy
    # Nothing but the cache restore and the act copy may put files in data/.
    for s in steps:
        if s is not copy:
            assert "cp -a" not in s and "gcloud" not in s and "curl" not in s and "empyrean-data.py" not in s


def test_the_kernel_step_verifies_every_file_the_engine_requires_and_prints_what_it_staged():
    action = code(ACTION.read_text())
    steps = re.split(r"^    - ", action.split("steps:\n", 1)[1], flags=re.MULTILINE)[1:]
    verify = next(s for s in steps if "Verify and list" in s)
    m = re.search(r'required="([^"]*)"', verify)
    assert m, "the verify step must hold its required-file list"
    assert m.group(1).split() == ENGINE_REQUIRED
    assert "[ -s " in verify, "a file must be present AND non-empty"
    assert "exit 1" in verify and "missing or empty" in verify
    assert "bytes" in verify and "engine-data: dataset" in verify, "the step prints what it staged"
    assert "EMPYREAN_DATA_DIR is not set" in verify


def test_the_registered_set_holds_every_file_the_step_requires_when_it_is_on_this_machine():
    import os

    d = pathlib.Path(os.path.expanduser("~/.empyrean/slate-out/dataset:kernels-full-2026-10-13"))
    if not d.is_dir():
        return  # the set is a local mirror; CI proves it by running the step
    for f in ENGINE_REQUIRED:
        assert (d / f).is_file(), f"the registered set lacks {f}, which the engine requires"
