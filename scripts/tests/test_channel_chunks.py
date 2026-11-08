"""channel-chunks.py: balanced split of the object matrix, strict reassembly."""

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


def test_concat_joins_chunks_in_id_order(cc, tmp_path):
    (tmp_path / "validation_python.chunk02.json").write_text(json.dumps([{"r": 3}]))
    (tmp_path / "validation_python.chunk01.json").write_text(json.dumps([{"r": 1}, {"r": 2}]))
    out = tmp_path / "validation_python.json"
    n = cc.concat(sorted(tmp_path.glob("validation_python.chunk*.json"), reverse=True), 2, out)
    assert n == 3 and json.loads(out.read_text()) == [{"r": 1}, {"r": 2}, {"r": 3}]


def test_concat_refuses_a_missing_chunk_and_a_non_list(cc, tmp_path):
    (tmp_path / "validation_c.chunk01.json").write_text(json.dumps([]))
    with pytest.raises(SystemExit):
        cc.concat([tmp_path / "validation_c.chunk01.json"], 2, tmp_path / "validation_c.json")
    (tmp_path / "validation_c.chunk02.json").write_text(json.dumps({"rows": []}))
    with pytest.raises(SystemExit):
        cc.concat(sorted(tmp_path.glob("validation_c.chunk*.json")), 2, tmp_path / "validation_c.json")


def plan_rows():
    # Interleaved, the way the plan builder emits one object's grid after
    # another's, with a name that carries the characters real names do.
    names = ["Bennu", "1I/'Oumuamua", "Eris", "Bennu", "2024 PT5", "Eris", "1I/'Oumuamua", "Bennu"]
    return [{"object": n, "row": i} for i, n in enumerate(names)]


def test_filter_keeps_the_named_objects_in_plan_order(cc):
    rows = plan_rows()
    kept, counts = cc.filter_plan(rows, cc.parse_objects("Eris,Bennu"))
    assert counts == {"Eris": 2, "Bennu": 3}
    assert len(kept) == sum(counts.values())
    assert [r["row"] for r in kept] == [0, 2, 3, 5, 7]


def test_filter_refuses_an_object_with_no_rows_by_name(cc):
    with pytest.raises(SystemExit, match=r"no rows for 'Hektor'"):
        cc.filter_plan(plan_rows(), cc.parse_objects("Bennu,Hektor"))
    # Exact spelling: a case slip is an unknown object, not a looser match.
    with pytest.raises(SystemExit, match=r"no rows for 'bennu'"):
        cc.filter_plan(plan_rows(), cc.parse_objects("bennu"))


def test_filter_refuses_an_empty_list_and_a_malformed_one(cc):
    for spec in ("", " ", ",", " , "):
        with pytest.raises(SystemExit, match="is empty"):
            cc.parse_objects(spec)
    with pytest.raises(SystemExit, match="empty entry"):
        cc.parse_objects("Bennu,,Eris")
    with pytest.raises(SystemExit, match="Bennu more than once"):
        cc.parse_objects("Bennu,Eris,Bennu")


def test_filter_naming_every_object_reproduces_the_plan(cc, tmp_path):
    rows = plan_rows()
    plan = tmp_path / "validation_plan.json"
    plan.write_text(json.dumps(rows))
    out = tmp_path / "validation_plan.chunk.json"
    every = ",".join(dict.fromkeys(r["object"] for r in rows))
    assert cc.main(["filter", "--plan", str(plan), "--objects", every, "--output", str(out)]) == 0
    assert json.loads(out.read_text()) == rows


def test_filter_cli_prints_the_counts_and_writes_nothing_on_a_refusal(cc, tmp_path, capsys):
    plan = tmp_path / "validation_plan.json"
    plan.write_text(json.dumps(plan_rows()))
    out = tmp_path / "validation_plan.chunk.json"
    cc.main(["filter", "--plan", str(plan), "--objects", "1I/'Oumuamua,2024 PT5", "--output", str(out)])
    printed = capsys.readouterr().out
    assert "  1I/'Oumuamua: 2 rows" in printed and "  2024 PT5: 1 rows" in printed
    assert "Plan chunk: 3 rows for OBJECTS=1I/'Oumuamua,2024 PT5 (of 8 rows" in printed
    assert [r["row"] for r in json.loads(out.read_text())] == [1, 4, 6]
    refused = tmp_path / "refused.json"
    with pytest.raises(SystemExit):
        cc.main(["filter", "--plan", str(plan), "--objects", "Vesta", "--output", str(refused)])
    assert not refused.exists() and not (tmp_path / "refused.json.tmp").exists()
