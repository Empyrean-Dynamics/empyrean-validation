"""Tests for the find_orb settings module and the two runners that share it.

Pure Python (stdlib + pytest); no find_orb binary needed:

    runners/adam-core/.venv/bin/python -m pytest runners/findorb/tests -q
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest

RUNNER = Path(__file__).resolve().parent.parent
REPO = RUNNER.parent.parent
MPC_FMT = RUNNER / "build" / "lunar" / "mpc_fmt.cpp"  # find_orb's catalog-name table (setup.sh)
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


def test_a_crunched_bias_table_in_the_home_config_dir_is_refused_by_name(tmp_path, monkeypatch):
    """find_orb reads biases.bin from its config directory before bias.dat and falls
    back to ~/.find_orb when the private directory has none (its first run)."""
    home = tmp_path / "home"
    (home / ".find_orb").mkdir(parents=True)
    (home / ".find_orb" / "cospar.txt").write_text("x")
    monkeypatch.setattr(Path, "home", classmethod(lambda cls: home))
    d = _data_dir(tmp_path)
    FC.stage_cfg_dir(tmp_path / "w1", d, debiasing=True)  # no biases.bin: staged
    (home / ".find_orb" / "biases.bin").write_bytes(b"\x1a")
    with pytest.raises(SystemExit) as e:
        FC.stage_cfg_dir(tmp_path / "w2", d, debiasing=True)
    assert str(home / ".find_orb" / "biases.bin") in str(e.value)
    FC.stage_cfg_dir(tmp_path / "w3", d, debiasing=False)  # undebiased runs never read it


# ── the two runners agree ───────────────────────────────────


def test_od_and_walk_write_the_same_settings_except_ephemeris_epochs(tmp_path):
    d = _data_dir(tmp_path)
    od_dir, walk_dir = tmp_path / "od", tmp_path / "walk"
    od_dir.mkdir()
    walk_dir.mkdir()
    OD.populate_fo_directory(str(od_dir), d)
    WALK._write_environ(walk_dir, 3, "times.txt", FC.PERTURBERS, "",
                        debiasing=WALK.DEBIAS_AXIS[WALK.FIT_DEBIAS])
    od = _kv((od_dir / "environ.dat").read_text().splitlines())
    walk = _kv((walk_dir / "environ.dat").read_text().splitlines())
    only_walk = {"EPHEM_STEPS"}
    assert set(walk) - set(od) == only_walk
    for k in set(od):
        assert od[k] == walk[k], k  # SETTINGS2 included: both debias with EFCC18
    assert walk["SETTINGS2"].split()[-1] == "1"
    assert od["ENCKE"] == "0" and od["ASTEROID_PERT_LIST"] == ""


# ── the walk's ephemeris dates come back UTC and are converted to TT ────

ENVIRON_DEF = RUNNER / "build" / "find_orb" / "environ.def"  # find_orb's documented settings
EPHEM0 = RUNNER / "build" / "find_orb" / "ephem0.cpp"
LSK = os.environ.get("NAIF_LSK")  # the registered naif0012.tls (layup/ folder of the tool set)


def test_neither_runner_sets_a_time_scale_keyword(tmp_path):
    """TT_EPHEMERIS is no keyword of find_orb's, and its real one
    (EPHEMERIS_TIMESCALE=TT) breaks total.json at the pinned commit, so the
    runners set neither and the walk converts the UTC dates itself."""
    walk_dir = tmp_path / "walk"
    walk_dir.mkdir()
    WALK._write_environ(walk_dir, 3, "times.txt", FC.PERTURBERS, "", debiasing=True)
    text = (walk_dir / "environ.dat").read_text()
    assert "TT_EPHEMERIS" not in text and "EPHEMERIS_TIMESCALE" not in text
    for src in ("run_findorb.py", "run_findorb_walk.py"):
        code = (RUNNER / src).read_text()
        assert "TT_EPHEMERIS=" not in code and "EPHEMERIS_TIMESCALE=TT\\n" not in code, src


def test_the_walk_dates_each_ephemeris_row_in_tt_from_find_orbs_utc(tmp_path):
    """A row find_orb dates 2024-03-08 09:36:18.144 UTC is the TT instant
    69.184 s later (37 leap seconds + 32.184 s), and the epoch check compares that
    with the TT epoch find_orb was handed."""
    mjd_utc = 60377.400210000
    jd_utc = mjd_utc + 2400000.5
    eph = tmp_path / "eph_304.txt"
    eph.write_text(f"#(304) x\n{jd_utc:.6f} 264.31462779 -21.37828213 3.10 2.20 120.0 0.0266 266.48\n")
    rows = WALK._parse_ephem_file(eph)
    assert isinstance(rows, list) and len(rows) == 1, rows
    assert rows[0]["jd_utc"] == pytest.approx(jd_utc, abs=1e-9)
    assert rows[0]["jd_tt"] == pytest.approx(jd_utc + 69.184 / 86400.0, abs=1e-9)
    assert abs(rows[0]["jd_tt"] - WALK.mjd_utc_to_jd_tt(mjd_utc)) < 1e-6  # within the walk's epoch check


@pytest.mark.skipif(not ENVIRON_DEF.is_file(), reason="find_orb source not built (runners/findorb/setup.sh)")
def test_find_orbs_time_scale_keyword_is_ephemeris_timescale_not_tt_ephemeris():
    environ, ephem = ENVIRON_DEF.read_text(), EPHEM0.read_text()
    assert "\nEPHEMERIS_TIMESCALE=" in environ
    assert 'get_environment_ptr( "EPHEMERIS_TIMESCALE")' in ephem
    assert "TT_EPHEMERIS" not in environ and "TT_EPHEMERIS" not in ephem
    # The JSON ephemeris writer keys the token at header offset 5 as the ISO date,
    # which the TT header's extra token breaks (why the runners leave it unset).
    assert "if( hptr == header + 5)   /* second token -> ISO date */" in ephem
    assert '(*timescale == \'T\' ? "(TT) "  : "(UTC)")' in ephem


@pytest.mark.skipif(not LSK, reason="set NAIF_LSK to the registered naif0012.tls")
def test_the_walks_leap_second_table_is_the_registered_lsk():
    text = Path(LSK).read_text()
    block = re.search(r"DELTA_AT\s*=\s*\((.*?)\)", text, re.DOTALL).group(1)
    pairs = re.findall(r"([\d.]+),\s*@(\d{4})-([A-Z]{3})-(\d+)", block)
    months = {m: i for i, m in enumerate("JAN FEB MAR APR MAY JUN JUL AUG SEP OCT NOV DEC".split(), 1)}

    def mjd(y, m, d):  # civil date to MJD (Fliegel and Van Flandern)
        a = (14 - m) // 12
        yy, mm = y + 4800 - a, m + 12 * a - 3
        jdn = d + (153 * mm + 2) // 5 + 365 * yy + yy // 4 - yy // 100 + yy // 400 - 32045
        return jdn - 2400001

    lsk = [(mjd(int(y), months[mo], int(d)), float(s)) for s, y, mo, d in pairs]
    assert lsk == [(m, s) for m, s in WALK.LEAP_SECONDS]


# ── the walk's fit debiases as the reference walk's does ────


def _walk_rs_default_debias() -> str:
    rs = (REPO / "runners" / "rust" / "src" / "walk.rs").read_text()
    default = re.search(r'fn default_debias\(\) -> String \{\s*"(\w+)"\.into\(\)', rs).group(1)
    builtin = rs[rs.index("pub fn builtin_arms()"):]
    arm = re.search(r'name: "default"\.into\(\),.*?debias: "(\w+)"\.into\(\)', builtin, re.DOTALL).group(1)
    assert default == arm, "the reference's default arm and its axis default disagree"
    return default


def test_the_walk_runs_the_reference_walks_fit_side_debias():
    assert WALK.FIT_DEBIAS == _walk_rs_default_debias() == "efcc"
    assert WALK.DEBIAS_AXIS == {"efcc": True, "off": False}
    assert WALK.CONFIG_ARM == "default"  # the reference arm whose debias it mirrors


def _manifest_object(name: str) -> dict:
    obs = [{"stn": "703", "obs_time": f"2020-01-0{i}T00:00:00.000Z", "mjd_utc": 58849.0 + i,
            "excluded": None} for i in range(1, 5)]
    return {"object": name, "mpc_designation": name, "observations": obs, "windows": [
        {"index": 0, "profiles": ["ladder"], "cut_utc": "2020-01-04T12:00:00Z", "n_obs_fit": 3,
         "targets": [{"obs_idx": 3}], "in_sample": [0]}]}


ADES_HEADER = "permID|stn|obsTime|ra|dec|astCat\n"


def _fixture(tmp_path: Path, name: str, cats=("Tyc2", "Gaia2", "UNK", "Tyc2"), header=ADES_HEADER) -> Path:
    fx = tmp_path / "fx"
    fx.mkdir(exist_ok=True)
    rows = "".join(f"433|703|2020-01-0{i}T00:00:00.000Z|10.0|5.0|{c}\n" for i, c in enumerate(cats, 1))
    if "astCat" not in header:
        rows = "".join(r.rsplit("|", 1)[0] + "\n" for r in rows.splitlines())
    (fx / f"{name}.psv").write_text("# version=2017\n" + header + rows)
    return fx


def _task(tmp_path: Path, fx: Path, name: str, debiasing: bool) -> dict:
    return {"obj": _manifest_object(name), "fixtures_dir": str(fx), "profile": "ladder",
            "fo_bin": "/nonexistent/fo", "jpl": None, "data_dir": str(_data_dir(tmp_path)),
            "timeout": 5.0, "perturbers": FC.PERTURBERS, "warm_start": False,
            "max_reduced_chi2": 100.0, "asteroid_pert_list": "", "mc_samples": 0,
            "debiasing": debiasing}


def test_the_walks_fit_stages_bias_dat_and_turns_find_orbs_debiasing_on(tmp_path, monkeypatch):
    """The window find_orb fits gets SETTINGS2 ... 1, the staged bias.dat, and
    find_orb's spelling of Tyc2, so every Tycho-2 observation can be debiased."""
    seen = {}

    def fake_fo(work, fo_bin, obs_name, codes, warm, timeout, cfg_dir):
        seen["environ"] = _kv((work / "environ.dat").read_text().splitlines())
        seen["ades"] = (work / obs_name).read_text()
        seen["bias"] = (cfg_dir / FC.BIAS_DAT).is_symlink()
        return WALK.FitOutcome(None, None, None, "stopped by the test", 1.0)

    monkeypatch.setattr(WALK, "_run_fo", fake_fo)
    fx = _fixture(tmp_path, "Eros")
    wins, _preds, stats = WALK._walk_object(_task(tmp_path, fx, "Eros", True))
    assert stats["error"] is None and wins[0]["failure"] == "stopped by the test"
    assert seen["environ"]["SETTINGS2"].split()[-1] == "1" and seen["bias"]
    cats = [ln.rsplit("|", 1)[1] for ln in seen["ades"].splitlines()[2:]]
    assert cats == ["Tycho-2", "Gaia2", "UNK"]  # the fit set: 3 rows before the cut


def test_the_walk_run_debiases_every_window_and_records_it(tmp_path, monkeypatch):
    """main(): the axis reaches every window's environ, and the settings sidecar
    beside the records says EFCC18 by find_orb, from the reference's efcc."""
    environs = []

    def fake_fo(work, fo_bin, obs_name, codes, warm, timeout, cfg_dir):
        environs.append(_kv((work / "environ.dat").read_text().splitlines()))
        return WALK.FitOutcome(None, None, None, "stopped by the test", 1.0)

    monkeypatch.setattr(WALK, "_run_fo", fake_fo)
    fx = _fixture(tmp_path, "Eros")
    obj = {**_manifest_object("Eros"), "eligible": True}
    (tmp_path / "windows.json").write_text(json.dumps({"objects": [obj]}))
    fo = tmp_path / "fo"
    fo.write_text("#!/bin/sh\n")
    fo.chmod(0o755)
    out = tmp_path / "res" / "walk.json"
    monkeypatch.setattr(sys, "argv", [
        "run_findorb_walk.py", "--manifest", str(tmp_path / "windows.json"), "--fixtures-dir",
        str(fx), "--fo-binary", str(fo), "--data-dir", str(_data_dir(tmp_path)), "--output",
        str(out), "--workers", "1", "--mc-samples", "0"])
    assert WALK.main() == 0
    assert [e["SETTINGS2"].split()[-1] for e in environs] == ["1"]
    rec = json.loads((tmp_path / "res" / "walk_settings.json").read_text())
    assert rec["fo_settings"]["debiasing"].startswith("EFCC18 by find_orb")
    assert rec["fo_settings"]["fit_debias_axis"].startswith("efcc")


def test_a_fixture_without_ast_cat_is_refused_by_name_when_debiasing(tmp_path, monkeypatch):
    monkeypatch.setattr(WALK, "_run_fo", lambda *a: pytest.fail("must not fit"))
    fx = _fixture(tmp_path, "Eros", header="permID|stn|obsTime|ra|dec\n")
    wins, _preds, stats = WALK._walk_object(_task(tmp_path, fx, "Eros", True))
    assert wins == [] and "no astCat column" in stats["error"]


def test_the_catalog_spelling_rewrites_only_the_named_column(tmp_path):
    line = " 433|703 |Tyc2|  Tyc2|x"
    assert FC.findorb_catalog_spelling(line, 3) == " 433|703 |Tyc2|  Tycho-2|x"
    assert FC.findorb_catalog_spelling(" 433|703|Gaia2", 2) == " 433|703|Gaia2"
    assert FC.findorb_catalog_spelling("short", 5) == "short"


def test_the_od_runner_gives_find_orb_its_catalog_spelling_before_the_fit():
    """find_orb's OD debiases every fit; a Tyc2 observation its reader misses would
    go undebiased without a word, so the OD sanitizer spells it Tycho-2 (and every
    other missed name its way), in the astCat column only."""
    psv = (
        "# version=2017\n"
        "permID|stn |obsTime                 |ra        |dec       |astCat|pos1  |rmsRA\n"
        "  433 |703 |2004-03-15T02:35:21.696Z|10.0      |20.0      |Tyc2  |      |0.5\n"
        "  433 |C51 |2014-03-15T02:35:21.696Z|10.0      |20.0      |  GSC |356814|Tyc2\n"
        "  433 |G96 |2020-03-15T02:35:21.696Z|10.0      |20.0      |Gaia2 |      |0.1\n"
    )
    out = OD.sanitize_psv(psv).splitlines()
    assert out[2].split("|")[5].strip() == "Tycho-2"
    cols = out[3].split("|")
    assert cols[5].strip() == "GSC-?" and cols[6] == "356814.0" and cols[7] == "Tyc2"
    assert out[4].split("|")[5].strip() == "Gaia2"
    assert out[:2] == psv.splitlines()[:2]


def test_the_od_runner_refuses_a_fixture_without_ast_cat_by_name():
    psv = "# version=2017\npermID|stn|obsTime|ra|dec\n433|703|2004-03-15T02:35:21Z|1|2\n"
    with pytest.raises(OD.FixtureError, match="no astCat column"):
        OD.sanitize_psv(psv)


def _find_orb_code(table: str, name: str) -> str:
    """lunar/mpc_fmt.cpp net_name_to_byte_code, reimplemented: drop '-', ' ', '.0'
    and 'DR', upper-case, and take the LAST matching entry (it does), else '?'."""
    def reduce(s):
        out, i = [], 0
        while i < len(s):
            if s[i] in "- ":
                i += 1
            elif s[i:i + 2] in (".0", "DR"):
                i += 2
            else:
                out.append(s[i].upper())
                i += 1
        return "".join(out)
    block = table[table.index("static const char *net_codes[] = {"):]
    block = block[:block.index("NULL };")]
    code = "?"
    for c, n in re.findall(r'^\s*"(.)(.*?)",', block, re.MULTILINE):
        if reduce(n) == reduce(name):
            code = c
    return code


@pytest.mark.skipif(not MPC_FMT.is_file(), reason="find_orb source not built (runners/findorb/setup.sh)")
def test_every_mpc_name_find_orbs_reader_misses_is_given_its_spelling_and_no_other():
    """Against find_orb's own table at the pinned commit and the suite's MPC table
    (src/debias.rs): the map holds exactly the ADES names find_orb reads as another
    code, and each spelling reads as the MPC code (within find_orb's 40-byte field)."""
    table = MPC_FMT.read_text()
    rs = (REPO / "src" / "debias.rs").read_text()
    fn = rs[rs.index("fn ades_to_mpc_code"):]
    mpc = dict(re.findall(r'"([^"]+)" => \'(.)\'', fn[:fn.index("_ => return None")]))
    misread = {a for a, c in mpc.items() if _find_orb_code(table, a) != c}
    assert misread == set(FC.ADES_CATALOG_SPELLING)
    for ades, spelling in FC.ADES_CATALOG_SPELLING.items():
        assert _find_orb_code(table, spelling) == mpc[ades], ades
        assert len(spelling) < 40


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
    off = FC.settings_record(d, debiasing=False)
    assert "bias_dat" not in off and off["debiasing"].startswith("off")
    assert "central" not in off["debiasing"]  # nothing debiases a fit centrally


def test_the_walk_records_its_debias_setting_beside_its_records(tmp_path):
    d = _data_dir(tmp_path)
    args = SimpleNamespace(asteroid_pert_list="", perturbers=FC.PERTURBERS, warm_start=False,
                           max_reduced_chi2=100.0, mc_samples=200)
    rec = WALK.walk_settings(Path("/nonexistent/fo"), d, True, args)
    assert rec["tool"] == "findorb" and rec["fo_version"].startswith("find_orb ")
    s = rec["fo_settings"]
    assert s["debiasing"].startswith("EFCC18 by find_orb") and "bias_dat" in s
    assert s["fit_debias_axis"].startswith("efcc") and "Tyc2 -> Tycho-2" in s["catalog_names"]
    json.dumps(rec)  # it is written as JSON
    assert all(isinstance(v, str) for v in s.values())  # merge-external's settings are strings


@pytest.mark.skipif(not (RUNNER / "build" / "find_orb" / "fo.cpp").is_file(),
                    reason="find_orb source not built (runners/findorb/setup.sh)")
def test_the_walk_reads_find_orbs_commit_not_this_repositorys():
    want = subprocess.run(["git", "-C", str(RUNNER / "build" / "find_orb"), "rev-parse", "--short",
                           "HEAD"], capture_output=True, text=True, check=True).stdout.strip()
    assert WALK._findorb_version(RUNNER / "install" / "bin" / "fo") == want
    assert WALK._findorb_version(RUNNER / "run_findorb.py").startswith("unknown (")
