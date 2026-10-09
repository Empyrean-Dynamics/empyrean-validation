"""empyrean validation runner — CLI channel driver.

Reads a rust-channel ValidationResult JSON, replays each row through
the empyrean-cli-runner binary in daemon mode (loaded once, one row
per stdin line), and emits a cli-channel JSON in the same schema.

The runner protocol is one row per stdin line, prefixed with mode:
- "prop EPOCH IC[6] A1-3 G[5] FORCE TARGET [METHOD]"
- "eph  EPOCH IC[6] A1-3 G[5] FORCE TARGET OBS_CODE"
- "od   FORCE ADES_PATH"

This mirrors the C runner's protocol (validation/runners/c/drive.py),
so both channels use the same fork-once-stream-many model.

Per-method uncertainty axis
---------------------------
Each plan row carries a ``propagation_uncertainty`` method tag. For every
tag but ``none`` (and the untagged legacy row) the driver appends the
tag as the optional 19th ``prop`` token, so the binary attaches the synthetic
covariance, requests that rung, and appends its delivered 0.11 products after
the 8 fixed fields as whitespace-free ``key=value`` tokens (the binary's
``Products::render``; see ``runners/cli/src/main.rs``). The driver parses those
tokens into the schema's 12 per-method fields (``resolved_method``,
``cov_kind``, ``cov_joint_width``, ``cov_tri``, ``orbit_delivered``,
``orbit_status`` and the six ``mix_*``), reading what the engine *delivered*,
never the request. A token-less response (the ``none`` / legacy line, the
ephemeris line) leaves all 12 fields null, byte-identical to the pre-widening
row. The cli binary emits no collapsed moment view and ``ODConfig`` carries no
``uncertainty_method`` at this distribution revision, so ephemeris rows keep
``resolved_method`` / ``cov_kind`` null (named gap) and OD fit rows record the
shared [`OD_METHOD_AXIS_NOT_PRODUCED`] note by name rather than a blank.
"""

from __future__ import annotations

import argparse
import json
import math
import subprocess
import sys
from datetime import datetime, timezone
from importlib.metadata import PackageNotFoundError, version
from pathlib import Path

_TIER_TO_INT = {"approximate": 0, "basic": 1, "standard": 2}
_AU_KM = 149_597_870.700


# The composite uncertainty-tag vocabulary, a LITERAL mirror of
# empyrean_validation::schema::uncertainty_modes (src/schema.rs): seven method
# prefixes × three detection/timing arms, spelled `<method>_<arm>`. Pinned
# against the schema by the driver's tests (the regex-on-schema.rs approach),
# so a drift in either the mirror or the schema turns the pin red.
_UNCERTAINTY_METHODS = (
    "none",
    "first_order",
    "second_order",
    "auto",
    "sigma_point",
    "monte_carlo",
    "gaussian_mixture",
)
_UNCERTAINTY_ARMS = (
    "detection_on",
    "detection_off",
    "detection_off_assist_default_like",
)


def _split_tag(tag: str | None) -> tuple[str | None, str | None]:
    """Split a composite ``<method>_<arm>`` tag into ``(method, arm)``, or
    ``(None, None)`` for any tag outside the vocabulary. No prefix guessing: the
    method must be a whole ``_UNCERTAINTY_METHODS`` entry and the remainder a
    whole ``_UNCERTAINTY_ARMS`` entry, so ``second_order_detection_on`` reads as
    ``second_order`` + ``detection_on``, never ``second`` + the rest.
    """
    if tag is None:
        return (None, None)
    for method in _UNCERTAINTY_METHODS:
        if tag.startswith(method) and tag[len(method) : len(method) + 1] == "_":
            arm = tag[len(method) + 1 :]
            if arm in _UNCERTAINTY_ARMS:
                return (method, arm)
    return (None, None)


def _method_of(tag: str | None) -> str | None:
    """The method prefix of a composite tag (mirrors ``uncertainty_modes::method_of``)."""
    return _split_tag(tag)[0]


def _arm_of(tag: str | None) -> str | None:
    """The arm suffix of a composite tag (mirrors ``uncertainty_modes::arm_of``)."""
    return _split_tag(tag)[1]


# A LITERAL mirror of empyrean_validation::schema::OD_METHOD_AXIS_NOT_PRODUCED
# (src/schema.rs). `ODConfig` carries no `uncertainty_method` at this
# distribution revision (ae00643), so OD fits run method-free; every OD fit row
# records this by name rather than a blank or a silent first-order default. The
# literal is pinned against the schema const by the driver's tests (the same
# regex-on-schema.rs approach runners/python/tests/test_run_products.py uses),
# so a drift in either the mirror or the schema turns the pin red.
_OD_METHOD_AXIS_NOT_PRODUCED = (
    "OD method axis not produced at this pin: "
    "ODConfig.uncertainty_method not on the wrapper"
)

# Method tags that carry NO daemon method token: the covariance-free `none`
# method — under any arm — attaches no covariance and emits no product tokens,
# so the line stays byte-identical to the pre-widening 18-field line, and
# `None` is the untagged legacy row. Every covariance-bearing method's
# composite tag is sent as the optional 19th `prop` token; the binary refuses a
# tag outside the vocabulary by name (`fail unknown_uncertainty_method`). Monte
# Carlo's sample count and seed come from the binary (schema constants), so
# there is nothing to send beyond the tag.

# The 12 per-method product fields plus the 2 collapsed moment views that travel
# with them. A cli output row starts as a copy of its rust-channel input row, so
# each must be CLEARED before the cli channel repopulates it from ITS OWN binary
# delivery — otherwise the input channel's products ride out under this
# channel's name (a leak). Cleared by DELETE (not set-to-null) so a token-less
# row stays byte-identical to the pre-widening output, which omitted these keys,
# rather than gaining explicit `null`s. The cli binary emits no collapsed moment
# view, so emp_pos_cov_au2 / emp_radec_cov_arcsec2 are reset and never
# repopulated here (a named cli gap). Mirrors runners/python/run.py
# `_PER_METHOD_FIELDS`.
_PER_METHOD_FIELDS = (
    "resolved_method",
    "cov_kind",
    "cov_joint_width",
    "cov_tri",
    "orbit_delivered",
    "orbit_status",
    "mix_n_components_total",
    "mix_weight_delivered",
    "mix_n_failed",
    "mix_n_unresolved",
    "mix_n_curvature_refused",
    "mix_n_sky_linearization_refused",
    "emp_pos_cov_au2",
    "emp_radec_cov_arcsec2",
)


def _with_od_method_note(base: str) -> str:
    """Append the OD method-axis note to an OD fit row's base note so the row
    records the missing axis by name — never a blank, never a silent
    first-order default. An empty base yields the marker alone. Mirrors the
    rust / python channels' ``with_od_method_note``.
    """
    if not base:
        return _OD_METHOD_AXIS_NOT_PRODUCED
    return f"{base}; {_OD_METHOD_AXIS_NOT_PRODUCED}"


def _reset_per_method(row: dict) -> None:
    """Delete every inherited per-method product field (and the moment views
    that travel with them) from a cli output row, so the input channel's
    products never ride out under this channel's name. Delete rather than
    null-out so a token-less row serializes byte-identically to before.
    """
    for k in _PER_METHOD_FIELDS:
        row.pop(k, None)


def _method_token(r: dict) -> str | None:
    """The optional 19th ``prop`` token for a plan row: its
    ``propagation_uncertainty`` composite tag, or ``None`` for the
    covariance-free ``none`` method / untagged legacy row (which sends the bare
    18-field line). The method is read off the tag's prefix, so every arm of
    ``none`` sends the bare line.
    """
    m = r.get("propagation_uncertainty")
    return None if m is None or _method_of(m) == "none" else m


def _prop_daemon_line(r: dict) -> str | None:
    """The daemon ``prop`` line for a plan row: the 18 fixed fields plus, for a
    covariance-bearing method, the optional 19th method token. Returns ``None``
    when the row has no usable IC (the caller skips it). The ``none`` /
    untagged row yields the bare 18-field line — byte-identical to the
    pre-widening driver — so its response stays the 8-field ``ok`` line.
    """
    ic = _ic_line(r)
    if ic is None:
        return None
    method = _method_token(r)
    return f"prop {ic}" + (f" {method}" if method else "")


def _product_bool(v: str) -> bool | None:
    """Parse the binary's ``orbit_delivered`` flag (``1`` / ``0``); the literal
    ``na`` (never emitted for this field, handled defensively) → ``None``.
    """
    return None if v == "na" else (v == "1")


def _product_int(v: str) -> int | None:
    """Parse an integer product token (``cov_kind`` / ``cov_joint_width`` / the
    ``mix_n_*`` tallies); the literal ``na`` → ``None``."""
    return None if v == "na" else int(v)


def _product_float(v: str) -> float | None:
    """Parse a float product token (``mix_weight_delivered``); ``na`` → ``None``."""
    return None if v == "na" else float(v)


def _product_str(v: str) -> str | None:
    """Parse a string product token (``resolved_method`` / ``orbit_status``);
    ``na`` → ``None``."""
    return None if v == "na" else v


def _product_tri(v: str) -> list[float] | None:
    """Parse the ``cov_tri`` token: a comma-separated packed lower triangle, or
    the literal ``na`` → ``None``.
    """
    return None if v == "na" else [float(x) for x in v.split(",")]


# One parser per per-method product token the binary's ``Products::render``
# appends after the 8 fixed prop fields (runners/cli/src/main.rs). The token
# keys ARE the schema field names, so a parsed token maps straight onto the
# JSON row. The literal ``na`` (absent scalar / absent triangle) becomes null;
# ``cov_tri`` is a list of floats; ``orbit_delivered`` is a bool.
_PRODUCT_TOKEN_PARSERS = {
    "resolved_method": _product_str,
    "cov_kind": _product_int,
    "cov_joint_width": _product_int,
    "cov_tri": _product_tri,
    "orbit_delivered": _product_bool,
    "orbit_status": _product_str,
    "mix_n_components_total": _product_int,
    "mix_weight_delivered": _product_float,
    "mix_n_failed": _product_int,
    "mix_n_unresolved": _product_int,
    "mix_n_curvature_refused": _product_int,
    "mix_n_sky_linearization_refused": _product_int,
}


def _parse_products(tokens: list[str]) -> dict:
    """Parse the per-method product tokens the binary appends after the 8 fixed
    prop fields into the schema's 12 per-method JSON fields. Each token is a
    whitespace-free ``key=value``; an absent scalar is the literal ``na`` → JSON
    null, ``cov_tri`` a comma list of floats, ``orbit_delivered`` a bool. An
    unknown key or a missing field is refused by name (no hidden fallback), so a
    wire/driver drift surfaces loudly instead of silently dropping a product.
    """
    out: dict = {}
    for tok in tokens:
        key, sep, val = tok.partition("=")
        if not sep:
            raise ValueError(f"malformed product token (no '='): {tok!r}")
        parser = _PRODUCT_TOKEN_PARSERS.get(key)
        if parser is None:
            raise ValueError(f"unknown product token key: {key!r}")
        out[key] = parser(val)
    missing = set(_PRODUCT_TOKEN_PARSERS) - set(out)
    if missing:
        raise ValueError(f"missing product tokens: {sorted(missing)}")
    return out


def _build_prop_row(
    r: dict, parts: list[str], method: str | None, timestamp: str, source_version: str
) -> dict:
    """Assemble a cli-channel propagation output row from the daemon response
    ``parts`` (already split + validated). The 8 fixed fields (state + timing)
    are set exactly as the covariance-free path did; the per-method fields are
    reset — so a leaked input product never rides out under this channel — and,
    when a method token was sent, repopulated from the binary's product tokens.
    A token-less (``none`` / legacy) response leaves all 12 null,
    byte-identical to the pre-widening row.
    """
    x, y, z, _vx, _vy, _vz, ms = map(float, parts[1:8])
    new = dict(r)
    new["channel"] = "cli"
    new["timestamp"] = timestamp
    new["source_version"] = source_version
    _reset_per_method(new)
    new["emp_pos_au"] = [x, y, z]
    new["emp_time_ms"] = ms
    ref = r.get("ref_pos_au")
    if ref:
        d = math.sqrt(sum(([x, y, z][i] - ref[i]) ** 2 for i in range(3)))
        new["emp_vs_horizons_km"] = d * _AU_KM
    if method is not None:
        for k, v in _parse_products(parts[8:]).items():
            new[k] = v
        # The binary emits `resolved_method` as the delivered kind's bare method
        # name; compose it with this row's arm so it reads as a composite tag,
        # like `propagation_uncertainty`.
        delivered = new.get("resolved_method")
        arm = _arm_of(new.get("propagation_uncertainty"))
        if delivered is not None and arm is not None:
            new["resolved_method"] = f"{delivered}_{arm}"
    return new


def _driver_source_version() -> str:
    """Provenance string stamped on every cli-channel row.

    The CLI runner is a separate binary (empyrean-cli-runner) and exposes no
    version command over its daemon protocol, so this driver reports the
    version of the ``empyrean`` distribution installed in its own environment
    — labelled ``(driver-reported)`` because it is the driver's view, which
    may differ from the binary's linked engine if they were built separately.
    No-hidden-fallbacks: an unresolved version stamps an explicit
    ``unknown (<reason>)`` rather than a silent blank.
    """
    try:
        return f"empyrean {version('empyrean')} (driver-reported)"
    except PackageNotFoundError:
        return "empyrean unknown (empyrean distribution not found)"
    except Exception as e:  # noqa: BLE001
        return f"empyrean unknown ({e})"


def _ic_line(r):
    ic_pos = r.get("ic_pos_au")
    ic_vel = r.get("ic_vel_au_d")
    if ic_pos is None or ic_vel is None:
        return None
    tier = _TIER_TO_INT.get(r["force_model"])
    if tier is None:
        return None
    # Trailing DT (days). NaN = no delay (asteroids and short-period
    # comets without an SBDB-fit DT). Both runners must accept the
    # 18-field shape — this column is required, not optional.
    dt = r.get("ic_non_grav_dt")
    dt_s = "nan" if dt is None else repr(dt)
    return (
        f"{r['epoch_mjd_tdb']!r} "
        f"{ic_pos[0]!r} {ic_pos[1]!r} {ic_pos[2]!r} "
        f"{ic_vel[0]!r} {ic_vel[1]!r} {ic_vel[2]!r} "
        f"{r.get('ic_a1') or 0.0!r} {r.get('ic_a2') or 0.0!r} {r.get('ic_a3') or 0.0!r} "
        f"{r.get('ic_g_alpha') or 0.0!r} {r.get('ic_g_r0') or 0.0!r} "
        f"{r.get('ic_g_m') or 0.0!r} {r.get('ic_g_n') or 0.0!r} {r.get('ic_g_k') or 0.0!r} "
        f"{tier} "
        f"{r['t_mjd_tdb']!r} "
        f"{dt_s}"
    )


def _read_line(proc):
    out = proc.stdout.readline().strip()
    return out


def _mark_nonfinite_fail(row):
    """If a row carries any non-finite float (NaN/Inf) — a v0.10.0 engine
    defect, e.g. the 67P comet ephemeris position — rewrite it as a FAIL row:
    the offending fields and their raw values are recorded as strings in
    ``notes``, the offending numeric field is set to null (the schema's
    ``[f64; 3]`` cannot hold a partial/string value), and the row is KEPT.
    Returns True if the row had any non-finite value; other rows are untouched.
    Previously json.dumps(..., allow_nan=False) raised on the first such value
    and the all-or-nothing dump discarded every row of the channel."""

    def _nf(v):
        return isinstance(v, float) and not math.isfinite(v)

    def _scan(lst, path, hits):
        for i, e in enumerate(lst):
            if isinstance(e, list):
                _scan(e, f"{path}[{i}]", hits)
            elif _nf(e):
                hits.append(f"{path}[{i}]={e!r}")

    offenders = []
    for k, v in list(row.items()):
        if _nf(v):
            offenders.append(f"{k}={v!r}")
            row[k] = None
        elif isinstance(v, list):
            hits = []
            _scan(v, k, hits)
            if hits:
                offenders.append(f"{k} raw={v!r}")
                offenders.extend(hits)
                row[k] = None
    if offenders:
        msg = "FAIL non-finite (v0.10.0 engine output): " + "; ".join(offenders)
        prev = row.get("notes") or ""
        row["notes"] = f"{prev} | {msg}" if prev else msg
        return True
    return False


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument(
        "--input",
        required=True,
        type=Path,
        action="append",
        help="rust-channel JSON (repeat to feed prop+eph and OD inputs)",
    )
    p.add_argument(
        "--output",
        type=Path,
        default=Path("results/validation_cli.json"),
        help="output cli-channel JSON",
    )
    p.add_argument(
        "--runner",
        type=Path,
        default=Path(__file__).parent / "target" / "release" / "empyrean-cli-runner",
        help="CLI runner executable",
    )
    p.add_argument(
        "--data-dir",
        type=str,
        default=None,
        help="empyrean data directory (defaults to ~/.empyrean/data/)",
    )
    p.add_argument(
        "--fixtures-dir",
        type=Path,
        default=Path(__file__).parent.parent.parent / "fixtures" / "psv",
        help="PSV fixture directory for OD rows",
    )
    args = p.parse_args()

    if not args.runner.exists():
        print(
            f"runner not built at {args.runner}; run `cargo build --release` in cli/",
            file=sys.stderr,
        )
        return 2

    rust_rows = []
    for inp in args.input:
        rust_rows.extend(json.loads(inp.read_text()))
    if not rust_rows:
        print("input JSON(s) empty", file=sys.stderr)
        return 1

    # Per-object SBDB reference non-grav signal (A1/A2/A3). OD rows carry
    # ic_a* = null, so the reference for an OD object is looked up from its
    # propagation/ephemeris rows (which carry the SBDB-published A1/A2/A3).
    # Mirrors the rust runner's per-object check `a1 != 0 || a2 != 0 || a3 != 0`
    # — an object qualifies for the non_grav_recovery second pass when any of
    # its reference coefficients is non-zero.
    ref_nongrav = {}
    for r in rust_rows:
        a = (r.get("ic_a1"), r.get("ic_a2"), r.get("ic_a3"))
        if any(v for v in a) and r["object"] not in ref_nongrav:
            ref_nongrav[r["object"]] = a

    cmd = [str(args.runner), "--daemon"]
    if args.data_dir:
        cmd.extend(["--data-dir", args.data_dir])
    proc = subprocess.Popen(
        cmd,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )

    # Wait for "ready" on stderr (context loaded).
    while True:
        line = proc.stderr.readline()
        if not line:
            print("runner exited without ready signal", file=sys.stderr)
            return 1
        if line.strip() == "ready":
            break
        sys.stderr.write(line)

    print(
        f"Loaded {len(rust_rows)} rust rows; replaying through CLI channel...",
        file=sys.stderr,
    )

    timestamp = datetime.now(timezone.utc).isoformat()
    source_version = _driver_source_version()
    out_rows = []
    n_skipped = 0
    n_missing_fixture = 0

    for r in rust_rows:
        tt = r.get("test_type")
        # Per-method OD transport rows are `not produced` on this channel at
        # this pin (the core channel produces them); skipping them leaves their
        # cell reading `not produced` in the report — never a blank row.
        # Mirrors the rust / python channels.
        if tt == "orbit_determination_transport":
            n_skipped += 1
            continue
        if tt == "propagation":
            # Build the daemon line (18 fixed fields + optional method token).
            # `none` / untagged rows send the bare 18-field line, so their
            # response is the 8-field `ok` line; every other method sends its
            # tag so the binary attaches the synthetic covariance and appends
            # the delivered products.
            line = _prop_daemon_line(r)
            if line is None:
                n_skipped += 1
                continue
            method = _method_token(r)
            proc.stdin.write(f"{line}\n")
            proc.stdin.flush()
            out_line = _read_line(proc)
            if not out_line or out_line.startswith("fail"):
                print(
                    f"  {r['object']} dt={r['dt_days']:+.0f}d prop FAIL: {out_line}",
                    file=sys.stderr,
                )
                n_skipped += 1
                continue
            parts = out_line.split()
            # A method row carries the 12 product tokens after the 8 fixed
            # fields; a token-less (f64 / legacy) row is the 8-field line.
            expected = 20 if method is not None else 8
            if len(parts) != expected or parts[0] != "ok":
                print(f"  unexpected prop output: {out_line!r}", file=sys.stderr)
                n_skipped += 1
                continue
            out_rows.append(
                _build_prop_row(r, parts, method, timestamp, source_version)
            )

        elif tt == "ephemeris":
            ic = _ic_line(r)
            obs = r.get("observer")
            if ic is None or not obs:
                n_skipped += 1
                continue
            proc.stdin.write(f"eph {ic} {obs}\n")
            proc.stdin.flush()
            out_line = _read_line(proc)
            if not out_line or out_line.startswith("fail"):
                print(
                    f"  {r['object']} dt={r['dt_days']:+.0f}d eph FAIL: {out_line}",
                    file=sys.stderr,
                )
                # Emit a FAIL row carrying the engine's message rather than
                # dropping it (same no-silent-drop principle as the OD path).
                msg = (
                    out_line[len("fail"):].strip()
                    if out_line and out_line.startswith("fail")
                    else "no output from CLI runner"
                )
                fail_row = dict(r)
                fail_row["channel"] = "cli"
                fail_row["timestamp"] = timestamp
                fail_row["source_version"] = source_version
                fail_row["emp_pos_au"] = None
                fail_row["notes"] = f"ephemeris FAIL: {msg}"
                out_rows.append(fail_row)
                n_skipped += 1
                continue
            parts = out_line.split()
            if len(parts) != 6 or parts[0] != "ok":
                print(f"  unexpected eph output: {out_line!r}", file=sys.stderr)
                n_skipped += 1
                continue
            ra_deg, dec_deg, rho_au, lt_d, ms = map(float, parts[1:])
            new = dict(r)
            new["channel"] = "cli"
            new["timestamp"] = timestamp
            new["source_version"] = source_version
            # The cli ephemeris leg is covariance-free first order at this pin
            # (no method token, no product tokens on the eph line), so the 12
            # per-method fields are reset and left null — a named gap matching
            # the rust channel's None ephemeris products. Reset also clears any
            # per-method product inherited from the input row (no leak).
            _reset_per_method(new)
            new["emp_time_ms"] = ms
            ref_ra = r.get("ref_ra_rad")
            ref_dec = r.get("ref_dec_rad")
            if ref_ra is not None and ref_dec is not None:
                emp_ra_rad = math.radians(ra_deg)
                emp_dec_rad = math.radians(dec_deg)
                cos_d1 = math.cos(emp_dec_rad)
                cos_d2 = math.cos(ref_dec)
                sin_d1 = math.sin(emp_dec_rad)
                sin_d2 = math.sin(ref_dec)
                dra = ref_ra - emp_ra_rad
                n1 = cos_d2 * math.sin(dra)
                n2 = cos_d1 * sin_d2 - sin_d1 * cos_d2 * math.cos(dra)
                num = math.sqrt(n1 * n1 + n2 * n2)
                den = sin_d1 * sin_d2 + cos_d1 * cos_d2 * math.cos(dra)
                sep_rad = math.atan2(num, den)
                new["separation_arcsec"] = math.degrees(sep_rad) * 3600.0
                new["d_ra_arcsec"] = (
                    math.degrees((emp_ra_rad - ref_ra) * cos_d1) * 3600.0
                )
                new["d_dec_arcsec"] = math.degrees(emp_dec_rad - ref_dec) * 3600.0
            ref_rho = r.get("ref_rho_au")
            if ref_rho is not None:
                new["d_rho_km"] = (rho_au - ref_rho) * _AU_KM
            ref_lt = r.get("ref_light_time_d")
            if ref_lt is not None and not math.isnan(lt_d):
                new["d_light_time_s"] = (lt_d - ref_lt) * 86400.0
            out_rows.append(new)

        elif tt == "orbit_determination":
            # "/"-bearing object names (comets / interstellars) store the
            # fixture with the slash rewritten to "_".
            psv = args.fixtures_dir / f"{r['object'].replace('/', '_')}.psv"
            if not psv.exists():
                # Loudly, and fatally at the end — same seam as the C
                # driver. A silent `continue` deleted the OD row from this
                # channel's output while the run still exited 0, so a channel
                # that fitted nothing looked like one that fitted everything.
                # The fixtures are fetched + hash-verified by `make fixtures`
                # (fixtures/README.md); a missing one here means something
                # bypassed that gate, never a normal condition.
                print(
                    f"  {r['object']} OD FAIL: no PSV fixture at {psv}",
                    file=sys.stderr,
                )
                n_missing_fixture += 1
                n_skipped += 1
                continue
            tier = _TIER_TO_INT.get(r["force_model"])
            if tier is None:
                n_skipped += 1
                continue
            excl = r.get("excluded_perturbers_naif") or []
            exclude_naif = int(excl[0]) if excl else 0
            proc.stdin.write(f"od {tier} {exclude_naif} {psv}\n")
            proc.stdin.flush()
            out_line = _read_line(proc)
            if not out_line or out_line.startswith("fail"):
                print(f"  {r['object']} OD FAIL: {out_line}", file=sys.stderr)
                # Emit a FAIL row carrying the engine's message, exactly as the
                # rust / core / python channels do, instead of silently dropping
                # the object. out_line is "fail <message>"; strip the marker.
                msg = (
                    out_line[len("fail"):].strip()
                    if out_line and out_line.startswith("fail")
                    else "no output from CLI runner"
                )
                fail_row = dict(r)
                fail_row["channel"] = "cli"
                fail_row["timestamp"] = timestamp
                fail_row["source_version"] = source_version
                fail_row["emp_pos_au"] = None
                fail_row["od_converged"] = False
                fail_row["notes"] = f"determine FAIL: {msg}"
                out_rows.append(fail_row)
                n_skipped += 1
                continue
            parts = out_line.split()
            # 9 base fields (ok + state[6] + iters + ms) + 6 non-grav fields
            # (a1 a2 a3 σ1 σ2 σ3) + optical rms + non-grav rms + non-grav
            # position[3] = 20 fields.
            if len(parts) != 20 or parts[0] != "ok":
                print(f"  unexpected od output: {out_line!r}", file=sys.stderr)
                n_skipped += 1
                continue
            x, y, z = map(float, parts[1:4])
            iters = int(parts[7])
            ms = float(parts[8])
            od_rms = float(parts[15])
            new = dict(r)
            new["channel"] = "cli"
            new["timestamp"] = timestamp
            new["source_version"] = source_version
            # OD fits run method-free at this pin — ODConfig carries no
            # uncertainty_method on the wrapper (ae00643) — so the 12 per-method
            # fields are reset and left null, and every OD fit row records the
            # missing method axis by name (never a blank or a silent first-order
            # default). The non_grav_recovery row below is a dict(r) copy that
            # carries the same note.
            _reset_per_method(new)
            new["emp_pos_au"] = [x, y, z]
            new["emp_time_ms"] = ms
            new["od_iterations"] = iters
            new["od_rms_combined_arcsec"] = od_rms
            new["notes"] = _with_od_method_note(new.get("notes") or "")
            out_rows.append(new)

            # Second OD pass: state + non-grav (9-param) on the same optical
            # arc. Only objects whose SBDB reference carries a non-grav signal
            # get a `non_grav_recovery` row (mirrors the rust runner's per-
            # object `a1 != 0 || a2 != 0 || a3 != 0` gate). The runner emitted
            # the fitted A1/A2/A3 ± σ in the trailing six fields; a value of
            # NaN means "non-grav not recovered" (9×9 covariance absent or the
            # fitted coefficient non-finite) and is recorded as JSON null —
            # never 0 — so a missing value reads as a non-recovery, not zero.
            if r["object"] in ref_nongrav:
                ng_a1, ng_a2, ng_a3, ng_s1, ng_s2, ng_s3 = map(float, parts[9:15])
                ng_rms = float(parts[16])
                ng_px, ng_py, ng_pz = map(float, parts[17:20])

                def _or_none(v):
                    return None if math.isnan(v) else v

                ref_a1, ref_a2, ref_a3 = ref_nongrav[r["object"]]
                ng = dict(r)
                ng["channel"] = "cli"
                ng["timestamp"] = timestamp
                ng["source_version"] = source_version
                # Mirrors the OD fit row: method-free at this pin, so the 12
                # per-method fields are reset and the shared OD method-axis note
                # is recorded by name.
                _reset_per_method(ng)
                ng["notes"] = _with_od_method_note(ng.get("notes") or "")
                ng["test_type"] = "non_grav_recovery"
                # Position + rms of the NON-GRAV fit, not the optical-only one.
                ng["emp_pos_au"] = [ng_px, ng_py, ng_pz]
                ng["emp_time_ms"] = ms
                ng["od_iterations"] = iters
                ng["od_rms_combined_arcsec"] = ng_rms
                # JPL SBDB reference A1/A2/A3 for the fitted-vs-reference
                # comparison (OD rows carry ic_a* = null; backfill from the
                # per-object reference looked up off the prop/eph rows).
                ng["ic_a1"] = ref_a1
                ng["ic_a2"] = ref_a2
                ng["ic_a3"] = ref_a3
                ng["od_a1"] = _or_none(ng_a1)
                ng["od_a2"] = _or_none(ng_a2)
                ng["od_a3"] = _or_none(ng_a3)
                ng["od_a1_sigma"] = _or_none(ng_s1)
                ng["od_a2_sigma"] = _or_none(ng_s2)
                ng["od_a3_sigma"] = _or_none(ng_s3)
                out_rows.append(ng)

        elif tt == "orbit_determination_radar":
            # Optical+radar OD. The CLI runner's `od` command reads its ADES
            # file through read_ades, which folds a <radar> delay/Doppler table
            # into the fit, so pointing it at the psv-radar fixture (the same
            # optical arc plus radar) fits optical+radar under the same config
            # as the optical row. No non_grav second pass — this row is the
            # radar-tightened state fit, cross-checked against find_orb's radar
            # fit the way the optical row is against its optical fit.
            radar_dir = args.fixtures_dir.parent / "psv-radar"
            psv = radar_dir / f"{r['object'].replace('/', '_')}.psv"
            if not psv.exists():
                print(
                    f"  {r['object']} radar OD FAIL: no radar PSV fixture at {psv}",
                    file=sys.stderr,
                )
                n_missing_fixture += 1
                n_skipped += 1
                continue
            tier = _TIER_TO_INT.get(r["force_model"])
            if tier is None:
                n_skipped += 1
                continue
            excl = r.get("excluded_perturbers_naif") or []
            exclude_naif = int(excl[0]) if excl else 0
            proc.stdin.write(f"od {tier} {exclude_naif} {psv}\n")
            proc.stdin.flush()
            out_line = _read_line(proc)
            if not out_line or out_line.startswith("fail"):
                print(f"  {r['object']} radar OD FAIL: {out_line}", file=sys.stderr)
                msg = (
                    out_line[len("fail"):].strip()
                    if out_line and out_line.startswith("fail")
                    else "no output from CLI runner"
                )
                fail_row = dict(r)
                fail_row["channel"] = "cli"
                fail_row["timestamp"] = timestamp
                fail_row["source_version"] = source_version
                fail_row["emp_pos_au"] = None
                fail_row["od_converged"] = False
                fail_row["notes"] = f"radar determine FAIL: {msg}"
                out_rows.append(fail_row)
                n_skipped += 1
                continue
            parts = out_line.split()
            if len(parts) != 20 or parts[0] != "ok":
                print(f"  unexpected radar od output: {out_line!r}", file=sys.stderr)
                n_skipped += 1
                continue
            x, y, z = map(float, parts[1:4])
            iters = int(parts[7])
            ms = float(parts[8])
            od_rms = float(parts[15])
            new = dict(r)
            new["channel"] = "cli"
            new["timestamp"] = timestamp
            new["source_version"] = source_version
            new["emp_pos_au"] = [x, y, z]
            new["emp_time_ms"] = ms
            new["od_iterations"] = iters
            new["od_rms_combined_arcsec"] = od_rms
            _prev = new.get("notes") or ""
            new["notes"] = f"{_prev} | optical+radar" if _prev else "optical+radar"
            out_rows.append(new)

        else:
            n_skipped += 1

    proc.stdin.close()
    proc.wait(timeout=5)

    args.output.parent.mkdir(parents=True, exist_ok=True)
    _n_nonfinite = sum(_mark_nonfinite_fail(r) for r in out_rows)
    if _n_nonfinite:
        print(
            f"  NOTE: {_n_nonfinite} row(s) carried a non-finite engine value; "
            "written as FAIL rows (offending fields + raw values in notes, "
            "numeric field nulled) instead of discarding the whole channel.",
            file=sys.stderr,
        )
    try:
        _payload = json.dumps(out_rows, indent=2, default=str, allow_nan=False)
    except ValueError as _e:
        print(
            f"ERROR: refusing to write non-finite values to {args.output}: {_e}\n"
            "       Bare NaN/Infinity is invalid JSON — Rust's serde_json rejects it, so this\n"
            "       whole channel would fail the reduce merge with a line number and no cause.\n"
            "       A quantity that could not be computed must be null.",
            file=sys.stderr,
        )
        raise
    args.output.write_text(_payload)
    print(
        f"Wrote {len(out_rows)} cli rows to {args.output} (skipped {n_skipped})",
        file=sys.stderr,
    )
    if n_missing_fixture:
        print(
            f"ERROR: {n_missing_fixture} OD row(s) had no PSV fixture under "
            f"{args.fixtures_dir}.\n"
            "       The fixtures come from the GCS snapshot pinned by "
            "fixtures/manifest.json;\n"
            "       `make fixtures` fetches + verifies them. Those OD rows are "
            "missing from this "
            "channel's output entirely.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
