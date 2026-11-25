"""One archive timestamp feeds the render and both archive copies.

The report's cite block carries the permanent archive link
(`archive/<timestamp>/validation_report.html`). The publish step used to mint the
timestamp after the report was rendered, so the render could not know it. These
tests read the workflow and the Makefile and RUN the pieces, and fail on each way
to bring that back:

  * the timestamp is minted in more than one place, or after the report is
    rendered, or for a run that will not publish (its report would then print a
    link to a page that never exists);
  * `make reduce` does not hand the value to the renderer, or hands one when the
    run has none (an unpublished render must say so, with no link);
  * the upload does not file BOTH archive copies (the page and its channels) under
    the very value the render was given, or mints its own when none was minted.
"""

import os
import pathlib
import re
import stat
import subprocess
import textwrap

ROOT = pathlib.Path(__file__).resolve().parents[2]
WORKFLOW = (ROOT / ".github" / "workflows" / "validation.yml").read_text()

BUCKET = "gs://test-bucket"
EMPYREAN_CHANNELS = ["rust", "python", "c", "cli", "core"]
STAMP = re.compile(r"date -u \+%Y%m%d_%H%M%S")


def reduce_job():
    body = WORKFLOW.split("\njobs:\n", 1)[1]
    parts = re.split(r"^  ([A-Za-z0-9_-]+):\n", body, flags=re.MULTILINE)
    return {parts[i]: parts[i + 1] for i in range(1, len(parts), 2)}["reduce"]


def steps():
    pieces = re.split(r"^      - name: (.*)$", reduce_job(), flags=re.MULTILINE)
    out = []
    for i in range(1, len(pieces), 2):
        text = re.split(r"^      # ──", pieces[i + 1], flags=re.MULTILINE)[0]
        out.append((pieces[i].strip(), text))
    return out


def step(prefix):
    for n, t in steps():
        if n.startswith(prefix):
            return n, t
    raise AssertionError(f"no step named {prefix!r} in the reduce job")


def index(prefix):
    for i, (n, _) in enumerate(steps()):
        if n.startswith(prefix):
            return i
    raise AssertionError(f"no step named {prefix!r} in the reduce job")


def run_block(text):
    m = re.search(r"^        run: (?:\|\n((?:          .*\n|\n)+)|(.*)\n)", text, flags=re.MULTILINE)
    assert m, "the step has no run block"
    return textwrap.dedent(m.group(1)) if m.group(1) else m.group(2)


def fake_bin(tmp, name, body):
    d = tmp / "bin"
    d.mkdir(exist_ok=True)
    p = d / name
    p.write_text("#!/bin/bash\n" + body)
    p.chmod(p.stat().st_mode | stat.S_IEXEC)
    return d


def test_the_timestamp_is_minted_once_before_the_render_and_only_for_a_publishing_run():
    assert len(STAMP.findall(WORKFLOW)) == 1, "the archive timestamp must be minted in exactly one place"
    name, text = step("Mint the archive timestamp")
    assert STAMP.search(text), "the mint step must be the place that mints it"
    assert "$GITHUB_ENV" in text, "the value must reach the later steps through the job environment"
    assert "if: steps.publish-gate.outputs.publish == 'true'" in text, (
        "a run that will not publish must not mint a timestamp: its report would link a page that never exists"
    )
    gate, mint = index("Decide whether this run may publish"), index(name)
    render, check, upload = index("Merge + report"), index("ci-check"), index("Upload to GCS")
    assert gate < mint < render, "decide, then mint, then render: the render needs the value"
    assert render < check < upload, "the report is rendered before the gate and the upload"
    assert not STAMP.search(step("Upload to GCS")[1]), "the upload step must not mint its own timestamp"


def test_the_decision_to_publish_does_not_wait_on_the_render():
    # The decision is made before the render, so it may read only what exists then.
    _, text = step("Decide whether this run may publish")
    for later in ("results/", "validation_summary", "steps.merge", "needs.reduce"):
        assert later not in text, f"the publish decision reads `{later}`, which does not exist before the render"


def make_reduce(env_extra):
    env = {k: v for k, v in os.environ.items() if k != "ARCHIVE_TIMESTAMP"}
    env.update(env_extra)
    r = subprocess.run(
        ["make", "-n", "reduce", "PREBUILT_BIN=1", "PLAN_PREBUILT=1", "WITH_CORE=1"],
        cwd=ROOT,
        env=env,
        capture_output=True,
        text=True,
    )
    assert r.returncode == 0, r.stderr
    return r.stdout


def test_make_reduce_hands_the_value_to_the_renderer_and_only_when_there_is_one():
    out = make_reduce({"ARCHIVE_TIMESTAMP": "20990101_000001"})
    assert "--archive-timestamp 20990101_000001" in out, "the render does not receive the minted value"
    assert out.count("--archive-timestamp") == 1
    report_call = out[out.index(" report \\") :]
    assert "--archive-timestamp 20990101_000001" in report_call, "the value goes to `report`, not merge-external"
    bare = make_reduce({})
    assert "--archive-timestamp" not in bare, "an unpublished run must render with no timestamp"


def test_one_value_feeds_the_render_and_both_archive_copies(tmp_path):
    # Run the mint step exactly as the job does, into a GITHUB_ENV file.
    gh_env = tmp_path / "github_env"
    gh_env.write_text("")
    _, text = step("Mint the archive timestamp")
    r = subprocess.run(
        ["bash", "-e", "-c", run_block(text)],
        env={**os.environ, "GITHUB_ENV": str(gh_env)},
        capture_output=True,
        text=True,
    )
    assert r.returncode == 0, r.stderr
    (line,) = gh_env.read_text().splitlines()
    name, value = line.split("=", 1)
    assert name == "ARCHIVE_TIMESTAMP" and re.fullmatch(r"\d{8}_\d{6}", value), line

    # The render receives it.
    assert f"--archive-timestamp {value}" in make_reduce({name: value})

    # The upload files both archive copies under the same value.
    results = tmp_path / "results"
    results.mkdir()
    for f in (
        "validation_core_merged.json",
        "validation_rust_od.json",
        "validation.json",
        "od_validation.json",
        "validation_report.html",
        "validation_report.data.json",
        "validation_summary.json",
    ):
        (results / f).write_text("{}")
    for ch in EMPYREAN_CHANNELS:
        (results / f"validation_{ch}.json").write_text("{}")
    log = tmp_path / "gcloud.log"
    bindir = fake_bin(tmp_path, "gcloud", f'echo "$@" >> "{log}"\n')
    _, up = step("Upload to GCS")
    script = run_block(up).replace("${{ env.GCS_BUCKET }}", BUCKET)
    env = {
        **os.environ,
        "PATH": f"{bindir}:{os.environ['PATH']}",
        "EXTERNAL_TOOLS": "assist,grss",
        name: value,
    }
    r = subprocess.run(["bash", "-e", "-c", script], cwd=tmp_path, env=env, capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    dests = [c.split()[-1] for c in log.read_text().splitlines() if c.startswith("storage cp ")]
    archive = [d for d in dests if "/archive/" in d]
    assert archive, "nothing was filed under archive/"
    assert all(d.startswith(f"{BUCKET}/archive/{value}/") for d in archive), archive
    assert f"{BUCKET}/archive/{value}/validation_report.html" in archive, "the report page is not filed under the minted value"
    assert any(d.startswith(f"{BUCKET}/archive/{value}/channels/") for d in archive), "the channel copies are not"


def test_the_upload_refuses_to_mint_its_own_timestamp(tmp_path):
    bindir = fake_bin(tmp_path, "gcloud", "exit 0\n")
    _, up = step("Upload to GCS")
    script = run_block(up).replace("${{ env.GCS_BUCKET }}", BUCKET)
    env = {k: v for k, v in os.environ.items() if k != "ARCHIVE_TIMESTAMP"}
    env.update({"PATH": f"{bindir}:{os.environ['PATH']}", "EXTERNAL_TOOLS": "assist"})
    r = subprocess.run(["bash", "-e", "-c", script], cwd=tmp_path, env=env, capture_output=True, text=True)
    assert r.returncode != 0, "the upload must fail, by name, when no timestamp was minted before the render"
    assert "the archive timestamp was not minted" in r.stderr
