"""run_oorb: every row states OpenOrb's version and configuration."""

import importlib.util
import json
import pathlib
import sys

import pytest

HERE = pathlib.Path(__file__).resolve().parents[1]
RUNNER = HERE / "run_oorb.py"


@pytest.fixture(scope="module")
def runner():
    spec = importlib.util.spec_from_file_location("run_oorb", RUNNER)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def test_settings_name_the_model(runner):
    for key in ("dynamical_model", "perturbers", "relativity", "frame"):
        assert runner.OORB_SETTINGS[key]


@pytest.mark.skipif(not (HERE / "install" / "bin" / "oorb").exists(), reason="build oorb (make setup-oorb)")
def test_every_emitted_row_carries_version_and_settings(runner, tmp_path, monkeypatch):
    plan = tmp_path / "plan.json"
    plan.write_text(json.dumps([{"object": "Eros", "test_type": "orbit_determination",
                                 "propagation_uncertainty": None}]))
    out = tmp_path / "o.json"
    monkeypatch.setattr(sys, "argv", ["run_oorb.py", "--input", str(plan), "--output", str(out)])
    runner.main()  # an OD-only plan computes nothing (exit 1), but its rows are written
    rows = json.loads(out.read_text())
    assert rows and all(r["oorb_version"] == r["source_version"] for r in rows)
    assert all(r["oorb_settings"] == runner.OORB_SETTINGS for r in rows)
