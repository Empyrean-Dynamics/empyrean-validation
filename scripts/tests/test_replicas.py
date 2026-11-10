"""replicas.py: fold a row's timing replicas into one row, or fail by name."""

import importlib.util
import pathlib

import pytest

SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "replicas.py"


@pytest.fixture(scope="module")
def rp():
    spec = importlib.util.spec_from_file_location("replicas_under_test", SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def base(**kw):
    r = {
        "object": "2020 CD3",
        "test_type": "propagation",
        "emp_pos_au": [0.1, 0.2, 0.3],
        "emp_pos_cov_au2": [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        "notes": "n",
        "timestamp": "2026-11-09T00:00:00+00:00",
        "emp_time_ms": 2500000.0,
        "speed_ratio": None,
    }
    r.update(kw)
    return r


def test_fold_keeps_the_minimum_time_and_the_first_replicas_other_fields(rp):
    a = base(emp_time_ms=2510000.0, timestamp="t1")
    b = base(emp_time_ms=2503800.0, timestamp="t2")
    c = base(emp_time_ms=2517000.0, timestamp="t3")
    out = rp.fold("row", [("01", a), ("02", b), ("03", c)])
    assert out["emp_time_ms"] == 2503800.0
    assert out["timestamp"] == "t1"  # leg order, not finish order
    assert out["emp_pos_au"] == a["emp_pos_au"]


def test_fold_does_not_mutate_its_inputs(rp):
    a, b = base(emp_time_ms=5.0), base(emp_time_ms=3.0)
    rp.fold("row", [("01", a), ("02", b)])
    assert a["emp_time_ms"] == 5.0


def test_fold_names_the_row_the_field_and_both_legs_on_any_disagreement(rp):
    a = base()
    b = base(notes="different")
    with pytest.raises(rp.FoldError) as e:
        rp.fold("2020 CD3|propagation", [("01", a), ("02", b)])
    msg = str(e.value)
    assert "2020 CD3|propagation" in msg and "`notes`" in msg and "01" in msg and "02" in msg


def test_fold_catches_a_one_ulp_difference_in_a_float_array(rp):
    import math

    a = base()
    b = base(emp_pos_au=[0.1, math.nextafter(0.2, 1.0), 0.3])
    with pytest.raises(rp.FoldError, match="emp_pos_au"):
        rp.fold("row", [("01", a), ("02", b)])


def test_fold_compares_every_field_not_only_the_position(rp):
    a = base()
    b = base(emp_pos_cov_au2=[[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 2.0]])
    with pytest.raises(rp.FoldError, match="emp_pos_cov_au2"):
        rp.fold("row", [("01", a), ("02", b)])


def test_fold_refuses_a_field_present_in_one_replica_only(rp):
    a = base()
    b = base()
    b["extra"] = 1
    with pytest.raises(rp.FoldError, match="extra"):
        rp.fold("row", [("01", a), ("02", b)])


def test_volatile_fields_are_ignored_by_the_comparison(rp):
    a = base(timestamp="t1", emp_time_ms=1.0, speed_ratio=1.0)
    b = base(timestamp="t2", emp_time_ms=2.0, speed_ratio=2.0)
    assert rp.first_difference(a, b) is None
    assert rp.is_volatile("assist_time_ms") and not rp.is_volatile("emp_pos_au")


def test_fold_refuses_replicas_where_only_some_carry_a_time(rp):
    with pytest.raises(rp.FoldError, match="some replicas carry emp_time_ms"):
        rp.fold("row", [("01", base()), ("02", base(emp_time_ms=None))])


def test_fold_of_a_single_replica_is_that_row_and_of_none_is_refused(rp):
    a = base()
    assert rp.fold("row", [("01", a)]) == a
    with pytest.raises(rp.FoldError):
        rp.fold("row", [])
