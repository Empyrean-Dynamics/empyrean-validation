"""jorbit external-reference runner for the empyrean validation suite.

Reads the canonical test plan (`validation_plan.json`) and replays each
row through jorbit (Ben Cassese's JAX-based N-body integrator). Emits
one row per input row with jorbit-specific fields populated. Output
JSON is consumed by the empyrean-validation report renderer alongside
ASSIST, find_orb, kete, and pyoorb.

jorbit is GPL-licensed and never linked into empyrean. This script
runs independently in its own virtual environment.

Schema mirrors the ValidationResult shape used by every other channel.
jorbit-specific fields:
    jorbit_pos_au              — propagated Cartesian state at t_mjd_tdb
    jorbit_vs_horizons_km      — |jorbit_pos - ref_pos| (propagation rows)
    jorbit_separation_arcsec   — angular separation vs Horizons (eph rows)
    jorbit_d_ra_arcsec         — dRA·cos(Dec) vs Horizons (eph rows)
    jorbit_d_dec_arcsec        — dDec vs Horizons (eph rows)
    jorbit_d_rho_km            — range diff vs Horizons (eph rows)
    jorbit_time_ms             — wall-clock per row

OD rows are skipped — jorbit's gradient-based orbit-fitting hooks need
a custom likelihood + Jacobian setup per fit and don't fit the
per-row replay model. find_orb covers cross-tool OD comparison.
"""

from __future__ import annotations

import argparse
import json
import math
import statistics
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

try:
    import jax.numpy as jnp
    import numpy as np

    import jorbit

    _HAVE_JORBIT = True
except Exception as e:  # noqa: BLE001
    print(f"warning: jorbit import failed: {e}", file=sys.stderr)
    _HAVE_JORBIT = False


_AU_KM = 149_597_870.700
_MJD_TO_JD = 2_400_000.5


def _angular_sep_arcsec(ra1: float, dec1: float, ra2: float, dec2: float) -> float:
    """Vincenty great-circle separation (radians → arcsec)."""
    cos_d1 = math.cos(dec1)
    cos_d2 = math.cos(dec2)
    sin_d1 = math.sin(dec1)
    sin_d2 = math.sin(dec2)
    dra = ra2 - ra1
    n1 = cos_d2 * math.sin(dra)
    n2 = cos_d1 * sin_d2 - sin_d1 * cos_d2 * math.cos(dra)
    num = math.sqrt(n1 * n1 + n2 * n2)
    den = sin_d1 * sin_d2 + cos_d1 * cos_d2 * math.cos(dra)
    return math.degrees(math.atan2(num, den)) * 3600.0


def _build_particle(
    epoch_mjd_tdb: float, pos_au, vel_au_d, name: str = ""
) -> "jorbit.Particle":
    """Construct a jorbit Particle for a Cartesian SSB-ICRF IC.

    jorbit's `Particle` constructor takes `x` (3D position, AU) and
    `v` (3D velocity, AU/day) jnp arrays plus a `time` argument. Times
    given as a bare jnp array are interpreted as TDB JD (the same scale
    empyrean's MJD TDB is on, just shifted by 2_400_000.5). The
    integration frame is barycentric (DE440 + 16-body asteroid set) —
    matches scott's Standard tier defaults.
    """
    return jorbit.Particle(
        x=jnp.asarray([float(pos_au[0]), float(pos_au[1]), float(pos_au[2])]),
        v=jnp.asarray([float(vel_au_d[0]), float(vel_au_d[1]), float(vel_au_d[2])]),
        time=jnp.asarray(epoch_mjd_tdb + _MJD_TO_JD),
        name=name,
    )


def _propagate_with_jorbit(row: dict) -> dict | None:
    if not _HAVE_JORBIT:
        return None
    ic_pos = row.get("ic_pos_au")
    ic_vel = row.get("ic_vel_au_d")
    if ic_pos is None or ic_vel is None:
        return None

    t0 = time.perf_counter()
    try:
        particle = _build_particle(
            row["epoch_mjd_tdb"], ic_pos, ic_vel, name=row["object"]
        )
        target_jd = jnp.asarray([float(row["t_mjd_tdb"]) + _MJD_TO_JD])
        positions, _velocities = particle.integrate(times=target_jd)
    except Exception as e:  # noqa: BLE001
        print(
            f"  {row['object']} dt={row.get('dt_days', 0):+.0f} prop FAIL: {e}",
            file=sys.stderr,
        )
        return None
    ms = (time.perf_counter() - t0) * 1000.0

    # `integrate` returns positions of shape (n_times, 3) in AU.
    arr = np.asarray(positions)
    if arr.ndim == 2 and arr.shape[1] >= 3:
        pos = [float(arr[-1, 0]), float(arr[-1, 1]), float(arr[-1, 2])]
    elif arr.ndim == 1 and arr.shape[0] >= 3:
        pos = [float(arr[0]), float(arr[1]), float(arr[2])]
    else:
        print(
            f"  {row['object']} dt={row.get('dt_days', 0):+.0f} prop unexpected output shape: {arr.shape}",
            file=sys.stderr,
        )
        return None

    res: dict = {"jorbit_pos_au": pos, "jorbit_time_ms": ms}
    ref = row.get("ref_pos_au")
    if ref is not None:
        d = math.sqrt(sum((pos[i] - ref[i]) ** 2 for i in range(3)))
        res["jorbit_vs_horizons_km"] = d * _AU_KM
    return res


def _ephemeris_with_jorbit(row: dict) -> dict | None:
    if not _HAVE_JORBIT:
        return None
    ic_pos = row.get("ic_pos_au")
    ic_vel = row.get("ic_vel_au_d")
    obs_code = row.get("observer")
    if ic_pos is None or ic_vel is None or not obs_code:
        return None

    t0 = time.perf_counter()
    try:
        particle = _build_particle(
            row["epoch_mjd_tdb"], ic_pos, ic_vel, name=row["object"]
        )
        # `Particle.ephemeris(times, observer)` returns an astropy
        # `SkyCoord` in ICRS, including light-time correction. Times
        # passed as a bare jnp array are interpreted as TDB JD.
        target_jd = jnp.asarray([float(row["t_mjd_tdb"]) + _MJD_TO_JD])
        sky = particle.ephemeris(times=target_jd, observer=str(obs_code))
        # SkyCoord.ra / .dec are astropy Longitude / Latitude objects;
        # `.rad` extracts the radian-scaled float. `.distance.to('au')`
        # gives the geocentric range. Use the first (and only) entry.
        ra_rad = float(sky.ra.rad[0]) if sky.ra.shape else float(sky.ra.rad)
        dec_rad = float(sky.dec.rad[0]) if sky.dec.shape else float(sky.dec.rad)
        rho_obj = sky.distance
        rho_au = float(rho_obj.to("au").value[0]) if rho_obj.shape else float(
            rho_obj.to("au").value
        )
    except Exception as e:  # noqa: BLE001
        print(
            f"  {row['object']} dt={row.get('dt_days', 0):+.0f} eph FAIL: {e}",
            file=sys.stderr,
        )
        return None
    ms = (time.perf_counter() - t0) * 1000.0

    res: dict = {"jorbit_time_ms": ms}
    ref_ra = row.get("ref_ra_rad")
    ref_dec = row.get("ref_dec_rad")
    if ref_ra is not None and ref_dec is not None:
        sep = _angular_sep_arcsec(ra_rad, dec_rad, ref_ra, ref_dec)
        res["jorbit_separation_arcsec"] = sep
        res["jorbit_d_ra_arcsec"] = (
            math.degrees((ra_rad - ref_ra) * math.cos(dec_rad)) * 3600.0
        )
        res["jorbit_d_dec_arcsec"] = math.degrees(dec_rad - ref_dec) * 3600.0
    ref_rho = row.get("ref_rho_au")
    if ref_rho is not None:
        res["jorbit_d_rho_km"] = (rho_au - ref_rho) * _AU_KM
    return res


def _executive_summary(rows: list[dict]) -> str:
    prop = [
        r
        for r in rows
        if r["test_type"] == "propagation"
        and r.get("jorbit_vs_horizons_km") is not None
    ]
    eph = [
        r
        for r in rows
        if r["test_type"] == "ephemeris"
        and r.get("jorbit_separation_arcsec") is not None
    ]

    def km_pct(arr: list[float], p: float) -> str:
        if not arr:
            return "—"
        s = sorted(arr)
        v = s[min(len(s) - 1, int(len(s) * p))]
        if v < 1e-3:
            return f"{v * 1e6:.2f} mm"
        if v < 1.0:
            return f"{v * 1e3:.2f} m"
        if v < 1000.0:
            return f"{v:.2f} km"
        return f"{v:.0f} km"

    def asec_pct(arr: list[float], p: float) -> str:
        if not arr:
            return "—"
        s = sorted(arr)
        v = s[min(len(s) - 1, int(len(s) * p))]
        if abs(v) < 1e-3:
            return f"{v * 1e6:.2f} µas"
        if abs(v) < 1.0:
            return f"{v * 1e3:.2f} mas"
        return f"{v:.2f}″"

    lines = []
    lines.append("\n──── jorbit external-reference summary ─────────────────")
    lines.append(f"  Total rows: {len(rows)}")
    lines.append(f"  Propagation rows compared: {len(prop)}")
    if prop:
        vals = [r["jorbit_vs_horizons_km"] for r in prop]
        lines.append(
            f"    |jorbit - Horizons| p50={km_pct(vals, 0.5)}   p95={km_pct(vals, 0.95)}   max={km_pct(vals, 1.0)}"
        )
        times = [r["jorbit_time_ms"] for r in prop if r.get("jorbit_time_ms")]
        if times:
            lines.append(
                f"    Wall clock          p50={statistics.median(times):.1f} ms   max={max(times):.0f} ms"
            )
    lines.append(f"  Ephemeris rows compared: {len(eph)}")
    if eph:
        sep = [r["jorbit_separation_arcsec"] for r in eph]
        lines.append(
            f"    Angular sep         p50={asec_pct(sep, 0.5)}   p95={asec_pct(sep, 0.95)}   max={asec_pct(sep, 1.0)}"
        )
    if not _HAVE_JORBIT:
        lines.append(
            "  NOTE: jorbit is not installed; all rows skipped. Run `./setup.sh` first."
        )
    lines.append("─" * 56)
    return "\n".join(lines)


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--input", required=True, type=Path, help="validation plan JSON")
    p.add_argument(
        "--output",
        type=Path,
        default=Path("results/validation_jorbit.json"),
        help="output JSON",
    )
    args = p.parse_args()

    plan = json.loads(args.input.read_text())
    if not plan:
        print("input plan empty", file=sys.stderr)
        return 1

    print(
        f"Loaded {len(plan)} plan rows; running through jorbit...", file=sys.stderr
    )

    timestamp = datetime.now(timezone.utc).isoformat()
    out_rows: list[dict] = []
    n_skipped = 0

    for r in plan:
        # Uncertainty axis: skip Jet1 rows. jorbit can produce STMs via
        # JAX autodiff (`jax.jacfwd` over `Particle.integrate`), but
        # cross-tool Jet1 parity needs a separate handshake on the
        # covariance representation — out of scope for the propagation /
        # ephemeris-only pass here.
        if r.get("propagation_uncertainty") == "first_order_with_cov":
            n_skipped += 1
            continue
        new = dict(r)
        new["channel"] = "jorbit"
        new["timestamp"] = timestamp
        tt = r.get("test_type")
        if tt == "propagation":
            update = _propagate_with_jorbit(r)
        elif tt == "ephemeris":
            update = _ephemeris_with_jorbit(r)
        else:
            # OD rows: jorbit's gradient-based fitting needs a custom
            # likelihood per fit (different state representation,
            # observation Jacobian). Cross-tool OD comparison is covered
            # by find_orb. Mark skipped.
            update = None
        if update is None:
            n_skipped += 1
        else:
            new.update(update)
        out_rows.append(new)

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(out_rows, indent=2, default=str))
    print(
        f"Wrote {len(out_rows)} jorbit rows to {args.output} ({n_skipped} skipped)",
        file=sys.stderr,
    )

    print(_executive_summary(out_rows), file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
