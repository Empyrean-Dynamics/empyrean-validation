"""run_layup: reported sigma kept, missing sigma filled and labelled, API driver settings.

    runners/layup/.venv/bin/python -m pytest runners/layup/tests -q
"""

import importlib.util
import io
import subprocess
import sys
from pathlib import Path

import pytest

pd = pytest.importorskip("pandas")

RUNNER = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(RUNNER))
sys.path.insert(0, str(RUNNER.parent))
spec = importlib.util.spec_from_file_location("run_layup", RUNNER / "run_layup.py")
RL = importlib.util.module_from_spec(spec)
sys.modules["run_layup"] = RL
spec.loader.exec_module(RL)

HEADER = "permID|provID|trkSub|mode|stn|obsTime|ra|dec|rmsRA|rmsDec|rmsCorr|astCat\n"


def _psv(tmp_path, rows):
    p = tmp_path / "X.psv"
    p.write_text("# version=2017\n" + HEADER + "".join(r + "\n" for r in rows))
    return p


def _row(stn, t, ra, dec, rra="", rdec="", cat="Gaia2"):
    return f"X|x|x|CCD|{stn}|{t}|{ra}|{dec}|{rra}|{rdec}|0.0|{cat}"


def test_reported_sigma_is_kept_and_missing_is_filled_and_labelled(tmp_path):
    p = _psv(tmp_path, [
        _row("703", "2020-01-01T00:00:00.000Z", 10.0, 5.0, "0.31", "0.29"),   # reported
        _row("703", "2020-01-01T00:10:00.000Z", 10.1, 5.1),                   # station 703 -> 0.8
        _row("ZZ9", "2020-01-01T00:20:00.000Z", 10.2, 5.2),                   # known catalog -> 1.0
        _row("ZZ9", "2020-01-01T00:30:00.000Z", 10.3, 5.3, cat="MadeUp"),     # unrecognized -> 1.5
    ])
    out, rep = RL._prepare_input(p, "X", tmp_path)
    df = pd.read_csv(out, sep="|", skiprows=1, dtype=str)
    assert df["rmsRA"].tolist() == ["0.31", "0.8", "1.0", "1.5"]
    assert df["rmsDec"].tolist() == ["0.29", "0.8", "1.0", "1.5"]
    assert rep["reported"] == 1 and rep["filled"] == 3
    assert rep["filled_by_level"] == {
        "station": 1, "catalog": 1, "no_catalog": 0, "catalog_unrecognized": 1}


def test_unused_text_columns_are_dropped_so_layups_tie_break_sort_cannot_raise(tmp_path):
    hdr = "permID|provID|trkSub|mode|stn|obsTime|ra|dec|rmsRA|rmsDec|rmsCorr|astCat|band|notes|remarks\n"
    rows = ["X||x|CCD|703|2020-01-01T00:00:00.000Z|10.0|5.0|0.3|0.3|0.0|Gaia2|G||",
            "X|P/1|x|CCD|703|2020-01-01T00:00:00.000Z|10.0|5.0|0.3|0.3|0.0|Gaia2||n|r"]  # duplicate obsTime
    p = tmp_path / "D.psv"
    p.write_text("# version=2017\n" + hdr + "".join(r + "\n" for r in rows))
    out, _ = RL._prepare_input(p, "D", tmp_path)
    cols = pd.read_csv(out, sep="|", skiprows=1, dtype=str).columns.tolist()
    for c in RL.UNUSED_COLUMNS:
        assert c not in cols
    for c in ("stn", "obsTime", "ra", "dec", "rmsRA", "rmsDec", "astCat"):
        assert c in cols


def test_a_fixture_with_no_rms_columns_gets_them(tmp_path):
    p = tmp_path / "Y.psv"
    p.write_text("# version=2017\npermID|stn|obsTime|ra|dec|astCat\n"
                 "Y|F51|2020-01-01T00:00:00.000Z|10.0|5.0|Gaia2\n")
    out, rep = RL._prepare_input(p, "Y", tmp_path)
    df = pd.read_csv(out, sep="|", skiprows=1, dtype=str)
    assert df["rmsRA"].tolist() == ["0.2"] and rep["filled_by_level"]["station"] == 1


def test_half_reported_row_keeps_the_reported_half(tmp_path):
    p = _psv(tmp_path, [_row("F51", "2020-01-01T00:00:00.000Z", 10.0, 5.0, "0.31", "")])
    out, rep = RL._prepare_input(p, "X", tmp_path)
    df = pd.read_csv(out, sep="|", skiprows=1, dtype=str)
    assert df["rmsRA"].tolist() == ["0.31"] and df["rmsDec"].tolist() == ["0.2"]
    assert rep["partially_reported"] == 1


def test_settings_state_every_choice_and_no_removed_flags():
    for k in ("weight_data", "debias", "fit_nongrav", "iod", "engine", "sigma_fill", "nongrav_gr",
              "force_model", "interface"):
        assert k in RL.LAYUP_SETTINGS
    assert "supplied" in RL.LAYUP_SETTINGS["weight_data"]
    assert "EFCC18" in RL.LAYUP_SETTINGS["debias"]
    assert "auto" in RL.LAYUP_SETTINGS["fit_nongrav"]
    # One settings surface: none of the three runners takes a weighting/debias flag.
    for name in ("run_layup.py", "run_layup_walk.py", "run_layup_mc_predict.py"):
        src = (RUNNER / name).read_text()
        assert '"--weight-data"' not in src and '"--debias"' not in src, name


def test_the_api_driver_requests_the_documented_settings():
    """Read the keywords of the real orbitfit(...) call, not the docstring."""
    import ast

    tree = ast.parse((RUNNER / "fit_one.py").read_text())
    calls = [n for n in ast.walk(tree) if isinstance(n, ast.Call)
             and getattr(n.func, "id", None) == "orbitfit"]
    assert len(calls) == 1
    kw = {k.arg: ast.literal_eval(k.value) for k in calls[0].keywords
          if isinstance(k.value, ast.Constant)}
    assert kw["weight_data"] == "supplied"
    assert kw["debias"] is True
    assert kw["fit_nongrav"] == "auto"
    assert kw["iod"] == "auto" and kw["engine"] == "cartesian"


DATA = RUNNER / "data"


@pytest.mark.skipif(not DATA.is_dir(), reason="layup data directory not staged (make setup-layup)")
def test_a_failed_fit_is_a_named_reason_not_a_skip(tmp_path):
    # ZZ9 is no observatory layup can place: layup raises, the row names the reason.
    # (A missing data directory is no test of this: layup then downloads at fit
    # time, which is why the runners refuse to start without missing_data() == [].)
    p = _psv(tmp_path, [_row("ZZ9", f"2020-01-0{i}T00:00:00.000Z", 10.0 + i, 5.0, "0.3", "0.3")
                        for i in range(1, 4)])
    payload, reason = RL._fit_one(p, "X", str(DATA.absolute()), 120.0)
    assert payload is None and reason.startswith("orbitfit exit") and "ZZ9" in reason


@pytest.mark.skipif(not (DATA / "debias_hires2018.tgz").exists() and not DATA.is_dir(),
                    reason="layup data directory not staged (make setup-layup)")
def test_api_driver_runs_and_returns_auto_nongrav_columns(tmp_path):
    import os

    fx = Path(os.environ.get("FIXTURES_PSV", RUNNER.parents[1] / "fixtures" / "psv")) / "2026 DA.psv"
    if not fx.exists():
        pytest.skip("fixture absent")
    payload, reason = RL._fit_one(fx, "2026_DA", str(DATA), 600.0)
    assert reason is None, reason
    assert payload["layup_converged"] is True
    assert payload["layup_sigma_source"]["total"] > 0
    assert "layup_nongrav_adopted" in payload  # the auto ladder's adopted terms (maybe empty)


@pytest.mark.skipif(not DATA.is_dir(), reason="layup data directory not staged (make setup-layup)")
def test_apophis_with_duplicate_obstimes_fits(tmp_path):
    """Regression: Apophis failed with layup's str/float tie-break TypeError."""
    import os

    fx = Path(os.environ.get("FIXTURES_PSV", RUNNER.parents[1] / "fixtures" / "psv")) / "Apophis.psv"
    if not fx.exists():
        pytest.skip("fixture absent")
    payload, reason = RL._fit_one(fx, "Apophis", str(DATA), 1800.0)
    assert reason is None, reason
    assert payload["layup_n_obs_used"] > 1000


# ── the walk fits through the same adapter and driver ───────


def _walk():
    spec = importlib.util.spec_from_file_location("run_layup_walk", RUNNER / "run_layup_walk.py")
    W = importlib.util.module_from_spec(spec)
    sys.modules["run_layup_walk"] = W
    spec.loader.exec_module(W)
    return W


class _Done(Exception):
    pass


def test_walk_window_fit_uses_the_shared_adapter_and_the_api_driver(tmp_path, monkeypatch):
    W = _walk()
    p = _psv(tmp_path, [
        _row("703", "2020-01-01T00:00:00.000Z", 10.0, 5.0, "0.31", "0.29"),
        _row("703", "2020-01-01T00:10:00.000Z", 10.1, 5.1),
    ])
    seen = {}

    def fake_run(cmd, timeout_s, cwd, env):
        seen["cmd"], seen["env"] = cmd, env
        seen["input"] = Path(cmd[2]).read_text()
        raise _Done

    monkeypatch.setattr(W.RL, "run_bounded", fake_run)
    opts = W.Namespace(ar_data_path=str(tmp_path / "data"), timeout=5.0)
    guess = {"epochMJD_TDB": 60000.0, "x": 1.0, "y": 0.0, "z": 0.0,
             "xdot": 0.0, "ydot": 0.017, "zdot": 0.0}
    with pytest.raises(_Done):
        W._fit_window(p, "X", tmp_path, guess, opts)
    cmd = seen["cmd"]
    assert cmd[1] == str(RL.FIT_ONE)
    assert cmd[cmd.index("--ar") + 1] == opts.ar_data_path
    assert "--guess" in cmd and Path(cmd[cmd.index("--guess") + 1]).exists()
    assert seen["env"]["LAYUP_CACHE_DIR"] == opts.ar_data_path
    df = pd.read_csv(io.StringIO(seen["input"]), sep="|", skiprows=1, dtype=str)
    assert df["rmsRA"].tolist() == ["0.31", "0.8"]  # reported kept, 703 filled at 0.8"
    with pytest.raises(_Done):
        W._fit_window(p, "X", tmp_path, None, opts)
    assert "--guess" not in seen["cmd"]  # a cold window passes no guess


def test_walk_records_solve_for_width_from_the_adopted_nongrav_terms():
    W = _walk()
    nan = float("nan")
    assert W._n_solve_for({"a1": nan, "a2": nan, "a3": nan}) == 6
    assert W._n_solve_for({"a1": nan, "a2": -2.4e-14, "a3": nan}) == 7
    assert W._n_solve_for({"a1": 2.1e-7, "a2": -2.4e-14, "a3": nan}) == 8
    assert W._n_solve_for({}) == 6  # a gravity-only schema has no a-columns


def test_the_driver_refuses_a_guess_that_is_not_bcart_eq(tmp_path):
    p = _psv(tmp_path, [_row("703", f"2020-01-0{i}T00:00:00.000Z", 10.0 + i, 5.0, "0.3", "0.3")
                        for i in range(1, 4)])
    prepared, _ = RL._prepare_input(p, "X", tmp_path)
    g = tmp_path / "g.psv"
    g.write_text("layupID|FORMAT|epochMJD_TDB|q|e|inc|node|argPeri|t_p_MJD_TDB|flag|csq|ndof|niter\n"
                 "X|COM|60000.0|1.0|0.1|1.0|0.0|0.0|60000.0|0|0.0|0|0\n")
    proc = subprocess.run([sys.executable, str(RL.FIT_ONE), str(prepared), "--ar", str(tmp_path),
                           "--primary-id", "layupID", "--out", str(tmp_path / "o.csv"),
                           "--guess", str(g)], capture_output=True, text=True, timeout=300)
    assert proc.returncode == 4 and "BCART_EQ" in proc.stderr


@pytest.mark.skipif(not DATA.is_dir(), reason="layup data directory not staged (make setup-layup)")
def test_walk_warm_window_fits_from_the_previous_state(tmp_path):
    """End to end through the API driver: a cold fit, then a warm fit seeded with its
    state converges without an IOD (the CLI's -g path cannot run at layup 1.1.0)."""
    import os

    W = _walk()
    fx = Path(os.environ.get("FIXTURES_PSV", RUNNER.parents[1] / "fixtures" / "psv")) / "2026 DA.psv"
    if not fx.exists():
        pytest.skip("fixture absent")
    opts = W.Namespace(ar_data_path=str(DATA.absolute()), timeout=600.0)
    (tmp_path / "c").mkdir()
    (tmp_path / "w").mkdir()
    cold = W._fit_window(fx, "2026_DA", tmp_path / "c", None, opts)
    assert cold.row is not None, cold.failure
    guess = {k: cold.row[k] for k in ("epochMJD_TDB", "x", "y", "z", "xdot", "ydot", "zdot")}
    warm = W._fit_window(fx, "2026_DA", tmp_path / "w", guess, opts)
    assert warm.row is not None, warm.failure
    assert W._inum(warm.row, "flag") == 0
    assert abs(W._fnum(warm.row, "csq") - W._fnum(cold.row, "csq")) < 1e-3 * max(1.0, W._fnum(cold.row, "csq"))


# ── self-perturbers and the runner's own rows ───────────────


def test_self_perturbers_are_named_unsupported_and_others_are_not():
    for name in ("Iris", "Vesta", "Pallas", "Hygiea"):
        assert "self-perturber" in RL.unsupported_reason(name)
    for name in ("Eros", "Apophis", "2026 DA", "1I/'Oumuamua"):
        assert RL.unsupported_reason(name) is None


def test_walk_never_fits_a_self_perturber_window(tmp_path, monkeypatch):
    W = _walk()

    def no_fit(*a, **k):
        raise AssertionError("a self-perturber window must not be fitted")

    monkeypatch.setattr(W.RL, "run_bounded", no_fit)
    p = _psv(tmp_path, [_row("703", "2020-01-01T00:00:00.000Z", 10.0, 5.0)])
    out = W._fit_window(p, "Vesta", tmp_path, None, W.Namespace(ar_data_path=str(tmp_path), timeout=5.0))
    assert out.row is None and "self-perturber" in out.failure


@pytest.mark.skipif(not DATA.is_dir(), reason="layup data directory not staged (make setup-layup)")
def test_main_writes_an_unsupported_row_and_a_named_failed_row(tmp_path, monkeypatch):
    import json

    fx = tmp_path / "fx"
    fx.mkdir()
    rows = [_row("ZZ9", f"2020-01-0{i}T00:00:00.000Z", 10.0 + i, 5.0, "0.3", "0.3") for i in range(1, 4)]
    for stem in ("Vesta", "Nowhere"):
        (fx / f"{stem}.psv").write_text("# version=2017\n" + HEADER + "".join(r + "\n" for r in rows))
    out = tmp_path / "out.json"
    monkeypatch.setattr(sys, "argv", ["run_layup.py", str(fx), "--ar-data-path", str(DATA),
                                      "--output", str(out), "--timeout", "120"])
    assert RL.main() == 0
    recs = {r["object"]: r for r in json.loads(out.read_text())}
    assert set(recs) == {"Vesta", "Nowhere"}  # neither is dropped
    v, n = recs["Vesta"], recs["Nowhere"]
    assert v["layup_converged"] is False and "self-perturber" in v["layup_unsupported"]
    assert "layup_chi2" not in v and "layup_error" not in v  # never fitted
    assert n["layup_converged"] is False and n["layup_error"].startswith("orbitfit exit")
    for r in (v, n):
        assert r["layup_settings"] == RL.LAYUP_SETTINGS and r["layup_version"] == "1.1.0"


def test_a_timeout_ends_the_fits_own_children(tmp_path):
    """layup's pool child must not outlive a timed-out fit and keep fitting."""
    import os
    import time

    pid_file = tmp_path / "child.pid"
    script = ("import subprocess, sys, time\n"
              "c = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'])\n"
              f"open({str(pid_file)!r}, 'w').write(str(c.pid))\n"
              "time.sleep(60)\n")
    with pytest.raises(subprocess.TimeoutExpired):
        RL.run_bounded([sys.executable, "-c", script], 2.0, tmp_path, dict(os.environ))
    child = int(pid_file.read_text())
    deadline = time.monotonic() + 5.0  # bounded: the orphan is reaped by init
    alive = True
    while alive and time.monotonic() < deadline:
        try:
            os.kill(child, 0)
            time.sleep(0.1)
        except ProcessLookupError:
            alive = False
    if alive:
        os.kill(child, 9)  # clean up before failing
    assert not alive, "the fit's child survived the timeout"
