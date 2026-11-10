"""External legs (S7): jorbit and OrbFit cut by object, each leg with a timeout
sized to its predicted wall, and every tool's legs merged back into its one
comparator file before the merge, in the order one unchunked run writes, with
comparator isolation intact.

Covers scripts/schedule.py (pack-external, validate-external),
scripts/channel-chunks.py (external-objects, concat-external) and the two
runner flags the object legs use (run_jorbit.py --only, run_orbfit.py
--only / --defer-all-failed)."""

import importlib.util
import json
import math
import pathlib
import subprocess
import sys

import pytest

SCRIPTS = pathlib.Path(__file__).resolve().parents[1]
ROOT = SCRIPTS.parent
TS = "2026-11-09T00:00:00+00:00"


def load(name, path):
    sys.path.insert(0, str(SCRIPTS))
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


@pytest.fixture(scope="module")
def sc():
    return load("schedule_for_external", SCRIPTS / "schedule.py")


@pytest.fixture(scope="module")
def cc():
    return load("channel_chunks_for_external", SCRIPTS / "channel-chunks.py")


OBJECTS = ["A", "B", "C", "D", "E"]


def row(obj, tt, method=None, dt=0.0, observer=None):
    return {"object": obj, "test_type": tt, "propagation_uncertainty": method, "dt_days": dt, "observer": observer, "force_model": "standard", "timestamp": TS}


def plan():
    """Object-major propagation and ephemeris rows, then every OD row: the
    reference's order, which is not object-major across the whole plan."""
    out = []
    for o in OBJECTS:
        for dt in (10.0, 20.0):
            out.append(row(o, "propagation", "none_detection_on", dt))
        out.append(row(o, "ephemeris", "none_detection_on", 10.0, "500"))
        out.append(row(o, "ephemeris", "none_detection_on", 10.0, "W84"))
    for o in OBJECTS:
        out.append(row(o, "orbit_determination"))
    return out


COSTS = {
    "schema": 1,
    "target_leg_min": 10.0,
    "timeout_factor": 3.0,
    "timeout_floor_min": 30,
    "tools": {
        "assist": {"job_min": 12.0},
        "findorb": {"job_min": 20.0},
        "jorbit": {"setup_min": 1.0, "leg_overhead_min": 1.0, "default_object_min": 4.0, "object_min": {"A": 9.0, "B": 4.0, "C": 4.0, "D": 3.0}},
        "orbfit": {"setup_min": 1.5, "leg_overhead_min": 0.5, "default_object_min": 1.0, "object_min": {o: 2.0 for o in OBJECTS}},
    },
}


@pytest.fixture
def world(sc, tmp_path):
    pp = tmp_path / "validation_plan.json"
    pp.write_text(json.dumps(plan()))
    cp = tmp_path / "external-costs.json"
    cp.write_text(json.dumps(COSTS))
    return {"pp": pp, "cp": cp, "dir": tmp_path}


def sched(sc, world, tools=("assist", "findorb", "jorbit", "orbfit"), target=None):
    s = sc.build_external_schedule(world["pp"], world["cp"], list(tools), target)
    sp = world["dir"] / "external-schedule.json"
    sp.write_text(json.dumps(s))
    return s, sp


# ── pack-external ───────────────────────────────────────────────────────────


def test_a_per_object_tool_is_cut_longest_object_first_and_covers_each_object_once(sc, world):
    s, _ = sched(sc, world)
    j = s["tools"]["jorbit"]
    total = 9.0 + 4.0 + 4.0 + 3.0 + 4.0  # E is priced at the default
    assert j["chunked"] and len(j["legs"]) == math.ceil(total / 10.0) == 3
    assert sorted(o for leg in j["legs"] for o in leg["objects"]) == OBJECTS
    assert j["legs"][0]["objects"] == ["A"], "the longest object opens the first leg"
    for leg in j["legs"]:
        assert leg["objects"] == [o for o in OBJECTS if o in leg["objects"]], "a leg's objects keep plan order"
        load = sum(COSTS["tools"]["jorbit"]["object_min"].get(o, 4.0) for o in leg["objects"])
        assert leg["predicted_min"] == pytest.approx(2.0 + load)
        assert leg["timeout_min"] == max(30, math.ceil(3.0 * leg["predicted_min"]))
    assert j["estimated"] == ["E: not in the external cost table; priced at 4.0 min"]
    assert sc.validate_external(plan(), s["plan_sha256"], s) == []


def test_every_other_tool_runs_as_one_leg_over_every_object_with_a_sized_timeout(sc, world):
    s, _ = sched(sc, world, tools=("assist", "findorb", "kete"))
    assert s["tools"]["assist"]["legs"] == [{"leg": "01", "objects": None, "predicted_min": 12.0, "timeout_min": 36}]
    assert s["tools"]["findorb"]["legs"][0]["timeout_min"] == 60
    assert s["tools"]["findorb"]["stems"] == ["findorb", "findorb_radar"]
    kete = s["tools"]["kete"]
    assert kete["legs"][0]["predicted_min"] == 20.0 and kete["estimated"], "an unknown tool is priced at the largest job wall, named"
    assert sc.external_leg_matrix(s) == [
        {"tool": "assist", "leg": "01", "timeout": 36},
        {"tool": "findorb", "leg": "01", "timeout": 60},
        {"tool": "kete", "leg": "01", "timeout": 60},
    ]


def test_a_rehearsal_target_cuts_two_objects_into_two_legs(sc, world):
    s, _ = sched(sc, world, tools=("orbfit",), target=0.5)
    assert len(s["tools"]["orbfit"]["legs"]) == len(OBJECTS)
    assert s["tools"]["orbfit"]["all_failed_field"] == "orbfit_error"


def test_the_external_validator_refuses_each_broken_shape(sc, world):
    s, _ = sched(sc, world)
    p = plan()

    def bad(mutate):
        t = json.loads(json.dumps(s))
        mutate(t)
        return sc.validate_external(p, s["plan_sha256"], t)

    assert any("in no leg" in b for b in bad(lambda t: t["tools"]["jorbit"]["legs"][0]["objects"].clear()))
    assert any("is in legs" in b for b in bad(lambda t: t["tools"]["jorbit"]["legs"][1]["objects"].append("A")))
    assert any("below the floor" in b for b in bad(lambda t: t["tools"]["assist"]["legs"][0].update(timeout_min=12)))
    assert any("one leg over every object" in b for b in bad(lambda t: t["tools"]["assist"]["legs"][0].update(objects=["A"])))
    assert sc.validate_external(p, "0" * 64, s) == ["the external schedule was built for a different plan (hash mismatch)"]


# ── external-objects ────────────────────────────────────────────────────────


def test_external_objects_names_a_legs_objects_and_refuses_a_stale_plan(cc, sc, world, capsys):
    _, sp = sched(sc, world)
    assert cc.main(["external-objects", "--plan", str(world["pp"]), "--schedule", str(sp), "--tool", "jorbit", "--leg", "01"]) == 0
    assert capsys.readouterr().out.strip() == "A"
    assert cc.main(["external-objects", "--plan", str(world["pp"]), "--schedule", str(sp), "--tool", "assist", "--leg", "01"]) == 0
    assert capsys.readouterr().out.strip() == ""
    with pytest.raises(SystemExit, match="has no leg"):
        cc.main(["external-objects", "--plan", str(world["pp"]), "--schedule", str(sp), "--tool", "jorbit", "--leg", "09"])
    world["pp"].write_text(json.dumps(plan()[1:]))
    with pytest.raises(SystemExit, match="different plan"):
        cc.main(["external-objects", "--plan", str(world["pp"]), "--schedule", str(sp), "--tool", "jorbit", "--leg", "01"])


# ── concat-external ─────────────────────────────────────────────────────────


def jorbit_like(objects):
    """What run_jorbit.py writes over `objects` of the plan: its replayed rows in
    plan order (OD rows ride along unreplayed), then one summary row per object
    for its ground-site ephemeris, which is not a plan row (no dt_days)."""
    rows = []
    for r in plan():
        if r["object"] not in objects or r.get("observer") == "W84":
            continue
        rows.append(dict(r, channel="jorbit", jorbit_vs_horizons_km=0.001 * float(r["dt_days"]) + ord(r["object"])))
    for o in [o for o in OBJECTS if o in objects]:
        rows.append({"object": o, "test_type": "ephemeris", "propagation_uncertainty": "none_detection_on", "channel": "jorbit", "jorbit_not_attempted": "ground sites", "jorbit_not_attempted_rows": 1})
    return rows


def stage_legs(world, s, tool, writer):
    out = world["dir"] / "results"
    out.mkdir(exist_ok=True)
    for leg in s["tools"][tool]["legs"]:
        objs = leg["objects"] if leg["objects"] is not None else OBJECTS
        (out / f"validation_{tool}.leg{leg['leg']}.json").write_text(json.dumps(writer(objs)))
    return out


def concat_ext(cc, world, sp, tool, *extra):
    return cc.main(["concat-external", "--plan", str(world["pp"]), "--schedule", str(sp), "--tool", tool, "--results-dir", str(world["dir"] / "results"), *extra])


def test_merged_object_legs_equal_one_unchunked_run_row_for_row(cc, sc, world):
    s, sp = sched(sc, world)
    out = stage_legs(world, s, "jorbit", jorbit_like)
    assert concat_ext(cc, world, sp, "jorbit") == 0
    merged = json.loads((out / "validation_jorbit.json").read_text())
    assert merged == jorbit_like(OBJECTS), "the merge writes exactly what one unchunked run writes"
    assert len(s["tools"]["jorbit"]["legs"]) == 3
    assert not (out / "validation_jorbit.json.absent").exists()


def test_one_leg_is_copied_in_its_own_order(cc, sc, world):
    s, sp = sched(sc, world, tools=("assist",))
    rows = list(reversed([dict(r, channel="assist") for r in plan()]))
    out = stage_legs(world, s, "assist", lambda objs: rows)
    assert concat_ext(cc, world, sp, "assist") == 0
    assert json.loads((out / "validation_assist.json").read_text()) == rows


@pytest.mark.parametrize(
    "breakage, reason",
    [
        ("missing", "did not deliver validation_jorbit"),
        ("foreign", "which leg 02 was not given"),
        ("twice", "is delivered by legs"),
        ("malformed", "is not readable JSON"),
    ],
)
def test_a_tool_whose_legs_cannot_merge_is_skipped_and_named_and_the_run_goes_on(cc, sc, world, breakage, reason, capsys):
    s, sp = sched(sc, world)
    out = stage_legs(world, s, "jorbit", jorbit_like)
    f2 = out / "validation_jorbit.leg02.json"
    if breakage == "missing":
        f2.unlink()
    elif breakage == "foreign":
        f2.write_text(json.dumps(json.loads(f2.read_text()) + jorbit_like(["A"])[:1]))
    elif breakage == "twice":
        a_row = jorbit_like(["A"])[0]
        leg = next(leg for leg in s["tools"]["jorbit"]["legs"] if "A" in leg["objects"])
        f = out / f"validation_jorbit.leg{leg['leg']}.json"
        f.write_text(json.dumps(json.loads(f.read_text()) + [a_row]))
    else:
        f2.write_text('[{"object": "B", "test_type": "propag')
    (out / "validation_jorbit.json").write_text("[]")  # a stale merge must not survive
    assert concat_ext(cc, world, sp, "jorbit") == 0, "comparator isolation: the step itself goes on"
    absent = out / "validation_jorbit.json.absent"
    assert absent.exists() and reason in absent.read_text()
    assert not (out / "validation_jorbit.json").exists()
    assert "NOT MERGED" in capsys.readouterr().out


def test_orbfit_failing_every_object_across_legs_is_skipped_but_one_fit_keeps_it(cc, sc, world):
    s, sp = sched(sc, world, tools=("orbfit",), target=2.0)
    od = lambda objs: [dict(r, channel="orbfit", orbfit_error="DC did not converge") for r in plan() if r["test_type"] == "orbit_determination" and r["object"] in objs]
    out = stage_legs(world, s, "orbfit", od)
    assert concat_ext(cc, world, sp, "orbfit") == 0
    assert "every one of its 5 rows carries orbfit_error" in (out / "validation_orbfit.json.absent").read_text()
    leg = s["tools"]["orbfit"]["legs"][0]
    f = out / f"validation_orbfit.leg{leg['leg']}.json"
    rows = json.loads(f.read_text())
    rows[0]["orbfit_error"] = None
    f.write_text(json.dumps(rows))
    assert concat_ext(cc, world, sp, "orbfit") == 0
    assert len(json.loads((out / "validation_orbfit.json").read_text())) == 5
    assert not (out / "validation_orbfit.json.absent").exists()


def test_a_tool_no_leg_delivered_is_simply_absent(cc, sc, world, capsys):
    _, sp = sched(sc, world)
    (world["dir"] / "results").mkdir()
    assert concat_ext(cc, world, sp, "findorb") == 0
    assert not list((world["dir"] / "results").iterdir())
    assert "no leg delivered it" in capsys.readouterr().out


def test_a_refused_decoupled_row_is_discarded_from_an_external_too(cc, sc, world):
    s, sp = sched(sc, world, tools=("assist",))
    rows = [dict(r, channel="assist") for r in plan()]
    out = stage_legs(world, s, "assist", lambda objs: rows)
    heavy = plan().index(row("B", "propagation", "none_detection_on", 20.0))
    chs = world["dir"] / "schedule.json"
    chs.write_text(json.dumps({"decoupled": [heavy]}))
    drop = world["dir"] / "dropped.json"
    drop.write_text(json.dumps([dict(row("B", "propagation", "none_detection_on", 20.0), reason="refused")]))
    assert concat_ext(cc, world, sp, "assist", "--dropped", str(drop), "--channel-schedule", str(chs)) == 0
    assert len(json.loads((out / "validation_assist.json").read_text())) == len(rows) - 1
    drop.write_text(json.dumps([dict(row("C", "propagation", "none_detection_on", 20.0), reason="refused")]))
    with pytest.raises(SystemExit, match="not a decoupled row"):
        concat_ext(cc, world, sp, "assist", "--dropped", str(drop), "--channel-schedule", str(chs))


# ── the runner flags the object legs use ────────────────────────────────────


def test_jorbit_refuses_an_only_naming_an_object_outside_the_plan(world):
    r = subprocess.run(
        [sys.executable, str(ROOT / "runners/jorbit/run_jorbit.py"), "--input", str(world["pp"]), "--only", "A,Nope", "--output", str(world["dir"] / "x.json")],
        capture_output=True,
        text=True,
        check=False,
    )
    assert r.returncode == 2 and "['Nope']" in r.stderr
    r = subprocess.run(
        [sys.executable, str(ROOT / "runners/jorbit/run_jorbit.py"), "--input", str(world["pp"]), "--only", " , ", "--output", str(world["dir"] / "x.json")],
        capture_output=True,
        text=True,
        check=False,
    )
    assert r.returncode == 2 and "no object" in r.stderr


@pytest.fixture
def orbfit():
    return load("run_orbfit_for_legs", ROOT / "runners/orbfit/run_orbfit.py")


def run_orbfit(orbfit, monkeypatch, world, *flags, fail=True):
    def fake(row, rows_by_object, psv_dir, **kw):
        d = dict(row, channel="orbfit", orbfit_time_ms=1.0)
        if fail:
            d["orbfit_error"] = "DC did not converge"
        else:
            d.update(orbfit_rms_arcsec=0.5, orbfit_n_obs_used=10, orbfit_n_obs_rejected=0, orbfit_is_nongrav=False)
        return d

    class FakeSun:  # the runner seeds from the registered DE440 Sun; no SPK needed here
        def __init__(self, path):
            self.path = path

        def sha256(self):
            return "0" * 64

    monkeypatch.setattr(orbfit, "process_od_row", fake)
    monkeypatch.setattr(orbfit, "orbfit_source_version", lambda image: "orbfit test")
    monkeypatch.setattr(orbfit, "SunEphemeris", FakeSun)
    out = world["dir"] / "orbfit.json"
    monkeypatch.setattr(sys, "argv", ["run_orbfit.py", "--plan", str(world["pp"]), "--output", str(out), "--psv-dir", str(world["dir"]),
                                      "--de440", str(world["dir"] / "de440.bsp"), *flags])
    return orbfit.main(), out


def test_an_orbfit_leg_defers_the_every_object_failed_verdict_to_the_merge(orbfit, monkeypatch, world):
    rc, out = run_orbfit(orbfit, monkeypatch, world, "--only", "A,B")
    assert rc == 1, "an unchunked run whose every fit failed still exits nonzero"
    rc, out = run_orbfit(orbfit, monkeypatch, world, "--only", "A,B", "--defer-all-failed")
    assert rc == 0 and [r["object"] for r in json.loads(out.read_text())] == ["A", "B"]
    rc, _ = run_orbfit(orbfit, monkeypatch, world, "--only", "A,Nope", "--defer-all-failed")
    assert rc == 2
