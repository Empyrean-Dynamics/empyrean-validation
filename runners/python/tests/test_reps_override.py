"""``--reps-override``: a replica leg of the heavy row runs it once.

The heavy row's three timing repetitions run on three legs, one each. The
override is a side file keyed by the row's identity, never a plan key (a plan
key would flow into the output through the row copy). The engine is stubbed:
what is under test is which timing count each row is handed.
"""

from __future__ import annotations

import contextlib
import json
import sys

import pytest
import run


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
    seen = {"prop": {}, "eph": {}}

    def prop(**kw):
        seen["prop"][kw["target_t_mjd_tdb"]] = kw["n_timing_runs"]

    def ephem(**kw):
        seen["eph"][kw["target_t_mjd_tdb"]] = kw["n_timing_runs"]

    monkeypatch.setattr(run, "_ensure_initialized", lambda data_dir: None)
    monkeypatch.setattr(run, "_wheel_source_version", lambda: "test")
    monkeypatch.setattr(run, "_propagate_one", prop)
    monkeypatch.setattr(run, "_ephemeris_one", ephem)

    def go(*extra):
        argv = ["run.py", "--input", str(tmp_path / "leg.json"), "--output", str(tmp_path / "out.json"), "--fixtures-dir", str(tmp_path), *extra]
        monkeypatch.setattr(sys, "argv", argv)
        run.main()
        return seen

    return go, tmp_path, heavy, light, eph


def test_the_overridden_row_runs_once_and_every_other_row_keeps_the_default(leg):
    go, tmp, heavy, _light, eph = leg
    (tmp / "reps.json").write_text(json.dumps([_entry(heavy, 1), _entry(eph, 2)]))
    seen = go("--reps-override", str(tmp / "reps.json"))
    assert seen["prop"] == {60000.0 - 5475.0: 1, 60000.0 - 3650.0: 3}
    assert seen["eph"] == {60000.0 - 5475.0: 2}


def test_without_an_override_every_row_runs_the_default_best_of_three(leg):
    go, *_ = leg
    seen = go()
    assert set(seen["prop"].values()) == {3} and set(seen["eph"].values()) == {3}


def test_an_override_for_a_row_not_in_the_input_is_refused_by_name(leg):
    go, tmp, heavy, *_ = leg
    ghost = dict(heavy, dt_days=-1.0)
    (tmp / "reps.json").write_text(json.dumps([_entry(ghost, 1)]))
    with pytest.raises(SystemExit, match="not in the input"):
        go("--reps-override", str(tmp / "reps.json"))


def test_zero_repetitions_and_a_row_listed_twice_are_refused(tmp_path):
    base = _row("propagation", 1.0)
    f = tmp_path / "r.json"
    f.write_text(json.dumps([_entry(base, 0)]))
    with pytest.raises(SystemExit, match="0 repetitions"):
        run._load_reps_override(f)
    f.write_text(json.dumps([_entry(base, 1), _entry(base, 2)]))
    with pytest.raises(SystemExit, match="listed twice"):
        run._load_reps_override(f)
    assert run._load_reps_override(None) == {}


def test_a_python_row_with_override_one_calls_the_engine_once(monkeypatch):
    """The plumbing end to end into the engine call: with the stubbed engine
    call counted, `n_timing_runs=1` is one call and the default is three."""
    calls = []

    def fake_propagate(**kw):
        calls.append(1)
        return {"x": [1.0, 2.0, 3.0, 0.0, 0.0, 0.0]}

    monkeypatch.setattr(run, "_propagate", fake_propagate)
    monkeypatch.setattr(run, "_declined", lambda result: None)
    for n in (1, 3):
        calls.clear()
        with contextlib.suppress(Exception):  # the stub result is thinner than a real one; only the call count matters
            run._propagate_one(
                object_id="X", population="NEO", epoch_mjd_tdb=60000.0, target_t_mjd_tdb=60010.0,
                ic_pos_au=[1.0, 0.0, 0.0], ic_vel_au_d=[0.0, 0.017, 0.0], ic_a1=0.0, ic_a2=0.0, ic_a3=0.0,
                ic_g_alpha=0.0, ic_g_r0=0.0, ic_g_m=0.0, ic_g_n=0.0, ic_g_k=0.0, ic_non_grav_dt=None,
                force_model="standard", method_tag="first_order_detection_on", n_timing_runs=n,
                excluded_perturbers_naif=None,
            )
        assert len(calls) == n
