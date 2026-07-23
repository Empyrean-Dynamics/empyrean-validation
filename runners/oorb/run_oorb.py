"""OpenOrb external-reference runner for the empyrean validation suite.

Reads the canonical test plan (`validation_plan.json`) and replays each
propagation row through the OpenOrb (`oorb`) Fortran CLI built by
`setup.sh`. Emits one row per input row with oorb-specific fields
populated. Output JSON is consumed by the empyrean-validation report
renderer alongside ASSIST, find_orb, kete, and jorbit.

Schema mirrors the ValidationResult shape used by every other channel.
oorb-specific fields:
    oorb_pos_au             — propagated Cartesian state at t_mjd_tdb
    oorb_vs_horizons_km     — |oorb_pos - ref_pos| (propagation rows)
    oorb_separation_arcsec  — angular separation vs Horizons (eph rows)
    oorb_d_ra_arcsec        — dRA·cos(Dec) vs Horizons (eph rows)
    oorb_d_dec_arcsec       — dDec vs Horizons (eph rows)
    oorb_d_rho_km           — range diff vs Horizons (eph rows)
    oorb_time_ms            — wall-clock per row

OD rows are skipped — oorb's orbit-fitting workflow (Ranging / LSL) is a
multi-stage pipeline that doesn't fit the per-row replay model. find_orb
covers cross-tool OD comparison.

oorb is GPL-3.0 licensed and never linked into empyrean. The runner
shells out to the upstream CLI binary built by `setup.sh`.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import re
import statistics
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path

_AU_KM = 149_597_870.700

# Obliquity at J2000.0 (IAU 1976), used to rotate oorb's ecliptic-J2000
# Cartesian output back to ICRF / equatorial-J2000 to match empyrean.
# `oorb --task=propagation` writes ecliptic vectors regardless of the
# input frame, so we always have to undo the rotation on the way out.
_J2000_OBL_RAD = math.radians(23.439_291_111_111_11)
_COS_OBL = math.cos(_J2000_OBL_RAD)
_SIN_OBL = math.sin(_J2000_OBL_RAD)


def _ecl_to_eq(v: tuple[float, float, float]) -> list[float]:
    """Rotate a 3-vector from ecliptic-J2000 to equatorial-J2000 (≈ICRF)."""
    x, y, z = v
    return [x, _COS_OBL * y - _SIN_OBL * z, _SIN_OBL * y + _COS_OBL * z]


# TAI−UTC (leap seconds) at the MJD thresholds covering the validation
# epoch range (2011–2041). TT−UTC = (TAI−UTC) + 32.184 s. No leap second
# has been announced since 2017; future epochs assume 37 s — if one is
# ever added, far-future rows pick up a ~1 s timing offset (≈ tens of km
# for fast NEOs) until this table is extended.
_LEAP_TABLE = [(57754, 37.0), (57204, 36.0), (56109, 35.0), (54832, 34.0)]


def _tt_minus_utc_s(mjd: float) -> float:
    for thresh, tai_utc in _LEAP_TABLE:
        if mjd >= thresh:
            return tai_utc + 32.184
    return _LEAP_TABLE[-1][1] + 32.184


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


# ── oorb .des file format ────────────────────────────────────────
# oorb dispatches between two input parsers based on the file
# extension: `.orb` triggers `readOpenOrbOrbitFile` (a fixed-width
# 4-header-line format with column markers like `-0008-` / `-0074-`),
# while `.des` triggers `readDESOrbitFile` (the simpler "Data Exchange
# Standard" format used by MOPS / LSST).
#
# We use .des because the layout is straightforward whitespace-
# separated:
#
#   !!OID FORMAT x y z xdot ydot zdot H t_0 INDEX N_PAR MOID COMPCODE
#   <id> CAREQ <x> <y> <z> <vx> <vy> <vz> <H> <mjd_tt> 1 6 -1.0 OPENORB
#
# `CAREQ` = equatorial Cartesian (matches empyrean's ICRF input),
# `CAR` would be ecliptic. The epoch is interpreted as MJD **TT**, not
# TDB — passing TDB-as-TT introduces a ≤1.6 ms periodic offset which
# is well below the cross-tool agreement we expect (~10⁻¹ km), so we
# accept the difference rather than convert.
_DES_HEADER = (
    "!!OID FORMAT x y z xdot ydot zdot H t_0 INDEX N_PAR MOID COMPCODE\n"
)


def _write_des_file(
    path: Path,
    object_id: str,
    epoch_mjd_tdb: float,
    pos_au,
    vel_au_d,
    h_mag: float = 20.0,
) -> None:
    """Write a single equatorial-Cartesian orbit to an oorb .des file."""
    safe_id = "".join(ch if ch.isalnum() else "_" for ch in object_id)
    line = (
        f"{safe_id} CAREQ "
        f"{pos_au[0]:.16e} {pos_au[1]:.16e} {pos_au[2]:.16e} "
        f"{vel_au_d[0]:.16e} {vel_au_d[1]:.16e} {vel_au_d[2]:.16e} "
        f"{h_mag:.3f} {epoch_mjd_tdb:.10f} 1 6 -1.0 OPENORB\n"
    )
    path.write_text(_DES_HEADER + line)


def _parse_propagated_state(path: Path) -> tuple[float, float, float, float, float, float]:
    """Parse a single Cartesian orbit row out of oorb's propagation output.

    Output layout (4 header lines + 1+ data lines):
        # Number  Ecliptic x  Ecliptic y  Ecliptic z  Ecliptic dx/dt ...
        # ...
        # ...
        # -----0001-----<>--------0039-------- ... markers ...
        <id> <x> <y> <z> <vx> <vy> <vz> <mjd_tt> <H> <G>

    The output is **always ecliptic-J2000 Cartesian** — the caller has to
    rotate to ICRF / equatorial-J2000 before comparing against empyrean.
    """
    for raw in path.read_text().splitlines():
        if not raw or raw.startswith("#"):
            continue
        parts = raw.split()
        # 10 fields: id x y z vx vy vz mjd H G
        if len(parts) < 7:
            continue
        try:
            return (
                float(parts[1]),
                float(parts[2]),
                float(parts[3]),
                float(parts[4]),
                float(parts[5]),
                float(parts[6]),
            )
        except ValueError:
            continue
    raise ValueError(f"no propagated state in {path}")


def _parse_eph_basic(text: str) -> dict:
    """Parse the first row out of an `oorb --task=ephemeris` text dump.

    `oorb --task=ephemeris --separately` emits a fixed-column table;
    the first non-comment row carries the topocentric RA / Dec / Δ
    fields the runner needs. Layout (whitespace-separated):
        designation code MJD ra_deg dec_deg <...> Δ_au <...>
    Column indices vary across oorb releases — we probe by scanning
    for an unambiguous decimal-degree pair (RA, Dec) followed by a
    decimal AU range.
    """
    for raw in text.splitlines():
        if not raw or raw.startswith("#") or raw.lstrip().startswith("Desig"):
            continue
        parts = raw.split()
        if len(parts) < 6:
            continue
        # Fixed layout per the task's own header:
        #   #Designation Code MJD_UTC/UT1 Delta RA Dec dDelta/dt ...
        # (Delta in AU BEFORE RA/Dec in degrees — a positional read; the
        # previous plausibility-probe heuristic silently locked onto Delta
        # as RA.)
        try:
            rho_au = float(parts[3])
            ra_deg = float(parts[4])
            dec_deg = float(parts[5])
        except ValueError:
            continue
        if 0.0 <= ra_deg <= 360.0 and -90.0 <= dec_deg <= 90.0 and rho_au > 0.0:
            return {"ra_deg": ra_deg, "dec_deg": dec_deg, "rho_au": rho_au}
    raise ValueError("no ephemeris row recognized")


def _run_oorb(
    bin_path: Path, args: list[str], env: dict[str, str]
) -> tuple[str, str, int]:
    proc = subprocess.run(
        [str(bin_path), *args],
        env=env,
        check=False,
        text=True,
        capture_output=True,
        timeout=120,
    )
    return proc.stdout, proc.stderr, proc.returncode


def _heliocentric_ic(row: dict, sun_epoch: dict) -> tuple | None:
    """Convert the plan's SSB-centered IC to heliocentric for oorb.

    OpenOrb's Cartesian orbit convention is HELIOCENTRIC (center 11 — the
    CAREQ .des variant carries no center override), while the plan's
    `ic_pos_au`/`ic_vel_au_d` are SSB-centered. Feeding SSB coordinates in
    unconverted biases the initial state by the Sun's barycentric offset
    (~1e6 km) and — worse — its ~12 m/s barycentric velocity, a
    semi-major-axis error worth ~1e6 km/yr of along-track drift.
    """
    sun = sun_epoch.get(row.get("object"))
    ic_pos = row.get("ic_pos_au")
    ic_vel = row.get("ic_vel_au_d")
    if ic_pos is None or ic_vel is None:
        return None
    if sun is None:
        print(
            f"  {row.get('object')}: SKIP — plan carries no Sun SSB state "
            "(ref_sun_pos_au/ref_sun_vel_au_d); regenerate the plan",
            file=sys.stderr,
        )
        return None
    sun_pos, sun_vel = sun
    helio_pos = [ic_pos[i] - sun_pos[i] for i in range(3)]
    helio_vel = [ic_vel[i] - sun_vel[i] for i in range(3)]
    return helio_pos, helio_vel


def _propagate(row: dict, oorb_bin: Path, env: dict[str, str], sun_epoch: dict) -> dict | None:
    helio = _heliocentric_ic(row, sun_epoch)
    if helio is None:
        return None
    ic_pos, ic_vel = helio
    sun_t = row.get("ref_sun_pos_au")
    if sun_t is None:
        print(
            f"  {row.get('object')} dt={row.get('dt_days', 0):+.0f}: SKIP — "
            "row carries no ref_sun_pos_au; regenerate the plan",
            file=sys.stderr,
        )
        return None

    with tempfile.TemporaryDirectory(prefix="oorb_run_") as td:
        in_path = Path(td) / "in.des"
        out_path = Path(td) / "out.des"
        _write_des_file(in_path, row["object"], row["epoch_mjd_tdb"], ic_pos, ic_vel)

        t0 = time.perf_counter()
        out, err, rc = _run_oorb(
            oorb_bin,
            [
                "--task=propagation",
                f"--orb-in={in_path}",
                f"--orb-out={out_path}",
                f"--epoch-mjd-tt={row['t_mjd_tdb']:.10f}",
            ],
            env,
        )
        ms = (time.perf_counter() - t0) * 1000.0
        if rc != 0 or not out_path.exists() or out_path.stat().st_size == 0:
            err_tail = err.strip().splitlines()[-1] if err.strip() else ""
            print(
                f"  {row['object']} dt={row.get('dt_days', 0):+.0f} prop FAIL rc={rc}: {err_tail}",
                file=sys.stderr,
            )
            return None
        try:
            x_e, y_e, z_e, _, _, _ = _parse_propagated_state(out_path)
        except Exception as e:  # noqa: BLE001
            print(
                f"  {row['object']} dt={row.get('dt_days', 0):+.0f} parse FAIL: {e}",
                file=sys.stderr,
            )
            return None

    # Rotate ecliptic-J2000 → equatorial-J2000 to match empyrean's frame,
    # then recenter heliocentric → SSB with the Sun's state at the target
    # epoch (carried on the plan row).
    helio_eq = _ecl_to_eq((x_e, y_e, z_e))
    pos = [helio_eq[i] + sun_t[i] for i in range(3)]
    res: dict = {"oorb_pos_au": pos, "oorb_time_ms": ms}
    ref = row.get("ref_pos_au")
    if ref is not None:
        d = math.sqrt(sum((pos[i] - ref[i]) ** 2 for i in range(3)))
        res["oorb_vs_horizons_km"] = d * _AU_KM
    return res


def _ephemeris(row: dict, oorb_bin: Path, env: dict[str, str], sun_epoch: dict) -> dict | None:
    obs_code = row.get("observer")
    if not obs_code:
        return None
    helio = _heliocentric_ic(row, sun_epoch)
    if helio is None:
        return None
    ic_pos, ic_vel = helio

    with tempfile.TemporaryDirectory(prefix="oorb_eph_") as td:
        # The .des extension matters: oorb picks its input parser from the
        # file extension, and a ".orb" name routes to the fixed-width
        # OpenOrb-format reader, which silently parses zero orbits out of a
        # DES-format file. The ephemeris task takes --start-mjd-utc (its
        # only epoch option — unknown flags like --epoch-mjd-tdb are
        # silently ignored, yielding an empty ephemeris), so convert the
        # plan's TDB epoch with the leap-second table (TDB≈TT to <2 ms).
        in_path = Path(td) / "in.des"
        _write_des_file(in_path, row["object"], row["epoch_mjd_tdb"], ic_pos, ic_vel)
        t_utc = row["t_mjd_tdb"] - _tt_minus_utc_s(row["t_mjd_tdb"]) / 86400.0
        t0 = time.perf_counter()
        out, err, rc = _run_oorb(
            oorb_bin,
            [
                "--task=ephemeris",
                f"--orb-in={in_path}",
                f"--code={obs_code}",
                f"--start-mjd-utc={t_utc:.10f}",
                "--timespan=0",
            ],
            env,
        )
        ms = (time.perf_counter() - t0) * 1000.0
    if rc != 0 or not out:
        print(
            f"  {row['object']} dt={row.get('dt_days', 0):+.0f} eph FAIL rc={rc}",
            file=sys.stderr,
        )
        return None
    try:
        eph = _parse_eph_basic(out)
    except Exception as e:  # noqa: BLE001
        print(
            f"  {row['object']} dt={row.get('dt_days', 0):+.0f} eph parse FAIL: {e}",
            file=sys.stderr,
        )
        return None

    ra_rad = math.radians(eph["ra_deg"])
    dec_rad = math.radians(eph["dec_deg"])
    rho_au = eph["rho_au"]

    res: dict = {"oorb_time_ms": ms}
    ref_ra = row.get("ref_ra_rad")
    ref_dec = row.get("ref_dec_rad")
    if ref_ra is not None and ref_dec is not None:
        sep = _angular_sep_arcsec(ra_rad, dec_rad, ref_ra, ref_dec)
        res["oorb_separation_arcsec"] = sep
        res["oorb_d_ra_arcsec"] = (
            math.degrees((ra_rad - ref_ra) * math.cos(dec_rad)) * 3600.0
        )
        res["oorb_d_dec_arcsec"] = math.degrees(dec_rad - ref_dec) * 3600.0
    ref_rho = row.get("ref_rho_au")
    if ref_rho is not None:
        res["oorb_d_rho_km"] = (rho_au - ref_rho) * _AU_KM
    return res


def _executive_summary(rows: list[dict]) -> str:
    prop = [
        r
        for r in rows
        if r["test_type"] == "propagation"
        and r.get("oorb_vs_horizons_km") is not None
    ]
    eph = [
        r
        for r in rows
        if r["test_type"] == "ephemeris"
        and r.get("oorb_separation_arcsec") is not None
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
    lines.append("\n──── oorb external-reference summary ───────────────────")
    lines.append(f"  Total rows: {len(rows)}")
    lines.append(f"  Propagation rows compared: {len(prop)}")
    if prop:
        vals = [r["oorb_vs_horizons_km"] for r in prop]
        lines.append(
            f"    |oorb - Horizons|   p50={km_pct(vals, 0.5)}   p95={km_pct(vals, 0.95)}   max={km_pct(vals, 1.0)}"
        )
        times = [r["oorb_time_ms"] for r in prop if r.get("oorb_time_ms")]
        if times:
            lines.append(
                f"    Wall clock          p50={statistics.median(times):.1f} ms   max={max(times):.0f} ms"
            )
    lines.append(f"  Ephemeris rows compared: {len(eph)}")
    if eph:
        sep = [r["oorb_separation_arcsec"] for r in eph]
        lines.append(
            f"    Angular sep         p50={asec_pct(sep, 0.5)}   p95={asec_pct(sep, 0.95)}   max={asec_pct(sep, 1.0)}"
        )
    lines.append("─" * 56)
    return "\n".join(lines)


def _oorb_source_version(oorb_bin: Path) -> str:
    """Provenance string stamped on every oorb-channel row.

    Prefer the git commit of the oorb source checkout (``build/oorb``, cloned
    by setup.sh); fall back to the binary's ``--version`` banner when the
    source tree is absent. No-hidden-fallbacks: when neither resolves, stamp
    an explicit ``oorb unknown (<reason>)`` rather than a silent blank.
    """
    build_tree = Path(__file__).parent / "build" / "oorb"
    try:
        proc = subprocess.run(
            ["git", "-C", str(build_tree), "rev-parse", "--short", "HEAD"],
            capture_output=True,
            text=True,
            timeout=10,
        )
        if proc.returncode == 0 and proc.stdout.strip():
            return f"oorb {proc.stdout.strip()}"
    except Exception:  # noqa: BLE001
        pass
    try:
        proc = subprocess.run(
            [str(oorb_bin), "--version"],
            capture_output=True,
            text=True,
            timeout=10,
        )
    except Exception as e:  # noqa: BLE001
        return f"oorb unknown ({e})"
    if proc.returncode == 0:
        banner = (proc.stdout or "").strip().splitlines()
        if banner:
            return f"oorb {banner[0].strip()}"
    reason = (proc.stderr or "").strip().splitlines()
    reason = reason[-1] if reason else f"--version rc={proc.returncode}"
    return f"oorb unknown (no source tree; {reason})"


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--input", required=True, type=Path, help="validation plan JSON")
    p.add_argument(
        "--output",
        type=Path,
        default=Path("results/validation_oorb.json"),
        help="output JSON",
    )
    p.add_argument(
        "--prefix",
        type=Path,
        default=Path(__file__).parent / "install",
        help="oorb install prefix (defaults to setup.sh's output dir)",
    )
    args = p.parse_args()

    oorb_bin = args.prefix / "bin" / "oorb"
    oorb_data = args.prefix / "data"
    oorb_conf = args.prefix / "oorb.conf"
    if not oorb_bin.exists():
        print(
            f"ERROR: oorb binary not found at {oorb_bin}. Run ./setup.sh first.",
            file=sys.stderr,
        )
        return 1

    env = os.environ.copy()
    if oorb_data.exists():
        env["OORB_DATA"] = str(oorb_data)
    if oorb_conf.exists():
        # The stock oorb.conf ships `dynamical_model: 2-body` — no planetary
        # perturbations at all, which is catastrophically wrong over the
        # ±15 yr grid (~10⁴ km/month of missing perturbation signal; 10⁷ km
        # over 15 yr). Force full n-body (the conf's perturber list is
        # already all-planets + Moon + Pluto, relativity on). Asteroid
        # perturbers stay off (BC430 not installed) — OpenOrb's residual vs
        # Horizons therefore retains the km-scale asteroid-perturbation
        # signal, which is honest and documented, not a bug.
        conf_text = oorb_conf.read_text()
        patched, n_sub = re.subn(
            r"(?m)^dynamical_model:\s+2-body\s*$",
            "dynamical_model:      n-body",
            conf_text,
        )
        if n_sub != 1 and not re.search(r"(?m)^dynamical_model:\s+n-body\s*$", conf_text):
            print(
                f"ERROR: could not force n-body dynamics in {oorb_conf} "
                f"(matched {n_sub} 'dynamical_model: 2-body' lines)",
                file=sys.stderr,
            )
            return 1
        conf_dir = Path(tempfile.mkdtemp(prefix="oorb_conf_"))
        nbody_conf = conf_dir / "oorb_nbody.conf"
        nbody_conf.write_text(patched)
        env["OORB_CONF"] = str(nbody_conf)
        print("oorb dynamics: n-body (planets + Moon + Pluto, relativity on; asteroid perturbers off)")

    plan = json.loads(args.input.read_text())
    if not plan:
        print("input plan empty", file=sys.stderr)
        return 1

    print(f"Loaded {len(plan)} plan rows; running through oorb...", file=sys.stderr)

    # Sun SSB state at each object's IC epoch (the dt=0 propagation row).
    sun_epoch: dict = {}
    for r in plan:
        if (
            r.get("test_type") == "propagation"
            and r.get("dt_days") == 0.0
            and r.get("ref_sun_pos_au") is not None
        ):
            sun_epoch[r["object"]] = (r["ref_sun_pos_au"], r["ref_sun_vel_au_d"])

    timestamp = datetime.now(timezone.utc).isoformat()
    source_version = _oorb_source_version(oorb_bin)
    out_rows: list[dict] = []
    n_skipped = 0

    for r in plan:
        # Uncertainty axis: skip Jet1 rows. oorb supports propagating a
        # full 6×6 covariance via `--cov-format` but cross-tool Jet1
        # parity needs a separate handshake on the covariance
        # representation. Out of scope for this propagation-only pass.
        if r.get("propagation_uncertainty") == "first_order_with_cov":
            n_skipped += 1
            continue
        new = dict(r)
        new["channel"] = "oorb"
        new["timestamp"] = timestamp
        new["source_version"] = source_version
        tt = r.get("test_type")
        if tt == "propagation":
            update = _propagate(r, oorb_bin, env, sun_epoch)
        elif tt == "ephemeris":
            update = _ephemeris(r, oorb_bin, env, sun_epoch)
        else:
            update = None
        if update is None:
            n_skipped += 1
        else:
            new.update(update)
        out_rows.append(new)

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(out_rows, indent=2, default=str))
    print(
        f"Wrote {len(out_rows)} oorb rows to {args.output} ({n_skipped} skipped)",
        file=sys.stderr,
    )
    print(_executive_summary(out_rows), file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
