"""run_kete: every row states kete's version and configuration."""

import importlib.util
import json
import os
import pathlib
import subprocess
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


DATA = os.environ.get("KETE_DATA_DIR")


def _run_main(tmp_path, plan_rows, *flags, out_name="o.json"):
    """run_kete.py in a fresh process (kete loads its kernels once, at import),
    with a HOME of its own so kete's default cache cannot stand in for the staged
    one. Returns the emitted rows."""
    plan = tmp_path / "plan.json"
    plan.write_text(json.dumps(plan_rows))
    out = tmp_path / out_name
    env = {k: v for k, v in os.environ.items() if k != "KETE_CACHE_DIR"}
    env["HOME"] = str(tmp_path / "home")
    r = subprocess.run(
        [sys.executable, str(HERE / "run_kete.py"), "--input", str(plan), "--output", str(out),
         "--data-dir", DATA, *flags],
        capture_output=True, text=True, timeout=300, env=env,
    )
    assert r.returncode == 0, r.stderr[-3000:]
    return json.loads(out.read_text())


@pytest.mark.skipif(not DATA, reason="set KETE_DATA_DIR to the kete folder (kete venv)")
def test_every_emitted_row_carries_version_and_settings(runner, tmp_path):
    # A row kind kete does not replay: emitted, untouched, and still stamped.
    rows = _run_main(tmp_path, [{"object": "Eros", "test_type": "orbit_determination_radar",
                                 "propagation_uncertainty": None}])
    assert len(rows) == 1
    assert rows[0]["kete_version"] == rows[0]["source_version"]
    settings = rows[0]["kete_settings"]
    for key in ("force_model", "self_perturbers", "non_grav", "ephemeris", "od"):
        assert settings[key] == runner.KETE_SETTINGS[key]
    assert settings["asteroid_masses"].startswith("16 (SB441-N16)")


def test_kete_imported_before_its_cache_is_staged_is_refused(runner, tmp_path, monkeypatch):
    monkeypatch.setitem(sys.modules, "kete", object())
    with pytest.raises(runner.KeteDataError, match="before the runner staged its cache"):
        runner._load_kete(tmp_path)


# ── the sixteen masses and the self-perturbers (no kete needed) ──────────


def test_the_sixteen_masses_are_kete_five_plus_eleven_registered(runner):
    assert len(runner.SB441_N16) == 16 and len(set(runner.SB441_N16)) == 16
    assert set(runner.KETE_OWN_FIVE) | set(runner.KETE_REGISTERED) == set(runner.SB441_N16)
    assert not set(runner.KETE_OWN_FIVE) & set(runner.KETE_REGISTERED)
    assert len(runner.KETE_MASS_IDS) == 16
    assert 20000704 in runner.KETE_MASS_IDS and 2000007 in runner.KETE_MASS_IDS
    assert 2000004 not in runner.KETE_MASS_IDS  # Vesta rides kete's own Horizons id


@pytest.mark.parametrize(
    "excluded, unsupported",
    [([2000007], True), ([2000004], True), ([2000002], True), ([2000010], True),
     ([2000511], True), ([], False), ([2000005], False), (None, False)],
)
def test_a_self_perturber_row_is_named_unsupported(runner, excluded, unsupported):
    row = {"object": "X", "test_type": "propagation"}
    if excluded is not None:
        row["excluded_perturbers_naif"] = excluded
    why = runner.kete_unsupported_reason(row)
    assert (why is not None) == unsupported
    if unsupported:
        assert "self-perturber" in why and "sixteen" in why


class _FakeKete:
    """kete's mass registry, as the runner calls it."""

    def __init__(self, table_ids, drop=None):
        self.registered = [(f"b{n}", 20000000 + n, 1e-10, 1e-6) for n in (1, 2, 4, 10, 704)]
        self.calls = []
        self.drop = drop
        table = [(f"b{i}", i, 1e-11 * (i % 1000), 1e-7) for i in table_ids]
        fake = self

        class _Core:
            @staticmethod
            def known_masses():
                return table

            @staticmethod
            def registered_masses():
                return [*fake.registered, ("sun", 10, 1.0, 0.0)]

        class _Prop:
            @staticmethod
            def register_custom_mass(naif, mass, radius):
                fake.calls.append((naif, mass, radius))
                if naif != fake.drop:
                    fake.registered.append((f"c{naif}", naif, mass, radius))

        self._core, self.propagation = _Core, _Prop


def test_the_eleven_are_registered_with_kete_own_masses(runner):
    fake = _FakeKete([20000000 + n for n in runner.SB441_N16])
    ids = runner.register_sb441_n16(fake)
    assert ids == sorted(runner.KETE_MASS_IDS)
    assert [c[0] for c in fake.calls] == [2000000 + n for n in runner.KETE_REGISTERED]
    for naif, mass, radius in fake.calls:
        assert (mass, radius) == (1e-11 * ((naif - 2000000 + 20000000) % 1000), 1e-7)


def test_a_body_missing_from_kete_table_is_named(runner):
    fake = _FakeKete([20000000 + n for n in runner.SB441_N16 if n != 65])
    with pytest.raises(runner.KeteDataError, match="20000065"):
        runner.register_sb441_n16(fake)


def test_a_registry_short_of_sixteen_is_refused(runner):
    fake = _FakeKete([20000000 + n for n in runner.SB441_N16], drop=2000087)
    with pytest.raises(runner.KeteDataError, match="not the sixteen"):
        runner.register_sb441_n16(fake)


@pytest.mark.skipif(not DATA, reason="set KETE_DATA_DIR to the kete folder (kete venv)")
def test_kete_runs_sixteen_masses_and_names_iris_unsupported(runner, tmp_path):
    """A plain heliocentric orbit propagates finite with the sixteen masses; Iris,
    which the plan excludes from its own forces, is named unsupported and not run."""
    base = {"epoch_mjd_tdb": 61200.0, "dt_days": -365.0, "t_mjd_tdb": 60835.0,
            "ic_pos_au": [2.5, 0.0, 0.0], "ic_vel_au_d": [0.0, 0.0108, 0.0005],
            "propagation_uncertainty": "none_detection_on", "test_type": "propagation"}
    plain, iris = _run_main(tmp_path, [{**base, "object": "Plain"},
                                       {**base, "object": "Iris", "excluded_perturbers_naif": [2000007]}])
    assert all(abs(x) < 10 for x in plain["kete_pos_au"]), plain
    assert "kete_unsupported" not in plain
    assert iris["kete_unsupported"] == runner.SELF_PERTURBER_UNSUPPORTED
    assert "kete_pos_au" not in iris and "kete_time_ms" not in iris
    assert iris["kete_settings"]["asteroid_masses"].startswith("16 (SB441-N16)")


# Fields that differ between any two runs of the same rows: the run's clock.
_RUN_CLOCK = ("timestamp", "kete_time_ms")


@pytest.mark.skipif(not DATA, reason="set KETE_DATA_DIR to the kete folder (kete venv)")
def test_object_legs_merge_to_the_unchunked_run(runner, tmp_path):
    """Two object legs (`--only`), merged by concat-external, write exactly the
    rows one unchunked run writes over the same plan, the run's clock aside."""
    sys.path.insert(0, str(HERE.parents[1] / "scripts"))
    import importlib

    sc = importlib.import_module("schedule")
    cc = importlib.util.spec_from_file_location("cc_kete", HERE.parents[1] / "scripts" / "channel-chunks.py")
    cc_mod = importlib.util.module_from_spec(cc)
    cc.loader.exec_module(cc_mod)

    def row(obj, dt, observer=None):
        x = 2.5 if obj == "A" else 1.2
        return {"object": obj, "epoch_mjd_tdb": 61200.0, "dt_days": dt, "t_mjd_tdb": 61200.0 + dt,
                "ic_pos_au": [x, 0.0, 0.0], "ic_vel_au_d": [0.0, 0.0172 / x ** 0.5, 0.0005],
                "propagation_uncertainty": "none_detection_on", "observer": observer, "force_model": "standard",
                "test_type": "ephemeris" if observer else "propagation"}

    plan = [row(o, dt, obs) for o in ("A", "B") for dt in (-365.0, 90.0) for obs in (None, "500")]
    whole = _run_main(tmp_path, plan, out_name="whole.json")
    results = tmp_path / "results"
    results.mkdir()
    costs = tmp_path / "costs.json"
    costs.write_text(json.dumps({"schema": 1, "target_leg_min": 1.0, "timeout_factor": 3.0, "timeout_floor_min": 30,
                                 "tools": {"kete": {"setup_min": 0.5, "leg_overhead_min": 0.1,
                                                    "default_object_min": 1.0, "object_min": {"A": 1.0, "B": 1.0}}}}))
    sched = sc.build_external_schedule(tmp_path / "plan.json", costs, ["kete"])
    legs = sched["tools"]["kete"]["legs"]
    assert [leg["objects"] for leg in legs] == [["A"], ["B"]]
    (tmp_path / "external-schedule.json").write_text(json.dumps(sched))
    for leg in legs:
        rows = _run_main(tmp_path, plan, "--only", ",".join(leg["objects"]), out_name=f"leg{leg['leg']}.json")
        assert {r["object"] for r in rows} == set(leg["objects"])
        (results / f"validation_kete.leg{leg['leg']}.json").write_text(json.dumps(rows))
    assert cc_mod.main(["concat-external", "--plan", str(tmp_path / "plan.json"), "--schedule",
                        str(tmp_path / "external-schedule.json"), "--tool", "kete", "--results-dir", str(results)]) == 0
    merged = json.loads((results / "validation_kete.json").read_text())

    def clockless(rows):
        return [{k: v for k, v in r.items() if k not in _RUN_CLOCK} for r in rows]

    assert all(r.get("kete_pos_au") for r in whole if r["test_type"] == "propagation"), "the rows ran"
    assert clockless(merged) == clockless(whole)
