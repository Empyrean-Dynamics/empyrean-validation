"""``--reps-override``: a replica leg of the heavy row runs it once.

The driver sends a ``reps N`` line ahead of the row's prop / eph line and the
daemon applies it to that line alone. The override is a side file keyed by the
row's identity, never a plan key. A stand-in daemon records the lines it
receives; the end-to-end test drives the real binary (skipped when it or the
data tier is unavailable) and reads the repetition count it applied.
"""

from __future__ import annotations

import json
import os
import stat
import subprocess
import sys
from pathlib import Path

import drive
import pytest

RECORDING_RUNNER = """#!{python}
import sys
print("ready", file=sys.stderr, flush=True)
log = open({log!r}, "a")
for line in sys.stdin:
    log.write(line.split()[0] + "\\n")
    log.flush()
    if line.startswith("reps"):
        continue  # the daemon replies to nothing
    print("fail stand-in", flush=True)
"""


def _row(test_type: str, dt: float, **kw) -> dict:
    r = {
        "object": "HEAVYOBJ",
        "test_type": test_type,
        "population": "NEO",
        "force_model": "standard",
        "propagation_uncertainty": "auto_detection_on",
        "dt_days": dt,
        "observer": "500" if test_type == "ephemeris" else None,
        "ic_pos_au": [1.0, 0.0, 0.0],
        "ic_vel_au_d": [0.0, 0.017, 0.0],
        "epoch_mjd_tdb": 60000.0,
        "t_mjd_tdb": 60000.0 + dt,
        "excluded_perturbers_naif": [],
    }
    r.update(kw)
    return r


def _entry(row: dict, reps) -> dict:
    keys = ("object", "test_type", "propagation_uncertainty", "dt_days", "observer", "force_model")
    return {**{k: row[k] for k in keys}, "reps": reps}


@pytest.fixture
def leg(tmp_path, monkeypatch):
    heavy, light = _row("propagation", -5475.0), _row("propagation", -3650.0)
    eph = _row("ephemeris", -5475.0)
    (tmp_path / "leg.json").write_text(json.dumps([heavy, light, eph]))
    log = tmp_path / "lines.log"
    runner = tmp_path / "recording-runner"
    runner.write_text(RECORDING_RUNNER.format(python=sys.executable, log=str(log)))
    runner.chmod(runner.stat().st_mode | stat.S_IXUSR)
    fixtures = tmp_path / "psv"
    fixtures.mkdir()

    def go(*extra):
        argv = ["drive.py", "--input", str(tmp_path / "leg.json"), "--output", str(tmp_path / "out.json"), "--runner", str(runner), "--fixtures-dir", str(fixtures), *extra]
        monkeypatch.setattr(sys, "argv", argv)
        drive.main()
        return log.read_text().split() if log.exists() else []

    return go, tmp_path, heavy, light, eph


def test_the_overridden_row_is_preceded_by_a_reps_line_and_no_other_row_is(leg):
    go, tmp, heavy, _light, eph = leg
    (tmp / "reps.json").write_text(json.dumps([_entry(heavy, 1), _entry(eph, 2)]))
    assert go("--reps-override", str(tmp / "reps.json")) == ["reps", "prop", "prop", "reps", "eph"]


def test_without_an_override_no_reps_line_is_sent(leg):
    go, *_ = leg
    assert go() == ["prop", "prop", "eph"]


def test_an_override_for_a_row_not_in_the_input_is_refused_by_name(leg):
    go, tmp, heavy, *_ = leg
    (tmp / "reps.json").write_text(json.dumps([_entry(dict(heavy, dt_days=-1.0), 1)]))
    with pytest.raises(SystemExit, match="not in the input"):
        go("--reps-override", str(tmp / "reps.json"))


def test_zero_repetitions_and_a_row_listed_twice_are_refused(tmp_path):
    base = _row("propagation", 1.0)
    f = tmp_path / "r.json"
    f.write_text(json.dumps([_entry(base, 0)]))
    with pytest.raises(SystemExit, match="0 repetitions"):
        drive._load_reps_override(f)
    f.write_text(json.dumps([_entry(base, 1), _entry(base, 2)]))
    with pytest.raises(SystemExit, match="listed twice"):
        drive._load_reps_override(f)
    assert drive._load_reps_override(None) == {}


# ── the real daemon applies the count and echoes it (test mode) ──────────────

_DATA_DIR = Path.home() / ".empyrean" / "data"


def _binary() -> Path | None:
    p = Path(drive.__file__).resolve().parent / "runner"
    return p if p.exists() else None


def _daemon_lines(binary: Path, lines: list[str]) -> tuple[list[str], list[str]]:
    """Run the daemon, send `lines`, return (stdout lines, stderr `reps applied` lines)."""
    cmd = [str(binary), str(_DATA_DIR)]
    proc = subprocess.Popen(
        cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        env={**os.environ, "EMPYREAN_RUNNER_ECHO_REPS": "1"},
    )
    out_lines = []
    try:
        while True:
            e = proc.stderr.readline()
            assert e, "daemon exited without ready"
            if e.strip() == "ready":
                break
        for ln in lines:
            proc.stdin.write(ln + "\n")
            proc.stdin.flush()
            if not ln.startswith("reps"):
                out_lines.append(proc.stdout.readline().strip())
    finally:
        proc.stdin.close()
        err = proc.stderr.read()
        proc.wait(timeout=300)
    return out_lines, [x for x in err.splitlines() if x.startswith("reps applied")]


@pytest.mark.skipif(_binary() is None or not _DATA_DIR.exists(), reason="daemon binary or data dir unavailable")
def test_the_daemon_applies_the_reps_line_to_the_next_row_only_and_echoes_it() -> None:
    row = _row("propagation", 30.0, propagation_uncertainty="first_order_detection_on", ic_pos_au=[1.0, 0.1, 0.05], ic_vel_au_d=[-0.002, 0.017, 0.001])
    line = drive._prop_daemon_line(row)
    out, echoed = _daemon_lines(_binary(), ["reps 1", line, line])
    assert all(o.startswith("ok") for o in out), out
    assert echoed == ["reps applied: prop 1", "reps applied: prop 3"]


@pytest.mark.skipif(_binary() is None or not _DATA_DIR.exists(), reason="daemon binary or data dir unavailable")
def test_a_refused_line_does_not_leave_its_reps_count_for_the_next_row() -> None:
    """`reps 1`, then a prop line the daemon cannot parse, then a good one: the
    good row runs the default best-of-three, not the count meant for the refused one."""
    row = _row("propagation", 30.0, propagation_uncertainty="first_order_detection_on", ic_pos_au=[1.0, 0.1, 0.05], ic_vel_au_d=[-0.002, 0.017, 0.001])
    good = drive._prop_daemon_line(row)
    out, echoed = _daemon_lines(_binary(), ["reps 1", "prop 1 2 3", good])
    assert out[0].startswith("fail"), out
    assert out[1].startswith("ok"), out
    assert echoed[-1] == "reps applied: prop 3", echoed
