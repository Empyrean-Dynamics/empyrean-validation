"""The validation workflow's job graph for the heavy decoupling (S5) and the
external object legs (S7), read from the workflow text (no YAML dependency).

The graph is what keeps the heavy row off the critical path and keeps it
honest: the heavy legs start from prep, the plan everything replays is
published by assemble, assemble-final checks the full reference against it and
publishes the reference reduce reads, and reduce takes the heavy rows from the
heavy legs. Each test fails on one way to break that."""

import pathlib
import re

ROOT = pathlib.Path(__file__).resolve().parents[2]
VALIDATION = (ROOT / ".github" / "workflows" / "validation.yml").read_text()


def jobs(text):
    body = text.split("\njobs:\n", 1)[1]
    parts = re.split(r"^  ([A-Za-z0-9_-]+):\n", body, flags=re.MULTILINE)
    return {parts[i]: parts[i + 1] for i in range(1, len(parts), 2)}


def code(text):
    return "\n".join(ln for ln in text.splitlines() if not ln.lstrip().startswith("#"))


JOBS = {k: code(v) for k, v in jobs(VALIDATION).items()}


def needs(job):
    m = re.search(r"^    needs: (.*)$", JOBS[job], re.MULTILINE)
    v = m.group(1).strip()
    return [x.strip() for x in v.strip("[]").split(",")] if v.startswith("[") else [v]


def test_the_heavy_legs_start_from_prep_and_skip_when_nothing_is_heavy():
    for job, out in (("reference-heavy", "reference_heavy_legs"), ("empyrean-heavy", "empyrean_heavy_legs")):
        assert needs(job) == ["prep"], f"{job} must not wait for the reference"
        assert f"if: needs.prep.outputs.{out} != '[]'" in JOBS[job]
        assert f"fromJson(needs.prep.outputs.{out})" in JOBS[job]
        assert f"{out}: ${{{{ steps.schedule-reference.outputs.{out} }}}}" in JOBS["prep"]
    assert "REFERENCE_LEG_SCHEDULE=results/reference-heavy-schedule.json" in JOBS["reference-heavy"]
    heavy = JOBS["empyrean-heavy"]
    assert "PLAN_KIND=heavy" in heavy and "PLAN=results/heavy-plan.json" in heavy and "SCHEDULE=results/schedule-heavy.json" in heavy
    assert "validation_${{ matrix.channel }}_heavy.leg${{ matrix.leg }}.json" in heavy


def test_prep_decouples_and_validates_both_heavy_schedules():
    prep = JOBS["prep"]
    assert "--heavy-schedule results/reference-heavy-schedule.json" in prep
    assert "validate-heavy --heavy-plan results/heavy-plan.json" in prep
    for f in ("heavy.json", "heavy-plan.json", "reference-heavy-schedule.json", "schedule-heavy.json"):
        assert f"empyrean-validation/results/{f}" in prep, f"prep does not upload {f}"


def test_assemble_publishes_the_plan_and_only_assemble_final_publishes_the_reference():
    assert "name: plan\n" in JOBS["assemble"] and "name: prep-plan-and-rust" not in JOBS["assemble"]
    uploads = [j for j, t in JOBS.items() if re.search(r"upload-artifact@v4\n\s+with:\n\s+name: prep-plan-and-rust", t)]
    assert uploads == ["assemble-final"]
    for job in ("empyrean", "external"):
        assert re.search(r"download-artifact@v4\n\s+with:\n\s+name: plan\n", JOBS[job]), f"{job} replays the published plan"
    af = JOBS["assemble-final"]
    assert "validation_rust_heavy_dropped.json" in af and "make assemble-final" in af


def test_assemble_final_waits_for_the_heavy_reference_and_tolerates_none():
    assert set(needs("assemble-final")) >= {"reference-heavy", "assemble"}
    af = JOBS["assemble-final"]
    assert "needs.assemble.result == 'success'" in af
    assert "(needs.reference-heavy.result == 'success' || needs.reference-heavy.result == 'skipped')" in af


def test_reduce_takes_the_heavy_rows_from_the_heavy_legs_and_the_final_reference():
    assert set(needs("reduce")) >= {"assemble-final", "empyrean", "empyrean-heavy", "external"}
    r = JOBS["reduce"]
    assert "needs.assemble-final.result == 'success'" in r
    assert "(needs.empyrean-heavy.result == 'success' || needs.empyrean-heavy.result == 'skipped')" in r
    for flag in ("--heavy-plan results/heavy-plan.json", "--heavy-schedule results/schedule-heavy.json", "--heavy-dir results", "--dropped results/validation_rust_heavy_dropped.json"):
        assert flag in r, f"reduce's channel concat lacks {flag}"
    assert "channel-chunks.py concat-external" in r


def test_external_legs_come_from_the_schedule_with_their_own_timeouts():
    x = JOBS["external"]
    assert "timeout-minutes: ${{ matrix.timeout }}" in x, "an external leg's limit is its own, not a flat budget"
    assert "include: ${{ fromJson(needs.assemble.outputs.external_legs) }}" in x
    assert re.search(r"max-parallel: \d+", x)
    assert 'EXT_LEG="${{ matrix.leg }}"' in x
    assert "name: channel-${{ matrix.tool }}-${{ matrix.leg }}" in x
    assert 'mv "$f" "${f%.json}.leg${{ matrix.leg }}.json"' in x
    assert "make schedule-external" in JOBS["assemble"] and "validate-external" in JOBS["assemble"]
