"""The driver closes its runner after the last row by draining the runner's
pipes and waiting up to ``RUNNER_EXIT_TIMEOUT_S``, and fails by name past it.

A heavy C leg in the 2026-11-17 act rehearsal answered every row, then failed
because its runner took longer than the old five seconds to exit. The runner
here is a stand-in that answers the OD command with a canned line; what it does
after its input closes is what is under test.
"""

from __future__ import annotations

import json
import os
import stat
import sys

import drive
import pytest

OBJ = "EXITOBJ"

RUNNER = """#!/usr/bin/env python3
import os, sys, time
open(sys.argv[0] + ".pid", "w").write(str(os.getpid()))
print("ready", file=sys.stderr, flush=True)
for line in sys.stdin:
    print("ok 1.0 0.0 0.0 0.0 0.017 0.0 3 12.5 0.25", flush=True)
{on_exit}
"""


@pytest.fixture
def leg(tmp_path, monkeypatch):
    """One orbit-determination row, its fixture, and a runner whose behaviour
    after its input closes is the test's to choose."""
    row = {
        "object": OBJ,
        "test_type": "orbit_determination",
        "population": "NEO",
        "force_model": "standard",
        "propagation_uncertainty": None,
        "dt_days": 0.0,
        "observer": None,
        "ic_a1": None,
        "ic_a2": None,
        "ic_a3": None,
        "ic_pos_au": [1.0, 0.0, 0.0],
        "ic_vel_au_d": [0.0, 0.017, 0.0],
        "epoch_mjd_tdb": 60000.0,
        "t_mjd_tdb": 60000.0,
    }
    (tmp_path / "leg.json").write_text(json.dumps([row]))
    fixtures = tmp_path / "psv"
    fixtures.mkdir()
    (fixtures / f"{OBJ}.psv").write_text("# fixture\n")
    runner = tmp_path / "fake-runner"

    def go(on_exit: str):
        runner.write_text(RUNNER.replace("{on_exit}", on_exit))
        runner.chmod(runner.stat().st_mode | stat.S_IXUSR)
        out = tmp_path / "out.json"
        argv = ["drive.py", "--input", str(tmp_path / "leg.json"), "--output", str(out), "--runner", str(runner), "--fixtures-dir", str(fixtures)]
        monkeypatch.setattr(sys, "argv", argv)
        rc = drive.main()
        return rc, json.loads(out.read_text())

    return go, runner


def test_a_runner_slower_to_exit_than_five_seconds_still_finishes_the_leg(leg):
    go, _ = leg
    rc, rows = go("time.sleep(6)")
    assert rc == 0
    assert [r["test_type"] for r in rows] == ["orbit_determination"]


def test_a_runner_writing_past_the_pipe_buffer_at_exit_still_finishes_the_leg(leg):
    """A plain wait would leave the runner blocked on a full stderr pipe."""
    go, _ = leg
    rc, rows = go("sys.stderr.write('x' * 262144); sys.stderr.flush()")
    assert rc == 0
    assert [r["test_type"] for r in rows] == ["orbit_determination"]


def test_a_runner_that_never_exits_is_killed_and_fails_the_leg_by_name(leg, monkeypatch):
    go, runner = leg
    monkeypatch.setattr(drive, "RUNNER_EXIT_TIMEOUT_S", 1)
    with pytest.raises(SystemExit) as e:
        go("print('stuck freeing', file=sys.stderr, flush=True); time.sleep(600)")
    msg = str(e.value)
    assert "the C runner answered every row but had not exited 1 s" in msg
    assert "stuck freeing" in msg
    pid = int((runner.parent / (runner.name + ".pid")).read_text())
    with pytest.raises(ProcessLookupError):
        os.kill(pid, 0)
