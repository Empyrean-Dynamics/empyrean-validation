"""The cli validation channel carries the binary's per-method products into the
JSON rows.

``drive.py`` sends each plan row's ``propagation_uncertainty`` method as the
optional 19th daemon ``prop`` token (all tags but ``none`` / the untagged
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
* (end-to-end, when the built binary + data dir are available offline) a real
  SecondOrder prop row through the binary reports the delivered second-order
  kind, a ``second_order`` OD fit is refused BY NAME (``orbit_delivered``
  false), and a ``first_order`` OD fit delivers with its products;
* the 28-column ephemeris file the binary writes round-trips through the
  driver's ``_read_ephemeris_csv``: ``cov_kind`` and the six ``mix_*`` columns
  survive write→read (a real-mixture row round-trips its tallies; an unsplit
  row's absent sentinels decode to None).

Mutation-proven (cp backup → edit drive.py → red → restore → cmp) in the build
report; here the assertions are the green side.
"""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

import drive
import pytest

_REPO = Path(__file__).resolve().parents[3]
_DATA_DIR = Path("/Users/moeyensj/projects/empyrean/data")
_PSV_DIR = _REPO / "fixtures" / "psv"

# A real heliocentric ICRF/SSB Cartesian NEO state at epoch MJD 61200 TDB (the
# same state the python channel's test uses), so the end-to-end row exercises a
# genuine propagation.
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
    method tag, for the daemon ``eph`` line."""
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


# Captured daemon `prop` responses (the binary's Products::render ordering;
# runners/cli/src/main.rs). 8 fixed fields then the 12 product tokens.
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
    r = _prop_row("none_detection_on", leak=True)
    assert drive._method_token(r) is None  # the none method sends no token
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
    # The cli channel emits no collapsed moment view.
    assert row.get("emp_pos_cov_au2") is None


# ── Line builder: method sent for non-f64, absent for f64 ────────────────────


def test_method_token_sent_for_non_f64_absent_for_f64() -> None:
    """The daemon line carries the 19th method token for every non-f64 method
    and omits it for f64 / the untagged row. Mutation: a ``_method_token`` that
    always returned None → the SecondOrder line loses its tag → red (and the
    SecondOrder binary response would report linear end-to-end)."""
    so_line = drive._prop_daemon_line(_prop_row("second_order_detection_on"))
    assert so_line.endswith(" second_order_detection_on")
    assert len(so_line.split()) == 20  # "prop" + 18 fields + method token

    for tag in (
        "first_order_detection_on",
        "auto_detection_on",
        "sigma_point_detection_on",
        "monte_carlo_detection_on",
        "gaussian_mixture_detection_on",
    ):
        line = drive._prop_daemon_line(_prop_row(tag))
        assert line.endswith(f" {tag}"), tag
        assert len(line.split()) == 20, tag

    f64_line = drive._prop_daemon_line(_prop_row("none_detection_on"))
    # The covariance-free none tag is never appended as a token.
    assert not f64_line.endswith(" none_detection_on")
    assert len(f64_line.split()) == 19  # "prop" + 18 fields, no method token


# ── End-to-end through the built binary (offline; skipped if unavailable) ─────


def _binary() -> Path | None:
    p = Path(drive.__file__).resolve().parent / "target" / "release" / "empyrean-cli-runner"
    return p if p.exists() else None


def _daemon_once(binary: Path, line: str) -> str:
    """Start the runner daemon, wait for its stderr ``ready`` signal, send one
    protocol line, and return the single response line."""
    proc = subprocess.Popen(
        [str(binary), "--daemon", "--data-dir", str(_DATA_DIR)],
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
    reason="cli binary or data dir unavailable (offline unit tests still run)",
)
def test_end_to_end_second_order_through_binary() -> None:
    """A real SecondOrder prop row through the built binary reports the
    delivered second-order kind (cov_kind == 1, cov_tri length 21). This is the
    end-to-end form of the method-token mutation: drop the token in
    ``_prop_daemon_line`` and the binary returns the 8-field f64 line, so the
    len==20 / resolved_method assertions go red."""
    line = drive._prop_daemon_line(_prop_row("second_order_detection_on"))
    out_line = _daemon_once(_binary(), line)
    parts = out_line.split()
    assert parts[0] == "ok", out_line
    assert len(parts) == 20, out_line
    prod = drive._parse_products(parts[8:])
    assert prod["resolved_method"] == "second_order"
    assert prod["cov_kind"] == 1
    assert len(prod["cov_tri"]) == 21
    assert prod["orbit_delivered"] is True


@pytest.mark.skipif(
    _binary() is None
    or not _DATA_DIR.exists()
    or not (_PSV_DIR / "2018 LA.psv").exists(),
    reason="cli binary / data dir / 2018 LA fixture unavailable",
)
def test_end_to_end_od_second_order_refused_by_name() -> None:
    """From-engine: a ``second_order`` OD fit through the binary is refused BY
    NAME — the daemon returns a ``refused <text>`` line whose text names
    ``SecondOrder`` — rather than delivering a first-order posterior under the
    method's name. (Mutation: swallow the refusal into an ``ok`` line and the
    ``startswith("refused")`` assertion goes red.)"""
    psv = _PSV_DIR / "2018 LA.psv"
    out_line = _daemon_once(_binary(), f"od 2 0 second_order_detection_on {psv}")
    assert out_line.startswith("refused "), f"expected a refusal line, got {out_line!r}"
    assert "SecondOrder" in out_line, f"refusal must name the method: {out_line!r}"


@pytest.mark.skipif(
    _binary() is None
    or not _DATA_DIR.exists()
    or not (_PSV_DIR / "2018 LA.psv").exists(),
    reason="cli binary / data dir / 2018 LA fixture unavailable",
)
def test_end_to_end_od_first_order_delivers_with_products() -> None:
    """From-engine: a ``first_order`` OD fit through the binary delivers the fit
    with its first-order products (``cov_kind == 0``, a 21-entry packed
    triangle, ``orbit_delivered`` true). The driver composes the bare
    ``first_order`` the binary emits with the row's arm."""
    psv = _PSV_DIR / "2018 LA.psv"
    out_line = _daemon_once(_binary(), f"od 2 0 first_order_detection_on {psv}")
    parts = out_line.split()
    assert parts[0] == "ok", out_line
    prod = drive._parse_products(parts[9:])
    assert prod["resolved_method"] == "first_order"
    assert prod["cov_kind"] == 0
    assert prod["cov_joint_width"] == 6
    assert len(prod["cov_tri"]) == 21
    assert prod["orbit_delivered"] is True


# ── 28-column ephemeris file reader ──────────────────────────────────────────


# The 28-column header the engine's `write_ephemeris_csv` emits (the binary's
# `--mode eph --out`): 21 ephemeris columns, then `cov_kind` and the six
# `mix_*` tallies.
_EPH_HEADER = (
    "orbit_id,obs_code,epoch_mjd_tdb,ra_deg,dec_deg,rho_au,vrho_au_day,"
    "vra_deg_day,vdec_deg_day,light_time_days,phase_angle_deg,elongation_deg,"
    "heliocentric_distance_au,mag,mag_sigma,zenith_angle_deg,azimuth_deg,"
    "hour_angle_deg,lunar_elongation_deg,position_angle_deg,sky_rate_deg_day,"
    "cov_kind,mix_n_components_total,mix_weight_delivered,mix_n_failed,"
    "mix_n_unresolved,mix_n_curvature_refused,mix_n_sky_linearization_refused"
)


def test_read_ephemeris_csv_mixture_row_round_trips(tmp_path) -> None:
    """A mixture row's ``cov_kind`` and its six ``mix_*`` tallies round-trip
    through ``_read_ephemeris_csv`` (write real values, read the same back); an
    unsplit row's absent sentinels (-1 / NaN) decode to None, never 0.
    (Mutation: drop a ``mix_*`` column from ``_EPH_MIX_COLUMNS`` → that tally is
    no longer read → the equality assertion goes red.)"""
    assert len(_EPH_HEADER.split(",")) == 28, "the ephemeris CSV is 28 columns"
    lead = "orbit_0,500,61565.0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18"
    mixture = f"{lead},3,4,9.87e-1,0,1,2,0"
    unsplit = f"{lead},1,-1,NaN,-1,-1,-1,-1"
    csv_path = tmp_path / "eph.csv"
    csv_path.write_text(f"{_EPH_HEADER}\n{mixture}\n{unsplit}\n")

    rows = drive._read_ephemeris_csv(csv_path)
    assert len(rows) == 2
    mix = rows[0]
    assert mix["cov_kind"] == 3  # mixture wire discriminant
    assert mix["mix_n_components_total"] == 4
    assert mix["mix_weight_delivered"] == pytest.approx(0.987)
    assert mix["mix_n_failed"] == 0
    assert mix["mix_n_unresolved"] == 1
    assert mix["mix_n_curvature_refused"] == 2
    assert mix["mix_n_sky_linearization_refused"] == 0
    # Unsplit row: cov_kind survives; the absent sentinels decode to None.
    un = rows[1]
    assert un["cov_kind"] == 1
    assert un["mix_n_components_total"] is None
    assert un["mix_weight_delivered"] is None
    assert un["mix_n_failed"] is None


@pytest.mark.skipif(
    _binary() is None or not _DATA_DIR.exists(),
    reason="cli binary or data dir unavailable",
)
def test_end_to_end_ephemeris_file_written_and_read(tmp_path) -> None:
    """From-engine: the binary writes a 28-column ephemeris CSV under a
    covariance-bearing method (``second_order``), and the driver reads
    ``cov_kind`` + the six ``mix_*`` columns back. Proves the cli file reader
    accepts the 28-column format the 0.11 engine emits (``cov_kind`` carries the
    delivered second-order kind; the unsplit row's mix_* are the absent
    sentinel → None)."""
    binary = _binary()
    out = tmp_path / "eph.csv"
    proc = subprocess.run(
        [
            str(binary), "--mode", "eph",
            "--epoch", str(_EPOCH),
            # Attached `=` form: the negative components would otherwise be read
            # as flags by the arg parser.
            f"--pos={','.join(str(v) for v in _IC_POS)}",
            f"--vel={','.join(str(v) for v in _IC_VEL)}",
            "--target", str(_TARGET),
            "--observer", "500",
            "--uncertainty-method", "second_order_detection_on",
            "--out", str(out),
            "--data-dir", str(_DATA_DIR),
        ],
        capture_output=True,
        text=True,
        timeout=120,
        check=False,
    )
    assert proc.returncode == 0, proc.stderr
    assert out.exists(), "the binary did not write the ephemeris CSV"
    # The header is exactly the 28-column layout the reader expects.
    assert out.read_text().splitlines()[0] == _EPH_HEADER
    rows = drive._read_ephemeris_csv(out)
    assert rows, "the ephemeris CSV had no data rows"
    assert rows[0]["cov_kind"] == 1  # second-order sky covariance delivered
    # The six mix_* columns are present and read (absent → None on this
    # non-mixture row, never a fabricated 0).
    for col in drive._EPH_MIX_COLUMNS:
        assert col in rows[0]


# ── End-to-end ephemeris LINE products through the binary ────────────────────


@pytest.mark.skipif(
    _binary() is None or not _DATA_DIR.exists(),
    reason="cli binary or data dir unavailable",
)
def test_end_to_end_ephemeris_line_second_order_products() -> None:
    """From-engine: a second_order ephemeris LINE through the built binary
    carries the delivered sky joint read off the entry's own ``joint`` —
    ``cov_kind == 1``, a 21-cell ``cov_tri``, ``resolved_method`` ``second_order``
    (the bare kind the driver composes with the arm), ``orbit_delivered`` true —
    and, being a non-mixture row, every ``mix_*`` tally ABSENT (``na`` → None,
    never a fabricated 0). This is the re-bind of the cli ephemeris line; before
    it the line was covariance-free first order (5 fields only). Mutation: drop
    the method token on the eph line → the binary returns the 6-field
    covariance-free line → the len==18 / cov_kind assertions go red."""
    eph_row = _eph_row("second_order_detection_on")
    ic = drive._ic_line(eph_row)
    line = f"eph {ic} {eph_row['observer']} second_order_detection_on"
    out_line = _daemon_once(_binary(), line)
    parts = out_line.split()
    assert parts[0] == "ok", out_line
    assert len(parts) == 18, out_line  # ok + 5 fixed eph fields + 12 product tokens
    prod = drive._parse_products(parts[6:])
    assert prod["resolved_method"] == "second_order"
    assert prod["cov_kind"] == 1
    assert prod["cov_joint_width"] == 6
    assert len(prod["cov_tri"]) == 21
    assert prod["orbit_delivered"] is True
    # Non-mixture row: every mix_* absent (na → None), never a fabricated 0.
    assert prod["mix_n_components_total"] is None
    assert prod["mix_weight_delivered"] is None
    assert prod["mix_n_failed"] is None
    assert prod["mix_n_sky_linearization_refused"] is None


if __name__ == "__main__":
    os.environ.setdefault("EMPYREAN_DATA_DIR", str(_DATA_DIR))
    raise SystemExit(pytest.main([__file__, "-v"]))
