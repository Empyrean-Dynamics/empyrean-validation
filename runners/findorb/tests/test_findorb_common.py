"""Tests for the find_orb settings module and the two runners that share it.

Pure Python (stdlib + pytest); no find_orb binary needed:

    runners/adam-core/.venv/bin/python -m pytest runners/findorb/tests -q
"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

import pytest

RUNNER = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(RUNNER))

import findorb_common as FC  # noqa: E402
import run_findorb as OD  # noqa: E402
import run_findorb_walk as WALK  # noqa: E402


def _data_dir(tmp_path: Path, *, bc405=True, bias=True, jpl=True, bc405_size=None) -> Path:
    d = tmp_path / "findorb"
    d.mkdir()
    if bc405:
        with open(d / FC.BC405_DAT, "wb") as f:
            f.truncate(FC.BC405_DAT_BYTES if bc405_size is None else bc405_size)
    if bias:
        (d / FC.BIAS_DAT).write_bytes(b"! BIAS_VERSION= 3.0\n")
    if jpl:
        (d / FC.JPL_DE_FILE).write_bytes(b"de")
    return d


def _kv(lines):
    return dict(ln.split("=", 1) for ln in lines)


# ── the documented settings ─────────────────────────────────


def test_environ_follows_find_orbs_documented_defaults():
    kv = _kv(FC.environ_lines(debiasing=True))
    assert kv["ENCKE"] == "0"  # Cowell: environ.def advises leaving it at zero
    assert kv["ASTEROID_PERT_LIST"] == ""  # find_orb default: all 300, culled by proximity
    assert kv["BC405_ASTEROIDS"] == "300" and kv["ASTEROID_THRESH"] == "10"
    assert kv["OUTLIER_REJECTION_LIMIT"] == "3"
    assert kv["PERTURBERS"] == "1007fe"
    assert kv["SETTINGS2"] == "1 22.00 0 -2 1"  # last field: EFCC18 debiasing on


def test_debiasing_flag_is_the_last_settings2_field_only():
    on = _kv(FC.environ_lines(debiasing=True))["SETTINGS2"].split()
    off = _kv(FC.environ_lines(debiasing=False))["SETTINGS2"].split()
    assert on[:-1] == off[:-1] and (on[-1], off[-1]) == ("1", "0")


def test_an_explicit_pert_list_is_written_verbatim_for_deliberate_replication():
    kv = _kv(FC.environ_lines(debiasing=False, pert_list="1,2,3"))
    assert kv["ASTEROID_PERT_LIST"] == "1,2,3"


# ── staged data and refusals ────────────────────────────────


def test_stage_links_registered_files_and_does_not_preseed_bc405pre(tmp_path):
    d = _data_dir(tmp_path)
    cfg = FC.stage_cfg_dir(tmp_path / "w", d, debiasing=True)
    assert (cfg / "bc405.dat").resolve() == (d / "bc405.dat").resolve()
    assert (cfg / "bias.dat").resolve() == (d / "bias.dat").resolve()
    assert not (cfg / "bc405pre.dat").exists()  # find_orb makes its own, correctly sized


@pytest.mark.parametrize(
    "kw,needle",
    [
        (dict(bc405=False), "bc405.dat is missing"),
        (dict(bc405_size=1234), "1234 bytes"),
        (dict(bias=False), "bias.dat is missing"),
        (dict(jpl=False), FC.JPL_DE_FILE),
    ],
)
def test_stage_refuses_each_missing_or_wrong_input_by_name(tmp_path, kw, needle):
    d = _data_dir(tmp_path, **kw)
    with pytest.raises(SystemExit) as e:
        FC.stage_cfg_dir(tmp_path / "w", d, debiasing=True)
    assert needle in str(e.value)


def test_stage_refuses_without_a_data_directory(tmp_path):
    with pytest.raises(SystemExit) as e:
        FC.stage_cfg_dir(tmp_path / "w", None, debiasing=True)
    assert "--data-dir" in str(e.value)


def test_planets_only_run_needs_no_bc405_and_debiasing_off_needs_no_bias(tmp_path):
    d = _data_dir(tmp_path, bc405=False, bias=False)
    cfg = FC.stage_cfg_dir(tmp_path / "w", d, debiasing=False, asteroids=False)
    assert not (cfg / "bc405.dat").exists() and not (cfg / "bias.dat").exists()


# ── the two runners agree ───────────────────────────────────


def test_od_and_walk_write_the_same_settings_except_debiasing_and_ephemeris_epochs(tmp_path):
    d = _data_dir(tmp_path)
    od_dir, walk_dir = tmp_path / "od", tmp_path / "walk"
    od_dir.mkdir()
    walk_dir.mkdir()
    OD.populate_fo_directory(str(od_dir), d)
    WALK._write_environ(walk_dir, 3, "times.txt", FC.PERTURBERS, "")
    od = _kv((od_dir / "environ.dat").read_text().splitlines())
    walk = _kv((walk_dir / "environ.dat").read_text().splitlines())
    only_walk = {"EPHEM_STEPS", "TT_EPHEMERIS"}
    assert set(walk) - set(od) == only_walk
    for k in set(od):
        if k == "SETTINGS2":
            assert od[k].split()[:-1] == walk[k].split()[:-1]
            assert (od[k].split()[-1], walk[k].split()[-1]) == ("1", "0")
        else:
            assert od[k] == walk[k], k
    assert od["ENCKE"] == "0" and od["ASTEROID_PERT_LIST"] == ""


def test_the_od_runner_refuses_to_start_without_the_registered_bc405(tmp_path):
    psv = tmp_path / "psv"
    psv.mkdir()
    (psv / "x.psv").write_text("# version=2017\n")
    d = _data_dir(tmp_path, bc405=False)
    r = subprocess.run(
        [sys.executable, str(RUNNER / "run_findorb.py"), str(psv), "--data-dir", str(d),
         "--output", str(tmp_path / "o.json")],
        capture_output=True, text=True,
    )
    assert r.returncode != 0
    assert "bc405.dat is missing" in (r.stderr + r.stdout)


def test_the_od_runner_refuses_without_bias_dat_because_debiasing_is_on(tmp_path):
    psv = tmp_path / "psv"
    psv.mkdir()
    (psv / "x.psv").write_text("# version=2017\n")
    d = _data_dir(tmp_path, bias=False)
    r = subprocess.run(
        [sys.executable, str(RUNNER / "run_findorb.py"), str(psv), "--data-dir", str(d),
         "--output", str(tmp_path / "o.json")],
        capture_output=True, text=True,
    )
    assert r.returncode != 0 and "bias.dat is missing" in (r.stderr + r.stdout)


# ── what the row records ────────────────────────────────────


def test_settings_record_states_the_model_difference_and_the_data_by_hash(tmp_path):
    d = _data_dir(tmp_path)
    rec = FC.settings_record(d, debiasing=True)
    assert "BC-405 (300, proximity-culled), not SB441-N16" in rec["model_differences"]
    assert rec["integrator"].startswith("Cowell")
    assert "EFCC18" in rec["debiasing"] and "bias_dat" in rec
    assert FC.sha256_file(d / FC.BC405_DAT) in rec["bc405_dat"]
    walk = FC.settings_record(d, debiasing=False)
    assert "bias_dat" not in walk and "centrally" in walk["debiasing"]
