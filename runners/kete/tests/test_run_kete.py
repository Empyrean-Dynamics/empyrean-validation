"""run_kete: every row states kete's version and configuration."""

import importlib.util
import json
import pathlib
import sys

import pytest

HERE = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))


@pytest.fixture(scope="module")
def runner():
    spec = importlib.util.spec_from_file_location("run_kete", HERE / "run_kete.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def test_every_emitted_row_carries_version_and_settings(runner, tmp_path, monkeypatch):
    # A row kind kete does not replay: emitted, untouched, and still stamped.
    plan = tmp_path / "plan.json"
    plan.write_text(json.dumps([{"object": "Eros", "test_type": "orbit_determination_radar",
                                 "propagation_uncertainty": None}]))
    out = tmp_path / "o.json"
    monkeypatch.setattr(sys, "argv", ["run_kete.py", "--input", str(plan), "--output", str(out),
                                      "--data-dir", str(tmp_path)])
    assert runner.main() == 0
    rows = json.loads(out.read_text())
    assert len(rows) == 1
    assert rows[0]["kete_version"] == rows[0]["source_version"]
    assert rows[0]["kete_settings"] == runner.KETE_SETTINGS
    for key in ("force_model", "self_perturbers", "non_grav", "ephemeris", "od"):
        assert runner.KETE_SETTINGS[key]
