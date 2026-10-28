"""jorbit_offline: jorbit's kernels resolve to the registered set, never a download."""

import importlib.util
import pathlib
import sys
import types

import pytest

RUNNER_DIR = pathlib.Path(__file__).resolve().parents[1]
# Shaped like jorbit's constants, on a host that cannot resolve: these tests
# never touch the network, and the repo's drift check flags real NASA URLs.
PLANET = "https://kernels.invalid//ftp/eph/planets/bsp/de440.bsp"
ASTEROID = "https://kernels.invalid//ftp/eph/small_bodies/asteroids_de441/sb441-n16.bsp"
MPCHECKER = "https://bundle.invalid/datasets/jorbit/jorbit_mpchecker/resolve/main/"


@pytest.fixture(scope="module")
def off():
    spec = importlib.util.spec_from_file_location(
        "jorbit_offline", RUNNER_DIR / "jorbit_offline.py"
    )
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def urls(planet=PLANET, asteroid=ASTEROID):
    return {
        "DEFAULT_PLANET_EPHEMERIS_URL": planet,
        "DEFAULT_ASTEROID_EPHEMERIS_URL": asteroid,
    }


def kernel_set(tmp_path, names=("de440.bsp", "sb441-n16.bsp")):
    for n in names:
        (tmp_path / n).write_bytes(b"DAF/SPK " + n.encode())
    return tmp_path


def test_constants_are_read_without_executing_the_file(off, tmp_path):
    f = tmp_path / "constants.py"
    f.write_text(
        "import a_module_that_does_not_exist\n"
        "PLANET = 'https://x/de440.bsp'\n"
        "SPLIT = (\n    'https://x/'\n    'sb441-n16.bsp'\n)\n"
        "TYPED: str = 'https://y/'\n"
        "NUMBER = 3\n"
        "OTHER = 'not asked for'\n"
    )
    got = off.read_string_constants(f, ("PLANET", "SPLIT", "TYPED", "NUMBER", "ABSENT"))
    assert got == {
        "PLANET": "https://x/de440.bsp",
        "SPLIT": "https://x/sb441-n16.bsp",
        "TYPED": "https://y/",
    }


def test_each_kernel_url_maps_to_the_set_file_of_the_same_name(off, tmp_path):
    d = kernel_set(tmp_path)
    assert off.kernel_sources(urls(), d) == {
        PLANET: d / "de440.bsp",
        ASTEROID: d / "sb441-n16.bsp",
    }


@pytest.mark.parametrize("missing", ["de440.bsp", "sb441-n16.bsp"])
def test_a_missing_set_file_is_a_hard_stop_naming_it(off, tmp_path, missing):
    d = kernel_set(
        tmp_path, names=[n for n in ("de440.bsp", "sb441-n16.bsp") if n != missing]
    )
    with pytest.raises(off.OfflineSetupError, match=str(d / missing)):
        off.kernel_sources(urls(), d)


def test_an_empty_set_file_is_a_hard_stop(off, tmp_path):
    d = kernel_set(tmp_path)
    (d / "sb441-n16.bsp").write_bytes(b"")
    with pytest.raises(off.OfflineSetupError, match="missing or empty"):
        off.kernel_sources(urls(), d)


def test_a_kernel_the_set_does_not_carry_is_refused_not_fetched(off, tmp_path):
    d = kernel_set(tmp_path)
    with pytest.raises(off.OfflineSetupError, match="de441.bsp"):
        off.kernel_sources(
            urls(planet="https://kernels.invalid/ftp/eph/planets/bsp/de441.bsp"), d
        )


def test_a_vanished_url_constant_is_a_hard_stop(off, tmp_path):
    d = kernel_set(tmp_path)
    with pytest.raises(off.OfflineSetupError, match="DEFAULT_ASTEROID_EPHEMERIS_URL"):
        off.kernel_sources({"DEFAULT_PLANET_EPHEMERIS_URL": PLANET}, d)


def test_installed_jorbit_still_reads_the_kernels_the_set_carries(off):
    """Guards a jorbit bump: its URLs must still end in the set's file names."""
    if importlib.util.find_spec("jorbit") is None:
        pytest.skip("jorbit not installed in this interpreter")
    got = off.read_string_constants(
        off.jorbit_constants_path(),
        (*off.KERNEL_URL_CONSTANTS, off.MPCHECKER_URL_CONSTANT),
    )
    names = {got[n].rsplit("/", 1)[-1] for n in off.KERNEL_URL_CONSTANTS}
    assert names == set(off.REGISTERED_SET_KERNELS)
    assert got[off.MPCHECKER_URL_CONSTANT].startswith("https://")


def test_staging_lands_the_set_bytes_under_jorbits_url_keys(off, tmp_path):
    pytest.importorskip("astropy")
    from astropy.config import paths
    from astropy.utils import data

    (tmp_path / "set").mkdir()
    d = kernel_set(tmp_path / "set")
    sources = off.kernel_sources(urls(), d)
    cache = tmp_path / "cache"
    with paths.set_temp_cache(str(cache)), data.conf.set_temp("allow_internet", False):
        cache_dir = pathlib.Path(paths.get_cache_dir())
        first = off.stage_kernels(sources, cache_dir)
        assert [line.split()[0] for line in first] == ["staged", "staged"]
        assert (
            pathlib.Path(data.download_file(PLANET, cache=True)).read_bytes()
            == (d / "de440.bsp").read_bytes()
        )
        again = off.stage_kernels(sources, cache_dir)
        assert [line.split()[0] for line in again] == ["ok", "ok"]
        (d / "de440.bsp").write_bytes(b"a different kernel")
        moved = off.stage_kernels(sources, cache_dir)
        assert [line.split()[0] for line in moved] == ["restaged", "ok"]
        # Anything not staged raises rather than downloading.
        with pytest.raises(Exception, match="allow_internet"):
            data.download_file(MPCHECKER + "names.npy", cache=True)


def test_url_check_suppression_is_confined_to_the_block(off, monkeypatch):
    pytest.importorskip("astropy")
    from astropy.utils import data

    real = data.is_url_in_cache
    captured = types.ModuleType("jorbit.fake_submodule")
    monkeypatch.setitem(sys.modules, "jorbit.fake_submodule", captured)
    asked = []
    with off.suppress_url_check(MPCHECKER, asked):
        captured.is_url_in_cache = (
            data.is_url_in_cache
        )  # what `from ... import` captures
        assert data.is_url_in_cache(MPCHECKER + "names.npy") is True
        assert data.is_url_in_cache("https://example.invalid/never-cached") is False
    assert asked == [MPCHECKER + "names.npy"]
    assert data.is_url_in_cache is real
    assert captured.is_url_in_cache is real
