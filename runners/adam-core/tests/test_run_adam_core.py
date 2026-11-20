"""Unit + integration tests for the adam-core runners (run inside the venv).

    runners/adam-core/.venv/bin/python -m pytest runners/adam-core/tests -q

Pure tests (record shapes, PSV normalization, self-perturber labels) run anywhere.
Tests that touch adam_core / adam-assist are integration tests: they ``importorskip``
the stack, so they are skipped when the venv is absent.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
RUNNER = HERE.parent
sys.path.insert(0, str(RUNNER))

import adam_core_common as C  # noqa: E402
import run_adam_core as R  # noqa: E402
import run_adam_core_walk as W  # noqa: E402


# ── pure tests ──────────────────────────────────────────────


def test_normalize_psv_strips_padding():
    raw = "# version=2017\n permID | mode | stn |  obsTime  \n 123 | CCD | 691 | 2020-01-01T00:00:00.000Z \n"
    norm = C.normalize_psv(raw)
    lines = norm.strip().splitlines()
    assert lines[0] == "# version=2017"  # comment preserved verbatim
    assert lines[1] == "permID|mode|stn|obsTime"  # padding stripped
    assert lines[2] == "123|CCD|691|2020-01-01T00:00:00.000Z"


def test_prediction_record_shape():
    obs = {"key_hash": 42, "stn": "691"}
    cov = [[1.0e-8, 2.0e-9], [2.0e-9, 1.5e-8]]
    rec = W.prediction_record("X", "adam-core", "core+sp", 3, obs, 7, True, 10.0, -5.0, cov)
    assert rec["tool"] == "adam-core" and rec["config_arm"] == "core+sp"
    assert rec["obs_idx"] == 7 and rec["key_hash"] == 42 and rec["in_sample"] is True
    assert rec["uncertainty_form"] == "radec_2x2"
    assert rec["cov_units"] == "deg2" and rec["cov_basis"] == "native_ra"
    assert rec["cov_radec"] == cov
    # zero / absent covariance -> uncertainty_form none (never a d^2 of 1e6)
    rec0 = W.prediction_record("X", "adam-core", "core+sp", 3, obs, 7, False, 10.0, -5.0, None)
    assert rec0["uncertainty_form"] == "none" and "cov_radec" not in rec0
    # non-finite position -> dropped
    assert W.prediction_record("X", "adam-core", "core+sp", 3, obs, 7, True, float("nan"), 0.0, None) is None


def test_window_record_shape_failure():
    rec = W.window_record("X", "adam-core", "core+mc", 2, False, 12.5, failure="not_converged")
    assert rec["tool"] == "adam-core" and rec["config_arm"] == "core+mc"
    assert rec["converged"] is False and rec["failure"] == "not_converged"
    assert rec["warm_start"] is False and rec["seed_source"] == "own_iod"
    assert rec["fit_time_ms"] == 12.5


def test_single_channel_tool_id():
    assert R.TOOL == "adam-core"
    assert R.PROFILE == "CORE_OD_V1"
    assert not hasattr(R, "channel_id")  # the --weights axis is gone (one public channel)
    # the private B612 lane is gone: no reference to the proprietary data package.
    assert "adam-observatory-uncertainties" not in (R.od_one.__doc__ or "")


def test_resolve_fixture_slash_mapping(tmp_path):
    # comets/ISOs: "103P/Hartley 2" -> "103P_Hartley 2.psv"
    (tmp_path / "103P_Hartley 2.psv").write_text("x")
    p, cands = C.resolve_fixture(tmp_path, "103P/Hartley 2")
    assert p is not None and p.name == "103P_Hartley 2.psv"
    assert any("103P_Hartley 2.psv" in c for c in cands)
    # exact name wins when present
    (tmp_path / "Eros.psv").write_text("x")
    p2, _ = C.resolve_fixture(tmp_path, "Eros")
    assert p2 is not None and p2.name == "Eros.psv"
    # mpc_designation fallback
    (tmp_path / "99999.psv").write_text("y")
    p3, _ = C.resolve_fixture(tmp_path, "NoSuchName", "99999")
    assert p3 is not None and p3.name == "99999.psv"
    # truly missing -> None, and the candidate list is reported (loud error)
    p4, cands4 = C.resolve_fixture(tmp_path, "Ghost")
    assert p4 is None and any("Ghost.psv" in c for c in cands4)


def test_self_perturber_labels():
    assert C.SELF_PERTURBERS == {"Iris", "Vesta", "Pallas", "Hygiea"}
    for o in ("Iris", "Vesta", "Pallas", "Hygiea"):
        assert C.is_self_perturber(o)
        assert "self-perturber" in C.force_model_label(o)
    for o in ("Apophis", "Eros", "67P"):
        assert not C.is_self_perturber(o)
        assert C.force_model_label(o) == "planets+SB441-n16"


def test_od_profile_models_names_the_public_pipeline():
    pytest.importorskip("adam_core")
    assert C.od_profile_models() == ["Efcc18CatalogBiasCorrection", "NightlyBatchDeweighting"]


def test_the_public_profile_applies_no_vfc2017_transform_so_sigmafill_stays_the_fill():
    """adam-core 0.8.0 adds typed VFC2017 uncertainty-floor and replacement transforms,
    but CORE_OD_V1 does not apply them (its pipeline is the EFCC18 debias then nightly
    de-weighting, and its only resource slot is the EFCC18 table). The channel therefore
    still fills missing sigma with the shared sigmafill before the fit. A release that
    puts a VFC2017 transform into the default profile must make this test fail, so the
    relation between that transform and sigmafill is decided, not inherited."""
    pytest.importorskip("adam_core")
    from adam_core.orbit_determination import CORE_OD_V1

    names = C.od_profile_models()
    assert not any("Vfc2017" in n or "VFC2017" in n.upper() for n in names), names
    assert [r.slot for r in CORE_OD_V1.requirements] == ["efcc18_catalog_bias_table"]


def test_setup_pins_the_release_pair_this_channel_documents():
    """setup.sh's defaults are the PyPI pair the runner, its README and the Makefile name
    (adam-core 0.8.0 with adam-assist 0.7.0, the first adam-assist that allows adam-core
    0.8). The installed pair must be that pair, or the recorded versions would lie."""
    from importlib.metadata import version

    setup = (Path(__file__).resolve().parent.parent / "setup.sh").read_text()
    import re

    core = re.search(r'ADAM_CORE_VERSION="\$\{ADAM_CORE_VERSION:-([0-9.]+)\}"', setup).group(1)
    assist = re.search(r'ADAM_ASSIST_VERSION="\$\{ADAM_ASSIST_VERSION:-([0-9.]+)\}"', setup).group(1)
    assert (core, assist) == ("0.8.0", "0.7.0")
    pytest.importorskip("adam_core")
    assert (version("adam-core"), version("adam-assist")) == (core, assist)


# ── integration tests (need the venv) ───────────────────────

adam_core = pytest.importorskip("adam_core")

# The manifest's adam-core/ folder holds exactly these two files, by hash.
REGISTERED_SHA = {
    "de440.bsp": "a4ce9bf9b3282becc9f4b2ac3cebe03a2ae7599981aabd7265fd8482fff7c4b5",
    "sb441-n16.bsp": "919d612ce3c72a78fc7158f9120156542d0f21e6b8b052e4c1339c759747fd90",
}


@pytest.fixture(scope="session")
def adam_core_dir(tmp_path_factory):
    """The registered adam-core/ folder: $ADAM_CORE_DATA_DIR, else a symlink view of the
    engine set's byte-identical copies. Skips (never fakes) when neither exists."""
    import os

    env = os.environ.get("ADAM_CORE_DATA_DIR")
    if env and (Path(env) / "de440.bsp").is_file():
        d = Path(env)
    else:
        src = Path.home() / ".empyrean" / "data"
        if not all((src / n).is_file() for n in REGISTERED_SHA):
            pytest.skip("registered kernels absent (set ADAM_CORE_DATA_DIR)")
        d = tmp_path_factory.mktemp("adam-core")
        for n in REGISTERED_SHA:
            (d / n).symlink_to(src / n)
    C.configure_kernels(d)
    return d


def test_od_config_binds_core_profile_public_only():
    """CORE_OD_V1 binds with PUBLIC resources only (the jpl-debias-2018 table); it
    must not require the private station_catalog_uncertainty_table."""
    from adam_core.orbit_determination import CORE_OD_V1

    cfg = C.od_config()  # binds; raises if a required resource is missing
    assert cfg is not None
    slots = [r.slot for r in CORE_OD_V1.requirements]
    assert slots == ["efcc18_catalog_bias_table"]
    assert "station_catalog_uncertainty_table" not in slots


def test_plan_orbit_roundtrip_frame_origin_scale():
    row = {
        "object": "unit_test",
        "epoch_mjd_tdb": 60000.0,
        "ic_pos_au": [1.1, -0.3, 0.05],
        "ic_vel_au_d": [0.002, 0.016, -0.001],
    }
    orbit, reason = C.plan_orbit(row)
    assert reason is None
    c = orbit.coordinates
    assert c.frame == "equatorial"
    assert c.origin.code[0].as_py() == "SOLAR_SYSTEM_BARYCENTER"
    assert c.time.scale == "tdb"
    assert abs(c.time.mjd()[0].as_py() - 60000.0) < 1e-9
    assert abs(c.x[0].as_py() - 1.1) < 1e-12
    assert abs(c.vz[0].as_py() - (-0.001)) < 1e-12


def test_nongrav_attachment_and_dt_unsupported():
    row = {"object": "a", "epoch_mjd_tdb": 60000.0, "ic_pos_au": [1, 0, 0],
           "ic_vel_au_d": [0, 0.017, 0], "ic_a1": 5e-13, "ic_a2": -3e-14, "ic_a3": 0.0}
    _orbit, reason = C.plan_orbit(row)
    assert reason is None
    ng, r2 = C.nongrav_for(row)
    assert ng is not None and abs(ng.A1[0].as_py() - 5e-13) < 1e-24
    # a non-zero Marsden DT is unsupported (no DT term in adam_core 0.7)
    row_dt = dict(row, ic_non_grav_dt=45.0, ic_g_alpha=0.1, ic_g_r0=2.8, ic_g_m=2.1, ic_g_n=5.0, ic_g_k=4.6)
    _o, reason_dt = C.plan_orbit(row_dt)
    assert reason_dt == "nongrav dt"


def test_build_source_catalog_fills_and_labels():
    """Public Vereš 2017 fill supplies ONLY missing sigmas; reported sigmas pass
    through untouched; the fill is labelled by level."""
    import math

    header = (
        "# version=2017\n"
        "permID|provID|trkSub|mode|stn|obsTime|ra|dec|rmsRA|rmsDec|rmsCorr|astCat|mag|band\n"
    )
    # Two reported-rms rows (0.30") at ground station 691, two blank-rms rows at 691.
    rows = [
        "T|t|t|CCD|691|2020-01-01T00:00:00.000Z|10.00000|5.00000|0.30|0.30|0.0|Gaia2|20|G",
        "T|t|t|CCD|691|2020-01-01T00:10:00.000Z|10.01000|5.01000|0.30|0.30|0.0|Gaia2|20|G",
        "T|t|t|CCD|691|2020-01-02T00:00:00.000Z|10.05000|5.05000|||0.0|Gaia2|20|G",
        "T|t|t|CCD|691|2020-01-02T00:10:00.000Z|10.06000|5.06000|||0.0|Gaia2|20|G",
    ]
    ok = R.resolvable_fn()
    catalog, assoc, n_dropped, _d, n_parsed, ss = C.build_source_catalog(
        header + "\n".join(rows) + "\n", "X", ok
    )
    assert n_dropped == 0 and n_parsed == 4 and len(catalog) == 4
    # 2 reported, 2 filled; 691 is a station rule -> station level.
    assert ss["reported"] == 2 and ss["filled"] == 2 and ss["total"] == 4
    assert ss["filled_by_level"]["station"] == 2
    assert ss["model"] == "Veres2017SigmaFill"
    # the two reported sigmas (0.30") survive untouched and the two blanks are
    # filled with station 691's 0.5", both in ADES units (RA sigma times cos(Dec));
    # the catalog takes the RA-axis sigma, divided by cos(Dec) as adam-core's ADES
    # route does.
    sra = list(catalog.ra_sigma.to_numpy(zero_copy_only=False))
    sdec = list(catalog.dec_sigma.to_numpy(zero_copy_only=False))
    cosd = [math.cos(math.radians(d)) for d in (5.0, 5.01, 5.05, 5.06)]
    for got, want in zip(sra, [0.30 / cosd[0], 0.30 / cosd[1], 0.50 / cosd[2], 0.50 / cosd[3]]):
        assert math.isclose(got, want, rel_tol=1e-12)
    assert [round(v, 12) for v in sdec] == [0.30, 0.30, 0.50, 0.50]
    # run_od input is valid: no NaN sigma remains.
    assert all(math.isfinite(v) and v > 0 for v in sra)


def test_an_all_dropped_object_reports_the_fill_levels_the_fill_uses():
    """An object whose every station is dropped reports the same counters as a filled
    one: the four fill levels and partially_reported, never a stale 'global' level."""
    from sigmafill.veres2017 import LEVELS

    psv = ("# version=2017\npermID|mode|stn|obsTime|ra|dec|rmsRA|rmsDec|astCat\n"
           "T|CCD|C51|2020-01-01T00:00:00.000Z|10.0|5.0|||Gaia2\n")
    catalog, _a, n_dropped, _d, _n, ss = C.build_source_catalog(psv, "X", lambda s: False)
    assert catalog is None and n_dropped == 1
    assert set(ss["filled_by_level"]) == set(LEVELS)
    assert ss["partially_reported"] == 0 and ss["total"] == 0


def test_missing_asteroid_path_raises():
    """The October planets-only trick (a non-existent asteroids_path) is dead in
    adam-assist 0.7.0: construction must RAISE (strict DAF binding). This is the
    proof behind the self-perturber policy."""
    reason = C.missing_asteroid_path_raises()
    assert "RuntimeError" in reason or "kernel" in reason.lower()


def test_od_one_self_perturber_unsupported():
    """A self-perturber OD is a NAMED unsupported row (no self-singular state),
    because adam-assist 0.7.0 has no planets-only force model."""
    res = R.od_one("Vesta", RUNNER.parent.parent / "fixtures" / "psv", {})
    assert res["adam_core_converged"] is False
    assert "self-perturber" in res["adam_core_unsupported"]


@pytest.mark.skipif(not (RUNNER.parent.parent / "fixtures" / "psv").is_dir(),
                    reason="PSV fixtures not fetched (make fixtures)")
def test_build_source_catalog_drops_space_based():
    """A fixture with space-based rows yields a positive dropped count and a fittable catalog."""
    fx = RUNNER.parent.parent / "fixtures" / "psv" / "2010 TK7.psv"
    if not fx.exists():
        pytest.skip("2010 TK7 fixture absent")
    ok = R.resolvable_fn()
    catalog, assoc, n_dropped, droppable, n_parsed, _ss = C.build_source_catalog(fx.read_text(), "2010 TK7", ok)
    assert n_parsed > 0
    assert n_dropped > 0 and droppable  # C51/C57/250 etc. dropped
    assert catalog is not None and len(catalog) == n_parsed - n_dropped


# ── registered kernels (unit: adam-core CI wiring) ──────────


def test_configure_kernels_refuses_missing_and_names_the_file(tmp_path):
    with pytest.raises(SystemExit) as e:
        C.configure_kernels(tmp_path)  # empty folder
    msg = str(e.value)
    assert "de440.bsp" in msg and "sb441-n16.bsp" in msg
    assert "Refusing to fall back" in msg
    with pytest.raises(SystemExit):
        C.configure_kernels(None)  # no directory at all is also a refusal


def test_registered_kernels_are_the_manifest_bytes(adam_core_dir):
    for name, sha in REGISTERED_SHA.items():
        assert C.sha256_file(adam_core_dir / name) == sha


def test_propagator_opens_the_registered_paths_not_the_pypi_ones(adam_core_dir):
    import adam_assist.propagator as P

    prop = C.default_propagator()
    assert prop.planets_path == str(adam_core_dir / "de440.bsp")
    assert prop.asteroids_path == str(adam_core_dir / "sb441-n16.bsp")
    assert prop.planets_path != str(P.de440)  # not the install-time package copy
    assert prop.asteroids_path != str(P.de441_n16)


def test_a_missing_registered_kernel_makes_construction_fail_at_that_path(adam_core_dir, tmp_path):
    """Proves the path is the one OPENED: pointing planets_path at a missing file
    raises naming that file (adam-assist binds kernels at construction)."""
    from adam_assist import ASSISTPropagator

    ghost = tmp_path / "not-de440.bsp"
    with pytest.raises(Exception) as e:
        ASSISTPropagator(planets_path=str(ghost), asteroids_path=str(adam_core_dir / "sb441-n16.bsp"))
    assert "not-de440.bsp" in str(e.value)


def test_settings_record_states_kernels_obscodes_and_model_differences(adam_core_dir):
    from sigmafill.veres2017 import Veres2017SigmaModel

    rec = C.settings_record(1e-6, 1, C.sha256_file(Veres2017SigmaModel.from_csv().source),
                            adam_core_dir / "no-such-obscodes.json")
    assert REGISTERED_SHA["de440.bsp"] in rec["planets_kernel"]
    assert REGISTERED_SHA["sb441-n16.bsp"] in rec["asteroid_kernel"]
    assert rec["epsilon"] == "1e-06" and rec["adaptive_mode"] == "1"
    assert rec["nightly_batch_cap"] == "4"
    assert "Efcc18CatalogBiasCorrection" in rec["observation_pipeline"]
    assert "no fill stage" in rec["sigma_fill"]
    # adam_core accepts no obscodes path: the difference is stated, here as unknown
    # because the registered file was not supplied.
    assert rec["obscodes_identical_to_registered"] == "unknown"
    assert "accepts no path" in rec["kernel_source"]
    assert rec["od_timeout_min"].startswith("30: each object is fit in its own child process")
    assert "rmsRA/cos(dec)" in rec["observation_covariance"]


def test_settings_record_every_file_the_install_resolved_like_the_rust_channel(adam_core_dir):
    """Each Earth-orientation file, the leap-second kernel, the SPICE planetary
    kernel and the observatory table adam-core resolved from its PyPI data packages
    is recorded by name, package version and sha256, under the adam-core-rust
    channel's field names and format."""
    from importlib import metadata

    import naif_de440
    import naif_earth_itrf93
    import naif_eop_high_prec
    import naif_eop_historical
    import naif_eop_predict
    import naif_leapseconds
    from mpc_obscodes import mpc_obscodes
    from sigmafill.veres2017 import Veres2017SigmaModel

    def entry(path, dist):
        p = Path(path)
        return f"{p.name} {dist} {metadata.version(dist)} (sha256 {C.sha256_file(p)})"

    rec = C.settings_record(1e-6, 1, C.sha256_file(Veres2017SigmaModel.from_csv().source))
    assert rec["earth_orientation"] == "; ".join([
        entry(naif_eop_high_prec.eop_high_prec, "naif-eop-high-prec"),
        entry(naif_eop_historical.eop_historical, "naif-eop-historical"),
        entry(naif_eop_predict.eop_predict, "naif-eop-predict"),
        entry(naif_earth_itrf93.earth_itrf93, "naif-earth-itrf93"),
    ])
    assert rec["leapseconds"] == entry(naif_leapseconds.leapseconds, "naif-leapseconds")
    assert rec["spice_planets_kernel"] == entry(naif_de440.de440, "naif-de440")
    assert rec["obscodes"] == entry(mpc_obscodes, "mpc-obscodes")


def test_a_default_kernel_the_record_does_not_name_is_refused(adam_core_dir, monkeypatch):
    import adam_core.utils.spice as S

    monkeypatch.setattr(S, "DEFAULT_KERNELS", S.DEFAULT_KERNELS + ["/x/some_new_pkg/k.bpc"])
    with pytest.raises(SystemExit, match="some_new_pkg"):
        C.resolved_files_record()


def test_obscodes_identical_flag_is_computed_from_hashes(adam_core_dir, tmp_path):
    from mpc_obscodes import mpc_obscodes

    same = tmp_path / "same.json"
    same.write_bytes(Path(mpc_obscodes).read_bytes())
    other = tmp_path / "other.json"
    other.write_text("{}")
    assert C.obscodes_record(same)["obscodes_identical_to_registered"] == "true"
    assert C.obscodes_record(other)["obscodes_identical_to_registered"] == "false"


def test_unrecognized_catalog_is_labelled_not_folded_into_a_known_level():
    header = (
        "# version=2017\n"
        "permID|provID|trkSub|mode|stn|obsTime|ra|dec|rmsRA|rmsDec|rmsCorr|astCat|mag|band\n"
    )
    rows = [
        "T|t|t|CCD|ZZ9|2020-01-01T00:00:00.000Z|10.0|5.0|||0.0|Gaia2|20|G",       # catalog
        "T|t|t|CCD|ZZ9|2020-01-01T00:10:00.000Z|10.1|5.1|||0.0|UNK|20|G",         # no_catalog
        "T|t|t|CCD|ZZ9|2020-01-01T00:20:00.000Z|10.2|5.2|||0.0|MadeUpCat|20|G",   # unrecognized
    ]
    # ZZ9 is not placeable; accept every station so only the fill branches are tested.
    _c, _a, nd, _d, _n, ss = C.build_source_catalog(header + "\n".join(rows) + "\n", "X", lambda code: True)
    assert nd == 0
    assert ss["filled_by_level"] == {
        "station": 0, "catalog": 1, "no_catalog": 1, "catalog_unrecognized": 1}


# ── the observation covariance: adam-core's ADES route ──────


ADES_HEADER = "# version=2017\npermID|provID|trkSub|mode|stn|obsTime|ra|dec|rmsRA|rmsDec|rmsCorr|astCat|mag|band\n"


def test_the_catalog_takes_the_ra_axis_sigma_and_a_zero_null_correlation():
    """ADES rmsRA is RA sigma times cos(Dec): at Dec 60 the RA-axis sigma is twice it.
    A null rmsCorr is a zero correlation; a reported one is kept."""
    import math

    rows = [
        "T|t|t|CCD|691|2020-01-01T00:00:00.000Z|10.0|60.0|0.36|0.20||Gaia2|20|G",
        "T|t|t|CCD|691|2020-01-01T00:10:00.000Z|10.1|-30.0|0.30|0.25|0.4|Gaia2|20|G",
    ]
    catalog, *_ = C.build_source_catalog(ADES_HEADER + "\n".join(rows) + "\n", "X", lambda c: True)
    sra = catalog.ra_sigma.to_pylist()
    assert math.isclose(sra[0], 0.72, rel_tol=1e-12)
    assert math.isclose(sra[1], 0.30 / math.cos(math.radians(30.0)), rel_tol=1e-12)
    assert catalog.radec_corr.to_pylist() == [0.0, 0.4]


def test_the_catalog_covariance_is_adam_cores_own_ades_covariance():
    """The covariance run_od builds from the catalog equals the one adam-core's own
    ADES route (OrbitDeterminationObservations.from_ades) builds from the same rows."""
    import numpy as np
    from adam_core.observations.ades import ADES_string_to_tables
    from adam_core.orbit_determination.evaluate import OrbitDeterminationObservations

    rows = [
        "T|t|t|CCD|691|2020-01-01T00:00:00.000Z|10.0|60.0|0.36|0.20||Gaia2|20|G",
        "T|t|t|CCD|F51|2020-01-01T00:10:00.000Z|200.1|-30.0|0.30|0.25|0.4|Gaia2|20|G",
        "T|t|t|CCD|I41|2020-01-02T00:00:00.000Z|300.2|85.0|0.12|0.10|-0.7|Gaia2|20|G",
    ]
    psv = ADES_HEADER + "\n".join(rows) + "\n"
    catalog, *_ = C.build_source_catalog(psv, "X", R.resolvable_fn())
    ours = catalog.coordinates().covariance.to_matrix()[:, 1:3, 1:3]
    _ctx, ades = ADES_string_to_tables(C.normalize_psv(psv))
    theirs = OrbitDeterminationObservations.from_ades(ades).coordinates.covariance.to_matrix()[:, 1:3, 1:3]
    assert np.all(np.isfinite(ours))
    np.testing.assert_allclose(ours, theirs, rtol=1e-14, atol=0.0)


# ── the fitted epoch is TDB ─────────────────────────────────


def test_the_fitted_epoch_helper_converts_utc_to_tdb():
    from adam_core.coordinates import CartesianCoordinates, Origin
    from adam_core.time import Timestamp

    utc = 59278.83463378815
    coords = CartesianCoordinates.from_kwargs(
        x=[1.0], y=[0.0], z=[0.0], vx=[0.0], vy=[0.017], vz=[0.0],
        time=Timestamp.from_mjd([utc], scale="utc"),
        origin=Origin.from_kwargs(code=["SOLAR_SYSTEM_BARYCENTER"]), frame="equatorial",
    )
    # TDB - UTC in 2021: 32.184 s + 37 leap seconds, plus TDB - TT under 2 ms.
    assert 69.182 < (C.fitted_epoch_mjd_tdb(coords) - utc) * 86400.0 < 69.186


@pytest.mark.skipif(not (RUNNER.parent.parent / "fixtures" / "psv" / "2008 LC18.psv").is_file(),
                    reason="PSV fixtures not fetched (make fixtures)")
def test_od_one_writes_the_fitted_epoch_in_tdb(adam_core_dir, monkeypatch):
    """The fit comes back in the observations' UTC; the row carries its TDB MJD."""
    seen = []
    real = C.to_bary_equatorial

    def spy(coords):
        out = real(coords)
        seen.append(out)
        return out

    monkeypatch.setattr(C, "to_bary_equatorial", spy)
    res = R.od_one("2008 LC18", RUNNER.parent.parent / "fixtures" / "psv", {})
    assert res["adam_core_converged"] is True
    t = seen[-1].time
    assert t.scale == "utc"
    utc = t.mjd()[0].as_py()
    assert res["adam_core_epoch_mjd_tdb"] == t.rescale("tdb").mjd()[0].as_py()
    # TDB - UTC in 2008: 32.184 s + 33 leap seconds, plus TDB - TT under 2 ms.
    assert 65.182 < (res["adam_core_epoch_mjd_tdb"] - utc) * 86400.0 < 65.186


# ── object legs and the per-object cap ──────────────────────


def _od_plan(path, objects):
    path.write_text(json.dumps([
        {"object": o, "test_type": "orbit_determination", "propagation_uncertainty": "none",
         "dt_days": 0.0, "observer": None, "force_model": "standard"} for o in objects
    ]))
    return path


def test_objects_naming_an_object_outside_the_plan_is_refused_before_anything_loads(tmp_path):
    """Refused by name before the kernels: the data dir here does not exist."""
    plan = _od_plan(tmp_path / "plan.json", ["2008 LC18"])
    r = subprocess.run(
        [sys.executable, str(RUNNER / "run_adam_core.py"), "--input", str(plan), "--output", str(tmp_path / "o.json"),
         "--data-dir", str(tmp_path / "no-such-folder"), "--objects", "2008 LC18,Nope"],
        capture_output=True, text=True, check=False,
    )
    assert r.returncode != 0
    assert "--objects names object(s) the plan does not hold: Nope" in r.stderr
    assert "registered kernel" not in r.stderr and "no adam-core data directory" not in r.stderr


@pytest.mark.skipif(not (RUNNER.parent.parent / "fixtures" / "psv" / "2008 LC18.psv").is_file(),
                    reason="PSV fixtures not fetched (make fixtures)")
def test_a_capped_object_is_a_named_row_and_the_cap_is_recorded(adam_core_dir, tmp_path):
    """The child is killed at the cap; the row says so, carries its wall time, and
    every row's settings record the cap. The log shows a line before and after."""
    plan = _od_plan(tmp_path / "plan.json", ["2008 LC18"])
    out = tmp_path / "o.json"
    r = subprocess.run(
        [sys.executable, str(RUNNER / "run_adam_core.py"), "--input", str(plan), "--output", str(out),
         "--fixtures-dir", str(RUNNER.parent.parent / "fixtures" / "psv"), "--data-dir", str(adam_core_dir),
         "--only", "orbit_determination", "--timeout-min", "0.001"],
        capture_output=True, text=True, check=False,
    )
    assert r.returncode == 0, r.stderr
    (row,) = json.loads(out.read_text())
    assert row["adam_core_timed_out"] is True and row["adam_core_converged"] is False
    assert row["adam_core_unsupported"] == "timed out after 0.001 min"
    assert row["adam_core_wall_s"] >= 0.06
    assert row["adam_core_settings"]["od_timeout_min"].startswith("0.001: each object is fit in its own child process")
    assert "[1/1] 2008 LC18 ... fitting (cap 0.001 min)" in r.stderr
    assert "TIMED OUT" in r.stderr
