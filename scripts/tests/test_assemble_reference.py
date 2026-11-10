"""assemble-reference.py and reference-legs.py: the reference built from legs
equals the reference built in one piece, and every way a leg can go missing,
repeat or disagree is refused by name."""

import importlib.util
import json
import pathlib
import sys

import pytest

SCRIPTS = pathlib.Path(__file__).resolve().parents[1]


def load(name, filename):
    sys.path.insert(0, str(SCRIPTS))
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / filename)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


@pytest.fixture(scope="module")
def ar():
    return load("assemble_reference_under_test", "assemble-reference.py")


@pytest.fixture(scope="module")
def rl():
    return load("reference_legs_under_test", "reference-legs.py")


@pytest.fixture(scope="module")
def sc():
    return load("schedule_for_ref_tests", "schedule.py")


OBJECTS = ["A", "B"]
TS = "2026-11-09T00:00:00+00:00"


def cand(obj, tt, method, dt, observer=None):
    return {
        "object": obj,
        "test_type": tt,
        "propagation_uncertainty": method,
        "dt_days": dt,
        "observer": observer,
        "force_model": "standard",
    }


def candidates():
    out = []
    for o in OBJECTS:
        for m in ("first_order_detection_on", "auto_detection_on"):
            for dt in (10.0, 20.0):
                out.append(cand(o, "propagation", m, dt))
            out.append(cand(o, "ephemeris", m, 10.0, "500"))
    return out


def result(c, t=100.0, **kw):
    r = dict(c, channel="rust", timestamp=TS, emp_time_ms=t, emp_pos_au=[0.1, 0.2, float(c["dt_days"])], notes="")
    r.update(kw)
    return r


def od_row(obj, n=2):
    return [{"object": obj, "test_type": "orbit_determination", "propagation_uncertainty": None, "dt_days": 0.0, "observer": None, "force_model": "standard", "od_chi2": float(i), "timestamp": TS} for i in range(n)]


def side(obj, kind):
    return [json.dumps({"object": obj, "kind": kind, "i": i}) for i in range(2)]


def write_leg(root, leg, rows, dropped=(), od_objects=(), name=None):
    d = root / (name or leg)
    d.mkdir(parents=True, exist_ok=True)
    (d / "validation_rust_propeph.json").write_text(json.dumps(rows))
    (d / "validation_rust_propeph.json.dropped.json").write_text(json.dumps(list(dropped)))
    od = [r for o in od_objects for r in od_row(o)]
    (d / "validation_rust_od.json").write_text(json.dumps(od))
    (d / "validation_rust_od_orbits.jsonl").write_text("".join(ln + "\n" for o in od_objects for ln in side(o, "orbits")))
    (d / "validation_rust_od_compare.jsonl").write_text("".join(ln + "\n" for o in od_objects for ln in side(o, "compare")))
    return d


def schedule_for(sc, tmp, legs, replicas=()):
    cp = tmp / "candidates.json"
    cp.write_text(json.dumps(candidates()))
    return {
        "schema": sc.SCHEMA,
        "kind": "reference",
        "candidates_sha256": sc.file_sha256(cp),
        "objects": OBJECTS,
        "legs": legs,
        "replicas": list(replicas),
    }, cp


def stage(sc, tmp, assignment, od_assign, refused=(), dropped_by=None):
    """Write legs for `assignment` (leg -> candidate indices) and run nothing.
    `refused` candidate indices appear in their leg's dropped file."""
    cands = candidates()
    legs = []
    for leg, idx in assignment.items():
        legs.append({"leg": leg, "rows": sorted(idx), "od_objects": od_assign.get(leg, [])})
    sched, cp = schedule_for(sc, tmp, legs)
    root = tmp / "shards"
    for leg, idx in assignment.items():
        rows = [result(cands[i], t=100.0 + i) for i in sorted(idx) if i not in refused]
        drops = [dict(cands[i], reason="empyrean FAIL (test)") for i in sorted(idx) if i in refused]
        write_leg(root, leg, rows, drops, od_assign.get(leg, []))
    return sched, cp, root


def run_assemble(ar, sched, cp, root, tmp, capsys=None):
    out = {n: tmp / f"out_{n}" for n in ("propeph", "od", "orbits", "compare", "unified", "dropped")}
    sp = tmp / "schedule.json"
    sp.write_text(json.dumps(sched))
    argv = ["--schedule", str(sp), "--candidates", str(cp), "--legs-dir", str(root)]
    for n in ("propeph", "od", "orbits", "compare", "unified", "dropped"):
        argv += [f"--out-{n}", str(out[n])]
    assert ar.main(argv) == 0
    return {n: p.read_text() for n, p in out.items()}


ALL = list(range(len(candidates())))


def test_assembly_over_legs_that_cut_an_object_equals_the_single_leg_output(ar, sc, tmp_path):
    (tmp_path / "one").mkdir()
    single, cp1, root1 = stage(sc, tmp_path / "one", {"01": ALL}, {"01": OBJECTS})
    out1 = run_assemble(ar, single, cp1, root1, tmp_path / "one")
    (tmp_path / "three").mkdir()
    # Object A is cut across all three legs and interleaved; B's OD runs elsewhere than A's.
    three, cp3, root3 = stage(
        sc,
        tmp_path / "three",
        {"01": ALL[0::3], "02": ALL[1::3], "03": ALL[2::3]},
        {"02": ["B"], "03": ["A"]},
    )
    out3 = run_assemble(ar, three, cp3, root3, tmp_path / "three")
    assert out3 == out1
    assert json.loads(out3["unified"]) == json.loads(out3["propeph"]) + json.loads(out3["od"])


def test_assembly_orders_objects_in_catalog_order_and_prop_rows_before_od_rows(ar, sc, tmp_path):
    sched, cp, root = stage(sc, tmp_path, {"01": ALL[:8], "02": ALL[8:]}, {"01": ["B"], "02": ["A"]})
    out = run_assemble(ar, sched, cp, root, tmp_path)
    objs = [r["object"] for r in json.loads(out["od"])]
    assert objs == ["A", "A", "B", "B"]  # catalog order, though B's leg is the first leg
    assert [json.loads(ln)["object"] for ln in out["orbits"].splitlines()] == ["A", "A", "B", "B"]
    unified = json.loads(out["unified"])
    assert [r["test_type"] for r in unified[-4:]] == ["orbit_determination"] * 4


def test_assembly_accepts_the_legs_in_either_download_layout(ar, sc, tmp_path):
    sched, cp = schedule_for(sc, tmp_path, [{"leg": "01", "rows": ALL, "od_objects": OBJECTS}])
    cands = candidates()
    write_leg(tmp_path / "shards", "01", [result(c) for c in cands], (), OBJECTS, name="reference-leg-01")
    out = run_assemble(ar, sched, cp, tmp_path / "shards", tmp_path)
    assert len(json.loads(out["propeph"])) == len(cands)


def test_a_missing_row_is_named(ar, sc, tmp_path, capsys):
    sched, cp, root = stage(sc, tmp_path, {"01": ALL[:8], "02": ALL[8:]}, {"01": OBJECTS})
    f = root / "02" / "validation_rust_propeph.json"
    rows = json.loads(f.read_text())
    gone = rows.pop(3)
    f.write_text(json.dumps(rows))
    with pytest.raises(SystemExit):
        run_assemble(ar, sched, cp, root, tmp_path)
    err = capsys.readouterr().err
    assert "neither a result nor named as dropped" in err and gone["object"] in err and "leg 02" in err


def test_a_duplicate_row_in_one_leg_is_named(ar, sc, tmp_path, capsys):
    sched, cp, root = stage(sc, tmp_path, {"01": ALL}, {"01": OBJECTS})
    f = root / "01" / "validation_rust_propeph.json"
    rows = json.loads(f.read_text())
    f.write_text(json.dumps(rows + [rows[0]]))
    with pytest.raises(SystemExit):
        run_assemble(ar, sched, cp, root, tmp_path)
    assert "delivered" in capsys.readouterr().err


def test_a_row_delivered_by_a_leg_that_was_not_given_it_is_named(ar, sc, tmp_path, capsys):
    sched, cp, root = stage(sc, tmp_path, {"01": ALL[:8], "02": ALL[8:]}, {"01": OBJECTS})
    f = root / "02" / "validation_rust_propeph.json"
    stray = result(candidates()[0])
    f.write_text(json.dumps(json.loads(f.read_text()) + [stray]))
    with pytest.raises(SystemExit):
        run_assemble(ar, sched, cp, root, tmp_path)
    err = capsys.readouterr().err
    assert "which the schedule gave to ['01']" in err


def test_a_row_that_is_not_a_candidate_is_named(ar, sc, tmp_path, capsys):
    sched, cp, root = stage(sc, tmp_path, {"01": ALL}, {"01": OBJECTS})
    f = root / "01" / "validation_rust_propeph.json"
    f.write_text(json.dumps(json.loads(f.read_text()) + [result(cand("A", "propagation", "auto_detection_on", 999.0))]))
    with pytest.raises(SystemExit):
        run_assemble(ar, sched, cp, root, tmp_path)
    assert "not a candidate" in capsys.readouterr().err


def test_a_refused_row_is_accepted_only_when_the_leg_lists_it(ar, sc, tmp_path, capsys):
    sched, cp, root = stage(sc, tmp_path, {"01": ALL}, {"01": OBJECTS}, refused={5})
    out = run_assemble(ar, sched, cp, root, tmp_path)
    dropped = json.loads(out["dropped"])
    assert len(dropped) == 1 and dropped[0]["reason"] == "empyrean FAIL (test)"
    assert len(json.loads(out["propeph"])) == len(ALL) - 1
    # The same row removed from the leg WITHOUT being listed is a lost row.
    (tmp_path / "x").mkdir()
    sched2, cp2, root2 = stage(sc, tmp_path / "x", {"01": ALL}, {"01": OBJECTS}, refused={5})
    (root2 / "01" / "validation_rust_propeph.json.dropped.json").write_text("[]")
    with pytest.raises(SystemExit):
        run_assemble(ar, sched2, cp2, root2, tmp_path / "x")
    assert "neither a result nor named as dropped" in capsys.readouterr().err


def test_a_dropped_row_the_leg_was_not_given_is_named(ar, sc, tmp_path, capsys):
    sched, cp, root = stage(sc, tmp_path, {"01": ALL[:8], "02": ALL[8:]}, {"01": OBJECTS})
    f = root / "01" / "validation_rust_propeph.json.dropped.json"
    f.write_text(json.dumps([dict(candidates()[10], reason="x")]))
    with pytest.raises(SystemExit):
        run_assemble(ar, sched, cp, root, tmp_path)
    assert "which was not selected for it" in capsys.readouterr().err


def test_an_absent_leg_is_named(ar, sc, tmp_path, capsys):
    sched, cp, root = stage(sc, tmp_path, {"01": ALL[:8], "02": ALL[8:]}, {"01": OBJECTS})
    for f in (root / "02").iterdir():
        f.unlink()
    (root / "02").rmdir()
    with pytest.raises(SystemExit):
        run_assemble(ar, sched, cp, root, tmp_path)
    err = capsys.readouterr().err
    assert "leg 02: did not arrive" in err and "1 leg(s) absent" in err


def test_the_ods_of_every_object_must_arrive_from_the_leg_that_owns_them(ar, sc, tmp_path, capsys):
    sched, cp, root = stage(sc, tmp_path, {"01": ALL}, {"01": ["A"]})  # B's OD is owned by nobody
    sched["legs"][0]["od_objects"] = ["A", "B"]  # the schedule says leg 01 runs B's OD too
    # leg 01 wrote only A's OD rows, so B has no OD rows: not an error by itself (an
    # object without a PSV fixture has none), but the whole reference needs some.
    out = run_assemble(ar, sched, cp, root, tmp_path)
    assert [r["object"] for r in json.loads(out["od"])] == ["A", "A"]


def test_od_rows_for_an_object_the_leg_does_not_own_are_named(ar, sc, tmp_path, capsys):
    sched, cp, root = stage(sc, tmp_path, {"01": ALL[:8], "02": ALL[8:]}, {"01": ["A"], "02": ["B"]})
    f = root / "01" / "validation_rust_od.json"
    f.write_text(json.dumps(od_row("A") + od_row("B")))
    with pytest.raises(SystemExit):
        run_assemble(ar, sched, cp, root, tmp_path)
    assert "OD rows for 'B', which the schedule gave to leg 02" in capsys.readouterr().err


def test_a_reference_with_no_od_rows_at_all_is_a_dead_axis(ar, sc, tmp_path):
    sched, cp, root = stage(sc, tmp_path, {"01": ALL}, {})
    with pytest.raises(SystemExit, match="zero OD rows"):
        run_assemble(ar, sched, cp, root, tmp_path)


def test_a_schedule_for_another_candidate_list_is_refused(ar, sc, tmp_path):
    sched, cp, root = stage(sc, tmp_path, {"01": ALL}, {"01": OBJECTS})
    sched["candidates_sha256"] = "0" * 64
    with pytest.raises(SystemExit, match="different candidate list"):
        run_assemble(ar, sched, cp, root, tmp_path)


# ── replicas ────────────────────────────────────────────────────────────────

HEAVY = 3  # candidate index of the replicated row


def stage_replicas(sc, tmp, times=(300.0, 100.0, 200.0), refused_in=(), mutate=None):
    cands = candidates()
    rest = [i for i in ALL if i != HEAVY]
    legs = [
        {"leg": "01", "rows": sorted(rest[:6] + [HEAVY]), "od_objects": OBJECTS, "reps": {str(HEAVY): 1}},
        {"leg": "02", "rows": sorted(rest[6:] + [HEAVY]), "od_objects": [], "reps": {str(HEAVY): 1}},
        {"leg": "03", "rows": [HEAVY], "od_objects": [], "reps": {str(HEAVY): 1}},
    ]
    reps = [{"channel": "rust", "group": "g", "row": HEAVY, "legs": ["01", "02", "03"], "reps": 1}]
    sched, cp = schedule_for(sc, tmp, legs, reps)
    root = tmp / "shards"
    for leg, t in zip(("01", "02", "03"), times):
        spec = next(x for x in legs if x["leg"] == leg)
        rows, drops = [], []
        for i in spec["rows"]:
            if i == HEAVY and leg in refused_in:
                drops.append(dict(cands[i], reason="refused"))
            else:
                r = result(cands[i], t=t if i == HEAVY else 100.0 + i)
                if i == HEAVY and mutate and leg in mutate:
                    r.update(mutate[leg])
                rows.append(r)
        write_leg(root, leg, rows, drops, spec["od_objects"])
    return sched, cp, root


def test_replicas_fold_to_the_minimum_time_and_equal_the_single_run(ar, sc, tmp_path):
    sched, cp, root = stage_replicas(sc, tmp_path)
    out = json.loads(run_assemble(ar, sched, cp, root, tmp_path)["propeph"])
    cands = candidates()
    key = cands[HEAVY]
    row = next(r for r in out if all(r[k] == key[k] for k in key))
    assert row["emp_time_ms"] == 100.0
    assert len(out) == len(cands)  # folded to one row, not three


def test_replicas_that_disagree_fail_naming_the_field_and_legs(ar, sc, tmp_path, capsys):
    sched, cp, root = stage_replicas(sc, tmp_path, mutate={"02": {"emp_pos_au": [0.1, 0.2, 99.0]}})
    with pytest.raises(SystemExit):
        run_assemble(ar, sched, cp, root, tmp_path)
    err = capsys.readouterr().err
    assert "emp_pos_au" in err and "01" in err and "02" in err


def test_replicas_that_disagree_on_whether_the_engine_refused_the_row_fail(ar, sc, tmp_path, capsys):
    sched, cp, root = stage_replicas(sc, tmp_path, refused_in=("03",))
    with pytest.raises(SystemExit):
        run_assemble(ar, sched, cp, root, tmp_path)
    assert "refused it in leg(s) ['03'] but delivered it" in capsys.readouterr().err


def test_a_replica_leg_that_did_not_deliver_the_row_is_named(ar, sc, tmp_path, capsys):
    sched, cp, root = stage_replicas(sc, tmp_path)
    f = root / "03" / "validation_rust_propeph.json"
    f.write_text("[]")
    with pytest.raises(SystemExit):
        run_assemble(ar, sched, cp, root, tmp_path)
    assert "(leg 03)" in capsys.readouterr().err


def test_replicas_all_refused_are_dropped_once(ar, sc, tmp_path):
    sched, cp, root = stage_replicas(sc, tmp_path, refused_in=("01", "02", "03"))
    out = run_assemble(ar, sched, cp, root, tmp_path)
    assert len(json.loads(out["dropped"])) == 1


# ── reference-legs.py ───────────────────────────────────────────────────────


def ref_sched(sc, tmp):
    cands = candidates()
    cp = tmp / "candidates.json"
    cp.write_text(json.dumps(cands))
    legs = [
        {"leg": "01", "rows": [0, 1, 2, 3], "od_objects": ["A"], "predicted_min": 3.0, "reps": {"3": 1}},
        {"leg": "02", "rows": [3, 4, 5, 6, 7], "od_objects": [], "predicted_min": 2.0, "reps": {"3": 1}},
        {"leg": "03", "rows": [], "od_objects": ["B"], "predicted_min": 1.0},
    ]
    sched = {"schema": sc.SCHEMA, "kind": "reference", "candidates_sha256": sc.file_sha256(cp), "objects": OBJECTS, "legs": legs, "replicas": []}
    sp = tmp / "refsched.json"
    sp.write_text(json.dumps(sched))
    return cp, sp


def select_args(tmp, cp, sp, leg):
    return ["select", "--candidates", str(cp), "--schedule", str(sp), "--leg", leg, "--rows-out", str(tmp / "rows.json"), "--reps-out", str(tmp / "reps.json"), "--objects-out", str(tmp / "objs")]


def test_select_writes_the_legs_rows_reps_and_od_objects(rl, sc, tmp_path, capsys):
    cp, sp = ref_sched(sc, tmp_path)
    assert rl.main(select_args(tmp_path, cp, sp, "01")) == 0
    rows = json.loads((tmp_path / "rows.json").read_text())
    assert rows == candidates()[:4]
    reps = json.loads((tmp_path / "reps.json").read_text())
    assert len(reps) == 1 and reps[0]["reps"] == 1 and reps[0]["dt_days"] == candidates()[3]["dt_days"]
    assert (tmp_path / "objs").read_text() == "A"
    assert "Reference leg 01 of 3: 4 candidate rows, 1 repetition override(s), OD of 1 object(s)" in capsys.readouterr().out


def test_select_on_an_od_only_leg_writes_no_rows_file_and_on_a_rows_only_leg_no_objects_file(rl, sc, tmp_path):
    cp, sp = ref_sched(sc, tmp_path)
    rl.main(select_args(tmp_path, cp, sp, "03"))
    assert not (tmp_path / "rows.json").exists() and (tmp_path / "objs").read_text() == "B"
    rl.main(select_args(tmp_path, cp, sp, "02"))
    assert (tmp_path / "rows.json").exists() and not (tmp_path / "objs").exists()
    assert json.loads((tmp_path / "reps.json").read_text())[0]["reps"] == 1


def test_select_refuses_a_stale_candidate_list_an_unknown_leg_and_an_empty_leg(rl, sc, tmp_path):
    cp, sp = ref_sched(sc, tmp_path)
    with pytest.raises(SystemExit, match="no leg '09'"):
        rl.main(select_args(tmp_path, cp, sp, "09"))
    sched = json.loads(sp.read_text())
    sched["legs"][2]["od_objects"] = []
    sp.write_text(json.dumps(sched))
    with pytest.raises(SystemExit, match="nothing to run"):
        rl.main(select_args(tmp_path, cp, sp, "03"))
    cp.write_text(json.dumps(candidates()[:-1]))
    with pytest.raises(SystemExit, match="different candidate list"):
        rl.main(select_args(tmp_path, cp, sp, "01"))


def test_check_accepts_a_whole_leg_and_refuses_each_way_it_can_fall_short(rl, sc, tmp_path):
    cands = candidates()
    rows = cands[:4]
    d = write_leg(tmp_path, "01", [result(c) for c in rows], [], ["A"])
    assert rl.check_leg(d, rows, ["A"]) == []
    # a selected row neither emitted nor dropped
    write_leg(tmp_path, "01", [result(c) for c in rows[:3]], [], ["A"])
    assert any("neither a result nor dropped" in p for p in rl.check_leg(d, rows, ["A"]))
    # the same row listed as dropped, with its reason, is accounted
    write_leg(tmp_path, "01", [result(c) for c in rows[:3]], [dict(rows[3], reason="refused")], ["A"])
    assert rl.check_leg(d, rows, ["A"]) == []
    write_leg(tmp_path, "01", [result(c) for c in rows[:3]], [dict(rows[3], reason="")], ["A"])
    assert any("no reason" in p for p in rl.check_leg(d, rows, ["A"]))
    # a row the leg was not given
    write_leg(tmp_path, "01", [result(c) for c in rows] + [result(cands[9])], [], ["A"])
    assert any("a result row the leg was not given" in p for p in rl.check_leg(d, rows, ["A"]))
    # OD rows for another object, and the sidecars missing
    write_leg(tmp_path, "01", [result(c) for c in rows], [], ["A", "B"])
    assert any("which this leg does not own" in p for p in rl.check_leg(d, rows, ["A"]))
    write_leg(tmp_path, "01", [result(c) for c in rows], [], ["A"])
    (d / "validation_rust_od_orbits.jsonl").unlink()
    assert any("validation_rust_od_orbits.jsonl: missing" in p for p in rl.check_leg(d, rows, ["A"]))
    # a file that is not there at all
    (d / "validation_rust_od.json").unlink()
    assert any("validation_rust_od.json: missing" in p for p in rl.check_leg(d, rows, ["A"]))
