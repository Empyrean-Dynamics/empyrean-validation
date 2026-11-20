"""Tests for the JSON front end to the shared sigma fill (fill_json.py), the path
the adam-core Rust API runner takes to the same fill the Python runners import.

    python -m pytest runners/sigmafill/tests -q
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
RUNNERS = HERE.parent.parent
sys.path.insert(0, str(RUNNERS))

from sigmafill.fill_json import fill_request
from sigmafill.veres2017 import Veres2017SigmaModel, fill_missing

SCRIPT = HERE.parent / "fill_json.py"
MODEL = Veres2017SigmaModel.from_csv()
REQ = {
    "stn": ["F51", "F51", "XXX", "703"],
    "astcat": ["Gaia2", "Gaia2", None, "UCAC4"],
    "rms_ra": [0.12, None, None, 0.5],
    "rms_dec": [0.11, None, 0.3, None],
}


def test_the_bridge_returns_exactly_what_fill_missing_returns():
    ra, dec, report = fill_missing(REQ["stn"], REQ["astcat"], REQ["rms_ra"], REQ["rms_dec"], MODEL)
    out = fill_request(REQ, MODEL)
    assert out == {"ra_sigma": ra, "dec_sigma": dec, "report": report}


def test_a_reported_sigma_passes_through_and_a_missing_half_is_filled():
    out = fill_request(REQ, MODEL)
    assert out["ra_sigma"][0] == 0.12 and out["dec_sigma"][0] == 0.11
    assert out["dec_sigma"][2] == 0.3, "the reported Dec half is kept"
    assert out["ra_sigma"][3] == 0.5, "the reported RA half is kept"
    r = out["report"]
    assert (r["reported"], r["filled"], r["partially_reported"], r["total"]) == (1, 3, 2, 4)


def test_the_script_round_trips_over_stdin_and_stdout():
    cp = subprocess.run([sys.executable, "-I", str(SCRIPT)], input=json.dumps(REQ),
                        capture_output=True, text=True, check=True)
    assert json.loads(cp.stdout) == fill_request(REQ, MODEL)


def test_a_malformed_request_exits_2_naming_the_defect_and_writes_nothing():
    bad = dict(REQ, rms_dec=[0.1])
    cp = subprocess.run([sys.executable, "-I", str(SCRIPT)], input=json.dumps(bad),
                        capture_output=True, text=True, check=False)
    assert cp.returncode == 2 and cp.stdout == ""
    assert "rms_dec must have the length of stn (4)" in cp.stderr
    cp = subprocess.run([sys.executable, "-I", str(SCRIPT)], input='{"stn": []}',
                        capture_output=True, text=True, check=False)
    assert cp.returncode == 2 and "lacks astcat, rms_ra, rms_dec" in cp.stderr
