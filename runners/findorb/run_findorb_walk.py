#!/usr/bin/env python3
"""find_orb walk-forward runner for the covariance-realism validation family.

Walks the window manifest (``fixtures/windows.json``) with find_orb — Bill
Gray's (Project Pluto) GPL orbit-determination package, console binary ``fo`` —
one fit per window and one prediction batch per window, and writes the two
runner sidecars the scoring kernel consumes:

    {stem}_windows.jsonl      one WalkWindowRecord per (object, window)
    {stem}_predictions.jsonl  one PredictedObservation per (window, target)

Walk-forward covariance-realism runner: per-night expanding windows, held-out
sky predictions scored against their predicted covariance. The schema of
record is ``src/predict_schema.rs``; both sidecar types are
``#[serde(deny_unknown_fields)]``, so this runner emits **only** fields that
exist there and omits absent optionals entirely (never ``null``).

``--output`` (default ``results/validation_findorb_walk.json``) gets an empty
JSON array: external tools emit no thin ``ValidationResult`` rows, and the empty
array exists purely for pipeline symmetry with the reference channel.

find_orb is GPL-licensed and is never linked into empyrean; it runs here as an
external subprocess only.

What find_orb does and does not deliver here
--------------------------------------------
* **Fit** — one ``fo`` subprocess per window over a per-window ADES PSV built by
  *filtering the original fixture lines* (§2.4: fit-set identity is a protocol
  invariant, not a per-tool reparse). find_orb reads ADES PSV natively, so the
  fixture bytes reach it unmodified apart from the ``pos1-3`` decimal-point
  sanitization its ADES reader requires (see ``_sanitize_ades``).
* **Predict** — the *same* ``fo`` invocation that fits also writes the
  ephemerides: ``-C <code,code,...>`` with ``-e eph_%c.txt`` runs the fit once
  and then loops over observatory codes (``fo.cpp``, the ``while(*mpc_code_tptr)``
  loop sits after the fit and after ``compute_variant_orbit``). Every target is
  therefore predicted **for its own station**, topocentrically, at a cost of one
  fit per window rather than one fit per station.
* **Uncertainty — 1-D, by construction.** ``fo`` derives ephemeris uncertainty
  from *variant orbits*: with a covariance in hand it builds exactly **one**
  variant, displaced 1σ along ``eigenvects[0]`` — the dominant eigenvector of
  the state covariance, i.e. the line of variations
  (``fo.cpp``: ``n_orbits_in_ephem = 2; compute_variant_orbit(…, 1.)``). The
  reported uncertainty is the great-circle separation between the nominal and
  variant sky positions plus its position angle. That is a σ along one axis,
  **not** an error ellipse: there is no minor axis anywhere in the output, so
  there is nothing to build a 2×2 from. These predictions are therefore emitted
  as ``uncertainty_form: "sigma1d_pa"`` — the form ``predict_schema`` documents
  as "find_orb's variant-orbit form". Manufacturing a minor axis (zero → a
  singular matrix the kernel rejects; anything else → a fabricated number) would
  be exactly the hidden fallback this family exists to avoid.
* **Position angle** — find_orb reports the σ position angle as an *integer*
  number of degrees folded into [0, 180) (``put_ephemeris_posn_angle_sigma``:
  ``(int)floor(-posn_ang*180/PI + .5) % 180``). Since ``calc_dist_and_posn_ang``
  returns the negated position angle East of North, the reported integer *is*
  the PA East of North, to 1° resolution, modulo the 180° axis fold. The kernel
  contracts the residual onto \\( \\hat e = (\\sin\\theta, \\cos\\theta) \\), so
  the fold flips the sign of ``z1`` but not its magnitude.
* **Uncertainty — the MC 2×2 arm.** Because the 1-D form above costs the
  channel every \\( d^2 \\) statistic, ``--mc-samples`` (default 200) adds a
  second arm, ``config_arm: "default+mc"``, carrying a genuine ``radec_2x2``.
  It is built **entirely from find_orb's own numbers**: sample states from
  find_orb's fitted 6×6, propagate each one with ``fo`` itself, and take the
  second moment of the resulting sky scatter. No dynamics are re-implemented
  here — every variant position is computed by find_orb.

  The draw mirrors ``runners/rust/src/walk.rs`` exactly (same LCG + Box–Muller,
  same \\( Lz \\) offsets, same per-(object, window) FNV seed, same gnomonic
  tangent plane, same **second moment about the nominal** rather than the
  sample mean), so the two MC arms differ only in the engine underneath.

  The no-refit path is the crux, and is asserted rather than assumed —
  see ``_run_fo_state`` and ``_no_refit_ok``. A silent refit would drag every
  variant back onto one least-squares solution, collapsing the spread into a
  covariance far tighter than the fit supports: the failure mode that looks
  like perfect confidence.

  Two artifacts are measured and reported per run rather than hidden. The
  **nominal round-trip** is the sky distance between the fit-path nominal and
  the same state pushed back through the no-refit path; it is nonzero because
  ``covar.json`` writes its epoch with ``%f`` (six decimals ≈ 0.086 s), which
  for a fast NEO is a tenth of an arcsecond. It is common-mode — centre and
  variants share the epoch — so it biases neither the spread nor the ratio
  below, and the second moment is taken about the *no-refit* nominal precisely
  so that it cannot leak in. The **LOV cross-check** contracts the MC 2×2 onto
  find_orb's own reported σ position angle and divides by its reported σ: if
  find_orb's single-variant machinery is honest, that ratio sits near 1.
* **Sky motion** — ``pa_motion_deg`` and ``sky_rate_deg_day`` come from
  find_orb's motion columns (``OPTION_MOTION_OUTPUT``). Its default motion unit
  is ``'/hr`` (arcmin per hour, ``get_motion_unit_text``), converted here by
  ``ARCMIN_PER_HR_TO_DEG_PER_DAY``.
* **Solve-for width** — 6 (state only; no non-gravs). find_orb will fit
  non-gravitational parameters when it judges them warranted, but nothing in
  this runner's configuration asks for them, and ``n_orbit_params`` is reset to
  6 per fit.
* **Perturbers** — Mercury through Pluto, the Moon, and the BC-405 asteroids
  (``PERTURBERS=1007fe``: ``0x7fe`` planets + Moon, bit 20 = ``IDX_ASTEROIDS``,
  the bit ``runge.cpp`` tests before calling ``detect_perturbers``). find_orb
  applies asteroid perturbations only where ``1 < r < 11.5`` AU.

  Enabling them takes **data, not just the flag**. ``detect_perturbers`` needs
  three files and gives up quietly if any is missing:

  - ``mu1.txt`` — asteroid numbers and masses. Ships with find_orb.
  - ``bc405.dat`` — Baer & Chesley's BC-405 ephemeris: 300 asteroids, 6 elements,
    3654 40-day chunks spanning 1800–2200, as raw doubles. Exactly
    52,617,600 bytes. Obtained by downloading the 131 MB
    ``asteroid_ephemeris.txt`` linked from
    https://www.projectpluto.com/ast_pert.htm and letting ``fo`` convert it once.
  - ``bc405pre.dat`` — memoized int16 asteroid positions (AU × 1000).
    Auto-created, and the one config file find_orb *writes*; see ``_make_cfg_dir``.

  The ``PERTURBERS`` mask asks for asteroids; only the data turns them on. A
  missing ``bc405.dat`` costs one line of find_orb output and silently changes
  the force model, so this runner refuses to start in that state rather than
  producing planets-only fits under an asteroid-shaped configuration.

  Note this is still not the reference channel's perturber set: BC-405 is 300
  asteroids with Baer–Chesley masses, where empyrean/ASSIST use SB441-N16.
  ``--asteroid-pert-list`` can pin find_orb to a named set if a like-for-like
  comparison is ever wanted.

One fit-set caveat find_orb imposes
-----------------------------------
find_orb de-duplicates on its own: two rows sharing a date and observatory code
but differing in astrometry draw "1 observations match in date and observatory
code, but not in other regards. They will be ignored." and one is dropped. The
MPC record contains such pairs (Apophis's 2004-06-19 discovery night has one),
so find_orb's effective fit set can be one or two rows smaller than the
manifest's ``n_obs_fit`` even when this runner handed it exactly ``n_obs_fit``
lines. The kept-line count is still checked against the manifest — that check
guards the *input* — and ``n_obs_used`` / ``n_obs_rejected`` then report what
find_orb actually did with it.

Cold fits, and why
------------------
Every window is fitted cold, with find_orb's own initial orbit determination
(``seed_source: "own_iod"``, ``warm_start: false``), and ``-i`` is passed so
find_orb's stored-solution cache cannot seed anything.

That flag is load-bearing, not cosmetic. Without it ``fetch_previous_solution``
(``elem_out.cpp``) looks up the object in ``orbits.sof`` — resolved through
``fopen_ext``, so a file in ``~/.find_orb`` counts — and adopts that orbit as
the starting point. In a walk-forward test a cached full-arc solution is
**future information**: a two-night window would be seeded by an orbit that
already knows how the arc ends. ``-i`` makes each window's fit depend on that
window's observations and nothing else, which is also what makes the run
reproducible and order-independent under multiprocessing.

``--warm-start`` opts into the alternative: find_orb's ``-v`` flag, which takes
``<epoch>,<x>,<y>,<z>,<vx>,<vy>,<vz>`` and seeds the fit from that state alone
(``extract_state_vect_from_text``). The previous window's converged state is
read from ``covar.json``, whose ``state_vect`` is heliocentric-ecliptic J2000 in
AU and AU/day — find_orb's own default interpretation for that string, so no
frame flags are needed. It is state-only by construction: the covariance is not
in the string and find_orb re-derives outlier selection every window regardless.

Timing
------
find_orb fits and predicts in one non-separable invocation, so ``fit_time_ms``
is the wall clock of that whole call and ``predict_time_ms`` is **omitted**
rather than guessed. Splitting it would mean fitting twice — doubling the cost
of the run to synthesize a number find_orb does not measure.

Failure discipline
------------------
Every window of the selected profile gets a ``WalkWindowRecord``, converged or
not; a non-converged window is a record with ``converged: false`` and a *named*
failure, never a dropped window (§2.5). The process exits nonzero only when
*no* selected object produced any record at all (missing fixtures, unusable
manifest, no ``fo`` binary): a comparator that compared nothing must not look
like a comparator that agreed.

A fit-set count mismatch — kept PSV lines ≠ the manifest's ``n_obs_fit`` — is a
loud per-window failure, because fit-set identity is the one thing that makes
"same window" mean the same thing across five tools.

Setup:

    runners/findorb/setup.sh        # or an existing tools/find_orb checkout

Usage:

    python3 runners/findorb/run_findorb_walk.py --profile ladder --only "2024 YR4"
"""

from __future__ import annotations

import argparse
import bisect
import json
import math
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

# Tool tag on every emitted record (matches the §4.4 runner table).
TOOL = "findorb"
# Externals run their defaults only — one arm.
CONFIG_ARM = "default"
# find_orb solves the 6-element state; no non-gravs are requested here.
N_SOLVE_FOR = 6

# ── Time scales ─────────────────────────────────────────────
# (first MJD UTC of validity, TAI − UTC seconds). Provenance: IERS Bulletin C,
# the modern integer-offset era; no leap second has been announced since
# 2017-01-01. Mirrors the table in `src/windows.rs` and in the layup runner so
# every channel converts identically.
LEAP_SECONDS: list[tuple[int, float]] = [
    (41317, 10.0),  # 1972-01-01
    (41499, 11.0),  # 1972-07-01
    (41683, 12.0),  # 1973-01-01
    (42048, 13.0),  # 1974-01-01
    (42413, 14.0),  # 1975-01-01
    (42778, 15.0),  # 1976-01-01
    (43144, 16.0),  # 1977-01-01
    (43509, 17.0),  # 1978-01-01
    (43874, 18.0),  # 1979-01-01
    (44239, 19.0),  # 1980-01-01
    (44786, 20.0),  # 1981-07-01
    (45151, 21.0),  # 1982-07-01
    (45516, 22.0),  # 1983-07-01
    (46247, 23.0),  # 1985-07-01
    (47161, 24.0),  # 1988-01-01
    (47892, 25.0),  # 1990-01-01
    (48257, 26.0),  # 1991-01-01
    (48804, 27.0),  # 1992-07-01
    (49169, 28.0),  # 1993-07-01
    (49534, 29.0),  # 1994-07-01
    (50083, 30.0),  # 1996-01-01
    (50630, 31.0),  # 1997-07-01
    (51179, 32.0),  # 1999-01-01
    (53736, 33.0),  # 2006-01-01
    (54832, 34.0),  # 2009-01-01
    (56109, 35.0),  # 2012-07-01
    (57204, 36.0),  # 2015-07-01
    (57754, 37.0),  # 2017-01-01
]
_LEAP_MJDS = [m for m, _ in LEAP_SECONDS]
# TT − TAI, seconds (IAU definition).
TT_MINUS_TAI_S = 32.184
MJD_TO_JD = 2_400_000.5
# find_orb's default ephemeris motion unit is arcmin/hour (`get_motion_unit_text`
# falls back to "'/hr"). 1 '/hr = (1/60) deg/hr × 24 hr/day.
ARCMIN_PER_HR_TO_DEG_PER_DAY = 24.0 / 60.0


def tai_minus_utc_seconds(mjd_utc: float) -> float:
    """TAI − UTC at an epoch, seconds, from the pinned IERS table."""
    i = bisect.bisect_right(_LEAP_MJDS, math.floor(mjd_utc))
    if i == 0:
        raise ValueError(
            f"MJD {mjd_utc:.6f} predates the 1972 integer-leap-second era; "
            "this corpus has no such observations and the table does not cover it."
        )
    return LEAP_SECONDS[i - 1][1]


def mjd_utc_to_jd_tt(mjd_utc: float) -> float:
    """MJD UTC → JD TT.

    TT = UTC + (TAI − UTC) + 32.184 s. find_orb is handed TT directly (the
    ephemeris times file carries ``OPTION T`` and ``TT_EPHEMERIS=1`` is set), so
    unlike the TDB-fed channels this conversion stops at TT and the emitted
    ``epoch_scale_used`` is ``"tt"``. TDB − TT is ≤ 1.7 ms — four orders of
    magnitude inside the manifest's 120-second cut clearance (§2.1) — so the
    channels remain comparable.
    """
    return mjd_utc + (tai_minus_utc_seconds(mjd_utc) + TT_MINUS_TAI_S) / 86_400.0 + MJD_TO_JD


def iso_utc_to_mjd(s: str) -> float:
    """Parse an ISO-8601 UTC instant (`Z` and fractional seconds optional) to MJD UTC."""
    s = s.strip().rstrip("Z")
    date, _, time_part = s.partition("T")
    if not time_part:
        time_part = "00:00:00"
    y, m, d = (int(v) for v in date.split("-"))
    hh, mm, ss = time_part.split(":")
    days = _days_from_civil(y, m, d)
    frac = (int(hh) * 3600.0 + int(mm) * 60.0 + float(ss)) / 86_400.0
    return float(days) + 40_587.0 + frac  # 40587 = MJD of 1970-01-01


def _days_from_civil(y: int, m: int, d: int) -> int:
    """Days from 1970-01-01 for a proleptic-Gregorian date (Hinnant)."""
    y -= m <= 2
    era = (y if y >= 0 else y - 399) // 400
    yoe = y - era * 400
    mp = (m + 9) % 12
    doy = (153 * mp + 2) // 5 + d - 1
    doe = yoe * 365 + yoe // 4 - yoe // 100 + doy
    return era * 146_097 + doe - 719_468


# ── find_orb install discovery ──────────────────────────────


def _fo_binary(explicit: str | None, repo: Path) -> Path | None:
    """Resolve the ``fo`` console binary.

    Search order: an explicit ``--fo-binary``, ``$FO_BINARY``, the prefix
    ``setup.sh`` installs into, a sibling ``tools/find_orb`` checkout beside the
    repo, then ``$PATH``.
    """
    here = Path(__file__).resolve().parent
    candidates: list[Path] = []
    if explicit:
        candidates.append(Path(explicit).expanduser())
    env = os.environ.get("FO_BINARY")
    if env:
        candidates.append(Path(env).expanduser())
    candidates += [
        here / "install" / "bin" / "fo",
        here / "build" / "find_orb" / "fo",
        repo.parent / "tools" / "find_orb" / "fo",
    ]
    for c in candidates:
        if c.is_file() and os.access(c, os.X_OK):
            return c
    found = shutil.which("fo")
    return Path(found) if found else None


def _findorb_version(fo_bin: Path) -> str:
    """Provenance string for the run banner: the checked-out find_orb commit.

    No-hidden-fallbacks: when the sha cannot be resolved (not a git checkout,
    git missing) say so explicitly rather than reporting a silent blank.
    """
    src = fo_bin.parent
    try:
        proc = subprocess.run(
            ["git", "-C", str(src), "rev-parse", "--short", "HEAD"],
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )
    except Exception as e:  # noqa: BLE001
        return f"unknown ({e})"
    if proc.returncode != 0:
        tail = (proc.stderr or "").strip().splitlines()
        return f"unknown ({tail[-1] if tail else 'git rev-parse failed'})"
    sha = proc.stdout.strip()
    return sha or "unknown (empty git sha)"


# find_orb's config directory: where every `fopen_ext(..., "c...")` file is
# resolved from. `default_config_dir_name` probes ~, /software, /root and an alt
# dir for cospar.txt; on this platform that is always ~/.find_orb.
FO_CONFIG_DIR = Path.home() / ".find_orb"
# The BC-405 asteroid ephemeris, converted to binary: 300 asteroids x 3654
# 40-day chunks x 6 elements x 8 bytes. bc405.cpp fixes every one of those
# numbers, so the size is an exact integrity check, not an approximation.
BC405_DAT = "bc405.dat"
BC405_DAT_BYTES = 6 * 300 * 3654 * 8  # 52_617_600
# find_orb's memoized int16 asteroid positions (AU x 1000), 3 per asteroid per
# chunk. Auto-created if absent, and the ONLY config file find_orb writes.
BC405_PRE = "bc405pre.dat"


def _bc405_status() -> tuple[Path | None, str]:
    """Locate ``bc405.dat`` and report whether it is intact.

    Returns (path or None, human-readable status). The size check matters: a
    truncated or half-converted file does not make find_orb fail, it makes the
    asteroid positions wrong somewhere in the middle of the file.
    """
    path = FO_CONFIG_DIR / BC405_DAT
    if not path.exists():
        return None, f"absent from {FO_CONFIG_DIR}"
    size = path.stat().st_size
    if size != BC405_DAT_BYTES:
        return None, f"WRONG SIZE {size} bytes (expected {BC405_DAT_BYTES})"
    return path, f"{path} ({size} bytes, intact)"


def _make_cfg_dir(base: Path) -> Path:
    """Create a private find_orb config directory for this process.

    Only one file goes in it. ``bc405pre.dat`` is opened ``"cr+b"`` — read-WRITE
    in the config directory — and `find_and_set_precomputed_data` fills in any
    chunk it finds zeroed and writes it back. Several `fo` processes sharing one
    copy would interleave those 1800-byte chunk writes, and while every writer
    computes identical bytes (so a torn write cannot corrupt a *value*), a
    reader can catch a chunk mid-write: the "already computed?" test reads only
    the first asteroid's three int16s, so a partially-written chunk reads as
    done and the remaining asteroids are used at position (0,0,0). That is a
    silent physics error, so each process gets its own file instead.

    Seeded by copying the shared cache when one exists, purely to skip
    recomputation — the cache is pure memoization, and a private copy was
    verified to reproduce shared-cache predictions byte for byte. When no shared
    cache exists the file is deliberately NOT created: find_orb then makes one
    here, correctly sized for whichever BC-405 variant is installed. Creating a
    wrong-sized placeholder would be worse than creating none.

    Everything else find_orb needs (`bc405.dat`, `mu1.txt`, `ObsCodes.htm`, the
    other 50 config files) is absent here and falls back to ~/.find_orb, which
    is read-only in this flow.
    """
    cfg = base / "cfg"
    cfg.mkdir(parents=True, exist_ok=True)
    master = FO_CONFIG_DIR / BC405_PRE
    private = cfg / BC405_PRE
    if master.exists() and not private.exists():
        shutil.copy2(master, private)
    return cfg


def _jpl_ephemeris(explicit: str | None, repo: Path) -> Path | None:
    """Locate Bill Gray's binary JPL DE file (``linux_p1550p2650.440``)."""
    candidates: list[Path] = []
    if explicit:
        candidates.append(Path(explicit).expanduser())
    candidates += [
        Path.home() / ".empyrean" / "data" / "linux_p1550p2650.440",
        Path.home() / ".find_orb" / "linux_p1550p2650.440",
        repo.parent / "data" / "linux_p1550p2650.440",
    ]
    for c in candidates:
        if c.is_file():
            return c
    return None


# ── ADES fixture handling ───────────────────────────────────


class Fixture:
    """The parsed ADES PSV fixture: pre-header, header, and per-line keys.

    Filtering happens on the ORIGINAL fixture lines. Re-parsing astrometry and
    re-emitting it would make every per-tool float-formatting difference part of
    the fit-set definition; keeping the bytes keeps the windows identical across
    tools. This mirrors the layup runner exactly.
    """

    def __init__(self, path: Path):
        self.path = path
        self.pre_header: list[str] = []
        self.header: str = ""
        self.lines: list[str] = []
        self.keys: list[tuple[str, str]] = []
        self.pos_cols: list[int] = []

        raw = path.read_text().splitlines()
        i = 0
        while i < len(raw) and raw[i].startswith(("#", "!")):
            self.pre_header.append(raw[i])
            i += 1
        if i >= len(raw):
            raise ValueError(f"{path}: no header line")
        self.header = raw[i]
        cols = [c.strip() for c in self.header.split("|")]
        try:
            i_stn = cols.index("stn")
            i_time = cols.index("obsTime")
        except ValueError as e:
            raise ValueError(f"{path}: header lacks stn/obsTime: {cols}") from e
        self.pos_cols = [cols.index(k) for k in ("pos1", "pos2", "pos3") if k in cols]
        for line in raw[i + 1 :]:
            if not line.strip():
                continue
            f = line.split("|")
            if len(f) <= max(i_stn, i_time):
                raise ValueError(f"{path}: short data line: {line[:80]!r}")
            self.lines.append(line)
            self.keys.append((f[i_stn].strip(), f[i_time].strip()))


def _sanitize_ades(line: str, pos_cols: list[int]) -> str:
    """Give integer-valued ADES ``pos1-3`` values an explicit decimal point.

    Space-based / roving observations (TESS C57, WISE C51, rovers 247/250/270)
    legally report integer observer positions (e.g. ``pos2=356814`` km), but
    find_orb's ADES converter (``ades2mpc.cpp``) asserts on a position value with
    no decimal point and aborts the whole fit. Appending ``.0`` is numerically
    identical and keeps ``fo`` alive. Same fix as ``run_findorb.py``.
    """
    if not pos_cols:
        return line
    cols = line.split("|")
    changed = False
    for i in pos_cols:
        if i < len(cols):
            v = cols[i].strip()
            if v and "." not in v and "e" not in v.lower():
                cols[i] = cols[i].replace(v, v + ".0", 1)
                changed = True
    return "|".join(cols) if changed else line


def _write_window_ades(fx: Fixture, keep: set[tuple[str, str]], out: Path) -> int:
    """Write the fixture lines whose (stn, obsTime) is in ``keep``. Returns the count.

    Duplicate (stn, obsTime) pairs occur in the MPC record, so membership is
    set-based and the *count* is what gets checked against the manifest.
    """
    kept = [line for line, key in zip(fx.lines, fx.keys) if key in keep]
    body = [_sanitize_ades(line, fx.pos_cols) for line in kept]
    with open(out, "w") as f:
        f.writelines(line + "\n" for line in (*fx.pre_header, fx.header, *body))
    return len(kept)


# ── environ.dat ─────────────────────────────────────────────


def _write_environ(
    work: Path, n_times: int, times_name: str, perturbers: str, pert_list: str
) -> None:
    """Write find_orb's per-run configuration file.

    Every path here is a bare relative name because ``fo`` runs with
    ``cwd=work``: find_orb copies configured paths into fixed-size buffers (94
    bytes for the JPL filename, ``%79s`` for the ephemeris step string), and an
    absolute path under a long scratch directory overflows them — harmlessly on
    macOS, fatally on a ``_FORTIFY_SOURCE`` Linux build.
    """
    lines = [
        # Symlinked into the working directory by the caller.
        "LINUX_JPL_FILENAME=jpl_de.440",
        # Mercury..Pluto + Moon (0x7fe) + asteroids (bit 20 = 0x100000, the
        # `IDX_ASTEROIDS` runge.cpp tests before calling `detect_perturbers`).
        f"PERTURBERS={perturbers}",
        # ── BC-405 asteroid perturbations ────────────────────
        # All three of these are find_orb's own documented defaults
        # (`environ.def`), written explicitly so the configuration states what
        # runs instead of relying on built-in fallbacks.
        #
        # BC405_ASTEROIDS: how many of BC-405's 300 asteroids are eligible. 300
        # is find_orb's default and the full model.
        "BC405_ASTEROIDS=300",
        # ASTEROID_THRESH: include an asteroid when it is within this many AU of
        # the target, scaled by sqrt(mass/Pallas). 10 is find_orb's default.
        "ASTEROID_THRESH=10",
        # ASTEROID_PERT_LIST: EMPTY is find_orb's default and means "consider all
        # 300, culled by the proximity test above". A non-empty list is the
        # opposite of what it looks like — `detect_perturbers` sets
        # `possible_perturber = 0` whenever `n_fixed` is set, so naming asteroids
        # RESTRICTS the model to exactly those and disables the other ~284.
        # Hence the default stays empty; `--asteroid-pert-list` exists for
        # deliberately replicating another integrator's fixed set.
        f"ASTEROID_PERT_LIST={pert_list}",
        # use_sigmas=1 (honour the ADES rmsRA/rmsDec), ephemeris_mag_limit=22
        # (fo resets this to 999 before any ephemeris), sigmas_in_columns=0,
        # forced_central_body=-2 (auto), apply_debiasing=0.
        #
        # The last field is the load-bearing one: find_orb's own EFCC/Farnocchia
        # debiasing stays OFF, because this family applies catalog debiasing
        # centrally from the manifest. Two debiasing passes would double-correct.
        "SETTINGS2=1 22.00 0 -2 0",
        # Encke's method for the integration.
        "ENCKE=1",
        # find_orb's stock 3-sigma outlier rejection.
        "OUTLIER_REJECTION_LIMIT=3",
        # Ephemeris epochs come from a file (leading 't' selects that mode); the
        # times in it are TT and we want TT back out.
        f"EPHEM_STEPS={n_times} t{times_name}",
        "TT_EPHEMERIS=1",
    ]
    (work / "environ.dat").write_text("\n".join(lines) + "\n")


# ── fo invocation ───────────────────────────────────────────


class FitOutcome:
    """One window's fo result: parsed fit + per-station ephemerides, or a failure."""

    def __init__(
        self,
        total: dict | None,
        covar: dict | None,
        ephem: dict[str, list[dict]] | None,
        failure: str | None,
        fit_time_ms: float,
    ):
        self.total = total
        self.covar = covar
        self.ephem = ephem
        self.failure = failure
        self.fit_time_ms = fit_time_ms
        # Per-station reason strings from `_parse_ephem_file`, so a window that
        # loses one station's ephemeris says *why* rather than just "missing".
        self.ephem_errors: dict[str, str] = {}


# find_orb's compact residual encoding (`put_residual_into_text`) uses SI
# prefixes below 0.00999": the printed number was multiplied by 1000 once per
# prefix step. Index order matches `lower_si_prefixes` in ephem0.cpp.
_SI_SUFFIX = {
    "m": 1e-3,
    "u": 1e-6,
    "n": 1e-9,
    "p": 1e-12,
    "f": 1e-15,
    "a": 1e-18,
    "z": 1e-21,
    "y": 1e-24,
    "r": 1e-27,
    "q": 1e-30,
}


def _parse_sigma_arcsec(tok: str) -> float | None:
    """Decode find_orb's ephemeris σ token into arcseconds.

    ``put_residual_into_text`` picks a unit by magnitude and the caller truncates
    to four characters, so the same column can read ``.141`` (arcsec),
    ``815u`` (microarcsec), ``12.3`` (arcsec), ``123`` (arcsec), ``45'``
    (arcmin), ``2d`` (degrees) or ``Err!``. Anything unrecognised returns None
    and the prediction is emitted with ``uncertainty_form: "none"`` rather than
    a guessed number.
    """
    tok = tok.strip()
    if not tok or tok.startswith("Err"):
        return None
    try:
        if tok.endswith("d"):
            return float(tok[:-1]) * 3600.0
        if tok.endswith("'"):
            return float(tok[:-1]) * 60.0
        if tok[-1] in _SI_SUFFIX:
            return float(tok[:-1]) * _SI_SUFFIX[tok[-1]]
        return float(tok)
    except ValueError:
        return None


# Token counts a `-E 5,16,17` computer-friendly row may legitimately have, and
# what each means. In that mode `ephem0.cpp` writes, in order:
#
#     JD  RA  Dec  delta  r  elong  [mag]  motion  motionPA  [sigma  sigmaPA]
#
# with exactly two optional pieces, because no suppression bit is set and no
# other optional column's bit is requested:
#
#   * `mag` appears only when find_orb worked out an absolute magnitude —
#     `if( abs_mag)  /* don't show a mag if you dunno how bright the object
#     really is! */`. Fixtures whose photometry find_orb cannot use (early
#     Eros, for one) simply have no magnitude column.
#   * the σ pair appears only when find_orb had a covariance to build its
#     variant orbit from (`OPTION_SHOW_SIGMAS` is stripped when
#     `n_objects == 1`).
#
# Those two independent options give four widths, each unambiguous. Rows are
# read from both ends — JD/RA/Dec from the left, the motion and σ blocks from
# the right — so the optional magnitude never shifts a field that matters.
_EPHEM_WIDTHS = {
    8: False,  # no mag, no sigma
    9: False,  # mag, no sigma
    10: True,  # no mag, sigma
    11: True,  # mag, sigma
}


def _parse_ephem_file(path: Path) -> list[dict] | str:
    """Parse one station's computer-friendly ephemeris written with ``-E 5,16,17``.

    Returns the parsed rows, or a *reason string* when the file cannot be used —
    the caller turns that into a named per-window failure rather than a silent
    gap. An unexpected row width is one of those reasons: a mis-indexed row
    would attach a plausible-looking number to the wrong quantity, which is far
    worse than a recorded failure.
    """
    if not path.exists():
        return "file_absent"
    rows: list[dict] = []
    width: int | None = None
    for ln in path.read_text().splitlines():
        if not ln.strip() or ln.startswith("#"):
            continue
        if ln.lstrip().startswith("No ephemeris output"):
            # find_orb's own "too faint / in daylight / below horizon" message.
            return "fo_reported_no_ephemeris_output"
        parts = ln.split()
        if width is None:
            width = len(parts)
            if width not in _EPHEM_WIDTHS:
                return f"unexpected_row_width_{width}"
        elif len(parts) != width:
            return f"ragged_rows_{width}_vs_{len(parts)}"
        has_sigma = _EPHEM_WIDTHS[width]
        # Offsets from the right end: [... motion, motionPA (, sigma, sigmaPA)].
        m0 = -4 if has_sigma else -2
        try:
            row = {
                "jd_tt": float(parts[0]),
                "ra_deg": float(parts[1]),
                "dec_deg": float(parts[2]),
                "motion_arcmin_hr": float(parts[m0]),
                "motion_pa_deg": float(parts[m0 + 1]),
            }
        except ValueError:
            return "non_numeric_row"
        if has_sigma:
            row["sigma_arcsec"] = _parse_sigma_arcsec(parts[-2])
            try:
                row["sigma_pa_deg"] = float(parts[-1])
            except ValueError:
                row["sigma_pa_deg"] = None
        else:
            row["sigma_arcsec"] = None
            row["sigma_pa_deg"] = None
        rows.append(row)
    return rows if rows else "empty_ephemeris"


def _load_json_lenient(path: Path) -> dict:
    """Load one of find_orb's JSON outputs.

    ``strict=False`` is required, not defensive: find_orb copies observation
    text into ``total.json`` verbatim, and MPC records carry raw control
    characters that a strict JSON parser rejects outright.
    """
    return json.loads(path.read_text(errors="replace"), strict=False)


def _run_fo(
    work: Path,
    fo_bin: Path,
    obs_name: str,
    codes: list[str],
    warm_state: tuple[float, list[float]] | None,
    timeout: float,
    cfg_dir: Path,
) -> FitOutcome:
    """Fit one window and produce every station's ephemeris in one ``fo`` call."""
    cmd = [
        str(fo_bin),
        obs_name,
        "-c",  # combine every record in the file into one object
        "-q",  # quiet: no progress chatter from parallel workers
        # Private config directory (see `_make_cfg_dir`). find_orb looks here
        # first and falls back to ~/.find_orb for everything absent, so this
        # costs one file and isolates the one config file fo WRITES.
        # The trailing separator is required: fopen_ext does `strcat` with no
        # separator of its own.
        "-x",
        f"{cfg_dir}{os.sep}",
        "-D",
        "environ.dat",
        "-O",
        ".",  # keep find_orb's temp/output files inside this worker's dir
        "-e",
        "eph_%c.txt",
        # bit 5 = motion, bit 16 = show sigmas, bit 17 = computer friendly;
        # bits 0-2 stay clear, selecting the observables ephemeris type.
        "-E",
        "5,16,17",
        "-C",
        ",".join(codes),
    ]
    if warm_state is None:
        # No stored solution may seed this fit — see the module docstring.
        cmd.append("-i")
    else:
        epoch_jd, state = warm_state
        vec = ",".join(repr(float(v)) for v in state)
        # Attached form (`-v<text>`): find_orb's get_arg() treats a following
        # argv beginning with '-' as an empty argument, and a state vector
        # routinely starts with a negative component.
        cmd.append(f"-vJD {epoch_jd!r},{vec}")

    t0 = time.perf_counter()
    try:
        proc = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=timeout,
            cwd=work,
            check=False,  # a nonzero exit is a named failure record, not an exception
        )
    except subprocess.TimeoutExpired:
        return FitOutcome(None, None, None, f"fo_timeout_{timeout:.0f}s", (time.perf_counter() - t0) * 1000.0)
    ms = (time.perf_counter() - t0) * 1000.0

    if proc.returncode != 0:
        rc = proc.returncode
        how = f"rc={rc}"
        if rc < 0:
            how = f"signal_{-rc}"
        elif rc > 128:
            how = f"rc={rc}_signal_{rc - 128}"
        tail = " | ".join((proc.stderr or proc.stdout or "").strip().splitlines()[-3:])
        return FitOutcome(None, None, None, f"fo_exit_{how}: {tail}"[:500], ms)

    total_path = work / "total.json"
    covar_path = work / "covar.json"
    if not total_path.exists():
        return FitOutcome(None, None, None, "fo_no_total_json", ms)
    try:
        total = _load_json_lenient(total_path)
    except Exception as e:  # noqa: BLE001
        return FitOutcome(None, None, None, f"fo_total_json_parse_error: {e}"[:500], ms)
    covar = None
    if covar_path.exists():
        try:
            covar = _load_json_lenient(covar_path)
        except Exception:  # noqa: BLE001
            covar = None

    ephem: dict[str, list[dict]] = {}
    errors: dict[str, str] = {}
    for code in codes:
        parsed = _parse_ephem_file(work / f"eph_{code}.txt")
        if isinstance(parsed, str):
            errors[code] = parsed
        else:
            ephem[code] = parsed
    outcome = FitOutcome(total, covar, ephem, None, ms)
    outcome.ephem_errors = errors
    return outcome


# ── Monte-Carlo sky covariance ──────────────────────────────
#
# find_orb's own ephemeris uncertainty is 1-D by construction (one variant
# orbit along the line of variations), so the tool can never print a 2x2. This
# channel builds one anyway, from find_orb's OWN fitted covariance and find_orb's
# OWN propagator: sample states from the 6x6, propagate each with `fo`, and take
# the second moment of the resulting sky scatter. Nothing here re-implements
# dynamics — every variant position is computed by find_orb.
#
# Conventions are mirrored from `runners/rust/src/walk.rs` on purpose, so the
# empyrean MC arm and this one differ only in the engine underneath: the same
# LCG + Box-Muller draw, the same \( L z \) offsets, the same gnomonic tangent
# plane, and the same second moment taken ABOUT THE NOMINAL PREDICTION rather
# than about the sample mean.

# The find_orb arm name for MC rows. Nominal rows keep CONFIG_ARM.
MC_CONFIG_ARM = "default+mc"


def _fnv1a64(s: str) -> int:
    h = 0xCBF29CE484222325
    for b in s.encode():
        h ^= b
        h = (h * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return h


def _mc_seed(obj: str, window_index: int) -> int:
    """Deterministic per-(object, window) seed — walk.rs's convention exactly."""
    return _fnv1a64(obj) ^ (window_index & 0xFFFFFFFFFFFFFFFF)


def _cholesky6(cov: list[list[float]]) -> list[list[float]] | None:
    """Lower Cholesky factor of a 6x6, or None if it is not positive definite.

    Returning None rather than nudging the matrix is deliberate: a covariance
    find_orb could not make positive definite is a fact about that fit, and
    sampling a repaired version would report confidence the fit never had.
    """
    n = 6
    lo = [[0.0] * n for _ in range(n)]
    for i in range(n):
        for j in range(i + 1):
            s = cov[i][j] - sum(lo[i][k] * lo[j][k] for k in range(j))
            if i == j:
                if not (s > 0.0) or not math.isfinite(s):
                    return None
                lo[i][j] = math.sqrt(s)
            else:
                if lo[j][j] == 0.0:
                    return None
                lo[i][j] = s / lo[j][j]
            if not math.isfinite(lo[i][j]):
                return None
    return lo


def _mc_offsets(lo: list[list[float]], seed: int, n_samples: int) -> list[list[float]]:
    """``L z`` with standard-normal ``z`` — bit-for-bit walk.rs's generator.

    A 64-bit LCG (the PCG multiplier/increment pair) feeding Box-Muller with a
    cached spare, matching `state_offsets`'s MonteCarlo arm so that a run is
    reproducible and comparable with the reference channel's MC arm.
    """
    mask = 0xFFFFFFFFFFFFFFFF
    state = max(seed, 1) & mask
    spare: list[float] = []

    def uniform() -> float:
        nonlocal state
        state = (state * 6364136223846793005 + 1442695040888963407) & mask
        return (state >> 11) / float(1 << 53)

    def normal() -> float:
        if spare:
            return spare.pop()
        u1 = max(uniform(), 1e-16)
        u2 = uniform()
        r = math.sqrt(-2.0 * math.log(u1))
        ang = 2.0 * math.pi * u2
        spare.append(r * math.sin(ang))
        return r * math.cos(ang)

    out: list[list[float]] = []
    for _ in range(n_samples):
        z = [normal() for _ in range(6)]
        out.append([sum(lo[row][col] * z[col] for col in range(6)) for row in range(6)])
    return out


def _gnomonic_offset_arcsec(
    nom_ra: float, nom_dec: float, ra: float, dec: float
) -> tuple[float, float] | None:
    """Gnomonic (east, north) offset of a variant about the nominal, arcsec.

    Same projection as walk.rs and as the scoring kernel. A variant more than
    90 deg from the nominal is behind the tangent plane and returns None, which
    withholds the whole window's MC covariance rather than folding in a
    projection that has lost its meaning.
    """
    RAD2ARCSEC = 3600.0 * 180.0 / math.pi

    def unit(a_deg: float, d_deg: float) -> tuple[float, float, float]:
        a, d = math.radians(a_deg), math.radians(d_deg)
        return (math.cos(d) * math.cos(a), math.cos(d) * math.sin(a), math.sin(d))

    u = unit(ra, dec)
    p = unit(nom_ra, nom_dec)
    dot = u[0] * p[0] + u[1] * p[1] + u[2] * p[2]
    if dot <= 0.0:
        return None
    a0, d0 = math.radians(nom_ra), math.radians(nom_dec)
    east = (-math.sin(a0), math.cos(a0), 0.0)
    north = (-math.sin(d0) * math.cos(a0), -math.sin(d0) * math.sin(a0), math.cos(d0))
    t = (u[0] / dot, u[1] / dot, u[2] / dot)
    return (
        (t[0] * east[0] + t[1] * east[1] + t[2] * east[2]) * RAD2ARCSEC,
        (t[0] * north[0] + t[1] * north[1] + t[2] * north[2]) * RAD2ARCSEC,
    )


def _second_moment_arcsec2(
    offsets: list[tuple[float, float] | None],
) -> list[list[float]] | None:
    """Second moment of the offsets ABOUT THE NOMINAL (not the sample mean).

    Divides by N, matching walk.rs's `second_moment_arcsec2`. Using the nominal
    as the centre is the point: it keeps any systematic offset between the
    nominal prediction and the variant cloud inside the covariance, where it
    belongs, instead of subtracting it away as the sample-mean form would.
    """
    n = float(len(offsets))
    if n == 0.0:
        return None
    m = [[0.0, 0.0], [0.0, 0.0]]
    for o in offsets:
        if o is None:
            return None
        e, no = o
        if not (math.isfinite(e) and math.isfinite(no)):
            return None
        m[0][0] += e * e / n
        m[0][1] += e * no / n
        m[1][1] += no * no / n
    m[1][0] = m[0][1]
    return m


def _run_fo_state(
    work: Path,
    fo_bin: Path,
    epoch_jd: float,
    state: list[float],
    codes: list[str],
    stem: str,
    timeout: float,
    cfg_dir: Path,
    environ_name: str,
) -> dict[str, list[dict]] | str:
    """Ephemeris for ONE supplied state, with **no fit** — returns rows or a reason.

    This is find_orb's "ephemeris from stored elements" path, driven from a state
    vector instead of a catalogue lookup:

    * ``-o<name>`` makes ``fo`` write a single placeholder 80-column observation
      (``make_fake_astrometry``, reference field ``Dummy``) and — the part that
      matters — clears ``drop_single_obs``, without which a one-observation
      object is discarded before anything runs.
    * ``-v<epoch>,<x>,…,<vz>`` supplies the state. ``fetch_previous_solution``
      checks ``state_vect_text`` FIRST, so this beats any stored solution.
    * With exactly one observation whose reference is ``Dummy``, that function
      takes the supplied state, calls ``set_locs`` and **returns before the
      improvement block** — no ``full_improvement``, no least squares, no
      covariance. Verified per run by `_no_refit_ok` below.

    The same ``environ.dat`` as the fit is reused, so variants integrate under
    the identical force model — checked empirically, not assumed: the nominal
    state pushed back through this path reproduces the fit-path ephemeris to
    ~2e-4 arcsec, which it could not do if the perturber set differed (dropping
    the asteroids alone moves a deep Holman window by up to 0.25 arcsec).
    """
    cmd = [
        str(fo_bin),
        "-oDUMMY",  # MUST be argv[1]; see reset_astrometry_filename
        "-c",
        "-q",
        "-i",
        "-x",
        f"{cfg_dir}{os.sep}",
        "-D",
        environ_name,
        "-O",
        ".",
        "-e",
        f"{stem}_%c.txt",
        "-E",
        "5,17",
        "-C",
        ",".join(codes),
        "-v" + f"JD {epoch_jd!r}," + ",".join(repr(float(v)) for v in state),
    ]
    for code in codes:
        stale = work / f"{stem}_{code}.txt"
        if stale.exists():
            stale.unlink()
    try:
        proc = subprocess.run(
            cmd, capture_output=True, text=True, timeout=timeout, cwd=work, check=False
        )
    except subprocess.TimeoutExpired:
        return f"variant_timeout_{timeout:.0f}s"
    if proc.returncode != 0:
        return f"variant_exit_{proc.returncode}"
    out: dict[str, list[dict]] = {}
    for code in codes:
        parsed = _parse_ephem_file(work / f"{stem}_{code}.txt")
        if isinstance(parsed, str):
            return f"variant_{code}_{parsed}"
        out[code] = parsed
    return out


def _mc_workdir(work: Path, jpl: Path | None) -> Path:
    """A clean subdirectory for the variant runs.

    Separate from the fit's directory for one reason: the fit left a
    ``covar.json`` there, and an empty directory is what makes "no covariance was
    written" a usable no-refit assertion (see `_no_refit_ok`).
    """
    mc = work / "mc"
    if mc.exists():
        shutil.rmtree(mc, ignore_errors=True)
    mc.mkdir(parents=True)
    if jpl is not None:
        os.symlink(jpl, mc / "jpl_de.440")
    shutil.copy2(work / "environ.dat", mc / "environ.dat")
    shutil.copy2(work / "times.txt", mc / "times.txt")
    return mc


def _no_refit_ok(mc_dir: Path) -> bool:
    """True when the dummy-path run really did skip the fit.

    ``fo`` writes no covariance when it adopts a supplied state without improving
    it (it prints "No sigmas"), and writes ``covar.json`` whenever
    ``full_improvement`` runs. A silent refit would pull every variant back onto
    the same least-squares solution, collapsing the sampled spread and
    manufacturing a covariance far tighter than the fit supports — the exact
    failure that would make this channel worthless while looking perfect — so
    the absence of a covariance is asserted, never assumed.
    """
    return not (mc_dir / "covar.json").exists()


def _mc_predictions(
    work: Path,
    cfg_dir: Path,
    fo_bin: Path,
    jpl: Path | None,
    covar: dict,
    entries: list[tuple[int, bool]],
    codes: list[str],
    observations: list[dict],
    nominal: list[dict],
    obj: str,
    window_index: int,
    n_samples: int,
    timeout: float,
) -> tuple[list[dict], dict]:
    """Build the MC 2x2 arm for one window. Returns (records, diagnostics).

    Diagnostics carry the no-refit assertion, the nominal round-trip residual and
    the per-prediction LOV cross-validation ratios; the caller counts and reports
    them. A failure anywhere returns an empty record list with a named reason —
    the nominal `sigma1d_pa` arm is unaffected either way.
    """
    diag: dict = {"reason": None, "ratios": [], "roundtrip_arcsec": None, "n_variants": 0}
    sv = covar.get("state_vect")
    cov = covar.get("covar")
    epoch = covar.get("epoch")
    if not (isinstance(sv, list) and len(sv) == 6 and isinstance(epoch, (int, float))):
        diag["reason"] = "covar_json_missing_state"
        return [], diag
    # Require a plain 6x6. find_orb writes n_params x n_params and would widen
    # this block if it ever solved non-gravs; slicing a leading 6x6 out of a
    # wider matrix silently assumes a parameter ORDER that has not been checked.
    if not (
        isinstance(cov, list)
        and len(cov) == 6
        and all(isinstance(r, list) and len(r) == 6 for r in cov)
    ):
        diag["reason"] = f"covar_not_6x6 (got {len(cov) if isinstance(cov, list) else type(cov).__name__})"
        return [], diag
    try:
        cov_f = [[float(v) for v in row] for row in cov]
        state = [float(v) for v in sv]
        epoch_jd = float(epoch)
    except (TypeError, ValueError):
        diag["reason"] = "covar_json_non_numeric"
        return [], diag
    lo = _cholesky6(cov_f)
    if lo is None:
        diag["reason"] = "covariance_not_positive_definite"
        return [], diag

    mc = _mc_workdir(work, jpl)
    # 1) The nominal state back through the SAME no-refit path. This is the
    #    centre the second moment is taken about, so it must come from the same
    #    propagator as the variants; comparing it with the fit-path nominal is
    #    also what proves the two paths share a force model.
    center = _run_fo_state(
        mc, fo_bin, epoch_jd, state, codes, "mcnom", timeout, cfg_dir, "environ.dat"
    )
    if isinstance(center, str):
        diag["reason"] = f"mc_nominal_{center}"
        return [], diag
    if not _no_refit_ok(mc):
        # Loud, and fatal for this window: every variant would be refitted too.
        diag["reason"] = "mc_nominal_REFITTED (covar.json written on the dummy path)"
        return [], diag
    worst = 0.0
    for k, (obs_idx, _) in enumerate(entries):
        row = center[observations[obs_idx]["stn"]][k]
        base = nominal[k]
        off = _gnomonic_offset_arcsec(base["ra_deg"], base["dec_deg"], row["ra_deg"], row["dec_deg"])
        if off is None:
            diag["reason"] = "mc_nominal_behind_tangent_plane"
            return [], diag
        worst = max(worst, math.hypot(*off))
    diag["roundtrip_arcsec"] = worst

    # 2) The variants.
    offsets = _mc_offsets(lo, _mc_seed(obj, window_index), n_samples)
    per_entry: list[list[tuple[float, float] | None]] = [[] for _ in entries]
    for d in offsets:
        variant = [state[i] + d[i] for i in range(6)]
        rows = _run_fo_state(
            mc, fo_bin, epoch_jd, variant, codes, "mcvar", timeout, cfg_dir, "environ.dat"
        )
        if isinstance(rows, str):
            diag["reason"] = f"mc_{rows}"
            return [], diag
        for k, (obs_idx, _) in enumerate(entries):
            c = center[observations[obs_idx]["stn"]][k]
            v = rows[observations[obs_idx]["stn"]][k]
            per_entry[k].append(
                _gnomonic_offset_arcsec(c["ra_deg"], c["dec_deg"], v["ra_deg"], v["dec_deg"])
            )
    if not _no_refit_ok(mc):
        diag["reason"] = "mc_variant_REFITTED (covar.json written on the dummy path)"
        return [], diag
    diag["n_variants"] = len(offsets)

    # 3) Records, plus the LOV cross-validation.
    out: list[dict] = []
    for k, (obs_idx, in_sample) in enumerate(entries):
        cov2 = _second_moment_arcsec2(per_entry[k])
        base = nominal[k]
        rec = {
            "object": obj,
            "tool": TOOL,
            "config_arm": MC_CONFIG_ARM,
            "window_index": int(window_index),
            "obs_idx": int(obs_idx),
            "key_hash": int(observations[obs_idx]["key_hash"]),
            "in_sample": bool(in_sample),
            # Position stays the nominal one, identical to the `default` arm:
            # only the uncertainty differs between the two arms.
            "ra_deg": base["ra_deg"],
            "dec_deg": base["dec_deg"],
            "epoch_scale_used": "tt",
        }
        if cov2 is not None:
            rec["uncertainty_form"] = "radec_2x2"
            rec["cov_radec"] = cov2
            rec["cov_units"] = "arcsec2"
            # Gnomonic east/north about the nominal: the RA axis is already
            # cos(dec)-scaled by construction.
            rec["cov_basis"] = "great_circle"
            # Cross-validation: contract the MC covariance onto find_orb's own
            # LOV direction and compare with the sigma it printed there. If
            # find_orb's variant-orbit machinery is honest, this sits near 1.
            pa = base.get("sigma1_pa_deg")
            s1 = base.get("sigma1_arcsec")
            if pa is not None and s1:
                th = math.radians(pa)
                e = (math.sin(th), math.cos(th))
                var = (
                    cov2[0][0] * e[0] * e[0]
                    + 2.0 * cov2[0][1] * e[0] * e[1]
                    + cov2[1][1] * e[1] * e[1]
                )
                if var > 0.0:
                    diag["ratios"].append(math.sqrt(var) / s1)
        else:
            rec["uncertainty_form"] = "none"
        for f in ("pa_motion_deg", "sky_rate_deg_day"):
            if f in base:
                rec[f] = base[f]
        out.append(rec)
    return out, diag


def _fo_object_block(total: dict) -> dict | None:
    """The single fitted object's block from ``total.json``."""
    objects = total.get("objects") or {}
    if not objects:
        return None
    return objects[next(iter(objects))]


# ── record builders ─────────────────────────────────────────


def _window_record(
    obj: str,
    window_index: int,
    converged: bool,
    warm_start: bool,
    fit_time_ms: float,
    failure: str | None = None,
    block: dict | None = None,
    covar: dict | None = None,
) -> dict:
    """Build a WalkWindowRecord dict.

    Only fields that exist in `predict_schema::WalkWindowRecord` appear, and
    absent optionals are omitted rather than emitted as null (the Rust side is
    `deny_unknown_fields` and `skip_serializing_if = Option::is_none`).

    `covariance_trust` is never emitted: it is a reference-engine verdict
    find_orb has no analogue for. `predict_time_ms` is never emitted either —
    see the module docstring on the single non-separable invocation.
    """
    rec: dict = {
        "object": obj,
        "tool": TOOL,
        "config_arm": CONFIG_ARM,
        "window_index": int(window_index),
        "converged": bool(converged),
    }
    if failure is not None:
        rec["failure"] = failure
    if block is not None:
        elements = block.get("elements") or {}
        residuals = (block.get("observations") or {}).get("residuals") or []
        used = [r for r in residuals if r.get("incl", 1) == 1]
        if residuals:
            rec["n_obs_used"] = len(used)
            rec["n_obs_rejected"] = len(residuals) - len(used)
        rec["n_solve_for"] = N_SOLVE_FOR
        # χ² summed directly from find_orb's own normalized residuals rather
        # than reconstructed from the 4-decimal `weighted_rms_residual`. Its
        # `n_resids` counts individual coordinate residuals (2 per observation),
        # which is the dof basis: reduced χ² = χ² / (n_resids − 6).
        chi2 = 0.0
        n_resids = 0
        for r in used:
            a, b = r.get("normalized_resid_1"), r.get("normalized_resid_2")
            if isinstance(a, (int, float)) and isinstance(b, (int, float)):
                chi2 += float(a) * float(a) + float(b) * float(b)
                n_resids += 2
        if n_resids and math.isfinite(chi2):
            rec["chi2"] = chi2
            dof = n_resids - N_SOLVE_FOR
            if dof > 0:
                red = chi2 / dof
                if math.isfinite(red):
                    rec["reduced_chi2"] = red
        # The COVARIANCE epoch (covar.json), not the element epoch: the
        # elements are reported at a rounded catalogue epoch while the
        # covariance — the thing this family scores — lives mid-arc.
        # find_orb epochs are TT; TDB − TT ≤ 1.7 ms (see mjd_utc_to_jd_tt).
        epoch_jd = (covar or {}).get("epoch")
        if isinstance(epoch_jd, (int, float)) and math.isfinite(float(epoch_jd)):
            rec["fit_epoch_mjd_tdb"] = float(epoch_jd) - MJD_TO_JD
        elif isinstance(elements.get("epoch"), (int, float)):
            rec["fit_epoch_mjd_tdb"] = float(elements["epoch"]) - MJD_TO_JD
    rec["warm_start"] = bool(warm_start)
    rec["seed_source"] = "warm_previous" if warm_start else "own_iod"
    rec["fit_time_ms"] = float(fit_time_ms)
    return rec


def _prediction_record(
    obj: str,
    window_index: int,
    obs: dict,
    obs_idx: int,
    in_sample: bool,
    row: dict,
) -> dict | None:
    """Build a PredictedObservation dict, or None if find_orb's position is unusable."""
    ra = row["ra_deg"]
    dec = row["dec_deg"]
    if not (math.isfinite(ra) and math.isfinite(dec)):
        return None
    rec: dict = {
        "object": obj,
        "tool": TOOL,
        "config_arm": CONFIG_ARM,
        "window_index": int(window_index),
        "obs_idx": int(obs_idx),
        "key_hash": int(obs["key_hash"]),
        "in_sample": bool(in_sample),
        "ra_deg": float(ra),
        "dec_deg": float(dec),
        # Times handed to find_orb are JD TT (see mjd_utc_to_jd_tt).
        "epoch_scale_used": "tt",
    }
    sigma = row.get("sigma_arcsec")
    pa = row.get("sigma_pa_deg")
    # §2.6: a σ that find_orb could not compute, or that rounds away entirely,
    # maps to `none` — never to a fabricated ellipse and never to a d² of 10⁶.
    if (
        sigma is not None
        and pa is not None
        and math.isfinite(sigma)
        and math.isfinite(pa)
        and sigma > 0.0
    ):
        rec["uncertainty_form"] = "sigma1d_pa"
        rec["sigma1_arcsec"] = float(sigma)
        rec["sigma1_pa_deg"] = float(pa)
    else:
        rec["uncertainty_form"] = "none"
    motion = row.get("motion_arcmin_hr")
    motion_pa = row.get("motion_pa_deg")
    if isinstance(motion_pa, float) and math.isfinite(motion_pa):
        rec["pa_motion_deg"] = motion_pa
    if isinstance(motion, float) and math.isfinite(motion):
        rec["sky_rate_deg_day"] = motion * ARCMIN_PER_HR_TO_DEG_PER_DAY
    return rec


# ── the walk ────────────────────────────────────────────────


def _resolve_fixture(fixtures_dir: Path, name: str, mpc_designation: str) -> Path | None:
    """The same three-candidate stem convention every runner uses."""
    for cand in (
        fixtures_dir / f"{name}.psv",
        fixtures_dir / f"{name.replace('/', '_')}.psv",
        fixtures_dir / f"{mpc_designation}.psv",
    ):
        if cand.exists():
            return cand
    return None


def _walk_object(task: dict) -> tuple[list[dict], list[dict], dict]:
    """Walk one object's windows. Returns (window records, predictions, stats).

    Takes a single dict argument so it can be handed straight to a
    multiprocessing pool.
    """
    obj = task["obj"]
    fixtures_dir = Path(task["fixtures_dir"])
    profile = task["profile"]
    fo_bin = Path(task["fo_bin"])
    jpl = Path(task["jpl"]) if task["jpl"] else None
    timeout = task["timeout"]
    perturbers = task["perturbers"]
    warm = task["warm_start"]
    max_reduced_chi2 = task["max_reduced_chi2"]
    pert_list = task["asteroid_pert_list"]
    n_mc = task["mc_samples"]

    name = obj["object"]
    stats = {
        "windows": 0,
        "converged": 0,
        "failed": 0,
        "predictions": 0,
        "with_sigma": 0,
        "mc_windows": 0,
        "mc_failed": 0,
        "mc_predictions": 0,
        "mc_with_cov": 0,
        "mc_time_ms": 0.0,
        "mc_roundtrip_max": 0.0,
        "mc_ratios": [],
        "error": None,
    }
    windows = [w for w in obj["windows"] if profile in w["profiles"]]
    if not windows:
        stats["error"] = f"no windows in profile {profile!r}"
        return [], [], stats

    fixture_path = _resolve_fixture(fixtures_dir, name, obj["mpc_designation"])
    if fixture_path is None:
        stats["error"] = "no PSV fixture found"
        return [], [], stats
    try:
        fx = Fixture(fixture_path)
    except Exception as e:  # noqa: BLE001
        stats["error"] = f"fixture parse failed: {e}"
        return [], [], stats

    observations = obj["observations"]
    win_records: list[dict] = []
    predictions: list[dict] = []
    prev_state: tuple[float, list[float]] | None = None

    def fail(idx: int, warm_used: bool, failure: str, fit_ms: float, **kw) -> None:
        """Record a named per-window failure. Never a dropped window (§2.5)."""
        print(f"    {name} w{idx}: {failure}", file=sys.stderr)
        win_records.append(
            _window_record(name, idx, False, warm_used, fit_ms, failure=failure, **kw)
        )
        stats["failed"] += 1

    with tempfile.TemporaryDirectory(prefix="fo_walk_") as td:
        base = Path(td)
        # One private config dir per task, OUTSIDE the per-window directory:
        # the asteroid-position cache it holds is worth carrying across an
        # object's windows, and it is what keeps parallel workers off each
        # other's writes.
        cfg_dir = _make_cfg_dir(base)
        for w in windows:
            idx = int(w["index"])
            stats["windows"] += 1
            warm_used = warm and prev_state is not None

            # A FRESH directory per window. find_orb both writes and re-reads
            # several files in its working directory (`fo.cpp` reopens
            # `combined.json` / `elements.json` for read, and the run leaves
            # ~20 more behind), so a directory shared across an object's
            # windows is a route for window N−1's solution to reach window N.
            # `_fo_object_block` takes the first object out of `total.json` and
            # would report it without noticing. Nothing here depends on
            # detecting that: the directory is recreated each pass, which also
            # keeps exactly one window's artifacts on disk at a time — objects
            # in this corpus run to ~1000 windows.
            work = base / "window"
            if work.exists():
                shutil.rmtree(work, ignore_errors=True)
            work.mkdir(parents=True)
            if jpl is not None:
                os.symlink(jpl, work / "jpl_de.440")

            # ── Fit set (§2.4): the manifest's non-excluded rows strictly
            # before the cut instant, matched back onto the original fixture
            # lines by (stn, obsTime). The manifest carries the cut in UTC and
            # in TDB; obsTime is UTC and the 120-s clearance makes the two
            # slicings equivalent, so we slice in UTC.
            cut_mjd_utc = iso_utc_to_mjd(w["cut_utc"])
            keep = {
                (o["stn"], o["obs_time"])
                for o in observations
                if o.get("excluded") is None and o["mjd_utc"] < cut_mjd_utc
            }
            obs_name = "window.ades"
            n_kept = _write_window_ades(fx, keep, work / obs_name)
            expected = int(w["n_obs_fit"])
            if n_kept != expected:
                # No fit was attempted, so nothing was warm-started.
                fail(
                    idx,
                    False,
                    f"fit_set_mismatch: kept {n_kept} PSV lines, manifest n_obs_fit={expected}",
                    0.0,
                )
                prev_state = None
                continue

            # ── Targets: this window's held-out targets plus its in-sample
            # controls, in one epoch list shared by every station.
            entries: list[tuple[int, bool]] = [(int(t["obs_idx"]), False) for t in w["targets"]]
            entries += [(int(i), True) for i in w["in_sample"]]
            if not entries:
                fail(idx, False, "no_targets_in_window", 0.0)
                prev_state = None
                continue
            times = [mjd_utc_to_jd_tt(observations[i]["mjd_utc"]) for i, _ in entries]
            codes = sorted({observations[i]["stn"] for i, _ in entries})

            with open(work / "times.txt", "w") as f:
                # OPTION T: the times below are TD/TT. find_orb converts them to
                # UTC on read and back to TT for the ephemeris (TT_EPHEMERIS=1),
                # so what comes back out is what went in.
                f.write("OPTION T\n")
                f.writelines(f"JD {t:.9f}\n" for t in times)
            _write_environ(work, len(times), "times.txt", perturbers, pert_list)
            for code in codes:
                stale = work / f"eph_{code}.txt"
                if stale.exists():
                    stale.unlink()

            outcome = _run_fo(
                work,
                fo_bin,
                obs_name,
                codes,
                prev_state if warm_used else None,
                timeout,
                cfg_dir,
            )
            if outcome.total is None:
                fail(idx, warm_used, outcome.failure, outcome.fit_time_ms)
                prev_state = None  # a failed fit seeds nothing; continue cold
                continue

            block = _fo_object_block(outcome.total)
            if block is None:
                fail(idx, warm_used, "fo_no_object_in_total_json", outcome.fit_time_ms)
                prev_state = None
                continue

            missing = [c for c in codes if c not in (outcome.ephem or {})]
            if missing:
                why = ", ".join(
                    f"{c}={outcome.ephem_errors.get(c, 'unknown')}" for c in missing[:6]
                )
                fail(
                    idx,
                    warm_used,
                    f"ephemeris_unusable_for_stations: {why}",
                    outcome.fit_time_ms,
                    block=block,
                    covar=outcome.covar,
                )
                prev_state = None
                continue
            # ── Convergence. find_orb emits no convergence flag: `fo` returns 0
            # and writes a full solution even when its IOD failed to link the
            # window's tracklets and it has settled on an orbit that fits
            # nothing. The observed failure mode is unmistakable in find_orb's
            # OWN arithmetic — an Apophis window whose 96-day precovery link
            # failed reported `weighted_rms_residual = 604`, i.e. a solution
            # missing its own retained observations by 604σ, while still
            # returning elements, a covariance and an ephemeris.
            #
            # So convergence is declared here, from find_orb's numbers, and the
            # deciding threshold is a documented CLI knob rather than a buried
            # constant: a window is converged when the fit is overdetermined and
            # its reduced χ² is below `--max-reduced-chi2`. Both the χ² and the
            # threshold travel in the output, so the call is auditable — and a
            # non-converged window is still a RECORD, never a dropped window.
            probe = _window_record(
                name, idx, True, warm_used, outcome.fit_time_ms, block=block, covar=outcome.covar
            )
            n_used = probe.get("n_obs_used")
            red = probe.get("reduced_chi2")
            if n_used is not None and n_used < 4:
                fail(
                    idx,
                    warm_used,
                    f"underdetermined: find_orb kept {n_used} of {expected} fit observations",
                    outcome.fit_time_ms,
                    block=block,
                    covar=outcome.covar,
                )
                prev_state = None
                continue
            if red is not None and red > max_reduced_chi2:
                fail(
                    idx,
                    warm_used,
                    f"did_not_converge: reduced_chi2={red:.4g} exceeds {max_reduced_chi2:g} "
                    f"(find_orb kept {n_used} of {expected} fit observations)",
                    outcome.fit_time_ms,
                    block=block,
                    covar=outcome.covar,
                )
                prev_state = None
                continue

            bad = [c for c in codes if len(outcome.ephem[c]) != len(times)]
            if bad:
                got = {c: len(outcome.ephem[c]) for c in bad[:4]}
                fail(
                    idx,
                    warm_used,
                    f"ephemeris_count_mismatch: expected {len(times)} rows, got {got}",
                    outcome.fit_time_ms,
                    block=block,
                    covar=outcome.covar,
                )
                prev_state = None
                continue
            # find_orb writes one row per requested time, in file order, for
            # every station. Verify rather than trust — a silent reordering or a
            # time-scale slip would attach every prediction to the wrong epoch.
            skew = max(
                abs(outcome.ephem[observations[i]["stn"]][k]["jd_tt"] - times[k])
                for k, (i, _) in enumerate(entries)
            )
            if skew > 1e-6:
                fail(
                    idx,
                    warm_used,
                    f"ephemeris_epoch_skew: max |Δepoch| = {skew:.3e} d",
                    outcome.fit_time_ms,
                    block=block,
                    covar=outcome.covar,
                )
                prev_state = None
                continue

            nominal_rows: list[dict] = []
            for k, (obs_idx, in_sample) in enumerate(entries):
                row = outcome.ephem[observations[obs_idx]["stn"]][k]
                rec = _prediction_record(
                    name, idx, observations[obs_idx], obs_idx, in_sample, row
                )
                if rec is None:
                    print(
                        f"    {name} w{idx}: non-finite predicted position for obs_idx "
                        f"{obs_idx}; prediction dropped",
                        file=sys.stderr,
                    )
                    continue
                nominal_rows.append(rec)
                predictions.append(rec)
                stats["predictions"] += 1
                if rec["uncertainty_form"] == "sigma1d_pa":
                    stats["with_sigma"] += 1

            win_records.append(probe)
            stats["converged"] += 1

            # ── MC 2x2 arm (§ optional): find_orb's own covariance, sampled and
            # propagated by find_orb itself. Only attempted when every nominal
            # row survived, so entry k and nominal_rows[k] stay aligned.
            if n_mc > 0 and outcome.covar and len(nominal_rows) == len(entries):
                t0 = time.perf_counter()
                mc_rows, diag = _mc_predictions(
                    work,
                    cfg_dir,
                    fo_bin,
                    jpl,
                    outcome.covar,
                    entries,
                    codes,
                    observations,
                    nominal_rows,
                    name,
                    idx,
                    n_mc,
                    timeout,
                )
                mc_ms = (time.perf_counter() - t0) * 1000.0
                stats["mc_time_ms"] += mc_ms
                if mc_rows:
                    predictions.extend(mc_rows)
                    stats["mc_predictions"] += len(mc_rows)
                    stats["mc_with_cov"] += sum(
                        1 for r in mc_rows if r["uncertainty_form"] == "radec_2x2"
                    )
                    stats["mc_ratios"].extend(diag["ratios"])
                    stats["mc_windows"] += 1
                    if diag["roundtrip_arcsec"] is not None:
                        stats["mc_roundtrip_max"] = max(
                            stats["mc_roundtrip_max"], diag["roundtrip_arcsec"]
                        )
                    # The MC arm shares this window's fit; only the prediction
                    # step differs, so the fit statistics are echoed and the
                    # variant cost is reported as predict_time_ms.
                    mc_win = dict(probe)
                    mc_win["config_arm"] = MC_CONFIG_ARM
                    mc_win["predict_time_ms"] = mc_ms
                    win_records.append(mc_win)
                else:
                    stats["mc_failed"] += 1
                    print(
                        f"    {name} w{idx}: MC arm skipped — {diag['reason']}",
                        file=sys.stderr,
                    )
            # §2.5: the state estimate alone crosses the window boundary.
            # covar.json's state_vect is heliocentric-ecliptic J2000, AU and
            # AU/day, at `epoch` (JD TT) — find_orb's own default reading of a
            # `-v` string, so it round-trips without frame flags.
            prev_state = None
            if warm and outcome.covar:
                sv = outcome.covar.get("state_vect")
                ep = outcome.covar.get("epoch")
                if isinstance(sv, list) and len(sv) == 6 and isinstance(ep, (int, float)):
                    vals = [float(v) for v in sv]
                    if all(math.isfinite(v) for v in vals) and math.isfinite(float(ep)):
                        prev_state = (float(ep), vals)

    return win_records, predictions, stats


# ── output ──────────────────────────────────────────────────


def _write_jsonl(path: Path, records: list[dict]) -> None:
    """Write JSONL, refusing non-finite values.

    Bare NaN/Infinity is invalid JSON — Rust's serde_json rejects it, so a single
    non-finite number would fail the whole channel at a line number with no
    cause. Anything that could not be computed is omitted, never NaN.
    """
    lines = []
    for rec in records:
        try:
            lines.append(json.dumps(rec, allow_nan=False))
        except ValueError as e:
            print(
                f"ERROR: refusing to write non-finite values to {path}: {e}\n"
                "       Bare NaN/Infinity is invalid JSON — Rust's serde_json rejects it, so this\n"
                "       whole channel would fail the scoring pass at a line number with no cause.\n"
                f"       Offending record: {rec!r}",
                file=sys.stderr,
            )
            raise
    path.write_text("".join(line + "\n" for line in lines))


def main() -> int:
    here = Path(__file__).resolve().parent
    repo = here.parent.parent  # runners/findorb/ → repo root

    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    p.add_argument(
        "--manifest",
        type=Path,
        default=repo / "fixtures" / "windows.json",
        help="Window manifest written by `empyrean-validation windows`.",
    )
    p.add_argument(
        "--fixtures-dir",
        type=Path,
        default=repo / "fixtures" / "psv",
        help="Directory of ADES PSV optical fixtures.",
    )
    p.add_argument(
        "--profile",
        type=str,
        default="ladder",
        choices=["full", "ci", "ladder"],
        help="Window-schedule profile to walk (a named subset of the bundle=1 schedule).",
    )
    p.add_argument(
        "--only",
        type=str,
        default=None,
        help="Comma-separated object names (or fixture stems) to walk; default all eligible.",
    )
    p.add_argument(
        "--output",
        type=Path,
        default=repo / "results" / "validation_findorb_walk.json",
        help="Thin-row JSON path. External tools emit no thin rows, so this gets an empty "
        "array for pipeline symmetry; the real output is the sidecars beside it "
        "({stem}_windows.jsonl, {stem}_predictions.jsonl).",
    )
    p.add_argument(
        "--fo-binary",
        type=str,
        default=None,
        help="Path to find_orb's `fo` console binary. Default: $FO_BINARY, then "
        "runners/findorb/install/bin/fo, then ../tools/find_orb/fo, then $PATH.",
    )
    p.add_argument(
        "--jpl-ephemeris",
        type=str,
        default=None,
        help="Bill Gray's binary JPL DE file (linux_p1550p2650.440). Default: "
        "~/.empyrean/data, then ~/.find_orb, then ../data.",
    )
    p.add_argument(
        "--perturbers",
        type=str,
        default="1007fe",
        help="find_orb PERTURBERS hex mask. Default 1007fe = Mercury..Pluto + Moon (7fe) "
        "plus BC-405 asteroids (bit 20). Clearing bit 20 (7fe) disables asteroid "
        "perturbations entirely.",
    )
    p.add_argument(
        "--asteroid-pert-list",
        type=str,
        default="",
        help="find_orb ASTEROID_PERT_LIST. EMPTY (the default, and find_orb's own) means "
        "all 300 BC-405 asteroids, culled by the ASTEROID_THRESH proximity test. Naming "
        "asteroids RESTRICTS the model to exactly those and switches the proximity logic "
        "off, so only pass a list to replicate another integrator (e.g. Horizons' DE-44x "
        "set: 1,2,3,4,10,15,16,31,48,52,65,87,88,451,511,704).",
    )
    p.add_argument(
        "--timeout",
        type=float,
        default=900.0,
        help="Per-window fo timeout in seconds (recorded as a named failure on overrun).",
    )
    p.add_argument(
        "--workers",
        type=int,
        default=4,
        help="Parallel worker processes, one object at a time each.",
    )
    p.add_argument(
        "--mc-samples",
        type=int,
        default=200,
        help="Monte-Carlo variant states per converged window, sampled from find_orb's own "
        "6x6 and propagated by find_orb itself, giving a full 2x2 sky covariance (and hence "
        "a d2) that find_orb's 1-D ephemeris sigma cannot provide. Emitted as a second arm, "
        f"config_arm {MC_CONFIG_ARM!r}; the nominal sigma1d_pa arm is unaffected. 0 disables. "
        "Cost is N propagations per window and scales with the integration span, so this is "
        "the run's dominant cost — see the summary's per-variant timing.",
    )
    p.add_argument(
        "--max-reduced-chi2",
        type=float,
        default=100.0,
        help="Reduced chi-squared above which a window is recorded as NOT converged. "
        "find_orb reports no convergence flag and returns a full solution even when its "
        "IOD failed to link the window, so this is the runner's stated criterion; the "
        "default (100, i.e. a weighted RMS around 10 sigma) is far above any real fit and "
        "far below the ~10^6 a failed linkage produces.",
    )
    p.add_argument(
        "--warm-start",
        action="store_true",
        help="Seed each window from the previous window's converged state via find_orb's "
        "-v flag (state only). Default is a cold fit per window with find_orb's own IOD, "
        "which is order-independent and cannot inherit a stored solution.",
    )
    args = p.parse_args()

    fo_bin = _fo_binary(args.fo_binary, repo)
    if fo_bin is None:
        print(
            "ERROR: find_orb's `fo` binary was not found.\n"
            "       Looked at $FO_BINARY, runners/findorb/install/bin/fo, "
            "../tools/find_orb/fo, and $PATH.\n"
            "       Build it with runners/findorb/setup.sh.\n"
            "       (Emitting empty sidecars would report 'find_orb predicted nothing' as a pass.)",
            file=sys.stderr,
        )
        return 1

    jpl = _jpl_ephemeris(args.jpl_ephemeris, repo)

    # Asteroid perturbations are requested by the PERTURBERS mask but ENABLED by
    # the presence of the data. Without bc405.dat find_orb prints one line about
    # it and integrates on happily with planets only — a silent downgrade of the
    # force model that would show up nowhere in the output. Refuse instead.
    asteroids_on = bool(int(args.perturbers, 16) & (1 << 20))
    bc405_path, bc405_status = _bc405_status()
    if asteroids_on and bc405_path is None:
        print(
            f"ERROR: asteroid perturbations are requested (PERTURBERS={args.perturbers}, "
            f"bit 20 set) but {BC405_DAT} is {bc405_status}.\n"
            "       find_orb would print 'asteroid perturbations cannot be included' and "
            "run with planets only —\n"
            "       a quietly different force model that no output field records.\n"
            "       Install it: download BC-405 (~131 MB ASCII, 6,577,200 lines) from the "
            "link on\n"
            "       https://www.projectpluto.com/ast_pert.htm, put it in "
            f"{FO_CONFIG_DIR}/asteroid_ephemeris.txt,\n"
            f"       and run one fo fit to convert it — {BC405_DAT} must end up exactly "
            f"{BC405_DAT_BYTES} bytes.\n"
            "       To run deliberately without asteroids, pass --perturbers 7fe.",
            file=sys.stderr,
        )
        return 1

    manifest_path = args.manifest.expanduser().resolve()
    if not manifest_path.exists():
        print(f"ERROR: manifest not found: {manifest_path}", file=sys.stderr)
        return 1
    manifest = json.loads(manifest_path.read_text())

    fixtures_dir = args.fixtures_dir.expanduser().resolve()
    if not fixtures_dir.is_dir():
        print(f"ERROR: fixtures directory not found: {fixtures_dir}", file=sys.stderr)
        return 1

    only: set[str] | None = None
    if args.only:
        only = {s.strip() for s in args.only.split(",") if s.strip()}

    selected = []
    for obj in manifest.get("objects", []):
        if not obj.get("eligible"):
            continue
        if only is not None and not (
            obj["object"] in only
            or obj["object"].replace("/", "_") in only
            or obj.get("mpc_designation") in only
        ):
            continue
        selected.append(obj)

    if not selected:
        print(
            f"ERROR: no eligible objects selected from {manifest_path}"
            + (f" with --only {args.only!r}" if only else "")
            + ".\n       find_orb had nothing to walk.",
            file=sys.stderr,
        )
        return 1

    print(
        f"find_orb walk-forward ({len(selected)} objects, profile {args.profile!r})",
        file=sys.stderr,
    )
    print(
        f"  fo: {fo_bin} (find_orb {_findorb_version(fo_bin)})\n"
        f"  JPL DE: {jpl if jpl else 'NOT FOUND — find_orb will fall back to its own search'}\n"
        f"  perturbers: {args.perturbers} "
        + (
            f"(Merc-Pluto + Moon + BC-405 asteroids: {bc405_status}, "
            + (
                f"fixed list {args.asteroid_pert_list})"
                if args.asteroid_pert_list
                else "all 300 proximity-culled)"
            )
            if asteroids_on
            else "(Merc-Pluto + Moon; asteroid perturbations OFF)"
        )
        + "\n"
        f"  warm_start: {args.warm_start}  workers: {args.workers}  "
        f"max_reduced_chi2: {args.max_reduced_chi2:g}",
        file=sys.stderr,
    )

    tasks = [
        {
            "obj": obj,
            "fixtures_dir": str(fixtures_dir),
            "profile": args.profile,
            "fo_bin": str(fo_bin),
            "jpl": str(jpl) if jpl else None,
            "timeout": args.timeout,
            "perturbers": args.perturbers,
            "warm_start": args.warm_start,
            "max_reduced_chi2": args.max_reduced_chi2,
            "asteroid_pert_list": args.asteroid_pert_list,
            "mc_samples": max(0, int(args.mc_samples)),
        }
        for obj in selected
    ]

    all_windows: list[dict] = []
    all_predictions: list[dict] = []
    n_delivered = 0
    totals = {
        "windows": 0,
        "converged": 0,
        "failed": 0,
        "predictions": 0,
        "with_sigma": 0,
        "mc_windows": 0,
        "mc_failed": 0,
        "mc_predictions": 0,
        "mc_with_cov": 0,
        "mc_time_ms": 0.0,
        "mc_roundtrip_max": 0.0,
    }
    mc_ratios: list[float] = []

    def absorb(obj_name: str, result: tuple[list[dict], list[dict], dict]) -> None:
        nonlocal n_delivered
        wins, preds, stats = result
        all_windows.extend(wins)
        all_predictions.extend(preds)
        for k, value in totals.items():
            # Every counter sums across objects; the round-trip residual is a
            # worst case, so it maxes instead.
            totals[k] = max(value, stats[k]) if k == "mc_roundtrip_max" else value + stats[k]
        mc_ratios.extend(stats["mc_ratios"])
        if wins:
            n_delivered += 1
        if stats["error"]:
            print(f"{obj_name}: SKIPPED — {stats['error']}", file=sys.stderr)
        else:
            print(
                f"{obj_name}: {stats['converged']}/{stats['windows']} windows converged, "
                f"{stats['predictions']} predictions ({stats['with_sigma']} with sigma)",
                file=sys.stderr,
            )

    workers = max(1, int(args.workers))
    if workers == 1 or len(tasks) == 1:
        for t in tasks:
            absorb(t["obj"]["object"], _walk_object(t))
    else:
        import multiprocessing as mp

        # 'spawn' rather than the platform default: a forked worker inherits the
        # parent's state, and these workers each need their own temp directory
        # and their own find_orb process tree.
        ctx = mp.get_context("spawn")
        with ctx.Pool(processes=min(workers, len(tasks))) as pool:
            for t, result in zip(tasks, pool.imap(_walk_object, tasks)):
                absorb(t["obj"]["object"], result)

    out = args.output.expanduser().resolve()
    out.parent.mkdir(parents=True, exist_ok=True)
    stem = out.with_suffix("")
    windows_path = stem.with_name(stem.name + "_windows.jsonl")
    preds_path = stem.with_name(stem.name + "_predictions.jsonl")

    # Sidecars first: the thin-row file is what a downstream `make` target keys
    # on, so it must not appear before the payload it stands for exists.
    _write_jsonl(windows_path, all_windows)
    _write_jsonl(preds_path, all_predictions)
    # External tools emit no thin ValidationResult rows; the empty array keeps
    # the pipeline's file shape uniform.
    out.write_text("[]\n")

    mc_block = ""
    if args.mc_samples > 0:
        n_var = totals["mc_windows"] * args.mc_samples
        per_var = (totals["mc_time_ms"] / n_var) if n_var else float("nan")
        mc_block = (
            f"  MC arm ({MC_CONFIG_ARM}, N={args.mc_samples}):\n"
            f"    windows:        {totals['mc_windows']} built, {totals['mc_failed']} skipped\n"
            f"    predictions:    {totals['mc_predictions']} "
            f"({totals['mc_with_cov']} with 2x2)\n"
            f"    cost:           {totals['mc_time_ms'] / 1000.0:.1f} s total, "
            f"{per_var:.0f} ms per variant propagation\n"
            f"    no-refit check: PASSED for every window built "
            f"(fo wrote no covariance on the dummy path)\n"
            f"    nominal round-trip: max {totals['mc_roundtrip_max']:.2e} arcsec "
            f"(fit path vs no-refit path — same force model)\n"
        )
        if mc_ratios:
            r = sorted(mc_ratios)

            def q(f: float) -> float:
                return r[min(len(r) - 1, int(f * len(r)))]

            mc_block += (
                f"    LOV cross-check: sigma_MC/sigma_findorb along find_orb's own PA — "
                f"median {q(0.5):.3f}, p10 {q(0.1):.3f}, p90 {q(0.9):.3f} (n={len(r)})\n"
            )
    print(
        f"\n──── find_orb walk-forward summary ({args.profile}) ───────\n"
        f"  Objects walked:   {n_delivered}/{len(selected)}\n"
        f"  Windows:          {totals['windows']}  "
        f"(converged {totals['converged']}, failed {totals['failed']})\n"
        f"  Predictions:      {totals['predictions']}  "
        f"(with sigma1d_pa {totals['with_sigma']}, "
        f"uncertainty_form=none {totals['predictions'] - totals['with_sigma']})\n"
        + mc_block
        + f"  Thin rows:        {out} (empty by design)\n"
        f"  Windows sidecar:  {windows_path}\n"
        f"  Predictions:      {preds_path}\n" + "─" * 56,
        file=sys.stderr,
    )

    if n_delivered == 0:
        print(
            "ERROR: no object produced a single window record — find_orb walked nothing.\n"
            "       (Non-convergence is DATA and exits 0; producing no records at all is not.)",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
