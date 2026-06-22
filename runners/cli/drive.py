"""empyrean validation runner — CLI channel driver.

Reads a rust-channel ValidationResult JSON, replays each row through
the empyrean-cli-runner binary in daemon mode (loaded once, one row
per stdin line), and emits a cli-channel JSON in the same schema.

The runner protocol is one row per stdin line, prefixed with mode:
- "prop EPOCH IC[6] A1-3 G[5] FORCE TARGET"
- "eph  EPOCH IC[6] A1-3 G[5] FORCE TARGET OBS_CODE"
- "od   FORCE ADES_PATH"

This mirrors the C runner's protocol (validation/runners/c/drive.py),
so both channels use the same fork-once-stream-many model.
"""

from __future__ import annotations

import argparse
import json
import math
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path


_TIER_TO_INT = {"approximate": 0, "basic": 1, "standard": 2}
_AU_KM = 149_597_870.700


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
    out_rows = []
    n_skipped = 0

    for r in rust_rows:
        tt = r.get("test_type")
        # Uncertainty axis: skip Jet1 rows. The CLI runner daemon
        # protocol does not yet accept a covariance for the input orbit;
        # cross-channel Jet1 parity is a follow-up.
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
            new["channel"] = "cli"
            new["timestamp"] = timestamp
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
            new["channel"] = "cli"
            new["timestamp"] = timestamp
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
            new["emp_pos_au"] = [x, y, z]
            new["emp_time_ms"] = ms
            new["od_iterations"] = iters
            new["od_rms_combined_arcsec"] = od_rms
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

        else:
            n_skipped += 1

    proc.stdin.close()
    proc.wait(timeout=5)

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(out_rows, indent=2, default=str))
    print(
        f"Wrote {len(out_rows)} cli rows to {args.output} (skipped {n_skipped})",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
