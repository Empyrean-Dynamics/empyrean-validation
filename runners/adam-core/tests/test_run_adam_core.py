"""Unit + integration tests for the adam-core runners (run inside the venv).

    runners/adam-core/.venv/bin/python -m pytest runners/adam-core/tests -q

Pure tests (record shapes, PSV normalization, self-perturber labels) run anywhere.
Tests that touch adam_core / adam-assist are integration tests: they ``importorskip``
the stack, so they are skipped when the venv is absent.
"""

from __future__ import annotations

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
    # the two reported sigmas (0.30") survive into the catalog untouched;
    # the two blanks are filled with station 691's 0.5".
    sra = list(catalog.ra_sigma.to_numpy(zero_copy_only=False))
    assert sum(1 for v in sra if math.isclose(v, 0.30, rel_tol=1e-9)) == 2
    assert sum(1 for v in sra if math.isclose(v, 0.50, rel_tol=1e-9)) == 2
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
    adam-assist 0.6.0: construction must RAISE (strict DAF binding). This is the
    proof behind the self-perturber policy."""
    reason = C.missing_asteroid_path_raises()
    assert "RuntimeError" in reason or "kernel" in reason.lower()


def test_od_one_self_perturber_unsupported():
    """A self-perturber OD is a NAMED unsupported row (no self-singular state),
    because adam-assist 0.6.0 has no planets-only force model."""
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
    assert "accepts no path" in rec["obscodes_source"]


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
