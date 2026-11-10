"""run_oorb: data from the oorb folder, BC430 perturbers on, the Sun from DE440,
and a BC430 body a named unsupported row.

    OORB_DATA_DIR=<third-party>/oorb <python with pytest> -m pytest runners/oorb/tests -q
"""

import importlib.util
import os
import pathlib
import sys

import pytest

HERE = pathlib.Path(__file__).resolve().parents[1]
RUNNER = HERE / "run_oorb.py"
DATA = os.environ.get("OORB_DATA_DIR")
FIXTURES = os.environ.get("FIXTURES_PSV")
BIN = HERE / "install" / "bin" / "oorb"


@pytest.fixture(scope="module")
def runner():
    spec = importlib.util.spec_from_file_location("run_oorb", RUNNER)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


CONF = """dynamical_model:      2-body
perturber.Moon:    T
perturber.asteroids: F
relativity: T
"""


def test_the_one_conf_sets_n_body_with_the_asteroids_on(runner, tmp_path):
    shipped = tmp_path / "oorb.conf"
    shipped.write_text(CONF)
    conf = runner.write_conf(shipped, tmp_path / "c")
    f = conf.read_text()
    assert "dynamical_model:      n-body" in f and "2-body" not in f
    assert "perturber.asteroids: T" in f and "perturber.asteroids: F" not in f
    assert "relativity: T" in f and "perturber.Moon:    T" in f  # the rest as shipped
    assert [c.name for c in (tmp_path / "c").iterdir()] == ["oorb.conf"]  # no second model


def test_a_conf_without_the_lines_is_refused(runner, tmp_path):
    shipped = tmp_path / "oorb.conf"
    shipped.write_text("relativity: T\n")
    with pytest.raises(ValueError, match="dynamical_model"):
        runner.write_conf(shipped, tmp_path / "c")


def test_staging_links_every_file_and_names_what_is_missing(runner, tmp_path):
    src = tmp_path / "oorb"
    src.mkdir()
    for n in runner.OORB_DATA_FILES[:-1]:
        (src / n).write_text("x")
    assert runner.stage_data(src, tmp_path / "d") == ["asteroid_masses.txt"]
    (src / "asteroid_masses.txt").write_text("x")
    assert runner.stage_data(src, tmp_path / "d") == []
    for n in runner.OORB_DATA_FILES:
        link = tmp_path / "d" / n
        assert link.is_symlink() and link.resolve() == (src / n).resolve()


def test_settings_name_the_model(runner):
    for key in ("commit", "dynamical_model", "perturbers", "self_perturbers", "relativity",
                "planetary_ephemeris", "frame"):
        assert key in runner.OORB_SETTINGS
    assert "BC430" in runner.OORB_SETTINGS["perturbers"]


def test_self_perturbers_come_from_the_indices_file_and_the_fixtures(runner, tmp_path):
    (tmp_path / "asteroid_indices.txt").write_text("1\n433 \n#4\n7\n")
    assert runner.bc430_numbers(tmp_path) == {1, 433, 7}  # a commented line is inactive
    fx = tmp_path / "fx"
    fx.mkdir()
    (fx / "Eros.psv").write_text("# version=2017\npermID |provID|stn\n  433 |     |500\n")
    (fx / "C_2023 A3.psv").write_text("permID|provID|stn\n|C/2023 A3|500\n")
    assert runner.fixture_number(fx, "Eros") == 433
    assert runner.fixture_number(fx, "C/2023 A3") is None  # unnumbered: never a BC430 body
    assert runner.fixture_number(fx, "Nowhere") is None
    (fx / "Ceres.psv").write_text("permID|provID|stn\n4|     |500\n")  # 4 is commented out
    assert runner.self_perturbers({"Eros", "C/2023 A3", "Nowhere", "Ceres"}, fx, tmp_path) == {"Eros"}


def test_the_self_perturber_reason_is_the_shared_vocabulary(runner):
    reason = runner.SELF_PERTURBER_UNSUPPORTED
    assert reason.startswith("self-perturber: ")  # as adam-core and layup word it
    assert "BC430" in reason and "cannot leave a body out" in reason


def test_a_self_perturber_is_a_named_unsupported_row_never_run(runner, tmp_path, monkeypatch):
    """Eros (BC430 number 433) gets one unsupported row per plan row with the
    reason and no position, and oorb is never called for it; Apophis runs."""
    import json

    prefix = tmp_path / "install"
    (prefix / "bin").mkdir(parents=True)
    (prefix / "bin" / "oorb").write_text("")
    (prefix / "oorb.conf").write_text(CONF)
    data = tmp_path / "oorb"
    data.mkdir()
    for n in runner.OORB_DATA_FILES:
        (data / n).write_text("433\n" if n == "asteroid_indices.txt" else "x")
    fx = tmp_path / "fx"
    fx.mkdir()
    (fx / "Eros.psv").write_text("permID|provID|stn\n433|     |500\n")
    (fx / "Apophis.psv").write_text("permID|provID|stn\n99942|     |500\n")
    monkeypatch.setattr(runner, "SunEphemeris", lambda path: object())
    ran = []

    def fake(kind):
        def run(row, oorb_bin, env, sun):
            ran.append((row["object"], kind))
            assert "perturber.asteroids: T" in pathlib.Path(env["OORB_CONF"]).read_text()
            return {"oorb_pos_au": [1.0, 2.0, 3.0]} if kind == "p" else {"oorb_separation_arcsec": 0.1}
        return run

    monkeypatch.setattr(runner, "_propagate", fake("p"))
    monkeypatch.setattr(runner, "_ephemeris", fake("e"))
    plan = [{"object": o, "test_type": t, "dt_days": 30.0, "observer": "500",
             "propagation_uncertainty": "none_detection_on"}
            for o in ("Apophis", "Eros") for t in ("propagation", "ephemeris")]
    (tmp_path / "plan.json").write_text(json.dumps(plan))
    out = tmp_path / "o.json"
    monkeypatch.setattr(sys, "argv", ["run_oorb.py", "--input", str(tmp_path / "plan.json"),
                                      "--output", str(out), "--prefix", str(prefix), "--data-dir",
                                      str(data), "--fixtures-dir", str(fx), "--jobs", "1"])
    assert runner.main() == 0
    rows = json.loads(out.read_text())
    assert [(r["object"], r["test_type"]) for r in rows] == [(p["object"], p["test_type"]) for p in plan]
    assert sorted(ran) == [("Apophis", "e"), ("Apophis", "p")]  # Eros never run
    for r in rows:
        assert r["oorb_settings"]["self_perturbers"].startswith("Eros: named unsupported")
        if r["object"] == "Eros":
            assert r["oorb_unsupported"] == runner.SELF_PERTURBER_UNSUPPORTED
            assert "oorb_pos_au" not in r and "oorb_separation_arcsec" not in r
        else:
            assert "oorb_unsupported" not in r


def test_the_heliocentric_ic_subtracts_the_suns_state(runner):
    class Sun:
        def state(self, jd):
            assert jd == 60000.0 + 2_400_000.5
            return [0.001, 0.002, 0.003], [1e-6, 2e-6, 3e-6]

    row = {"epoch_mjd_tdb": 60000.0, "ic_pos_au": [1.0, 0.0, 0.0], "ic_vel_au_d": [0.0, 0.017, 0.0]}
    pos, vel = runner._heliocentric_ic(row, Sun())
    assert pos == [1.0 - 0.001, -0.002, -0.003]
    assert vel == [-1e-6, 0.017 - 2e-6, -3e-6]


@pytest.mark.skipif(not (DATA and FIXTURES and BIN.exists()),
                    reason="set OORB_DATA_DIR and FIXTURES_PSV; build oorb (make setup-oorb)")
def test_main_propagates_with_the_registered_data(runner, tmp_path, monkeypatch):
    """A real oorb run: Apophis (not a BC430 body) runs with the asteroids, Eros
    (BC430 number 433) is a named unsupported row; every row is stamped."""
    import json

    def prop(obj):
        return {"object": obj, "test_type": "propagation", "epoch_mjd_tdb": 60000.0,
                "t_mjd_tdb": 60030.0, "dt_days": 30.0, "propagation_uncertainty": "none_detection_on",
                "ic_pos_au": [2.5, 0.3, 0.1], "ic_vel_au_d": [-0.002, 0.011, 0.001]}

    plan = tmp_path / "plan.json"
    plan.write_text(json.dumps([prop("Apophis"), prop("Eros")]))
    out = tmp_path / "o.json"
    monkeypatch.setattr(sys, "argv", ["run_oorb.py", "--input", str(plan), "--output", str(out),
                                      "--data-dir", DATA, "--fixtures-dir", FIXTURES])
    assert runner.main() == 0
    rows = json.loads(out.read_text())
    assert len(rows[0]["oorb_pos_au"]) == 3 and "oorb_unsupported" not in rows[0]
    assert rows[1]["oorb_unsupported"] == runner.SELF_PERTURBER_UNSUPPORTED
    assert "oorb_pos_au" not in rows[1]
    assert "Eros" in rows[0]["oorb_settings"]["self_perturbers"]
    assert all(r["oorb_version"] == r["source_version"] for r in rows)
    assert rows[0]["oorb_settings"]["commit"] == "02278424f6c13a5c653a5e3ab021aa4ccc537966"
