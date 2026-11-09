"""The non-grav second pass runs exactly for the legacy fits the plan holds a
``non_grav_recovery`` row for, read off the FULL plan (``--context``).

The plan carries the rust runner's gate (SBDB A2 != 0). Re-deriving it from the
propagation rows' A1/A2/A3 ran the pass for 2020 CD3 (A1 only), which has no
plan row: a surplus row that failed the cli channel's account on hosted run
42940569667. Reading the full plan rather than the leg keeps the account
strict: a leg holding an object's legacy fit without its plan non-grav row (a
split scheduling unit) writes a surplus row instead of quietly losing one.

The fit is mocked: the lookup and the row plumbing are what is under test.
"""

from __future__ import annotations

import json
import sys

import pytest
import run

OBJ = "NGOBJ"


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
    # The plan's NG row, written by the legacy fit's second pass, carries the
    # SBDB reference the rust runner compared against.
    ng = _row("non_grav_recovery", ic_a1=1e-13, ic_a2=2e-14, ic_a3=0.0, ic_pos_au=None, ic_vel_au_d=None)
    (tmp_path / "leg.json").write_text(json.dumps([od, ng]))
    (tmp_path / "plan.json").write_text(json.dumps([prop, od, ng]))
    fixtures = tmp_path / "psv"
    fixtures.mkdir()
    (fixtures / f"{OBJ}.psv").write_text("# fixture\n")

    calls = {"ng": 0}
    raw = {
        "converged": True,
        "iterations": 3,
        "summary_num_selected": 10,
        "summary_rms_ra": 0.1,
        "summary_rms_dec": 0.1,
        "summary_rms_combined": 0.1,
        "summary_chi2": 1.0,
        "summary_reduced_chi2": 1.0,
    }
    fit = ([1.0, 0.0, 0.0], raw, 12.5)

    def ng_fit(**kwargs):
        calls["ng"] += 1
        return ([1.0, 0.0, 0.0], raw, 99.0)

    monkeypatch.setattr(run, "_ensure_initialized", lambda data_dir: None)
    monkeypatch.setattr(run, "_wheel_source_version", lambda: "test")
    monkeypatch.setattr(run, "_determine_one", lambda **kwargs: fit)
    monkeypatch.setattr(run, "_determine_nongrav_one", ng_fit)

    def go(*extra, rc=0):
        out = tmp_path / "out.json"
        argv = ["run.py", "--input", str(tmp_path / "leg.json"), "--output", str(out), "--fixtures-dir", str(fixtures), *extra]
        monkeypatch.setattr(sys, "argv", argv)
        assert run.main() == rc
        return json.loads(out.read_text())

    return go, calls, tmp_path


def test_a_leg_with_only_the_legacy_od_row_asks_for_the_ng_second_pass_given_context(leg):
    go, calls, tmp = leg
    rows = go("--context", str(tmp / "plan.json"))
    assert calls["ng"] == 1
    assert sorted(r["test_type"] for r in rows) == ["non_grav_recovery", "orbit_determination"]


def test_without_context_the_legs_own_plan_ng_row_asks_for_the_pass(leg):
    go, calls, _ = leg
    rows = go()
    assert calls["ng"] == 1
    assert sorted(r["test_type"] for r in rows) == ["non_grav_recovery", "orbit_determination"]


def test_a_reference_without_a_plan_ng_row_gets_no_second_pass(leg):
    """2020 CD3: SBDB A1 on its propagation rows, A2 = 0, so the plan holds no
    non_grav_recovery row: the fit is not run and no row is written."""
    go, calls, tmp = leg
    od = _row("orbit_determination")
    prop = _row("propagation", ic_a1=1.36e-10, ic_a2=0.0, ic_a3=0.0, dt_days=10.0, propagation_uncertainty="first_order_detection_on")
    (tmp / "leg.json").write_text(json.dumps([od]))
    (tmp / "plan.json").write_text(json.dumps([prop, od]))
    rows = go("--context", str(tmp / "plan.json"))
    assert calls["ng"] == 0
    assert [r["test_type"] for r in rows] == ["orbit_determination"]


def test_a_split_unit_writes_a_surplus_row_and_fails_the_account(leg):
    """The leg holds the legacy fit but its plan NG row went to another leg: the
    full plan still asks for the pass, so the row is a surplus the account
    refuses, never a row lost without a word."""
    go, calls, tmp = leg
    (tmp / "leg.json").write_text(json.dumps([_row("orbit_determination")]))
    rows = go("--context", str(tmp / "plan.json"), rc=1)
    assert calls["ng"] == 1
    assert sorted(r["test_type"] for r in rows) == ["non_grav_recovery", "orbit_determination"]


def test_a_context_that_lacks_an_input_object_is_refused_by_name(leg):
    go, _calls, tmp = leg
    (tmp / "other.json").write_text(json.dumps([dict(_row("propagation"), object="Elsewhere")]))
    with pytest.raises(SystemExit) as e:
        go("--context", str(tmp / "other.json"))
    assert OBJ in str(e.value)


def test_the_gate_is_the_plans_non_grav_rows():
    ng = _row("non_grav_recovery", object="B", ic_a1=0.0, ic_a2=2e-14, ic_a3=0.0)
    rows = [_row("propagation", ic_a1=1.36e-10, ic_a2=0.0, ic_a3=0.0), ng, _row("orbit_determination", object="C")]
    assert run._plan_non_grav_refs(rows) == {("B", "standard"): (0.0, 2e-14, 0.0)}
    with pytest.raises(SystemExit, match="carries no SBDB reference"):
        run._plan_non_grav_refs([dict(ng, ic_a3=None)])
    with pytest.raises(SystemExit, match="two non_grav_recovery rows for B"):
        run._plan_non_grav_refs([ng, dict(ng)])
