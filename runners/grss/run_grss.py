"""GRSS external-reference runner for the empyrean validation suite.

Replays the canonical test plan (`results/validation_plan.json`) through
**GRSS** — the Gauss-Radau Small-body Simulator (Makadia et al.,
github.com/rahil-makadia/grss): a C++ small-body propagation / orbit-
determination core behind a Python interface. GPL-3.0, so — like ASSIST,
jorbit and OrbFit — it is never linked into empyrean: `setup.sh` installs it
from PyPI into this directory's `.venv` and it is driven only as a subprocess.

GRSS covers all three axes the suite measures, and it is one of only two
external references here that ingests **radar** astrometry — so it turns radar
OD from a single-witness find_orb comparison into a real cross-check.

    propagation                replay the plan's IC to `t_mjd_tdb`
    ephemeris                  + observed RA/Dec/range for `observer`
    orbit_determination        fit `fixtures/psv/<object>.psv`
    orbit_determination_radar  fit `fixtures/psv-radar/<object>.psv`
                               (optical table + ADES `<radar>` table)

Two invocations, mirroring the find_orb runner: the default pass is plan-driven
and covers propagation / ephemeris / optical OD; `--radar` walks the
`fixtures/psv-radar/` directory instead and emits `orbit_determination_radar`
rows. The radar rows cannot come from the plan — `strip-plan` removes them
(`plan::PLAN_RUST_ONLY_TEST_TYPES`, empyrean-s1ab) — so the radar pass is
driven by the fixtures and seeded from the plan, exactly as find_orb's is
driven by the fixture directory.

Fields emitted (all `None` when not computed — never 0, never a copied input):

    grss_pos_au                propagated / fitted position (AU, ICRF, SSB)
    grss_vel_au_d              ditto velocity (AU/day)
    grss_vs_horizons_km        |grss - Horizons|            propagation rows
    grss_separation_arcsec     angular sep vs Horizons       ephemeris rows
    grss_d_ra_arcsec           dRA·cos(Dec) vs Horizons      ephemeris rows
    grss_d_dec_arcsec          dDec vs Horizons              ephemeris rows
    grss_d_rho_km              range diff vs Horizons        ephemeris rows
    grss_time_ms               wall clock for this row
    grss_epoch_mjd_tdb         fit epoch (MJD TDB)                  OD rows
    grss_rms_ra_arcsec         post-fit RA·cos(Dec) residual RMS    OD rows
    grss_rms_dec_arcsec        post-fit Dec residual RMS            OD rows
    grss_rms_arcsec            combined RA·cosδ+Dec RMS (find_orb / OrbFit
                               convention, comparable to
                               `od_rms_combined_arcsec`)            OD rows
    grss_weighted_rms          dimensionless weighted RMS ≈ √(χ²/n)  OD rows
    grss_chi2 / grss_reduced_chi2                                   OD rows
    grss_converged             LSQ reached GRSS's convergence test  OD rows
    grss_iterations            LSQ iterations run                   OD rows
    grss_n_obs_used            accepted OPTICAL observations        OD rows
    grss_n_obs_rejected        auto+force-rejected optical          OD rows
    grss_n_obs_unsupported     observations GRSS cannot ingest at
                               all (star catalog outside its ADES
                               table), excluded before the fit     OD rows
    grss_rms_delay_us          post-fit delay residual RMS (µs)   radar rows
    grss_rms_doppler_hz        post-fit Doppler residual RMS (Hz) radar rows
    grss_n_delay_used          accepted delay measurements       radar rows
    grss_n_doppler_used        accepted Doppler measurements     radar rows
    grss_sigma_pos_km          1σ position from the fitted 6×6      OD rows
    grss_model_note            a known, quantified difference between GRSS's
                               force model and the plan's — so no row can be
                               read in isolation as like-for-like
    grss_error                 why this row produced nothing

The radar residual columns are new to the suite: no other channel reports a
delay/Doppler post-fit RMS, and `grss_n_delay_used` / `grss_n_doppler_used`
pair directly with JPL's own `ref_od_n_del_obs_used` / `ref_od_n_dop_obs_used`
from the SBDB merge.

── What this runner does NOT hide ────────────────────────────────────────────

Every one of these is a real property of the comparison, stamped or logged
rather than smoothed over:

* **Planetary ephemeris.** GRSS's `PropSimulation` takes a DE kernel case.
  `--de-kernel 440` (default) is DE440 + the short-span SB441-N16, 1550-2650,
  ~235 MB; `--de-kernel 441` is DE441 + the long-span SB441-N16 and needs a
  4.4 GB download from `setup.sh`. The validation grid lies entirely inside
  DE440's span. Whichever is used is stamped into every row's
  `source_version`, and the required kernels are verified present and
  non-empty before a single row is computed.

* **Non-gravitational forces are the plan's, held fixed.** The Marsden
  A1/A2/A3 + g(r) parameters on the plan row are passed to GRSS verbatim, for
  propagation and as `nongrav_info` for the fit. GRSS *can* solve for
  A1/A2/A3, and does not here: the plan's `orbit_determination` axis is a
  6-parameter state fit, and solving non-grav would be answering a different
  question (that is the `non_grav_recovery` axis). Consequence, stated because
  it is visible in the numbers: an object whose plan IC carries A1=A2=A3=0
  while JPL's own solution fits a Yarkovsky term will show a real trend in its
  radar delay residuals. Bennu is the worked example.

* **The non-grav time delay DT cannot be modelled at all.** GRSS's
  `NongravParameters` exposes A1/A2/A3 + alpha/r0/m/n/k and no DT term, so the
  plan's `ic_non_grav_dt` has nowhere to go. Five catalog objects carry one —
  67P (+45.689 d), 103P/Hartley 2 (+12.234), 46P/Wirtanen (−14.148),
  2I/Borisov (−65.130), 3I/ATLAS (+9.479) — and their offsets against Horizons
  therefore include a model difference, measured at 1377 / 3287 / 24036 /
  4668 km (propagation p50) against 4 m for the rest of the catalog. Every
  affected row carries `grss_model_note` saying so, the object is logged once,
  and the executive summary re-states all of them; without that the four
  orders of magnitude would read as an unexplained numerical outlier.

* **Self-perturbers.** GRSS's default body set is the Sun, planets, Moon,
  Pluto and the SB441-N16 asteroids — four of which the suite propagates AS
  test objects (Vesta, Pallas, Hygiea, Iris). Left alone, the object would
  perturb itself through a 1/r² singularity: measured 1.0e12 km at ±15 yr for
  Vesta. GRSS has per-body removal, so the offending perturber is dropped
  with `remove_body` and the removal is logged per row. The removal set is
  the union of the plan's own `excluded_perturbers_naif` (authoritative,
  matched by SPICE id) and any default perturber whose name equals the object
  name (the plan populates the field on OD rows only).

* **Radar bounce point.** ADES `com=0` records are surface-bounce
  measurements; correcting them needs the body radius, which the plan does
  not carry, so GRSS integrates with `radius=0` (centre-of-mass geometry).
  Reported per object on stderr and on the row in `grss_model_note`. Only
  Eros's fixture has `com=0` records (2 of 6), and its delay residual RMS is
  correspondingly the set's largest at 11.95 µs.

* **Uncertainty axis.** Plan rows are emitted once per (object, test type,
  dt, observer): the `first_order_with_cov` duplicates are skipped, because
  GRSS propagates state only here and a covariance-mode row would be the same
  computation twice. `merge-external` keys GRSS's propagation / ephemeris
  rows without the uncertainty tag, so one GRSS row attaches to both empyrean
  modes — the same convention the OpenOrb runner uses.

* **The OD fits are seeded refits, not blind IODs.** GRSS's LSQ needs an
  initial orbit; it is the plan's IC for that object, propagated by GRSS to
  the plan's OD epoch. Same shape as the OrbFit runner (which seeds neofit2.x
  from empyrean's IC) and unlike find_orb / layup, which run their own initial
  orbit determination.

* **Analytic partials.** GRSS can build the measurement partials from its
  propagated state-transition matrix instead of by finite-differencing 12
  extra trajectories. That is the default here (measured: 2.7 s vs 8.3 s on
  Apophis optical+radar, results equal to 6 significant figures);
  `--numeric-partials` selects the finite-difference path.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import statistics
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

import numpy as np
import pandas as pd

_AU_KM = 149_597_870.700
_C_AU_PER_DAY = 173.144632674240

# Test types this runner replays from the plan. The narrower recovery axes
# (non_grav_recovery / dt_recovery / photometry_recovery / thrust_recovery) are
# deliberately absent: they ask for fitted non-grav / DT / H-G / thrust
# parameters, and this runner holds all of those fixed (see the module
# docstring). A row emitted for one of them would carry a state-only fit under
# a name that promises something else.
_PLAN_TEST_TYPES = ("propagation", "ephemeris", "orbit_determination")

# SPICE kernels GRSS opens, by `--de-kernel` case. setup.sh fetches exactly
# these; a fit or propagation against a missing or truncated kernel is a hard
# stop, not a degraded run.
_KERNELS_COMMON = (
    "earth_latest.bpc",
    "earth_historic.bpc",
    "earth_predict.bpc",
    "moon_pa_de440.bpc",
    "pck00011.tpc",
)
_KERNELS_BY_DE = {
    440: ("de440.bsp", "sb441-n16s.bsp"),
    441: ("de441.bsp", "sb441-n16.bsp"),
}


# ── grss import ─────────────────────────────────────────────


def _import_grss():
    """Import `grss`, with its import-time kernel download suppressed.

    `grss.utils.initialize()` runs on import and shells out to the package's
    own `get_kernels.py`, which pulls the FULL 4.4 GB kernel set — including
    DE441 — regardless of what this run needs. `setup.sh` has already fetched
    exactly the kernels for the requested `--de-kernel` case and verified
    their sizes, so the shell-outs are blocked here and what they would have
    provided is asserted instead by `_check_kernels`.

    The block is scoped to the import and reported, so a future grss release
    that shells out for something else shows up in the log rather than
    silently not happening.
    """
    real_system = os.system
    blocked: list[str] = []

    def _guard(cmd: str) -> int:
        blocked.append(cmd)
        return 0

    os.system = _guard
    try:
        import grss
    finally:
        os.system = real_system

    for cmd in blocked:
        script = cmd.split()[-1] if cmd.split() else cmd
        print(
            f"  note: suppressed grss's import-time shell-out to {script} "
            "(setup.sh owns the kernel/debias fetch)",
            file=sys.stderr,
        )
    return grss


def _check_kernels(kernel_dir: Path, de_kernel: int) -> None:
    """Fail loudly when a kernel GRSS will open is absent or empty.

    GRSS opens its kernels lazily, so a missing DE file surfaces as a
    mid-integration SPICE error on some later row, or — worse — as a state
    that is merely wrong. Check up front and name the fix.
    """
    required = list(_KERNELS_COMMON) + list(_KERNELS_BY_DE[de_kernel])
    missing = []
    for name in required:
        p = kernel_dir / name
        if not p.is_file():
            missing.append(f"{name} (absent)")
        elif p.stat().st_size == 0:
            missing.append(f"{name} (zero bytes)")
    if missing:
        print(
            f"ERROR: GRSS kernel directory {kernel_dir} is not usable for "
            f"--de-kernel {de_kernel}:\n"
            + "".join(f"       - {m}\n" for m in missing)
            + "       Run runners/grss/setup.sh"
            + (f" with GRSS_DE_KERNEL={de_kernel}" if de_kernel != 440 else "")
            + " to fetch and verify them.\n"
            "       Propagating against a missing kernel yields a wrong state, "
            "not an error, so this is a hard stop.",
            file=sys.stderr,
        )
        raise SystemExit(1)


# ── provenance ──────────────────────────────────────────────


def _source_version(grss, de_kernel: int, analytic: bool) -> str:
    """Provenance string stamped on every grss-channel row.

    Carries the DE kernel case and the partials mode as well as the version,
    because both change the numbers: they are properties of the comparison,
    not of the installation.
    """
    try:
        ver = grss.__version__
    except Exception as e:  # noqa: BLE001
        ver = f"unknown ({e})"
    partials = "STM partials" if analytic else "finite-difference partials"
    return f"grss {ver} (DE{de_kernel}+SB441-N16, {partials})"


def _angular_sep_arcsec(ra1: float, dec1: float, ra2: float, dec2: float) -> float:
    """Vincenty great-circle separation of two (RA, Dec) in radians → arcsec."""
    cd1, cd2 = math.cos(dec1), math.cos(dec2)
    sd1, sd2 = math.sin(dec1), math.sin(dec2)
    dra = ra2 - ra1
    n1 = cd2 * math.sin(dra)
    n2 = cd1 * sd2 - sd1 * cd2 * math.cos(dra)
    num = math.sqrt(n1 * n1 + n2 * n2)
    den = sd1 * sd2 + cd1 * cd2 * math.cos(dra)
    return math.degrees(math.atan2(num, den)) * 3600.0


# ── force model ─────────────────────────────────────────────


def _nongrav_dict(row: dict) -> dict:
    """The plan row's Marsden parameters in GRSS's `nongrav_info` shape.

    GRSS's defaults for an un-fitted body are A1=A2=A3=0, alpha=1, r0=1 AU,
    m=2, n=0, k=0 — identical to the plan's defaults for an asteroid, so a row
    with no non-grav data maps to a pure-gravity force model either way.
    """
    return {
        "a1": row.get("ic_a1") or 0.0,
        "a2": row.get("ic_a2") or 0.0,
        "a3": row.get("ic_a3") or 0.0,
        "alpha": row.get("ic_g_alpha") or 1.0,
        "r0_au": row.get("ic_g_r0") or 1.0,
        "m": row.get("ic_g_m") or 2.0,
        "n": row.get("ic_g_n") or 0.0,
        "k": row.get("ic_g_k") or 0.0,
    }


def _nongrav_params(libgrss, row: dict):
    """`libgrss.NongravParameters` built from the plan row."""
    ng = libgrss.NongravParameters()
    d = _nongrav_dict(row)
    ng.a1, ng.a2, ng.a3 = d["a1"], d["a2"], d["a3"]
    ng.alpha, ng.r0_au = d["alpha"], d["r0_au"]
    ng.m, ng.n, ng.k = d["m"], d["n"], d["k"]
    return ng


def _model_note(row: dict) -> str | None:
    """A known, quantified difference between GRSS's force model and the plan's.

    Right now there is exactly one, and it is not optional to report: GRSS
    4.5.7's `NongravParameters` exposes A1/A2/A3 + alpha/r0/m/n/k and **no
    time-delay term**, so the plan's Marsden `ic_non_grav_dt` cannot be applied.
    Five catalog objects carry one — 67P (+45.689 d), 103P/Hartley 2 (+12.234),
    46P/Wirtanen (−14.148), 2I/Borisov (−65.130), 3I/ATLAS (+9.479) — and for
    those the GRSS-vs-Horizons offset contains a real model difference on top of
    any numerical disagreement. Measured: their propagation p50 offsets are
    1377 / — / 3287 / 24036 / 4668 km against a 4 m p50 for the rest of the
    catalog. Silently dropping the delay would turn that into an unexplained
    four-orders-of-magnitude outlier in the report.

    Returned as text on the row (`grss_model_note`) rather than as
    `grss_error`: the row IS a valid GRSS measurement, it is just not a
    like-for-like one, and conflating the two would either hide a real number
    or invent a failure.
    """
    dt = row.get("ic_non_grav_dt")
    if dt:
        return (
            f"plan carries a Marsden non-grav time delay DT={dt:+.3f} d; GRSS's "
            "NongravParameters exposes A1/A2/A3 + alpha/r0/m/n/k and no DT term, "
            "so the delay is NOT modelled here and this comparison includes that "
            "model difference"
        )
    return None


class PerturberTrim:
    """Which default GRSS perturbers must be removed, per object.

    GRSS preloads the SB441-N16 asteroids as massive perturbers. When the test
    object IS one of them, leaving it in place makes the body perturb itself —
    a 1/r² singularity, measured at 1.0e12 km over ±15 yr for Vesta. Unlike
    some other runners' upstreams, GRSS exposes `remove_body`, so the fix is
    exact rather than "propagate planets-only".

    The set to remove is the union of two sources, because neither is complete:

    * the plan's `excluded_perturbers_naif` (SPICE ids, e.g. 2000004 = Vesta)
      — authoritative, and exactly what the reference channel excluded, but
      the plan populates it on OD rows only;
    * any default perturber whose name equals the object's — covers the
      propagation and ephemeris rows, where the field is empty.
    """

    def __init__(self, libgrss, kernel_path: str, de_kernel: int):
        probe = libgrss.PropSimulation(
            "perturber_probe", 60000.0, de_kernel, kernel_path
        )
        self._by_id = {b.spiceId: b.name for b in probe.spiceBodies}
        self._by_name = {b.name.lower(): b.name for b in probe.spiceBodies}

    def names_for(self, row: dict) -> list[str]:
        out: dict[str, None] = {}
        for naif in row.get("excluded_perturbers_naif") or []:
            name = self._by_id.get(int(naif))
            if name is not None:
                out[name] = None
        self_name = self._by_name.get(str(row.get("object", "")).lower())
        if self_name is not None:
            out[self_name] = None
        return list(out)


def _new_sim(libgrss, name, t0, de_kernel, kernel_path, trim, row):
    """A `PropSimulation` at `t0` with this object's self-perturbers removed."""
    sim = libgrss.PropSimulation(name, t0, de_kernel, kernel_path)
    removed = trim.names_for(row)
    for body in removed:
        sim.remove_body(body)
    return sim, removed


# ── ADES PSV → GRSS observation frame ───────────────────────
#
# GRSS's own file reader (`create_optical_obs_df`) takes ADES *XML*; its PSV
# entry point (`add_psv_obs`) appends to an existing frame. The suite's
# fixtures are standalone PSV, so the frame is assembled here — and then handed
# to GRSS's OWN debiasing (Eggl et al. 2020), weighting (Vereš et al. 2017) and
# nightly-deweighting functions, so the observation model stays GRSS's rather
# than becoming this runner's. Verified against GRSS's MPC path: for
# `fixtures/psv/Bennu.psv` every column of the assembled frame — times, RA/Dec,
# station, catalog, biases, sigmas — is identical to what
# `create_optical_obs_df('101955')` returns from the MPC's ADES XML.


def _psv_tables(path: Path) -> list[list[str]]:
    """Split an ADES PSV file into its pipe-header-delimited tables.

    The radar-augmented fixtures carry two: the optical table and a `<radar>`
    table (`trx`/`rcv`/`delay`|`doppler`/`rms*`/`com`/`frq`).
    """
    lines = path.read_text(encoding="utf-8").splitlines()
    starts = [i for i, ln in enumerate(lines) if _is_psv_header(ln)]
    return [
        lines[s : (starts[k + 1] if k + 1 < len(starts) else len(lines))]
        for k, s in enumerate(starts)
    ]


# A PSV table header starts with one of ADES's identifier columns. It is NOT
# always `permID`: an unnumbered object has no permanent designation, so its
# fixture leads with `provID` (12 of the 50 optical fixtures do — 2008 TC3,
# 2014 AA, 2018 LA, 2020 CD3, 2023 CX1, 2024 BX1, 2024 PT5, 2026 DA/FO12/FQ12,
# 2005 TN53, 2008 LC18). Matching `permID|` alone silently found no header in
# those files and lost their whole OD row.
_PSV_HEADER_LEAD = ("permID", "provID", "trkSub")


def _is_psv_header(line: str) -> bool:
    """Is this line an ADES PSV pipe-header (rather than a data or comment row)?

    Requires both an identifier lead column and an `obsTime` column, so a data
    row whose first cell happens to read like a column name cannot match.
    """
    if "|" not in line or line.lstrip().startswith(("#", "!")):
        return False
    fields = [c.strip() for c in line.split("|")]
    return fields[0] in _PSV_HEADER_LEAD and "obsTime" in fields


def _psv_frame(table: list[str]) -> pd.DataFrame:
    """One PSV table → DataFrame of stripped strings (blank cells → NaN)."""
    header = [c.strip() for c in table[0].split("|")]
    records = []
    for ln in table[1:]:
        if not ln.strip():
            continue
        cells = [c.strip() for c in ln.split("|")]
        cells += [""] * (len(header) - len(cells))
        records.append(dict(zip(header, cells[: len(header)])))
    return pd.DataFrame(records).replace("", np.nan)


def build_obs_df(
    grss, path: Path, verbose: bool = False
) -> tuple[pd.DataFrame, int, int, int, int]:
    """Assemble a GRSS observation frame from one ADES PSV fixture.

    Returns `(obs_df, n_optical, n_radar, n_bounce_point, n_unsupported_cat)`.

    Order matters and is GRSS's own: debias → weight → deweight on the optical
    table, and only then append the radar rows. `apply_weighting_scheme`
    raises on an unknown observation mode, and `RAD` is unknown to it —
    radar weights come from the fixture's `rmsDelay` / `rmsDoppler`.
    """
    from astropy.time import Time
    from grss.fit.fit_ades import ades_catalog_map, ades_column_types, special_codes
    from grss.fit.fit_optical import (
        _ades_ast_cat_check,
        _ades_mode_check,
        apply_debiasing_scheme,
        apply_weighting_scheme,
        deweight_obs,
    )

    tables = _psv_tables(path)
    if not tables:
        raise ValueError(f"{path} carries no ADES PSV header line (no 'permID|' row)")

    raw = _psv_frame(tables[0])
    keep = [c for c in raw.columns if c in ades_column_types]
    df = raw[keep].copy()
    for col in keep:
        kind = ades_column_types[col]
        if kind == "float":
            df[col] = pd.to_numeric(df[col])
        elif kind == "Int64":
            df[col] = pd.to_numeric(df[col]).astype("Int64")
    # GRSS's own convention for string columns it needs but the file omits.
    # NOT applied to columns the file DOES carry: `prog` must stay NaN rather
    # than becoming the string "nan", because GRSS's station-weight rules
    # unpack any str-valued prog id and abort on an unpackable one.
    for col in ("trx", "rcv", "sys", "selAst"):
        if col not in df:
            df[col] = str(np.nan)
    for col in ades_column_types:
        if col not in df:
            df[col] = np.nan
    df = df[list(ades_column_types.keys())]

    times = Time(df["obsTime"].to_list(), format="isot", scale="utc")
    df["obsTimeMJD"] = times.utc.mjd
    df["obsTimeMJDTDB"] = times.tdb.mjd
    _ades_mode_check(df)
    df = _ades_ast_cat_check(df)

    # Observations GRSS structurally cannot ingest. Two kinds, both fatal to the
    # whole object's fit if left in the frame — GRSS indexes a lookup table
    # unconditionally and raises KeyError partway through:
    #
    #   1. A star catalog outside GRSS's ADES table. `_ades_ast_cat_check` above
    #      has ALREADY ruled these observations out of the fit (it sets
    #      selAst='d', force-rejected) but leaves them in the frame, and both
    #      `apply_debiasing_scheme` and `apply_station_weight_rules` then do
    #      `ades_catalog_map[cat]`. Removing them carries out GRSS's own
    #      decision. Measured over the fixture set: one value, AGK3R, 37
    #      observations across 6 of 50 optical fixtures (Eros, Pallas, Iris,
    #      Vesta, Chiron, 103P/Hartley 2).
    #
    #   2. A space-based / roving / occultation / Gaia observation whose
    #      observer position the fixture does not carry. There is no way to
    #      place such an observer — GRSS does `conv_to_au[sys]` on a missing
    #      `sys` and raises. Measured: only Toutatis, 33 WISE (C51) records per
    #      fixture file with `sys` / `pos1..3` all empty. Dropping them keeps
    #      Toutatis — the set's only bistatic-radar object — in the comparison
    #      instead of losing its entire OD row to a KeyError.
    #
    # Neither is a convenience drop. The count is printed per object with its
    # reason and travels on the row as `grss_n_obs_unsupported`, so a GRSS fit
    # that saw fewer observations than empyrean's or find_orb's says so in the
    # report rather than quietly looking like a cleaner fit.
    cats = df["astCat"].astype(str).str.lower()
    bad_cat = ~cats.isin(ades_catalog_map)
    off_earth = set(special_codes["gaia"]) | set(special_codes["occultation"])
    off_earth |= set(special_codes["spacecraft"]) | set(special_codes["roving"])
    no_position = df["stn"].isin(off_earth) & (
        df["sys"].isna()
        | df["sys"].astype(str).str.lower().isin(["nan", ""])
        | df[["pos1", "pos2", "pos3"]].isna().any(axis=1)
    )
    unsupported_mask = bad_cat | no_position
    n_unsupported = int(unsupported_mask.sum())
    if n_unsupported:
        if bad_cat.any():
            names = sorted({str(c) for c in df.loc[bad_cat, "astCat"]})
            print(
                f"  NOTE: {int(bad_cat.sum())} of {len(df)} optical observations cite a "
                f"star catalog GRSS has no ADES mapping for ({', '.join(names)}); GRSS "
                "force-rejects them.",
                file=sys.stderr,
            )
        if no_position.any():
            stns = sorted({str(s) for s in df.loc[no_position, "stn"]})
            print(
                f"  NOTE: {int(no_position.sum())} of {len(df)} optical observations are "
                f"off-Earth ({', '.join(stns)}) but the fixture carries no observer "
                "position (sys / pos1-3 empty), so GRSS cannot place the observer.",
                file=sys.stderr,
            )
        print(
            f"  NOTE: dropping those {n_unsupported} observation(s) from the frame — "
            "GRSS raises partway\n        through its debiasing / weighting passes "
            "otherwise. Reported as grss_n_obs_unsupported.",
            file=sys.stderr,
        )
        df = df.loc[~unsupported_mask].reset_index(drop=True)

    df["cosDec"] = np.cos(df["dec"] * np.pi / 180)
    df["biasRA"] = 0.0
    df["biasDec"] = 0.0
    df["sigCorr"] = 0.0
    df = apply_debiasing_scheme(df, True, verbose)
    df = apply_weighting_scheme(df, verbose)
    df = deweight_obs(df, 5, verbose)
    n_optical = len(df)

    n_radar = 0
    n_bounce = 0
    if len(tables) > 1:
        radar = _psv_frame(tables[1])
        perm_id = df.iloc[-1]["permID"]
        prov_id = df.iloc[-1]["provID"]
        # The fixture's radar obsTime carries a trailing 'Z'; strip it for the
        # isot parser, which is what GRSS's JPL-API path feeds it too.
        r_times = Time(
            [str(s).rstrip("Z") for s in radar["obsTime"]], format="isot", scale="utc"
        )
        for i, row in radar.iterrows():

            def _f(key, _row=row):
                v = _row.get(key)
                return float(v) if pd.notna(v) else np.nan

            j = len(df)
            df.loc[j, "permID"] = perm_id
            df.loc[j, "provID"] = prov_id
            df.loc[j, "obsTime"] = f"{r_times[i].utc.isot}Z"
            df.loc[j, "obsTimeMJD"] = r_times[i].utc.mjd
            df.loc[j, "obsTimeMJDTDB"] = r_times[i].tdb.mjd
            df.loc[j, "mode"] = "RAD"
            df.loc[j, "trx"] = str(row["trx"])
            df.loc[j, "rcv"] = str(row["rcv"])
            # ADES/GRSS units: delay in SECONDS, rmsDelay in µs, Doppler and
            # rmsDoppler in Hz, frq in MHz. Identical to what GRSS's own
            # `add_radar_obs` stores from the JPL sb_radar API — verified
            # against it on Bennu, where the per-record delay residuals agree
            # to under 1 µs (the remainder being MPC-vs-JPL station
            # coordinates).
            df.loc[j, "delay"] = _f("delay")
            df.loc[j, "rmsDelay"] = _f("rmsDelay")
            df.loc[j, "sigDelay"] = _f("rmsDelay")
            df.loc[j, "doppler"] = _f("doppler")
            df.loc[j, "rmsDoppler"] = _f("rmsDoppler")
            df.loc[j, "sigDoppler"] = _f("rmsDoppler")
            com = int(row["com"]) if pd.notna(row.get("com")) else 1
            df.loc[j, "com"] = com
            n_bounce += com == 0
            df.loc[j, "frq"] = _f("frq")
            df.loc[j, "selAst"] = "A"
            n_radar += 1
        df.sort_values("obsTimeMJD", inplace=True, kind="stable")
        df.reset_index(drop=True, inplace=True)

    return df, n_optical, n_radar, n_bounce, n_unsupported


# ── per-axis replay ─────────────────────────────────────────


def propagate_row(ctx, row: dict) -> dict:
    """Replay one `propagation` plan row. Returns the `grss_*` update."""
    libgrss = ctx["libgrss"]
    ic_pos, ic_vel = row.get("ic_pos_au"), row.get("ic_vel_au_d")
    if not ic_pos or not ic_vel:
        return {"grss_error": "plan row carries no ic_pos_au / ic_vel_au_d"}
    t0 = time.perf_counter()
    sim, removed = _new_sim(
        libgrss,
        str(row["object"]),
        row["epoch_mjd_tdb"],
        ctx["de_kernel"],
        ctx["kernel_path"],
        ctx["trim"],
        row,
    )
    sim.add_integ_body(
        libgrss.IntegBody(
            "target",
            row["epoch_mjd_tdb"],
            0.0,
            0.0,
            list(ic_pos),
            list(ic_vel),
            _nongrav_params(libgrss, row),
        )
    )
    sim.set_integration_parameters(row["t_mjd_tdb"])
    sim.integrate()
    ms = (time.perf_counter() - t0) * 1000.0
    state = list(sim.xInteg[:6])
    if not all(math.isfinite(v) for v in state):
        return {"grss_error": "GRSS integration produced a non-finite state"}
    ctx["removed"].update(removed)
    out = {
        "grss_pos_au": state[:3],
        "grss_vel_au_d": state[3:6],
        "grss_time_ms": ms,
    }
    ref = row.get("ref_pos_au")
    if ref is not None:
        out["grss_vs_horizons_km"] = math.dist(state[:3], list(ref)) * _AU_KM
    return out


def ephemeris_row(ctx, row: dict) -> dict:
    """Replay one `ephemeris` plan row (observed RA/Dec/range for `observer`)."""
    libgrss = ctx["libgrss"]
    ic_pos, ic_vel = row.get("ic_pos_au"), row.get("ic_vel_au_d")
    code = row.get("observer")
    if not ic_pos or not ic_vel:
        return {"grss_error": "plan row carries no ic_pos_au / ic_vel_au_d"}
    if not code:
        return {"grss_error": "ephemeris row carries no observer code"}
    site = ctx["codes"].get(str(code))
    if site is None:
        return {
            "grss_error": (
                f"observatory code {code!r} is not in GRSS's MPC code table "
                "(runners/grss/.venv/.../grss/fit/codes.json); rerun setup.sh "
                "to refresh it"
            )
        }
    lon, lat, rho_site = site
    t0 = time.perf_counter()
    sim, removed = _new_sim(
        libgrss,
        str(row["object"]),
        row["epoch_mjd_tdb"],
        ctx["de_kernel"],
        ctx["kernel_path"],
        ctx["trim"],
        row,
    )
    sim.add_integ_body(
        libgrss.IntegBody(
            "target",
            row["epoch_mjd_tdb"],
            0.0,
            0.0,
            list(ic_pos),
            list(ic_vel),
            _nongrav_params(libgrss, row),
        )
    )
    sim.tEvalMargin = 1.0
    # GRSS rejects a zero-length integration when an evaluation time is
    # requested ("The initial and final times must be different"), which is
    # exactly the plan's dt=0 ephemeris rows. Extend the integration by one day
    # in those cases and still evaluate the measurement AT t_mjd_tdb: the
    # measurement comes from tEval, not from tf. Verified on a dt=+90 row —
    # tf=t and tf=t+1 give bit-identical RA, Dec and light time — so this
    # changes which rows GRSS can answer, not what it answers.
    t_eval = row["t_mjd_tdb"]
    tf = t_eval if t_eval != row["epoch_mjd_tdb"] else t_eval + 1.0
    # tEvalUTC=False — the plan's t_mjd_tdb is TDB. evalApparentState +
    # convergedLightTime give the astrometric (light-time-lagged) direction
    # that the plan's Horizons reference RA/Dec is.
    sim.set_integration_parameters(
        tf,
        [t_eval],
        False,
        True,
        True,
        [[399, lon, lat, rho_site]],
    )
    sim.evalMeasurements = True
    sim.integrate()
    ms = (time.perf_counter() - t0) * 1000.0
    if not sim.opticalObs or not sim.lightTimeEval:
        return {"grss_error": "GRSS produced no optical measurement for this epoch"}
    ctx["removed"].update(removed)
    # opticalObs is [RA, Dec] in arcsec; opticalObsCorr is the additive
    # correction, whose RA component is already scaled by cos(Dec) (see
    # FitSimulation._get_computed_obs).
    obs = sim.opticalObs[0]
    corr = sim.opticalObsCorr[0] if sim.opticalObsCorr else (0.0, 0.0)
    dec_arcsec = obs[1] + corr[1]
    dec_rad = math.radians(dec_arcsec / 3600.0)
    cos_dec = math.cos(dec_rad)
    ra_rad = math.radians((obs[0] + corr[0] / cos_dec) / 3600.0)
    light_time_d = sim.lightTimeEval[0][0]
    rho_au = light_time_d * _C_AU_PER_DAY
    out = {"grss_time_ms": ms}
    ref_ra, ref_dec = row.get("ref_ra_rad"), row.get("ref_dec_rad")
    if ref_ra is not None and ref_dec is not None:
        out["grss_separation_arcsec"] = _angular_sep_arcsec(
            ra_rad, dec_rad, ref_ra, ref_dec
        )
        out["grss_d_ra_arcsec"] = math.degrees((ra_rad - ref_ra) * cos_dec) * 3600.0
        out["grss_d_dec_arcsec"] = math.degrees(dec_rad - ref_dec) * 3600.0
    if row.get("ref_rho_au") is not None:
        out["grss_d_rho_km"] = (rho_au - row["ref_rho_au"]) * _AU_KM
    return out


def _seed_state(ctx, ic_row: dict, t_fit: float) -> list[float]:
    """The plan's IC for this object, propagated by GRSS to the fit epoch."""
    libgrss = ctx["libgrss"]
    sim, _ = _new_sim(
        libgrss,
        f"{ic_row['object']}_seed",
        ic_row["epoch_mjd_tdb"],
        ctx["de_kernel"],
        ctx["kernel_path"],
        ctx["trim"],
        ic_row,
    )
    sim.add_integ_body(
        libgrss.IntegBody(
            "seed",
            ic_row["epoch_mjd_tdb"],
            0.0,
            0.0,
            list(ic_row["ic_pos_au"]),
            list(ic_row["ic_vel_au_d"]),
            _nongrav_params(libgrss, ic_row),
        )
    )
    sim.set_integration_parameters(t_fit)
    sim.integrate()
    return list(sim.xInteg[:6])


def _fit_sim_class(ctx, remove: list[str]):
    """A `FitSimulation` subclass that trims self-perturbers from its own sims.

    `FitSimulation` builds its past/future `PropSimulation` objects internally,
    once per LSQ iteration, so the removal cannot be done from outside — but
    the two factory methods are the only place they are constructed, and
    overriding them covers every iteration and every perturbed trajectory.
    """
    from grss.fit.fit_simulation import FitSimulation

    class _TrimmedFitSimulation(FitSimulation):
        def _get_prop_sim_past(self, *a, **kw):
            sim = super()._get_prop_sim_past(*a, **kw)
            for body in remove:
                sim.remove_body(body)
            return sim

        def _get_prop_sim_future(self, *a, **kw):
            sim = super()._get_prop_sim_future(*a, **kw)
            for body in remove:
                sim.remove_body(body)
            return sim

    return _TrimmedFitSimulation


def _residual_rms(iteration, key: str) -> tuple[int, float | None]:
    """(count, RMS) of one accepted residual column, or (0, None) if empty."""
    vals = np.asarray(iteration.all_info[key], dtype=float)[iteration.accepted_idx]
    vals = vals[~np.isnan(vals)]
    if not len(vals):
        return 0, None
    return len(vals), float(np.sqrt(np.mean(vals**2)))


def fit_object(ctx, obj: str, psv_path: Path, ic_row: dict, t_fit: float) -> dict:
    """Fit one object's astrometry with GRSS. Returns the `grss_*` update."""
    t0 = time.perf_counter()
    obs_df, n_optical, n_radar, n_bounce, n_unsupported = build_obs_df(
        ctx["grss"], psv_path, ctx["verbose"]
    )
    print(f"  {n_optical} optical + {n_radar} radar observations", file=sys.stderr)
    # Force-model caveats that apply to THIS fit, collected so they travel on
    # the row rather than living only in a log line nobody re-reads.
    notes = [n for n in (_model_note(ic_row),) if n]
    if n_bounce:
        notes.append(
            f"{n_bounce} of {n_radar} radar records are surface-bounce (ADES "
            "com=0); the plan carries no body radius, so GRSS fits them with "
            "radius=0 (centre-of-mass geometry) and their delay residuals carry "
            "a bounce-point offset of order 2R/c"
        )
        print(f"  NOTE: {notes[-1]}", file=sys.stderr)

    seed = _seed_state(ctx, ic_row, t_fit)
    x_init = dict(zip(("t", "x", "y", "z", "vx", "vy", "vz"), [t_fit] + seed))
    # Diagonal a-priori on the seed. GRSS uses `cov_init` only to size the
    # solved parameter set (its LSQ is unconstrained unless `prior_est` /
    # `prior_sig` are set, which they are not here), so this is a shape, not a
    # weight on the answer.
    cov_init = np.diag([1e-8, 1e-8, 1e-8, 1e-10, 1e-10, 1e-10])

    remove = ctx["trim"].names_for(ic_row)
    cls = _fit_sim_class(ctx, remove)
    if remove:
        print(
            f"  removed self-perturber(s) from the fit: {', '.join(remove)}",
            file=sys.stderr,
        )
        ctx["removed"].update(remove)

    fs = cls(
        x_init,
        obs_df,
        cov_init=cov_init,
        n_iter_max=ctx["n_iter_max"],
        de_kernel=ctx["de_kernel"],
        nongrav_info=_nongrav_dict(ic_row),
    )
    fs.analytic_partials = ctx["analytic_partials"]
    fs.filter_lsq(verbose=ctx["verbose"])
    ms = (time.perf_counter() - t0) * 1000.0

    note = "; ".join(notes) or None
    if len(fs.iters) < 2:
        return {
            "grss_error": "GRSS LSQ recorded no iteration (see the log above)",
            "grss_time_ms": ms,
            "grss_model_note": note,
        }
    it = fs.iters[-1]
    n_ra, rms_ra = _residual_rms(it, "ra_res")
    n_dec, rms_dec = _residual_rms(it, "dec_res")
    n_del, rms_del = _residual_rms(it, "delay_res")
    n_dop, rms_dop = _residual_rms(it, "doppler_res")
    state = [it.x_nom[k] for k in ("x", "y", "z", "vx", "vy", "vz")]
    out: dict = {
        "grss_pos_au": [float(v) for v in state[:3]],
        "grss_vel_au_d": [float(v) for v in state[3:6]],
        "grss_epoch_mjd_tdb": t_fit,
        "grss_time_ms": ms,
        "grss_converged": bool(fs.converged),
        "grss_iterations": int(fs.n_iter),
        "grss_chi2": float(it.chi_squared),
        "grss_reduced_chi2": float(it.reduced_chi_squared),
        "grss_weighted_rms": float(it.weighted_rms),
        "grss_rms_ra_arcsec": rms_ra,
        "grss_rms_dec_arcsec": rms_dec,
        # Combined RA·cosδ + Dec RMS — the find_orb / OrbFit reporting
        # convention, directly comparable to `od_rms_combined_arcsec`. NOT
        # GRSS's own `unweighted_rms`, which mixes arcsec, µs and Hz into one
        # number whenever radar is in the fit and is therefore not an
        # angular RMS at all.
        "grss_rms_arcsec": (
            float(math.sqrt((rms_ra**2 * n_ra + rms_dec**2 * n_dec) / (n_ra + n_dec)))
            if n_ra + n_dec
            else None
        ),
        "grss_n_obs_used": int(n_ra),
        "grss_n_obs_rejected": int(n_optical - n_ra),
        "grss_n_obs_unsupported": int(n_unsupported),
        "grss_rms_delay_us": rms_del,
        "grss_rms_doppler_hz": rms_dop,
        "grss_n_delay_used": int(n_del) if n_radar else None,
        "grss_n_doppler_used": int(n_dop) if n_radar else None,
    }
    diag = np.diag(np.asarray(it.covariance, dtype=float))
    if np.all(np.isfinite(diag[:3])) and np.all(diag[:3] >= 0):
        out["grss_sigma_pos_km"] = float(math.sqrt(diag[:3].sum()) * _AU_KM)
    if note:
        out["grss_model_note"] = note
    if not fs.converged:
        out["grss_error"] = (
            f"GRSS LSQ did not converge in {fs.n_iter} iteration(s) "
            f"(max {ctx['n_iter_max']}); post-fit values are the last iterate"
        )
    return out


# ── plan-driven pass ────────────────────────────────────────


# Smallest MJD this suite could legitimately fit at (MJD 10000 = 1892-09-16).
# `empyrean-validation plan` writes `t_mjd_tdb: 0.0` on its OD rows — a
# placeholder the rust runner replaces with the arc-weighted fit epoch it
# chooses. Reading that 0.0 as an epoch would fit every object at MJD 0
# (1858-11-17) and produce a converged, entirely meaningless orbit, so a row
# below this floor is treated as carrying no epoch at all.
_MJD_FLOOR = 10_000.0


def _fit_epoch(od_row: dict | None, ic_row: dict) -> tuple[float, str]:
    """The epoch to solve at, and where it came from.

    Prefers the plan OD row's own `t_mjd_tdb` so GRSS's solution lands on the
    same epoch as empyrean's; falls back to the object's IC epoch when the plan
    carries only the placeholder. Both are reported — the value on the row as
    `grss_epoch_mjd_tdb`, the provenance on stderr — because which one was used
    changes what `grss_pos_au` means.
    """
    t = (od_row or {}).get("t_mjd_tdb")
    if t is not None and float(t) >= _MJD_FLOOR:
        return float(t), "plan OD row t_mjd_tdb"
    return (
        float(ic_row["epoch_mjd_tdb"]),
        (
            "plan IC epoch (the plan's OD row carries no usable t_mjd_tdb — "
            "`empyrean-validation plan` writes 0.0 there and the rust runner fills it in)"
        ),
    )


def _ic_rows_by_object(plan: list[dict]) -> dict[str, dict]:
    """First plan row per object that carries an initial condition.

    Every row for an object carries the same IC (same epoch, same state, same
    non-grav parameters), so the first one found is the object's seed. OD rows
    carry none, which is why they are seeded from the object's propagation /
    ephemeris rows — the same lookup the OrbFit runner does.
    """
    out: dict[str, dict] = {}
    for row in plan:
        obj = row.get("object")
        if obj and obj not in out and row.get("ic_pos_au") and row.get("ic_vel_au_d"):
            out[obj] = row
    return out


def _note_once(ctx, obj: str, note: str) -> None:
    """Log a per-object model note the first time it comes up, and remember it.

    Once per object, not once per row: the delay note applies to all 187 of an
    object's rows, and 187 identical lines would bury the other output. The
    executive summary re-states every object noted, so the log cannot scroll it
    out of sight.
    """
    if obj in ctx["model_notes"]:
        return
    ctx["model_notes"][obj] = note
    print(f"  NOTE {obj}: {note}", file=sys.stderr)


def run_plan_pass(ctx, plan: list[dict], fixtures_dir: Path) -> list[dict]:
    """Replay the plan's propagation / ephemeris / optical-OD rows."""
    ic_rows = _ic_rows_by_object(plan)
    rows: list[dict] = []
    n_error = 0
    for row in plan:
        tt = row.get("test_type")
        if tt not in _PLAN_TEST_TYPES:
            continue
        # One GRSS row per (object, test type, dt, observer): see the module
        # docstring on the uncertainty axis.
        if row.get("propagation_uncertainty") not in (None, "f64_no_cov"):
            continue
        out = dict(row)
        out["channel"] = "grss"
        out["timestamp"] = ctx["timestamp"]
        out["source_version"] = ctx["source_version"]
        # Known force-model difference for this object, if any — attached to
        # EVERY affected row (not just once) so no row can be read in isolation
        # as a like-for-like comparison, and logged once per object.
        note = _model_note(row)
        if note:
            out["grss_model_note"] = note
            _note_once(ctx, row["object"], note)
        try:
            if tt == "propagation":
                update = propagate_row(ctx, row)
            elif tt == "ephemeris":
                update = ephemeris_row(ctx, row)
            else:
                obj = row["object"]
                psv = fixtures_dir / f"{_safe_name(obj)}.psv"
                if not psv.is_file():
                    update = {
                        "grss_error": (
                            f"no optical fixture {psv} — the OD fixtures come from "
                            "the GCS snapshot pinned by fixtures/manifest.json; "
                            "`make fixtures` fetches and verifies them"
                        )
                    }
                elif obj not in ic_rows:
                    update = {
                        "grss_error": (
                            "no initial condition in the plan for this object "
                            "(GRSS's LSQ is seeded from the plan's IC, propagated "
                            "to the OD epoch); the plan carries no propagation or "
                            "ephemeris row with ic_pos_au for it"
                        )
                    }
                else:
                    t_fit, source = _fit_epoch(row, ic_rows[obj])
                    print(
                        f"{obj} — optical OD at MJD {t_fit:.5f} TDB ({source})",
                        file=sys.stderr,
                    )
                    update = fit_object(ctx, obj, psv, ic_rows[obj], t_fit)
        except Exception as e:  # noqa: BLE001
            # A row that blew up is a row with a reason, never an omission: an
            # omitted row is indistinguishable from a passing one downstream.
            update = {"grss_error": f"{type(e).__name__}: {e}"}
            print(
                f"  {row.get('object')} {tt} dt={row.get('dt_days')} FAILED: "
                f"{type(e).__name__}: {e}",
                file=sys.stderr,
            )
        if update.get("grss_error"):
            n_error += 1
        out.update(update)
        rows.append(out)
    print(
        f"Replayed {len(rows)} plan rows ({n_error} with grss_error)", file=sys.stderr
    )
    return rows


# ── radar pass ──────────────────────────────────────────────


def _safe_name(obj: str) -> str:
    """Object name → fixture file stem (`2P/Encke` → `2P_Encke`)."""
    return obj.replace("/", "_")


def _unsafe_name(stem: str) -> str:
    """Fixture file stem → object name (`2P_Encke` → `2P/Encke`)."""
    return stem.replace("_", "/")


def run_radar_pass(
    ctx, plan: list[dict], fixtures_dir: Path, objects: set[str] | None
) -> list[dict]:
    """Fit every radar-augmented fixture, one `orbit_determination_radar` row each.

    Driven by the fixture directory rather than the plan because the plan
    carries no radar rows to replay (`plan::PLAN_RUST_ONLY_TEST_TYPES`) — the
    same reason the find_orb radar pass is fixture-driven. The plan is still
    required, for the seed orbit and the object's population / epoch.
    """
    psv_files = sorted(fixtures_dir.glob("*.psv"))
    if not psv_files:
        print(
            f"ERROR: no PSV files found in {fixtures_dir}\n"
            "       GRSS had nothing to fit. The radar-augmented fixtures come from "
            "the GCS snapshot\n"
            "       pinned by fixtures/manifest.json; `make fixtures` fetches and "
            "verifies them.\n"
            "       A directory that exists with zero .psv in it is the DEFAULT state "
            "of a fresh checkout.",
            file=sys.stderr,
        )
        raise SystemExit(1)

    if objects is not None:
        wanted = [p for p in psv_files if _unsafe_name(p.stem).lower() in objects]
        if not wanted:
            print(
                f"ERROR: --objects matched none of the {len(psv_files)} radar fixtures "
                f"in {fixtures_dir}.\n"
                f"       Requested: {', '.join(sorted(objects))}\n"
                f"       Available: {', '.join(_unsafe_name(p.stem) for p in psv_files)}",
                file=sys.stderr,
            )
            raise SystemExit(1)
        psv_files = wanted

    ic_rows = _ic_rows_by_object(plan)
    od_rows = {
        r["object"]: r for r in plan if r.get("test_type") == "orbit_determination"
    }
    force_model = next(
        (r["force_model"] for r in plan if r.get("force_model")), "standard"
    )

    rows: list[dict] = []
    n_fit = 0
    for psv in psv_files:
        obj = _unsafe_name(psv.stem)
        ic = ic_rows.get(obj)
        base = {
            "object": obj,
            "population": (ic or {}).get("population", ""),
            "test_type": "orbit_determination_radar",
            "channel": "grss",
            "dt_days": 0.0,
            "force_model": (ic or {}).get("force_model", force_model),
            "timestamp": ctx["timestamp"],
            "source_version": ctx["source_version"],
        }
        if ic is None:
            msg = (
                "no initial condition in the plan for this object (GRSS's LSQ is "
                "seeded from the plan's IC); the radar fixture cannot be fitted "
                "without it"
            )
            print(f"{obj} — optical+radar OD\n  FAIL: {msg}", file=sys.stderr)
            rows.append(
                {**base, "epoch_mjd_tdb": 0.0, "t_mjd_tdb": 0.0, "grss_error": msg}
            )
            continue
        t_fit, source = _fit_epoch(od_rows.get(obj), ic)
        print(
            f"{obj} — optical+radar OD at MJD {t_fit:.5f} TDB ({source})",
            file=sys.stderr,
        )
        note = _model_note(ic)
        if note:
            base["grss_model_note"] = note
            _note_once(ctx, obj, note)
        base["epoch_mjd_tdb"] = t_fit
        base["t_mjd_tdb"] = t_fit
        try:
            update = fit_object(ctx, obj, psv, ic, t_fit)
        except Exception as e:  # noqa: BLE001
            update = {"grss_error": f"{type(e).__name__}: {e}"}
            print(f"  FAIL: {type(e).__name__}: {e}", file=sys.stderr)
        if not update.get("grss_error"):
            n_fit += 1
        rows.append({**base, **update})

    if n_fit == 0:
        print(
            f"ERROR: GRSS fitted NONE of the {len(psv_files)} radar fixtures. The channel "
            "computed nothing;\n"
            "       every row carries grss_error. See the per-object output above.",
            file=sys.stderr,
        )
        # Rows are still written by the caller so the failures stay visible.
    return rows


# ── summary ─────────────────────────────────────────────────


def _pct(vals: list[float], p: float) -> float | None:
    if not vals:
        return None
    s = sorted(vals)
    return s[min(len(s) - 1, int(len(s) * p))]


def _fmt_km(v: float | None) -> str:
    if v is None:
        return "—"
    if v < 1e-3:
        return f"{v * 1e6:.2f} mm"
    if v < 1.0:
        return f"{v * 1e3:.2f} m"
    if v < 1000.0:
        return f"{v:.3f} km"
    return f"{v:.0f} km"


def _fmt_asec(v: float | None) -> str:
    if v is None:
        return "—"
    if abs(v) < 1e-3:
        return f"{v * 1e6:.2f} µas"
    if abs(v) < 1.0:
        return f"{v * 1e3:.2f} mas"
    return f'{v:.3f}"'


def executive_summary(ctx, rows: list[dict]) -> str:
    def have(tt, field):
        return [
            r[field]
            for r in rows
            if r.get("test_type") == tt and r.get(field) is not None
        ]

    lines = ["\n──── GRSS external-reference summary ───────────────────"]
    lines.append(f"  {ctx['source_version']}")
    lines.append(f"  Total rows: {len(rows)}")
    n_err = sum(1 for r in rows if r.get("grss_error"))
    lines.append(f"  Rows carrying grss_error: {n_err}")

    prop = have("propagation", "grss_vs_horizons_km")
    lines.append(f"  Propagation rows compared: {len(prop)}")
    if prop:
        lines.append(
            f"    |GRSS - Horizons|   p50={_fmt_km(_pct(prop, 0.5))}"
            f"   p95={_fmt_km(_pct(prop, 0.95))}   max={_fmt_km(_pct(prop, 1.0))}"
        )
        t = have("propagation", "grss_time_ms")
        if t:
            lines.append(
                f"    Wall clock          p50={statistics.median(t):.1f} ms   max={max(t):.0f} ms"
            )

    eph = have("ephemeris", "grss_separation_arcsec")
    lines.append(f"  Ephemeris rows compared: {len(eph)}")
    if eph:
        lines.append(
            f"    Angular separation  p50={_fmt_asec(_pct(eph, 0.5))}"
            f"   p95={_fmt_asec(_pct(eph, 0.95))}   max={_fmt_asec(_pct(eph, 1.0))}"
        )
        rho = [abs(v) for v in have("ephemeris", "grss_d_rho_km")]
        if rho:
            lines.append(
                f"    |d range|           p50={_fmt_km(_pct(rho, 0.5))}"
                f"   max={_fmt_km(_pct(rho, 1.0))}"
            )

    for tt, label in (
        ("orbit_determination", "OD (optical)"),
        ("orbit_determination_radar", "OD (optical+radar)"),
    ):
        fits = [
            r
            for r in rows
            if r.get("test_type") == tt and r.get("grss_rms_arcsec") is not None
        ]
        if not fits:
            continue
        lines.append(f"  {label} fits: {len(fits)}")
        conv = sum(1 for r in fits if r.get("grss_converged"))
        lines.append(f"    Converged           {conv}/{len(fits)}")
        rms = [r["grss_rms_arcsec"] for r in fits]
        lines.append(
            f"    Post-fit RMS        p50={_fmt_asec(_pct(rms, 0.5))}   max={_fmt_asec(_pct(rms, 1.0))}"
        )
        chi = [
            r["grss_reduced_chi2"]
            for r in fits
            if r.get("grss_reduced_chi2") is not None
        ]
        if chi:
            lines.append(
                f"    Reduced chi^2       p50={statistics.median(chi):.3f}   max={max(chi):.3f}"
            )
        dly = [
            r["grss_rms_delay_us"]
            for r in fits
            if r.get("grss_rms_delay_us") is not None
        ]
        if dly:
            lines.append(
                f"    Delay residual RMS  p50={statistics.median(dly):.3f} us   max={max(dly):.3f} us"
            )
        dop = [
            r["grss_rms_doppler_hz"]
            for r in fits
            if r.get("grss_rms_doppler_hz") is not None
        ]
        if dop:
            lines.append(
                f"    Doppler resid RMS   p50={statistics.median(dop):.3f} Hz   max={max(dop):.3f} Hz"
            )
        sig = [
            r["grss_sigma_pos_km"]
            for r in fits
            if r.get("grss_sigma_pos_km") is not None
        ]
        if sig:
            lines.append(
                f"    1-sigma position    p50={_fmt_km(_pct(sig, 0.5))}   max={_fmt_km(_pct(sig, 1.0))}"
            )
    if ctx["removed"]:
        lines.append(
            f"  Self-perturbers removed from GRSS's force model: "
            f"{', '.join(sorted(ctx['removed']))}"
        )
    # Re-stated here, in full, on every run. These rows carry real GRSS numbers
    # against a force model that differs from the plan's in a named way, and a
    # per-object log line 4000 rows up is not a place anyone will find that.
    if ctx["model_notes"]:
        lines.append(
            f"  Force-model differences on {len(ctx['model_notes'])} object(s) "
            "(also on every affected row as grss_model_note):"
        )
        for obj, note in sorted(ctx["model_notes"].items()):
            lines.append(f"    {obj}: {note}")
    lines.append("─" * 56)
    return "\n".join(lines)


# ── main ────────────────────────────────────────────────────


def main() -> int:
    here = Path(__file__).resolve().parent
    repo = here.parent.parent
    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    p.add_argument("--input", required=True, type=Path, help="validation plan JSON")
    p.add_argument("--output", type=Path, default=Path("results/validation_grss.json"))
    p.add_argument(
        "--fixtures-dir",
        type=Path,
        default=None,
        help="ADES PSV fixture directory. Default: fixtures/psv, or "
        "fixtures/psv-radar with --radar.",
    )
    p.add_argument(
        "--radar",
        action="store_true",
        help="Radar pass: walk the fixture directory and emit one "
        "orbit_determination_radar row per radar-augmented fixture instead of "
        "replaying the plan's propagation / ephemeris / optical-OD rows.",
    )
    p.add_argument(
        "--objects",
        default="",
        help="Comma-separated object subset (case-insensitive). Empty = all.",
    )
    p.add_argument(
        "--de-kernel",
        type=int,
        default=440,
        choices=sorted(_KERNELS_BY_DE),
        help="GRSS DE kernel case. 440 = DE440 + short-span SB441-N16 "
        "(1550-2650, ~235 MB, the default and what setup.sh fetches); "
        "441 = DE441 + long-span SB441-N16 (~4.4 GB, needs "
        "GRSS_DE_KERNEL=441 at setup).",
    )
    p.add_argument("--n-iter-max", type=int, default=10, help="LSQ iteration cap.")
    p.add_argument(
        "--numeric-partials",
        action="store_true",
        help="Build the measurement partials by finite-differencing 12 extra "
        "trajectories instead of from GRSS's propagated STM (~3x slower; "
        "measured equal to 6 significant figures).",
    )
    p.add_argument(
        "--verbose", action="store_true", help="Print GRSS's own per-fit logs."
    )
    args = p.parse_args()

    fixtures_dir = args.fixtures_dir or (
        repo / "fixtures" / ("psv-radar" if args.radar else "psv")
    )

    if not args.input.is_file():
        print(f"ERROR: plan {args.input} does not exist.", file=sys.stderr)
        return 1
    plan = json.loads(args.input.read_text())
    if not plan:
        print(
            f"ERROR: plan {args.input} carries zero rows. GRSS has nothing to replay; "
            "a comparator\n       with nothing to compare is a failure, not an empty "
            "success. Regenerate it with\n       `make plan`.",
            file=sys.stderr,
        )
        return 1

    objects = None
    if args.objects.strip():
        objects = {o.strip().lower() for o in args.objects.split(",") if o.strip()}
        present = {str(r.get("object", "")).lower() for r in plan}
        if not (objects & present):
            print(
                f"ERROR: --objects matched no object in {args.input}.\n"
                f"       Requested: {', '.join(sorted(objects))}\n"
                f"       Plan carries: {', '.join(sorted(x for x in present if x))}",
                file=sys.stderr,
            )
            return 1
        plan = [r for r in plan if str(r.get("object", "")).lower() in objects]

    grss = _import_grss()
    from grss import libgrss
    from grss.fit.fit_utils import get_codes_dict
    from grss.utils import default_kernel_path

    _check_kernels(Path(default_kernel_path), args.de_kernel)

    analytic = not args.numeric_partials
    trim = PerturberTrim(libgrss, default_kernel_path, args.de_kernel)
    ctx = {
        "grss": grss,
        "libgrss": libgrss,
        "codes": get_codes_dict(),
        "kernel_path": default_kernel_path,
        "de_kernel": args.de_kernel,
        "n_iter_max": args.n_iter_max,
        "analytic_partials": analytic,
        "verbose": args.verbose,
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "source_version": _source_version(grss, args.de_kernel, analytic),
        "trim": trim,
        "removed": set(),
        "model_notes": {},
    }
    print(f"{ctx['source_version']}", file=sys.stderr)
    print(f"Plan: {len(plan)} rows from {args.input}", file=sys.stderr)
    print(f"Fixtures: {fixtures_dir}", file=sys.stderr)

    if args.radar:
        rows = run_radar_pass(ctx, plan, fixtures_dir, objects)
    else:
        rows = run_plan_pass(ctx, plan, fixtures_dir)

    if not rows:
        print(
            "ERROR: GRSS produced zero rows. Nothing was compared, and an empty "
            "result file reads\n       downstream as 'GRSS compared everything and "
            "agreed'. Check the plan's test types\n"
            f"       (this runner replays {', '.join(_PLAN_TEST_TYPES)}) and the log above.",
            file=sys.stderr,
        )
        return 1

    args.output.parent.mkdir(parents=True, exist_ok=True)
    # allow_nan=False: Python writes a bare `NaN` for a non-finite float, which
    # is not valid JSON. Rust's serde_json rejects it, so one such value makes
    # the whole channel unparseable and the failure surfaces in the reduce merge
    # two jobs later as a line number with no cause. Fail in the runner that
    # produced it. A quantity that could not be computed must be null.
    try:
        payload = json.dumps(rows, indent=2, default=str, allow_nan=False)
    except ValueError as e:
        print(
            f"ERROR: refusing to write non-finite values to {args.output}: {e}\n"
            "       Bare NaN/Infinity is invalid JSON — the reduce merge would reject\n"
            "       this whole channel. A quantity that could not be computed must be\n"
            "       null, and an observable that is not finite is a failed row.",
            file=sys.stderr,
        )
        return 1
    args.output.write_text(payload)
    print(f"Wrote {len(rows)} grss rows to {args.output}", file=sys.stderr)
    print(executive_summary(ctx, rows), file=sys.stderr)

    n_computed = sum(
        1
        for r in rows
        if any(
            r.get(k) is not None
            for k in (
                "grss_vs_horizons_km",
                "grss_separation_arcsec",
                "grss_rms_arcsec",
            )
        )
    )
    if n_computed == 0:
        print(
            f"ERROR: none of the {len(rows)} GRSS rows carries a computed comparison. "
            "Every row is a\n       failure row; the channel ran and measured nothing. "
            "See the per-row reasons above.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
