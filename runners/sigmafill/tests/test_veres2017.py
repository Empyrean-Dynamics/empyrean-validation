"""Tests for the shared public Vereš et al. (2017) sigma-fill helper.

Pure tests (no venv, no adam-core): the helper is standalone. Run with any python:

    python -m pytest runners/sigmafill/tests -q
"""

from __future__ import annotations

import math
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
RUNNERS = HERE.parent.parent  # runners/
sys.path.insert(0, str(RUNNERS))

from sigmafill.veres2017 import (  # noqa: E402
    LEVELS,
    MODEL_NAME,
    Veres2017SigmaModel,
    fill_missing,
)

MODEL = Veres2017SigmaModel.from_csv()


# ── table integrity (transcription guards) ──────────────────


def test_table_row_counts():
    # 13 station rules (Tables 2-3), 46 catalog rules (45 named + UNK), no global row.
    assert len(MODEL.station) == 13
    assert len(MODEL.catalog) == 46
    assert not hasattr(MODEL, "global_sigma")
    assert LEVELS == ("station", "catalog", "no_catalog", "catalog_unrecognized")


def test_epoch_dependent_stations_use_latest_epoch():
    # Re-checked against the paper: latest epoch values. A flip to the earlier
    # epoch (703->1.0, 691->0.6, 644->0.6) must make these fail.
    assert MODEL.station["703"] == (0.8, 0.8)
    assert MODEL.station["691"] == (0.5, 0.5)
    assert MODEL.station["644"] == (0.4, 0.4)


def test_table3_station_values():
    for code, s in {"704": 1.0, "G96": 0.5, "F51": 0.2, "G45": 0.6, "699": 0.8,
                    "C51": 1.0, "E12": 0.75, "608": 0.6, "J75": 1.0, "D29": 0.75}.items():
        assert MODEL.station[code] == (s, s), code


# ── lookup precedence + levels ──────────────────────────────


def test_station_rule_overrides_catalog():
    # A station rule wins over the generic catalog rule.
    sra, sdec, level = MODEL.lookup("703", "Gaia2")
    assert (sra, sdec, level) == (0.8, 0.8, "station")


def test_known_catalog_level():
    sra, sdec, level = MODEL.lookup("XYZ", "Gaia2")  # unknown station, known catalog
    assert (sra, sdec, level) == (1.0, 1.0, "catalog")


def test_empty_catalog_is_no_catalog_level():
    # Missing/empty/UNK star catalog -> the paper's unknown-catalog weight (1.5"),
    # labelled no_catalog (not "catalog").
    for cat in ("", None, "UNK"):
        assert MODEL.lookup("XYZ", cat) == (1.5, 1.5, "no_catalog"), cat


def test_unrecognized_catalog_is_conservative_and_labelled():
    # A non-empty code outside the table: the paper is silent, so UNK weight (1.5"),
    # labelled catalog_unrecognized -- NEVER the old 0.75" global.
    assert MODEL.lookup("XYZ", "FOOCAT-v9") == (1.5, 1.5, "catalog_unrecognized")


def test_table_has_no_global_row():
    text = Path(MODEL.source).read_text()
    data = [ln for ln in text.splitlines() if ln and not ln.startswith("#")][1:]
    assert not any(ln.startswith(",,") for ln in data)
    assert "0.75,0.75" not in "\n".join(ln for ln in data if ln.startswith(","))


# ── fill_missing: labelling + reported never overwritten ────


def test_reported_sigma_never_overwritten():
    # Row 0 is reported (0.3"); it must pass through untouched and count as reported.
    obs = ["703", "703"]
    cats = ["Gaia2", "Gaia2"]
    ra = [0.3, float("nan")]
    dec = [0.3, None]
    fra, fdec, rep = fill_missing(obs, cats, ra, dec, MODEL)
    assert fra[0] == 0.3 and fdec[0] == 0.3          # reported, untouched
    assert fra[1] == 0.8 and fdec[1] == 0.8          # filled from station 703
    assert rep["reported"] == 1 and rep["filled"] == 1
    assert rep["filled_by_level"]["station"] == 1
    assert rep["model"] == MODEL_NAME and rep["total"] == 2


def test_fill_levels_counted():
    obs = ["703", "ZZZ", "ZZZ", "ZZZ"]
    cats = ["Gaia2", "Gaia2", "", "FOOCAT"]
    ra = [math.nan] * 4
    dec = [math.nan] * 4
    _fra, _fdec, rep = fill_missing(obs, cats, ra, dec, MODEL)
    assert rep["filled_by_level"] == {
        "station": 1, "catalog": 1, "no_catalog": 1, "catalog_unrecognized": 1}
    assert rep["filled"] == 4 and rep["reported"] == 0
    assert len(rep["table_sha256"]) == 64


def test_nonpositive_sigma_is_treated_as_missing():
    # A zero or negative reported sigma is not a valid uncertainty -> filled.
    _fra, _fdec, rep = fill_missing(["F51"], ["Gaia2"], [0.0], [-1.0], MODEL)
    assert rep["reported"] == 0 and rep["filled"] == 1
    assert rep["filled_by_level"]["station"] == 1
