"""Shared helpers for the adam-core external-reference runner (one channel).

ONE channel, tool id ``adam-core``: **adam-core as it ships publicly** — the
released PyPI packages ``adam-core==0.7.0`` and ``adam-assist==0.6.0``, run
through the public ``CORE_OD_V1`` orbit-determination profile. That profile's
observation pipeline is

    Efcc18CatalogBiasCorrection -> NightlyBatchDeweighting(cap=4)

and its only resource is the **public** EFCC18 star-catalog bias table that ships
as the ``jpl-debias-2018`` PyPI package (bound by
``core_od_v1_resource_bindings()``). There is no SigmaFill and no empirical-
covariance stage, and no private Asteroid-Institute table is installed, imported
or referenced anywhere. The proprietary ``ASTEROID_INSTITUTE_OD_V1`` profile (the
one that adds ``FillMissingAstrometricUncertainty`` on the private ``v2_sigma_fill``
table plus the ``v2_full`` residual-covariance table) is NOT used.

``CORE_OD_V1`` has no sigma-fill stage and requires finite, positive input
variances, so observations with no reported uncertainty are filled BEFORE the fit
from the suite's shared PUBLIC Vereš et al. (2017) model (``runners/sigmafill``) —
never adam-core's private table — with a reported sigma never overwritten and every
filled observation labelled by level (station / catalog / global) on the OD row.
The owner ruled this shared fill in (2026-11-08); it is the same fill the suite
applies to every tool that refuses missing sigma (layup next).

Shared by the entry points (run_adam_core.py, run_adam_core_walk.py, tests/).

Plan conventions (shared with every other runner, see runners/kete):
    ic_pos_au / ic_vel_au_d   ICRF (= J2000 equatorial), SSB-centred (NAIF 0), au / au d^-1
    epoch_mjd_tdb             initial-condition epoch, MJD TDB
    t_mjd_tdb                 target epoch, MJD TDB
    ref_pos_au                Horizons Cartesian reference at t (propagation rows)
    ref_ra_rad / ref_dec_rad  Horizons astrometric reference (ephemeris rows)
    ref_rho_au                Horizons range (ephemeris rows)
    observer                  MPC observatory code (ephemeris rows)
    ic_a1..ic_a3, ic_g_*      Marsden non-grav A-terms + g(r) (alpha, r0, m, n, k), ic_non_grav_dt

No-hidden-fallbacks discipline (brief + lang): every missing reported sigma,
dropped space-based observation, refused/failed fit and unsupported non-grav term
is COUNTED and carried on the output row with its reason — never silently dropped
or substituted, and no default sigma is ever injected.
"""

from __future__ import annotations

import json
import math
import os
import sys
from importlib import metadata
from pathlib import Path

AU_KM = 149_597_870.7
C_AU_PER_DAY = 173.144632674240
MJD_TO_JD = 2_400_000.5

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))  # runners/ -> the shared sigmafill package
VERSIONS_JSON = HERE / ".venv" / "adam-core-versions.json"


# ── provenance ───────────────────────────────────────────────


def source_version() -> str:
    """Compact provenance string stamped on every row's ``source_version``."""
    try:
        v = metadata.version("adam-core")
    except metadata.PackageNotFoundError:
        return "adam-core unknown (adam-core not installed)"
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


# ── self-perturbers (planets-only force model) ───────────────

# Four catalog objects — Iris, Vesta, Pallas, Hygiea — are themselves in ASSIST's
# SB441-n16 massive-asteroid perturber set, so the full force model makes the object
# perturb ITSELF (a 1/r^2 self-singularity: ~1e9 km error at 1 yr). The suite's own
# ASSIST runner handles this by dropping asteroid forces (a planets-only Ephem).
#
# adam-assist 0.6.0 (public release) exposes NO planets-only switch:
#   * its new ``adam_assist.perturbers`` module only DETECTS self-perturbers so a
#     caller can attach a warning — it does not change the force model;
#   * ``ASSISTPropagator(asteroids_path=None)`` resolves the DEFAULT SB441-n16
#     (still full model);
#   * a non-existent ``asteroids_path`` now RAISES (strict DAF binding in 0.6.0 —
#     the October runner's missing-file trick is dead);
#   * pointing ``asteroids_path`` at a planets kernel constructs but integrates
#     garbage (~1e9 km).
# So a self-perturber cannot be given a planets-only force model through the public
# wrapper. These rows are emitted as a NAMED unsupported row carrying that reason,
# never a self-singular state. (Owner decision pending; see the runner README.)
SELF_PERTURBERS = {"Iris", "Vesta", "Pallas", "Hygiea"}
_FORCE_MODEL_FULL = "planets+SB441-n16"
_FORCE_MODEL_SELF = (
    "self-perturber (SB441-n16 includes the object); adam-assist 0.6.0 has no "
    "planets-only switch — full model is self-singular, so this row is unsupported"
)
SELF_PERTURBER_UNSUPPORTED = (
    "self-perturber: adam-assist 0.6.0 offers no planets-only force model "
    "(asteroids_path=None -> full SB441-n16; missing file -> RuntimeError; "
    "planets kernel -> ~1e9 km garbage); full model perturbs the object with itself"
)


def is_self_perturber(obj) -> bool:
    return str(obj) in SELF_PERTURBERS


def force_model_label(obj) -> str:
    return _FORCE_MODEL_SELF if is_self_perturber(obj) else _FORCE_MODEL_FULL


# adam-assist 0.6.0 shipped integrator defaults. These are EXPLICIT runner settings
# (recorded on every row as adam_core_epsilon / adam_core_adaptive_mode), not hidden:
# lang notes epsilon 1e-6 is loose and bead app3v records min_dt collapsing on
# encounter arcs under adaptive_mode 1 (layup moved to adaptive_mode 2). The smoke
# compares the shipped 1e-6/mode-1 against 1e-9/mode-2.
SHIPPED_EPSILON = 1e-6
SHIPPED_ADAPTIVE_MODE = 1
SHIPPED_MIN_DT = 1e-9
SHIPPED_INITIAL_DT = 1e-6


# ── registered kernels (the third-party set's adam-core/ folder) ─────────

# adam-assist's constructor accepts planets_path / asteroids_path. The suite feeds
# every tool the same registered data products (owner ruling D4), so the runner
# hands adam-assist the third-party set's adam-core/ folder explicitly and NEVER
# falls back to the install-time PyPI kernel packages: a missing file is a refusal
# to start, naming the file.
KERNEL_FILES = {"planets": "de440.bsp", "asteroids": "sb441-n16.bsp"}
_KERNELS: dict | None = None
_SHA_CACHE: dict = {}


def sha256_file(path) -> str:
    path = str(path)
    if path not in _SHA_CACHE:
        import hashlib

        h = hashlib.sha256()
        with open(path, "rb") as fh:
            for chunk in iter(lambda: fh.read(1 << 20), b""):
                h.update(chunk)
        _SHA_CACHE[path] = h.hexdigest()
    return _SHA_CACHE[path]


def configure_kernels(data_dir) -> dict:
    """Bind the registered kernels from ``data_dir`` (the ``adam-core/`` folder of the
    third-party tool set). Raises SystemExit naming every missing file; there is no
    fallback to the PyPI data packages."""
    global _KERNELS
    if data_dir is None:
        raise SystemExit(
            "ERROR: no adam-core data directory. Pass --data-dir <third-party>/adam-core "
            "or set ADAM_CORE_DATA_DIR; the runner reads DE440 and SB441-N16 only from "
            "the registered set (make run-adam-core stages it)."
        )
    d = Path(data_dir)
    paths = {k: d / name for k, name in KERNEL_FILES.items()}
    missing = [str(p) for p in paths.values() if not p.is_file()]
    if missing:
        raise SystemExit(
            "ERROR: registered kernel(s) missing: " + ", ".join(missing)
            + ". Refusing to fall back to the install-time PyPI kernels."
        )
    _KERNELS = paths
    return paths


def kernels() -> dict:
    if _KERNELS is None:
        raise RuntimeError("configure_kernels() was not called before building a propagator")
    return _KERNELS


def default_propagator(epsilon: float = SHIPPED_EPSILON,
                       adaptive_mode: int = SHIPPED_ADAPTIVE_MODE):
    """An ASSISTPropagator on the REGISTERED kernels with explicit integrator
    settings (never propagator=None, which would silently select adam_core's
    built-in two-body provider)."""
    from adam_assist import ASSISTPropagator

    k = kernels()
    prop = ASSISTPropagator(
        planets_path=str(k["planets"]), asteroids_path=str(k["asteroids"]),
        epsilon=epsilon, adaptive_mode=adaptive_mode,
        min_dt=SHIPPED_MIN_DT, initial_dt=SHIPPED_INITIAL_DT,
    )
    # The paths the propagator holds are the registered ones, not the PyPI default.
    assert prop.planets_path == str(k["planets"]), (prop.planets_path, k["planets"])
    assert prop.asteroids_path == str(k["asteroids"]), (prop.asteroids_path, k["asteroids"])
    return prop


def propagator_label(prop) -> str:
    """Assert which propagator a row was computed with (point 3: run_od(propagator=None)
    silently falls back to two-body, so we never pass None and we record the backend)."""
    try:
        nb = prop.native_backend
        return f"{nb.name} {nb.version} (eps={prop.epsilon}, adaptive_mode={prop.adaptive_mode})"
    except Exception:  # noqa: BLE001
        return "unknown"


def missing_asteroid_path_raises(path: str = "/nonexistent-sb441-self-perturber.bsp") -> str:
    """Prove (don't assume) that the October planets-only trick is dead in
    adam-assist 0.6.0: constructing an ``ASSISTPropagator`` with a non-existent
    ``asteroids_path`` must now RAISE. Returns the exception's text; raises
    RuntimeError if construction unexpectedly SUCCEEDS (which would mean a
    planets-only route exists after all and the self-perturber policy should
    be revisited)."""
    from adam_assist import ASSISTPropagator

    try:
        ASSISTPropagator(asteroids_path=path)
    except Exception as e:  # noqa: BLE001
        return f"{type(e).__name__}: {e}"
    raise RuntimeError(
        "ASSISTPropagator(asteroids_path=<non-existent>) did NOT raise in this "
        "adam-assist build — a planets-only route may exist; revisit the "
        "self-perturber policy."
    )


# ── adam_core state construction (propagation / ephemeris) ────

# Standard Marsden asteroid g(r) = 1/r^2 (alpha=1, r0=1, m=2, n=0, k=0). Used
# when a row carries A-terms but no explicit g(r) — this is the DEFINITION of the
# standard non-grav law, not a substitution; comet rows carry their own g(r).
_STD_GR = {"ALN": 1.0, "R0": 1.0, "NM": 2.0, "NN": 0.0, "NK": 0.0}


def nongrav_for(row: dict):
    """(NonGravitationalParameters | None, unsupported_reason | None) for a plan row.

    adam_core's ``NonGravitationalParameters`` carries A1/A2/A3 and the g(r) terms
    (ALN, R0, NM, NN, NK) but has NO Marsden time-delay (DT) term, so a row with a
    non-zero ``ic_non_grav_dt`` is modelled WITHOUT its DT — surfaced (never hidden)
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
    non-grav A1/A2/A3 + g(r) are attached when present.
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


# ── OD observation ingestion (ADES PSV -> SourceCatalog) ──────


def normalize_psv(text: str) -> str:
    """Strip ADES fixed-width padding (adam_core's parser is cleaner on trimmed fields)."""
    out = []
    for line in text.splitlines():
        if line.startswith(("#", "!")) or not line.strip():
            out.append(line)
            continue
        out.append("|".join(c.strip() for c in line.split("|")))
    return "\n".join(out) + "\n"


def od_config():
    """The bound public ``CORE_OD_V1`` OdConfig (EFCC18 debias + nightly-batch
    deweighting on the public jpl-debias-2018 table). No private resource."""
    from adam_core.orbit_determination import CORE_OD_V1, core_od_v1_resource_bindings

    return CORE_OD_V1.bind(core_od_v1_resource_bindings())


def od_profile_models() -> list[str]:
    """The model-stack NAMES of the bound CORE_OD_V1 pipeline, for the row metadata."""
    from adam_core.orbit_determination import CORE_OD_V1

    return [type(t).__name__ for t in CORE_OD_V1.observation_pipeline.transforms]


def sigma_model():
    """The shared public Vereš et al. (2017) sigma-fill model (loaded from the CSV
    beside the shared helper under ``runners/sigmafill``)."""
    sys.path.insert(0, str(HERE.parent))  # runners/ -> import sigmafill
    from sigmafill.veres2017 import Veres2017SigmaModel

    return Veres2017SigmaModel.from_csv()


def build_source_catalog(psv_text: str, object_id: str, ok, model=None):
    """Parse an ADES PSV into a (SourceCatalog, Associations) for ``run_od``.

    Space-based / unplaceable stations (satellites, geocentric occultation) cannot
    be placed as ground observers, so their rows are dropped and COUNTED — a
    capability gap shared with every other runner, never a silent skip.

    CORE_OD_V1 has no sigma-fill stage and requires finite positive input variances,
    so observations with no reported uncertainty are filled from the suite's shared
    PUBLIC Vereš et al. (2017) model (``runners/sigmafill``) BEFORE the catalog is
    built — never adam-core's private table, and a REPORTED sigma is never
    overwritten. Returns ``(catalog | None, associations | None, n_dropped,
    dropped_stations, n_parsed, sigma_report)`` where ``sigma_report`` carries the
    fill model, ``reported``, ``filled`` and ``filled_by_level`` (station / catalog
    / global) counts.
    """
    import pyarrow as pa
    import pyarrow.compute as pc
    from adam_core.observations import SourceCatalog
    from adam_core.observations.ades import ADES_string_to_tables

    sys.path.insert(0, str(HERE.parent))
    from sigmafill.veres2017 import fill_missing

    _ctx, ades = ADES_string_to_tables(normalize_psv(psv_text))
    stns = ades.stn.to_pylist()
    n_parsed = len(stns)
    droppable = sorted({s for s in set(stns) if not ok(s)})
    n_dropped = sum(1 for s in stns if s in set(droppable))
    if droppable:
        keep = pa.array(sorted(s for s in set(stns) if ok(s)))
        ades = ades.apply_mask(pc.is_in(ades.stn, value_set=keep))
    n = len(ades)
    if n == 0:
        return None, None, n_dropped, droppable, n_parsed, {
            "model": "Veres2017SigmaFill", "reported": 0, "filled": 0,
            "filled_by_level": {"station": 0, "catalog": 0, "global": 0}, "total": 0,
        }

    obs_codes = ades.stn.to_pylist()
    astcat = ades.astCat.to_pylist()
    rra = ades.rmsRACosDec.to_pylist()
    rdec = ades.rmsDec.to_pylist()
    if model is None:
        model = sigma_model()
    filled_ra, filled_dec, sigma_report = fill_missing(obs_codes, astcat, rra, rdec, model)

    catalog_id = [a if a not in (None, "") else "UNK" for a in astcat]
    catalog = SourceCatalog.from_kwargs(
        id=[f"{object_id}#{i}" for i in range(n)],
        time=ades.obsTime,
        ra=ades.ra,
        dec=ades.dec,
        ra_sigma=filled_ra,
        dec_sigma=filled_dec,
        radec_corr=ades.rmsCorr,
        observatory_code=ades.stn,
        object_id=[object_id] * n,
        catalog_id=catalog_id,
        astcat=astcat,
    )
    return catalog, catalog.associations(), n_dropped, droppable, n_parsed, sigma_report


# ── settings + versions stamped on every row ─────────────────

def obscodes_record(registered_obscodes: Path | None = None) -> dict:
    """Where adam_core's observatory codes come from. adam_core reads the file of its
    ``mpc-obscodes`` PyPI dependency and accepts no path, so the difference from the
    registered ``obscodes_extended.json`` is STATED (hashes + identical flag), not
    patched."""
    from mpc_obscodes import mpc_obscodes

    installed = Path(mpc_obscodes)
    rec = {
        "obscodes_source": f"mpc-obscodes {metadata.version('mpc-obscodes')} (installed package file; adam_core accepts no path)",
        "obscodes_sha256": sha256_file(installed),
    }
    if registered_obscodes is not None and Path(registered_obscodes).is_file():
        reg = sha256_file(registered_obscodes)
        rec["obscodes_registered_sha256"] = reg
        rec["obscodes_identical_to_registered"] = str(reg == rec["obscodes_sha256"]).lower()
    else:
        rec["obscodes_registered_sha256"] = "registered obscodes_extended.json not found"
        rec["obscodes_identical_to_registered"] = "unknown"
    return rec


def settings_record(epsilon: float, adaptive_mode: int, fill_table_sha256: str,
                    registered_obscodes: Path | None = None) -> dict[str, str]:
    """Every setting that shaped this run, as strings, read from the bound profile and
    the installed packages (nothing hand-typed that the tool can report)."""
    from adam_core.orbit_determination import CORE_OD_V1, core_od_v1_resource_bindings

    k = kernels()
    iod = CORE_OD_V1.iod.search_correction_ranging
    ref = CORE_OD_V1.refinement.refinement
    bias_path = dict(core_od_v1_resource_bindings().paths)["efcc18_catalog_bias_table"]
    rec = {
        "profile": f"{CORE_OD_V1.name} v{CORE_OD_V1.version} (sha256 {CORE_OD_V1.definition_sha256})",
        "observation_pipeline": " -> ".join(
            f"{type(t).__name__}" for t in CORE_OD_V1.observation_pipeline.transforms
        ),
        "nightly_batch_cap": str(next(
            (t.effective_observation_cap for t in CORE_OD_V1.observation_pipeline.transforms
             if hasattr(t, "effective_observation_cap")), "none")),
        "debias_table": f"jpl-debias-2018 {metadata.version('jpl-debias-2018')} (sha256 {sha256_file(bias_path)})",
        "iod": f"Gauss spans IOD, min_obs={iod.min_obs}, min_arc_length={iod.min_arc_length} d, rchi2_threshold={iod.rchi2_threshold}",
        "refinement": f"{ref.method}, chi2_reject={ref.chi2_reject}, chi2_recover={ref.chi2_recover}, max_iterations={ref.max_iterations}",
        "propagator": f"adam-assist {metadata.version('adam-assist')} (ASSIST IAS15)",
        "epsilon": repr(epsilon),
        "adaptive_mode": str(adaptive_mode),
        "min_dt": repr(SHIPPED_MIN_DT),
        "initial_dt": repr(SHIPPED_INITIAL_DT),
        "planets_kernel": f"{k['planets'].name} (sha256 {sha256_file(k['planets'])})",
        "asteroid_kernel": f"{k['asteroids'].name} (sha256 {sha256_file(k['asteroids'])})",
        "kernel_source": "registered third-party set, adam-core/ folder, passed as planets_path/asteroids_path",
        "sigma_fill": f"{SIGMA_FILL_NAME} (sha256 {fill_table_sha256}); CORE_OD_V1 has no fill stage",
        "self_perturbers": "Iris, Vesta, Pallas, Hygiea: named unsupported (adam-assist 0.6.0 cannot exclude a body)",
        "non_grav": "A1/A2/A3 + g(r); no Marsden DT term",
        "space_based_observers": "dropped and counted (adam_core cannot place them)",
    }
    rec.update(obscodes_record(registered_obscodes))
    return rec


SIGMA_FILL_NAME = "Veres2017SigmaFill"


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
