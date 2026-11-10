"""run_orbfit: the seed is heliocentric from the registered DE440, the image is pinned,
and the container has no network.

    DE440_BSP=<third-party>/orbfit/de440.bsp <python with pytest> -m pytest runners/orbfit/tests -q
"""

import importlib.util
import os
import pathlib
import sys

import pytest

RUNNER = pathlib.Path(__file__).resolve().parents[1] / "run_orbfit.py"
DE440 = os.environ.get("DE440_BSP")


@pytest.fixture(scope="module")
def runner():
    spec = importlib.util.spec_from_file_location("run_orbfit_seed", RUNNER)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def test_the_image_is_pinned_by_digest(runner):
    assert runner.DEFAULT_IMAGE.startswith("minorplanetcenter/orbfit@sha256:")
    assert len(runner.DEFAULT_IMAGE.split("@sha256:")[1]) == 64


def _od_case(runner, tmp_path, monkeypatch):
    """Run process_od_row with the container replaced by a spy."""
    psv = tmp_path / "psv"
    psv.mkdir()
    (psv / "Eros.psv").write_text("# version=2017\npermID|stn|obsTime|ra|dec\n433|500|2020-01-01T00:00:00Z|1|1\n")
    seen = {}

    def spy(desig, obs_psv, seed_eq0, is_ngr, image, platform, timeout_s, leave_out=None):
        seen["seed"] = seed_eq0
        return None, "spy: not run"

    monkeypatch.setattr(runner, "run_neofit_container", spy)
    ic_row = {"object": "Eros", "test_type": "propagation", "epoch_mjd_tdb": 60000.0,
              "ic_pos_au": [1.0, 0.5, 0.1], "ic_vel_au_d": [-0.005, 0.015, 0.001]}
    od = {"object": "Eros", "test_type": "orbit_determination"}
    return seen, ic_row, od, psv


@pytest.mark.skipif(not DE440, reason="set DE440_BSP to a de440.bsp")
def test_the_seed_subtracts_the_suns_de440_state(runner, tmp_path, monkeypatch):
    seen, ic_row, od, psv = _od_case(runner, tmp_path, monkeypatch)
    sun = runner.SunEphemeris(DE440)
    out = runner.process_od_row(od, {"Eros": [ic_row]}, psv, image="img", platform="p",
                                timeout_s=1.0, source_version="v", sun=sun, massive=[])
    assert out["orbfit_seed_origin"].startswith("heliocentric")
    sp, sv = sun.state(60000.0 + 2_400_000.5)
    helio = [p - s for p, s in zip(ic_row["ic_pos_au"], sp)]
    desig = runner._orbfit_designation("Eros")
    want = runner.write_eq0_seed(desig, helio, [v - s for v, s in zip(ic_row["ic_vel_au_d"], sv)], 60000.0)
    assert seen["seed"] == want  # exactly the heliocentric seed, rotated by write_eq0_seed
    raw = runner.write_eq0_seed(desig, ic_row["ic_pos_au"], ic_row["ic_vel_au_d"], 60000.0)
    assert seen["seed"] != raw  # never the raw barycentric state


def test_the_runner_refuses_to_start_without_de440(runner, tmp_path, monkeypatch, capsys):
    plan = tmp_path / "plan.json"
    plan.write_text("[]")
    monkeypatch.setattr(sys, "argv", ["run_orbfit.py", "--plan", str(plan), "--output",
                                      str(tmp_path / "o.json"), "--psv-dir", str(tmp_path),
                                      "--de440", str(tmp_path / "de440.bsp")])
    assert runner.main() == 2
    assert "no such file" in capsys.readouterr().err


def test_the_container_runs_without_a_network(runner, tmp_path, monkeypatch):
    seen = {}

    class Done(Exception):
        pass

    def fake_run(cmd, **kw):
        seen["cmd"] = cmd
        raise Done

    monkeypatch.setattr(runner.subprocess, "run", fake_run)
    with pytest.raises(Done):
        runner.run_neofit_container("433", "", "", False, "img", "linux/amd64", 1.0)
    cmd = seen["cmd"]
    assert cmd[cmd.index("--network") + 1] == "none"
