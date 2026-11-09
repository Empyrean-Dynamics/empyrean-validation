"""channel-chunks.py: row-level select of a leg, strict plan-order reassembly."""

import importlib.util
import json
import pathlib

import pytest

SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "channel-chunks.py"


@pytest.fixture(scope="module")
def cc():
    spec = importlib.util.spec_from_file_location("channel_chunks", SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def objs(*names):
    return [{"name": n, "slug": n.lower()} for n in names]


def test_split_deals_round_robin_with_zero_padded_ids(cc):
    chunks = cc.split(objs("A", "B", "C", "D", "E"), 2)
    assert chunks == [{"id": "01", "objects": "A,C,E"}, {"id": "02", "objects": "B,D"}]


def test_split_caps_the_count_at_the_object_count(cc):
    chunks = cc.split(objs("Chiron"), 10)
    assert chunks == [{"id": "01", "objects": "Chiron"}]


def test_split_refuses_an_empty_matrix_and_a_comma_in_a_name(cc):
    with pytest.raises(SystemExit):
        cc.split([], 3)
    with pytest.raises(SystemExit):
        cc.split(objs("a,b"), 1)


def row(obj, tt, method=None, dt=0.0, observer=None):
    return {
        "object": obj,
        "test_type": tt,
        "propagation_uncertainty": method,
        "dt_days": dt,
        "observer": observer,
        "force_model": "standard",
    }


def small_plan():
    plan = []
    for o in ("Bennu", "1I/'Oumuamua"):
        for dt in (10.0, 20.0, 30.0):
            plan.append(row(o, "propagation", "first_order_detection_on", dt))
        plan.append(row(o, "orbit_determination"))
        plan.append(row(o, "non_grav_recovery"))
    return plan


@pytest.fixture
def staged(cc, tmp_path):
    """A plan on disk, a hand-written two-leg python schedule over it whose legs
    interleave plan rows (so leg order is not plan order), and the legs' outputs."""
    plan = small_plan()
    plan_path = tmp_path / "validation_plan.json"
    plan_path.write_text(json.dumps(plan))
    leg1, leg2 = [0, 2, 3, 4, 6, 8, 9], [1, 5, 7]
    # The legacy fit and its NG row share a leg: rows 3/4 and 8/9 are both in leg 01.
    schedule = {
        "schema": cc.sched.SCHEMA,
        "plan_sha256": cc.sched.file_sha256(plan_path),
        "channels": {
            "python": {
                "legs": [
                    {"leg": "01", "rows": leg1, "predicted_min": 3.0},
                    {"leg": "02", "rows": leg2, "predicted_min": 1.0},
                ]
            }
        },
    }
    sched_path = tmp_path / "schedule.json"
    sched_path.write_text(json.dumps(schedule))

    def leg_file(leg, rows):
        p = tmp_path / f"validation_python.leg{leg}.json"
        p.write_text(json.dumps([dict(plan[i], channel="python") for i in rows]))
        return p

    files = [leg_file("01", leg1), leg_file("02", leg2)]
    return plan, plan_path, sched_path, files, leg_file, tmp_path


def test_select_writes_the_legs_rows_in_plan_order(cc, staged, capsys):
    plan, plan_path, sched_path, _, _, tmp = staged
    out = tmp / "validation_plan.leg-python.json"
    assert cc.main(["select", "--plan", str(plan_path), "--schedule", str(sched_path), "--channel", "python", "--leg", "02", "--output", str(out)]) == 0
    assert json.loads(out.read_text()) == [plan[1], plan[5], plan[7]]
    assert "Plan leg: 3 rows for python leg 02 of 2 (of 10 rows" in capsys.readouterr().out


def test_select_over_every_leg_reproduces_the_plan_exactly_once(cc, staged):
    plan, plan_path, sched_path, _, _, tmp = staged
    seen = []
    for leg in ("01", "02"):
        out = tmp / f"leg{leg}.json"
        cc.main(["select", "--plan", str(plan_path), "--schedule", str(sched_path), "--channel", "python", "--leg", leg, "--output", str(out)])
        seen += json.loads(out.read_text())
    assert sorted(seen, key=lambda r: plan.index(r)) == plan and len(seen) == len(plan)


def test_select_refuses_a_stale_plan_hash(cc, staged):
    plan, plan_path, sched_path, _, _, tmp = staged
    plan_path.write_text(json.dumps(plan + [row("Eris", "propagation", "first_order_detection_on", 1.0)]))
    out = tmp / "never.json"
    with pytest.raises(SystemExit, match="different plan"):
        cc.main(["select", "--plan", str(plan_path), "--schedule", str(sched_path), "--channel", "python", "--leg", "01", "--output", str(out)])
    assert not out.exists()


def test_select_refuses_an_unknown_leg_an_unknown_channel_and_an_empty_leg(cc, staged):
    plan, plan_path, sched_path, _, _, tmp = staged
    args = ["--plan", str(plan_path), "--schedule", str(sched_path), "--output", str(tmp / "o.json")]
    with pytest.raises(SystemExit, match="no leg '09'"):
        cc.main(["select", *args, "--channel", "python", "--leg", "09"])
    with pytest.raises(SystemExit, match="no channel 'cli'"):
        cc.main(["select", *args, "--channel", "cli", "--leg", "01"])
    sched = json.loads(sched_path.read_text())
    sched["channels"]["python"]["legs"][1]["rows"] = []
    sched_path.write_text(json.dumps(sched))
    with pytest.raises(SystemExit, match="selects no rows"):
        cc.main(["select", *args, "--channel", "python", "--leg", "02"])


def run_concat(cc, staged, files=None):
    plan, plan_path, sched_path, default_files, _, tmp = staged
    out = tmp / "validation_python.json"
    argv = ["concat", "--plan", str(plan_path), "--schedule", str(sched_path), "--channel", "python", "--out", str(out)]
    cc.main(argv + [str(f) for f in (files if files is not None else default_files)])
    return json.loads(out.read_text())


def test_concat_restores_plan_order_from_shuffled_legs(cc, staged):
    plan, *_ = staged
    rows = run_concat(cc, staged, list(reversed(staged[3])))
    assert [(r["object"], r["test_type"], r["dt_days"]) for r in rows] == [(r["object"], r["test_type"], r["dt_days"]) for r in plan]


def test_concat_accepts_rows_a_channel_did_not_produce(cc, staged):
    plan, _, _, _, leg_file, _ = staged
    leg1 = leg_file("01", [0, 2, 3, 4, 6, 8])  # the second NG row was skipped by the channel
    rows = run_concat(cc, staged, [leg1, staged[3][1]])
    assert len(rows) == len(plan) - 1


def test_concat_refuses_a_duplicate_row(cc, staged):
    plan, _, _, _, leg_file, _ = staged
    dup = leg_file("01", [0, 2, 3, 4, 6, 8, 9, 9])
    with pytest.raises(SystemExit, match="delivered twice"):
        run_concat(cc, staged, [dup, staged[3][1]])


def test_concat_refuses_a_missing_leg_and_an_unexpected_one(cc, staged):
    with pytest.raises(SystemExit, match=r"missing leg\(s\) 02 of 2"):
        run_concat(cc, staged, [staged[3][0]])
    stray = staged[5] / "validation_python.leg03.json"
    stray.write_text("[]")
    with pytest.raises(SystemExit, match=r"leg file\(s\) 03"):
        run_concat(cc, staged, [*staged[3], stray])


def test_concat_refuses_a_row_outside_its_legs_selection(cc, staged):
    plan, _, _, _, leg_file, _ = staged
    wrong = leg_file("02", [1, 5, 7, 0])  # plan row 0 belongs to leg 01 and is not duplicated
    right = leg_file("01", [2, 3, 4, 6, 8, 9])
    with pytest.raises(SystemExit, match="gave to another leg"):
        run_concat(cc, staged, [right, wrong])


def test_concat_refuses_a_row_that_is_not_in_the_plan(cc, staged):
    _, _, _, _, leg_file, tmp = staged
    leg2 = tmp / "validation_python.leg02.json"
    rows = json.loads(leg2.read_text()) + [row("Eris", "propagation", "first_order_detection_on", 5.0)]
    leg2.write_text(json.dumps(rows))
    with pytest.raises(SystemExit, match="not in the plan"):
        run_concat(cc, staged)


def test_concat_refuses_a_non_list_a_misnamed_file_and_a_repeated_leg_id(cc, staged):
    _, _, _, files, _, tmp = staged
    bad = tmp / "validation_python.leg02.json"
    bad.write_text(json.dumps({"rows": []}))
    with pytest.raises(SystemExit, match="not a JSON list"):
        run_concat(cc, staged)
    (tmp / "validation_python.chunk02.json").write_text("[]")
    with pytest.raises(SystemExit, match="not a leg file"):
        run_concat(cc, staged, [files[0], tmp / "validation_python.chunk02.json"])
    twin = tmp / "other"
    twin.mkdir()
    (twin / "validation_python.leg01.json").write_text("[]")
    with pytest.raises(SystemExit, match="duplicate leg id 01"):
        run_concat(cc, staged, [files[0], twin / "validation_python.leg01.json", files[1]])


def test_concat_refuses_a_plan_with_a_repeated_row_key(cc, staged):
    plan, plan_path, sched_path, _, _, _ = staged
    plan_path.write_text(json.dumps(plan + [plan[0]]))
    sched = json.loads(sched_path.read_text())
    sched["plan_sha256"] = cc.sched.file_sha256(plan_path)
    sched_path.write_text(json.dumps(sched))
    with pytest.raises(SystemExit, match="share the row key"):
        run_concat(cc, staged)


def test_concat_writes_nothing_on_a_refusal(cc, staged):
    out = staged[5] / "validation_python.json"
    with pytest.raises(SystemExit):
        run_concat(cc, staged, [staged[3][0]])
    assert not out.exists() and not (staged[5] / "validation_python.json.tmp").exists()


def test_a_real_schedule_round_trips_select_then_concat_to_the_plan(cc, tmp_path):
    """schedule.py pack -> select every leg -> a channel that echoes its input
    (plus the rows' own output field) -> concat: the plan again, in plan order."""
    plan = small_plan() * 1
    for o in ("Eris", "Vesta"):
        for dt in (1.0, 2.0, 3.0, 4.0):
            plan.append(row(o, "propagation", "first_order_detection_on", dt))
            plan.append(row(o, "ephemeris", "first_order_detection_on", dt, "500"))
        plan.append(row(o, "orbit_determination"))
        plan.append(row(o, "non_grav_recovery"))
    plan_path = tmp_path / "validation_plan.json"
    plan_path.write_text(json.dumps(plan))
    run_dir = tmp_path / "run"
    run_dir.mkdir()
    (run_dir / "validation_plan.json").write_text(json.dumps(plan))
    for ch in cc.sched.CHANNELS:
        name = "validation_rust.json" if ch == "rust" else f"validation_{ch}.json"
        (run_dir / name).write_text(json.dumps([dict(r, emp_time_ms=1500.0) for r in plan]))
    costs = tmp_path / "costs.json"
    costs.write_text(json.dumps(cc.sched.refresh_costs(run_dir, "synthetic", "pin")))
    for channel, n_legs in (("python", 3), ("c", 3), ("cli", 4), ("core", 5)):
        sched_path = tmp_path / f"schedule-{channel}.json"
        assert cc.sched.main(["pack", "--plan", str(plan_path), "--costs", str(costs), "--channels", channel, "--budget", str(n_legs), "--out", str(sched_path)]) == 0
        legs = json.loads(sched_path.read_text())["channels"][channel]["legs"]
        files = []
        for leg in legs:
            sel = tmp_path / f"sel-{channel}-{leg['leg']}.json"
            cc.main(["select", "--plan", str(plan_path), "--schedule", str(sched_path), "--channel", channel, "--leg", leg["leg"], "--output", str(sel)])
            f = tmp_path / f"validation_{channel}.leg{leg['leg']}.json"
            f.write_text(json.dumps([dict(r, channel=channel) for r in json.loads(sel.read_text())]))
            files.append(f)
        out = tmp_path / f"validation_{channel}.json"
        cc.main(["concat", "--plan", str(plan_path), "--schedule", str(sched_path), "--channel", channel, "--out", str(out), *map(str, reversed(files))])
        assert json.loads(out.read_text()) == [dict(r, channel=channel) for r in plan], channel
