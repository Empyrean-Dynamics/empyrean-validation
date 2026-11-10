"""run_grss: every row states GRSS's configuration for the run."""

import importlib.util
import pathlib

import pytest

RUNNER = pathlib.Path(__file__).resolve().parents[1] / "run_grss.py"


@pytest.fixture(scope="module")
def runner():
    spec = importlib.util.spec_from_file_location("run_grss", RUNNER)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def test_settings_follow_the_run_context(runner):
    ctx = {"de_kernel": 440, "analytic_partials": True, "n_iter_max": 12}
    s = runner.grss_settings(ctx)
    assert s["kernels"] == "DE440 + SB441-N16"
    assert s["partials"] == "STM" and s["od_max_iterations"] == "12"
    assert "remove_body" in s["self_perturbers"]
    ctx2 = {"de_kernel": 441, "analytic_partials": False, "n_iter_max": 5}
    s2 = runner.grss_settings(ctx2)
    assert s2["kernels"] == "DE441 + SB441-N16" and s2["partials"] == "finite difference"
    assert all(isinstance(v, str) for v in s2.values())
