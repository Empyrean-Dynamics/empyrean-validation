"""``--context``: a row-level leg replays a subset of the plan, but the lookup
that decides whether an object gets the non-grav second pass reads the FULL plan.

A channel leg can hold an object's legacy OD row without any of its propagation
rows (the only rows that carry the SBDB A1/A2/A3 reference). Harvesting the
reference from the leg's own input would then skip the second pass, and the
object's ``non_grav_recovery`` row would silently vanish from the channel.

The runner is a stand-in daemon that answers the OD commands with canned
lines: the lookup and the row plumbing are what is under test, not the fit.
"""

from __future__ import annotations

import json
import stat
import sys

import drive
import pytest

OBJ = "NGOBJ"

FAKE_RUNNER = """#!/usr/bin/env python3
import sys
print("ready", file=sys.stderr, flush=True)
for line in sys.stdin:
    cmd = line.split()[0]
    if cmd == "od":
        print("ok 1.0 0.0 0.0 0.0 0.017 0.0 3 12.5 0.25", flush=True)
    elif cmd == "odng":
        print("ok 1e-13 0 0 1e-14 1e-14 1e-14 3 99.0 0.2 1.0 0.0 0.0", flush=True)
    else:
        print("fail unexpected " + cmd, flush=True)
"""


def _row(test_type: str, **kw) -> dict:
    r = {
        "object": OBJ,
        "test_type": test_type,
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
    r.update(kw)
    return r


@pytest.fixture
def leg(tmp_path, monkeypatch):
    """A leg holding only the object's legacy OD row and its NG row (one
    scheduling unit), the full plan beside it."""
    od = _row("orbit_determination")
    prop = _row("propagation", ic_a1=1e-13, dt_days=10.0, propagation_uncertainty="first_order_detection_on")
    ng = _row("non_grav_recovery", ic_pos_au=None, ic_vel_au_d=None)  # the plan's NG row is written by the legacy fit
    (tmp_path / "leg.json").write_text(json.dumps([od, ng]))
    (tmp_path / "plan.json").write_text(json.dumps([prop, od, ng]))
    fixtures = tmp_path / "psv"
    fixtures.mkdir()
    (fixtures / f"{OBJ}.psv").write_text("# fixture\n")
    runner = tmp_path / "fake-runner"
    runner.write_text(FAKE_RUNNER)
    runner.chmod(runner.stat().st_mode | stat.S_IXUSR)

    def go(*extra):
        out = tmp_path / "out.json"
        argv = ["drive.py", "--input", str(tmp_path / "leg.json"), "--output", str(out), "--runner", str(runner), "--fixtures-dir", str(fixtures), *extra]
        monkeypatch.setattr(sys, "argv", argv)
        assert drive.main() == 0
        return json.loads(out.read_text())

    return go, tmp_path


def test_a_leg_with_only_the_legacy_od_row_asks_for_the_ng_second_pass_given_context(leg):
    go, tmp = leg
    rows = go("--context", str(tmp / "plan.json"))
    assert sorted(r["test_type"] for r in rows) == ["non_grav_recovery", "orbit_determination"]


def test_without_context_the_leg_input_is_the_context(leg):
    go, _ = leg
    rows = go()
    assert [r["test_type"] for r in rows] == ["orbit_determination"]  # the input carries no A1 reference


def test_a_context_that_lacks_an_input_object_is_refused_by_name(leg):
    go, tmp = leg
    (tmp / "other.json").write_text(json.dumps([dict(_row("propagation"), object="Elsewhere")]))
    with pytest.raises(SystemExit) as e:
        go("--context", str(tmp / "other.json"))
    assert OBJ in str(e.value)


def test_harvest_reads_every_nonzero_reference():
    rows = [_row("propagation", ic_a1=0.0, ic_a2=0.0), _row("propagation", ic_a2=2e-14, object="B"), _row("orbit_determination", object="C")]
    assert drive._harvest_ref_non_grav(rows) == {"B": (0.0, 2e-14, 0.0)}
