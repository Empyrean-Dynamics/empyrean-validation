"""Heavy decoupling (S5): the heavy propagation row runs on legs of its own from
prep, its plan row is emitted with no engine call, and the final assemble
checks the reference's strip equals the plan everything replayed.

Covers scripts/schedule.py (heavy, pack-reference --heavy, pack --heavy,
pack-heavy and their validators), scripts/channel-chunks.py (concat with the
heavy legs and --dropped), scripts/assemble-reference.py (the emitted plan rows,
the heavy legs, check-final) and the Makefile's heavy plan check."""

import importlib.util
import json
import pathlib
import subprocess
import sys

import pytest

SCRIPTS = pathlib.Path(__file__).resolve().parents[1]
ROOT = SCRIPTS.parent
TS = "2026-11-09T00:00:00+00:00"


def load(name, filename):
    sys.path.insert(0, str(SCRIPTS))
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / filename)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


@pytest.fixture(scope="module")
def sc():
    return load("schedule_for_heavy", "schedule.py")


@pytest.fixture(scope="module")
def cc():
    return load("channel_chunks_for_heavy", "channel-chunks.py")


@pytest.fixture(scope="module")
def ar():
    return load("assemble_reference_for_heavy", "assemble-reference.py")


OBJECTS = ["A", "B"]
DTS = (10.0, 20.0, 30.0, 40.0)
CHANNELS = ["python", "c", "cli", "core"]


def ident(obj, tt, method, dt, observer=None):
    return {"object": obj, "test_type": tt, "propagation_uncertainty": method, "dt_days": dt, "observer": observer, "force_model": "standard"}


def candidates():
    out = []
    for o in OBJECTS:
        for m in ("first_order_detection_on", "auto_detection_on"):
            for dt in DTS:
                out.append(ident(o, "propagation", m, dt))
            out.append(ident(o, "ephemeris", m, 10.0, "500"))
    return out


HEAVY_KEY = "A|propagation|auto_detection_on|30.0||standard"


def costs_table(sc, cands, heavy=(HEAVY_KEY,), heavy_minutes=40.0):
    """A cost table pricing every row at 0.01 min per run and the heavy rows at
    `heavy_minutes` per run (both times the channel's repetitions)."""
    chans = {}
    for ch in sc.CHANNELS:
        rows = {}
        for r in cands:
            n = sc.reps(ch, r)
            rows[sc.key_str(sc.row_key(r))] = (heavy_minutes if sc.key_str(sc.row_key(r)) in heavy else 0.01) * n
        classes = {sc.class_of(r) for r in cands} | {"orbit_determination|"}
        chans[ch] = {"rows": rows, "class_median": {c: 0.01 for c in classes}, "class_max": {c: 0.01 for c in classes}}
    known = {}
    for r in cands:
        known.setdefault(sc.group_of(r), []).append(float(r["dt_days"]))
    return {"schema": sc.SCHEMA, "channels": chans, "known_rows": known, "heavy": {"min_single_run_s": 600.0, "rows": list(heavy)}, "run_id": "test"}


@pytest.fixture
def world(sc, tmp_path):
    cands = candidates()
    cp = tmp_path / "candidates.json"
    cp.write_text(json.dumps(cands))
    op = tmp_path / "objects.json"
    op.write_text(json.dumps([{"name": o} for o in OBJECTS]))
    tp = tmp_path / "costs.json"
    tp.write_text(json.dumps(costs_table(sc, cands)))
    return {"cands": cands, "cp": cp, "op": op, "tp": tp, "dir": tmp_path}


def plan_row(c, notes="catalog note"):
    r = dict(c, channel="plan", timestamp=TS, notes=notes, ic_pos_au=[1.0, 0.0, 0.0], excluded_perturbers_naif=[])
    for k in ("emp_pos_au", "emp_time_ms", "emp_vs_horizons_km"):
        r[k] = None
    return r


def write_heavy(sc, world):
    heavy = sc.build_heavy(world["cp"], world["tp"], CHANNELS, None)
    hp = world["dir"] / "heavy.json"
    hp.write_text(json.dumps(heavy))
    sel = sc.heavy_select_rows(heavy, world["cands"])
    plan = [plan_row(r) for r in sel]
    hplan = world["dir"] / "heavy-plan.json"
    hplan.write_text(json.dumps(plan))
    (world["dir"] / "heavy-plan.json.dropped.json").write_text("[]")
    return heavy, hp, hplan


# ── schedule.py ─────────────────────────────────────────────────────────────


def test_only_heavy_propagation_rows_decouple(sc, world):
    cands = world["cands"]
    eph_key = "A|ephemeris|auto_detection_on|10.0|500|standard"
    table = sc.CostTable(costs_table(sc, cands, heavy=(HEAVY_KEY, eph_key)), "x")
    got = sc.select_heavy(cands, table, ["rust", *CHANNELS], None)
    assert [sc.key_str(sc.row_key(cands[i])) for i in got] == [HEAVY_KEY], "a marked ephemeris row stays in-path"
    # A threshold decouples a row whose single run is that slow in any channel.
    slow = sc.select_heavy(cands, table, ["rust", *CHANNELS], 0.5)
    assert all(cands[i]["test_type"] == "propagation" for i in slow) and len(slow) == 16


def test_leaders_are_distinct_light_rows_of_the_object_ahead_of_the_heavy_row(sc, world):
    heavy = sc.build_heavy(world["cp"], world["tp"], CHANNELS, None)
    (h,) = heavy["rows"]
    assert h["key"] == HEAVY_KEY and len(h["leaders"]) == 3
    keys = [sc.key_str(sc.row_key(x)) for x in h["leaders"]]
    assert len(set(keys)) == 3
    idx = {sc.key_str(sc.row_key(r)): i for i, r in enumerate(world["cands"])}
    assert all(idx[k] < h["index"] and k.startswith("A|propagation|") for k in keys)
    table = sc.CostTable(costs_table(sc, world["cands"]), "x")
    with pytest.raises(SystemExit, match="needs 9 light propagation rows"):
        sc.pick_leaders(world["cands"], [h["index"]], table, 9)


def test_the_reference_puts_the_heavy_row_on_its_own_legs_only(sc, world):
    heavy, hp, _ = write_heavy(sc, world)
    light, cands, table = sc.build_reference_schedule(world["cp"], world["op"], world["tp"], 2, None, hp)
    hs = sc.build_heavy_reference_schedule(world["cp"], world["op"], world["tp"], hp)
    i = heavy["rows"][0]["index"]
    assert light["decoupled"] == [i]
    assert all(i not in leg["rows"] for leg in light["legs"])
    assert [leg["leg"] for leg in hs["legs"]] == ["h01", "h02", "h03"]
    assert all(leg["rows"] == [i] and leg["reps"] == {str(i): 1} and leg["od_objects"] == [] for leg in hs["legs"])
    assert sc.validate_reference(cands, OBJECTS, light["candidates_sha256"], light, table) == []
    assert sc.validate_heavy_reference(cands, light["candidates_sha256"], hs, light) == []
    # Mutations the validators must catch.
    bad_light = json.loads(json.dumps(light))
    bad_light["legs"][0]["rows"] = sorted(bad_light["legs"][0]["rows"] + [i])
    assert any("decoupled row" in b for b in sc.validate_reference(cands, OBJECTS, light["candidates_sha256"], bad_light, table))
    bad_heavy = json.loads(json.dumps(hs))
    bad_heavy["legs"].pop()
    assert any("the reference times it 3 times" in b for b in sc.validate_heavy_reference(cands, light["candidates_sha256"], bad_heavy, light))


def test_the_channel_schedule_leaves_the_decoupled_row_out_of_every_light_leg(sc, world):
    _, hp, _ = write_heavy(sc, world)
    plan = [plan_row(c) for c in world["cands"]] + [plan_row(ident(o, "orbit_determination", None, 0.0)) for o in OBJECTS]
    pp = world["dir"] / "validation_plan.json"
    pp.write_text(json.dumps(plan))
    s, plan, table = sc.build_schedule(pp, world["tp"], CHANNELS, 8, None, None, hp)
    i = next(j for j, r in enumerate(plan) if sc.key_str(sc.row_key(r)) == HEAVY_KEY)
    assert s["decoupled"] == [i]
    for ch in CHANNELS:
        assert all(i not in leg["rows"] for leg in s["channels"][ch]["legs"])
    assert sc.validate(plan, s["plan_sha256"], s, table) == []
    s["channels"]["c"]["legs"][0]["rows"] = sorted(s["channels"]["c"]["legs"][0]["rows"] + [i])
    assert any("decoupled row" in b for b in sc.validate(plan, s["plan_sha256"], s, table))


def test_the_heavy_channel_legs_run_one_replica_each_behind_a_leader_of_their_own(sc, world):
    _, hp, hplan = write_heavy(sc, world)
    s, plan = sc.build_heavy_channel_schedule(hplan, hp, world["tp"], CHANNELS)
    table = sc.CostTable.load(world["tp"])
    assert sc.validate_heavy_channels(plan, s["plan_sha256"], s, table) == []
    h = s["decoupled"][0]
    for ch in CHANNELS:
        legs = s["channels"][ch]["legs"]
        assert len(legs) == 3 and all(h in leg["rows"] and leg["reps"] == {str(h): 1} for leg in legs)
        if ch in ("c", "cli"):
            firsts = [leg["rows"][0] for leg in legs]
            assert all(f in s["leaders"] for f in firsts) and len(set(firsts)) == 3
        else:
            assert all(leg["rows"] == [h] for leg in legs)
    # Mutations: a leg without its leader, a leader shared by two legs.
    bad = json.loads(json.dumps(s))
    bad["channels"]["cli"]["legs"][1]["rows"] = [h]
    assert any("does not open on a light leader row" in b for b in sc.validate_heavy_channels(plan, s["plan_sha256"], bad, table))
    bad = json.loads(json.dumps(s))
    bad["channels"]["c"]["legs"][1]["rows"] = list(bad["channels"]["c"]["legs"][0]["rows"])
    assert any("leads legs" in b for b in sc.validate_heavy_channels(plan, s["plan_sha256"], bad, table))


def test_no_heavy_row_means_no_heavy_legs(sc, world):
    world["tp"].write_text(json.dumps(costs_table(sc, world["cands"], heavy=())))
    heavy, hp, hplan = write_heavy(sc, world)
    assert heavy["rows"] == []
    hs = sc.build_heavy_reference_schedule(world["cp"], world["op"], world["tp"], hp)
    s, _ = sc.build_heavy_channel_schedule(hplan, hp, world["tp"], CHANNELS)
    assert hs["legs"] == [] and sc.heavy_leg_matrix(s) == []


# ── channel-chunks.py concat with the heavy legs ────────────────────────────


@pytest.fixture
def staged(sc, cc, world):
    """A main plan with one decoupled row, its light schedule and leg outputs, the
    heavy plan and schedule and the heavy legs' outputs for the python and c
    channels."""
    _, hp, hplan = write_heavy(sc, world)
    plan = [plan_row(c) for c in world["cands"]] + [plan_row(ident(o, "orbit_determination", None, 0.0)) for o in OBJECTS]
    pp = world["dir"] / "validation_plan.json"
    pp.write_text(json.dumps(plan))
    s, plan, _ = sc.build_schedule(pp, world["tp"], ["python", "c"], 4, None, None, hp)
    sp = world["dir"] / "schedule.json"
    sp.write_text(json.dumps(s))
    hs, hplan_rows = sc.build_heavy_channel_schedule(hplan, hp, world["tp"], ["python", "c"])
    hsp = world["dir"] / "schedule-heavy.json"
    hsp.write_text(json.dumps(hs))
    out = world["dir"] / "out"
    out.mkdir()

    def result(r, t):
        return dict(r, channel="x", emp_pos_au=[0.5, 0.25, float(r["dt_days"])], emp_time_ms=t)

    for ch in ("python", "c"):
        for leg in s["channels"][ch]["legs"]:
            rows = [result(plan[i], 1.0) for i in leg["rows"]]
            (out / f"validation_{ch}.leg{leg['leg']}.json").write_text(json.dumps(rows))
        for k, leg in enumerate(hs["channels"][ch]["legs"]):
            rows = [result(hplan_rows[i], 100.0 + k) for i in leg["rows"]]
            (out / f"validation_{ch}_heavy.leg{leg['leg']}.json").write_text(json.dumps(rows))
    return {"plan": plan, "pp": pp, "sp": sp, "hplan": hplan, "hsp": hsp, "out": out, "s": s}


def concat(cc, st, ch, *extra, out_name="merged.json"):
    legs = sorted(st["out"].glob(f"validation_{ch}.leg*.json"))
    args = ["concat", "--plan", str(st["pp"]), "--schedule", str(st["sp"]), "--channel", ch, "--out", str(st["out"] / out_name)]
    args += ["--heavy-plan", str(st["hplan"]), "--heavy-schedule", str(st["hsp"]), "--heavy-dir", str(st["out"])]
    return cc.main(args + list(extra) + [str(p) for p in legs])


def test_the_decoupled_row_lands_at_its_place_folded_and_no_leader_is_kept(cc, staged, capsys):
    for ch in ("python", "c"):
        assert concat(cc, staged, ch) == 0
        merged = json.loads((staged["out"] / "merged.json").read_text())
        keys = [cc.sched.key_str(cc.sched.row_key(r)) for r in merged]
        assert keys == [cc.sched.key_str(cc.sched.row_key(r)) for r in staged["plan"]], "every plan row once, in plan order"
        heavy = merged[keys.index(HEAVY_KEY)]
        assert heavy["emp_time_ms"] == 100.0, "the replicas fold to the minimum time"
    out = capsys.readouterr().out
    assert "warm-up leader row(s) not kept" in out and "3 warm-up leader" in out


def test_replicas_that_disagree_or_a_missing_heavy_leg_fail_the_channel(cc, staged):
    f = staged["out"] / "validation_python_heavy.legh02.json"
    rows = json.loads(f.read_text())
    rows[-1]["emp_pos_au"][0] += 1e-16
    f.write_text(json.dumps(rows))
    with pytest.raises(SystemExit, match="timing replicas disagree in `emp_pos_au`"):
        concat(cc, staged, "python")
    f.unlink()
    with pytest.raises(SystemExit, match="missing leg"):
        concat(cc, staged, "python")


def test_a_decoupling_schedule_without_heavy_legs_is_refused(cc, staged):
    legs = sorted(staged["out"].glob("validation_python.leg*.json"))
    args = ["concat", "--plan", str(staged["pp"]), "--schedule", str(staged["sp"]), "--channel", "python", "--out", str(staged["out"] / "m.json")]
    with pytest.raises(SystemExit, match="no heavy legs were given"):
        cc.main(args + [str(p) for p in legs])


def test_a_dropped_decoupled_row_is_discarded_and_any_other_dropped_row_is_an_error(cc, staged):
    drop = staged["out"] / "heavy_dropped.json"
    drop.write_text(json.dumps([dict(ident("A", "propagation", "auto_detection_on", 30.0), reason="refused")]))
    assert concat(cc, staged, "c", "--dropped", str(drop)) == 0
    keys = [cc.sched.key_str(cc.sched.row_key(r)) for r in json.loads((staged["out"] / "merged.json").read_text())]
    assert HEAVY_KEY not in keys and len(keys) == len(staged["plan"]) - 1
    drop.write_text(json.dumps([dict(ident("B", "propagation", "auto_detection_on", 30.0), reason="refused")]))
    with pytest.raises(SystemExit, match="not a decoupled row"):
        concat(cc, staged, "c", "--dropped", str(drop))


# ── assemble-reference.py ───────────────────────────────────────────────────


def rust_row(c, t=10.0, note="catalog note"):
    return dict(c, channel="rust", timestamp=TS, emp_time_ms=t, emp_pos_au=[0.1, 0.2, float(c["dt_days"])], notes=note)


def write_leg(root, leg, rows, dropped=(), od_objects=()):
    d = root / leg
    d.mkdir(parents=True, exist_ok=True)
    (d / "validation_rust_propeph.json").write_text(json.dumps(rows))
    (d / "validation_rust_propeph.json.dropped.json").write_text(json.dumps(list(dropped)))
    od = [dict(ident(o, "orbit_determination", None, 0.0), timestamp=TS, od_chi2=1.0) for o in od_objects]
    (d / "validation_rust_od.json").write_text(json.dumps(od))
    for name in ("validation_rust_od_orbits.jsonl", "validation_rust_od_compare.jsonl"):
        (d / name).write_text("".join(json.dumps({"object": o}) + "\n" for o in od_objects))


@pytest.fixture
def ref(sc, world):
    heavy, hp, hplan = write_heavy(sc, world)
    light, cands, _ = sc.build_reference_schedule(world["cp"], world["op"], world["tp"], 2, None, hp)
    hs = sc.build_heavy_reference_schedule(world["cp"], world["op"], world["tp"], hp)
    legs = world["dir"] / "shards"
    for leg in light["legs"]:
        write_leg(legs, leg["leg"], [rust_row(cands[i]) for i in leg["rows"]], od_objects=leg["od_objects"])
    for k, leg in enumerate(hs["legs"]):
        write_leg(legs, leg["leg"], [rust_row(cands[i], t=500.0 + k) for i in leg["rows"]])
    return {"light": light, "hs": hs, "heavy": heavy, "hplan": hplan, "legs": legs, "cands": cands}


def test_the_provisional_reference_takes_the_emitted_row_at_its_candidate_place(ar, ref):
    leaders = {ar.sched.key_str(ar.sched.row_key(x)) for r in ref["heavy"]["rows"] for x in r["leaders"]}
    emitted = (json.loads(ref["hplan"].read_text()), [], leaders)
    a = ar.assemble(ref["cands"], ref["light"], ref["legs"], emitted=emitted)
    keys = [ar.sched.key_str(ar.sched.row_key(r)) for r in a["propeph"]]
    assert keys == [ar.sched.key_str(ar.sched.row_key(c)) for c in ref["cands"]]
    row = a["propeph"][keys.index(HEAVY_KEY)]
    assert row["channel"] == "plan" and row["emp_pos_au"] is None, "the decoupled row is its emitted plan row"
    # An emitted row that is neither decoupled nor a leader is refused.
    stray = (emitted[0] + [plan_row(ident("B", "propagation", "auto_detection_on", 40.0))], [], leaders)
    with pytest.raises(SystemExit):
        ar.assemble(ref["cands"], ref["light"], ref["legs"], emitted=stray)


def test_the_final_reference_folds_the_heavy_legs_and_refuses_one_missing(ar, ref):
    a = ar.assemble(ref["cands"], ref["light"], ref["legs"], heavy=ref["hs"])
    keys = [ar.sched.key_str(ar.sched.row_key(r)) for r in a["propeph"]]
    row = a["propeph"][keys.index(HEAVY_KEY)]
    assert row["emp_time_ms"] == 500.0 and any("folded " + HEAVY_KEY in f for f in a["folded"])
    import shutil

    shutil.rmtree(ref["legs"] / "h02")
    with pytest.raises(SystemExit):
        ar.assemble(ref["cands"], ref["light"], ref["legs"], heavy=ref["hs"])


def stripped(rows):
    return [plan_row({k: r[k] for k in ("object", "test_type", "propagation_uncertainty", "dt_days", "observer", "force_model")}, notes=r.get("notes", "catalog note")) for r in rows]


def test_check_final_passes_an_equal_strip_whatever_the_timestamps(ar, ref):
    published = stripped([rust_row(c) for c in ref["cands"]])
    final = json.loads(json.dumps(published))
    for r in final:
        r["timestamp"] = "2026-11-09T01:23:45+00:00"
    assert ar.check_final(published, final, ref["heavy"], []) == []


def test_check_final_fails_closed_on_a_perturbed_note_naming_the_row_and_field(ar, ref):
    published = stripped([rust_row(c) for c in ref["cands"]])
    final = json.loads(json.dumps(published))
    i = next(j for j, r in enumerate(final) if ar.sched.key_str(ar.sched.row_key(r)) == HEAVY_KEY)
    final[i]["notes"] = "catalog note; sky_cov_diag(...)"
    with pytest.raises(SystemExit, match=r"published plan row \d+ \(A\|propagation\|auto_detection_on\|30\.0\|\|standard\) differs .* in `notes`"):
        ar.check_final(published, final, ref["heavy"], [])


def test_check_final_names_a_refused_decoupled_row_and_refuses_any_other_gap(ar, ref):
    published = stripped([rust_row(c) for c in ref["cands"]])
    final = [r for r in published if ar.sched.key_str(ar.sched.row_key(r)) != HEAVY_KEY]
    refused = [dict(ident("A", "propagation", "auto_detection_on", 30.0), reason="engine refused")]
    got = ar.check_final(published, final, ref["heavy"], refused)
    assert [ar.sched.key_str(ar.sched.row_key(d)) for d in got] == [HEAVY_KEY] and got[0]["reason"] == "engine refused"
    with pytest.raises(SystemExit, match="neither delivered it nor named it refused"):
        ar.check_final(published, final, ref["heavy"], [])
    light_gap = published[1:]
    with pytest.raises(SystemExit, match="not a decoupled row"):
        ar.check_final(published, light_gap, ref["heavy"], [])
    with pytest.raises(SystemExit, match="which the published plan lacks"):
        ar.check_final(published[1:], published, ref["heavy"], [])
    swapped = list(published)
    swapped[0], swapped[1] = swapped[1], swapped[0]
    with pytest.raises(SystemExit, match="orders its rows differently"):
        ar.check_final(published, swapped, ref["heavy"], [])


# ── Makefile: a heavy leg checks the heavy plan for what it is ──────────────


def make_check_plan(tmp_path, rows, kind):
    plan = tmp_path / f"plan-{kind}.json"
    plan.write_text(json.dumps(rows))
    return subprocess.run(
        ["make", "-s", "-C", str(ROOT), "check-plan", "PLAN_PREBUILT=1", f"PLAN_KIND={kind}", f"PLAN={plan}"],
        capture_output=True,
        text=True,
        check=False,
    )


def test_a_heavy_plan_passes_the_heavy_check_and_fails_the_full_one(tmp_path):
    rows = [plan_row(ident("A", "propagation", "auto_detection_on", 30.0))]
    ok = make_check_plan(tmp_path, rows, "heavy")
    assert ok.returncode == 0, ok.stdout + ok.stderr
    full = make_check_plan(tmp_path, rows, "full")
    assert full.returncode != 0 and "ZERO orbit-determination rows" in full.stdout + full.stderr
    od = make_check_plan(tmp_path, rows + [plan_row(ident("A", "orbit_determination", None, 0.0))], "heavy")
    assert od.returncode != 0 and "propagation rows only" in od.stdout + od.stderr
