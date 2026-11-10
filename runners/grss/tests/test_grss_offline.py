"""run_grss: every file GRSS opens comes from the third-party tool set, checked by name.

    GRSS_DATA_DIR=<third-party>/grss runners/grss/.venv/bin/python -m pytest runners/grss/tests -q
"""

import importlib.util
import os
import pathlib
import time

import pytest

RUNNER = pathlib.Path(__file__).resolve().parents[1] / "run_grss.py"


@pytest.fixture(scope="module")
def runner():
    spec = importlib.util.spec_from_file_location("run_grss_offline", RUNNER)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def _package(tmp_path, runner, skip=(), empty=()):
    for rel in runner._ASTROMETRY_TABLES:
        if rel in skip:
            continue
        p = tmp_path / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text("" if rel in empty else "x")
    return tmp_path


def test_every_table_present_passes(runner, tmp_path):
    runner._check_astrometry_tables(_package(tmp_path, runner))


@pytest.mark.parametrize(
    "rel", ["debias/lowres_data/bias.dat", "fit/codes.json", "fit/special_codes.json"]
)
def test_each_missing_or_empty_table_is_named(runner, tmp_path, rel):
    with pytest.raises(SystemExit, match=f"{rel} \\(absent\\)"):
        runner._check_astrometry_tables(_package(tmp_path / "a", runner, skip=(rel,)))
    with pytest.raises(SystemExit, match=f"{rel} \\(zero bytes\\)"):
        runner._check_astrometry_tables(_package(tmp_path / "b", runner, empty=(rel,)))


def test_grss_never_opens_pck00011_so_it_is_not_required(runner):
    required = set(runner._KERNELS_COMMON) | set(runner._KERNELS_BY_DE[440])
    assert "pck00011.tpc" not in required
    assert required == {"earth_latest.bpc", "earth_historic.bpc", "earth_predict.bpc",
                        "moon_pa_de440.bpc", "de440.bsp", "sb441-n16s.bsp"}


@pytest.mark.skipif(not os.environ.get("GRSS_DATA_DIR"), reason="set GRSS_DATA_DIR (grss venv, setup.sh run)")
def test_the_staged_tables_are_the_folders_bytes_and_only_codes_json_is_touched(runner):
    grss_path = runner._grss_package_dir()
    folder = pathlib.Path(os.environ["GRSS_DATA_DIR"])
    for rel in runner._ASTROMETRY_TABLES:
        staged = grss_path / rel
        assert staged.read_bytes() == (folder / staged.name).read_bytes(), rel
        assert not staged.is_symlink(), f"{rel} must be a copy, not a link into the pool"
    special = grss_path / "fit" / "special_codes.json"
    before = special.stat().st_mtime
    t0 = time.time()
    runner._freshen_observatory_codes()
    assert (grss_path / "fit" / "codes.json").stat().st_mtime >= t0 - 1
    assert special.stat().st_mtime == before
    assert (folder / "codes.json").stat().st_mtime < t0 - 1, "the folder's bytes are never touched"
