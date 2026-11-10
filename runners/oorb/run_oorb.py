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
shells out to the upstream CLI binary built by `setup.sh` at a pinned commit.

Data and model (each tool at its own best practice on the suite's data
products). Everything oorb reads comes from the oorb/ folder of the
third-party tool set (`--data-dir`), linked into a private OORB_DATA
directory: de440.dat (OpenOrb's binary form of DE440), OBSCODE.dat, ET-UT.dat,
TAI-UTC.dat, and the BC430 asteroid files asteroid_indices.txt,
asteroid_ephemeris.txt and asteroid_masses.txt (Baer & Chesley 2017,
"BC430 Asteroid Ephemerides", doi:10.5281/zenodo.3558850, CC-BY-4.0). The
shipped oorb.conf is run with its dynamical model set to n-body and
`perturber.asteroids: T`: planets, Moon, Pluto, relativity, and every BC430
asteroid the indices file lists (all 300), rows run as concurrent oorb
processes (`--jobs`).
An object that is itself a BC430 perturber (its number, the fixture's permID,
is in the indices file: Eros, Lutetia, Nysa, Iris, Vesta, Pallas and Hygiea in
this catalog) would perturb itself, and OpenOrb cannot leave one out of its
force model, so each of its rows is a named unsupported row carrying the reason
in `oorb_unsupported` and is never run: the rule the adam-core and layup runners
follow, and no row runs a different force model.
OpenOrb models no non-gravitational acceleration; rows whose plan carries
A1/A2/A3 say so. OpenOrb works in heliocentric coordinates; the
Sun's barycentric state at each epoch comes from the folder's de440.bsp
through the shared helper `runners/sunstate`, never from the plan.
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
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))  # runners/ -> sunstate
from sunstate.de440_sun import SpkError, SunEphemeris  # noqa: E402

_AU_KM = 149_597_870.700

# Every file oorb opens from OORB_DATA, as the oorb/ folder of the third-party
# tool set names it. de440.bsp in the same folder feeds the Sun-state helper.
OORB_DATA_FILES = (
    "de440.dat",
    "OBSCODE.dat",
    "ET-UT.dat",
    "TAI-UTC.dat",
    "asteroid_indices.txt",
    "asteroid_ephemeris.txt",
    "asteroid_masses.txt",
)
# A BC430 body integrated with the BC430 perturbers on is pulled toward its own
# ephemeris (a 1/r^2 self-singularity: Eros 1.8e8 km from Horizons at p50), and
# OpenOrb's conf switches the asteroids for every body at once. Such an object's
# rows are named unsupported rows, never run, the rule the adam-core and layup
# runners follow (owner ruling 2026-11-10: no per-object switch of force model).
SELF_PERTURBER_UNSUPPORTED = (
    "self-perturber: BC430 includes the object and OpenOrb's n-body force model "
    "cannot leave a body out, so the object would perturb itself"
)
NONGRAV_NOTE = (
    "OpenOrb models no non-gravitational acceleration; the plan's A1/A2/A3 are not applied"
)


def bc430_numbers(data_dir: Path) -> set[int]:
    """The asteroid numbers OpenOrb integrates as perturbers: the active
    (uncommented) lines of the folder's asteroid_indices.txt."""
    nums = set()
    for line in (data_dir / "asteroid_indices.txt").read_text().splitlines():
        tok = line.split()
        if tok and not line.lstrip().startswith("#") and tok[0].isdigit():
            nums.add(int(tok[0]))
    return nums


def fixture_number(fixtures_dir: Path, name: str) -> int | None:
    """The object's asteroid number from its fixture's permID, or None (unnumbered)."""
    psv = fixtures_dir / f"{name.replace('/', '_')}.psv"
    if not psv.is_file():
        return None
    header = None
    for line in psv.read_text().splitlines():
        if line.startswith(("#", "!")) or not line.strip():
            continue
        cols = [c.strip() for c in line.split("|")]
        if header is None:
            header = cols
            if "permID" not in header:
                return None
            continue
        perm = cols[header.index("permID")] if len(cols) > header.index("permID") else ""
        return int(perm) if perm.isdigit() else None
    return None


def self_perturbers(objects, fixtures_dir: Path, data_dir: Path) -> set[str]:
    """The objects that are themselves BC430 perturbers: the fixture's permID is
    an active number of the folder's asteroid_indices.txt."""
    perturbers = bc430_numbers(data_dir)
    return {o for o in objects if fixture_number(fixtures_dir, o) in perturbers}


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


def _heliocentric_ic(row: dict, sun: SunEphemeris) -> tuple | None:
    """Convert the plan's SSB-centered IC to heliocentric for oorb.

    OpenOrb's Cartesian orbit convention is HELIOCENTRIC (center 11 — the
    CAREQ .des variant carries no center override), while the plan's
    `ic_pos_au`/`ic_vel_au_d` are SSB-centered. Feeding SSB coordinates in
    unconverted biases the initial state by the Sun's barycentric offset
    (~1e6 km) and — worse — its ~12 m/s barycentric velocity, a
    semi-major-axis error worth ~1e6 km/yr of along-track drift. The Sun's
    state at the IC epoch comes from the registered DE440.
    """
    ic_pos = row.get("ic_pos_au")
    ic_vel = row.get("ic_vel_au_d")
    if ic_pos is None or ic_vel is None:
        return None
    sun_pos, sun_vel = sun.state(row["epoch_mjd_tdb"] + 2_400_000.5)
    helio_pos = [ic_pos[i] - sun_pos[i] for i in range(3)]
    helio_vel = [ic_vel[i] - sun_vel[i] for i in range(3)]
    return helio_pos, helio_vel


def _propagate(row: dict, oorb_bin: Path, env: dict[str, str], sun: SunEphemeris) -> dict | None:
    helio = _heliocentric_ic(row, sun)
    if helio is None:
        return None
    ic_pos, ic_vel = helio
    sun_t, _ = sun.state(row["t_mjd_tdb"] + 2_400_000.5)

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
    # epoch from the registered DE440.
    helio_eq = _ecl_to_eq((x_e, y_e, z_e))
    pos = [helio_eq[i] + sun_t[i] for i in range(3)]
    res: dict = {"oorb_pos_au": pos, "oorb_time_ms": ms}
    ref = row.get("ref_pos_au")
    if ref is not None:
        d = math.sqrt(sum((pos[i] - ref[i]) ** 2 for i in range(3)))
        res["oorb_vs_horizons_km"] = d * _AU_KM
    return res


def _ephemeris(row: dict, oorb_bin: Path, env: dict[str, str], sun: SunEphemeris) -> dict | None:
    obs_code = row.get("observer")
    if not obs_code:
        return None
    helio = _heliocentric_ic(row, sun)
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


# OpenOrb's configuration, stamped on every row (merge-external carries it to
# the report's Configurations appendix).
OORB_SETTINGS = {
    "commit": "",  # filled from the build (setup.sh's pin) in main()
    "dynamical_model": "n-body (the shipped oorb.conf's 2-body set to n-body), "
    "Bulirsch-Stoer as shipped",
    "perturbers": "planets, Moon, Pluto and every BC430 asteroid the indices file "
    "lists (300; Baer & Chesley 2017)",
    "self_perturbers": "",  # the plan's BC430 bodies, named unsupported; set in main()
    "non_grav": NONGRAV_NOTE,
    "relativity": "on (as shipped)",
    "planetary_ephemeris": "de440.dat from the oorb/ folder",
    "frame": "plan SSB states made heliocentric with the Sun from the oorb/ "
    "folder's de440.bsp, and back",
    "uncertainty": "state only (covariance rows are not replayed)",
}


def write_conf(shipped: Path, conf_dir: Path) -> Path:
    """The shipped oorb.conf with n-body dynamics and the asteroid perturbers on,
    the one force model every row runs. Refuses a conf whose lines it cannot
    find, naming them, rather than running a different model."""
    text = shipped.read_text()
    for key in ("dynamical_model", "perturber.asteroids"):
        if len(re.findall(rf"(?m)^{re.escape(key)}:", text)) != 1:
            raise ValueError(f"{shipped}: expected one '{key}:' line")
    nbody = re.sub(r"(?m)^dynamical_model:.*$", "dynamical_model:      n-body", text)
    full = re.sub(r"(?m)^perturber\.asteroids:.*$", "perturber.asteroids: T", nbody)
    conf_dir.mkdir(parents=True, exist_ok=True)
    (conf_dir / "oorb.conf").write_text(full)
    return conf_dir / "oorb.conf"


def stage_data(data_dir: Path, dest: Path) -> list[str]:
    """Link every file oorb opens from the oorb/ folder into ``dest`` (its
    OORB_DATA). Returns the files the folder lacks (empty: ready)."""
    missing = [n for n in OORB_DATA_FILES if not (data_dir / n).is_file()
               or (data_dir / n).stat().st_size == 0]
    if missing:
        return missing
    dest.mkdir(parents=True, exist_ok=True)
    for n in OORB_DATA_FILES:
        link = dest / n
        if link.is_symlink() or link.exists():
            link.unlink()
        link.symlink_to((data_dir / n).resolve())
    return []


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
    p.add_argument(
        "--fixtures-dir",
        type=Path,
        required=True,
        help="ADES PSV fixtures: each object's permID says whether it is itself a BC430 perturber",
    )
    p.add_argument(
        "--jobs",
        type=int,
        default=os.cpu_count() or 1,
        help="oorb processes run at once (default: the CPU count); results keep plan order",
    )
    p.add_argument(
        "--data-dir",
        type=Path,
        required=True,
        help="the oorb/ folder of the third-party tool set: every file oorb reads, "
        "plus de440.bsp for the Sun's barycentric state",
    )
    args = p.parse_args()

    oorb_bin = args.prefix / "bin" / "oorb"
    oorb_conf = args.prefix / "oorb.conf"
    if not oorb_bin.exists() or not oorb_conf.exists():
        print(
            f"ERROR: oorb binary or conf not found under {args.prefix}. Run ./setup.sh first.",
            file=sys.stderr,
        )
        return 1
    try:
        sun = SunEphemeris(args.data_dir / "de440.bsp")
    except SpkError as e:
        print(f"ERROR: {e}", file=sys.stderr)
        return 1
    work = Path(tempfile.mkdtemp(prefix="oorb_data_"))
    missing = stage_data(args.data_dir, work / "data")
    if missing:
        print(
            f"ERROR: the oorb folder {args.data_dir} lacks {', '.join(missing)}; "
            "oorb would read another file or none.",
            file=sys.stderr,
        )
        return 1
    try:
        conf = write_conf(oorb_conf, work / "conf")
    except ValueError as e:
        print(f"ERROR: {e}", file=sys.stderr)
        return 1
    plan_objects = {r.get("object") for r in json.loads(args.input.read_text()) if r.get("object")}
    unsupported = self_perturbers(plan_objects, args.fixtures_dir, args.data_dir)
    OORB_SETTINGS["self_perturbers"] = (
        f"{', '.join(sorted(unsupported)) or 'none'}: named unsupported (BC430 includes "
        "each; OpenOrb cannot leave a body out of its force model)"
    )
    commit_file = args.prefix / "COMMIT"
    OORB_SETTINGS["commit"] = (
        commit_file.read_text().strip() if commit_file.exists() else "unknown (no COMMIT from setup.sh)"
    )
    env = os.environ.copy()
    env["OORB_DATA"] = str(work / "data")
    env["OORB_CONF"] = str(conf)
    print(f"oorb data: {args.data_dir} -> {work / 'data'}", file=sys.stderr)
    print("oorb dynamics: n-body, planets + Moon + Pluto + BC430 asteroids, relativity on; "
          f"named unsupported (self-perturbers): {', '.join(sorted(unsupported)) or 'none'}",
          file=sys.stderr)

    plan = json.loads(args.input.read_text())
    if not plan:
        print("input plan empty", file=sys.stderr)
        return 1

    print(f"Loaded {len(plan)} plan rows; running through oorb...", file=sys.stderr)

    timestamp = datetime.now(timezone.utc).isoformat()
    source_version = _oorb_source_version(oorb_bin)
    out_rows: list[dict] = []
    n_skipped = 0
    # Rows oorb actually computed something for. Counted explicitly rather
    # than derived from len(out_rows) - n_skipped: the Jet1-axis skip
    # `continue`s WITHOUT appending a row while the compute-failure skip
    # appends a pass-through, so the two are not a single arithmetic
    # relationship.
    n_computed = 0
    # Rows of a self-perturber: emitted with their reason, never run.
    n_unsupported = 0
    jobs: list[tuple] = []

    for r in plan:
        # Uncertainty axis: skip covariance-bearing rows. oorb supports
        # propagating a full 6×6 covariance via `--cov-format` but cross-tool
        # Jet1 parity needs a separate handshake on the covariance
        # representation. Out of scope for this propagation-only pass. The tag
        # is a composite `<method>_<arm>`, so the method is read off the
        # prefix: a `None` tag (OD rows) and every arm of `none` are
        # covariance-free.
        _u = r.get("propagation_uncertainty")
        if _u is not None and not (_u == "none" or _u.startswith("none_")):
            n_skipped += 1
            continue
        new = dict(r)
        new["channel"] = "oorb"
        new["timestamp"] = timestamp
        new["source_version"] = source_version
        new["oorb_version"] = source_version
        new["oorb_settings"] = OORB_SETTINGS
        out_rows.append(new)
        if r.get("object") in unsupported:
            # A named unsupported row: never run, never dropped.
            new["oorb_unsupported"] = SELF_PERTURBER_UNSUPPORTED
            n_unsupported += 1
            continue
        if any(r.get(k) for k in ("ic_a1", "ic_a2", "ic_a3")):
            new["oorb_model_note"] = NONGRAV_NOTE
        jobs.append((r, new))
    if n_unsupported:
        print(f"{n_unsupported} rows named unsupported: {SELF_PERTURBER_UNSUPPORTED}", file=sys.stderr)

    def compute(job: tuple) -> dict | None:
        r, _new = job
        tt = r.get("test_type")
        if tt == "propagation":
            return _propagate(r, oorb_bin, env, sun)
        if tt == "ephemeris":
            return _ephemeris(r, oorb_bin, env, sun)
        return None

    with ThreadPoolExecutor(max_workers=max(1, args.jobs)) as pool:
        updates = list(pool.map(compute, jobs))
    for (_r, new), update in zip(jobs, updates):
        if update is None:
            n_skipped += 1
        else:
            new.update(update)
            n_computed += 1

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
        f"Wrote {len(out_rows)} oorb rows to {args.output} ({n_skipped} skipped, "
        f"{n_unsupported} named unsupported)",
        file=sys.stderr,
    )
    print(_executive_summary(out_rows), file=sys.stderr)

    # A run in which every row skipped computed nothing: a pass-through of
    # untouched plan rows must never look like a successful channel.
    if n_computed == 0:
        print(
            f"ERROR: oorb computed ZERO rows ({n_skipped} skipped, {n_unsupported} "
            "named unsupported).\n"
            "       The output is a pass-through of untouched plan rows; nothing "
            "in it came from OpenOrb.\n"
            "       See the per-row FAIL lines above for the reason.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
