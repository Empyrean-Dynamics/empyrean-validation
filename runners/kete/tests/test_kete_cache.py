"""run_kete: kete's cache is staged from the kete folder before kete is imported.

    KETE_DATA_DIR=<third-party>/kete runners/kete/.venv/bin/python -m pytest runners/kete/tests -q
"""

import importlib.util
import os
import pathlib
import subprocess
import sys
import time

import pytest

HERE = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))
DATA = os.environ.get("KETE_DATA_DIR")


@pytest.fixture(scope="module")
def runner():
    spec = importlib.util.spec_from_file_location("run_kete_cache", HERE / "run_kete.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def _folder(tmp_path, runner, skip=()):
    d = tmp_path / "kete"
    d.mkdir()
    for n in (*runner.KETE_CORE_FILES, "earth_1962_260806_2126_combined.bpc", "bias.dat"):
        if n not in skip:
            (d / n).write_text("x")
    return d


def test_every_kernel_is_linked_into_kernels_core(runner, tmp_path):
    d = _folder(tmp_path, runner)
    cache = tmp_path / "cache"
    (cache / "kernels" / "core").mkdir(parents=True)
    (cache / "kernels" / "core" / "stale.bsp").write_text("old")  # kete loads every file here
    names = runner.stage_kete_cache(d, cache)
    core = cache / "kernels" / "core"
    want = sorted([*runner.KETE_CORE_FILES, "earth_1962_260806_2126_combined.bpc"])
    assert sorted(names) == want
    assert sorted(p.name for p in core.iterdir()) == want
    assert "bias.dat" not in names  # the debias table is read from the folder, not loaded
    for n in names:
        assert (core / n).resolve() == (d / n).resolve()


def test_a_missing_kernel_is_named_not_downloaded(runner, tmp_path):
    d = _folder(tmp_path, runner, skip=("20000704.bsp", "earth_1962_260806_2126_combined.bpc"))
    with pytest.raises(runner.KeteDataError) as e:
        runner.stage_kete_cache(d, tmp_path / "cache")
    assert "20000704.bsp" in str(e.value) and "combined.bpc" in str(e.value)


def test_the_sb441_n16_kernel_is_staged_and_its_absence_named(runner, tmp_path):
    assert "sb441-n16.bsp" in runner.KETE_CORE_FILES
    d = _folder(tmp_path, runner, skip=("sb441-n16.bsp",))
    with pytest.raises(runner.KeteDataError, match="sb441-n16.bsp"):
        runner.stage_kete_cache(d, tmp_path / "cache")


def test_kete_is_not_imported_at_module_load(runner):
    assert runner.kete is None and runner._HAVE_KETE is False


@pytest.mark.skipif(not DATA, reason="set KETE_DATA_DIR to the kete folder (kete venv)")
def test_kete_imports_from_the_staged_folder_without_a_download(tmp_path):
    """In a fresh process: kete loads the folder's kernels, quickly. With a kernel
    missing, kete would retry its download for about a minute instead."""
    code = (
        "import sys, pathlib; sys.path.insert(0, %r)\n"
        "import run_kete as R\n"
        "R.KETE_CACHE_DIR = pathlib.Path(%r)\n"
        "R._load_kete(pathlib.Path(%r))\n"
        "assert str(R.kete.cache.cache_path()).rstrip('/') == %r, R.kete.cache.cache_path()\n"
        "ceres = R.kete.spice.get_state(20000001, 2460000.5)  # the folder's Horizons SPK\n"
        "print(ceres)\n"
    ) % (str(HERE), str(tmp_path / "cache"), DATA, str(tmp_path / "cache"))
    # A HOME of its own: kete's default cache (~/.kete) must not stand in for the
    # staged one, so an import that ignored KETE_CACHE_DIR would find nothing.
    env = {k: v for k, v in os.environ.items() if k != "KETE_CACHE_DIR"}
    env["HOME"] = str(tmp_path / "home")
    t0 = time.monotonic()
    r = subprocess.run([sys.executable, "-c", code], capture_output=True, text=True, timeout=300, env=env)
    assert r.returncode == 0, r.stderr[-2000:]
    assert time.monotonic() - t0 < 30.0
