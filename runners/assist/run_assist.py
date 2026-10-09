#!/usr/bin/env python3
"""Run ASSIST propagation on the empyrean validation object catalog.

Produces a JSON file that `empyrean validate --assist-results` can merge
into the validation report for 3-way comparison (empyrean vs ASSIST vs
Horizons).

ASSIST is GPL-licensed and never linked into empyrean. This script runs
independently in its own virtual environment.

Usage:
    python run_assist.py --data-dir ~/.empyrean/data --output assist_results.json
    python run_assist.py --objects Apophis,Bennu --output assist_results.json
    python run_assist.py --populations NEO,MBA --output assist_results.json

Setup:
    ./setup.sh  # creates venv, installs rebound + assist, downloads data
"""

import argparse
import hashlib
import json
import re
import sys
import time
import urllib.parse
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

import numpy as np

try:
    import assist
    import rebound
except ImportError:
    print("ERROR: rebound and assist not installed.")
    print("Run: ./setup.sh")
    sys.exit(1)

# ── Constants ───────────────────────────────────────────

MJD_TO_JD = 2_400_000.5
J2000_JD = 2_451_545.0
AU_KM = 149_597_870.700


class WalltimeExceeded(Exception):
    """Raised by :func:`propagate_assist` when a row's wall-clock cap is hit.

    Carries the elapsed and cap seconds so the caller can write a loud FAIL row
    with the reason and the measured time — never a silent skip and never a
    substituted number. Under REBOUND's library defaults IAS15 has ``min_dt = 0``
    (no floor on the adaptive step), so an encounter arc that makes the step
    shrink without bound would otherwise hang the channel; the cap turns that
    into an explicit failure.
    """

    def __init__(self, elapsed_s: float, cap_s: float):
        self.elapsed_s = elapsed_s
        self.cap_s = cap_s
        super().__init__(
            f"wall-clock cap {cap_s:.1f}s exceeded (integration ran {elapsed_s:.1f}s "
            "without reaching the target epoch — REBOUND IAS15 step collapse)"
        )


# ── ASSIST configurations ───────────────────────────────
#
# A configuration is a named set of integrator settings applied in ONE place,
# to the simulation AFTER `assist.Extras(sim, ephem)` has attached. Attach
# itself sets `ri_ias15.adaptive_mode = 1` (assist_attach, assist.c) and rescales
# G; REBOUND leaves epsilon 1e-9, min_dt 0, initial dt 0.001 day untouched. The
# runner sets nothing else on the simulation but the particle and the epoch, so
# `assist_default` applies NOTHING (it is exactly what attach leaves). Every arm
# reads its resolved settings back off the simulation after attach + config for
# provenance — assumed values are never stamped.
#
# Sources:
#   assist_default            — attach defaults (mode 1, eps 1e-9, min_dt 0, dt 0.001 d).
#   assist_layup              — Smithsonian/layup sets ri_ias15.adaptive_mode = 2 (PRS 2024)
#                               after attach; src/lib/orbit_fit/orbit_fit.cpp:72.
#   assist_asteroid_institute — B612 adam-assist ASSISTPropagator constructor defaults
#                               (src/adam_assist/propagator.py): eps 1e-6, min_dt 1e-9 d,
#                               adaptive_mode 1, initial dt 1e-6 d. This is the configuration
#                               the runner used before, i.e. the stored report numbers.
ASSIST_CONFIGS = {
    "assist_default": {},
    "assist_layup": {"adaptive_mode": 2},
    "assist_asteroid_institute": {
        "epsilon": 1e-6, "min_dt": 1e-9, "adaptive_mode": 1, "dt": 1e-6,
    },
}

# The six arms: three configurations, each with variational particles off / on.
ASSIST_ARMS = [
    ("assist_default", False, "assist_default_variational_off"),
    ("assist_default", True, "assist_default_variational_on"),
    ("assist_layup", False, "assist_layup_variational_off"),
    ("assist_layup", True, "assist_layup_variational_on"),
    ("assist_asteroid_institute", False, "assist_asteroid_institute_variational_off"),
    ("assist_asteroid_institute", True, "assist_asteroid_institute_variational_on"),
]


def apply_config(sim, config):
    """Apply a named configuration's integrator settings to `sim`.

    Called ONCE, right after `assist.Extras(sim, ephem)` attaches. `assist_default`
    applies nothing (attach already left the settings it wants). The others set
    only the knobs their source specifies, over what attach left.
    """
    c = ASSIST_CONFIGS[config]
    if "epsilon" in c:
        sim.ri_ias15.epsilon = c["epsilon"]
    if "min_dt" in c:
        sim.ri_ias15.min_dt = c["min_dt"]
    if "adaptive_mode" in c:
        sim.ri_ias15.adaptive_mode = c["adaptive_mode"]
    if "dt" in c:
        sim.dt = c["dt"]


def resolved_config(config, ephem):
    """Read the integrator settings back off a probe simulation AFTER attach +
    config, so provenance reports what the arm actually ran under — never
    assumed values. A dummy heliocentric particle is enough to attach."""
    sim = rebound.Simulation()
    sim.add(x=1.0, y=0.0, z=0.0, vx=0.0, vy=0.0172, vz=0.0)
    assist.Extras(sim, ephem)
    apply_config(sim, config)
    return {
        "config": config,
        "assist_version": assist.__version__,
        "rebound_version": rebound.__version__,
        "integrator": sim.integrator,
        "ias15_epsilon": sim.ri_ias15.epsilon,
        "ias15_min_dt": sim.ri_ias15.min_dt,
        "ias15_adaptive_mode": str(sim.ri_ias15.adaptive_mode),
        "initial_dt": sim.dt,
    }


# ── Horizons cache ──────────────────────────────────────


class HorizonsCache:
    """Disk cache for Horizons state vectors.

    Compatible with empyrean's disk-cache format: each entry is a JSON
    file named by sanitized key, containing {key, response}.
    """

    def __init__(self, cache_dir: Path):
        self.cache_dir = cache_dir
        self.hits = 0
        self.misses = 0
        self._index: dict[tuple[str, float], tuple[np.ndarray, np.ndarray]] = {}

        if not cache_dir.exists():
            cache_dir.mkdir(parents=True, exist_ok=True)
            return

        # Load all cache files and index by (command, epoch_mjd)
        for path in cache_dir.glob("*.json"):
            try:
                data = json.loads(path.read_text())
                # DiskCache format: {key, response} where response has "result"
                if "response" in data and "result" in data.get("response", {}):
                    result_text = data["response"]["result"]
                    pos, vel = self._parse_vectors(result_text)
                    # Key format: "command|epoch_mjd"
                    parts = data["key"].split("|")
                    if len(parts) == 2:
                        cmd = parts[0]
                        epoch = round(float(parts[1]), 6)
                        self._index[(cmd, epoch)] = (pos, vel)
                # Legacy format: {command, epoch_mjd, pos, vel}
                elif "query_type" in data and data.get("query_type") == "vectors":
                    key = (data["command"], round(data["epoch_mjd"], 6))
                    self._index[key] = (
                        np.array(data["pos"]),
                        np.array(data["vel"]),
                    )
            except (json.JSONDecodeError, KeyError, ValueError):
                continue

        print(f"  Loaded {len(self._index)} cached Horizons vectors")

    @staticmethod
    def _parse_vectors(result_text: str):
        """Parse X/Y/Z/VX/VY/VZ from Horizons vector-table text."""
        in_data = False
        values = {}
        for line in result_text.splitlines():
            if "$$SOE" in line:
                in_data = True
                continue
            if "$$EOE" in line:
                break
            if not in_data or not line.strip():
                continue
            segments = line.split("=")
            for i in range(1, len(segments)):
                key = segments[i - 1].split()[-1] if segments[i - 1].split() else ""
                val_str = segments[i].split()[0] if segments[i].split() else ""
                try:
                    values[key] = float(val_str)
                except ValueError:
                    pass
        pos = np.array([values["X"], values["Y"], values["Z"]])
        vel = np.array([values["VX"], values["VY"], values["VZ"]])
        return pos, vel

    @staticmethod
    def _sanitize(key: str) -> str:
        result = []
        for c in key:
            if c in "/ |":
                result.append("_")
            elif c.isalnum() or c in "_-.":
                result.append(c)
            else:
                result.append("_")
        return "".join(result)

    def get(self, command: str, epoch_mjd: float):
        key = (command, round(epoch_mjd, 6))
        result = self._index.get(key)
        if result is not None:
            self.hits += 1
        return result

    def put(self, command: str, epoch_mjd: float, pos, vel):
        """Store a fetched result in the index and write to disk.

        Writes in empyrean's disk-cache format so Rust can read it back.
        We store the raw Horizons JSON response (with "result" text).
        """
        key = (command, round(epoch_mjd, 6))
        self._index[key] = (np.array(pos), np.array(vel))
        self.misses += 1

        # We don't have the raw Horizons JSON here — store in legacy format
        # that this class can read back on next init.
        cache_key = f"{command}|{epoch_mjd:.8f}"
        filename = self._sanitize(cache_key) + ".json"
        path = self.cache_dir / filename
        data = {
            "command": command,
            "epoch_mjd": epoch_mjd,
            "query_type": "vectors",
            "pos": [float(x) for x in pos],
            "vel": [float(x) for x in vel],
        }
        # allow_nan=False here too: this is a CACHE, so a non-finite value from
        # a JPL response would be written once and then silently reused as an
        # initial condition by every later run. Fail on the way in, not on the
        # thousandth read.
        path.write_text(json.dumps(data, indent=2, allow_nan=False))


# ── Horizons API ────────────────────────────────────────


def fetch_horizons_vectors(command: str, epoch_mjd: float):
    """Fetch SSB ICRF cartesian state from JPL Horizons API."""
    epoch_jd = epoch_mjd + MJD_TO_JD
    params = {
        "format": "json",
        "COMMAND": f"'{command}'",
        "OBJ_DATA": "NO",
        "MAKE_EPHEM": "YES",
        "EPHEM_TYPE": "VECTORS",
        "CENTER": "@0",
        "REF_PLANE": "FRAME",
        "VEC_TABLE": "2",
        "TLIST": f"{epoch_jd:.8f}",
        "VEC_LABELS": "YES",
        "CSV_FORMAT": "NO",
        "OUT_UNITS": "AU-D",
    }
    url = "https://ssd.jpl.nasa.gov/api/horizons.api?" + urllib.parse.urlencode(params)
    with urllib.request.urlopen(url, timeout=30) as resp:
        data = json.loads(resp.read())

    text = data["result"]
    in_data = False
    data_lines = []
    for line in text.split("\n"):
        if "$$SOE" in line:
            in_data = True
            continue
        if "$$EOE" in line:
            break
        if in_data and line.strip():
            data_lines.append(line.strip())

    values = {}
    for line in data_lines:
        for m in re.finditer(r"(V?[XYZ])\s*=\s*([+\-]?\d+\.\d+E[+\-]\d+)", line):
            values[m.group(1)] = float(m.group(2))

    for k in ["X", "Y", "Z", "VX", "VY", "VZ"]:
        if k not in values:
            raise ValueError(f"Missing {k} in Horizons response for {command}")

    return (
        np.array([values["X"], values["Y"], values["Z"]]),
        np.array([values["VX"], values["VY"], values["VZ"]]),
    )


def query_horizons(cache: HorizonsCache, command: str, epoch_mjd: float):
    """Query Horizons with cache."""
    cached = cache.get(command, epoch_mjd)
    if cached is not None:
        return cached
    pos, vel = fetch_horizons_vectors(command, epoch_mjd)
    cache.put(command, epoch_mjd, pos, vel)
    return pos, vel


# ── ASSIST propagation ──────────────────────────────────


def propagate_assist(
    pos_ssb,
    vel_ssb,
    epoch_mjd,
    target_mjd,
    ephem,
    a1=0.0,
    a2=0.0,
    a3=0.0,
    gr_alpha=None,
    gr_nk=None,
    gr_nm=None,
    gr_nn=None,
    gr_r0=None,
    with_stm=False,
    walltime_cap_s=None,
    config="assist_default",
):
    """Propagate with ASSIST. Returns (pos, vel, time_ms, stm).

    Two integration modes, selected by the `with_stm` flag:

    - False (default): single-particle f64 propagation. `stm`
      returned as None. Comparable to empyrean's
      `propagation_uncertainty = "none"` path.
    - True: 6 REBOUND first-order variational particles seeded with
      unit vectors in each state component. The integrated 6×6 STM is
      returned. Comparable to empyrean's `"first_order"`
      path.

    ASSIST encodes first-order variational derivatives only ("the
    first order variational equations are included for all terms"),
    so there is deliberately NO second-order mode: REBOUND's order-2
    variational machinery would integrate shadow particles that
    receive no second-order contributions from ASSIST's force model,
    and the resulting timing would not be comparable to a genuine
    STT propagation. empyrean's `"second_order"` rows have
    no ASSIST counterpart.

    REBOUND propagates the variational equations under the
    gravitational force model only — ASSIST's non-gravitational
    `additional_forces` callback is applied to the real particle but
    not to the variational shadows. The STM is therefore exact for
    purely gravitating objects and gravity-only-approximate for active
    bodies with non-zero a1/a2/a3.

    Parameters
    ----------
    a1, a2, a3 : float
        Marsden non-gravitational parameters (AU/day^2).
    gr_* : float or None
        g(r) function parameters. None = ASSIST defaults (r^{-2}).
    with_stm : bool
        Adds 6 first-order variational particles for the STM.
    walltime_cap_s : float or None
        When set, install a REBOUND heartbeat that calls ``sim.stop()`` once the
        call's wall-clock exceeds this many seconds; if the integration is
        stopped short of the target epoch, raise :class:`WalltimeExceeded`. The
        heartbeat fires every timestep and adds a per-step Python-callback cost,
        so the caller uses it ONLY on the untimed warm-up call and leaves the
        timed best-of-N runs uncapped (a warm-up that finished within the cap
        proves the same deterministic integration cannot hang when repeated).
        ``None`` (default) installs no heartbeat and no cap.

    config : str
        Name of the configuration in :data:`ASSIST_CONFIGS` to apply AFTER
        `assist.Extras` attaches. ``assist_default`` applies nothing (attach's
        own settings ride). The resolved settings are read back off the
        simulation for provenance by :func:`resolved_config`.
    """
    t0_sim = epoch_mjd + MJD_TO_JD - J2000_JD
    dt = target_mjd - epoch_mjd
    tmax = t0_sim + dt

    # Whole-call timer: everything from the start of the row (Simulation
    # creation, Extras binding, adding the particle, variational setup)
    # through the end of sim.integrate. The ephemeris object is created by the
    # caller and stays outside this timer, matching empyrean's Context. This
    # is the symmetric partner of empyrean's whole-propagate() f64_detection_on
    # timing; the narrower `elapsed_ms` below times only sim.integrate.
    t_call0 = time.perf_counter()
    sim = rebound.Simulation()
    sim.t = t0_sim
    # No integrator overrides: run at rebound + assist library defaults.
    if walltime_cap_s is not None:
        # Heartbeat runs in-process every timestep; when the wall-clock cap is
        # exceeded it asks REBOUND to stop cleanly (ret_value 5, no exception
        # from integrate). We then detect the short stop below and raise.
        def _heartbeat(_sim_ptr, _cap=walltime_cap_s, _start=t_call0):
            if time.perf_counter() - _start > _cap:
                sim.stop()

        sim.heartbeat = _heartbeat
    sim.add(
        x=pos_ssb[0], y=pos_ssb[1], z=pos_ssb[2],
        vx=vel_ssb[0], vy=vel_ssb[1], vz=vel_ssb[2],
    )
    extras = assist.Extras(sim, ephem)
    # Apply the configuration in ONE place, after attach. `assist_default`
    # applies nothing (attach's own settings ride).
    apply_config(sim, config)

    # Non-gravitational forces
    has_ng = a1 != 0.0 or a2 != 0.0 or a3 != 0.0
    if has_ng:
        # ASSIST's non-grav variational loop (assist_additional_force_non_gravitational
        # in src/forces.c) reads particle_params for EACH variational particle at
        # index 3*(N_real + v); with only the three real-particle values it reads
        # past the array (undefined behaviour — it produced erratic, up to ~1e4,
        # STM elements for active a1/a2/a3 bodies). The STM perturbs the state
        # only, never A1/A2/A3, so every shadow carries a zero non-grav
        # perturbation. Pad to cover the six variational particles whenever they
        # are present; the f64 (no-STM) path keeps the bare three-element array.
        if with_stm:
            extras.particle_params = np.array([a1, a2, a3] + [0.0] * 18)
        else:
            extras.particle_params = np.array([a1, a2, a3])

        # Set g(r) parameters if provided
        if gr_alpha is not None:
            extras.alpha = gr_alpha
        if gr_nk is not None:
            extras.nk = gr_nk
        if gr_nm is not None:
            extras.nm = gr_nm
        if gr_nn is not None:
            extras.nn = gr_nn
        if gr_r0 is not None:
            extras.r0 = gr_r0

    # Variational particles: 6 first-order particles seeded with unit
    # perturbations in each state component (x, y, z, vx, vy, vz).
    #
    # `testparticle=0` is REQUIRED, not optional. ASSIST's variational force
    # loop (assist_additional_force_direct in src/forces.c) applies the
    # gravitational-gradient acceleration to a variational particle only when
    # its configuration's `testparticle` index equals the real particle it
    # tracks: `if (tp == j)`. REBOUND's `add_variation(order=1)` defaults
    # `testparticle=-1` (a whole-simulation variation), so with the default the
    # `tp == j` test never fires, the shadows receive ZERO acceleration, and the
    # returned STM degenerates to the free-particle matrix [[I, dt·I],[0,I]] for
    # every object. Tying each variation to the single real particle (index 0)
    # makes ASSIST integrate the variational equations under its ephemeris
    # gravity; the resulting STM then agrees with a single-particle central
    # finite difference to ~1e-8 (verified on Nysa at |dt| = 365 d and 5475 d).
    first_order = []
    if with_stm:
        for k in range(6):
            v = sim.add_variation(order=1, testparticle=0)
            vp = v.particles[0]
            vp.x = 1.0 if k == 0 else 0.0
            vp.y = 1.0 if k == 1 else 0.0
            vp.z = 1.0 if k == 2 else 0.0
            vp.vx = 1.0 if k == 3 else 0.0
            vp.vy = 1.0 if k == 4 else 0.0
            vp.vz = 1.0 if k == 5 else 0.0
            first_order.append(v)

    t0 = time.perf_counter()
    sim.integrate(tmax)
    _t_after_integrate = time.perf_counter()
    elapsed_ms = (_t_after_integrate - t0) * 1000.0
    call_ms = (_t_after_integrate - t_call0) * 1000.0

    # Cap hit: the heartbeat stopped the integration short of the target epoch.
    # Fail loudly with the elapsed time; never return a partial state as if it
    # reached the requested epoch.
    if walltime_cap_s is not None and abs(sim.t - tmax) > 1e-6:
        raise WalltimeExceeded(_t_after_integrate - t_call0, walltime_cap_s)

    p = sim.particles[0]
    pos = np.array([p.x, p.y, p.z])
    vel = np.array([p.vx, p.vy, p.vz])

    stm = None
    if first_order:
        stm = np.zeros((6, 6))
        for k, v in enumerate(first_order):
            vp = v.particles[0]
            stm[0, k] = vp.x
            stm[1, k] = vp.y
            stm[2, k] = vp.z
            stm[3, k] = vp.vx
            stm[4, k] = vp.vy
            stm[5, k] = vp.vz
    # STT (21 second-order variational particles) is propagated for
    # timing parity with empyrean's Jet2 path but not extracted into
    # an output array here — we don't compare STT values directly,
    # only the propagated state and the timing.

    del extras
    return pos, vel, elapsed_ms, call_ms, stm


# ── Object catalog ──────────────────────────────────────
# Mirrors empyrean's validation object catalog.

_SPANS = [30, 90, 180, 365, 3 * 365, 5 * 365, 10 * 365]
DEFAULT_DT_DAYS = sorted([-dt for dt in _SPANS] + [+dt for dt in _SPANS])


class Obj:
    def __init__(self, name, horizons_command, population, dt_days=None, notes="",
                 a1=0.0, a2=0.0, a3=0.0, gr_alpha=None, gr_nk=None, gr_nm=None,
                 gr_nn=None, gr_r0=None):
        self.name = name
        self.horizons_command = horizons_command
        self.population = population
        self.dt_days = dt_days or DEFAULT_DT_DAYS
        self.notes = notes
        self.a1 = a1
        self.a2 = a2
        self.a3 = a3
        self.gr_alpha = gr_alpha
        self.gr_nk = gr_nk
        self.gr_nm = gr_nm
        self.gr_nn = gr_nn
        self.gr_r0 = gr_r0

    @property
    def has_nongrav(self):
        return self.a1 != 0.0 or self.a2 != 0.0 or self.a3 != 0.0


# SBDB non-grav params must be fetched separately for comets.
# These are placeholder values — the script fetches actual params from SBDB API.
ALL_OBJECTS = [
    # NEOs
    Obj("Apophis", "99942;", "NEO",
        dt_days=[-3650, -1825, -365, -90, -30, 1239, 1300, 1600],
        notes="2029 Earth CA"),
    Obj("Bennu", "101955;", "NEO", notes="OSIRIS-REx target"),
    Obj("Didymos", "65803;", "NEO", notes="DART target"),
    Obj("Duende", "367943;", "NEO", notes="2013 Earth CA"),
    Obj("Eros", "433;", "NEO", notes="NEAR target"),
    Obj("Farnocchia", "84100;", "NEO", notes="ASSIST validation object"),

    # MBAs
    Obj("Lutetia", "21;", "MBA"),
    Obj("Nysa", "44;", "MBA"),
    Obj("Holman", "3666;", "MBA"),

    # Self-Perturbers
    Obj("Iris", "7;", "Self-Perturber"),
    Obj("Vesta", "4;", "Self-Perturber"),
    Obj("Pallas", "2;", "Self-Perturber"),
    Obj("Hygiea", "10;", "Self-Perturber"),

    # TNOs
    Obj("Eris", "136199;", "TNO"),
    Obj("Makemake", "136472;", "TNO"),
    Obj("Varuna", "20000;", "TNO"),

    # Comets (non-grav params fetched from SBDB at runtime)
    Obj("67P", "DES=67P;CAP;NOFRAG", "Comet",
        notes="Rosetta target, non-grav from SBDB"),
    Obj("2P/Encke", "DES=2P;CAP;NOFRAG", "Comet",
        notes="Shortest period comet"),
    Obj("103P/Hartley 2", "DES=103P;CAP;NOFRAG", "Comet",
        notes="EPOXI target"),
    Obj("46P/Wirtanen", "DES=46P;CAP;NOFRAG", "Comet",
        notes="2018 close approach"),

    # Centaur
    Obj("Chiron", "2060;", "Centaur"),

    # Jupiter Trojans
    Obj("Hektor", "624;", "Jupiter Trojan"),
    Obj("Patroclus", "617;", "Jupiter Trojan"),
    Obj("Achilles", "588;", "Jupiter Trojan"),

    # Neptune Trojans
    Obj("2001 QR322", "DES=2001 QR322;", "Neptune Trojan"),
    Obj("2005 TN53", "DES=2005 TN53;", "Neptune Trojan"),
    Obj("2008 LC18", "DES=2008 LC18;", "Neptune Trojan"),

    # Earth Trojans
    Obj("2010 TK7", "DES=2010 TK7;", "Earth Trojan"),
    Obj("2020 XL5", "DES=2020 XL5;", "Earth Trojan"),

    # ISOs
    Obj("1I/'Oumuamua", "DES=1I;", "ISO", dt_days=[-365, -180, -90, -30],
        notes="Custom g(r) non-grav"),
    Obj("2I/Borisov", "DES=2I;", "ISO", dt_days=[-365, -180, -90, -30]),

    # TCOs
    Obj("2020 CD3", "DES=2020 CD3;", "TCO",
        dt_days=[-2400, -2030, -1825, -365, -30]),
    Obj("2024 PT5", "DES=2024 PT5;", "TCO",
        dt_days=[-418, -365, -300, -30]),

    # Impactors
    Obj("2008 TC3", "DES=2008 TC3;", "Impactor"),
    Obj("2023 CX1", "DES=2023 CX1;", "Impactor"),
    Obj("2024 BX1", "DES=2024 BX1;", "Impactor"),
    Obj("2014 AA", "DES=2014 AA;", "Impactor"),
    Obj("2018 LA", "DES=2018 LA;", "Impactor"),

    # Additional MBAs
    Obj("Parijskij", "5303;", "MBA"),
]


# ── SBDB non-grav parameter fetch ───────────────────────


def fetch_sbdb_nongrav(horizons_command: str):
    """Fetch non-gravitational parameters from JPL SBDB API.

    Returns (a1, a2, a3, epoch_mjd) or None if not available.
    A1/A2/A3 are converted from AU/day^2 as provided by SBDB.
    """
    # Extract designation from Horizons command
    cmd = horizons_command.strip().rstrip(";")
    if cmd.startswith("DES="):
        des = cmd[4:].split(";")[0]
    else:
        des = cmd

    url = f"https://ssd-api.jpl.nasa.gov/sbdb.api?sstr={urllib.parse.quote(des)}&phys-par=true&cov=true"
    try:
        with urllib.request.urlopen(url, timeout=15) as resp:
            data = json.loads(resp.read())
    except Exception:
        return None

    orbit = data.get("orbit", {})
    elements = orbit.get("elements", [])

    # Extract non-grav params
    params = {}
    for elem in elements:
        if elem.get("name") in ("A1", "A2", "A3"):
            params[elem["name"]] = float(elem["value"])

    if not params:
        return None

    # Get epoch
    epoch_jd = None
    for elem in elements:
        if elem.get("name") == "epoch":
            epoch_jd = float(elem["value"])
            break

    epoch_mjd = epoch_jd - MJD_TO_JD if epoch_jd else None

    return {
        "a1": params.get("A1", 0.0),
        "a2": params.get("A2", 0.0),
        "a3": params.get("A3", 0.0),
        "epoch_mjd": epoch_mjd,
    }


# ── Main ────────────────────────────────────────────────


def fmt_km(km):
    if km < 0.001:
        return f"{km * 1e6:>8.1f} mm"
    elif km < 1.0:
        return f"{km * 1000:>8.1f}  m"
    elif km < 1000:
        return f"{km:>8.3f} km"
    else:
        return f"{km:>8.1f} km"


def main():
    parser = argparse.ArgumentParser(
        description="Run ASSIST propagation from empyrean validation data"
    )
    parser.add_argument(
        "validation_json", type=str,
        help="Path to empyrean validation results JSON (from empyrean validate)",
    )
    parser.add_argument(
        "--horizons-cache", type=str, default=None,
        help="Path to empyrean Horizons cache directory. "
             "Default: ~/.empyrean/cache/horizons",
    )
    parser.add_argument(
        "--data-dir", type=str, default=None,
        help="Path to data directory with ASSIST ephemeris files. "
             "Default: ~/.empyrean/data",
    )
    parser.add_argument(
        "--output", "-o", type=str, default="assist_results.json",
        help="Output JSON file path",
    )
    parser.add_argument(
        "--n-timing-runs", type=int, default=3,
        help="Number of timing runs (reports best-of-N)",
    )
    parser.add_argument(
        "--prev-timings", type=str, default=None,
        help="Prior ASSIST results JSON. Its single-particle (f64_detection_on) "
             "assist_time_ms per (object, dt) seeds the per-row wall-clock cap "
             "(cap = max(floor, factor x previous single-particle time)). When "
             "absent, every row's cap is the floor.",
    )
    parser.add_argument(
        "--walltime-cap-floor", type=float, default=10.0,
        help="Minimum per-row wall-clock cap in seconds (default 10).",
    )
    parser.add_argument(
        "--walltime-cap-factor", type=float, default=100.0,
        help="Per-row cap = max(floor, factor x the row's previous "
             "single-particle time) (default 100).",
    )
    parser.add_argument(
        "--print-defaults", action="store_true",
        help="Print the resolved rebound/assist integrator defaults as JSON and "
             "exit without propagating (dry run).",
    )
    args = parser.parse_args()

    if args.print_defaults:
        # Read every configuration's settings back off a probe simulation AFTER
        # assist attach (a planets-only ephem is enough to attach and read).
        dd = Path(args.data_dir) if args.data_dir else Path.home() / ".empyrean" / "data"
        probe_ephem = assist.Ephem(planets_path=str(dd / "linux_p1550p2650.440"))
        for c in ASSIST_CONFIGS:
            print(json.dumps(resolved_config(c, probe_ephem)))
        return

    # Load empyrean validation results to get objects, epochs, and dt_days
    with open(args.validation_json) as f:
        emp_results = json.load(f)

    # Data directory for ASSIST ephemeris
    data_dir = Path(args.data_dir) if args.data_dir else Path.home() / ".empyrean" / "data"
    planets_path = data_dir / "linux_p1550p2650.440"
    asteroids_path = data_dir / "sb441-n16.bsp"

    if not planets_path.exists():
        print(f"ERROR: ASSIST planets file not found: {planets_path}")
        print("Run: ./setup.sh")
        sys.exit(1)

    ephem_kw = {"planets_path": str(planets_path)}
    if asteroids_path.exists():
        ephem_kw["asteroids_path"] = str(asteroids_path)
    ephem = assist.Ephem(**ephem_kw)
    # Self-perturber objects (SB441-N16 members: Iris/Vesta/Pallas/Hygiea)
    # CANNOT be propagated with the asteroid-perturber set loaded: ASSIST has
    # no per-body exclusion, so the test particle starts on top of its own
    # 1/r² singularity and is ejected (~7e8 km error within 30 days,
    # measured). Empyrean excludes only the self body; ASSIST's closest
    # honest equivalent is a planets-only force model for these objects —
    # their residual therefore retains the other-15-asteroid signal, which
    # is stated, not hidden.
    ephem_planets_only = assist.Ephem(planets_path=str(planets_path))

    # Horizons cache (read empyrean's cache directly)
    cache_dir = Path(args.horizons_cache) if args.horizons_cache else Path.home() / ".empyrean" / "cache" / "horizons_assist"
    cache = HorizonsCache(cache_dir)

    print(f"ASSIST {assist.__version__} + rebound {rebound.__version__}")
    print(f"Data: {data_dir}")
    print(f"Horizons cache: {cache_dir}")

    # Group empyrean results by object to get epoch + dt_days
    from collections import defaultdict
    obj_data = defaultdict(lambda: {
        "epoch": None, "population": "", "notes": "", "dt_days": [],
        "a1": 0.0, "a2": 0.0, "a3": 0.0,
        "gr_alpha": None, "gr_r0": None, "gr_m": None, "gr_n": None, "gr_k": None,
    })
    for r in emp_results:
        if r.get("test_type") != "propagation":
            continue
        name = r["object"]
        obj_data[name]["epoch"] = r["epoch_mjd_tdb"]
        obj_data[name]["population"] = r["population"]
        obj_data[name]["notes"] = r.get("notes", "")
        obj_data[name]["dt_days"].append(r["dt_days"])
        # Pick up non-grav params from empyrean results. The schema uses
        # `ic_*` keys (set by the rust runner from SBDB) — older versions
        # of this runner read `non_grav_*` which never matched anything,
        # silently dropping all Yarkovsky / cometary outgassing terms and
        # producing a quietly-wrong "no non-grav" comparison.
        if r.get("ic_a1") is not None:
            obj_data[name]["a1"] = r["ic_a1"] or 0.0
            obj_data[name]["a2"] = r.get("ic_a2") or 0.0
            obj_data[name]["a3"] = r.get("ic_a3") or 0.0
        # g(r) function parameters (Marsden water-ice form for comets;
        # asteroids carry the inverse-square defaults that ASSIST uses
        # natively, so we only override when the row supplies an explicit
        # alpha).
        if r.get("ic_g_alpha"):
            obj_data[name]["gr_alpha"] = r["ic_g_alpha"]
            obj_data[name]["gr_r0"] = r.get("ic_g_r0")
            obj_data[name]["gr_m"] = r.get("ic_g_m")
            obj_data[name]["gr_n"] = r.get("ic_g_n")
            obj_data[name]["gr_k"] = r.get("ic_g_k")

    # Build Horizons command mapping from ALL_OBJECTS
    cmd_map = {o.name: o for o in ALL_OBJECTS}

    print(f"Objects: {len(obj_data)}")

    timestamp = datetime.now(timezone.utc).isoformat()
    # Provenance: the ASSIST channel exercises both assist and its REBOUND
    # integrator backend, so stamp both versions. Both modules are guaranteed
    # imported (the top-of-file import guard exits otherwise), so no fallback
    # is needed here.
    source_version = f"assist {assist.__version__} + rebound {rebound.__version__}"
    # Resolved settings per configuration, read back off a probe simulation
    # AFTER attach + config, stamped onto every row of that configuration so the
    # numbers say what they were measured under (never assumed values).
    config_prov = {c: resolved_config(c, ephem) for c in ASSIST_CONFIGS}
    results = []
    n_runs = args.n_timing_runs

    # Per-row wall-clock caps seeded from a prior run's single-particle timings.
    # Map (object, round(dt)) -> previous f64_detection_on assist_time_ms.
    prev_ms = {}
    if args.prev_timings:
        try:
            with open(args.prev_timings) as pf:
                for r in json.load(pf):
                    if r.get("propagation_uncertainty") == "f64_detection_on" \
                            and r.get("assist_time_ms") is not None:
                        prev_ms[(r["object"], round(r["dt_days"]))] = float(r["assist_time_ms"])
            print(f"Wall-clock caps seeded from {len(prev_ms)} prior single-particle timings "
                  f"in {args.prev_timings}")
        except (OSError, ValueError, KeyError) as e:
            print(f"WARNING: could not read --prev-timings {args.prev_timings}: {e}; "
                  f"every cap falls back to the {args.walltime_cap_floor:.0f}s floor")

    def cap_for(obj, dt):
        pm = prev_ms.get((obj, round(dt)))
        if pm is None:
            return args.walltime_cap_floor
        return max(args.walltime_cap_floor, args.walltime_cap_factor * pm / 1000.0)

    print(f"\n{'=' * 80}")
    print("  ASSIST Propagation — six arms (three configurations x variational off/on)")
    for c in ASSIST_CONFIGS:
        print(f"  {c}: {json.dumps(config_prov[c])}")
    print(f"{'=' * 80}\n")
    n_capped = 0
    capped_rows = []
    capped_by_arm = {arm: 0 for _, _, arm in ASSIST_ARMS}

    for name, data in sorted(obj_data.items()):
        epoch = data["epoch"]
        dt_days = sorted(set(data["dt_days"]))
        obj_def = cmd_map.get(name)

        if obj_def is None:
            print(f"{name}: SKIP (not in ASSIST object catalog)")
            continue

        # Self-perturbers: planets-only forces (see ephem_planets_only note).
        if data["population"] == "Self-Perturber":
            obj_ephem = ephem_planets_only
            print(f"{name}: self-perturber — ASSIST runs planets-only "
                  "(no per-body exclusion; asteroid set would include the object itself)")
        else:
            obj_ephem = ephem

        print(f"{name} ({data['population']})")

        # Get initial conditions from Horizons (with cache)
        try:
            cached = query_horizons(cache, obj_def.horizons_command, epoch)
        except Exception as e:
            print(f"  SKIP (Horizons IC fetch failed: {e})")
            continue

        pos0, vel0 = cached

        # Non-grav params from empyrean validation results
        a1 = data["a1"]
        a2 = data["a2"]
        a3 = data["a3"]
        gr_alpha = data["gr_alpha"]
        gr_r0 = data["gr_r0"]
        gr_nk = data["gr_k"]
        gr_nm = data["gr_m"]
        gr_nn = data["gr_n"]
        has_ng = a1 != 0.0 or a2 != 0.0 or a3 != 0.0

        ng_str = ""
        if has_ng:
            ng_str = f" [A1={a1:.2e}, A2={a2:.2e}, A3={a3:.2e}]"
        print(f"  epoch={epoch:.1f}{ng_str}")

        for dt in dt_days:
            target = epoch + dt

            # Get Horizons reference at target (with cache)
            try:
                hor_pos, hor_vel = query_horizons(cache, obj_def.horizons_command, target)
            except Exception as e:
                print(f"  dt={dt:>+.0f}d SKIP (Horizons ref: {e})")
                continue

            # ASSIST propagation over the six arms: three configurations
            # (assist_default / assist_layup / assist_asteroid_institute), each
            # with variational particles off (single-particle f64) and on (six
            # first-order variational particles, a 6x6 STM).
            # No STT mode: ASSIST encodes first-order variational derivatives
            # only, so empyrean's second-order (Jet2) rows have no ASSIST
            # counterpart. The variational equations propagate under gravity
            # only (ASSIST's non-grav `additional_forces` is not seen by the
            # shadows), so for active bodies the STM is a gravity-only
            # approximation of empyrean's full-Jacobian outputs.
            for config, with_stm, arm in ASSIST_ARMS:
                # Per-row wall-clock cap, seeded from the stored single-particle
                # timing for this (object, dt). Applied only to the untimed
                # warm-up call; the timed best-of-N runs are uncapped so the
                # heartbeat's per-step cost never enters the reported time.
                cap = cap_for(name, dt)
                try:
                    # warmup (carries the wall-clock cap)
                    propagate_assist(
                        pos0, vel0, epoch, target, obj_ephem,
                        a1=a1, a2=a2, a3=a3,
                        gr_alpha=gr_alpha, gr_nk=gr_nk, gr_nm=gr_nm,
                        gr_nn=gr_nn, gr_r0=gr_r0,
                        with_stm=with_stm, config=config,
                        walltime_cap_s=cap,
                    )

                    ast_times = []
                    ast_call_times = []
                    ast_pos = ast_vel = None
                    ast_stm = None
                    for _ in range(n_runs):
                        ast_pos, ast_vel, ms, call_ms, ast_stm = propagate_assist(
                            pos0, vel0, epoch, target, obj_ephem,
                            a1=a1, a2=a2, a3=a3,
                            gr_alpha=gr_alpha, gr_nk=gr_nk, gr_nm=gr_nm,
                            gr_nn=gr_nn, gr_r0=gr_r0,
                            with_stm=with_stm, config=config,
                        )
                        ast_times.append(ms)
                        ast_call_times.append(call_ms)

                    ast_ms = min(ast_times)
                    ast_call_ms = min(ast_call_times)
                    ast_vs_hor = float(np.linalg.norm(ast_pos - hor_pos) * AU_KM)

                    print(
                        f"  dt={dt:>+6.0f}d  {arm:<44} a-h={fmt_km(ast_vs_hor)}  {ast_ms:>7.1f}ms"
                    )

                    row = {
                        "object": name,
                        "population": data["population"],
                        "epoch_mjd_tdb": epoch,
                        "dt_days": dt,
                        "t_mjd_tdb": target,
                        "force_model": "full",
                        "test_type": "assist",
                        "propagation_uncertainty": arm,
                        "assist_config": config,
                        "variational": with_stm,
                        "assist_vs_horizons_km": ast_vs_hor,
                        "assist_time_ms": ast_ms,
                        "assist_call_time_ms": ast_call_ms,
                        "assist_pos_au": ast_pos.tolist(),
                        "horizons_pos_au": hor_pos.tolist(),
                        "has_nongrav": has_ng,
                        "a1": a1,
                        "a2": a2,
                        "a3": a3,
                        "timestamp": timestamp,
                        "notes": data["notes"],
                        "source_version": source_version,
                        "assist_version": assist.__version__,
                        "rebound_version": rebound.__version__,
                        "assist_config_settings": config_prov[config],
                    }
                    if ast_stm is not None:
                        row["assist_stm"] = ast_stm.tolist()
                    results.append(row)

                except WalltimeExceeded as e:
                    # Loud FAIL row: reason + elapsed, numeric fields null, never
                    # a silent skip and never a substituted number.
                    n_capped += 1
                    capped_by_arm[arm] += 1
                    capped_rows.append((name, dt, arm, e.elapsed_s, e.cap_s))
                    print(
                        f"  dt={dt:>+6.0f}d  {arm:<44} ASSIST FAIL — {e}"
                    )
                    results.append({
                        "object": name,
                        "population": data["population"],
                        "epoch_mjd_tdb": epoch,
                        "dt_days": dt,
                        "t_mjd_tdb": target,
                        "force_model": "full",
                        "test_type": "assist",
                        "propagation_uncertainty": arm,
                        "assist_config": config,
                        "variational": with_stm,
                        "status": "FAIL",
                        "assist_vs_horizons_km": None,
                        "assist_time_ms": None,
                        "assist_call_time_ms": None,
                        "assist_pos_au": None,
                        "horizons_pos_au": hor_pos.tolist(),
                        "has_nongrav": has_ng,
                        "a1": a1,
                        "a2": a2,
                        "a3": a3,
                        "timestamp": timestamp,
                        "notes": (
                            f"WALL-CLOCK CAP: {e}. "
                            f"cap={e.cap_s:.1f}s elapsed={e.elapsed_s:.1f}s. {data['notes']}"
                        ).strip(),
                        "source_version": source_version,
                        "assist_version": assist.__version__,
                        "rebound_version": rebound.__version__,
                        "assist_config_settings": config_prov[config],
                        "assist_fail_reason": "walltime_cap_exceeded",
                        "assist_fail_elapsed_s": e.elapsed_s,
                        "assist_fail_cap_s": e.cap_s,
                    })

                except Exception as e:
                    print(f"  dt={dt:>+6.0f}d  {arm:<44} ASSIST FAIL ({e})")

    # Save results
    output_path = Path(args.output)
    output_path.parent.mkdir(parents=True, exist_ok=True)
    with open(output_path, "w") as f:
        try:
            _payload = json.dumps(results, indent=2, default=str, allow_nan=False)
        except ValueError as _e:
            print(
                f"ERROR: refusing to write non-finite values to {args.output}: {_e}\n"
                "       Bare NaN/Infinity is invalid JSON — Rust's serde_json rejects it, so this\n"
                "       whole channel would fail the reduce merge with a line number and no cause.\n"
                "       A quantity that could not be computed must be null.",
                file=sys.stderr,
            )
            raise
        f.write(_payload)

    print(f"\n{'=' * 80}")
    print(f"  Results saved to {output_path}")
    print(f"  {len(results)} test cases")
    for c in ASSIST_CONFIGS:
        print(f"  config {c}: {json.dumps(config_prov[c])}")
    print(f"  Horizons cache: {cache.hits} hits")
    print(f"  WALL-CLOCK CAP FAILURES: {n_capped} row(s) total")
    for arm, n in capped_by_arm.items():
        print(f"    {arm:<44} {n} capped")
    if n_capped:
        for obj, dt, arm, el, cp in capped_rows:
            print(f"    {obj:<14} dt={dt:>+6.0f}d  {arm:<44} elapsed={el:.1f}s cap={cp:.1f}s")
    print(f"{'=' * 80}")


if __name__ == "__main__":
    main()
