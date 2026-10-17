"""Shared helpers for the adam-core external-reference runner (one channel).

ONE channel, tool id `adamcore`: adam-core with its OWN shipped defaults —
run_od(models="default") = SigmaFill(v2_sigma_fill) -> EFCC18 debias ->
EmpiricalCovariance(v2_full, add) -> NightBatch(cap 4), on the Asteroid
Institute's observatory-uncertainty tables. The tables come from the PRIVATE
`adam-observatory-uncertainties` package, so this runner is LOCAL-ONLY (CI skips
it; results are not published). No public/alternate weighting channel exists.

Shared by the three entry points (run_adamcore.py, run_adamcore_walk.py, tests/).

Plan conventions (shared with every other runner, see runners/kete):
    ic_pos_au / ic_vel_au_d   ICRF (= J2000 equatorial), SSB-centred (NAIF 0), au / au d^-1
    epoch_mjd_tdb             initial-condition epoch, MJD TDB
    t_mjd_tdb                 target epoch, MJD TDB
    ref_pos_au                Horizons Cartesian reference at t (propagation rows)
    ref_ra_rad / ref_dec_rad  Horizons astrometric reference (ephemeris rows)
    ref_rho_au                Horizons range (ephemeris rows)
    observer                  MPC observatory code (ephemeris rows)
    ic_a1..ic_a3, ic_g_*      Marsden non-grav A-terms + g(r) (alpha, r0, m, n, k), ic_non_grav_dt

No-hidden-fallbacks discipline (brief + lang): every filled sigma, dropped
space-based observation, refused/timed-out fit and unsupported non-grav term is
COUNTED and carried on the output row with its reason — never silently dropped
or substituted.
"""

from __future__ import annotations

import json
import math
import os
from importlib import metadata
from pathlib import Path

AU_KM = 149_597_870.7
C_AU_PER_DAY = 173.144632674240
MJD_TO_JD = 2_400_000.5

HERE = Path(__file__).resolve().parent
VERSIONS_JSON = HERE / ".venv" / "adamcore-versions.json"
EFCC18_DIR = Path(os.environ.get("ADAM_CORE_EFCC18_DIR", HERE / ".cache" / "efcc18"))
EFCC18_BIAS_DAT = EFCC18_DIR / "bias.dat"
# Point adam_core's own EFCC18 resolver at our runner-local cache, so the
# `models="default"` path (which builds EFCC18DebiasModel internally) finds the
# bias table setup.sh fetched. Inherited by the per-object OD subprocess.
os.environ["ADAM_CORE_EFCC18_DIR"] = str(EFCC18_DIR)
if EFCC18_BIAS_DAT.exists():
    os.environ.setdefault("ADAM_CORE_EFCC18_BIAS_DAT", str(EFCC18_BIAS_DAT))


# ── provenance ───────────────────────────────────────────────


def source_version() -> str:
    """Compact provenance string stamped on every row's `source_version`."""
    try:
        v = metadata.version("adam-core")
    except metadata.PackageNotFoundError:
        return "adamcore unknown (adam-core not installed)"
    try:
        va = metadata.version("adam-assist")
    except metadata.PackageNotFoundError:
        va = "absent"
    return f"adam-core {v} + adam-assist {va}"


def load_versions() -> dict:
    """The full versions stamp written by setup.sh, or a marker dict if absent."""
    if VERSIONS_JSON.exists():
        try:
            return json.loads(VERSIONS_JSON.read_text())
        except Exception as e:  # noqa: BLE001
            return {"error": f"versions.json unreadable: {e}"}
    return {"error": f"versions.json absent at {VERSIONS_JSON}"}


# ── angles / numerics ────────────────────────────────────────


def resolve_fixture(fixtures_dir: Path, name: str, mpc_designation: str | None = None):
    """The three-candidate stem convention every runner uses (see layup's
    _resolve_fixture): the object name, the name with '/' -> '_' (comets/ISOs like
    '103P/Hartley 2' -> '103P_Hartley 2.psv'), then the MPC designation. Returns the
    Path or None, and the candidate list tried (so a miss is a loud, specific error)."""
    cands = [fixtures_dir / f"{name}.psv", fixtures_dir / f"{name.replace('/', '_')}.psv"]
    if mpc_designation:
        cands.append(fixtures_dir / f"{mpc_designation}.psv")
    for c in cands:
        if c.exists():
            return c, [str(x) for x in cands]
    return None, [str(x) for x in cands]


def angular_sep_arcsec(ra1: float, dec1: float, ra2: float, dec2: float) -> float:
    """Vincenty great-circle separation (radians in, arcsec out)."""
    cos_d1, cos_d2 = math.cos(dec1), math.cos(dec2)
    sin_d1, sin_d2 = math.sin(dec1), math.sin(dec2)
    dra = ra2 - ra1
    n1 = cos_d2 * math.sin(dra)
    n2 = cos_d1 * sin_d2 - sin_d1 * cos_d2 * math.cos(dra)
    num = math.sqrt(n1 * n1 + n2 * n2)
    den = sin_d1 * sin_d2 + cos_d1 * cos_d2 * math.cos(dra)
    return math.degrees(math.atan2(num, den)) * 3600.0


def finite(*vals) -> bool:
    return all(isinstance(v, (int, float)) and math.isfinite(v) for v in vals)


# ── SigmaFill table (for σ-source fill-level classification) ─


def data_sigma_table():
    """The SigmaFill table the default model stack fills from: the Institute's
    proprietary ``v2_sigma_fill`` (from the adam-observatory-uncertainties package).
    Used only to CLASSIFY which level supplied a filled sigma (station / catalog /
    global) — never to read or emit a sigma value."""
    from adam_core.orbit_determination import load_sigma_table

    return load_sigma_table("v2_sigma_fill")


def sigma_table_row_sets(table):
    """(station_codes, catalog_cats, station_cat_pairs, has_global_row) of a sigma table,
    for classifying which level a SigmaFill match comes from."""
    codes = table["obs_code"].to_pylist()
    cats = table["astcat"].to_pylist()
    station: set = set()
    catalog: set = set()
    station_cat: set = set()
    has_global = False
    for c, a in zip(codes, cats):
        if c is not None and a is not None:
            station_cat.add((c, a))
        elif c is not None:
            station.add(c)
        elif a is not None:
            catalog.add(a)
        else:
            has_global = True
    return station, catalog, station_cat, has_global


def classify_fill_level(code, cat, sets) -> str:
    """Which table level supplies a missing sigma for (station, catalog), in the Rust
    lookup's precedence: (station,catalog) > station > catalog > global > fallback."""
    station, catalog, station_cat, has_global = sets
    if (code, cat) in station_cat:
        return "station+catalog"
    if code in station:
        return "station"
    if cat in catalog:
        return "catalog"
    if has_global:
        return "global"
    return "model_fallback_0.75"


# ── self-perturbers (planets-only force model) ───────────────

# These objects are IN ASSIST's SB441-n16 massive-asteroid perturber set, so
# propagating them with the full force model makes the object perturb ITSELF (a
# 1/r^2 self-singularity: ~1e9 km error at 1 yr). ASSIST has no per-body
# exclusion, so — exactly as the suite's ASSIST runner does — we run these
# planets-only. The adam-assist route (no patching): construct the propagator
# with a deliberately non-existent asteroids_path, which makes ASSIST print
# "(ASSIST) The JPL asteroid ephemeris file has not been found. Asteroid forces
# have been disabled." and integrate with planets + Sun + Moon only. NOTE:
# asteroids_path=None does NOT disable (it resolves the default SB441-n16).
SELF_PERTURBERS = {"Iris", "Vesta", "Pallas", "Hygiea"}
_FORCE_MODEL_SELF = "planets-only (self-perturber; SB441-n16 includes the object)"
_FORCE_MODEL_FULL = "planets+SB441-n16"
_NONEXISTENT_ASTEROIDS = "/nonexistent-sb441-self-perturber.bsp"


def is_self_perturber(obj) -> bool:
    return str(obj) in SELF_PERTURBERS


def force_model_label(obj) -> str:
    return _FORCE_MODEL_SELF if is_self_perturber(obj) else _FORCE_MODEL_FULL


def default_propagator():
    from adam_assist import ASSISTPropagator

    return ASSISTPropagator()


def planets_only_propagator():
    """An ASSISTPropagator with asteroid forces DISABLED (self-perturber route).

    Verified empirically (2026-10-16) that adam-assist @ce52ce1e holds its ASSIST
    ephemeris per-instance (NOT process-shared), so a planets-only and a default
    instance coexist in one process and each keeps its own force model.
    """
    from adam_assist import ASSISTPropagator

    return ASSISTPropagator(asteroids_path=_NONEXISTENT_ASTEROIDS)


def propagator_for(obj, default_prop, planets_only_prop):
    """Pick the force model for an object: planets-only for self-perturbers."""
    return planets_only_prop if is_self_perturber(obj) else default_prop


_PLANETS_ONLY_DIVERGENCE_KM = None


def verify_planets_only_active(planets_only_prop) -> float:
    """Assert asteroid forces are OFF by propagating a synthetic main-belt orbit over
    a 10-yr arc under the planets-only propagator AND a fresh default one, and
    requiring they diverge (asteroid forces toggled). Measured ~19 km for this probe;
    if planets-only silently reverted to the full force model the two would be
    bit-identical, so a > 0.1 km divergence is the gate. Memoised per process.

    (The ASSIST 'Asteroid forces have been disabled' stderr line is written by the C
    library past a Python fd-2 redirect, so it cannot be captured reliably; this
    divergence check is the robust equivalent, and it also confirms the two instances
    carry different force models in the same process.)"""
    global _PLANETS_ONLY_DIVERGENCE_KM
    if _PLANETS_ONLY_DIVERGENCE_KM is not None:
        return _PLANETS_ONLY_DIVERGENCE_KM
    import numpy as np

    from adam_core.coordinates import CartesianCoordinates, Origin
    from adam_core.orbits import Orbits
    from adam_core.time import Timestamp

    def probe(prop):
        o = Orbits.from_kwargs(
            orbit_id=["_probe"], object_id=["_probe"],
            coordinates=CartesianCoordinates.from_kwargs(
                x=[2.3], y=[0.0], z=[0.0], vx=[0.0], vy=[0.0115], vz=[0.0],
                time=Timestamp.from_mjd([59000.0], scale="tdb"),
                origin=Origin.from_kwargs(code=["SOLAR_SYSTEM_BARYCENTER"]), frame="equatorial",
            ),
        )
        r = prop.propagate_orbits(o, Timestamp.from_mjd([59000.0 + 3650.0], scale="tdb")).coordinates
        return np.array([r.x[0].as_py(), r.y[0].as_py(), r.z[0].as_py()])

    diff_km = float(np.linalg.norm(probe(planets_only_prop) - probe(default_propagator())) * AU_KM)
    if diff_km < 0.1:
        raise RuntimeError(
            "planets-only propagator did NOT disable asteroid forces: its 10-yr probe "
            f"differs from the default by only {diff_km:.3e} km (expected ~19 km). "
            "Self-perturber rows would be self-singular."
        )
    _PLANETS_ONLY_DIVERGENCE_KM = diff_km
    return diff_km


# ── adam_core state construction (STABLE API) ────────────────

# Standard Marsden asteroid g(r) = 1/r^2 (alpha=1, r0=1, m=2, n=0, k=0). Used
# when a row carries A-terms but no explicit g(r) — this is the DEFINITION of the
# standard non-grav law, not a substitution; comet rows carry their own g(r).
_STD_GR = {"ALN": 1.0, "R0": 1.0, "NM": 2.0, "NN": 0.0, "NK": 0.0}


def nongrav_for(row: dict):
    """(NonGravitationalParameters | None, unsupported_reason | None) for a plan row.

    adam_core's `NonGravitationalParameters` carries A1/A2/A3 and the g(r) terms
    (ALN, R0, NM, NN, NK) but has NO Marsden time-delay (DT) term, so a row with a
    non-zero `ic_non_grav_dt` is modelled WITHOUT its DT — surfaced (never hidden)
    as the unsupported reason "nongrav dt".
    """
    a1 = row.get("ic_a1") or 0.0
    a2 = row.get("ic_a2") or 0.0
    a3 = row.get("ic_a3") or 0.0
    if not (a1 or a2 or a3):
        return None, None
    from adam_core.orbits import NonGravitationalParameters

    gr = {
        "ALN": row.get("ic_g_alpha"),
        "R0": row.get("ic_g_r0"),
        "NM": row.get("ic_g_m"),
        "NN": row.get("ic_g_n"),
        "NK": row.get("ic_g_k"),
    }
    if all(v is None for v in gr.values()):
        gr = dict(_STD_GR)
    ng = NonGravitationalParameters.from_kwargs(
        A1=[a1], A2=[a2], A3=[a3],
        ALN=[gr["ALN"]], R0=[gr["R0"]], NM=[gr["NM"]], NN=[gr["NN"]], NK=[gr["NK"]],
    )
    dt = row.get("ic_non_grav_dt")
    reason = "nongrav dt" if dt not in (None, 0, 0.0) else None
    return ng, reason


def plan_orbit(row: dict):
    """Build an adam_core Orbits from a plan IC. Returns (orbits, unsupported_reason).

    SSB-centred ICRF Cartesian (au / au d^-1) at epoch_mjd_tdb -> CartesianCoordinates
    with origin SOLAR_SYSTEM_BARYCENTER, frame equatorial, scale tdb. Marsden
    non-grav A1/A2/A3 + g(r) are attached when present (verified bit-for-bit
    against the ASSIST runner's assist_asteroid_institute arm).
    """
    from adam_core.coordinates import CartesianCoordinates, Origin
    from adam_core.orbits import Orbits
    from adam_core.time import Timestamp

    pos = row["ic_pos_au"]
    vel = row["ic_vel_au_d"]
    time = Timestamp.from_mjd([row["epoch_mjd_tdb"]], scale="tdb")
    coords = CartesianCoordinates.from_kwargs(
        x=[pos[0]], y=[pos[1]], z=[pos[2]],
        vx=[vel[0]], vy=[vel[1]], vz=[vel[2]],
        time=time, origin=Origin.from_kwargs(code=["SOLAR_SYSTEM_BARYCENTER"]),
        frame="equatorial",
    )
    name = str(row.get("object", "empyrean_test"))
    kw = dict(orbit_id=[name], object_id=[name], coordinates=coords)
    ng, reason = nongrav_for(row)
    if ng is not None:
        kw["non_gravitational_parameters"] = ng
    return Orbits.from_kwargs(**kw), reason


def to_bary_equatorial(coords):
    """Transform a CartesianCoordinates to SSB-centred equatorial (for output state/cov)."""
    from adam_core.coordinates import CartesianCoordinates, OriginCodes, transform_coordinates

    return transform_coordinates(
        coords,
        representation_out=CartesianCoordinates,
        frame_out="equatorial",
        origin_out=OriginCodes.SOLAR_SYSTEM_BARYCENTER,
    )


def orbit_to_ecliptic(orbit):
    """Return a copy of an Orbits with ecliptic-frame Cartesian coordinates.

    adam_core's typed ephemeris generation requires an ecliptic Cartesian orbit;
    the plan IC (and our propagation path) is equatorial, so the ephemeris path
    rotates it here. Non-grav parameters and ids ride along unchanged.
    """
    from adam_core.coordinates import CartesianCoordinates, transform_coordinates
    from adam_core.orbits import Orbits

    ecl = transform_coordinates(
        orbit.coordinates, representation_out=CartesianCoordinates, frame_out="ecliptic"
    )
    kw = dict(orbit_id=orbit.orbit_id, object_id=orbit.object_id, coordinates=ecl)
    ng = orbit.non_gravitational_parameters
    if ng is not None and len(ng) and not ng.A1.null_count == len(ng):
        kw["non_gravitational_parameters"] = ng
    return Orbits.from_kwargs(**kw)


# ── writing JSON (no bare NaN) ───────────────────────────────


def dump_rows(rows: list[dict], output: Path) -> None:
    """Write a JSON array, refusing non-finite floats (invalid JSON for serde_json)."""
    output.parent.mkdir(parents=True, exist_ok=True)
    try:
        payload = json.dumps(rows, indent=2, default=str, allow_nan=False)
    except ValueError as e:
        raise SystemExit(
            f"ERROR: refusing to write non-finite values to {output}: {e}\n"
            "       Bare NaN/Infinity is invalid JSON; the Rust merger would reject the "
            "whole channel."
        )
    output.write_text(payload)
