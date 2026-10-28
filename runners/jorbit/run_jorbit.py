"""jorbit external-reference runner for the empyrean validation suite.

Reads the canonical test plan (`validation_plan.json`) and replays each
row through jorbit (Ben Cassese's JAX-based N-body integrator). Emits
one row per replayed plan row with jorbit-specific fields populated.
Output JSON is consumed by the empyrean-validation report renderer
alongside ASSIST, find_orb, kete, and pyoorb.

jorbit is GPL-licensed and never linked into empyrean. This script
runs independently in its own virtual environment.

Runs with no network access. jorbit's two SPICE kernels (DE440 and
SB441-N16) come from the registered kernel set in DATA_DIR, staged into
a runner-private astropy cache under jorbit's own URL keys, with
astropy's internet access off; a missing kernel is a hard stop, never a
download (see `jorbit_offline.py`).

Schema mirrors the ValidationResult shape used by every other channel.
jorbit-specific fields:
    jorbit_pos_au              — propagated Cartesian state at t_mjd_tdb
    jorbit_vs_horizons_km      — |jorbit_pos - ref_pos| (propagation rows)
    jorbit_separation_arcsec   — angular separation vs Horizons (eph rows)
    jorbit_d_ra_arcsec         — dRA·cos(Dec) vs Horizons (eph rows)
    jorbit_d_dec_arcsec        — dDec vs Horizons (eph rows)
    jorbit_d_rho_km            — range diff vs Horizons (eph rows)
    jorbit_time_ms             — wall-clock per row

Ephemeris rows run for the geocentric observer (500) only, with the
observer at jorbit's own DE440 geocentre. jorbit's only observer model
for a ground site is a live JPL Horizons query, which the suite's seal
forbids and the pinned inputs snapshot cannot stand in for, so those
rows are not attempted: each object gets ONE explicit row instead,
carrying `jorbit_not_attempted` (the reason),
`jorbit_not_attempted_observers` and `jorbit_not_attempted_rows`.

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
from importlib.metadata import PackageNotFoundError, version
from pathlib import Path

# Bound by _load_jorbit_offline(); importing jorbit any earlier would run its
# import-time initializer, which downloads kernels when they are not cached.
jnp = None
np = None
jorbit = None

_AU_KM = 149_597_870.700
_MJD_TO_JD = 2_400_000.5

RUNNER_DIR = Path(__file__).resolve().parent
# Runner-private astropy download cache holding the registered set's kernels
# under jorbit's URL keys (gitignored; restaged and byte-verified every run).
ASTROPY_CACHE_DIR = RUNNER_DIR / ".astropy-cache"
# The suite's documented default data directory (the Makefile's DATA_DIR).
DEFAULT_DATA_DIR = Path.home() / ".empyrean" / "data"

GEOCENTER = "500"
SITE_NOT_ATTEMPTED_REASON = (
    "jorbit's only observer model for a ground site is a live JPL Horizons query "
    "(jorbit.utils.horizons.get_observer_positions); the suite runs sealed from JPL "
    "and the pinned inputs snapshot carries no observer position vectors. The "
    "geocentric observer (500) runs from jorbit's own DE440 Earth."
)


def _dist_version(name: str) -> str:
    """Installed distribution version, or an explicit ``unknown (<reason>)``."""
    try:
        return version(name)
    except PackageNotFoundError:
        return "unknown (distribution not found)"
    except Exception as e:  # noqa: BLE001
        return f"unknown ({e})"


def _jorbit_source_version() -> str:
    """Provenance string stamped on every jorbit-channel row.

    Carries the installed jorbit, JAX and jaxlib versions (JAX changes the
    numbers jorbit produces as much as jorbit does) and the kernel case, which
    is always DE440 + SB441-N16 from the registered set. No-hidden-fallbacks:
    a version that can't be read is stamped ``unknown (<reason>)``, never left
    blank.
    """
    return (
        f"jorbit {_dist_version('jorbit')} (jax {_dist_version('jax')}, "
        f"jaxlib {_dist_version('jaxlib')}; DE440+SB441-N16)"
    )


def _load_jorbit_offline(data_dir: Path) -> None:
    """Stage the registered kernels, cut astropy off the network, import jorbit.

    Exits with the reason when jorbit cannot run from local files alone.
    """
    global jnp, np, jorbit
    sys.path.insert(0, str(RUNNER_DIR))
    from jorbit_offline import OfflineSetupError, import_jorbit_offline

    print(f"jorbit: kernels from the registered set in {data_dir}", file=sys.stderr)
    try:
        jorbit, lines, suppressed = import_jorbit_offline(data_dir, ASTROPY_CACHE_DIR)
    except OfflineSetupError as e:
        sys.exit(f"ERROR: jorbit cannot run offline: {e}")
    for line in lines:
        print(f"  {line}", file=sys.stderr)
    if suppressed:
        names = ", ".join(sorted({u.rsplit("/", 1)[-1] for u in suppressed}))
        print(
            f"  jorbit's import-time download of its mpchecker bundle ({names}) was "
            "suppressed: no code path this runner uses reads it, and astropy's "
            "internet access is off, so any read of it fails loudly",
            file=sys.stderr,
        )
    import jax.numpy as _jnp
    import numpy as _np

    jnp, np = _jnp, _np


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
    epoch_mjd_tdb: float, pos_au, vel_au_d, name: str = "", gravity: str = "default solar system"
) -> "jorbit.Particle":
    """Construct a jorbit Particle for a Cartesian SSB-ICRF IC.

    jorbit's `Particle` constructor takes `x` (3D position, AU) and
    `v` (3D velocity, AU/day) jnp arrays plus a `time` argument. Times
    given as a bare jnp array are interpreted as TDB JD (the same scale
    empyrean's MJD TDB is on, just shifted by 2_400_000.5). The
    integration frame is barycentric (DE440 + 16-body asteroid set) —
    matches empyrean's Standard tier defaults.
    """
    return jorbit.Particle(
        x=jnp.asarray([float(pos_au[0]), float(pos_au[1]), float(pos_au[2])]),
        v=jnp.asarray([float(vel_au_d[0]), float(vel_au_d[1]), float(vel_au_d[2])]),
        time=jnp.asarray(epoch_mjd_tdb + _MJD_TO_JD),
        name=name,
        gravity=gravity,
    )


def _row_gravity(row: dict) -> str:
    """Pick jorbit's gravity preset for one plan row.

    The default ("default solar system") includes the SB441-N16 asteroid
    set — which contains Iris/Vesta/Pallas/Hygiea, four objects this suite
    propagates AS test particles. jorbit has no per-body exclusion, so the
    particle would sit inside its own 1/r² singularity; use the PPN
    planets-only preset ("gr planets") for them, stated loudly.
    """
    if row.get("population") == "Self-Perturber":
        print(
            f"  {row.get('object')}: jorbit runs 'gr planets' (planets-only) — "
            "the default asteroid set would include the object itself",
            file=sys.stderr,
        )
        return "gr planets"
    if (row.get("ic_a1") or row.get("ic_a2") or row.get("ic_a3")) and row.get("dt_days") == 0.0:
        print(
            f"  {row.get('object')}: NOTE — jorbit models gravity only; the "
            "non-grav (A1/A2/A3) signal remains in its residual",
            file=sys.stderr,
        )
    return "default solar system"


class _Geocenter:
    """Barycentric position of observer 500 (the geocentre) from jorbit's DE440.

    jorbit's string-observer path asks Horizons for the Earth-Moon barycentre
    from the SSB and from the observer and differences them; for the geocentre
    that is the Earth's barycentric position, which jorbit's own `Ephemeris`
    reads from the same DE440 it integrates against. One ephemeris spans every
    geocentric row of the plan.
    """

    def __init__(self, t_min_mjd_tdb: float, t_max_mjd_tdb: float) -> None:
        from astropy.time import Time

        self._eph = jorbit.Ephemeris(
            ssos="default planets",
            earliest_time=Time(t_min_mjd_tdb + _MJD_TO_JD - 1.0, format="jd", scale="tdb"),
            latest_time=Time(t_max_mjd_tdb + _MJD_TO_JD + 1.0, format="jd", scale="tdb"),
            de_ephemeris_version="440",
        )

    def position_au(self, times) -> jnp.ndarray:
        """(N, 3) barycentric ICRF positions in AU at an astropy Time array."""
        return jnp.asarray(self._eph.state(times)["earth"]["x"].to_value("au"))


class SiteRowsNotAttempted:
    """Ground-site ephemeris rows jorbit cannot replay offline, grouped per object.

    The rows are not replayed and not emitted one by one; each object gets one
    explicit row naming the reason, the observers and how many plan rows it
    stands for.
    """

    def __init__(self, reason: str) -> None:
        self.reason = reason
        self._by_object: dict[str, dict] = {}

    def add(self, row: dict) -> None:
        entry = self._by_object.setdefault(
            row["object"], {"first": row, "observers": set(), "rows": 0}
        )
        entry["observers"].add(row.get("observer"))
        entry["rows"] += 1

    @property
    def n_rows(self) -> int:
        return sum(e["rows"] for e in self._by_object.values())

    def __len__(self) -> int:
        return len(self._by_object)

    def rows(self, timestamp: str, source_version: str) -> list[dict]:
        out = []
        for name, e in self._by_object.items():
            first = e["first"]
            out.append(
                {
                    "object": name,
                    "population": first.get("population"),
                    "epoch_mjd_tdb": first.get("epoch_mjd_tdb"),
                    "test_type": "ephemeris",
                    "propagation_uncertainty": first.get("propagation_uncertainty"),
                    "channel": "jorbit",
                    "timestamp": timestamp,
                    "source_version": source_version,
                    "jorbit_not_attempted": self.reason,
                    "jorbit_not_attempted_observers": sorted(e["observers"]),
                    "jorbit_not_attempted_rows": e["rows"],
                }
            )
        return out

    def report_lines(self) -> list[str]:
        return [
            f"  {name}: ephemeris from {','.join(sorted(e['observers']))} "
            f"({e['rows']} rows) not attempted — {self.reason}"
            for name, e in self._by_object.items()
        ]


def _propagate_with_jorbit(row: dict) -> dict | None:
    ic_pos = row.get("ic_pos_au")
    ic_vel = row.get("ic_vel_au_d")
    if ic_pos is None or ic_vel is None:
        return None

    t0 = time.perf_counter()
    try:
        particle = _build_particle(
            row["epoch_mjd_tdb"], ic_pos, ic_vel, name=row["object"],
            gravity=_row_gravity(row),
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


def _ephemeris_with_jorbit(row: dict, geocenter: _Geocenter) -> dict | None:
    """Geocentric (observer 500) astrometric RA/Dec through jorbit."""
    ic_pos = row.get("ic_pos_au")
    ic_vel = row.get("ic_vel_au_d")
    if ic_pos is None or ic_vel is None or row.get("observer") != GEOCENTER:
        return None

    t0 = time.perf_counter()
    try:
        particle = _build_particle(
            row["epoch_mjd_tdb"], ic_pos, ic_vel, name=row["object"],
            gravity=_row_gravity(row),
        )
        # `Particle.ephemeris(times, observer)` returns an astropy
        # `SkyCoord` in ICRS, including light-time correction. The
        # docstring accepts a bare jnp array (TDB JD), but the
        # implementation dereferences `.jd` — pass an astropy Time
        # explicitly (upstream array path is broken). The observer is
        # handed over as a barycentric position vector, so jorbit's
        # string-observer path (a live Horizons query) is never taken.
        from astropy.time import Time
        target_t = Time([float(row["t_mjd_tdb"]) + _MJD_TO_JD], format="jd", scale="tdb")
        obs_pos = geocenter.position_au(target_t)
        sky = particle.ephemeris(times=target_t, observer=obs_pos)
        # SkyCoord.ra / .dec are astropy Longitude / Latitude objects;
        # `.rad` extracts the radian-scaled float. Use the first (and only) entry.
        ra_rad = float(sky.ra.rad[0]) if sky.ra.shape else float(sky.ra.rad)
        dec_rad = float(sky.dec.rad[0]) if sky.dec.shape else float(sky.dec.rad)
        # jorbit's ephemeris SkyCoord carries RA/Dec only — its distance is a
        # dimensionless placeholder, and .to("au") on it raises. Range is
        # simply not available from this API; emit angles only.
        rho_au = None
        try:
            if sky.distance.unit.physical_type == "length":
                rho_obj = sky.distance
                rho_au = float(rho_obj.to("au").value[0]) if rho_obj.shape else float(
                    rho_obj.to("au").value
                )
        except Exception:
            rho_au = None
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
    if ref_rho is not None and rho_au is not None:
        res["jorbit_d_rho_km"] = (rho_au - ref_rho) * _AU_KM
    return res


def _executive_summary(rows: list[dict], not_attempted: SiteRowsNotAttempted) -> str:
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
    lines.append(f"  Ephemeris rows compared (geocentric): {len(eph)}")
    if eph:
        sep = [r["jorbit_separation_arcsec"] for r in eph]
        lines.append(
            f"    Angular sep         p50={asec_pct(sep, 0.5)}   p95={asec_pct(sep, 0.95)}   max={asec_pct(sep, 1.0)}"
        )
    if len(not_attempted):
        lines.append(
            f"  Ephemeris rows NOT ATTEMPTED (ground-site observers): {not_attempted.n_rows} "
            f"across {len(not_attempted)} objects, one explicit row each — {not_attempted.reason}"
        )
    lines.append("─" * 56)
    return "\n".join(lines)


def _check(data_dir: Path) -> int:
    """Setup smoke test: stage the kernels, import jorbit offline, read DE440."""
    _load_jorbit_offline(data_dir)
    from astropy.time import Time

    j2000 = 51544.5
    earth = _Geocenter(j2000, j2000).position_au(
        Time([j2000 + _MJD_TO_JD], format="jd", scale="tdb")
    )
    r = float(np.linalg.norm(np.asarray(earth)[0]))
    print(
        f"{_jorbit_source_version()} offline OK: geocentre |r| = {r:.6f} au at J2000 "
        "from the staged DE440",
        file=sys.stderr,
    )
    return 0


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--input", type=Path, help="validation plan JSON")
    p.add_argument(
        "--output",
        type=Path,
        default=Path("results/validation_jorbit.json"),
        help="output JSON",
    )
    p.add_argument(
        "--data-dir",
        type=Path,
        default=DEFAULT_DATA_DIR,
        help="registered kernel set (the suite's DATA_DIR); jorbit's DE440 and "
        "SB441-N16 are read from here, never downloaded",
    )
    p.add_argument(
        "--check",
        action="store_true",
        help="stage the kernels, import jorbit offline and read DE440, then exit",
    )
    args = p.parse_args()

    if args.check:
        return _check(args.data_dir)
    if args.input is None:
        p.error("--input is required (unless --check)")

    plan = json.loads(args.input.read_text())
    if not plan:
        print("input plan empty", file=sys.stderr)
        return 1

    _load_jorbit_offline(args.data_dir)
    print(
        f"Loaded {len(plan)} plan rows; running through jorbit...", file=sys.stderr
    )

    timestamp = datetime.now(timezone.utc).isoformat()
    source_version = _jorbit_source_version()
    out_rows: list[dict] = []
    n_skipped = 0
    not_attempted = SiteRowsNotAttempted(SITE_NOT_ATTEMPTED_REASON)
    geocenter: _Geocenter | None = None

    def replayed(r: dict) -> bool:
        # Uncertainty axis: skip covariance-bearing rows. jorbit can produce
        # STMs via JAX autodiff (`jax.jacfwd` over `Particle.integrate`), but
        # cross-tool Jet1 parity needs a separate handshake on the covariance
        # representation — out of scope for the propagation / ephemeris-only
        # pass here. The tag is a composite `<method>_<arm>`, so the method is
        # read off the prefix: a `None` tag (OD rows) and every arm of `none`
        # are covariance-free.
        u = r.get("propagation_uncertainty")
        return u is None or u == "none" or u.startswith("none_")

    geo_times = [
        float(r["t_mjd_tdb"])
        for r in plan
        if replayed(r) and r.get("test_type") == "ephemeris"
        and r.get("observer") == GEOCENTER and r.get("t_mjd_tdb") is not None
    ]
    if geo_times:
        geocenter = _Geocenter(min(geo_times), max(geo_times))

    for r in plan:
        if not replayed(r):
            n_skipped += 1
            continue
        tt = r.get("test_type")
        if tt == "ephemeris" and r.get("observer") != GEOCENTER:
            not_attempted.add(r)
            continue
        new = dict(r)
        new["channel"] = "jorbit"
        new["timestamp"] = timestamp
        new["source_version"] = source_version
        if tt == "propagation":
            update = _propagate_with_jorbit(r)
        elif tt == "ephemeris":
            update = _ephemeris_with_jorbit(r, geocenter)
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

    for line in not_attempted.report_lines():
        print(line, file=sys.stderr)
    out_rows.extend(not_attempted.rows(timestamp, source_version))

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
        f"Wrote {len(out_rows)} jorbit rows to {args.output} ({n_skipped} skipped, "
        f"{not_attempted.n_rows} ground-site ephemeris rows not attempted as "
        f"{len(not_attempted)} explicit rows)",
        file=sys.stderr,
    )

    print(_executive_summary(out_rows, not_attempted), file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
