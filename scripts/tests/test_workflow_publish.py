"""The publish step of the reduce job, read from the workflow text and RUN.

The first rc.1 publish left GRSS's channel file out of the bucket (its name was
missing from a hand-written copy list) and went out served as application/json,
so a browser showed the source instead of the report. These tests fail on each
way to repeat that:

  * a comparator the run was asked for (EXTERNAL_TOOLS) that a publish loop does
    not copy, in `latest/` or in `archive/`. The loop is executed against a fake
    `gcloud`, with a comparator the workflow has never heard of added to the
    run's list, so a hand-written list cannot pass;
  * a missing post-publish content-type check, one that runs before the copies,
    or one that does not fail the job, by name, unless the public report is
    served as text/html (the check is executed against a fake `curl`).
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


def reduce_job():
    body = WORKFLOW.split("\njobs:\n", 1)[1]
    parts = re.split(r"^  ([A-Za-z0-9_-]+):\n", body, flags=re.MULTILINE)
    return {parts[i]: parts[i + 1] for i in range(1, len(parts), 2)}["reduce"]


def steps(job):
    """(name, text) of each step in order; a step runs to the next step or the
    next comment banner."""
    pieces = re.split(r"^      - name: (.*)$", job, flags=re.MULTILINE)
    out = []
    for i in range(1, len(pieces), 2):
        text = re.split(r"^      # ──", pieces[i + 1], flags=re.MULTILINE)[0]
        out.append((pieces[i].strip(), text))
    return out


def step(name_prefix):
    for n, t in steps(reduce_job()):
        if n.startswith(name_prefix):
            return n, t
    raise AssertionError(f"no step named {name_prefix!r} in the reduce job")


def run_block(step_text):
    m = re.search(r"^        run: \|\n((?:          .*\n|\n)+)", step_text, flags=re.MULTILINE)
    assert m, "the step has no run block"
    return textwrap.dedent(m.group(1))


def default_external_tools():
    m = re.search(r"EXTERNAL_TOOLS: \$\{\{ inputs\.external_tools \|\| '([^']+)' \}\}", WORKFLOW)
    assert m, "EXTERNAL_TOOLS default not found"
    return m.group(1).split(",")


def fake_bin(tmp, name, body):
    d = tmp / "bin"
    d.mkdir(exist_ok=True)
    p = d / name
    p.write_text("#!/bin/bash\n" + body)
    p.chmod(p.stat().st_mode | stat.S_IEXEC)
    return d


def test_every_comparator_is_copied_in_both_publish_loops(tmp_path):
    tools = default_external_tools() + ["brand-new-comparator"]
    results = tmp_path / "results"
    results.mkdir()
    for f in ("validation_core_merged.json", "validation_rust_od.json", "validation.json", "od_validation.json",
              "validation_report.html", "validation_report.data.json", "validation_summary.json"):
        (results / f).write_text("{}")
    for ch in EMPYREAN_CHANNELS + tools:
        (results / f"validation_{ch}.json").write_text("{}")
    for radar in ("findorb", "grss"):
        (results / f"validation_{radar}_radar.json").write_text("{}")
    log = tmp_path / "gcloud.log"
    bindir = fake_bin(tmp_path, "gcloud", f'echo "$@" >> "{log}"\n')

    _, text = step("Upload to GCS")
    script = run_block(text).replace("${{ env.GCS_BUCKET }}", BUCKET)
    env = {
        **os.environ,
        "PATH": f"{bindir}:{os.environ['PATH']}",
        "EXTERNAL_TOOLS": ",".join(tools),
        # Minted before the report was rendered (test_workflow_archive_timestamp.py).
        "ARCHIVE_TIMESTAMP": "20990101_000000",
    }
    r = subprocess.run(["bash", "-e", "-c", script], cwd=tmp_path, env=env, capture_output=True, text=True)
    assert r.returncode == 0, r.stderr

    copies = log.read_text().splitlines()
    dests = [c.split()[-1] for c in copies if c.startswith("storage cp ")]
    for ch in EMPYREAN_CHANNELS + tools:
        for root in ("latest", "archive"):
            hits = [d for d in dests if d.startswith(f"{BUCKET}/{root}/") and d.endswith(f"/channels/{ch}.json")]
            assert len(hits) == 1, f"{ch} was copied {len(hits)} times under {root}/ (want 1): the publish loop misses it"
    for radar in ("findorb", "grss"):
        assert any(d.endswith(f"/channels/{radar}_radar.json") for d in dests), f"{radar}'s radar pass is not published"
    # GRSS is the comparator the first publish dropped; name it.
    assert "grss" in default_external_tools()


def test_the_publish_loops_are_not_a_hand_written_list():
    _, text = step("Upload to GCS")
    assert "for ch in rust python c cli core assist" not in text, "a literal comparator list is back in a publish loop"
    assert text.count("copy_channels ") >= 2, "both the latest/ and archive/ copies must use the one loop"
    assert "${EXTERNAL_TOOLS}" in text, "the loop must come from the run's EXTERNAL_TOOLS"


def test_the_content_type_check_follows_the_copies_and_gates_on_publishing():
    names = [n for n, _ in steps(reduce_job())]
    up = next(i for i, n in enumerate(names) if n.startswith("Upload to GCS"))
    chk = next((i for i, n in enumerate(names) if n.startswith("Check the published report is served as HTML")), None)
    assert chk is not None, "no post-publish content-type check step"
    assert chk > up, "the content-type check must run after the copies"
    _, text = step("Check the published report is served as HTML")
    assert "if: steps.publish-gate.outputs.publish == 'true'" in text
    assert "https://validation.empyrean-dynamics.com/latest/validation_report.html" in text
    assert "0b94c" in text, "the intended failure must be explained (bead 0b94c)"


def run_check(tmp_path, content_type):
    headers = "HTTP/2 200\\r\\n" + (f"Content-Type: {content_type}\\r\\n" if content_type else "") + "\\r\\n"
    bindir = fake_bin(tmp_path, "curl", f'printf "{headers}"\n')
    _, text = step("Check the published report is served as HTML")
    env = {**os.environ, "PATH": f"{bindir}:{os.environ['PATH']}"}
    return subprocess.run(["bash", "-e", "-c", run_block(text)], env=env, capture_output=True, text=True)


def test_the_check_fails_by_name_unless_the_report_is_html(tmp_path):
    bad = run_check(tmp_path, "application/json")
    assert bad.returncode != 0, "a report served as application/json must fail the job"
    assert "::error title=Published report is not served as HTML" in bad.stdout
    assert "application/json" in bad.stdout
    none = run_check(tmp_path, None)
    assert none.returncode != 0, "a response with no content-type must fail the job"
    good = run_check(tmp_path, "text/html; charset=utf-8")
    assert good.returncode == 0, good.stdout + good.stderr
    upper = run_check(tmp_path, "TEXT/HTML")
    assert upper.returncode == 0, "the media type is case-insensitive"
