"""``--context``: a row-level leg replays a subset of the plan, but the lookup
that decides whether an object gets the non-grav second pass reads the FULL plan.

A channel leg can hold an object's legacy OD row without any of its propagation
rows (the only rows that carry the SBDB A1/A2/A3 reference). Harvesting the
reference from the leg's own input would then skip the second pass, and the
object's ``non_grav_recovery`` row would silently vanish from the channel.

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
    ng = _row("non_grav_recovery", ic_pos_au=None, ic_vel_au_d=None)  # the plan's NG row is written by the legacy fit
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

    def go(*extra):
        out = tmp_path / "out.json"
        argv = ["run.py", "--input", str(tmp_path / "leg.json"), "--output", str(out), "--fixtures-dir", str(fixtures), *extra]
        monkeypatch.setattr(sys, "argv", argv)
        assert run.main() == 0
        return json.loads(out.read_text())

    return go, calls, tmp_path


def test_a_leg_with_only_the_legacy_od_row_asks_for_the_ng_second_pass_given_context(leg):
    go, calls, tmp = leg
    rows = go("--context", str(tmp / "plan.json"))
    assert calls["ng"] == 1
    assert sorted(r["test_type"] for r in rows) == ["non_grav_recovery", "orbit_determination"]


def test_without_context_the_leg_input_is_the_context(leg):
    go, calls, _ = leg
    rows = go()
    assert calls["ng"] == 0  # the input has no propagation row to carry A1
    assert [r["test_type"] for r in rows] == ["orbit_determination"]


def test_a_context_that_lacks_an_input_object_is_refused_by_name(leg):
    go, _calls, tmp = leg
    (tmp / "other.json").write_text(json.dumps([dict(_row("propagation"), object="Elsewhere")]))
    with pytest.raises(SystemExit) as e:
        go("--context", str(tmp / "other.json"))
    assert OBJ in str(e.value)


def test_harvest_reads_every_nonzero_reference():
    rows = [_row("propagation", ic_a1=0.0, ic_a2=0.0), _row("propagation", ic_a2=2e-14, object="B"), _row("orbit_determination", object="C")]
    assert run._harvest_ref_non_grav(rows) == {"B": (0.0, 2e-14, 0.0)}
