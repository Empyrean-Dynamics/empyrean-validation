"""run_jorbit: ground-site ephemeris rows become one explicit row per object."""

import importlib.util
import pathlib

import pytest

RUNNER = pathlib.Path(__file__).resolve().parents[1] / "run_jorbit.py"


@pytest.fixture(scope="module")
def runner():
    spec = importlib.util.spec_from_file_location("run_jorbit", RUNNER)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def eph(obj, obs, dt):
    return {
        "object": obj,
        "population": "NEO",
        "epoch_mjd_tdb": 61200.0,
        "test_type": "ephemeris",
        "observer": obs,
        "dt_days": dt,
        "propagation_uncertainty": "none_detection_on",
        "ref_ra_rad": 1.0,
        "ref_dec_rad": 0.1,
    }


def test_one_row_per_object_naming_reason_observers_and_count(runner):
    na = runner.SiteRowsNotAttempted("reason X")
    for obj in ("Apophis", "Didymos"):
        for obs in ("W84", "F51", "X05", "I41"):
            for dt in (-10.0, 10.0):
                na.add(eph(obj, obs, dt))
    rows = na.rows("ts", "jorbit 1.7.1")
    assert [r["object"] for r in rows] == ["Apophis", "Didymos"]
    assert len(na) == 2 and na.n_rows == 16
    for r in rows:
        assert r["jorbit_not_attempted"] == "reason X"
        assert r["jorbit_not_attempted_observers"] == ["F51", "I41", "W84", "X05"]
        assert r["jorbit_not_attempted_rows"] == 8
        assert r["channel"] == "jorbit" and r["test_type"] == "ephemeris"
        # Never a fabricated value: no comparison or timing field on the row.
        assert not any(
            k.startswith("jorbit_") and not k.startswith("jorbit_not_attempted")
            for k in r
        )
        assert "dt_days" not in r and "observer" not in r
    assert len(na.report_lines()) == 2


def test_no_ground_site_rows_means_no_extra_rows(runner):
    na = runner.SiteRowsNotAttempted("reason")
    assert na.rows("ts", "v") == [] and na.n_rows == 0 and na.report_lines() == []


def test_the_reason_names_the_horizons_dependency(runner):
    assert "Horizons" in runner.SITE_NOT_ATTEMPTED_REASON
    assert runner.GEOCENTER == "500"


def test_source_version_names_jorbit_jax_and_jaxlib_never_blank(runner):
    v = runner._jorbit_source_version()
    for part in ("jorbit ", "jax ", "jaxlib ", "DE440+SB441-N16"):
        assert part in v
