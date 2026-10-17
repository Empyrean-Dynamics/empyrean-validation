"""Unit + integration tests for the adam-core runners (run inside the venv).

    runners/adamcore/.venv/bin/python -m pytest runners/adamcore/tests -q

Pure tests (record shapes, PSV normalization) run anywhere. Tests that touch
adam_core / adam-assist are integration tests: they `importorskip` the stack, so
they are skipped when the venv is absent (per the brief).
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
RUNNER = HERE.parent
sys.path.insert(0, str(RUNNER))

import adamcore_common as C  # noqa: E402
import run_adamcore as R  # noqa: E402
import run_adamcore_walk as W  # noqa: E402


# ── pure tests ──────────────────────────────────────────────


def test_normalize_psv_strips_padding():
    raw = "# version=2017\n permID | mode | stn |  obsTime  \n 123 | CCD | 691 | 2020-01-01T00:00:00.000Z \n"
    norm = R.normalize_psv(raw)
    lines = norm.strip().splitlines()
    assert lines[0] == "# version=2017"  # comment preserved verbatim
    assert lines[1] == "permID|mode|stn|obsTime"  # padding stripped
    assert lines[2] == "123|CCD|691|2020-01-01T00:00:00.000Z"


def test_prediction_record_shape():
    obs = {"key_hash": 42, "stn": "691"}
    cov = [[1.0e-8, 2.0e-9], [2.0e-9, 1.5e-8]]
    rec = W.prediction_record("X", "adamcore", "default+sp", 3, obs, 7, True, 10.0, -5.0, cov)
    assert rec["tool"] == "adamcore" and rec["config_arm"] == "default+sp"
    assert rec["obs_idx"] == 7 and rec["key_hash"] == 42 and rec["in_sample"] is True
    assert rec["uncertainty_form"] == "radec_2x2"
    assert rec["cov_units"] == "deg2" and rec["cov_basis"] == "native_ra"
    assert rec["cov_radec"] == cov
    # zero / absent covariance -> uncertainty_form none (never a d^2 of 1e6)
    rec0 = W.prediction_record("X", "adamcore", "default+sp", 3, obs, 7, False, 10.0, -5.0, None)
    assert rec0["uncertainty_form"] == "none" and "cov_radec" not in rec0
    # non-finite position -> dropped
    assert W.prediction_record("X", "adamcore", "default+sp", 3, obs, 7, True, float("nan"), 0.0, None) is None


def test_window_record_shape_failure_and_warm():
    rec = W.window_record("X", "adamcore", "default+mc", 2, False, False, 12.5, failure="not_converged")
    assert rec["tool"] == "adamcore" and rec["config_arm"] == "default+mc"
    assert rec["converged"] is False and rec["failure"] == "not_converged"
    assert rec["warm_start"] is False and rec["seed_source"] == "own_iod"
    assert rec["fit_time_ms"] == 12.5


def test_single_channel_tool_id():
    assert R.TOOL == "adamcore"
    assert not hasattr(R, "channel_id")  # the --weights axis is gone


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
        assert C.force_model_label(o) == "planets-only (self-perturber; SB441-n16 includes the object)"
    for o in ("Apophis", "Eros", "67P"):
        assert not C.is_self_perturber(o)
        assert C.force_model_label(o) == "planets+SB441-n16"


# ── integration tests (need the venv) ───────────────────────

adam_core = pytest.importorskip("adam_core")


def test_data_sigma_table_loads():
    """The default stack's fill table (private v2_sigma_fill) loads on
    VERES2017_SIGMA_TABLE_SCHEMA and classification sets are derivable from it."""
    from adam_core.orbit_determination import VERES2017_SIGMA_TABLE_SCHEMA

    t = C.data_sigma_table()
    assert t.column_names == list(VERES2017_SIGMA_TABLE_SCHEMA.names)
    station, catalog, station_cat, _has_global = C.sigma_table_row_sets(t)
    assert len(station) + len(catalog) + len(station_cat) > 0


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
    # no non-grav attached for a gravity-only row
    ng = orbit.non_gravitational_parameters
    assert ng is None or len(ng) == 0 or ng.A1.null_count == len(ng)


def test_nongrav_attachment_and_dt_unsupported():
    # A-terms + standard g(r) -> attached, no unsupported reason
    row = {"object": "a", "epoch_mjd_tdb": 60000.0, "ic_pos_au": [1, 0, 0],
           "ic_vel_au_d": [0, 0.017, 0], "ic_a1": 5e-13, "ic_a2": -3e-14, "ic_a3": 0.0}
    _orbit, reason = C.plan_orbit(row)
    assert reason is None
    ng, r2 = C.nongrav_for(row)
    assert ng is not None and abs(ng.A1[0].as_py() - 5e-13) < 1e-24
    # a non-zero Marsden DT is unsupported (no DT term in adam_core)
    row_dt = dict(row, ic_non_grav_dt=45.0, ic_g_alpha=0.1, ic_g_r0=2.8, ic_g_m=2.1, ic_g_n=5.0, ic_g_k=4.6)
    _o, reason_dt = C.plan_orbit(row_dt)
    assert reason_dt == "nongrav dt"


def test_from_ades_blank_rms_filled_reported_untouched():
    """SigmaFill fills ONLY missing sigmas; reported sigmas pass through untouched."""
    from adam_core.orbit_determination import SigmaFillModel, apply_observation_models

    header = (
        "# version=2017\n"
        "permID|provID|trkSub|mode|stn|obsTime|ra|dec|rmsRA|rmsDec|rmsCorr|astCat|mag|band\n"
    )
    # Two reported-rms rows (0.30") and two blank-rms rows, ground station 691, Gaia2.
    rows = [
        "T|t|t|CCD|691|2020-01-01T00:00:00.000Z|10.00000|5.00000|0.30|0.30|0.0|Gaia2|20|G",
        "T|t|t|CCD|691|2020-01-01T00:10:00.000Z|10.01000|5.01000|0.30|0.30|0.0|Gaia2|20|G",
        "T|t|t|CCD|691|2020-01-02T00:00:00.000Z|10.05000|5.05000|||0.0|Gaia2|20|G",
        "T|t|t|CCD|691|2020-01-02T00:10:00.000Z|10.06000|5.06000|||0.0|Gaia2|20|G",
    ]
    import math

    ok = R.resolvable_fn()
    odo, n_dropped, _d, n_parsed, rms_present = R.build_odo(header + "\n".join(rows) + "\n", ok)
    assert n_dropped == 0 and n_parsed == 4 and len(odo) == 4
    # ODO astrometry lives on `coordinates` (SphericalCoordinates); sigma_lon is
    # an ndarray, NaN where no rms was supplied.
    orig = list(odo.coordinates.sigma_lon)
    reported_mask = [not math.isnan(v) for v in orig]
    assert sum(reported_mask) == 2 and sum(not m for m in reported_mask) == 2
    # the default stack's SigmaFillModel() (v2_sigma_fill) fills only the blanks
    used = apply_observation_models(odo, SigmaFillModel())
    used_sig = list(used.coordinates.sigma_lon)
    assert all(not math.isnan(v) for v in used_sig)  # every blank now filled
    # the two reported sigmas are untouched by the fill
    for o, u, rep in zip(orig, used_sig, reported_mask):
        if rep:
            assert math.isclose(o, u, rel_tol=1e-9)

    # the sigma-source counter reads the ORIGINAL covariance (not the fitter's
    # members): 2 reported, 2 filled (by some table level), 0 defaulted.
    ss, n_def = R._sigma_source(odo, rms_present)
    assert ss["reported"] == 2
    assert ss["filled"] == 2
    assert sum(ss["filled_by_level"].values()) == 2
    assert ss["defaulted_by_adamcore"] == 0
    assert ss["total"] == 4


def test_defaulted_by_adamcore_measured():
    """A finite lon/lat variance on a row that had NO ADES rms counts as
    defaulted_by_adamcore (a from_ades default), never as reported."""
    from adam_core.orbit_determination import SigmaFillModel, apply_observation_models  # noqa: F401

    header = (
        "# version=2017\n"
        "permID|provID|trkSub|mode|stn|obsTime|ra|dec|rmsRA|rmsDec|rmsCorr|astCat|mag|band\n"
    )
    # One row WITH rms, so from_ades gives it a finite lon/lat variance.
    row = "T|t|t|CCD|691|2020-01-01T00:00:00.000Z|10.0|5.0|0.30|0.30|0.0|Gaia2|20|G"
    ok = R.resolvable_fn()
    odo, _d, _dr, n_parsed, rms = R.build_odo(header + row + "\n", ok)
    assert n_parsed == 1 and rms == [True]
    # Tell the counter the rms was ABSENT (simulating a from_ades default): the finite
    # variance must now be classified defaulted_by_adamcore == 1, not reported.
    ss, n_def = R._sigma_source(odo, [False])
    assert n_def == 1
    assert ss["defaulted_by_adamcore"] == 1
    assert ss["reported"] == 0
    assert ss["filled"] == 0
    assert ss["total"] == 1


def test_planets_only_disables_asteroid_forces():
    """The planets-only propagator must actually disable ASSIST asteroid forces:
    its 10-yr probe diverges from the default (full) model by ~19 km, not 0."""
    import adamcore_common as _c

    _c._PLANETS_ONLY_DIVERGENCE_KM = None  # reset the per-process memo
    div = C.verify_planets_only_active(C.planets_only_propagator())
    assert div > 1.0  # measured ~19 km; a silent revert to the full model would give ~0


@pytest.mark.skipif(not (RUNNER.parent.parent / "fixtures" / "psv").is_dir(),
                    reason="PSV fixtures not fetched (make fixtures)")
def test_build_odo_drops_space_based():
    """A fixture with space-based rows yields a positive dropped count and a fittable ODO."""
    fx = RUNNER.parent.parent / "fixtures" / "psv" / "2010 TK7.psv"
    if not fx.exists():
        pytest.skip("2010 TK7 fixture absent")
    ok = R.resolvable_fn()
    odo, n_dropped, droppable, n_parsed, _rms = R.build_odo(fx.read_text(), ok)
    assert n_parsed > 0
    assert n_dropped > 0 and droppable  # C51/C57/250 etc. dropped
    assert odo is not None and len(odo) == n_parsed - n_dropped
