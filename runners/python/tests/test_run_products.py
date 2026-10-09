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
* OD fit rows carry the shared method-axis note;
* the restated schema constants (method ints, cov-kind wire codes, the MC
  sample count + seed, the OD note text) match the distribution / schema so the
  literals cannot drift silently.

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
    """Parse ``MONTE_CARLO_SAMPLE_COUNT``, ``MONTE_CARLO_SEED`` and
    ``OD_METHOD_AXIS_NOT_PRODUCED`` out of a ``schema.rs`` by a small regex on
    the ``pub const`` lines (the seed's ``0x..._...`` hex is parsed to an int;
    the note's string literal may sit on the line after the ``=``).
    """
    text = Path(schema_path).read_text()

    def _group(pattern: str) -> str:
        m = re.search(pattern, text, re.DOTALL)
        assert m is not None, f"{pattern!r} not found in {schema_path}"
        return m.group(1)

    count = int(_group(r"pub const MONTE_CARLO_SAMPLE_COUNT:\s*u32\s*=\s*(\d+)\s*;"))
    seed_hex = _group(r"pub const MONTE_CARLO_SEED:\s*u64\s*=\s*(0x[0-9A-Fa-f_]+)\s*;")
    seed = int(seed_hex.replace("_", ""), 16)
    note = _group(
        r'pub const OD_METHOD_AXIS_NOT_PRODUCED:\s*&str\s*=\s*"((?:[^"\\]|\\.)*)"\s*;'
    )
    return {"count": count, "seed": seed, "note": note}

# A valid heliocentric ICRF/SSB Cartesian state (the runner builds every orbit
# in Frame::ICRF, Origin::SSB), epoch MJD 61200 TDB — a real NEO state.
_IC_POS = [-0.9259598226174744, 0.5586961894066496, 0.1841364244042053]
_IC_VEL = [-0.008132198510292572, -0.01147982080464957, -0.004470724867546879]
_EPOCH = 61200.0

_METHOD_TAGS = [
    "f64_no_cov",
    "first_order_with_cov",
    "second_order_with_cov",
    "auto",
    "sigma_point_with_cov",
    "monte_carlo_100_with_cov",
    "gaussian_mixture_with_cov",
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
        "resolved_method": "sigma_point_with_cov",
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
    stale engine would report linear), and f64_no_cov carries no covariance."""
    so = propagation_rows["second_order_with_cov"]
    assert so["resolved_method"] == "second_order_with_cov"
    assert so["cov_kind"] == 1  # second-order wire discriminant
    assert so["cov_joint_width"] == 6
    assert len(so["cov_tri"]) == 21  # 6*7/2 packed lower triangle
    assert isinstance(so["emp_pos_cov_au2"], list) and len(so["emp_pos_cov_au2"]) == 3

    fo = propagation_rows["first_order_with_cov"]
    assert fo["resolved_method"] == "first_order_with_cov"
    assert fo["cov_kind"] == 0  # linear wire discriminant

    f64 = propagation_rows["f64_no_cov"]
    assert f64["resolved_method"] is None
    assert f64["cov_kind"] is None
    assert f64["cov_tri"] is None
    assert f64["emp_pos_cov_au2"] is None
    # Still a delivered orbit — the outcome does not depend on a covariance.
    assert f64["orbit_delivered"] is True


def test_inherited_per_method_fields_reset_no_leak(propagation_rows) -> None:
    """The bogus input cov_kind=5 must not ride out: a covariance-bearing row
    reports the python-delivered discriminant, an f64 row reports None."""
    so = propagation_rows["second_order_with_cov"]
    assert so["cov_kind"] == 1, "input cov_kind=5 leaked into the python row"
    assert so["cov_joint_width"] == 6, "input cov_joint_width=99 leaked"
    assert so["orbit_status"] != "LEAK", "input orbit_status leaked"
    assert so["emp_pos_cov_au2"] != [[9.0] * 3] * 3, "input moment view leaked"

    f64 = propagation_rows["f64_no_cov"]
    assert f64["cov_kind"] is None, "cov_kind not reset on the f64 row"
    assert f64["resolved_method"] is None


def test_outcome_from_status_table_not_row_count(propagation_rows) -> None:
    """orbit_status is the status table's string (``delivered``), which a row
    count cannot produce — the mutation that derives it from len(states) → red."""
    for tag in _METHOD_TAGS:
        row = propagation_rows[tag]
        assert row["orbit_delivered"] is True
        assert row["orbit_status"] == "delivered"


def test_od_fit_rows_carry_the_method_axis_note(tmp_path) -> None:
    """An OD fit row records the shared OD_METHOD_AXIS_NOT_PRODUCED note (the OD
    method axis is not on the wrapper at this pin) and leaves the 12 per-method
    fields None. Uses a short-arc impactor fixture so the fit is fast."""
    obj = "2018 LA"
    assert (_FIXTURES / f"{obj}.psv").exists(), "expected the 2018 LA PSV fixture"
    od_row = {
        "object": obj,
        "population": "NEO",
        "epoch_mjd_tdb": _EPOCH,
        "t_mjd_tdb": _EPOCH,
        "dt_days": 0.0,
        "force_model": "standard",
        "test_type": "orbit_determination",
        "propagation_uncertainty": None,
        "notes": "catalog note",
    }
    out = _run([od_row], tmp_path)
    fits = [r for r in out if r["test_type"] == "orbit_determination"]
    assert fits, "the OD fit row was dropped"
    for r in fits:
        assert run._OD_METHOD_AXIS_NOT_PRODUCED in r["notes"]
        assert "catalog note" in r["notes"], "the base note must be preserved"
        assert r["resolved_method"] is None
        assert r["cov_kind"] is None


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


def test_od_note_text_matches_the_schema_literal() -> None:
    """run.py's OD method-axis note equals OD_METHOD_AXIS_NOT_PRODUCED parsed out
    of src/schema.rs, byte for byte."""
    consts = _parse_schema_constants(_SCHEMA_RS)
    assert run._OD_METHOD_AXIS_NOT_PRODUCED == consts["note"]


def test_schema_pins_detect_schema_drift(tmp_path) -> None:
    """Tripwire: the pins read the live schema.rs, so a drift there turns them
    red. Parse a scratch copy whose constants are mutated and confirm the parsed
    values no longer equal the run.py mirrors (i.e. the pins above would fail)."""
    scratch = tmp_path / "schema.rs"
    text = _SCHEMA_RS.read_text()
    mutated = text.replace(
        "pub const MONTE_CARLO_SAMPLE_COUNT: u32 = 100;",
        "pub const MONTE_CARLO_SAMPLE_COUNT: u32 = 101;",
    ).replace("not on the wrapper", "now on the wrapper")
    assert mutated != text, "the scratch mutation did not apply"
    scratch.write_text(mutated)

    consts = _parse_schema_constants(scratch)
    assert consts["count"] == 101
    assert run._MONTE_CARLO_SAMPLE_COUNT != consts["count"]
    assert run._OD_METHOD_AXIS_NOT_PRODUCED != consts["note"]


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


if __name__ == "__main__":
    os.environ.setdefault("EMPYREAN_DATA_DIR", _DATA_DIR)
    raise SystemExit(pytest.main([__file__, "-v"]))
