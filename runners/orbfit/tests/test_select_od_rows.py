"""select_od_rows: one orbit-determination row per object, the untagged fit."""

import importlib.util
import pathlib

import pytest

RUNNER = pathlib.Path(__file__).resolve().parents[1] / "run_orbfit.py"


@pytest.fixture(scope="module")
def runner():
    spec = importlib.util.spec_from_file_location("run_orbfit", RUNNER)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def od(obj, tag):
    return {"object": obj, "test_type": "orbit_determination", "propagation_uncertainty": tag}


def test_one_untagged_row_per_object(runner):
    rows = [
        od("A", None), od("A", "first_order_detection_on"), od("A", "second_order_detection_on"),
        {"object": "A", "test_type": "propagation", "propagation_uncertainty": None},
        od("B", "auto_detection_on"), od("B", None),
    ]
    sel = runner.select_od_rows(rows)
    assert [(r["object"], r["propagation_uncertainty"]) for r in sel] == [("A", None), ("B", None)]


def test_object_without_an_untagged_row_keeps_its_first_fit(runner):
    rows = [od("C", "sigma_point_detection_on"), od("C", "first_order_detection_on")]
    sel = runner.select_od_rows(rows)
    assert len(sel) == 1 and sel[0]["propagation_uncertainty"] == "sigma_point_detection_on"


def test_no_od_rows_selects_nothing(runner):
    assert runner.select_od_rows([{"object": "D", "test_type": "ephemeris"}]) == []
