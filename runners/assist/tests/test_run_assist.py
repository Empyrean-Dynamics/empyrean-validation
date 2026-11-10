"""run_assist: one force model. A self-perturber is a named unsupported row; every
other object propagates with the planets and the sixteen SB441-N16 masses.

The runner imports assist and rebound at module level. These tests stand both in
with stubs (the light gate venv carries neither), so what they prove is the
runner's own routing: which ephemeris each propagation is handed, and which rows
are never run, looked up or timed.
"""

import importlib.util
import json
import pathlib
import sys
import types
from typing import ClassVar

import numpy as np
import pytest

HERE = pathlib.Path(__file__).resolve().parents[1]
RUNNER = HERE / "run_assist.py"

ARMS = {
    "assist_default_variational_off", "assist_default_variational_on",
    "assist_layup_variational_off", "assist_layup_variational_on",
    "assist_asteroid_institute_variational_off", "assist_asteroid_institute_variational_on",
}


class FakeEphem:
    """Stands in for assist.Ephem and records the files it was built from."""

    built: ClassVar[list] = []

    def __init__(self, planets_path, asteroids_path=None):
        self.planets_path = planets_path
        self.asteroids_path = asteroids_path
        FakeEphem.built.append(self)


@pytest.fixture()
def runner(monkeypatch):
    FakeEphem.built = []
    fake_assist = types.ModuleType("assist")
    fake_assist.__version__ = "1.2.3"
    fake_assist.Ephem = FakeEphem
    fake_rebound = types.ModuleType("rebound")
    fake_rebound.__version__ = "4.6.0"
    monkeypatch.setitem(sys.modules, "assist", fake_assist)
    monkeypatch.setitem(sys.modules, "rebound", fake_rebound)
    monkeypatch.syspath_prepend(str(HERE))  # snapshot_inputs
    spec = importlib.util.spec_from_file_location("run_assist", RUNNER)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def plan_row(obj, population, dt, method, excluded=None, test_type="propagation"):
    r = {
        "object": obj, "population": population, "test_type": test_type,
        "epoch_mjd_tdb": 61200.0, "dt_days": dt, "propagation_uncertainty": method,
        "notes": "",
    }
    if excluded is not None:
        r["excluded_perturbers_naif"] = excluded
    return r


def run_main(runner, monkeypatch, tmp_path, rows):
    """Run main() over `rows` with the propagator and the Horizons lookup spied."""
    data = tmp_path / "assist"
    data.mkdir()
    (data / "linux_p1550p2650.440").write_bytes(b"planets")
    (data / "sb441-n16.bsp").write_bytes(b"DAF/SPK ")
    plan = tmp_path / "plan.json"
    plan.write_text(json.dumps(rows))
    out = tmp_path / "out.json"
    looked_up, propagated = [], []

    def lookup(cache, command, epoch_mjd, object_name):
        looked_up.append(object_name)
        return np.array([1.0, 0.0, 0.0]), np.array([0.0, 0.0172, 0.0])

    def propagate(pos, vel, epoch, target, ephem, **kw):
        propagated.append((epoch, target, ephem, kw["config"], kw["with_stm"]))
        stm = np.eye(6) if kw["with_stm"] else None
        return np.array([1.0, 0.0, 0.0]), np.array([0.0, 0.0172, 0.0]), 1.0, 2.0, stm

    monkeypatch.setattr(runner, "query_horizons", lookup)
    monkeypatch.setattr(runner, "propagate_assist", propagate)
    monkeypatch.setattr(runner, "resolved_config", lambda config, ephem: {"config": config})
    monkeypatch.setattr(sys, "argv", [
        "run_assist.py", str(plan), "--output", str(out), "--data-dir", str(data),
        "--horizons-cache", str(tmp_path / "horizons"), "--n-timing-runs", "1",
    ])
    runner.main()
    return json.loads(out.read_text()), looked_up, propagated, data


def test_the_reason_comes_from_excluded_perturbers_naif_not_the_name_or_population(runner):
    vesta = {"object": "Vesta", "population": "Self-Perturber", "excluded_perturbers_naif": [2000004]}
    assert runner.assist_unsupported_reason(vesta) == runner.SELF_PERTURBER_UNSUPPORTED
    # Every one of the sixteen, and only those, makes a row unsupported.
    for n in runner.SB441_N16:
        assert runner.assist_unsupported_reason({"excluded_perturbers_naif": [2_000_000 + n]})
    assert runner.assist_unsupported_reason({"excluded_perturbers_naif": [2000005]}) is None
    # Population and name alone decide nothing: the plan's exclusion does.
    assert runner.assist_unsupported_reason({"object": "Vesta", "population": "Self-Perturber"}) is None
    assert runner.assist_unsupported_reason({"object": "Eros", "excluded_perturbers_naif": []}) is None
    assert len(runner.SELF_PERTURBER_NAIF) == 16


def test_a_self_perturber_is_a_named_unsupported_row_never_run_looked_up_or_timed(runner, monkeypatch, tmp_path):
    """Vesta (the plan excludes NAIF 2000004 from its own forces) is one of the
    sixteen perturbers ASSIST integrates: every arm of every horizon carries the
    reason, and nothing about it is propagated, looked up or timed."""
    rows = []
    for dt in (-30.0, 30.0):
        for method in ("none_detection_on", "first_order_detection_on"):
            rows.append(plan_row("Vesta", "Self-Perturber", dt, method, excluded=[2000004]))
            rows.append(plan_row("Eros", "NEO", dt, method, excluded=[]))
    rows.append(plan_row("Vesta", "Self-Perturber", 30.0, "none_detection_on", [2000004], "ephemeris"))
    got, looked_up, propagated, _ = run_main(runner, monkeypatch, tmp_path, rows)

    vesta = [r for r in got if r["object"] == "Vesta"]
    assert len(vesta) == 2 * 6
    for dt in (-30.0, 30.0):
        assert {r["propagation_uncertainty"] for r in vesta if r["dt_days"] == dt} == ARMS
    for r in vesta:
        assert r["assist_unsupported"] == runner.SELF_PERTURBER_UNSUPPORTED
        assert r["test_type"] == "assist" and r["assist_version"] == "1.2.3"
        # Never a fabricated value: no position, comparison or timing field.
        for k in ("assist_time_ms", "assist_call_time_ms", "assist_vs_horizons_km",
                  "assist_pos_au", "horizons_pos_au", "assist_stm"):
            assert k not in r, k
    assert "Vesta" not in looked_up
    assert all(target - 61200.0 in (-30.0, 30.0) for _, target, *_ in propagated)
    assert len(propagated) == 2 * 6 * 2, "Eros only: two horizons, six arms, warm-up + one timed run"
    eros = [r for r in got if r["object"] == "Eros"]
    assert len(eros) == 12 and all("assist_unsupported" not in r for r in eros)


def test_a_non_member_propagates_with_the_sixteen_masses_and_no_other_model(runner, monkeypatch, tmp_path):
    """Eros is handed the one ephemeris built from the planets file AND
    sb441-n16.bsp on every arm; no planets-only ephemeris exists to switch to."""
    rows = [plan_row("Eros", "NEO", dt, "none_detection_on", excluded=[]) for dt in (-30.0, 30.0)]
    rows += [plan_row("Iris", "Self-Perturber", 30.0, "none_detection_on", excluded=[2000007])]
    got, looked_up, propagated, data = run_main(runner, monkeypatch, tmp_path, rows)

    assert len(FakeEphem.built) == 1, "one force model: exactly one ephemeris is built"
    (ephem,) = FakeEphem.built
    assert ephem.planets_path == str(data / "linux_p1550p2650.440")
    assert ephem.asteroids_path == str(data / "sb441-n16.bsp")
    assert len(propagated) == 2 * 6 * 2
    assert all(e is ephem for _, _, e, _, _ in propagated)
    eros = [r for r in got if r["object"] == "Eros"]
    assert len(eros) == 12
    assert all(r["assist_time_ms"] == 1.0 and r["assist_vs_horizons_km"] == 0.0 for r in eros)
    assert looked_up.count("Eros") == 1 + 2, "the epoch state once, then each target"
    assert [r["object"] for r in got if "assist_unsupported" in r] == ["Iris"] * 6
