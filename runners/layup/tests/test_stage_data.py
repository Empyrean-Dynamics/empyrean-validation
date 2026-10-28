"""stage_data: layup's data directory comes from the registered kernel set alone."""

import gzip
import importlib.util
import io
import pathlib
import re
import sys
import tarfile

import pytest

MODULE = pathlib.Path(__file__).resolve().parents[1] / "stage_data.py"


@pytest.fixture(scope="module")
def sd():
    spec = importlib.util.spec_from_file_location("stage_data", MODULE)
    mod = importlib.util.module_from_spec(spec)
    sys.modules["stage_data"] = mod  # dataclasses resolve annotations through it
    spec.loader.exec_module(mod)
    return mod


# layup 1.1.0's defaults (AuxiliaryConfigs), so the tests need no layup install.
NAMES = {
    "planet_ephemeris": "de440s.bsp",
    "earth_predict": "earth_2026_260806_2126_predict.bpc",
    "earth_historical": "earth_620120_260806.bpc",
    "earth_high_precision": "earth_latest_high_prec.bpc",
    "jpl_planets": "linux_p1550p2650.440",
    "jpl_small_bodies": "sb441-n16.bsp",
    "leap_seconds": "naif0012.tls",
    "orientation_constants": "pck00010.pck",
    "observatory_codes": "ObsCodes.json",
    "observatory_codes_compressed": "ObsCodes.json.gz",
    "debiasing_data_compressed": "debias_hires2018.tgz",
    "debiasing_data": "bias.dat",
}
REQUIRED = (
    "de440s.bsp",
    "earth_2026_260806_2126_predict.bpc",
    "earth_620120_260806.bpc",
    "earth_latest_high_prec.bpc",
    "linux_p1550p2650.440",
    "sb441-n16.bsp",
    "naif0012.tls",
    "meta_kernel.txt",
    "ObsCodes.json.gz",
    "ObsCodes.json",
    "pck00010.pck",
    "debias_hires2018.tgz",
    "bias.dat",
)
SPICE_ORDER = (
    "naif0012.tls",
    "earth_620120_260806.bpc",
    "earth_2026_260806_2126_predict.bpc",
    "pck00010.pck",
    "de440s.bsp",
    "earth_latest_high_prec.bpc",
)

# The registered set (kernels-full-2026-10-13) plus the binary DE440 the suite
# adds beside it. It carries no LSK, no text PCK and no high-resolution
# debiasing archive; its bias.dat is the LOW-resolution table.
REGISTERED_SET = (
    "bias.dat",
    "de440.bsp",
    "earth_2026_260806_2126_predict.bpc",
    "earth_620120_260806.bpc",
    "earth_latest_high_prec.bpc",
    "EGM2008.gfc",
    "gm_de440.tpc",
    "jwst_rec.bsp",
    "moon_pa_de440_200625.bpc",
    "obscodes_extended.json",
    "sb441-n16.bsp",
    "linux_p1550p2650.440",
)
HIRES_BIAS = b"high-resolution table, nside 256\n"


@pytest.fixture
def layout(sd):
    return sd.Layout(names=dict(NAMES), meta_kernel="meta_kernel.txt", required=REQUIRED, spice_order=SPICE_ORDER)


def _tgz(path, members):
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz") as tf:
        for name, data in members.items():
            info = tarfile.TarInfo(name)
            info.size = len(data)
            info.mtime = 1677709479
            tf.addfile(info, io.BytesIO(data))
    path.write_bytes(buf.getvalue())


def _set(root, names=REGISTERED_SET, extra=("naif0012.tls", "pck00010.tpc", "debias_hires2018.tgz")):
    root.mkdir(parents=True, exist_ok=True)
    for n in list(names) + list(extra):
        if n == "debias_hires2018.tgz":
            _tgz(root / n, {"tiles.dat": b"tiles", "README.txt": b"readme", "bias.dat": HIRES_BIAS})
        else:
            (root / n).write_bytes(f"contents of {n}\n".encode())
    return root


def test_every_name_layup_checks_for_is_staged_from_the_set(sd, layout, tmp_path):
    data = _set(tmp_path / "data")
    dest = tmp_path / "ar"
    rows = sd.stage(data, dest, layout)

    assert sd.verify(dest, layout) == []
    assert sorted(p.name for p in dest.iterdir()) == sorted(REQUIRED)
    # Links point at the set's own files, by absolute path.
    assert (dest / "de440s.bsp").resolve() == (data / "de440.bsp").resolve()
    assert (dest / "pck00010.pck").resolve() == (data / "pck00010.tpc").resolve()
    assert (dest / "ObsCodes.json").resolve() == (data / "obscodes_extended.json").resolve()
    assert pathlib.Path(__import__("os").readlink(dest / "sb441-n16.bsp")).is_absolute()
    # Derived files carry the set's content.
    assert gzip.decompress((dest / "ObsCodes.json.gz").read_bytes()) == (data / "obscodes_extended.json").read_bytes()
    assert (dest / "bias.dat").read_bytes() == HIRES_BIAS
    assert {r[0] for r in rows} == {"linked", "gzipped", "extracted", "wrote"}


def test_restaging_is_a_no_op(sd, layout, tmp_path):
    data = _set(tmp_path / "data")
    sd.stage(data, tmp_path / "ar", layout)
    rows = sd.stage(data, tmp_path / "ar", layout)
    assert {r[0] for r in rows} == {"ok"}


def test_the_sets_low_resolution_bias_table_is_never_staged_as_layups(sd, layout, tmp_path):
    data = _set(tmp_path / "data")
    assert (data / "bias.dat").read_bytes() != HIRES_BIAS
    sd.stage(data, tmp_path / "ar", layout)
    assert not (tmp_path / "ar" / "bias.dat").is_symlink()
    assert (tmp_path / "ar" / "bias.dat").read_bytes() == HIRES_BIAS


def test_the_registered_set_alone_names_every_missing_file_at_once(sd, layout, tmp_path):
    data = _set(tmp_path / "data", extra=())
    dest = tmp_path / "ar"
    with pytest.raises(sd.StagingError) as e:
        sd.stage(data, dest, layout)
    text = "\n".join(e.value.problems)
    assert "naif0012.tls" in text
    assert "'pck00010.tpc'" in text
    assert text.count("'debias_hires2018.tgz'") == 2  # the archive and the table inside it
    assert len(e.value.problems) == 4
    assert not dest.exists()  # nothing staged when anything is missing


def test_a_stale_second_match_is_a_hard_stop(sd, layout, tmp_path):
    data = _set(tmp_path / "data")
    (data / "earth_620120_250826.bpc").write_bytes(b"older kernel")
    with pytest.raises(sd.StagingError) as e:
        sd.stage(data, tmp_path / "ar", layout)
    [line] = e.value.problems
    assert "earth_620120_250826.bpc, earth_620120_260806.bpc" in line and "--prune" in line


def test_an_empty_file_is_a_hard_stop(sd, layout, tmp_path):
    data = _set(tmp_path / "data")
    (data / "sb441-n16.bsp").write_bytes(b"")
    with pytest.raises(sd.StagingError) as e:
        sd.stage(data, tmp_path / "ar", layout)
    assert e.value.problems == ["sb441-n16.bsp (ASSIST small-body perturbers): sb441-n16.bsp is empty"]


def test_a_file_layup_adds_that_no_role_fills_is_a_hard_stop(sd, tmp_path):
    lay = sd.Layout(
        names=dict(NAMES),
        meta_kernel="meta_kernel.txt",
        required=REQUIRED + ("new_layup_file.dat",),
        spice_order=SPICE_ORDER,
    )
    with pytest.raises(sd.StagingError) as e:
        sd.stage(_set(tmp_path / "data"), tmp_path / "ar", lay)
    assert e.value.problems == ["new_layup_file.dat: the installed layup reads it, but no staging role fills it"]


def test_a_set_update_retargets_the_link(sd, layout, tmp_path):
    data = _set(tmp_path / "data")
    dest = tmp_path / "ar"
    sd.stage(data, dest, layout)
    (data / "earth_620120_260806.bpc").rename(data / "earth_620120_270806.bpc")
    rows = sd.stage(data, dest, layout)
    assert ("linked", "earth_620120_260806.bpc", "earth_620120_270806.bpc") in rows
    assert (dest / "earth_620120_260806.bpc").resolve() == (data / "earth_620120_270806.bpc").resolve()
    assert "$SET/earth_620120_270806.bpc" in (dest / "meta_kernel.txt").read_text()


def test_meta_kernel_loads_the_sets_files_in_layups_order(sd, layout, tmp_path):
    data = _set(tmp_path / "data")
    sd.stage(data, tmp_path / "ar", layout)
    text = (tmp_path / "ar" / "meta_kernel.txt").read_text()
    loads = re.findall(r"'\$SET/([^']+)'", text)
    assert loads == [
        "naif0012.tls",
        "earth_620120_260806.bpc",
        "earth_2026_260806_2126_predict.bpc",
        "pck00010.tpc",
        "de440.bsp",
        "earth_latest_high_prec.bpc",
    ]


def test_long_paths_are_continued_within_the_kernel_pool_limit(sd, layout, tmp_path):
    deep = tmp_path / ("very-long-directory-name-" * 6) / "data"
    data = _set(deep)
    sd.stage(data, tmp_path / "ar", layout)
    text = (tmp_path / "ar" / "meta_kernel.txt").read_text()
    block = text.split("PATH_VALUES = (")[1].split(")")[0]
    pieces = re.findall(r"'([^']*)'", block)
    assert len(pieces) > 1 and all(len(p) <= 80 for p in pieces)
    assert "".join(p.rstrip("+") for p in pieces) == str(data.absolute())


def test_verify_catches_a_dangling_link(sd, layout, tmp_path):
    data = _set(tmp_path / "data")
    dest = tmp_path / "ar"
    sd.stage(data, dest, layout)
    (data / "sb441-n16.bsp").unlink()
    assert sd.verify(dest, layout) == ["sb441-n16.bsp"]


def test_a_missing_data_directory_is_named(sd, layout, tmp_path):
    with pytest.raises(sd.StagingError) as e:
        sd.stage(tmp_path / "nowhere", tmp_path / "ar", layout)
    assert "does not exist" in e.value.problems[0]
