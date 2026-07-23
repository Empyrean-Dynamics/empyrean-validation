"""empyrean validation runner — C channel driver.

Reads a rust-channel ValidationResult JSON, replays each row through
the C executable at validation/runners/c/runner (built via `make`),
and emits a c-channel JSON in the same schema. Output `emp_pos_au`,
`emp_time_ms`, plus ephemeris/OD-specific fields are repopulated from
the C runner's stdout.

The runner protocol is one row per stdin line, prefixed with mode:
- "prop EPOCH IC[6] A1-3 G[5] FORCE TARGET"
- "eph  EPOCH IC[6] A1-3 G[5] FORCE TARGET OBS_CODE"
- "od   ADES_PATH FORCE"
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

    for r in rust_rows:
        tt = r.get("test_type")
        # Uncertainty axis: skip Jet1 rows. The C runner stdin protocol
        # does not yet accept a covariance for the input orbit; the
        # cross-channel Jet1 parity comparison is a follow-up.
        if r.get("propagation_uncertainty") == "first_order_with_cov":
            n_skipped += 1
            continue
        if tt == "propagation":
            ic = _ic_line(r)
            if ic is None:
                n_skipped += 1
                continue
            proc.stdin.write(f"prop {ic}\n")
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
            if len(parts) != 8 or parts[0] != "ok":
                print(f"  unexpected prop output: {out_line!r}", file=sys.stderr)
                n_skipped += 1
                continue
            x, y, z, _vx, _vy, _vz, ms = map(float, parts[1:])
            new = dict(r)
            new["channel"] = "c"
            new["timestamp"] = timestamp
            new["source_version"] = source_version
            new["emp_pos_au"] = [x, y, z]
            new["emp_time_ms"] = ms
            ref = r.get("ref_pos_au")
            if ref:
                d = math.sqrt(sum(([x, y, z][i] - ref[i]) ** 2 for i in range(3)))
                new["emp_vs_horizons_km"] = d * _AU_KM
            out_rows.append(new)

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
                n_skipped += 1
                continue
            parts = out_line.split()
            if len(parts) != 6 or parts[0] != "ok":
                print(f"  unexpected eph output: {out_line!r}", file=sys.stderr)
                n_skipped += 1
                continue
            ra_deg, dec_deg, rho_au, lt_d, ms = map(float, parts[1:])
            new = dict(r)
            new["channel"] = "c"
            new["timestamp"] = timestamp
            new["source_version"] = source_version
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
            # Object names with "/" (the comets / interstellars) store the
            # fixture with the slash rewritten to "_" so it is not read as a
            # path separator.
            psv = args.fixtures_dir / f"{r['object'].replace('/', '_')}.psv"
            if not psv.exists():
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
            proc.stdin.write(f"od {tier} {exclude_naif} {psv}\n")
            proc.stdin.flush()
            out_line = _read_line(proc)
            if not out_line or out_line.startswith("fail"):
                print(f"  {r['object']} OD FAIL: {out_line}", file=sys.stderr)
                n_skipped += 1
                continue
            parts = out_line.split()
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
    args.output.write_text(json.dumps(out_rows, indent=2, default=str))
    print(
        f"Wrote {len(out_rows)} c rows to {args.output} (skipped {n_skipped})",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
