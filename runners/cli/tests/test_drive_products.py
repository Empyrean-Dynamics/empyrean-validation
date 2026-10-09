"""The cli validation channel carries the binary's per-method products into the
JSON rows.

``drive.py`` sends each plan row's ``propagation_uncertainty`` method as the
optional 19th daemon ``prop`` token (all tags but ``f64_no_cov`` / the untagged
legacy row) and parses the delivered 0.11 product tokens the binary appends
after the 8 fixed fields into the schema's 12 per-method fields. These tests pin:

* the product tokens of a captured SecondOrder response parse to the schema
  types (``resolved_method``, ``cov_kind`` == 1, ``cov_tri`` of length 21,
  ``orbit_delivered`` is True) — the discriminator a dropped-token bug fails;
* a token-less (``f64`` / legacy) response leaves all 12 per-method fields null
  and the 8 fixed fields exactly as before — byte-identical, and an inherited
  foreign product is reset (no leak);
* the method token is sent for every non-``f64`` row and absent for ``f64`` /
  the untagged row (the line builder);
* the OD method-axis note equals the schema constant byte-for-byte and is
  appended to an OD fit row's base note;
* (end-to-end, when the built binary + data dir are available offline) a real
  SecondOrder row through the binary reports the delivered second-order kind.

Mutation-proven (cp backup → edit drive.py → red → restore → cmp) in the build
report; here the assertions are the green side.
"""

from __future__ import annotations

import os
import re
import subprocess
from pathlib import Path

import drive
import pytest

_REPO = Path(__file__).resolve().parents[3]
_SCHEMA_RS = _REPO / "src" / "schema.rs"
_DATA_DIR = Path("/Users/moeyensj/projects/empyrean/data")

# A real heliocentric ICRF/SSB Cartesian NEO state at epoch MJD 61200 TDB (the
# same state the python channel's test uses), so the end-to-end row exercises a
# genuine propagation.
_IC_POS = [-0.9259598226174744, 0.5586961894066496, 0.1841364244042053]
_IC_VEL = [-0.008132198510292572, -0.01147982080464957, -0.004470724867546879]
_EPOCH = 61200.0
_TARGET = 61565.0


# ── Schema-constant parser (pinned, not hand-copied) ─────────────────────────
#
# The schema crate (src/schema.rs) is the single source of truth for the OD
# method-axis note; drive.py mirrors it as a Python literal (it cannot import a
# Rust const). This parser reads the LIVE schema.rs and the pin below compares
# drive.py's mirror to THAT, so a drift in either the mirror or the schema turns
# the pin red — a second hand-copied spelling here could not.
#
# This is a small self-contained copy of
# ``runners/python/tests/test_run_products.py::_parse_schema_constants`` (note
# portion). It is copied rather than imported because importing that module
# pulls in ``import run`` (the python runner), which imports the engine wheel —
# the cli tests must stay offline/wheel-free — and the task may touch only
# ``runners/cli/**``, so a shared helper under ``runners/`` that both test dirs
# import is out of scope here. See that sibling parser for the source of the
# approach.
def _parse_schema_note(schema_path: Path) -> str:
    text = Path(schema_path).read_text()
    m = re.search(
        r'pub const OD_METHOD_AXIS_NOT_PRODUCED:\s*&str\s*=\s*"((?:[^"\\]|\\.)*)"\s*;',
        text,
        re.DOTALL,
    )
    assert m is not None, f"OD_METHOD_AXIS_NOT_PRODUCED not found in {schema_path}"
    return m.group(1)


def _prop_row(tag: str | None, *, leak: bool = False) -> dict:
    """One synthetic propagation plan row. ``leak=True`` seeds BOGUS inherited
    per-method fields (as if copied from a rust input row) so the reset can be
    observed."""
    row = {
        "object": "TestNEO",
        "population": "NEO",
        "epoch_mjd_tdb": _EPOCH,
        "t_mjd_tdb": _TARGET,
        "dt_days": 365.0,
        "force_model": "standard",
        "test_type": "propagation",
        "propagation_uncertainty": tag,
        "ic_pos_au": _IC_POS,
        "ic_vel_au_d": _IC_VEL,
        "notes": "",
    }
    if leak:
        row.update(
            {
                "cov_kind": 5,
                "cov_joint_width": 99,
                "cov_tri": [1.0, 2.0, 3.0],
                "resolved_method": "sigma_point_with_cov",
                "orbit_delivered": False,
                "orbit_status": "LEAK",
                "emp_pos_cov_au2": [[9.0, 9.0, 9.0], [9.0, 9.0, 9.0], [9.0, 9.0, 9.0]],
            }
        )
    return row


# Captured daemon `prop` responses (the binary's Products::render ordering;
# runners/cli/src/main.rs). 8 fixed fields then the 12 product tokens.
_SECOND_ORDER_LINE = (
    "ok -1.06e0 8.30e-3 -2.39e-2 1.65e-3 -1.42e-2 -5.25e-3 25.759042 "
    "resolved_method=second_order_with_cov cov_kind=1 cov_joint_width=6 "
    "orbit_delivered=1 orbit_status=delivered mix_n_components_total=na "
    "mix_weight_delivered=na mix_n_failed=na mix_n_unresolved=na "
    "mix_n_curvature_refused=na mix_n_sky_linearization_refused=na "
    "cov_tri=" + ",".join(["1.0e0"] * 21)
)
_MIXTURE_LINE = (
    "ok 1.0e0 2.0e0 3.0e0 4.0e0 5.0e0 6.0e0 9.9 "
    "resolved_method=gaussian_mixture_with_cov cov_kind=3 cov_joint_width=6 "
    "orbit_delivered=1 orbit_status=delivered mix_n_components_total=4 "
    "mix_weight_delivered=9.87e-1 mix_n_failed=0 mix_n_unresolved=1 "
    "mix_n_curvature_refused=2 mix_n_sky_linearization_refused=0 "
    "cov_tri=" + ",".join(["0.0e0"] * 21)
)
# A token-less (f64 / legacy) response: just the 8 fixed fields.
_LEGACY_LINE = "ok 1.0e0 2.0e0 3.0e0 4.0e0 5.0e0 6.0e0 7.5"


# ── Token parsing ────────────────────────────────────────────────────────────


def test_parse_products_second_order_line() -> None:
    """A captured SecondOrder response parses to the schema types. Mutation:
    drop a key from ``_PRODUCT_TOKEN_PARSERS`` (or default an absent field to 0
    instead of None) → the line still carries that token → ``_parse_products``
    raises on the unknown key, or the value assertion flips → red."""
    parts = _SECOND_ORDER_LINE.split()
    assert parts[0] == "ok" and len(parts) == 20
    prod = drive._parse_products(parts[8:])
    assert prod["resolved_method"] == "second_order_with_cov"
    assert prod["cov_kind"] == 1
    assert prod["cov_joint_width"] == 6
    assert len(prod["cov_tri"]) == 21
    assert prod["orbit_delivered"] is True
    assert prod["orbit_status"] == "delivered"
    # An unsplit (second-order) row carries no mixture tally: the six mix_* read
    # `na` → None, never a fabricated zero.
    assert prod["mix_n_components_total"] is None
    assert prod["mix_weight_delivered"] is None


def test_parse_products_mixture_line_tallies() -> None:
    """A mixture response parses its int / float tallies. Mutation: a parser
    that coerced `na`/ints wrongly → red."""
    prod = drive._parse_products(_MIXTURE_LINE.split()[8:])
    assert prod["cov_kind"] == 3
    assert prod["mix_n_components_total"] == 4
    assert prod["mix_weight_delivered"] == pytest.approx(0.987)
    assert prod["mix_n_unresolved"] == 1
    assert prod["mix_n_curvature_refused"] == 2
    assert prod["mix_n_failed"] == 0


def test_parse_products_refuses_unknown_and_missing() -> None:
    """No hidden fallback: an unknown token key and a missing field are both
    refused by name. Mutation: a parser that skipped unknown keys / tolerated a
    missing field → no raise → red here."""
    with pytest.raises(ValueError, match="unknown product token key"):
        drive._parse_products(["bogus_key=1"] + _SECOND_ORDER_LINE.split()[8:])
    with pytest.raises(ValueError, match="missing product tokens"):
        drive._parse_products(_SECOND_ORDER_LINE.split()[9:])  # drop one token


# ── Byte-identity + no-leak on token-less rows ───────────────────────────────


def test_legacy_prop_row_keeps_per_method_null_and_resets_leak() -> None:
    """An 18-field legacy line (no method token) yields the 8-field response;
    the cli row keeps all 12 per-method fields null and the 8 fixed fields
    exactly as before, and an inherited foreign product is cleared (no leak).
    Mutation: default a per-method field to 0 instead of null, or set-to-null
    (adding a key) instead of delete → red here or in the byte diff."""
    r = _prop_row("f64_no_cov", leak=True)
    assert drive._method_token(r) is None  # f64 sends no token
    parts = _LEGACY_LINE.split()
    assert len(parts) == 8
    row = drive._build_prop_row(r, parts, None, "TS", "VER")
    # All 12 per-method fields + the 2 moment views read null (absent → None).
    for f in drive._PER_METHOD_FIELDS:
        assert row.get(f) is None, f"{f} should be null on a token-less row"
    # ... and the deleted keys are truly absent (byte-identity: no `null`s added).
    for f in drive._PER_METHOD_FIELDS:
        assert f not in row, f"{f} must be absent, not an explicit null"
    # The 8 fixed fields exactly as the covariance-free path produced them.
    assert row["emp_pos_au"] == [1.0, 2.0, 3.0]
    assert row["emp_time_ms"] == 7.5
    assert row["channel"] == "cli"


def test_untagged_row_sends_no_token() -> None:
    """The untagged legacy row (propagation_uncertainty=None) sends no method
    token, exactly like f64."""
    assert drive._method_token(_prop_row(None)) is None


# ── Method populated on a method row ─────────────────────────────────────────


def test_second_order_prop_row_populates_products() -> None:
    """A SecondOrder row populates the 12 fields from the binary tokens; the
    collapsed moment view stays None (the cli binary emits no 6x6 — named gap).
    Mutation: ignore the parsed tokens → the fields stay null → red."""
    r = _prop_row("second_order_with_cov")
    assert drive._method_token(r) == "second_order_with_cov"
    parts = _SECOND_ORDER_LINE.split()
    row = drive._build_prop_row(r, parts, "second_order_with_cov", "TS", "VER")
    assert row["resolved_method"] == "second_order_with_cov"
    assert row["cov_kind"] == 1
    assert row["cov_joint_width"] == 6
    assert len(row["cov_tri"]) == 21
    assert row["orbit_delivered"] is True
    assert row["orbit_status"] == "delivered"
    # The cli channel emits no collapsed moment view.
    assert row.get("emp_pos_cov_au2") is None


# ── Line builder: method sent for non-f64, absent for f64 ────────────────────


def test_method_token_sent_for_non_f64_absent_for_f64() -> None:
    """The daemon line carries the 19th method token for every non-f64 method
    and omits it for f64 / the untagged row. Mutation: a ``_method_token`` that
    always returned None → the SecondOrder line loses its tag → red (and the
    SecondOrder binary response would report linear end-to-end)."""
    so_line = drive._prop_daemon_line(_prop_row("second_order_with_cov"))
    assert so_line.endswith(" second_order_with_cov")
    assert len(so_line.split()) == 20  # "prop" + 18 fields + method token

    for tag in (
        "first_order_with_cov",
        "auto",
        "sigma_point_with_cov",
        "monte_carlo_100_with_cov",
        "gaussian_mixture_with_cov",
    ):
        line = drive._prop_daemon_line(_prop_row(tag))
        assert line.endswith(f" {tag}"), tag
        assert len(line.split()) == 20, tag

    f64_line = drive._prop_daemon_line(_prop_row("f64_no_cov"))
    assert not f64_line.endswith("_with_cov")
    assert len(f64_line.split()) == 19  # "prop" + 18 fields, no method token


# ── OD method-axis note ──────────────────────────────────────────────────────


def test_od_note_matches_schema_literal() -> None:
    """drive.py's OD note equals OD_METHOD_AXIS_NOT_PRODUCED parsed out of
    src/schema.rs, byte for byte. Mutation: blank or alter the mirror → red."""
    assert drive._OD_METHOD_AXIS_NOT_PRODUCED == _parse_schema_note(_SCHEMA_RS)


def test_od_note_appended_to_base() -> None:
    """The note replaces an empty base and is appended (``base; note``) to a
    non-empty one, so the catalog note is preserved."""
    assert drive._with_od_method_note("") == drive._OD_METHOD_AXIS_NOT_PRODUCED
    assert drive._with_od_method_note("catalog note") == (
        f"catalog note; {drive._OD_METHOD_AXIS_NOT_PRODUCED}"
    )


def test_schema_pin_detects_drift(tmp_path) -> None:
    """Tripwire: the pin reads the live schema.rs, so a drift there turns it
    red. Parse a scratch copy whose note is mutated and confirm it no longer
    equals the drive.py mirror (i.e. the pin above would fail)."""
    scratch = tmp_path / "schema.rs"
    text = _SCHEMA_RS.read_text()
    mutated = text.replace("not on the wrapper", "now on the wrapper")
    assert mutated != text, "the scratch mutation did not apply"
    scratch.write_text(mutated)
    assert _parse_schema_note(scratch) != drive._OD_METHOD_AXIS_NOT_PRODUCED


# ── End-to-end through the built binary (offline; skipped if unavailable) ─────


def _binary() -> Path | None:
    p = Path(drive.__file__).resolve().parent / "target" / "release" / "empyrean-cli-runner"
    return p if p.exists() else None


@pytest.mark.skipif(
    _binary() is None or not _DATA_DIR.exists(),
    reason="cli binary or data dir unavailable (offline unit tests still run)",
)
def test_end_to_end_second_order_through_binary() -> None:
    """A real SecondOrder prop row through the built binary reports the
    delivered second-order kind (cov_kind == 1, cov_tri length 21). This is the
    end-to-end form of the method-token mutation: drop the token in
    ``_prop_daemon_line`` and the binary returns the 8-field f64 line, so the
    len==20 / resolved_method assertions go red."""
    binary = _binary()
    line = drive._prop_daemon_line(_prop_row("second_order_with_cov"))
    proc = subprocess.Popen(
        [str(binary), "--daemon", "--data-dir", str(_DATA_DIR)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        # Wait for the "ready" signal on stderr (context loaded).
        while True:
            sline = proc.stderr.readline()
            assert sline, "runner exited without ready signal"
            if sline.strip() == "ready":
                break
        proc.stdin.write(line + "\n")
        proc.stdin.flush()
        out_line = proc.stdout.readline().strip()
    finally:
        proc.stdin.close()
        proc.wait(timeout=120)
    parts = out_line.split()
    assert parts[0] == "ok", out_line
    assert len(parts) == 20, out_line
    prod = drive._parse_products(parts[8:])
    assert prod["resolved_method"] == "second_order_with_cov"
    assert prod["cov_kind"] == 1
    assert len(prod["cov_tri"]) == 21
    assert prod["orbit_delivered"] is True


if __name__ == "__main__":
    os.environ.setdefault("EMPYREAN_DATA_DIR", str(_DATA_DIR))
    raise SystemExit(pytest.main([__file__, "-v"]))
