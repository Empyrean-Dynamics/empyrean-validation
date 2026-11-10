"""schedule.py: row-level units, cost-table pricing, longest-first packing, validation."""

import importlib.util
import json
import os
import pathlib
import subprocess
import sys

import pytest

SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "schedule.py"


@pytest.fixture(scope="module")
def sc():
    spec = importlib.util.spec_from_file_location("schedule_under_test", SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def row(obj, tt, method=None, dt=0.0, observer=None):
    return {
        "object": obj,
        "test_type": tt,
        "propagation_uncertainty": method,
        "dt_days": dt,
        "observer": observer,
        "force_model": "standard",
    }


def object_rows(obj, props=(10.0, 20.0, 30.0), ng=False):
    """One object's plan rows in the order the plan builder emits them."""
    rows = [row(obj, "propagation", "first_order_detection_on", dt) for dt in props]
    rows += [row(obj, "ephemeris", "first_order_detection_on", dt, "500") for dt in props]
    rows.append(row(obj, "orbit_determination"))
    rows.append(row(obj, "orbit_determination", "auto_detection_on"))
    rows.append(row(obj, "orbit_determination_radar"))
    rows.append(row(obj, "orbit_determination_transport", "first_order_detection_on"))
    if ng:
        rows.append(row(obj, "non_grav_recovery"))
    return rows


def make_plan(*objs, ng=()):
    plan = []
    for o in objs:
        plan += object_rows(o, ng=o in ng)
    return plan


def make_table(sc, plan, minutes=None, default=0.001):
    """A cost table: every plan row known, `minutes` (key -> min) priced explicitly."""
    minutes = minutes or {}
    chans = {}
    for ch in sc.CHANNELS:
        by_class = {}
        for r in plan:
            by_class.setdefault(sc.class_of(r), []).append(minutes.get(sc.key_str(sc.row_key(r)), default))
        chans[ch] = {
            "rows": {k: v for k, v in minutes.items() if v * 60.0 >= sc.EXPLICIT_MIN_S},
            "class_median": {c: default for c in by_class},
            "class_max": {c: max(v) for c, v in by_class.items()},
        }
    known = {}
    for r in plan:
        known.setdefault(sc.group_of(r), set()).add(float(r["dt_days"]) if r["test_type"] in sc.PROP_EPH else 0.0)
    data = {
        "schema": sc.SCHEMA,
        "run_id": "synthetic",
        "channels": chans,
        "known_rows": {g: sorted(v) for g, v in known.items()},
    }
    return sc.CostTable(data, "synthetic-sha")


def keyof(sc, r):
    return sc.key_str(sc.row_key(r))


def sched_for(sc, plan, table, channel, n_legs):
    legs, estimated, _, _, _ = sc.pack(channel, plan, table, n_legs)
    return {
        "schema": sc.SCHEMA,
        "plan_sha256": "p",
        "cost_table_sha256": table.sha256,
        "channels": {channel: {"legs": legs}},
    }, estimated


# ── identity and units ───────────────────────────────────────────────────────


def test_row_key_is_unique_across_a_plan_and_ignores_dt_on_od_rows(sc):
    plan = make_plan("A", "B", ng=("A",))
    keys = [keyof(sc, r) for r in plan]
    assert len(set(keys)) == len(plan)
    r = row("A", "orbit_determination")
    r["dt_days"] = 99.0  # an OD row's dt is not part of its identity
    assert keyof(sc, r) == keyof(sc, row("A", "orbit_determination"))


def test_every_unit_lands_in_exactly_one_leg(sc):
    plan = make_plan("A", "B", "C", ng=("A", "C"))
    table = make_table(sc, plan)
    for ch in ("python", "c", "cli", "core", "rust"):
        legs, _, _, _, _ = sc.pack(ch, plan, table, 4)
        rows = [i for leg in legs for i in leg["rows"]]
        assert sorted(rows) == list(range(len(plan))), ch
        assert sum(leg["units"] for leg in legs) == len(sc.units(ch, plan)), ch


def test_legacy_fit_and_ng_row_never_split_for_python_c_cli(sc):
    plan = make_plan("A", "B", ng=("A", "B"))
    table = make_table(sc, plan)
    for ch in ("python", "c", "cli"):
        for n_legs in range(1, 9):
            legs, _, _, _, _ = sc.pack(ch, plan, table, n_legs)
            where = {i: leg["leg"] for leg in legs for i in leg["rows"]}
            for obj in ("A", "B"):
                legacy = next(i for i, r in enumerate(plan) if r["object"] == obj and r["test_type"] == "orbit_determination" and r["propagation_uncertainty"] is None)
                ng = next(i for i, r in enumerate(plan) if r["object"] == obj and r["test_type"] == "non_grav_recovery")
                assert where[legacy] == where[ng], (ch, n_legs, obj)


def test_unit_shapes_per_channel(sc):
    plan = make_plan("A", ng=("A",))
    n_od = sum(1 for r in plan if r["test_type"] not in sc.PROP_EPH)
    assert n_od == 5
    assert len(sc.units("core", plan)) == len(plan)
    # rust: the whole OD family of an object is one unit
    rust = sc.units("rust", plan)
    assert len(rust) == 6 + 1
    # python: legacy+ng share a unit, the other three OD rows are their own
    py = sc.units("python", plan)
    assert len(py) == 6 + 1 + 3
    with pytest.raises(SystemExit):
        sc.units("nope", plan)


def test_repetitions_follow_the_channel_rules(sc):
    p_fo = row("A", "propagation", "first_order_detection_on")
    p_mc = row("A", "propagation", "monte_carlo_detection_on")
    e_mc = row("A", "ephemeris", "monte_carlo_detection_on")
    assert [sc.reps(c, p_fo) for c in ("rust", "python", "c", "cli", "core")] == [3, 3, 3, 3, 3]
    assert [sc.reps(c, p_mc) for c in ("rust", "python", "c", "cli", "core")] == [1, 1, 3, 3, 1]
    assert [sc.reps(c, e_mc) for c in ("rust", "python", "c", "cli", "core")] == [1, 3, 3, 3, 1]
    assert sc.reps("python", row("A", "orbit_determination")) == 1
    assert sc.zero_cost("rust", row("A", "orbit_determination", "none_detection_on"))
    assert sc.zero_cost("c", row("A", "orbit_determination_transport", "none_detection_on"))
    assert not sc.zero_cost("core", row("A", "orbit_determination_transport", "none_detection_on"))


# ── determinism ──────────────────────────────────────────────────────────────


def _pack_cli(tmp_path, seed):
    plan = make_plan("A", "B", "C", "D", ng=("B",))  # equal costs everywhere: all ties
    pp = tmp_path / "plan.json"
    pp.write_text(json.dumps(plan))
    table = make_table(sc_module(), plan)
    cp = tmp_path / "costs.json"
    cp.write_text(json.dumps(table.data))
    out = tmp_path / f"sched-{seed}.json"
    env = dict(os.environ, PYTHONHASHSEED=str(seed))
    subprocess.run(
        [sys.executable, str(SCRIPT), "pack", "--plan", str(pp), "--costs", str(cp), "--channels", "python,c,cli,core", "--budget", "12", "--out", str(out)],
        check=True,
        env=env,
        capture_output=True,
    )
    return out.read_bytes()


_SC = []


def sc_module():
    if not _SC:
        spec = importlib.util.spec_from_file_location("schedule_for_helpers", SCRIPT)
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        _SC.append(mod)
    return _SC[0]


def test_schedule_bytes_identical_across_runs_and_hash_seeds(tmp_path):
    first = _pack_cli(tmp_path, 1)
    assert first == _pack_cli(tmp_path, 2)
    assert first == _pack_cli(tmp_path, 3)


def test_leg_counts_spend_the_budget_and_are_deterministic(sc):
    totals = {"python": (500.0, 125.0), "c": (900.0, 140.0), "cli": (1000.0, 146.0), "core": (600.0, 125.0)}
    a = sc.leg_counts(totals, 50)
    assert a == sc.leg_counts(dict(reversed(list(totals.items()))), 50)
    assert sum(a.values()) == 50
    assert a["cli"] > a["python"]
    with pytest.raises(SystemExit):
        sc.leg_counts(totals, 3)


# ── the warm-up rule ─────────────────────────────────────────────────────────


def warmup_plan():
    # Two light propagation rows lead the plan, then two heavy ones, then OD.
    plan = [
        row("A", "propagation", "first_order_detection_on", 1.0),
        row("A", "propagation", "first_order_detection_on", 2.0),
        row("A", "propagation", "first_order_detection_on", 3.0),
        row("A", "propagation", "first_order_detection_on", 4.0),
        row("A", "orbit_determination"),
    ]
    minutes = {keyof(sc_module(), plan[2]): 100.0, keyof(sc_module(), plan[3]): 90.0}
    return plan, minutes


def test_each_c_and_cli_leg_opens_with_a_light_propagation_row(sc):
    plan, minutes = warmup_plan()
    table = make_table(sc, plan, minutes)
    for ch in ("c", "cli"):
        schedule, _ = sched_for(sc, plan, table, ch, 2)
        schedule["plan_sha256"] = "p"
        assert sc.validate(plan, "p", schedule, table) == []
        for leg in schedule["channels"][ch]["legs"]:
            first = next(i for i in leg["rows"] if plan[i]["test_type"] == "propagation")
            assert table.price(ch, plan[first])[0] * 60.0 < sc.LEADER_MAX_S


def test_validator_names_a_leg_that_opens_on_a_heavy_row(sc):
    plan, minutes = warmup_plan()
    table = make_table(sc, plan, minutes)
    schedule = {
        "schema": sc.SCHEMA,
        "plan_sha256": "p",
        "cost_table_sha256": table.sha256,
        "channels": {
            "c": {
                "legs": [
                    {"leg": "01", "rows": [2, 4]},  # heavy propagation row first
                    {"leg": "02", "rows": [0, 1, 3]},
                ]
            }
        },
    }
    bad = sc.validate(plan, "p", schedule, table)
    assert any("opens on heavy propagation" in b and "leg 01" in b for b in bad), bad


def test_a_heavy_row_with_no_light_leader_before_it_is_refused_by_name(sc):
    plan = [row("A", "propagation", "first_order_detection_on", 1.0), row("A", "propagation", "first_order_detection_on", 2.0)]
    table = make_table(sc, plan, {keyof(sc, plan[0]): 100.0})
    with pytest.raises(SystemExit) as e:
        sc.pack("c", plan, table, 1)
    assert "heavy propagation row" in str(e.value)


# ── the cost table ───────────────────────────────────────────────────────────


def test_a_row_missing_from_the_table_is_priced_and_listed(sc):
    plan = make_plan("A")
    table = make_table(sc, plan, default=0.5)
    plan2 = plan + make_plan("NEW")  # an object the table has never seen
    legs, estimated, _, _, _ = sc.pack("core", plan2, table, 2)
    assert len(estimated) == len(make_plan("NEW"))
    assert all(keyof(sc, r) in " ".join(estimated) for r in make_plan("NEW")[:3])
    new_rows = [i for i, r in enumerate(plan2) if r["object"] == "NEW"]
    priced = sum(leg["predicted_min"] for leg in legs)
    assert priced > 0.5 * len(plan2) * 0.9  # the new rows cost something, never 0
    assert all(table.price("core", plan2[i])[0] > 0 for i in new_rows)


def test_an_od_row_missing_from_the_table_is_priced_at_the_class_maximum(sc):
    plan = make_plan("A")
    od = next(r for r in plan if r["test_type"] == "orbit_determination" and r["propagation_uncertainty"] is None)
    table = make_table(sc, plan, {keyof(sc, od): 9.0}, default=0.01)
    new_od = row("NEW", "orbit_determination")
    v, why = table.price("core", new_od)
    assert v == pytest.approx(9.0) and "maximum" in why
    new_prop = row("NEW", "propagation", "first_order_detection_on", 10.0)
    v, why = table.price("core", new_prop)
    assert v > 0 and "median" in why


def test_an_unknown_class_is_still_not_free(sc):
    plan = make_plan("A")
    table = make_table(sc, plan, default=0.25)
    v, why = table.price("core", row("A", "never_seen_type"))
    assert v == pytest.approx(0.25) and "class not in the cost table" in why


def test_rows_the_table_knows_are_not_estimates(sc):
    plan = make_plan("A")
    table = make_table(sc, plan, default=0.001)
    _, estimated, _, _, _ = sc.pack("python", plan, table, 2)
    assert estimated == []


def test_cli_rows_carry_the_measured_per_row_overhead(sc):
    plan = make_plan("A")
    table = make_table(sc, plan, default=0.0)
    r = plan[0]
    assert table.price("cli", r)[0] == pytest.approx(0.26 / 60)
    assert table.price("c", r)[0] == 0.0


def test_refresh_costs_prices_heavy_rows_by_name_and_light_rows_by_class(sc, tmp_path):
    plan = make_plan("A")
    for r in plan:
        r["emp_time_ms"] = None
    (tmp_path / "validation_plan.json").write_text(json.dumps(plan))
    heavy = plan[0]
    for ch in sc.CHANNELS:
        rows = [dict(r, emp_time_ms=10.0) for r in plan]
        rows[0]["emp_time_ms"] = 120000.0
        name = "validation_rust.json" if ch == "rust" else f"validation_{ch}.json"
        (tmp_path / name).write_text(json.dumps(rows))
    table = sc.refresh_costs(tmp_path, "run-1", "pin-1")
    assert table["run_id"] == "run-1" and table["engine_pin"] == "pin-1"
    rows = table["channels"]["core"]["rows"]
    assert list(rows) == [keyof(sc, heavy)]
    assert rows[keyof(sc, heavy)] == pytest.approx(120000.0 * 3 / 60000.0)  # best of 3, in minutes
    assert table["channels"]["core"]["class_median"]["propagation|first_order_detection_on"] > 0
    # every plan row is a known row, so a light row is not reported as missing
    t = sc.CostTable(json.loads(json.dumps(table)), "x")
    assert all(t.is_known(r) for r in plan)


def test_a_cost_table_of_another_schema_is_refused(sc):
    with pytest.raises(SystemExit):
        sc.CostTable({"schema": 999, "known_rows": {}, "channels": {}}, "x")


# ── the validator ────────────────────────────────────────────────────────────


def good(sc):
    plan = make_plan("A", "B", ng=("A",))
    table = make_table(sc, plan)
    schedule, _ = sched_for(sc, plan, table, "python", 3)
    return plan, table, schedule


def test_validator_accepts_a_packed_schedule(sc):
    plan, table, schedule = good(sc)
    assert sc.validate(plan, "p", schedule, table) == []


def test_validator_names_a_dropped_row_a_repeated_row_and_a_split_unit(sc):
    plan, table, schedule = good(sc)
    legs = schedule["channels"]["python"]["legs"]
    dropped = legs[0]["rows"].pop()
    assert any(f"{1} plan rows are in no leg" in b for b in sc.validate(plan, "p", schedule, table))
    legs[0]["rows"].append(dropped)
    legs[1]["rows"].append(legs[0]["rows"][0])
    assert any("is in legs" in b for b in sc.validate(plan, "p", schedule, table))
    legs[1]["rows"].pop()
    # move the NG row away from its legacy fit
    ng = next(i for i, r in enumerate(plan) if r["test_type"] == "non_grav_recovery")
    src = next(leg for leg in legs if ng in leg["rows"])
    dst = next(leg for leg in legs if leg is not src)
    src["rows"].remove(ng)
    dst["rows"].append(ng)
    assert any("is split across legs" in b for b in sc.validate(plan, "p", schedule, table))


def test_validator_refuses_a_stale_plan_and_a_stale_cost_table(sc):
    plan, table, schedule = good(sc)
    assert any("different plan" in b for b in sc.validate(plan, "other", schedule, table))
    table2 = make_table(sc, plan)
    table2.sha256 = "changed"
    assert any("different cost table" in b for b in sc.validate(plan, "p", schedule, table2))


def test_leg_matrix_lists_every_leg_once(sc):
    plan = make_plan("A", "B")
    table = make_table(sc, plan)
    schedule = {"channels": {}}
    for ch, n in (("python", 2), ("core", 3)):
        legs, _, _, _, _ = sc.pack(ch, plan, table, n)
        schedule["channels"][ch] = {"legs": legs}
    assert sc.leg_matrix(schedule) == [
        {"channel": "python", "leg": "01"},
        {"channel": "python", "leg": "02"},
        {"channel": "core", "leg": "01"},
        {"channel": "core", "leg": "02"},
        {"channel": "core", "leg": "03"},
    ]


# ── heavy rows: the timing repetitions fan out (S4) ──────────────────────────


def heavy_plan():
    """Three objects; C's propagation at dt 20 is the heavy row (15 min a run,
    three timed runs in every channel that times it three times)."""
    plan = make_plan("A", "B", "C", ng=("A",))
    # Late in the plan, as 2020 CD3's row is: c and cli need three light
    # propagation rows ahead of it to lead the three replica legs.
    heavy = next(i for i, r in enumerate(plan) if r["object"] == "C" and r["test_type"] == "propagation" and r["dt_days"] == 20.0)
    return plan, heavy


def heavy_table(sc, plan, heavy, mark=True, minutes=45.0):
    table = make_table(sc, plan, {keyof(sc, plan[heavy]): minutes})
    if mark:
        table.heavy_rows = {keyof(sc, plan[heavy])}
    return table


def test_a_heavy_row_runs_once_in_each_of_its_replica_legs_priced_at_one_run(sc):
    plan, heavy = heavy_plan()
    table = heavy_table(sc, plan, heavy)
    for ch in ("python", "c", "cli", "core", "rust"):
        legs, _, _, _, replicas = sc.pack(ch, plan, table, 5)
        holding = [leg for leg in legs if heavy in leg["rows"]]
        assert len(holding) == 3, ch  # the channel times it three times
        assert len({leg["leg"] for leg in holding}) == 3, ch
        for leg in holding:
            assert leg["reps"] == {str(heavy): 1}, ch
            assert leg["predicted_min"] >= 15.0 - 1e-6, ch  # 45 min over three runs
        assert [leg for leg in legs if heavy not in leg["rows"] and "reps" in leg] == [], ch
        assert replicas == [
            {"channel": ch, "group": keyof(sc, plan[heavy]), "row": heavy, "legs": [x["leg"] for x in holding], "reps": 1}
        ], ch
        # every other row is still in exactly one leg
        others = [i for leg in legs for i in leg["rows"] if i != heavy]
        assert sorted(others) == [i for i in range(len(plan)) if i != heavy], ch


def test_a_row_the_table_does_not_mark_does_not_fan_out(sc):
    plan, heavy = heavy_plan()
    table = heavy_table(sc, plan, heavy, mark=False)
    legs, _, _, _, replicas = sc.pack("python", plan, table, 5)
    assert replicas == [] and all("reps" not in leg for leg in legs)
    assert sum(heavy in leg["rows"] for leg in legs) == 1


def test_heavy_min_s_fans_out_a_cheap_row_for_a_rehearsal(sc):
    plan, heavy = heavy_plan()
    table = heavy_table(sc, plan, heavy, mark=False, minutes=0.3)  # 6 s a run
    _, _, _, _, none = sc.pack("python", plan, table, 5, heavy_min_s=None)
    assert none == []
    _, _, _, _, some = sc.pack("python", plan, table, 5, heavy_min_s=5.0)
    assert [r["row"] for r in some] == [heavy]
    _, _, _, _, high = sc.pack("python", plan, table, 5, heavy_min_s=60.0)
    assert high == []


def test_a_row_the_channel_times_once_has_no_repetitions_to_fan_out(sc):
    plan, _ = heavy_plan()
    eph = next(i for i, r in enumerate(plan) if r["object"] == "C" and r["test_type"] == "ephemeris" and r["dt_days"] == 20.0)
    table = heavy_table(sc, plan, eph, minutes=45.0)  # core times an ephemeris row once
    _, _, _, _, core = sc.pack("core", plan, table, 5)
    assert core == []
    _, _, _, _, py = sc.pack("python", plan, table, 5)  # python times it three times
    assert [r["row"] for r in py] == [eph]


def test_replica_legs_of_c_and_cli_each_open_on_a_light_propagation_row(sc):
    plan, heavy = heavy_plan()
    table = heavy_table(sc, plan, heavy)
    for ch in ("c", "cli"):
        legs, _, _, _, _ = sc.pack(ch, plan, table, 5)
        for leg in legs:
            if heavy in leg["rows"]:
                first = next(i for i in leg["rows"] if plan[i]["test_type"] == "propagation")
                assert first < heavy and first != heavy, ch
        sched = {"schema": sc.SCHEMA, "plan_sha256": "p", "cost_table_sha256": table.sha256, "channels": {ch: {"legs": legs}}, "replicas": sc.pack(ch, plan, table, 5)[4]}
        assert sc.validate(plan, "p", sched, table) == [], ch


def test_too_few_legs_for_the_fan_out_is_refused_by_name(sc):
    plan, heavy = heavy_plan()
    table = heavy_table(sc, plan, heavy)
    with pytest.raises(SystemExit, match="fans out to 3 distinct legs"):
        sc.pack("python", plan, table, 2)


def replica_schedule(sc, ch="python"):
    plan, heavy = heavy_plan()
    table = heavy_table(sc, plan, heavy)
    legs, _, _, _, replicas = sc.pack(ch, plan, table, 5)
    sched = {"schema": sc.SCHEMA, "plan_sha256": "p", "cost_table_sha256": table.sha256, "channels": {ch: {"legs": legs}}, "replicas": replicas}
    return plan, heavy, table, sched


def test_validator_accepts_a_replicated_schedule(sc):
    plan, _, table, sched = replica_schedule(sc)
    assert sc.validate(plan, "p", sched, table) == []


def test_validator_refuses_replicas_that_share_a_leg(sc):
    plan, heavy, table, sched = replica_schedule(sc)
    sched["replicas"][0]["legs"] = [sched["replicas"][0]["legs"][0]] * 3
    assert any("share a leg" in b for b in sc.validate(plan, "p", sched, table))


def test_validator_refuses_a_replica_row_missing_from_a_declared_leg_and_a_fan_out_count_that_is_wrong(sc):
    plan, heavy, table, sched = replica_schedule(sc)
    holding = [leg for leg in sched["channels"]["python"]["legs"] if heavy in leg["rows"]]
    holding[0]["rows"].remove(heavy)
    holding[0]["reps"].pop(str(heavy))
    bad = sc.validate(plan, "p", sched, table)
    assert any("declared in legs" in b for b in bad)
    holding[0]["rows"].append(heavy)
    holding[0]["rows"].sort()
    holding[0]["reps"][str(heavy)] = 1
    sched["replicas"][0]["legs"] = sched["replicas"][0]["legs"][:2]
    assert any("fans out to 2 legs, but the channel times it 3 times" in b for b in sc.validate(plan, "p", sched, table))


def test_validator_refuses_a_replica_leg_without_its_override_and_an_override_without_a_replica(sc):
    plan, heavy, table, sched = replica_schedule(sc)
    legs = sched["channels"]["python"]["legs"]
    holding = [leg for leg in legs if heavy in leg["rows"]]
    holding[0]["reps"] = {}
    assert any("without its repetition override" in b for b in sc.validate(plan, "p", sched, table))
    holding[0]["reps"] = {str(heavy): 1}
    other = next(leg for leg in legs if heavy not in leg["rows"])
    other["reps"] = {str(other["rows"][0]): 1}
    assert any("not a declared replica of this leg" in b for b in sc.validate(plan, "p", sched, table))


def test_an_unreplicated_row_in_two_legs_is_still_refused(sc):
    plan, heavy, table, sched = replica_schedule(sc)
    legs = sched["channels"]["python"]["legs"]
    other = next(i for leg in legs for i in leg["rows"] if i != heavy and plan[i]["test_type"] == "propagation")
    dst = next(leg for leg in legs if other not in leg["rows"])
    dst["rows"] = sorted(dst["rows"] + [other])
    assert any("is in legs" in b for b in sc.validate(plan, "p", sched, table))


def test_refresh_costs_marks_the_heavy_row_by_name(sc, tmp_path):
    plan, heavy = heavy_plan()
    key = keyof(sc, plan[heavy])
    (tmp_path / "validation_plan.json").write_text(json.dumps(plan))
    emp = {key: 45 * 60_000.0 / 3}
    for ch in ("rust", "python", "c", "cli", "core"):
        rows = [dict(r, emp_time_ms=emp.get(keyof(sc, r), 10.0)) for r in plan]
        (tmp_path / ("validation_rust.json" if ch == "rust" else f"validation_{ch}.json")).write_text(json.dumps(rows))
    table = sc.refresh_costs(tmp_path, "run", "pin")
    assert table["heavy"]["rows"] == [key]
    assert table["heavy"]["min_single_run_s"] == sc.HEAVY_MIN_S
    assert sc.CostTable(table, "x").heavy_rows == {key}


# ── the reference (rust) schedule (S3) ───────────────────────────────────────


def ref_candidates(*objs):
    out = []
    for o in objs:
        for m in ("first_order_detection_on", "auto_detection_on"):
            for dt in (10.0, 20.0, 30.0):
                out.append(row(o, "propagation", m, dt))
                out.append(row(o, "ephemeris", m, dt, "500"))
    return out


def ref_world(sc, tmp_path, objs=("A", "B", "C"), minutes=None):
    cands = ref_candidates(*objs)
    plan = make_plan(*objs)  # the plan the table came from: carries the OD rows
    table_data = make_table(sc, plan + cands, minutes or {}).data
    costs = tmp_path / "costs.json"
    costs.write_text(json.dumps(table_data))
    cp = tmp_path / "cands.json"
    cp.write_text(json.dumps(cands))
    op = tmp_path / "objects.json"
    op.write_text(json.dumps([{"name": o, "slug": o} for o in objs]))
    return cands, cp, op, costs


def test_every_candidate_row_lands_in_exactly_one_reference_leg_and_every_od_in_one(sc, tmp_path):
    cands, cp, op, costs = ref_world(sc, tmp_path)
    for n in (1, 2, 4, 7):
        sched, _, table = sc.build_reference_schedule(cp, op, costs, n)
        rows = [i for leg in sched["legs"] for i in leg["rows"]]
        assert sorted(rows) == list(range(len(cands))), n
        objs = [o for leg in sched["legs"] for o in leg["od_objects"]]
        assert sorted(objs) == ["A", "B", "C"], n
        assert sc.validate_reference(cands, ["A", "B", "C"], sched["candidates_sha256"], sched, table) == [], n


def test_the_reference_schedule_is_a_pure_function_of_its_inputs(sc, tmp_path):
    cands, cp, op, costs = ref_world(sc, tmp_path)
    one, _, _ = sc.build_reference_schedule(cp, op, costs, 5)
    two, _, _ = sc.build_reference_schedule(cp, op, costs, 5)
    assert json.dumps(one, sort_keys=True) == json.dumps(two, sort_keys=True)


def test_an_od_unit_priced_from_the_table_goes_to_a_leg_by_cost(sc, tmp_path):
    key = None
    plan = make_plan("A", "B", "C")
    key = keyof(sc, row("B", "orbit_determination_radar"))
    cands, cp, op, costs = ref_world(sc, tmp_path, minutes={key: 30.0})
    sched, _, _ = sc.build_reference_schedule(cp, op, costs, 3)
    leg_b = next(leg for leg in sched["legs"] if "B" in leg["od_objects"])
    assert leg_b["predicted_min"] >= 30.0
    assert plan  # the OD rows came from the table's known rows, not the candidates


def test_an_object_the_table_has_never_seen_gets_a_placeholder_od_unit_named_as_an_estimate(sc, tmp_path):
    cands, cp, op, costs = ref_world(sc, tmp_path)
    op.write_text(json.dumps([{"name": o, "slug": o} for o in ("A", "B", "C", "NEW")]))
    cp.write_text(json.dumps(cands + ref_candidates("NEW")))
    sched, _, _ = sc.build_reference_schedule(cp, op, costs, 3)
    assert any("NEW" in line for line in sched["estimated_rows"])
    assert sum("NEW" in leg["od_objects"] for leg in sched["legs"]) == 1


def test_the_reference_heavy_row_fans_out_to_distinct_legs_with_overrides(sc, tmp_path):
    cands, cp, op, costs = ref_world(sc, tmp_path)
    heavy = next(i for i, r in enumerate(cands) if r["object"] == "A" and r["test_type"] == "propagation" and r["propagation_uncertainty"] == "auto_detection_on" and r["dt_days"] == 20.0)
    data = json.loads(costs.read_text())
    key = keyof(sc, cands[heavy])
    data["channels"]["rust"]["rows"][key] = 45.0
    data["heavy"] = {"rows": [key]}
    costs.write_text(json.dumps(data))
    sched, _, table = sc.build_reference_schedule(cp, op, costs, 5)
    holding = [leg for leg in sched["legs"] if heavy in leg["rows"]]
    assert len(holding) == 3 and all(leg["reps"] == {str(heavy): 1} for leg in holding)
    assert sched["replicas"][0]["legs"] == [leg["leg"] for leg in holding]
    assert sc.validate_reference(cands, ["A", "B", "C"], sched["candidates_sha256"], sched, table) == []


def test_validate_reference_names_a_dropped_row_a_doubled_od_and_a_stale_candidate_list(sc, tmp_path):
    cands, cp, op, costs = ref_world(sc, tmp_path)
    sched, _, table = sc.build_reference_schedule(cp, op, costs, 3)
    objs = ["A", "B", "C"]
    h = sched["candidates_sha256"]
    sched["legs"][0]["rows"] = sched["legs"][0]["rows"][1:]
    assert any("plan rows are in no leg" in b for b in sc.validate_reference(cands, objs, h, sched, table))
    sched, _, table = sc.build_reference_schedule(cp, op, costs, 3)
    sched["legs"][1]["od_objects"] = sched["legs"][0]["od_objects"] + sched["legs"][1]["od_objects"]
    assert any("must be in exactly one" in b for b in sc.validate_reference(cands, objs, h, sched, table))
    assert any("different candidate list" in b for b in sc.validate_reference(cands, objs, "0" * 64, sched, table))


def test_a_small_leg_budget_still_gives_a_heavy_channel_its_replica_legs(sc, tmp_path):
    plan, heavy = heavy_plan()
    table = heavy_table(sc, plan, heavy)
    plan_path = tmp_path / "plan.json"
    plan_path.write_text(json.dumps(plan))
    costs = tmp_path / "costs.json"
    costs.write_text(json.dumps(dict(table.data, heavy={"rows": [keyof(sc, plan[heavy])]})))
    # A budget of 4 gives each of the four channels one leg; the heavy row needs three.
    sched, _, _ = sc.build_schedule(plan_path, costs, ["python", "c", "cli", "core"], 4, None)
    for ch in ("python", "c", "cli", "core"):
        assert len(sched["channels"][ch]["legs"]) >= 3, ch
    assert len(sched["replicas"]) == 4


def test_a_small_reference_leg_count_still_gives_the_heavy_row_its_replica_legs(sc, tmp_path):
    cands, cp, op, costs = ref_world(sc, tmp_path)
    heavy = next(i for i, r in enumerate(cands) if r["object"] == "A" and r["test_type"] == "propagation" and r["propagation_uncertainty"] == "auto_detection_on" and r["dt_days"] == 20.0)
    data = json.loads(costs.read_text())
    key = keyof(sc, cands[heavy])
    data["channels"]["rust"]["rows"][key] = 45.0
    data["heavy"] = {"rows": [key]}
    costs.write_text(json.dumps(data))
    sched, _, table = sc.build_reference_schedule(cp, op, costs, 1)
    assert len(sched["legs"]) == 3
    assert sc.validate_reference(cands, ["A", "B", "C"], sched["candidates_sha256"], sched, table) == []


def test_replicas_of_one_row_land_on_distinct_legs_even_when_another_leg_stays_the_lightest(sc):
    """Three 100-minute units fill three legs; a fourth leg stays the lightest
    after taking one replica. The next replica must still go elsewhere."""
    plan = make_plan("A", "B", "C", "D", "E")
    big = [i for i, r in enumerate(plan) if r["test_type"] == "propagation" and r["dt_days"] == 30.0][:3]
    heavy = next(i for i, r in enumerate(plan) if r["object"] == "E" and r["test_type"] == "propagation" and r["dt_days"] == 20.0)
    minutes = {keyof(sc, plan[i]): 100.0 for i in big}
    minutes[keyof(sc, plan[heavy])] = 45.0
    table = make_table(sc, plan, minutes)
    table.heavy_rows = {keyof(sc, plan[heavy])}
    legs, _, _, _, replicas = sc.pack("python", plan, table, 4)
    holding = [leg["leg"] for leg in legs if heavy in leg["rows"]]
    assert len(holding) == 3 and replicas[0]["legs"] == holding


def test_a_replica_is_priced_at_one_run_not_the_whole_best_of_three(sc):
    plan, heavy = heavy_plan()
    table = heavy_table(sc, plan, heavy, minutes=45.0)
    legs, _, _, _, _ = sc.pack("python", plan, table, 3)
    for leg in legs:
        assert heavy in leg["rows"]
        # one 15-minute run plus the leg's share of the 0.001-minute rows
        assert 15.0 <= leg["predicted_min"] < 15.2, leg
