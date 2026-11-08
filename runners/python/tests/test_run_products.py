"""The python validation channel produces every method row the plan carries,
reporting ITS OWN 0.11 per-method products (never the input channel's).

These tests run the real dist-stable engine through the wheel. They pin:

* every uncertainty method yields one result row tagged with its method, with
  the python-channel products populated off the delivered packed joint / the
  per-orbit outcome table / the retained mixture components — and the
  SecondOrder row reports the delivered kind ``second_order`` (the discriminator
  a stale engine, or a dropped-method bug, fails as ``linear``);
* the 12 inherited per-method fields are reset — a bogus input ``cov_kind`` does
  not ride out under this channel's name;
* the per-orbit outcome is the status table's string, not a row count;
* an OD fit row runs the fit under its tag's method: ``first_order`` delivers
  and is bit-identical to the legacy untagged fit, while ``second_order`` is
  refused by the engine BY NAME (``orbit_delivered = False``);
* the restated schema constants (method ints, cov-kind wire codes, the MC
  sample count + seed) match the distribution / schema so the literals cannot
  drift silently.

Mutation-proven (cp backup → edit run.py → red → restore → cmp) in the build
report; here the assertions are the green side.
"""

from __future__ import annotations

import json
import os
import re
import sys
from pathlib import Path

import pytest
import run

_REPO = Path(__file__).resolve().parents[3]
_DATA_DIR = "/Users/moeyensj/projects/empyrean/data"
_FIXTURES = _REPO / "fixtures" / "psv"

# The schema crate is the single source of truth for the Monte-Carlo sample
# count / seed and the OD method-axis note; run.py mirrors them as Python
# literals (it cannot import a Rust const). The pins below parse the constants
# straight out of this file and compare run.py to THOSE, so a drift in either
# the mirror OR the schema turns the pin red — a hand-copied second spelling in
# the test could not. Module-level (not a default arg) so a test can point the
# parser at a mutated scratch copy via monkeypatch and prove the pin would go
# red.
_SCHEMA_RS = _REPO / "src" / "schema.rs"


def _parse_schema_constants(schema_path: Path) -> dict[str, object]:
    """Parse ``MONTE_CARLO_SAMPLE_COUNT`` and ``MONTE_CARLO_SEED`` out of a
    ``schema.rs`` by a small regex on the ``pub const`` lines (the seed's
    ``0x..._...`` hex is parsed to an int).
    """
    text = Path(schema_path).read_text()

    def _group(pattern: str) -> str:
        m = re.search(pattern, text, re.DOTALL)
        assert m is not None, f"{pattern!r} not found in {schema_path}"
        return m.group(1)

    count = int(_group(r"pub const MONTE_CARLO_SAMPLE_COUNT:\s*u32\s*=\s*(\d+)\s*;"))
    seed_hex = _group(r"pub const MONTE_CARLO_SEED:\s*u64\s*=\s*(0x[0-9A-Fa-f_]+)\s*;")
    seed = int(seed_hex.replace("_", ""), 16)
    return {"count": count, "seed": seed}

# A valid heliocentric ICRF/SSB Cartesian state (the runner builds every orbit
# in Frame::ICRF, Origin::SSB), epoch MJD 61200 TDB — a real NEO state.
_IC_POS = [-0.9259598226174744, 0.5586961894066496, 0.1841364244042053]
_IC_VEL = [-0.008132198510292572, -0.01147982080464957, -0.004470724867546879]
_EPOCH = 61200.0

_METHOD_TAGS = [
    "none_detection_on",
    "first_order_detection_on",
    "second_order_detection_on",
    "auto_detection_on",
    "sigma_point_detection_on",
    "monte_carlo_detection_on",
    "gaussian_mixture_detection_on",
]


def _prop_row(tag: str) -> dict:
    """One synthetic propagation plan row carrying BOGUS inherited per-method
    fields (as if copied from a rust input row) so the reset can be observed."""
    return {
        "object": "TestNEO",
        "population": "NEO",
        "epoch_mjd_tdb": _EPOCH,
        "t_mjd_tdb": _EPOCH + 365.0,
        "dt_days": 365.0,
        "force_model": "standard",
        "test_type": "propagation",
        "propagation_uncertainty": tag,
        "ic_pos_au": _IC_POS,
        "ic_vel_au_d": _IC_VEL,
        "notes": "",
        # The leak the reset must clear — a foreign channel's products:
        "cov_kind": 5,
        "cov_joint_width": 99,
        "cov_tri": [1.0, 2.0, 3.0],
        "resolved_method": "sigma_point",
        "orbit_delivered": False,
        "orbit_status": "LEAK",
        "emp_pos_cov_au2": [[9.0, 9.0, 9.0], [9.0, 9.0, 9.0], [9.0, 9.0, 9.0]],
    }


def _run(rows: list[dict], tmp_path: Path) -> list[dict]:
    """Run the channel's ``main`` on an in-memory plan, return the output rows."""
    inp = tmp_path / "plan.json"
    out = tmp_path / "out.json"
    inp.write_text(json.dumps(rows))
    argv = [
        "run.py",
        "--input",
        str(inp),
        "--output",
        str(out),
        "--data-dir",
        _DATA_DIR,
        "--n-timing-runs",
        "1",
        "--fixtures-dir",
        str(_FIXTURES),
    ]
    old = sys.argv
    sys.argv = argv
    try:
        rc = run.main()
    finally:
        sys.argv = old
    assert rc == 0, f"runner exited {rc}"
    return json.loads(out.read_text())


@pytest.fixture(scope="module")
def propagation_rows(tmp_path_factory) -> dict[str, dict]:
    """Run one object under every method once; key the output rows by method."""
    tmp = tmp_path_factory.mktemp("prop")
    rows = _run([_prop_row(t) for t in _METHOD_TAGS], tmp)
    by_tag = {r["propagation_uncertainty"]: r for r in rows}
    assert set(by_tag) == set(_METHOD_TAGS), "one result row per requested method"
    return by_tag


def test_every_method_row_reports_its_products(propagation_rows) -> None:
    """Each attach-covariance method yields a packed joint + resolved kind; the
    SecondOrder row's DELIVERED kind is second_order (a dropped-method bug or a
    stale engine would report linear), and the ``none`` row carries no
    covariance."""
    so = propagation_rows["second_order_detection_on"]
    assert so["resolved_method"] == "second_order_detection_on"
    assert so["cov_kind"] == 1  # second-order wire discriminant
    assert so["cov_joint_width"] == 6
    assert len(so["cov_tri"]) == 21  # 6*7/2 packed lower triangle
    assert isinstance(so["emp_pos_cov_au2"], list) and len(so["emp_pos_cov_au2"]) == 3

    fo = propagation_rows["first_order_detection_on"]
    assert fo["resolved_method"] == "first_order_detection_on"
    assert fo["cov_kind"] == 0  # linear wire discriminant

    f64 = propagation_rows["none_detection_on"]
    assert f64["resolved_method"] is None
    assert f64["cov_kind"] is None
    assert f64["cov_tri"] is None
    assert f64["emp_pos_cov_au2"] is None
    # Still a delivered orbit — the outcome does not depend on a covariance.
    assert f64["orbit_delivered"] is True


def test_inherited_per_method_fields_reset_no_leak(propagation_rows) -> None:
    """The bogus input cov_kind=5 must not ride out: a covariance-bearing row
    reports the python-delivered discriminant, an f64 row reports None."""
    so = propagation_rows["second_order_detection_on"]
    assert so["cov_kind"] == 1, "input cov_kind=5 leaked into the python row"
    assert so["cov_joint_width"] == 6, "input cov_joint_width=99 leaked"
    assert so["orbit_status"] != "LEAK", "input orbit_status leaked"
    assert so["emp_pos_cov_au2"] != [[9.0] * 3] * 3, "input moment view leaked"

    f64 = propagation_rows["none_detection_on"]
    assert f64["cov_kind"] is None, "cov_kind not reset on the f64 row"
    assert f64["resolved_method"] is None


def test_outcome_from_status_table_not_row_count(propagation_rows) -> None:
    """orbit_status is the status table's string (``delivered``), which a row
    count cannot produce — the mutation that derives it from len(states) → red."""
    for tag in _METHOD_TAGS:
        row = propagation_rows[tag]
        assert row["orbit_delivered"] is True
        assert row["orbit_status"] == "delivered"


# ── Ephemeris method rows ───────────────────────────────────────────────────

_EPH_METHOD_TAGS = ["first_order_detection_on", "second_order_detection_on"]


def _eph_row(tag: str) -> dict:
    """One synthetic geocentric (``500``) ephemeris plan row, carrying BOGUS
    inherited per-method fields so the fill can be observed to overwrite them."""
    return {
        "object": "TestNEO",
        "population": "NEO",
        "epoch_mjd_tdb": _EPOCH,
        "t_mjd_tdb": _EPOCH + 30.0,
        "dt_days": 30.0,
        "force_model": "standard",
        "test_type": "ephemeris",
        "propagation_uncertainty": tag,
        "ic_pos_au": _IC_POS,
        "ic_vel_au_d": _IC_VEL,
        "observer": "500",
        "notes": "",
        # The leak the reset must clear — a foreign channel's products:
        "cov_kind": 5,
        "cov_joint_width": 99,
        "cov_tri": [1.0, 2.0, 3.0],
        "resolved_method": "sigma_point",
    }


@pytest.fixture(scope="module")
def ephemeris_rows(tmp_path_factory) -> dict[str, dict]:
    """Run one geocentric ephemeris row under first_order + second_order."""
    tmp = tmp_path_factory.mktemp("eph")
    rows = _run([_eph_row(t) for t in _EPH_METHOD_TAGS], tmp)
    by_tag = {r["propagation_uncertainty"]: r for r in rows}
    assert set(by_tag) == set(_EPH_METHOD_TAGS), "one ephemeris row per method"
    return by_tag


def test_ephemeris_second_order_row_carries_the_delivered_joint(ephemeris_rows) -> None:
    """From-engine: the second_order ephemeris row carries the delivered sky
    joint read off the entry's own ``joint`` column — kind 1 (second-order),
    width 6, a 21-cell lower triangle, ``resolved_method``
    ``second_order_detection_on`` — and, being a non-mixture row, every ``mix_*``
    tally ABSENT (``None``, never 0). This is the re-bind of the ephemeris method
    rows; before it these 12 fields were null (the old named gap). The bogus
    inherited ``cov_kind=5`` must not survive. Mutation: make
    ``_fill_ephemeris_products`` skip the joint read → ``cov_kind`` null → red."""
    so = ephemeris_rows["second_order_detection_on"]
    assert so["cov_kind"] == 1, "input cov_kind=5 leaked or the joint was not read"
    assert so["cov_joint_width"] == 6
    assert len(so["cov_tri"]) == 21  # 6*7/2 packed lower triangle
    assert so["resolved_method"] == "second_order_detection_on"
    # A second-order row is not a mixture: every tally ABSENT (None), never 0.
    assert so["mix_n_components_total"] is None
    assert so["mix_weight_delivered"] is None
    assert so["mix_n_failed"] is None
    assert so["mix_n_unresolved"] is None
    assert so["mix_n_curvature_refused"] is None
    assert so["mix_n_sky_linearization_refused"] is None


def test_ephemeris_first_order_row_kind_zero(ephemeris_rows) -> None:
    """The first_order ephemeris row's delivered joint is tagged kind 0 (linear)
    and its ``resolved_method`` is ``first_order_detection_on`` — the delivered
    joint rides beside the published sky covariance (this channel publishes the
    engine-delivered covariance; the diagnostic lives on the rust channel)."""
    fo = ephemeris_rows["first_order_detection_on"]
    assert fo["cov_kind"] == 0
    assert fo["cov_joint_width"] == 6
    assert len(fo["cov_tri"]) == 21
    assert fo["resolved_method"] == "first_order_detection_on"


def _od_row(tag) -> dict:
    """One OD fit plan row for the 2018 LA short-arc impactor fixture (a fast
    fit), carrying the given method tag (``None`` = the legacy untagged row)."""
    return {
        "object": "2018 LA",
        "population": "NEO",
        "epoch_mjd_tdb": _EPOCH,
        "t_mjd_tdb": _EPOCH,
        "dt_days": 0.0,
        "force_model": "standard",
        "test_type": "orbit_determination",
        "propagation_uncertainty": tag,
        "notes": "catalog note",
    }


def test_od_fit_under_first_order_is_bit_identical_to_the_legacy_fit(tmp_path) -> None:
    """From-engine: the ``first_order`` OD fit delivers and is bit-identical to
    the legacy untagged fit (same config, default method) — the fitted position,
    χ² and residual RMS match field for field — while carrying the first-order
    products (``resolved_method`` / ``cov_kind`` / the joint). The ``none`` row
    delivers the same fit covariance-free."""
    assert (_FIXTURES / "2018 LA.psv").exists(), "expected the 2018 LA PSV fixture"
    out = _run(
        [_od_row(None), _od_row("first_order_detection_on"), _od_row("none_detection_on")],
        tmp_path,
    )
    fits = {r["propagation_uncertainty"]: r for r in out if r["test_type"] == "orbit_determination"}
    legacy = fits[None]
    fo = fits["first_order_detection_on"]
    none = fits["none_detection_on"]

    assert legacy["od_converged"] is True
    # Bit-identity of the FIT: the per-method config differs from the legacy
    # config by the method name alone, and FirstOrder is the default, so the
    # delivered numbers are identical.
    assert fo["emp_pos_au"] == legacy["emp_pos_au"], "fitted state must match the legacy fit"
    assert fo["od_chi2"] == legacy["od_chi2"]
    assert fo["od_rms_combined_arcsec"] == legacy["od_rms_combined_arcsec"]
    assert fo["n_obs_used"] == legacy["n_obs_used"]
    # The first_order row carries the first-order products; the legacy row does
    # not (no method tag).
    assert fo["orbit_delivered"] is True
    assert fo["resolved_method"] == "first_order_detection_on"
    assert fo["cov_kind"] == 0
    assert fo["cov_joint_width"] == 6
    assert len(fo["cov_tri"]) == 21
    assert legacy["resolved_method"] is None and legacy["cov_kind"] is None
    # `none` delivers the same fit covariance-free.
    assert none["emp_pos_au"] == legacy["emp_pos_au"]
    assert none["orbit_delivered"] is True
    assert none["resolved_method"] is None and none["cov_tri"] is None


def test_od_fit_under_second_order_carries_the_engine_refusal_by_name(tmp_path) -> None:
    """From-engine: the ``second_order`` OD fit is refused by the engine BY NAME
    — ``orbit_delivered`` False, the engine's refusal text (which names
    ``SecondOrder``) in ``orbit_status``, no joint, and the method in ``notes`` —
    rather than silently composing a first-order posterior under the method's
    name. (Mutation: swallowing the refusal into a delivered row drops
    ``orbit_delivered = False``.)"""
    assert (_FIXTURES / "2018 LA.psv").exists(), "expected the 2018 LA PSV fixture"
    out = _run([_od_row("second_order_detection_on")], tmp_path)
    fits = [r for r in out if r["test_type"] == "orbit_determination"]
    assert fits, "the refused OD fit row was dropped (must be a row, never silent)"
    so = fits[0]
    assert so["orbit_delivered"] is False, "a refused method must not deliver a row"
    assert "SecondOrder" in (so["orbit_status"] or ""), (
        f"orbit_status must carry the engine refusal by name: {so['orbit_status']!r}"
    )
    assert so["cov_kind"] is None and so["resolved_method"] is None
    assert "second_order" in so["notes"], "notes must name the refused method"
    assert "catalog note" in so["notes"], "the base note must be preserved"


# ── Restated-constant pins (no silent drift from the schema / distribution) ──


def test_method_ints_match_the_distribution() -> None:
    """The wire ints this channel lowers to must match the distribution's own
    UncertaintyMethod→int map (FIRST_ORDER 0 … GAUSSIAN_MIXTURE 5)."""
    from empyrean import UncertaintyMethod as UM

    assert run._UNCERTAINTY_METHOD_TO_INT[UM.FIRST_ORDER] == 0
    assert run._UNCERTAINTY_METHOD_TO_INT[UM.SECOND_ORDER] == 1
    assert run._UNCERTAINTY_METHOD_TO_INT[UM.SIGMA_POINT] == 2
    assert run._UNCERTAINTY_METHOD_TO_INT[UM.MONTE_CARLO] == 3
    assert run._UNCERTAINTY_METHOD_TO_INT[UM.AUTO] == 4
    assert run._UNCERTAINTY_METHOD_TO_INT[UM.GAUSSIAN_MIXTURE] == 5


def test_cov_kind_wire_matches_the_distribution() -> None:
    """The restated cov_kind wire discriminants match the distribution's own
    CovarianceKind→code map (linear 0, second_order 1, mixture 3, mc 4, sp 5)."""
    from empyrean.orbits.joint import _KIND_TO_CODE

    assert run._COV_KIND_WIRE == _KIND_TO_CODE


def test_monte_carlo_constants_match_the_schema() -> None:
    """run.py's MONTE_CARLO_SAMPLE_COUNT / MONTE_CARLO_SEED equal the values
    parsed out of src/schema.rs — so a change to either the run.py mirror or the
    schema const turns this red (a hand-copied literal here could not)."""
    consts = _parse_schema_constants(_SCHEMA_RS)
    assert run._MONTE_CARLO_SAMPLE_COUNT == consts["count"]
    assert run._MONTE_CARLO_SEED == consts["seed"]


def test_schema_pins_detect_schema_drift(tmp_path) -> None:
    """Tripwire: the pins read the live schema.rs, so a drift there turns them
    red. Parse a scratch copy whose constants are mutated and confirm the parsed
    values no longer equal the run.py mirrors (i.e. the pins above would fail)."""
    scratch = tmp_path / "schema.rs"
    text = _SCHEMA_RS.read_text()
    mutated = text.replace(
        "pub const MONTE_CARLO_SAMPLE_COUNT: u32 = 100;",
        "pub const MONTE_CARLO_SAMPLE_COUNT: u32 = 101;",
    )
    assert mutated != text, "the scratch mutation did not apply"
    scratch.write_text(mutated)

    consts = _parse_schema_constants(scratch)
    assert consts["count"] == 101
    assert run._MONTE_CARLO_SAMPLE_COUNT != consts["count"]


def test_synthetic_covariance_matches_the_rust_channel() -> None:
    """1 km (1σ) position, 1 mm·s⁻¹ (1σ) velocity, uncorrelated — bit-identical
    to the rust / cli channels' synthetic_covariance."""
    c = run._synthetic_covariance()
    assert c.shape == (6, 6)
    pos = (1.0 / 149_597_870.700) ** 2
    vel = (1e-6 / 149_597_870.700 * 86_400.0) ** 2
    for i in range(3):
        assert c[i, i] == pos
        assert c[i + 3, i + 3] == vel
    # Uncorrelated: every off-diagonal exactly zero.
    for i in range(6):
        for j in range(6):
            if i != j:
                assert c[i, j] == 0.0


# ── Perturber exclusions and declined orbits ─────────────────────────────────

_PLAN = _REPO / "results" / "validation_plan.json"


def _vesta_eph_row(tag: str) -> dict | None:
    """Vesta's F51 dt=+365 ephemeris plan row under ``tag`` (a self-perturber
    row: ``excluded_perturbers_naif`` names Vesta itself), read off the staged
    plan; ``None`` when no plan is staged."""
    if not _PLAN.exists():
        return None
    for r in json.loads(_PLAN.read_text()):
        if (
            r.get("object") == "Vesta"
            and r.get("test_type") == "ephemeris"
            and r.get("observer") == "F51"
            and r.get("dt_days") == 365.0
            and r.get("propagation_uncertainty") == tag
        ):
            return r
    return None


def test_write_declined_records_the_refusal_without_residuals() -> None:
    """A declined orbit is recorded on its row by name — ``orbit_delivered``
    false, the ``failed:<variant>`` status, the engine's text in ``notes`` — and
    its measured fields are left untouched (the caller resets them to null)."""
    row = {"notes": "SB441-N16 perturber", "d_ra_arcsec": None}
    run._write_declined(row, run._Declined("failed:other", "sampled sky delivery refused"))
    assert row["orbit_delivered"] is False
    assert row["orbit_status"] == "failed:other"
    assert row["notes"] == (
        "SB441-N16 perturber; engine declined the orbit: sampled sky delivery refused"
    )
    assert row["d_ra_arcsec"] is None


@pytest.mark.skipif(
    _vesta_eph_row("sigma_point_detection_on") is None, reason="no staged plan"
)
def test_vesta_exclusion_delivers_and_its_absence_is_a_declined_row(tmp_path, capsys) -> None:
    """From-engine, on Vesta's own self-perturber row: with the plan's exclusion
    the sigma-point ephemeris DELIVERS (the core channel's request), with finite
    residuals; with the exclusion forced off the engine declines the orbit, and
    the runner writes ONE declined row naming the engine's text (never a skip,
    never a NaN residual)."""
    r = _vesta_eph_row("sigma_point_detection_on")
    forced = dict(r, excluded_perturbers_naif=[], notes="forced")
    rows = _run([r, forced], tmp_path)
    assert len(rows) == 2, "a declined orbit is a row, not a skip"
    delivered, declined = rows
    assert delivered["orbit_delivered"] is True
    assert delivered["orbit_status"] == "delivered"
    for f in ("d_ra_arcsec", "d_dec_arcsec", "d_rho_km"):
        assert delivered[f] is not None and delivered[f] == delivered[f], f
    assert declined["orbit_delivered"] is False
    assert declined["orbit_status"] == "failed:other"
    assert "sampled sky delivery refused" in declined["notes"], declined["notes"]
    for f in ("emp_time_ms", "separation_arcsec", "d_ra_arcsec", "d_dec_arcsec", "d_rho_km"):
        assert declined[f] is None, f
    err = capsys.readouterr().err
    assert "Loaded 2 rows; wrote 1, declined 1, skipped 0 (1 + 1 + 0 = 2)" in err, err


def test_row_account_line_is_honest() -> None:
    """Every plan row is counted once — written, declined or skipped — so
    W + D + S = N; a declined row is not also counted as skipped, and the
    non-grav row the legacy fit's second pass generates stands for the plan's
    non_grav_recovery row. A surplus row fails the account."""
    acct = run._RowAccount(5)
    out: list = []
    plan = [
        {"test_type": "propagation"},
        {"test_type": "ephemeris"},
        {"test_type": "orbit_determination_transport"},
        {"test_type": "orbit_determination"},
        {"test_type": "non_grav_recovery"},
    ]
    acct.begin(plan[0], out)
    out.append({"test_type": "propagation"})
    acct.begin(plan[1], out)
    out.append(acct.declined_row({"test_type": "ephemeris", "orbit_delivered": False}))
    acct.begin(plan[2], out)  # transport: not produced, no row
    acct.begin(plan[3], out)
    out.append({"test_type": "non_grav_recovery"})  # the second pass's row
    out.append({"test_type": "orbit_determination"})
    acct.begin(plan[4], out)  # the plan's non-grav row: written by the pass above
    line, ok = acct.line(out, "CH", "out.json")
    assert ok, line
    assert line.startswith("Loaded 5 rows; wrote 3, declined 1, skipped 1 (3 + 1 + 1 = 5); 4 CH rows")
    # A plan non-grav row with no generated row is skipped; a surplus row fails.
    acct = run._RowAccount(2)
    out = []
    acct.begin({"test_type": "non_grav_recovery"}, out)
    acct.begin({"test_type": "propagation"}, out)
    out.extend([{"test_type": "propagation"}, {"test_type": "propagation"}])
    w, d, s, ok = acct.finish(out)
    assert (w, d, s, ok) == (1, 0, 1, False)


if __name__ == "__main__":
    os.environ.setdefault("EMPYREAN_DATA_DIR", _DATA_DIR)
    raise SystemExit(pytest.main([__file__, "-v"]))
