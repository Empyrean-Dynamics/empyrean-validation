"""The c validation channel carries the C ABI's per-method products into the
JSON rows.

``drive.py`` sends each plan row's ``propagation_uncertainty`` method as the
optional 19th runner ``prop`` token (all tags but ``none`` / the untagged
legacy row) and as the ``od`` command's METHOD token, and parses the delivered
0.11 product tokens the binary (``runners/c/runner.c``) appends into the
schema's 12 per-method fields. These tests pin:

* the product tokens of a captured SecondOrder response parse to the schema
  types (``resolved_method``, ``cov_kind`` == 1, ``cov_tri`` of length 21,
  ``orbit_delivered`` is True) — the discriminator a dropped-token bug fails;
* a token-less (``none`` / legacy) response leaves all 12 per-method fields null
  and the 8 fixed fields exactly as before — byte-identical, and an inherited
  foreign product is reset (no leak);
* the method token is sent for every non-``none`` row and absent for ``none`` /
  the untagged row (the line builder);
* the driver's schema mirror (the method / arm vocabulary) and the runner's
  Monte-Carlo constants match ``src/schema.rs`` (a drift in either turns the pin
  red);
* (end-to-end, when the built binary + data dir are available offline) a real
  SecondOrder prop row through the binary reports the delivered second-order
  kind (``cov_kind`` == 1, 21-entry triangle, no mixture tally); a ``none`` row
  returns the bare 8-field line; a ``second_order`` OD fit is refused BY NAME
  (``orbit_delivered`` false); a ``first_order`` OD fit delivers with its
  products.

Mutation-proven (cp backup → edit drive.py / runner.c → red → restore → cmp) in
the build report; here the assertions are the green side.
"""

from __future__ import annotations

import json
import math
import os
import re
import subprocess
import sys
from pathlib import Path

import drive
import pytest

_REPO = Path(__file__).resolve().parents[3]
_SCHEMA_RS = _REPO / "src" / "schema.rs"
_RUNNER_C = Path(drive.__file__).resolve().parent / "runner.c"
_DATA_DIR = Path("/Users/moeyensj/projects/empyrean/data")
_PSV_DIR = _REPO / "fixtures" / "psv"

# A real heliocentric ICRF/SSB Cartesian NEO state at epoch MJD 61200 TDB (the
# same state the cli / python channels' tests use), so the end-to-end row
# exercises a genuine propagation.
_IC_POS = [-0.9259598226174744, 0.5586961894066496, 0.1841364244042053]
_IC_VEL = [-0.008132198510292572, -0.01147982080464957, -0.004470724867546879]
_EPOCH = 61200.0
_TARGET = 61565.0


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
                "resolved_method": "sigma_point",
                "orbit_delivered": False,
                "orbit_status": "LEAK",
                "emp_pos_cov_au2": [[9.0, 9.0, 9.0], [9.0, 9.0, 9.0], [9.0, 9.0, 9.0]],
            }
        )
    return row


def _eph_row(tag: str) -> dict:
    """One synthetic geocentric (``500``) ephemeris plan row carrying the given
    method tag, for the runner ``eph`` line."""
    return {
        "object": "TestNEO",
        "population": "NEO",
        "epoch_mjd_tdb": _EPOCH,
        "t_mjd_tdb": _TARGET,
        "dt_days": 365.0,
        "force_model": "standard",
        "test_type": "ephemeris",
        "propagation_uncertainty": tag,
        "ic_pos_au": _IC_POS,
        "ic_vel_au_d": _IC_VEL,
        "observer": "500",
        "notes": "",
    }


# Captured runner `prop` responses (the binary's render_products ordering;
# runners/c/runner.c). 8 fixed fields then the 12 product tokens.
_SECOND_ORDER_LINE = (
    "ok -1.06e0 8.30e-3 -2.39e-2 1.65e-3 -1.42e-2 -5.25e-3 25.759042 "
    "resolved_method=second_order cov_kind=1 cov_joint_width=6 "
    "orbit_delivered=1 orbit_status=delivered mix_n_components_total=na "
    "mix_weight_delivered=na mix_n_failed=na mix_n_unresolved=na "
    "mix_n_curvature_refused=na mix_n_sky_linearization_refused=na "
    "cov_tri=" + ",".join(["1.0e0"] * 21)
)
_MIXTURE_LINE = (
    "ok 1.0e0 2.0e0 3.0e0 4.0e0 5.0e0 6.0e0 9.9 "
    "resolved_method=gaussian_mixture cov_kind=3 cov_joint_width=6 "
    "orbit_delivered=1 orbit_status=delivered mix_n_components_total=4 "
    "mix_weight_delivered=9.87e-1 mix_n_failed=0 mix_n_unresolved=1 "
    "mix_n_curvature_refused=2 mix_n_sky_linearization_refused=na "
    "cov_tri=" + ",".join(["0.0e0"] * 21)
)
# A token-less (none / legacy) response: just the 8 fixed fields.
_LEGACY_LINE = "ok 1.0e0 2.0e0 3.0e0 4.0e0 5.0e0 6.0e0 7.5"


# ── Token parsing ────────────────────────────────────────────────────────────


def test_parse_products_second_order_line() -> None:
    """A captured SecondOrder response parses to the schema types. Mutation:
    drop ``cov_kind`` from ``_PRODUCT_TOKEN_PARSERS`` (the row mapping) → the
    line still carries that token → ``_parse_products`` raises on the unknown
    key, or the value assertion flips → red."""
    parts = _SECOND_ORDER_LINE.split()
    assert parts[0] == "ok" and len(parts) == 20
    prod = drive._parse_products(parts[8:])
    assert prod["resolved_method"] == "second_order"
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
    """A mixture response parses its int / float tallies; a per-field `-1 → na`
    tally (``mix_n_sky_linearization_refused`` on the propagation seam) reads
    None, never 0. Mutation: a parser that coerced `na`/ints wrongly → red."""
    prod = drive._parse_products(_MIXTURE_LINE.split()[8:])
    assert prod["cov_kind"] == 3
    assert prod["mix_n_components_total"] == 4
    assert prod["mix_weight_delivered"] == pytest.approx(0.987)
    assert prod["mix_n_unresolved"] == 1
    assert prod["mix_n_curvature_refused"] == 2
    assert prod["mix_n_failed"] == 0
    # The propagation seam does not carry the sky tally → the runner renders
    # `na` → None, never a fabricated 0.
    assert prod["mix_n_sky_linearization_refused"] is None


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
    the c row keeps all 12 per-method fields null and the 8 fixed fields exactly
    as before, and an inherited foreign product is cleared (no leak). Mutation:
    default a per-method field to 0 instead of null, or set-to-null (adding a
    key) instead of delete → red here or in the byte diff."""
    r = _prop_row(None, leak=True)
    assert drive._method_token(r) is None  # only the untagged legacy row sends no token
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
    assert row["channel"] == "c"


def test_untagged_row_sends_no_token() -> None:
    """The untagged legacy row (propagation_uncertainty=None) sends no method
    token, exactly like the none method."""
    assert drive._method_token(_prop_row(None)) is None


# ── Method populated on a method row ─────────────────────────────────────────


def test_second_order_prop_row_populates_products() -> None:
    """A SecondOrder row populates the 12 fields from the binary tokens; the
    collapsed moment view stays None (the c binary emits no 6×6 — named gap).
    Mutation: ignore the parsed tokens → the fields stay null → red."""
    r = _prop_row("second_order_detection_on")
    assert drive._method_token(r) == "second_order_detection_on"
    parts = _SECOND_ORDER_LINE.split()
    row = drive._build_prop_row(r, parts, "second_order_detection_on", "TS", "VER")
    # The binary's bare `second_order` is composed with the row's detection arm.
    assert row["resolved_method"] == "second_order_detection_on"
    assert row["cov_kind"] == 1
    assert row["cov_joint_width"] == 6
    assert len(row["cov_tri"]) == 21
    assert row["orbit_delivered"] is True
    assert row["orbit_status"] == "delivered"
    # The c channel emits no collapsed moment view.
    assert row.get("emp_pos_cov_au2") is None


# ── Line builder: method sent for non-none, absent for none / untagged ───────


def test_method_token_sent_for_non_none_absent_for_none() -> None:
    """The daemon line carries the trailing method token (after EXCL) for every covariance-bearing
    method and omits it for `none` / the untagged row. Mutation: a
    ``_method_token`` that always returned None → the SecondOrder line loses its
    tag → red (and the SecondOrder binary response would report linear
    end-to-end)."""
    so_line = drive._prop_daemon_line(_prop_row("second_order_detection_on"))
    assert so_line.endswith(" second_order_detection_on")
    assert len(so_line.split()) == 21  # "prop" + 18 fields + EXCL + method token

    for tag in (
        "first_order_detection_on",
        "auto_detection_on",
        "sigma_point_detection_on",
        "monte_carlo_detection_on",
        "gaussian_mixture_detection_on",
    ):
        line = drive._prop_daemon_line(_prop_row(tag))
        assert line.endswith(f" {tag}"), tag
        assert len(line.split()) == 21, tag

    none_line = drive._prop_daemon_line(_prop_row("none_detection_on"))
    # The none tag is sent too, so the binary reports the row's per-orbit
    # outcome (the rust reference's rule); only the untagged row sends none.
    assert none_line.endswith(" none_detection_on")
    assert len(none_line.split()) == 21  # "prop" + 18 fields + EXCL + method token
    untagged = drive._prop_daemon_line(_prop_row(None))
    assert len(untagged.split()) == 20  # "prop" + 18 fields + EXCL, no method token


# ── Schema pins: the mirror and the runner constants track src/schema.rs ─────


def test_method_vocabulary_mirrors_the_schema() -> None:
    """The driver's ``_UNCERTAINTY_METHODS`` / ``_UNCERTAINTY_ARMS`` mirror the
    ``uncertainty_modes`` string consts in ``src/schema.rs``, parsed live, so a
    drift in either the mirror OR the schema turns the pin red."""
    text = _SCHEMA_RS.read_text()

    def _const(name: str) -> str:
        m = re.search(rf'pub const {name}:\s*&str\s*=\s*"([^"]+)"\s*;', text)
        assert m is not None, f"{name} not found in {_SCHEMA_RS}"
        return m.group(1)

    expected_methods = tuple(
        _const(n)
        for n in (
            "NONE",
            "FIRST_ORDER",
            "SECOND_ORDER",
            "AUTO",
            "SIGMA_POINT",
            "MONTE_CARLO",
            "GAUSSIAN_MIXTURE",
        )
    )
    expected_arms = tuple(
        _const(n)
        for n in ("DETECTION_ON", "DETECTION_OFF", "DETECTION_OFF_ASSIST_DEFAULT_LIKE")
    )
    assert drive._UNCERTAINTY_METHODS == expected_methods
    assert drive._UNCERTAINTY_ARMS == expected_arms


def test_runner_monte_carlo_constants_match_the_schema() -> None:
    """runner.c's ``EMPYREAN_VALIDATION_MC_SAMPLES`` / ``_MC_SEED`` mirror the
    schema's ``MONTE_CARLO_SAMPLE_COUNT`` / ``MONTE_CARLO_SEED`` (both parsed
    live), so a seeded Monte-Carlo row stays a cross-channel bit check."""
    schema = _SCHEMA_RS.read_text()
    runner = _RUNNER_C.read_text()

    s_count = int(
        re.search(r"pub const MONTE_CARLO_SAMPLE_COUNT:\s*u32\s*=\s*(\d+)\s*;", schema)
        .group(1)
    )
    s_seed = int(
        re.search(
            r"pub const MONTE_CARLO_SEED:\s*u64\s*=\s*(0x[0-9A-Fa-f_]+)\s*;", schema
        )
        .group(1)
        .replace("_", ""),
        16,
    )
    c_count = int(
        re.search(r"#define EMPYREAN_VALIDATION_MC_SAMPLES\s+(\d+)u", runner).group(1)
    )
    c_seed = int(
        re.search(
            r"#define EMPYREAN_VALIDATION_MC_SEED\s+(0x[0-9A-Fa-f]+)ULL", runner
        ).group(1),
        16,
    )
    assert c_count == s_count
    assert c_seed == s_seed


# ── End-to-end through the built binary (offline; skipped if unavailable) ─────


def _binary() -> Path | None:
    p = Path(drive.__file__).resolve().parent / "runner"
    return p if p.exists() else None


def _run_once(binary: Path, line: str) -> str:
    """Start the runner, wait for its stderr ``ready`` signal, send one protocol
    line, and return the single response line. The C runner is always in the
    one-row-per-stdin-line mode (no flag), driven exactly as ``drive.py`` drives
    it."""
    proc = subprocess.Popen(
        [str(binary), str(_DATA_DIR)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        while True:
            sline = proc.stderr.readline()
            assert sline, "runner exited without ready signal"
            if sline.strip() == "ready":
                break
        proc.stdin.write(line + "\n")
        proc.stdin.flush()
        return proc.stdout.readline().strip()
    finally:
        proc.stdin.close()
        proc.wait(timeout=120)


@pytest.mark.skipif(
    _binary() is None or not _DATA_DIR.exists(),
    reason="c runner or data dir unavailable (offline unit tests still run)",
)
def test_end_to_end_second_order_through_binary() -> None:
    """A real SecondOrder prop row through the built binary reports the
    delivered second-order kind (cov_kind == 1, cov_joint_width == 6, cov_tri
    length 21, orbit_delivered). This is the end-to-end form of the
    method-token mutation: drop the token in ``_prop_daemon_line`` and the
    binary returns the 8-field line, so the len==20 / resolved_method
    assertions go red."""
    line = drive._prop_daemon_line(_prop_row("second_order_detection_on"))
    out_line = _run_once(_binary(), line)
    parts = out_line.split()
    assert parts[0] == "ok", out_line
    assert len(parts) == 20, out_line
    prod = drive._parse_products(parts[8:])
    assert prod["resolved_method"] == "second_order"
    assert prod["cov_kind"] == 1
    assert prod["cov_joint_width"] == 6
    assert len(prod["cov_tri"]) == 21
    assert prod["orbit_delivered"] is True


@pytest.mark.skipif(
    _binary() is None or not _DATA_DIR.exists(),
    reason="c runner or data dir unavailable",
)
def test_end_to_end_none_row_reports_its_outcome_without_a_covariance() -> None:
    """A `none` prop row through the binary reports its per-orbit outcome —
    ``orbit_delivered`` true, ``orbit_status`` ``delivered``, read off the
    engine like every other method (the rust reference's rule) — and no
    covariance: no kind, width, triangle or resolved method. Mutation: stop
    sending the none tag → the bare 8-field line → no outcome → red."""
    r = _prop_row("none_detection_on")
    out_line = _run_once(_binary(), drive._prop_daemon_line(r))
    parts = out_line.split()
    assert parts[0] == "ok" and len(parts) == 20, out_line
    row = drive._build_prop_row(r, parts, drive._method_token(r), "TS", "VER")
    assert row["orbit_delivered"] is True
    assert row["orbit_status"] == "delivered"
    for f in ("resolved_method", "cov_kind", "cov_joint_width", "cov_tri"):
        assert row[f] is None, f


@pytest.mark.skipif(
    _binary() is None or not _DATA_DIR.exists(),
    reason="c runner or data dir unavailable",
)
def test_end_to_end_absent_tally_reads_none() -> None:
    """From-engine: the SecondOrder prop row carries no mixture tally, so the six
    ``mix_*`` fields decode to None (the runner emitted `na`), never a
    fabricated 0."""
    line = drive._prop_daemon_line(_prop_row("second_order_detection_on"))
    prod = drive._parse_products(_run_once(_binary(), line).split()[8:])
    for f in (
        "mix_n_components_total",
        "mix_weight_delivered",
        "mix_n_failed",
        "mix_n_unresolved",
        "mix_n_curvature_refused",
        "mix_n_sky_linearization_refused",
    ):
        assert prod[f] is None, f


@pytest.mark.skipif(
    _binary() is None
    or not _DATA_DIR.exists()
    or not (_PSV_DIR / "2018 LA.psv").exists(),
    reason="c runner / data dir / 2018 LA fixture unavailable",
)
def test_end_to_end_od_second_order_refused_by_name() -> None:
    """From-engine: a ``second_order`` OD fit through the binary is refused BY
    NAME — the runner returns a ``refused <text>`` line whose text names
    ``SecondOrder`` — rather than delivering a first-order posterior under the
    method's name. (Mutation: swallow the refusal into an ``ok`` line and the
    ``startswith("refused")`` assertion goes red.)"""
    psv = _PSV_DIR / "2018 LA.psv"
    out_line = _run_once(_binary(), f"od 2 0 second_order_detection_on {psv}")
    assert out_line.startswith("refused "), f"expected a refusal line, got {out_line!r}"
    assert "SecondOrder" in out_line, f"refusal must name the method: {out_line!r}"


@pytest.mark.skipif(
    _binary() is None
    or not _DATA_DIR.exists()
    or not (_PSV_DIR / "2018 LA.psv").exists(),
    reason="c runner / data dir / 2018 LA fixture unavailable",
)
def test_end_to_end_od_first_order_delivers_with_products() -> None:
    """From-engine: a ``first_order`` OD fit through the binary delivers the fit
    with its first-order products (``cov_kind`` == 0, a 21-entry packed
    triangle, ``orbit_delivered`` true). The 21-field line is `ok` + state[6] +
    iters + ms + the 12 product tokens."""
    psv = _PSV_DIR / "2018 LA.psv"
    out_line = _run_once(_binary(), f"od 2 0 first_order_detection_on {psv}")
    parts = out_line.split()
    assert parts[0] == "ok", out_line
    prod = drive._parse_products(parts[9:])
    assert prod["resolved_method"] == "first_order"
    assert prod["cov_kind"] == 0
    assert prod["cov_joint_width"] == 6
    assert len(prod["cov_tri"]) == 21
    assert prod["orbit_delivered"] is True


@pytest.mark.skipif(
    _binary() is None or not _DATA_DIR.exists(),
    reason="c runner or data dir unavailable",
)
def test_end_to_end_ephemeris_line_second_order_products() -> None:
    """From-engine: a second_order ephemeris LINE through the built binary
    carries the delivered sky joint read off the entry's own ``joint`` —
    ``cov_kind`` == 1, ``cov_joint_width`` == 6, a 21-cell ``cov_tri``,
    ``resolved_method`` ``second_order`` (the bare kind the driver composes with
    the arm), ``orbit_delivered`` true — and, being a non-mixture row, every
    ``mix_*`` tally ABSENT (``na`` → None, never a fabricated 0). This is the
    re-bind of the C ephemeris line; before it the line was covariance-free first
    order (5 fields only). Mutation: drop the method token on the eph line → the
    binary returns the 6-field covariance-free line → the len==18 / cov_kind
    assertions go red."""
    eph_row = _eph_row("second_order_detection_on")
    line = drive._eph_daemon_line(eph_row)
    out_line = _run_once(_binary(), line)
    parts = out_line.split()
    assert parts[0] == "ok", out_line
    assert len(parts) == 18, out_line  # ok + 5 fixed eph fields + 12 product tokens
    prod = drive._parse_products(parts[6:])
    assert prod["resolved_method"] == "second_order"
    assert prod["cov_kind"] == 1
    assert prod["cov_joint_width"] == 6
    assert len(prod["cov_tri"]) == 21
    assert prod["orbit_delivered"] is True
    for f in (
        "mix_n_components_total",
        "mix_weight_delivered",
        "mix_n_failed",
        "mix_n_sky_linearization_refused",
    ):
        assert prod[f] is None, f


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


def test_excl_token_rides_every_prop_and_eph_line() -> None:
    """The row's ``excluded_perturbers_naif`` is the EXCL token on both lines,
    ahead of the method token (``-`` for none, else the NAIF ids
    comma-separated). Dropping it is what sent the self-perturber rows a
    different request than the reference's."""
    prop = _prop_row("second_order_detection_on")
    prop["excluded_perturbers_naif"] = [2000004]
    toks = drive._prop_daemon_line(prop).split()
    assert len(toks) == 21 and toks[19] == "2000004", toks
    assert toks[20] == "second_order_detection_on"
    assert drive._prop_daemon_line(_prop_row(None)).split()[19:] == ["-"]
    eph = _eph_row("sigma_point_detection_on")
    eph["excluded_perturbers_naif"] = [2000004, 2000002]
    toks = drive._eph_daemon_line(eph).split()
    assert toks[19:] == ["500", "2000004,2000002", "sigma_point_detection_on"], toks
    assert drive._eph_daemon_line(_eph_row("none_detection_on")).split()[19:] == [
        "500",
        "-",
        "none_detection_on",
    ]


def test_refused_line_becomes_a_declined_row_never_a_skip() -> None:
    """A ``refused failed:<variant> <engine text>`` line becomes one row:
    ``orbit_delivered`` false, the ``failed:<variant>`` status, the engine's
    text in ``notes``, and no measured field (no residual from a position that
    was never delivered)."""
    r = _eph_row("sigma_point_detection_on")
    r["d_ra_arcsec"] = 9.9  # an inherited value the refused row must not keep
    line = (
        "refused failed:other orbit orbit_0: sampled sky delivery refused — the "
        "state coincides with Asteroid 4 (Vesta)'s own ephemeris"
    )
    row = drive._refused_row(r, line, "TS", "VER")
    assert row["channel"] == "c"
    assert row["orbit_delivered"] is False
    assert row["orbit_status"] == "failed:other"
    assert "sampled sky delivery refused" in row["notes"]
    for f in drive._MEASURED_FIELDS:
        assert row[f] is None, f
    with pytest.raises(ValueError):
        drive._refused_row(r, "refused something else", "TS", "VER")


# A stand-in runner for the driver's own handling, offline: it signals `ready`,
# then answers each line by its observer code — `NAN` gets an `ok` line of NaN
# (what the runner printed for a zero-entry ephemeris before it read outcomes),
# `REF` gets a `refused failed:other …` line.
_STUB_RUNNER = """
import sys
sys.stderr.write("ready\\n")
sys.stderr.flush()
for line in sys.stdin:
    toks = line.split()
    if "NAN" in toks:
        print("ok nan nan nan nan 1.0", flush=True)
    elif "REF" in toks:
        print("refused failed:other stub engine text for a declined orbit", flush=True)
    else:
        print("fail stub: unexpected line", flush=True)
"""


def test_nan_ok_line_never_reaches_residuals_and_a_refusal_is_a_row(tmp_path, capsys) -> None:
    """Through the driver's ``main`` with a stand-in runner: an ``ok`` line of
    NaN never becomes residuals (the row is left out, loudly, and no NaN reaches
    the output), and a ``refused`` line becomes one declined row."""
    stub = tmp_path / "stub_runner"
    stub.write_text(f"#!{sys.executable}\n{_STUB_RUNNER}")
    stub.chmod(0o755)
    nan_row = dict(
        _eph_row("none_detection_on"),
        observer="NAN",
        ref_ra_rad=1.0,
        ref_dec_rad=0.3,
        ref_rho_au=2.0,
    )
    ref_row = dict(_eph_row("none_detection_on"), observer="REF")
    inp = tmp_path / "plan.json"
    out = tmp_path / "out.json"
    inp.write_text(json.dumps([nan_row, ref_row]))
    old = sys.argv
    sys.argv = [
        "drive.py",
        "--input",
        str(inp),
        "--output",
        str(out),
        "--runner",
        str(stub),
        "--fixtures-dir",
        str(tmp_path),
    ]
    try:
        assert drive.main() == 0
    finally:
        sys.argv = old
    raw = out.read_text()
    assert "NaN" not in raw
    rows = json.loads(raw)
    assert [r["observer"] for r in rows] == ["REF"], rows
    assert rows[0]["orbit_delivered"] is False
    assert rows[0]["orbit_status"] == "failed:other"
    assert "stub engine text" in rows[0]["notes"]
    err = capsys.readouterr().err
    assert "Loaded 2 rows; wrote 0, declined 1, skipped 1 (0 + 1 + 1 = 2)" in err, err


@pytest.mark.skipif(
    _binary() is None or not _DATA_DIR.exists() or _vesta_eph_row("sigma_point_detection_on") is None,
    reason="c runner, data dir or staged plan unavailable",
)
def test_end_to_end_vesta_exclusion_delivers_and_its_absence_is_refused() -> None:
    """From-engine, on Vesta's own self-perturber row: with the plan's exclusion
    the sigma-point ephemeris DELIVERS its sampled sky covariance (the core
    channel's request); with the exclusion forced off, the engine declines the
    orbit and the binary writes a ``refused failed:other …`` line naming why,
    which becomes a declined row (never an ``ok`` line of NaN)."""
    r = _vesta_eph_row("sigma_point_detection_on")
    out_line = _run_once(_binary(), drive._eph_daemon_line(r))
    parts = out_line.split()
    assert parts[0] == "ok" and len(parts) == 18, out_line
    assert all(math.isfinite(float(v)) for v in parts[1:4]), out_line
    prod = drive._parse_products(parts[6:])
    assert prod["orbit_delivered"] is True
    assert prod["orbit_status"] == "delivered"
    assert prod["resolved_method"] == "sigma_point"

    forced = dict(r, excluded_perturbers_naif=[])
    out_line = _run_once(_binary(), drive._eph_daemon_line(forced))
    assert out_line.startswith("refused failed:other "), out_line
    assert "nan" not in out_line.lower().split()
    row = drive._refused_row(forced, out_line, "TS", "VER")
    assert row["orbit_delivered"] is False
    assert "sampled sky delivery refused" in row["notes"], row["notes"]


def test_row_account_line_is_honest() -> None:
    """Every plan row is counted once — written, declined or skipped — so
    W + D + S = N; a declined row is not also counted as skipped, and the
    non-grav row the legacy fit's second pass generates stands for the plan's
    non_grav_recovery row. A surplus row fails the account."""
    acct = drive._RowAccount(5)
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
    acct = drive._RowAccount(2)
    out = []
    acct.begin({"test_type": "non_grav_recovery"}, out)
    acct.begin({"test_type": "propagation"}, out)
    out.extend([{"test_type": "propagation"}, {"test_type": "propagation"}])
    w, d, s, ok = acct.finish(out)
    assert (w, d, s, ok) == (1, 0, 1, False)


if __name__ == "__main__":
    os.environ.setdefault("EMPYREAN_DATA_DIR", str(_DATA_DIR))
    raise SystemExit(pytest.main([__file__, "-v"]))
