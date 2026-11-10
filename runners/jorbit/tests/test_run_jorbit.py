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


# ── one worker process per object (bounded memory) ──────────


def prop(obj, dt, unc="none_detection_on"):
    return {
        "object": obj, "population": "NEO", "epoch_mjd_tdb": 61200.0,
        "test_type": "propagation", "dt_days": dt, "t_mjd_tdb": 61200.0 + dt,
        "propagation_uncertainty": unc,
        "ic_pos_au": [1.0, 0.1, 0.0], "ic_vel_au_d": [0.0, 0.0172, 0.0],
    }


def _plan(tmp_path):
    rows = [
        prop("A", 10.0),
        dict(eph("A", "500", 10.0), t_mjd_tdb=61210.0),
        eph("A", "F51", 10.0),                      # ground site: not attempted
        prop("B", 10.0),
        prop("A", 20.0, unc="first_order_detection_on"),  # covariance row: skipped
        {"object": "B", "test_type": "orbit_determination", "propagation_uncertainty": None},
        prop("B", -10.0),
    ]
    p = tmp_path / "plan.json"
    p.write_text(__import__("json").dumps(rows))
    return p, rows


def _main(runner, monkeypatch, tmp_path, fake):
    import json
    import sys

    plan, rows = _plan(tmp_path)
    out = tmp_path / "out.json"
    calls = []

    def spy(name, items, geo_span, data_dir, timeout_s):
        calls.append((name, [i for i, _ in items], geo_span))
        return fake(name, items)

    monkeypatch.setattr(runner, "_run_object", spy)
    monkeypatch.setattr(sys, "argv", ["run_jorbit.py", "--input", str(plan), "--output", str(out),
                                      "--data-dir", str(tmp_path)])
    rc = runner.main()
    return rc, calls, (json.loads(out.read_text()) if out.exists() else None)


def test_each_object_replays_in_its_own_worker_and_rows_keep_plan_order(runner, monkeypatch, tmp_path):
    def fake(name, items):
        return {str(i): {"jorbit_time_ms": 1.0, "jorbit_pos_au": [float(i), 0.0, 0.0]} for i, _ in items}, 100.0, None

    rc, calls, out = _main(runner, monkeypatch, tmp_path, fake)
    assert rc == 0
    # One worker per object, carrying only that object's propagation and geocentric rows.
    assert [c[0] for c in calls] == ["A", "B"]
    assert calls[0][1] == [0, 1] and calls[1][1] == [2, 4]
    assert calls[0][2] == [61210.0, 61210.0]  # one geocentre span for every worker
    # Plan order, the OD row kept without a result, then the not-attempted row.
    assert [(r["object"], r["test_type"]) for r in out] == [
        ("A", "propagation"), ("A", "ephemeris"), ("B", "propagation"),
        ("B", "orbit_determination"), ("B", "propagation"), ("A", "ephemeris")]
    assert out[0]["jorbit_pos_au"] == [0.0, 0.0, 0.0] and out[4]["jorbit_pos_au"] == [4.0, 0.0, 0.0]
    # Every replayed row states jorbit's version and configuration.
    for r in out[:5]:
        assert r["jorbit_version"] == r["source_version"]
        assert r["jorbit_settings"] == runner.JORBIT_SETTINGS
    assert "jorbit_pos_au" not in out[3]
    assert out[5]["jorbit_not_attempted_rows"] == 1


def test_a_failed_worker_marks_only_its_own_objects_rows(runner, monkeypatch, tmp_path):
    def fake(name, items):
        if name == "A":
            return {}, None, "jorbit worker for A exited -9: LLVM ERROR"
        return {str(i): {"jorbit_time_ms": 1.0} for i, _ in items}, 90.0, None

    rc, _calls, out = _main(runner, monkeypatch, tmp_path, fake)
    assert rc == 0
    a_rows = [r for r in out[:2]]
    assert all(r["jorbit_error"] == "jorbit worker for A exited -9: LLVM ERROR" for r in a_rows)
    assert all("jorbit_time_ms" not in r for r in a_rows)
    b_rows = [out[2], out[4]]
    assert all("jorbit_error" not in r and r["jorbit_time_ms"] == 1.0 for r in b_rows)


def test_a_broken_offline_setup_stops_the_run(runner, monkeypatch, tmp_path):
    def fake(name, items):
        raise runner.OfflineSetupFailed("ERROR: jorbit cannot run offline: de440.bsp missing")

    rc, calls, out = _main(runner, monkeypatch, tmp_path, fake)
    assert rc == 1 and len(calls) == 1 and out is None


def test_a_worker_without_kernels_reports_offline_setup_not_a_row_failure(runner, tmp_path):
    # A real worker process: the kernel check fails before jorbit is imported.
    with pytest.raises(runner.OfflineSetupFailed):
        runner._run_object("A", [[0, prop("A", 10.0)]], None, tmp_path / "empty", 120.0)


JORBIT_DATA = __import__("os").environ.get("JORBIT_DATA_DIR")


@pytest.mark.skipif(not JORBIT_DATA, reason="set JORBIT_DATA_DIR to the jorbit folder (jorbit venv)")
def test_a_real_worker_integrates_and_the_parent_never_imports_jorbit(runner):
    import sys

    rows = [[0, prop("A", 10.0)], [1, prop("A", -10.0)]]
    updates, peak, failure = runner._run_object("A", rows, None, pathlib.Path(JORBIT_DATA), 600.0)
    assert failure is None and peak and peak > 0
    assert set(updates) == {"0", "1"}
    assert len(updates["0"]["jorbit_pos_au"]) == 3 and updates["0"]["jorbit_pos_au"] != updates["1"]["jorbit_pos_au"]
    assert "jorbit" not in sys.modules and "jax" not in sys.modules
