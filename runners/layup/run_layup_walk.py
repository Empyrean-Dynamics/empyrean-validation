#!/usr/bin/env python3
"""layup walk-forward runner for the covariance-realism validation family.

Walks the window manifest (``fixtures/windows.json``) with layup — Matthew
Holman's (Smithsonian / CfA) MIT-licensed, ASSIST-backed orbit fitter — one fit
per window and one prediction batch per window, and writes the two runner
sidecars the scoring kernel consumes:

    {stem}_windows.jsonl      one WalkWindowRecord per (object, window)
    {stem}_predictions.jsonl  one PredictedObservation per (window, target)

Walk-forward covariance-realism runner: per-night expanding windows, held-out
sky predictions scored against their predicted covariance.
The schema of record is ``src/predict_schema.rs``; both sidecar types are
``#[serde(deny_unknown_fields)]``, so this runner emits **only** fields that
exist there and omits absent optionals entirely (never ``null``).

``--output`` (default ``results/validation_layup_walk.json``) gets an empty JSON
array: external tools emit no thin ``ValidationResult`` rows, and the empty array
exists purely for pipeline symmetry with the reference channel. Everything real
is in the sidecars beside it.

What layup does and does not deliver here
-----------------------------------------
* **Fit** — one ``layup-orbitfit`` subprocess per window over a per-window ADES
  PSV built by *filtering the original fixture lines* (§2.4: fit-set identity is
  a protocol invariant, not a per-tool reparse). Same flags/conventions as the
  full-arc runner ``run_layup.py``: injected constant ``layupID`` primary-id
  column, ``--weight-data`` (Veres 2017 per-observatory σ) on by default,
  ``--debias`` off by default, ``--ar`` passed through when given.
* **Warm start** — §2.5 allows the *state estimate only* to cross a window
  boundary. layup's ``-g/--guess`` mechanism is exactly that: its C++
  ``run_from_vector_with_initial_guess`` seeds the LM from ``state`` + ``epoch``
  (plus non-grav amplitudes, unused here) and ignores the guess covariance
  entirely. The guess file this runner writes therefore carries the state, the
  epoch, ``FORMAT=BCART_EQ`` and ``flag=0``, the three fit statistics layup's
  parser hard-requires (as zeros), and **no covariance columns** — so "state
  only" is true by construction, not by trust. A warm window is genuinely seeded
  rather than re-IOD'd: ``layup.orbitfit._orbitfit`` runs its IOD only when the
  guess is absent or carries a nonzero ``flag``. Outlier selection is re-derived
  by layup from scratch every window regardless (layup has no cross-window
  rejection state), which is what §2.5 requires. Note that the warm fit adopts
  the *guess* epoch, so a warm chain reports the first window's
  ``fit_epoch_mjd_tdb`` throughout — layup's behavior, recorded as it is.
* **Predict** — one ``layup.predict.predict(data, obscode, times)`` Python-API
  call per window covering every target of that window plus its in-sample
  controls; ``times`` are JD TDB, ``obscode`` is the per-target station list
  (layup light-corrects each epoch against its own station).
* **Uncertainty** — layup returns ``obs_cov_xx / obs_cov_yy / obs_cov_xy``,
  **radians²** on the cos δ-scaled great-circle basis (``compute_single_predict``
  contracts \\(B \\Sigma B^\\top\\) onto the unit A/D tangent vectors), so
  ``cov_units="rad2"``, ``cov_basis="great_circle"``. layup writes **zeros**, not
  nulls, when it has no covariance; §2.6 pins those to ``uncertainty_form:
  "none"`` — never to a d² of 10⁶ — and this runner counts them.
* **Not delivered** — ``pa_motion_deg`` / ``sky_rate_deg_day``. layup's
  prediction record carries no sky-motion columns (its ``onsky_data`` path
  computes RA/Dec rates internally but drops them from the emitted recarray), so
  both fields are omitted and the scoring kernel falls back to the reference
  channel's shared position angle for the AT/CT rotation.
* **Solve-for width** — 6 (state only; no non-gravs, no Marsden DT), recorded as
  ``n_solve_for`` so no cross-tool statement is made across differing
  dimensionality.

Failure discipline
------------------
Every window of the selected profile gets a ``WalkWindowRecord``, converged or
not; a non-converged window is a record with ``converged: false`` and a *named*
failure, never a dropped window (§2.5). layup is expected to fail to converge on
roughly a third of this corpus (§4.4) — those records are data, and the run still
exits 0. The process exits nonzero only when *no* selected object produced any
record at all (missing fixtures, unusable manifest, no layup install): a
comparator that compared nothing must not look like a comparator that agreed.

A fit-set count mismatch — kept PSV lines ≠ the manifest's ``n_obs_fit`` — is a
loud per-window failure record, because fit-set identity is the one thing that
makes "same window" mean the same thing across five tools.

Known upstream issue (named, not hidden)
----------------------------------------
At the pinned layup ref (``60b5b75``) the ``layup-orbitfit`` console script
**cannot run a warm fit at all**: ``layup_cmdline/orbitfit.py::execute`` reads
``args.i`` while the parser stores ``--iod`` under ``dest="iod"``, so every
``-g/--guess`` invocation dies with ``AttributeError: 'Namespace' object has no
attribute 'i'`` before any fitting happens. No command line avoids it. Cold fits
therefore go through the console script unchanged (exactly as ``run_layup.py``
invokes it); warm fits go through ``_GUESS_SHIM`` below — that same console
script with the one missing attribute supplied and nothing else changed. Run
with ``--no-warm-start`` to walk entirely through the stock console script.

Usage (must run under the layup venv's interpreter, see ``layup/setup.sh``):

    runners/layup/.venv/bin/python runners/layup/run_layup_walk.py \\
        --profile ladder --only "2024 YR4"
"""

from __future__ import annotations

import argparse
import bisect
import json
import math
import shutil
import subprocess
import sys
import tempfile
import time
from argparse import Namespace
from pathlib import Path

try:
    import numpy as np
    import pandas as pd

    _HAVE_DEPS = True
    _DEPS_ERROR = ""
except Exception as e:  # noqa: BLE001
    _HAVE_DEPS = False
    _DEPS_ERROR = str(e)


# Tool tag on every emitted record (matches the §4.4 runner table).
TOOL = "layup"
# Externals run their defaults only — one arm.
CONFIG_ARM = "default"
# layup solves the 6-element barycentric-Cartesian state and nothing else.
N_SOLVE_FOR = 6
# Primary-id column injected into every prepared PSV. Named distinctly so it
# never collides with the fixtures' own (mostly-blank) provID / permID columns.
_PRIMARY_ID = "layupID"

# ── Time scales ─────────────────────────────────────────────
# (first MJD UTC of validity, TAI − UTC seconds). Provenance: IERS Bulletin C,
# the modern integer-offset era; no leap second has been announced since
# 2017-01-01. Mirrors the table in `src/windows.rs` so the two channels convert
# identically.
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


def tai_minus_utc_seconds(mjd_utc: float) -> float:
    """TAI − UTC at an epoch, seconds, from the pinned IERS table."""
    i = bisect.bisect_right(_LEAP_MJDS, math.floor(mjd_utc))
    if i == 0:
        raise ValueError(
            f"MJD {mjd_utc:.6f} predates the 1972 integer-leap-second era; "
            "this corpus has no such observations and the table does not cover it."
        )
    return LEAP_SECONDS[i - 1][1]


def mjd_utc_to_jd_tdb(mjd_utc: float) -> float:
    """MJD UTC → JD TDB.

    TT = UTC + (TAI − UTC) + 32.184 s. TDB is taken equal to TT: the periodic
    TDB − TT term is ≤ 1.7 ms, four orders of magnitude inside the manifest's
    120-second cut clearance (§2.1), so the approximation cannot move an
    observation across a window boundary or shift a prediction epoch measurably.
    The manifest itself carries `cut_mjd_tdb == cut_mjd_tt` for the same reason.
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


# ── layup install discovery ─────────────────────────────────


def _orbitfit_binary() -> Path | None:
    """Resolve the ``layup-orbitfit`` console script next to this interpreter.

    This script is executed by the layup venv's python, so the entry point is a
    sibling of ``sys.executable``. The direct entry point is used rather than the
    ``layup orbitfit`` dispatcher: the dispatcher discovers verbs by scanning
    ``$PATH`` for ``layup-*`` executables, which fails when the venv bin dir is
    not on ``$PATH`` (as when invoked by absolute path). Same resolution as
    ``run_layup.py``.
    """
    cand = Path(sys.executable).parent / "layup-orbitfit"
    if cand.exists():
        return cand
    found = shutil.which("layup-orbitfit")
    return Path(found) if found else None


def _layup_version() -> str | None:
    """Best available layup version string, for the run banner.

    The installed *distribution* version is preferred because setuptools-scm
    encodes the git commit in it (``0.1.devNNN+g<sha>``), which is what pins the
    validation-of-record; ``layup.__version__`` is the scm fallback literal
    ``"unknown version"`` in this build and identifies nothing.
    """
    from importlib import metadata

    try:
        return metadata.version("layup")
    except metadata.PackageNotFoundError:
        pass
    try:
        import layup

        v = str(getattr(layup, "__version__", "") or "").strip()
        return v or None
    except Exception:  # noqa: BLE001
        return None


# ── PSV handling ────────────────────────────────────────────


class Fixture:
    """The parsed ADES PSV fixture: pre-header, header, and per-line keys.

    Filtering happens on the ORIGINAL fixture lines. Re-parsing astrometry and
    re-emitting it would make every per-tool float-formatting difference part of
    the fit-set definition; keeping the bytes keeps the windows identical.
    """

    def __init__(self, path: Path):
        self.path = path
        self.pre_header: list[str] = []
        self.header: str = ""
        self.lines: list[str] = []
        self.keys: list[tuple[str, str]] = []

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
        for line in raw[i + 1 :]:
            if not line.strip():
                continue
            f = line.split("|")
            if len(f) <= max(i_stn, i_time):
                raise ValueError(f"{path}: short data line: {line[:80]!r}")
            self.lines.append(line)
            self.keys.append((f[i_stn].strip(), f[i_time].strip()))


def _write_window_psv(fx: Fixture, keep: set[tuple[str, str]], out: Path) -> int:
    """Write the fixture lines whose (stn, obsTime) is in ``keep``. Returns the count.

    Duplicate (stn, obsTime) pairs occur in the MPC record, so membership is
    set-based and the *count* is what gets checked against the manifest.
    """
    kept = [line for line, key in zip(fx.lines, fx.keys) if key in keep]
    with open(out, "w") as f:
        f.writelines(line + "\n" for line in (*fx.pre_header, fx.header, *kept))
    return len(kept)


def _prepare_input(psv_path: Path, obj_token: str, out: Path) -> None:
    """Normalize a window PSV into a layup-ingestible PSV.

    Strips the ADES space padding and prepends a constant ``layupID`` column so
    the whole file fits as one object — the fixtures lead with a ``permID``
    column that is blank for many comets and recently-designated objects, so
    layup's default (first column == a consistently-populated primary id) does
    not hold. Identical to ``run_layup.py``'s adapter.
    """
    pre_header: list[str] = []
    with open(psv_path) as fh:
        for line in fh:
            if line.startswith(("#", "!")):
                pre_header.append(line.rstrip("\n"))
            else:
                break
    df = pd.read_csv(
        psv_path,
        sep="|",
        skiprows=list(range(len(pre_header))),
        dtype=str,
        keep_default_na=False,
    )
    df.columns = [c.strip() for c in df.columns]
    for col in df.columns:
        df[col] = df[col].str.strip()
    df = df.drop(columns=[c for c in df.columns if c == _PRIMARY_ID], errors="ignore")
    df.insert(0, _PRIMARY_ID, obj_token)
    with open(out, "w") as f:
        for line in pre_header:
            f.write(line + "\n")
        df.to_csv(f, sep="|", index=False)


# ── orbitfit invocation ─────────────────────────────────────

_STATE_COLS = ("x", "y", "z", "xdot", "ydot", "zdot")


def _write_guess(state: dict, obj_token: str, out: Path) -> None:
    """Write layup's ``-g/--guess`` file carrying the STATE ONLY (§2.5).

    The guess reader is the input reader (``CSVDataReader`` with the PSV
    separator, because the observations arrive as ``ADES_psv``), keyed on the
    same primary-id column. layup requires ``FORMAT`` and ``epochMJD_TDB`` on a
    guess, checks ``flag``, and its ``parse_fit_result`` hard-reads ``csq`` /
    ``ndof`` / ``niter`` (a missing one is an uncaught ``ValueError``). Those
    three are fit *statistics*, not state, and the LM recomputes all of them from
    the new window's residuals — so they are written as zeros rather than carried
    over: nothing about how the previous window fit reaches the next one.

    The 36 ``cov_i_j`` columns are omitted entirely. ``parse_fit_result``
    substitutes 0.0 for any covariance column that is absent, and
    ``run_from_vector_with_initial_guess`` never reads the guess covariance at
    all, so omitting them makes "warm start carries the state estimate only"
    (§2.5) true by construction rather than by trust.
    """
    cols = [
        _PRIMARY_ID,
        "FORMAT",
        "epochMJD_TDB",
        *_STATE_COLS,
        "flag",
        "csq",
        "ndof",
        "niter",
    ]
    vals = [
        obj_token,
        "BCART_EQ",
        repr(float(state["epochMJD_TDB"])),
        *(repr(float(state[c])) for c in _STATE_COLS),
        "0",
        "0.0",
        "0",
        "0",
    ]
    out.write_text("|".join(cols) + "\n" + "|".join(vals) + "\n")


# The `layup-orbitfit` console script is
#     from layup_cmdline.orbitfit import main; sys.exit(main())
# and is used verbatim for cold fits. It cannot run a WARM fit at the pinned ref
# (60b5b75): `layup_cmdline/orbitfit.py::execute` opens with
#     if args.g and args.i == "gauss":
# but the parser stores `--iod` under `dest="iod"`, so `args.i` never exists and
# EVERY `-g/--guess` invocation dies with `AttributeError: 'Namespace' object has
# no attribute 'i'` before reaching `orbitfit_cli`. That is an upstream bug, not
# a usage error — there is no command line that creates the attribute.
#
# This shim is that console script with the missing alias supplied and nothing
# else. It changes no science: `execute` only uses `args.i` to null *itself* out,
# while `orbitfit_cli` reads the real `cli_args.iod`, and `_orbitfit` skips IOD
# whenever a valid guess is present regardless. The alias is added only when
# absent, so an upstream fix silently turns this back into the console script.
# Warm fits therefore run layup's own CLI path with layup's own flags; the one
# difference from the cold path is the `-g` flag itself.
_GUESS_SHIM = '''"""`layup-orbitfit` console script + the -g/--guess attribute fix (see runner)."""
import sys

import layup_cmdline.orbitfit as m

_execute = m.execute


def execute(args):
    if not hasattr(args, "i"):
        args.i = getattr(args, "iod", "gauss")
    return _execute(args)


m.execute = execute

# The __main__ guard is load-bearing, exactly as in the console script: layup
# fits through a spawn-context ProcessPoolExecutor, and every worker re-imports
# this file. Without the guard the workers would re-enter main() and die with
# multiprocessing's "attempt to start a new process before bootstrapping".
if __name__ == "__main__":
    sys.exit(m.main())
'''


def _read_orbitfit_csv(csv_path: Path) -> dict | None:
    """Read the single fitted-orbit row layup's ``orbitfit`` writes.

    Schema (layup ``orbitfit._get_result_dtypes``): ``csq`` / ``ndof`` / the
    BCART_EQ state / ``epochMJD_TDB`` / ``niter`` / ``method`` / ``flag`` /
    ``nobs_fit`` plus 36 flat ``cov_i_j`` columns. Read by column name.
    """
    df = pd.read_csv(csv_path)
    if len(df) == 0:
        return None
    return df.iloc[0].to_dict()


def _fnum(row: dict, key: str) -> float | None:
    try:
        v = float(row[key])
    except (KeyError, TypeError, ValueError):
        return None
    return v if math.isfinite(v) else None


def _inum(row: dict, key: str) -> int | None:
    v = _fnum(row, key)
    return int(v) if v is not None else None


class FitOutcome:
    """One window's fit result: the parsed row, or a named failure."""

    def __init__(
        self,
        row: dict | None,
        failure: str | None,
        fit_time_ms: float,
    ):
        self.row = row
        self.failure = failure
        self.fit_time_ms = fit_time_ms


def _fit_window(
    window_psv: Path,
    obj_token: str,
    orbitfit_bin: Path,
    work: Path,
    guess: dict | None,
    opts: argparse.Namespace,
) -> FitOutcome:
    """Run one ``layup-orbitfit`` subprocess over one window's observations."""
    prepared = work / "layup_input.psv"
    try:
        _prepare_input(window_psv, obj_token, prepared)
    except Exception as e:  # noqa: BLE001
        return FitOutcome(None, f"psv_prepare_failed: {e}", 0.0)

    out_stem = work / "layup_orbit"
    out_csv = Path(f"{out_stem}.csv")
    if out_csv.exists():
        out_csv.unlink()

    if guess is None:
        launch = [str(orbitfit_bin)]
    else:
        # Warm fit: the console script cannot take -g at this pin (see _GUESS_SHIM).
        shim = work / "layup_orbitfit_guess.py"
        if not shim.exists():
            shim.write_text(_GUESS_SHIM)
        launch = [sys.executable, str(shim)]

    cmd = [
        *launch,
        str(prepared),
        "ADES_psv",
        "--primary-id-column-name",
        _PRIMARY_ID,
        "-o",
        str(out_stem),
        "-f",
    ]
    if opts.weight_data:
        cmd.append("--weight-data")
    if opts.debias:
        cmd.append("--debias")
    if opts.ar_data_path:
        cmd += ["--ar", opts.ar_data_path]
    if guess is not None:
        guess_path = work / "layup_guess.psv"
        _write_guess(guess, obj_token, guess_path)
        cmd += ["-g", str(guess_path)]

    t0 = time.perf_counter()
    try:
        proc = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=opts.timeout,
            cwd=work,
            check=False,  # a nonzero exit is a named failure record, not an exception
        )
    except subprocess.TimeoutExpired:
        return FitOutcome(None, f"orbitfit_timeout_{opts.timeout:.0f}s", (time.perf_counter() - t0) * 1000.0)
    ms = (time.perf_counter() - t0) * 1000.0

    if proc.returncode != 0:
        tail = " | ".join((proc.stderr or proc.stdout or "").strip().splitlines()[-3:])
        return FitOutcome(None, f"orbitfit_exit_{proc.returncode}: {tail}"[:500], ms)
    if not out_csv.exists():
        return FitOutcome(None, "orbitfit_no_output_file", ms)
    try:
        row = _read_orbitfit_csv(out_csv)
    except Exception as e:  # noqa: BLE001
        return FitOutcome(None, f"orbitfit_parse_error: {e}"[:500], ms)
    if row is None:
        return FitOutcome(None, "orbitfit_no_output_row", ms)
    return FitOutcome(row, None, ms)


# ── prediction ──────────────────────────────────────────────


def _orbit_array(row: dict):
    """The fitted orbit as a one-row structured array for ``layup.predict``.

    ``predict`` needs the BCART_EQ state, ``epochMJD_TDB``, the flat ``cov_i_j``
    block (its presence is what makes layup emit the a/b/PA ellipse columns) and
    the primary-id column; every numeric column is carried as f8 and every string
    column as an object, which is the shape ``parse_fit_result`` reads.
    """
    names: list[str] = []
    formats: list[str] = []
    values: list[object] = []
    for key, val in row.items():
        names.append(str(key))
        if isinstance(val, str):
            formats.append("O")
            values.append(val)
        else:
            formats.append("f8")
            values.append(float(val))
    return np.array([tuple(values)], dtype=np.dtype({"names": names, "formats": formats}))


def _predict_batch(row: dict, stations: list[str], times_jd_tdb: list[float], cache_dir: str | None):
    """One ``layup.predict.predict`` call covering a whole window's epochs."""
    # Imported lazily: layup drags in jax/sorcha/ASSIST, so a --help or an
    # argument error must not pay for it.
    from layup.predict import predict

    data = _orbit_array(row)
    # `predict` consults args.onsky_data only; the on-sky path needs a
    # LayupConfigs + a second ASSIST simulation and adds no column this family
    # uses (its RA/Dec rates are computed and then dropped from the emitted
    # recarray), so it stays off.
    #
    # kclear in finally: predict() furnishes the full kernel set on EVERY
    # call (predict.py layup_furnish_spiceypy) but only its onsky path ever
    # clears the pool, so a long-lived pool worker accumulates duplicate
    # loads until SpiceNOMOREROOM (~1/3 of the first full-catalog ci run's
    # windows died this way). Clearing is safe precisely because every
    # predict call re-furnishes from scratch.
    import spiceypy as spice

    try:
        return predict(
            data,
            obscode=stations,
            times=times_jd_tdb,
            primary_id_column_name=_PRIMARY_ID,
            num_workers=1,
            cache_dir=cache_dir,
            args=Namespace(onsky_data=False, primary_id_column_name=_PRIMARY_ID),
            configs=None,
        )
    finally:
        spice.kclear()


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


def _window_record(
    obj: str,
    window_index: int,
    converged: bool,
    warm_start: bool,
    fit_time_ms: float,
    failure: str | None = None,
    row: dict | None = None,
    predict_time_ms: float | None = None,
) -> dict:
    """Build a WalkWindowRecord dict.

    Only fields that exist in `predict_schema::WalkWindowRecord` appear, and
    absent optionals are omitted rather than emitted as null (the Rust side is
    `deny_unknown_fields` and `skip_serializing_if = Option::is_none`).

    Two fields are deliberately never emitted. `covariance_trust` is a
    reference-engine verdict layup has no analogue for. `n_obs_rejected` is
    omitted rather than set to 0: layup's default fit path has no observation
    rejection stage at all (§4.4 — it is the natural pair for empyrean's
    rejection-off arm), and a literal 0 would claim a selection step ran and
    rejected nothing. Absent means "this tool does not do that"; 0 would mean
    "it did it and found none".
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
    if row is not None:
        csq = _fnum(row, "csq")
        ndof = _fnum(row, "ndof")
        nobs = _inum(row, "nobs_fit")
        niter = _inum(row, "niter")
        epoch = _fnum(row, "epochMJD_TDB")
        if nobs is not None:
            rec["n_obs_used"] = nobs
        rec["n_solve_for"] = N_SOLVE_FOR
        if niter is not None:
            rec["iterations"] = niter
        if csq is not None:
            rec["chi2"] = csq
            if ndof is not None and ndof > 0:
                red = csq / ndof
                if math.isfinite(red):
                    rec["reduced_chi2"] = red
        if epoch is not None:
            rec["fit_epoch_mjd_tdb"] = epoch
    rec["warm_start"] = bool(warm_start)
    # §2.5: layup runs its own IOD when cold; a warm window is seeded from this
    # same tool's previous-window state.
    rec["seed_source"] = "warm_previous" if warm_start else "own_iod"
    rec["fit_time_ms"] = float(fit_time_ms)
    if predict_time_ms is not None:
        rec["predict_time_ms"] = float(predict_time_ms)
    return rec


def _prediction_record(
    obj: str,
    window_index: int,
    obs: dict,
    obs_idx: int,
    in_sample: bool,
    pred,
) -> dict | None:
    """Build a PredictedObservation dict, or None if layup's position is unusable."""
    ra = float(pred["ra_deg"])
    dec = float(pred["dec_deg"])
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
        "ra_deg": ra,
        "dec_deg": dec,
        # `times` handed to layup are JD TDB (see mjd_utc_to_jd_tdb).
        "epoch_scale_used": "tdb",
    }
    xx = float(pred["obs_cov_xx"])
    yy = float(pred["obs_cov_yy"])
    xy = float(pred["obs_cov_xy"])
    finite = math.isfinite(xx) and math.isfinite(yy) and math.isfinite(xy)
    # §2.6: layup writes ZEROS, not nulls, for a covariance it does not have. A
    # zeroed matrix maps to `none` — never to a d² of 10⁶.
    if finite and not (xx == 0.0 and yy == 0.0 and xy == 0.0):
        rec["uncertainty_form"] = "radec_2x2"
        rec["cov_radec"] = [[xx, xy], [xy, yy]]
        rec["cov_units"] = "rad2"
        # compute_single_predict projects onto the unit A/D tangent vectors, so
        # the RA component is already cos δ-scaled.
        rec["cov_basis"] = "great_circle"
    else:
        rec["uncertainty_form"] = "none"
    # pa_motion_deg / sky_rate_deg_day: layup's predict output carries no
    # sky-motion columns, so both are omitted and the kernel falls back to the
    # reference channel's shared position angle.
    return rec


def _walk_object(
    obj: dict,
    fixtures_dir: Path,
    profile: str,
    orbitfit_bin: Path,
    opts: argparse.Namespace,
) -> tuple[list[dict], list[dict], dict]:
    """Walk one object's windows. Returns (window records, predictions, stats)."""
    name = obj["object"]
    stats = {
        "windows": 0,
        "converged": 0,
        "failed": 0,
        "predictions": 0,
        "with_cov": 0,
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
    obj_token = name.replace("/", "_")

    win_records: list[dict] = []
    predictions: list[dict] = []
    guess: dict | None = None  # previous window's state, when it converged

    def fail(idx: int, warm: bool, failure: str, fit_ms: float, **kw) -> None:
        """Record a named per-window failure. Never a dropped window (§2.5)."""
        print(f"    {name} w{idx}: {failure}", file=sys.stderr)
        win_records.append(_window_record(name, idx, False, warm, fit_ms, failure=failure, **kw))
        stats["failed"] += 1

    with tempfile.TemporaryDirectory(prefix="layup_walk_") as td:
        work = Path(td)
        for w in windows:
            idx = int(w["index"])
            stats["windows"] += 1
            warm = opts.warm_start and guess is not None

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
            window_psv = work / "window.psv"
            n_kept = _write_window_psv(fx, keep, window_psv)
            expected = int(w["n_obs_fit"])
            if n_kept != expected:
                failure = (
                    f"fit_set_mismatch: kept {n_kept} PSV lines, manifest n_obs_fit={expected}"
                )
                # No fit was attempted, so nothing was warm-started: recording
                # `warm_start: true` here would claim a seeded fit that never ran.
                fail(idx, False, failure, 0.0)
                guess = None
                continue

            outcome = _fit_window(
                window_psv, obj_token, orbitfit_bin, work, guess if opts.warm_start else None, opts
            )
            if outcome.row is None:
                fail(idx, warm, outcome.failure, outcome.fit_time_ms)
                guess = None  # a failed fit seeds nothing; continue cold
                continue

            row = outcome.row
            flag = _inum(row, "flag")
            csq = _fnum(row, "csq")
            converged = flag == 0 and csq is not None
            if not converged:
                failure = (
                    f"orbitfit_flag_{flag}"
                    if flag not in (0, None)
                    else ("orbitfit_nonfinite_chi2" if flag == 0 else "orbitfit_missing_flag")
                )
                fail(idx, warm, failure, outcome.fit_time_ms, row=row)
                guess = None
                continue

            # ── Predict: this window's held-out targets plus its in-sample
            # controls, in one call over the full epoch list.
            entries: list[tuple[int, bool]] = [(int(t["obs_idx"]), False) for t in w["targets"]]
            entries += [(int(i), True) for i in w["in_sample"]]
            stations = [observations[i]["stn"] for i, _ in entries]
            times = [mjd_utc_to_jd_tdb(observations[i]["mjd_utc"]) for i, _ in entries]

            predict_ms: float | None = None
            if entries:
                t0 = time.perf_counter()
                try:
                    preds = _predict_batch(row, stations, times, opts.ar_data_path)
                except Exception as e:  # noqa: BLE001
                    predict_ms = (time.perf_counter() - t0) * 1000.0
                    preds = None
                    predict_failure = f"predict_failed: {type(e).__name__}: {e}"[:500]
                else:
                    predict_ms = (time.perf_counter() - t0) * 1000.0
                    predict_failure = None
                    if len(preds) != len(entries):
                        predict_failure = (
                            f"predict_count_mismatch: got {len(preds)} for {len(entries)} epochs"
                        )
                    else:
                        # layup documents "results are returned in input order";
                        # verify rather than trust — a silent reordering would
                        # attach every prediction to the wrong observation.
                        skew = max(
                            abs(float(preds["epoch_JD_TDB"][k]) - times[k])
                            for k in range(len(entries))
                        )
                        if skew > 1e-6:
                            predict_failure = f"predict_epoch_reordered: max |Δepoch| = {skew:.3e} d"

                if predict_failure is not None:
                    # The fit converged; the prediction batch did not. The window
                    # delivered nothing to score, so it is recorded as failed with
                    # the named cause and the fit's own statistics attached — and,
                    # like every failed window, it seeds nothing, so the rule
                    # "converged: false ⇒ the next window is cold" holds uniformly.
                    fail(
                        idx,
                        warm,
                        predict_failure,
                        outcome.fit_time_ms,
                        row=row,
                        predict_time_ms=predict_ms,
                    )
                    guess = None
                    continue

                for k, (obs_idx, in_sample) in enumerate(entries):
                    rec = _prediction_record(
                        name, idx, observations[obs_idx], obs_idx, in_sample, preds[k]
                    )
                    if rec is None:
                        print(
                            f"    {name} w{idx}: non-finite predicted position for obs_idx "
                            f"{obs_idx}; prediction dropped",
                            file=sys.stderr,
                        )
                        continue
                    predictions.append(rec)
                    stats["predictions"] += 1
                    if rec["uncertainty_form"] == "radec_2x2":
                        stats["with_cov"] += 1

            win_records.append(
                _window_record(
                    name,
                    idx,
                    True,
                    warm,
                    outcome.fit_time_ms,
                    row=row,
                    predict_time_ms=predict_ms,
                )
            )
            stats["converged"] += 1
            # §2.5: state only crosses the boundary.
            guess = {k: row[k] for k in (*_STATE_COLS, "epochMJD_TDB")}

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
    repo = here.parent.parent  # runners/layup/ → repo root

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
        default=repo / "results" / "validation_layup_walk.json",
        help="Thin-row JSON path. External tools emit no thin rows, so this gets an empty "
        "array for pipeline symmetry; the real output is the sidecars beside it "
        "({stem}_windows.jsonl, {stem}_predictions.jsonl).",
    )
    p.add_argument(
        "--ar-data-path",
        type=str,
        default=None,
        help="ASSIST+Rebound data directory from `layup bootstrap` (passed through as --ar, "
        "and used as the predict cache dir). Default: let layup resolve its own cache.",
    )
    p.add_argument(
        "--timeout",
        type=float,
        default=900.0,
        help="Per-window fit timeout in seconds (recorded as a named failure on overrun).",
    )
    p.add_argument(
        "--weight-data",
        action=argparse.BooleanOptionalAction,
        default=True,
        help="Apply layup's Veres-2017 per-observatory astrometric weighting. ON by default, "
        "matching run_layup.py: many fixture rows carry blank rmsRA/rmsDec and layup's stock "
        "default then applies one flat sigma to every observation.",
    )
    p.add_argument(
        "--debias",
        action="store_true",
        help="Pass layup's --debias (catalog/epoch astrometry debiasing). Off by default, "
        "matching run_layup.py and the find_orb runner.",
    )
    p.add_argument(
        "--no-warm-start",
        action="store_true",
        help="Fit every window cold (own IOD). Default is to warm-start from the previous "
        "window's state, which is the A/B control for the warm-vs-fresh systematic (§2.5).",
    )
    args = p.parse_args()

    if not _HAVE_DEPS:
        print(
            f"ERROR: numpy/pandas import failed ({_DEPS_ERROR}).\n"
            "       This script must run under the layup venv's interpreter "
            "(runners/layup/.venv/bin/python); build it with `make setup-layup`.",
            file=sys.stderr,
        )
        return 1

    orbitfit_bin = _orbitfit_binary()
    if orbitfit_bin is None:
        print(
            "ERROR: `layup-orbitfit` was not found next to this interpreter and not on $PATH.\n"
            "       Run this under runners/layup/.venv/bin/python; build the venv with "
            "`make setup-layup`.\n"
            "       (Emitting empty sidecars would report 'layup predicted nothing' as a pass.)",
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
            + ".\n       layup had nothing to walk.",
            file=sys.stderr,
        )
        return 1

    version = _layup_version()
    print(
        f"layup walk-forward ({len(selected)} objects, profile {args.profile!r})",
        file=sys.stderr,
    )
    print(
        f"  layup: {orbitfit_bin} (version {version or 'unknown'})\n"
        f"  weighting: {'veres2017' if args.weight_data else 'flat_default'}"
        f"{'+debias' if args.debias else ''}  warm_start: {not args.no_warm_start}",
        file=sys.stderr,
    )

    all_windows: list[dict] = []
    all_predictions: list[dict] = []
    n_delivered = 0
    totals = {"windows": 0, "converged": 0, "failed": 0, "predictions": 0, "with_cov": 0}

    opts = Namespace(
        weight_data=args.weight_data,
        debias=args.debias,
        ar_data_path=args.ar_data_path,
        timeout=args.timeout,
        warm_start=not args.no_warm_start,
    )

    for obj in selected:
        wins, preds, stats = _walk_object(obj, fixtures_dir, args.profile, orbitfit_bin, opts)
        all_windows.extend(wins)
        all_predictions.extend(preds)
        for k in totals:
            totals[k] += stats[k]
        if wins:
            n_delivered += 1
        if stats["error"]:
            print(f"{obj['object']}: SKIPPED — {stats['error']}", file=sys.stderr)
        else:
            print(
                f"{obj['object']}: {stats['converged']}/{stats['windows']} windows converged, "
                f"{stats['predictions']} predictions ({stats['with_cov']} with covariance)",
                file=sys.stderr,
            )

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

    print(
        f"\n──── layup walk-forward summary ({args.profile}) ───────\n"
        f"  Objects walked:   {n_delivered}/{len(selected)}\n"
        f"  Windows:          {totals['windows']}  "
        f"(converged {totals['converged']}, failed {totals['failed']})\n"
        f"  Predictions:      {totals['predictions']}  "
        f"(with 2x2 covariance {totals['with_cov']}, "
        f"uncertainty_form=none {totals['predictions'] - totals['with_cov']})\n"
        f"  Thin rows:        {out} (empty by design)\n"
        f"  Windows sidecar:  {windows_path}\n"
        f"  Predictions:      {preds_path}\n"
        + "─" * 56,
        file=sys.stderr,
    )

    if n_delivered == 0:
        print(
            "ERROR: no object produced a single window record — layup walked nothing.\n"
            "       (Non-convergence is DATA and exits 0; producing no records at all is not.)",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
