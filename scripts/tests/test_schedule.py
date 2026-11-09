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
    legs, estimated, _, _ = sc.pack(channel, plan, table, n_legs)
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
        legs, _, _, _ = sc.pack(ch, plan, table, 4)
        rows = [i for leg in legs for i in leg["rows"]]
        assert sorted(rows) == list(range(len(plan))), ch
        assert sum(leg["units"] for leg in legs) == len(sc.units(ch, plan)), ch


def test_legacy_fit_and_ng_row_never_split_for_python_c_cli(sc):
    plan = make_plan("A", "B", ng=("A", "B"))
    table = make_table(sc, plan)
    for ch in ("python", "c", "cli"):
        for n_legs in range(1, 9):
            legs, _, _, _ = sc.pack(ch, plan, table, n_legs)
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
    legs, estimated, _, _ = sc.pack("core", plan2, table, 2)
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
    _, estimated, _, _ = sc.pack("python", plan, table, 2)
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


def test_validator_requires_a_timing_fold_to_use_distinct_legs(sc):
    plan, table, schedule = good(sc)
    schedule["replicas"] = [{"channel": "python", "group": "g", "legs": ["01", "01", "02"]}]
    assert any("share a leg" in b for b in sc.validate(plan, "p", schedule, table))
    schedule["replicas"] = [{"channel": "python", "group": "g", "legs": ["01", "02", "03"]}]
    assert sc.validate(plan, "p", schedule, table) == []


def test_leg_matrix_lists_every_leg_once(sc):
    plan = make_plan("A", "B")
    table = make_table(sc, plan)
    schedule = {"channels": {}}
    for ch, n in (("python", 2), ("core", 3)):
        legs, _, _, _ = sc.pack(ch, plan, table, n)
        schedule["channels"][ch] = {"legs": legs}
    assert sc.leg_matrix(schedule) == [
        {"channel": "python", "leg": "01"},
        {"channel": "python", "leg": "02"},
        {"channel": "core", "leg": "01"},
        {"channel": "core", "leg": "02"},
        {"channel": "core", "leg": "03"},
    ]
