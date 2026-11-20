"""`make shard-reference` end to end with a stand-in runner: the leg's own check
must see the leg's selection. A `$(wildcard)` on a file written earlier in the
same recipe read as absent (make caches directory listings), so the check ran
against an empty selection and refused every result row (act rehearsal, 2026-11-08)."""

import json
import pathlib
import shutil
import subprocess
import sys
import textwrap

import pytest

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
import schedule as sc  # noqa: E402

STUB = textwrap.dedent(
    """\
    #!{py}
    import json, sys
    a = sys.argv[1:]
    out = a[a.index("--output") + 1]
    if a[0] == "run":
        rows = json.load(open(a[a.index("--select") + 1]))
        res = [dict(r, channel="rust", emp_time_ms=1.0, timestamp="t") for r in rows]
        if {extra!r}:
            res.append(dict(rows[0], dt_days=999.0, channel="rust"))
        json.dump(res, open(out, "w"))
        json.dump([], open(out + ".dropped.json", "w"))
    else:
        json.dump([], open(out, "w"))
    """
)


def cand(dt, observer=None):
    tt = "ephemeris" if observer else "propagation"
    return {"object": "A", "test_type": tt, "propagation_uncertainty": "first_order_detection_on", "dt_days": dt, "observer": observer, "force_model": "standard"}


def run_make(tmp, extra):
    cands = [cand(10.0), cand(10.0, "500"), cand(10.0, "F51"), cand(20.0, "500")]
    cp = tmp / "results" / "reference-candidates.json"
    cp.parent.mkdir(parents=True)
    cp.write_text(json.dumps(cands))
    sched = {
        "schema": sc.SCHEMA,
        "kind": "reference",
        "candidates_sha256": sc.file_sha256(cp),
        "objects": ["A"],
        "replicas": [],
        "legs": [{"leg": "01", "rows": [0, 1, 2, 3], "od_objects": [], "predicted_min": 1.0}],
    }
    (tmp / "results" / "reference-schedule.json").write_text(json.dumps(sched))
    stub = tmp / "stub-validate"
    stub.write_text(STUB.format(py=sys.executable, extra=extra))
    stub.chmod(0o755)
    return subprocess.run(
        ["make", "-o", "fixtures", "shard-reference", "LEG=01", f"RESULTS_DIR={tmp / 'results'}", f"RUST_BIN={stub}", f"EMP_VAL_BIN={shutil.which('true')}"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )


@pytest.mark.skipif(shutil.which("make") is None, reason="make unavailable")
def test_the_legs_check_sees_its_selection_when_make_runs_the_whole_recipe(tmp_path):
    r = run_make(tmp_path, extra=False)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "leg OK: 4 prop+eph rows" in r.stdout


@pytest.mark.skipif(shutil.which("make") is None, reason="make unavailable")
def test_the_same_recipe_still_refuses_a_row_the_leg_was_not_given(tmp_path):
    r = run_make(tmp_path, extra=True)
    assert r.returncode != 0
    assert "a result row the leg was not given" in r.stderr
