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
