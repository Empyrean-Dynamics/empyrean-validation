"""empyrean validation runner — C channel driver.

Reads a rust-channel ValidationResult JSON, replays each row through
the C executable at validation/runners/c/runner (built via `make`),
and emits a c-channel JSON in the same schema. Output `emp_pos_au`,
`emp_time_ms`, plus ephemeris/OD-specific fields are repopulated from
the C runner's stdout.

The runner protocol is one row per stdin line, prefixed with mode:
- "prop EPOCH IC[6] A1-3 G[5] FORCE TARGET DT [METHOD]"
- "eph  EPOCH IC[6] A1-3 G[5] FORCE TARGET DT OBS_CODE [METHOD]"
- "od   FORCE EXCLUDE_NAIF METHOD ADES_PATH"
- "odng FORCE EXCLUDE_NAIF ADES_PATH"

This mirrors the CLI runner's protocol (validation/runners/cli/drive.py),
so both channels use the same fork-once-stream-many model.

# Per-method uncertainty axis (0.11)

Each plan row's ``propagation_uncertainty`` composite ``<method>_<arm>`` tag is
replayed through the C ABI's uncertainty surface, parsing what the engine
*delivered* (never the request) into the schema's 12 per-method fields:
``resolved_method``, ``cov_kind``, ``cov_joint_width``, ``cov_tri``,
``orbit_delivered``, ``orbit_status`` and the six ``mix_*``.

- **Propagation rows** send the tag as the optional 19th ``prop`` token (all
  tags but ``none`` / the untagged legacy row, which send the bare 18-field
  line). The binary attaches the synthetic input covariance, runs the rung and
  appends 12 product tokens read off the delivered packed joint (``orbit_cov``),
  the per-orbit outcome (``outcomes[0]``) and — for a mixture — ``mixture_tally``.
  A token-less response leaves all 12 fields null, byte-identical to the
  pre-widening row.
- **Ephemeris rows** send the tag as the optional trailing ``eph`` token after
  the observer, exactly like the prop line's 19th token. The binary attaches the
  synthetic covariance, runs the rung and appends the same 12 product tokens read
  off the delivered ephemeris ENTRY's own packed joint (``entry.joint``), the
  per-orbit outcome (``outcomes[0]``) and the entry's PRE-RETENTION
  ``mixture_tally``. The bare ``resolved_method`` is composed with the row's arm.
  A token-less row leaves all 12 fields null, byte-identical to before.
- **OD fit rows** send the tag as the ``od`` command's METHOD token. ``-`` is
  the legacy untagged fit (first-order + non-grav recovery). A tagged fit
  returns the delivered products (``first_order`` / ``auto`` deliver; ``none``
  delivers covariance-free) or a ``refused <engine text>`` line for a method the
  engine refuses BY NAME, recorded as ``orbit_delivered = false`` with the
  refusal in ``orbit_status``.

Products NOT reachable through the C ABI at this pin are named "not produced",
never a blank or a zero:
- **Collapsed moment views** (``emp_pos_cov_au2`` / ``emp_radec_cov_arcsec2``) —
  a named gap on the prop and ephemeris rows: the C binary emits no
  Jacobian-projected 6×6. The per-method joint products themselves ARE carried
  on the ephemeris rows (above).
- **``orbit_determination_transport`` rows** — not produced on this channel at
  this pin (the core channel produces them); skipped so the report renders
  "not produced" for the cell, mirroring the rust / python / cli channels.
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
# so a drift in either the mirror or the schema turns the pin red. Identical to
# the cli driver's copy (runners/cli/drive.py).
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


# The 12 per-method product fields plus the 2 collapsed moment views that travel
# with them. A c output row starts as a copy of its rust-channel input row, so
# each must be CLEARED before the c channel repopulates it from ITS OWN binary
# delivery — otherwise the input channel's products ride out under this
# channel's name (a leak). Cleared by DELETE (not set-to-null) so a token-less
# row stays byte-identical to the pre-widening output, which omitted these keys.
# The c binary emits no collapsed moment view, so emp_pos_cov_au2 /
# emp_radec_cov_arcsec2 are reset and never repopulated here (a named c gap).
# Mirrors runners/cli/drive.py `_PER_METHOD_FIELDS`.
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


def _reset_per_method(row: dict) -> None:
    """Delete every inherited per-method product field (and the moment views
    that travel with them) from a c output row, so the input channel's products
    never ride out under this channel's name. Delete rather than null-out so a
    token-less row serializes byte-identically to before.
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


# One parser per per-method product token the binary's ``render_products``
# appends (runners/c/runner.c). The token keys ARE the schema field names, so a
# parsed token maps straight onto the JSON row. The literal ``na`` (absent
# scalar / absent triangle) becomes null; ``cov_tri`` is a list of floats;
# ``orbit_delivered`` is a bool. Identical to the cli driver's map.
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
    """Parse the per-method product tokens the binary appends into the schema's
    12 per-method JSON fields. Each token is a whitespace-free ``key=value``; an
    absent scalar is the literal ``na`` → JSON null, ``cov_tri`` a comma list of
    floats, ``orbit_delivered`` a bool. An unknown key or a missing field is
    refused by name (no hidden fallback), so a wire/driver drift surfaces loudly
    instead of silently dropping a product.
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


def _driver_source_version() -> str:
    """Provenance string stamped on every c-channel row.

    The C runner is a separate binary linked against the empyrean C ABI and
    exposes no version command, so this driver reports the version of the
    ``empyrean`` distribution installed in its own environment — labelled
    ``(driver-reported)`` because it is the driver's view, which may differ
    from the binary's linked libempyrean if they were built separately.
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


def _prop_daemon_line(r) -> str | None:
    """The runner ``prop`` line for a plan row: the 18 fixed fields plus, for a
    covariance-bearing method, the optional 19th method token. Returns ``None``
    when the row has no usable IC (the caller skips it). The ``none`` / untagged
    row yields the bare 18-field line — byte-identical to the pre-widening
    driver — so its response stays the 8-field ``ok`` line. Mirrors the cli
    driver's ``_prop_daemon_line``.
    """
    ic = _ic_line(r)
    if ic is None:
        return None
    method = _method_token(r)
    return f"prop {ic}" + (f" {method}" if method else "")


def _build_prop_row(r, parts, method, timestamp, source_version) -> dict:
    """Assemble a c-channel propagation output row from the runner response
    ``parts`` (already split + validated). The 8 fixed fields (state + timing)
    are set exactly as the covariance-free path did; the per-method fields are
    reset — so a leaked input product never rides out under this channel — and,
    when a method token was sent, repopulated from the binary's product tokens.
    A token-less (``none`` / legacy) response leaves all 12 null, byte-identical
    to the pre-widening row. Mirrors the cli driver's ``_build_prop_row``.
    """
    x, y, z, _vx, _vy, _vz, ms = map(float, parts[1:8])
    new = dict(r)
    new["channel"] = "c"
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


def _read_line(proc):
    out = proc.stdout.readline().strip()
    return out


def _float_or_none(tok):
    """Parse one `odng` coefficient/σ token. The runner emits the literal
    "null" when non-grav was not actually recovered (9×9 covariance absent
    or a non-finite fit); map that — and any NaN that slips through — to
    None so the row reads loudly as "non-grav not recovered" rather than 0."""
    if tok == "null":
        return None
    v = float(tok)
    return None if math.isnan(v) else v


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
        default=Path("results/validation_c.json"),
        help="output c-channel JSON",
    )
    p.add_argument(
        "--runner",
        type=Path,
        default=Path(__file__).parent / "runner",
        help="C runner executable",
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
        print(f"runner not built at {args.runner}; run `make` in c/", file=sys.stderr)
        return 2

    rust_rows = []
    for inp in args.input:
        rust_rows.extend(json.loads(inp.read_text()))
    if not rust_rows:
        print("input JSON(s) empty", file=sys.stderr)
        return 1

    # Per-object reference non-grav signal, keyed by object name. The
    # `orbit_determination` rows null out ic_a1/ic_a2/ic_a3, so read the
    # JPL SBDB reference off the object's `propagation` rows (which carry
    # it). Mirrors the rust runner's per-object gate
    # `data.a1 != 0 || data.a2 != 0 || data.a3 != 0` — only objects with a
    # known non-grav signal get a second StateAndNonGrav fit.
    ref_non_grav: dict[str, tuple[float, float, float]] = {}
    for r in rust_rows:
        a1 = r.get("ic_a1") or 0.0
        a2 = r.get("ic_a2") or 0.0
        a3 = r.get("ic_a3") or 0.0
        if a1 != 0.0 or a2 != 0.0 or a3 != 0.0:
            ref_non_grav[r["object"]] = (a1, a2, a3)

    cmd = [str(args.runner)]
    if args.data_dir:
        cmd.append(args.data_dir)
    proc = subprocess.Popen(
        cmd,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )

    # Wait for "ready"
    while True:
        line = proc.stderr.readline()
        if not line:
            print("runner exited without ready signal", file=sys.stderr)
            return 1
        if line.strip() == "ready":
            break
        sys.stderr.write(line)

    print(
        f"Loaded {len(rust_rows)} rust rows; replaying through C channel...",
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
        # Mirrors the rust / python / cli channels.
        if tt == "orbit_determination_transport":
            n_skipped += 1
            continue
        if tt == "propagation":
            # The daemon line: 18 fixed fields + optional method token. Every
            # covariance-bearing method sends its tag so the binary attaches the
            # synthetic covariance and appends the 12 products; `none` / the
            # untagged legacy row send the bare line.
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
            # fields; a token-less (none / legacy) row is the 8-field line.
            expected = 20 if method is not None else 8
            if len(parts) != expected or parts[0] != "ok":
                print(f"  unexpected prop output: {out_line!r}", file=sys.stderr)
                n_skipped += 1
                continue
            out_rows.append(_build_prop_row(r, parts, method, timestamp, source_version))

        elif tt == "ephemeris":
            ic = _ic_line(r)
            obs = r.get("observer")
            if ic is None or not obs:
                n_skipped += 1
                continue
            # The optional method token rides after the observer, exactly like
            # the prop line's 19th token: every method but `none` attaches the
            # synthetic covariance so the entry delivers a joint; `none` / the
            # untagged row sends the bare line, byte-identical to before.
            method = _method_token(r)
            eph_line = f"eph {ic} {obs}" + (f" {method}" if method else "")
            proc.stdin.write(f"{eph_line}\n")
            proc.stdin.flush()
            out_line = _read_line(proc)
            if not out_line or out_line.startswith("fail"):
                print(
                    f"  {r['object']} dt={r['dt_days']:+.0f}d eph FAIL: {out_line}",
                    file=sys.stderr,
                )
                n_skipped += 1
                continue
            parts = out_line.split()
            # A method row carries the 12 product tokens after the 5 fixed eph
            # fields (ra dec rho lt ms); a token-less row is the 6-field line.
            expected = 18 if method is not None else 6
            if len(parts) != expected or parts[0] != "ok":
                print(f"  unexpected eph output: {out_line!r}", file=sys.stderr)
                n_skipped += 1
                continue
            ra_deg, dec_deg, rho_au, lt_d, ms = map(float, parts[1:6])
            new = dict(r)
            new["channel"] = "c"
            new["timestamp"] = timestamp
            new["source_version"] = source_version
            # Reset the 12 per-method fields + 2 moment views first (so an
            # inherited rust product never leaks), then — when a method token
            # was sent — repopulate them from the binary's product tokens read
            # off the delivered entry's packed joint.
            _reset_per_method(new)
            new["emp_time_ms"] = ms
            if method is not None:
                for k, v in _parse_products(parts[6:]).items():
                    new[k] = v
                # The binary emits `resolved_method` as the delivered kind's bare
                # method name; compose it with this row's arm so it reads as a
                # composite tag, like the prop line (`_build_prop_row`).
                delivered = new.get("resolved_method")
                arm = _arm_of(new.get("propagation_uncertainty"))
                if delivered is not None and arm is not None:
                    new["resolved_method"] = f"{delivered}_{arm}"
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
            # Object names with "/" (the comets / interstellars) store the
            # fixture with the slash rewritten to "_" so it is not read as a
            # path separator.
            psv = args.fixtures_dir / f"{r['object'].replace('/', '_')}.psv"
            if not psv.exists():
                # Loudly, and fatally at the end. This used to skip with no
                # message at all: the OD row vanished from the C channel's
                # output and the run still exited 0, so a channel that fitted
                # nothing was indistinguishable from one that fitted
                # everything. The fixtures are fetched + hash-verified by
                # `make fixtures` (fixtures/README.md), so a missing one here
                # means something bypassed that gate, never a normal condition.
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
            # Pick first excluded NAIF id (currently at most one — the
            # body itself for SB441-N16 self-perturbers); 0 = no exclusion.
            excl = r.get("excluded_perturbers_naif") or []
            exclude_naif = int(excl[0]) if excl else 0
            # The OD method axis: the input row's tag selects the fit's
            # `ODConfig.uncertainty_method`. `-` is the legacy untagged fit
            # (first-order + non-grav recovery, the 10-field line); a composite
            # tag runs that method.
            tag = r.get("propagation_uncertainty")
            method_token = "-" if tag is None else tag
            proc.stdin.write(f"od {tier} {exclude_naif} {method_token} {psv}\n")
            proc.stdin.flush()
            out_line = _read_line(proc)
            if out_line and out_line.startswith("refused"):
                # A method-tagged fit the engine refused BY NAME (the richer
                # methods have no honest first-order implementation): record the
                # refusal, never a silent downgrade or a dropped row. The refusal
                # text is the trailing field (spaces preserved) and names the
                # engine method.
                text = out_line[len("refused") :].strip()
                new = dict(r)
                new["channel"] = "c"
                new["timestamp"] = timestamp
                new["source_version"] = source_version
                _reset_per_method(new)
                new["emp_pos_au"] = None
                new["orbit_delivered"] = False
                new["orbit_status"] = text
                method_name = _method_of(tag)
                base = new.get("notes") or ""
                refused = f"uncertainty method {method_name} refused by the engine"
                new["notes"] = refused if not base else f"{base}; {refused}"
                out_rows.append(new)
                continue
            if not out_line or out_line.startswith("fail"):
                print(f"  {r['object']} OD FAIL: {out_line}", file=sys.stderr)
                n_skipped += 1
                continue
            parts = out_line.split()

            if tag is not None:
                # ── Method-tagged delivered fit ────────────────────────────
                # `ok` + state[6] + iters + ms + the per-method product tokens
                # (runner.c `render_products`). No non-grav second pass — that
                # rides the legacy untagged fit, once per object.
                if parts[0] != "ok" or len(parts) < 9:
                    print(f"  unexpected od output: {out_line!r}", file=sys.stderr)
                    n_skipped += 1
                    continue
                x, y, z = map(float, parts[1:4])
                iters = int(parts[7])
                ms = float(parts[8])
                new = dict(r)
                new["channel"] = "c"
                new["timestamp"] = timestamp
                new["source_version"] = source_version
                _reset_per_method(new)
                new["emp_pos_au"] = [x, y, z]
                new["emp_time_ms"] = ms
                new["od_iterations"] = iters
                for k, v in _parse_products(parts[9:]).items():
                    new[k] = v
                # The binary emits `resolved_method` as the delivered kind's bare
                # method name; compose it with this row's arm (like the prop
                # rows) so it reads as a composite tag.
                delivered = new.get("resolved_method")
                arm = _arm_of(tag)
                if delivered is not None and arm is not None:
                    new["resolved_method"] = f"{delivered}_{arm}"
                out_rows.append(new)
                continue

            # ── Legacy untagged fit: the 10-field optical line ─────────────
            # ok + state[6] + iters + ms + rms_combined = 10 fields.
            if len(parts) != 10 or parts[0] != "ok":
                print(f"  unexpected od output: {out_line!r}", file=sys.stderr)
                n_skipped += 1
                continue
            x, y, z = map(float, parts[1:4])
            iters = int(parts[7])
            ms = float(parts[8])
            od_rms = float(parts[9])
            new = dict(r)
            new["channel"] = "c"
            new["timestamp"] = timestamp
            new["source_version"] = source_version
            # The legacy untagged fit carries no OD method axis and no per-method
            # products (reset above); the method-tagged rows carry it. The
            # non_grav_recovery row below is a dict(r) copy.
            _reset_per_method(new)
            new["emp_pos_au"] = [x, y, z]
            new["emp_time_ms"] = ms
            new["od_iterations"] = iters
            new["od_rms_combined_arcsec"] = od_rms
            out_rows.append(new)

            # ── Second OD: non-grav recovery ──────────────────────────────
            # For objects with a known SBDB non-grav signal (a1/a2/a3 != 0,
            # the same gate the rust runner applies), run a second determine
            # with solve_for = StateAndNonGrav on the SAME optical fixture and
            # emit a `non_grav_recovery` row carrying the FITTED A1/A2/A3 + 1σ
            # (od_a*/od_a*_sigma) so the report can compare fitted-vs-JPL in σ.
            # The runner emits "null" for both a coefficient and its σ when
            # non-grav was not actually recovered (9×9 covariance absent — the
            # current engine bug where StateAndNonGrav silently falls back to a
            # 6-param state-only fit — or a non-finite value); those map to
            # None here, never 0/NaN.
            if r["object"] in ref_non_grav:
                proc.stdin.write(f"odng {tier} {exclude_naif} {psv}\n")
                proc.stdin.flush()
                ng_line = _read_line(proc)
                if not ng_line or ng_line.startswith("fail"):
                    print(
                        f"  {r['object']} non-grav OD FAIL: {ng_line}", file=sys.stderr
                    )
                    n_skipped += 1
                else:
                    ng_parts = ng_line.split()
                    # ok + a[3] + sigma[3] + iters + ms + rms + pos[3] = 13.
                    if len(ng_parts) != 13 or ng_parts[0] != "ok":
                        print(f"  unexpected odng output: {ng_line!r}", file=sys.stderr)
                        n_skipped += 1
                    else:
                        od_a1 = _float_or_none(ng_parts[1])
                        od_a2 = _float_or_none(ng_parts[2])
                        od_a3 = _float_or_none(ng_parts[3])
                        od_a1_sigma = _float_or_none(ng_parts[4])
                        od_a2_sigma = _float_or_none(ng_parts[5])
                        od_a3_sigma = _float_or_none(ng_parts[6])
                        ng_iters = int(ng_parts[7])
                        ng_ms = float(ng_parts[8])
                        # rms + fitted position of the NON-GRAV solution, so
                        # the row reports the 9-param fit's residual / state
                        # rather than inheriting the optical-only values.
                        ng_rms = float(ng_parts[9])
                        ng_x, ng_y, ng_z = map(float, ng_parts[10:13])
                        ng_row = dict(r)
                        ng_row["channel"] = "c"
                        ng_row["timestamp"] = timestamp
                        ng_row["source_version"] = source_version
                        # Mirrors the legacy OD fit row: no OD method axis, so
                        # the 12 per-method fields are reset.
                        _reset_per_method(ng_row)
                        ng_row["test_type"] = "non_grav_recovery"
                        ng_row["emp_pos_au"] = [ng_x, ng_y, ng_z]
                        ng_row["emp_time_ms"] = ng_ms
                        ng_row["od_iterations"] = ng_iters
                        ng_row["od_rms_combined_arcsec"] = ng_rms
                        ng_row["od_a1"] = od_a1
                        ng_row["od_a2"] = od_a2
                        ng_row["od_a3"] = od_a3
                        ng_row["od_a1_sigma"] = od_a1_sigma
                        ng_row["od_a2_sigma"] = od_a2_sigma
                        ng_row["od_a3_sigma"] = od_a3_sigma
                        out_rows.append(ng_row)

        else:
            n_skipped += 1

    proc.stdin.close()
    proc.wait(timeout=5)

    args.output.parent.mkdir(parents=True, exist_ok=True)
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
        f"Wrote {len(out_rows)} c rows to {args.output} (skipped {n_skipped})",
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
