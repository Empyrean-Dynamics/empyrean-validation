#!/usr/bin/env python3
"""layup external-reference OD runner for the empyrean validation suite.

Fits every optical orbit-determination fixture in ``fixtures/psv/`` through
``layup`` — Matthew Holman's (Smithsonian / CfA) MIT-licensed, ASSIST-backed
orbit fitter — and emits one JSON record per object with layup-specific fields.
The output is consumed by ``empyrean-validation merge-external --layup``, which
folds the fields onto the ``orbit_determination`` rows of the reference channel
(the same shape as the find_orb and OrbFit external OD references).

Each fit runs layup's Python API in a subprocess (``fit_one.py``, one process per
object) under layup's own venv (see ``setup.sh``), so no layup code is imported
into empyrean. Its N-body fit uses ASSIST with the binary DE440 planets + the
SB441-N16 asteroid set — the same dynamical model the ASSIST propagation channel
uses — which makes it a strong independent cross-check of empyrean's OD pipeline.

Every file layup reads comes from its data directory (``--ar-data-path``), which
``stage_data.py`` builds from the registered kernel set; this runner refuses to
start when any file layup checks for is missing there, because layup would
otherwise download it from its upstream address mid-fit.

layup-specific fields (all keyed by object; ``test_type`` is stamped so the merge
routes them onto the optical OD rows):

    layup_chi2            — post-fit χ² (``csq`` from orbitfit)
    layup_reduced_chi2    — ``csq / ndof`` (≈1 for a consistent fit)
    layup_n_obs_used      — fittable-observation count (``nobs_fit``)
    layup_converged       — bool; the fit was well-conditioned (``flag == 0``)
    layup_time_ms         — wall-clock per fit
    layup_state_bcart_eq  — fitted barycentric Cartesian equatorial state
                             [x,y,z,xdot,ydot,zdot] (au, au/day) — carried for
                             provenance; not folded onto the ValidationResult
    layup_epoch_mjd_tdb   — fit epoch (MJD TDB)
    layup_niter           — LM iteration count
    layup_method          — IOD/fit method layup reported
    layup_flag            — raw fit flag (0 == success)
    layup_nongrav_adopted — the A1/A2/A3 terms the auto ladder adopted ({} = gravity only)
    layup_sigma_source    — reported / filled counts and the fill level of each filled sigma
    layup_settings        — every setting, each citing layup (``LAYUP_SETTINGS``)
    layup_version         — the installed layup distribution
    layup_error           — on a failed row only: the reason (exit tail, timeout)
    layup_unsupported     — on a row layup cannot fit at all: the reason (self-perturbers)

Why χ² and not an arcsec RMS: layup reports a χ² (``csq``) and its degrees of
freedom, not a post-fit residual RMS in arcsec. Deriving an arcsec figure would
need the per-observation weights layup does not emit, so — rather than fabricate
one — the χ²/reduced-χ² layup does report are folded instead.

Comparability caveat: ``csq`` depends on each tool's observation weighting,
debiasing, and astrometric error model, so layup's χ² is NOT strictly
apples-to-apples with empyrean's ``od_chi2`` (nor find_orb's / OrbFit's RMS).
Settings (owner ruling D4: each tool at its own documented best practice, fed
the same data products). Every fit runs through ``fit_one.py``, layup's Python API
(the CLI cannot express these): ``weight_data="supplied"`` uses the per-observation
``rmsRA`` / ``rmsDec``, which hold the fixture's REPORTED sigma where it has one
(about a third of the optical rows carry one) and the shared public Vereš 2017
value (``runners/sigmafill``) where it does not; ``debias=True`` applies EFCC18;
``fit_nongrav="auto"`` is layup's adaptive A1/A2/A3 ladder. The row's
``layup_settings`` (``LAYUP_SETTINGS``, each value citing layup) and
``layup_sigma_source`` state all of this; ``run_layup_walk.py`` fits through the
same driver and the same input adapter. The most tool-agnostic quantity is reduced-χ² (≈1 for a
statistically consistent fit regardless of the weighting scale). Likewise
``nobs_fit`` is layup's own fittable-observation count; whether it counts
observations or residuals is layup's convention and may differ from
empyrean/find_orb's observation count.

Input handling: the ADES PSV fixtures are space-padded and lead with a ``permID``
column that is blank for many comets / recently-designated objects, so layup's
default (first column == the primary-id column, consistently populated) does not
hold. Because each fixture file is exactly one object, a small adapter strips the
padding and injects a constant ``layupID`` first column, guaranteeing every
observation groups into a single fit.

Design — one fit per subprocess (not layup's native batch): layup is built to
fit many objects in a single process (one input file grouped by primary id,
fanned out over ``--num-workers`` in ``--chunksize`` blocks), which amortizes its
heavy cold start (importing jax/numba/sorcha, JIT, SPICE-kernel load, ASSIST
init — a ~3 s per-process floor). This runner deliberately does NOT batch, for
two reasons: (1) **individual fit times** — layup's output carries no per-object
time column, so a batch reports only a wall-clock total, whereas per-fixture
subprocesses give a real (if cold-start-dominated) ``layup_time_ms`` per object;
and (2) **crash isolation** — layup fits via ``concurrent.futures`` +
``np.concatenate([f.result() …])``, so one bad object (e.g. an internal
``np.sort`` type error, or a missing-obscode / satellite-``sys`` ingest error)
aborts the whole batch; separate subprocesses contain the failure to that one
object. The cost is the repeated cold start, so ``layup_time_ms`` here is
cold-subprocess wall clock (dominated by startup), not a fit-kernel benchmark.

Radar OD (``fixtures/psv-radar/``) is out of scope for this first integration —
this runner does optical OD only, matching the ``orbit_determination`` rows.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import signal
import statistics
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path

try:
    import pandas as pd

    _HAVE_PANDAS = True
except Exception as e:  # noqa: BLE001
    print(f"warning: pandas import failed: {e}", file=sys.stderr)
    _HAVE_PANDAS = False


# Primary-id column injected into every prepared fixture. Named distinctly so it
# never collides with the fixtures' own (mostly-blank) provID / permID columns.
_PRIMARY_ID = "layupID"

# layup's data directory as `make run-layup` stages it (stage_data.py).
_STAGED_DATA = Path(__file__).resolve().parent / "data"


sys.path.insert(0, str(Path(__file__).resolve().parent.parent))  # runners/ -> sigmafill
from sigmafill.veres2017 import Veres2017SigmaModel, fill_missing  # noqa: E402

FIT_ONE = Path(__file__).resolve().parent / "fit_one.py"

# Identification, photometry and free-text columns the fit does not use. Dropped
# from the prepared input because layup's `np.sort(data, order="obsTime")`
# (layup/orbitfit.py:1421) breaks timestamp ties by comparing the remaining
# fields, and an object column that mixes strings with the NaN pandas reads for a
# blank makes that comparison raise TypeError: Apophis (4 duplicate obsTimes) failed
# every time. Nothing here feeds the fit under weight_data="supplied".
UNUSED_COLUMNS = ("provID", "trkSub", "prog", "band", "photCat", "notes", "remarks")

# Iris, Vesta, Pallas and Hygiea are themselves in SB441-N16, and layup's ASSIST
# force model takes the whole set with no way to leave a body out, so a fit of one
# of them pulls the object toward its own ephemeris (a 1/r^2 self-singularity).
# Fitted anyway, none converges (flag 3 for Vesta and Hygiea, 4 for Iris and
# Pallas). They are named unsupported rows instead, the rule the adam-core runner
# follows for the same reason (owner ruling 2026-11-08), never a self-perturbed fit.
SELF_PERTURBERS = frozenset({"Iris", "Vesta", "Pallas", "Hygiea"})
SELF_PERTURBER_UNSUPPORTED = (
    "self-perturber: SB441-N16 includes the object and layup 1.1.0's ASSIST force "
    "model cannot leave a body out, so the object would perturb itself"
)


def unsupported_reason(obj_name: str) -> str | None:
    """Why layup cannot fit this object at its settings, or None when it can."""
    return SELF_PERTURBER_UNSUPPORTED if obj_name in SELF_PERTURBERS else None


# layup's own documented best-practice settings, recorded on every row. fit_one.py
# makes the call; each value cites where layup 1.1.0 documents it.
LAYUP_SETTINGS = {
    "interface": "layup 1.1.0 Python API, layup.orbitfit.orbitfit, one process per object (fit_one.py); "
    "the CLI cannot set weight_data='supplied' or fit_nongrav",
    "weight_data": "supplied: each observation's rmsRA/rmsDec used directly (layup orbitfit docstring: "
    "'e.g. ADES-reported uncertainties')",
    "sigma_fill": "reported rmsRA/rmsDec kept; a missing one filled by the shared public Veres et al. 2017 "
    "model (runners/sigmafill), so no observation falls to layup's flat C++ default",
    "debias": "EFCC18 star-catalog debiasing, debias=True (layup's high-resolution bias.dat, from the "
    "registered debias_hires2018.tgz)",
    "fit_nongrav": "auto: layup's adaptive ladder (issue #357; A2, A1, A3, then A1+A2, then A1+A2+A3) at its "
    "default NongravAutoThresholds (tried only above reduced chi2 1.5; adopted at delta chi2 >= 9 per "
    "parameter and 3 sigma each)",
    "nongrav_gr": "layup's default asteroidal (r/r0)^-2 for every object; its Marsden 1973 water-ice law is "
    "not selected for comets, because choosing a law by designation is barred",
    "iod": "auto (Gauss, BK-IOD fallback; layup default)",
    "engine": "cartesian (layup default)",
    "force_model": "ASSIST: DE440 planets + SB441-N16 asteroids (the registered layup/ folder)",
    "self_perturbers": "Iris, Vesta, Pallas, Hygiea: named unsupported (SB441-N16 includes each; "
    "layup cannot leave a body out of its force model)",
    "input_columns_dropped": "provID, trkSub, prog, band, photCat, notes, remarks (unused by the fit; a "
    "string/NaN mix there makes layup's obsTime tie-break sort raise, orbitfit.py:1421)",
}


def _prepare_input(psv_path: Path, obj_token: str, work_dir: Path):
    """Normalize an ADES PSV fixture into a layup-ingestible PSV.

    Strips the space padding from names and values, and prepends a constant
    ``layupID`` column so the whole file fits as one object. Observations with no
    reported ``rmsRA`` / ``rmsDec`` get the shared public Vereš 2017 value written
    into those columns (a reported value is never touched), so layup's
    ``weight_data="supplied"`` sees a sigma on every row. Returns
    ``(written path, sigma report)``, or ``(None, None)`` if the file could not be
    parsed.
    """
    if not _HAVE_PANDAS:
        return None, None
    # Count leading comment lines (e.g. `# version=2017`) so pandas reads the
    # true header row, and re-emit them so the output stays valid ADES.
    pre_header: list[str] = []
    with open(psv_path) as fh:
        for line in fh:
            if line.startswith(("#", "!")):
                pre_header.append(line.rstrip("\n"))
            else:
                break
    try:
        df = pd.read_csv(
            psv_path,
            sep="|",
            skiprows=list(range(len(pre_header))),
            dtype=str,
            keep_default_na=False,
        )
    except Exception as e:  # noqa: BLE001
        print(f"  {obj_token}: PSV parse failed: {e}", file=sys.stderr)
        return None, None
    # Strip padding from column names and every value.
    df.columns = [c.strip() for c in df.columns]
    for col in df.columns:
        df[col] = df[col].str.strip()
    # Drop any pre-existing column that would shadow the injected primary id,
    # then prepend the constant id so it is unambiguously the first column.
    df = df.drop(columns=[c for c in df.columns if c == _PRIMARY_ID], errors="ignore")
    df.insert(0, _PRIMARY_ID, obj_token)
    df, report = _fill_sigma(df)
    df = df.drop(columns=[c for c in UNUSED_COLUMNS if c in df.columns])

    out = work_dir / "layup_input.psv"
    with open(out, "w") as f:
        for line in pre_header:
            f.write(line + "\n")
        df.to_csv(f, sep="|", index=False)
    return out, report


def missing_data(ar_data_path: str) -> list[str]:
    """Every file layup checks for that the data directory lacks (empty: ready).

    layup checks for every data file before it fits and downloads a missing one
    from the address its release names, so a runner refuses to start instead.
    Shared by the OD, walk and Monte-Carlo runners.
    """
    from stage_data import Layout, verify

    return verify(Path(ar_data_path), Layout.from_layup())


def _num(v: str):
    try:
        f = float(v)
    except (TypeError, ValueError):
        return None
    return f if f == f else None


def _fill_sigma(df):
    """Write the shared Vereš 2017 value into rmsRA/rmsDec where none was reported.

    A reported value is never overwritten; a row with only one of the two keeps it
    and gets the other filled. Returns ``(df, report)`` with the reported / filled
    counts and the fill level of every filled observation.
    """
    for col in ("rmsRA", "rmsDec"):
        if col not in df.columns:
            df[col] = ""
    stn = df["stn"].tolist()
    cat = df["astCat"].tolist() if "astCat" in df.columns else [""] * len(df)
    ra = [_num(v) for v in df["rmsRA"]]
    dec = [_num(v) for v in df["rmsDec"]]
    fra, fdec, report = fill_missing(stn, cat, ra, dec, _SIGMA_MODEL)
    df["rmsRA"] = [repr(v) for v in fra]
    df["rmsDec"] = [repr(v) for v in fdec]
    return df, report


_SIGMA_MODEL = Veres2017SigmaModel.from_csv()


def _read_orbitfit_csv(csv_path: Path) -> dict | None:
    """Read the single fitted-orbit row layup's ``orbitfit`` writes.

    The output schema (see layup ``_get_result_dtypes``) carries ``csq`` /
    ``ndof`` / the barycentric-Cartesian state / ``epochMJD_TDB`` / ``niter`` /
    ``method`` / ``flag`` / ``nobs_fit`` plus a flat 36-element covariance. Read
    by column name (robust to the covariance columns in between). Returns the
    first data row as a dict, or ``None`` if the file is empty / unreadable.
    """
    try:
        df = pd.read_csv(csv_path)
    except Exception as e:  # noqa: BLE001
        print(f"  orbitfit output parse failed: {e}", file=sys.stderr)
        return None
    if len(df) == 0:
        return None
    return df.iloc[0].to_dict()


def layup_env(ar_data_path: str | None) -> dict[str, str]:
    """The environment layup runs in: its cache directory IS the staged data directory.

    ``--ar`` moves only the ASSIST data. layup 1.1 still reads ``naif0012.tls``
    (and the other SPICE kernels) from its per-user cache in several fit-path
    functions, so a run that passes ``--ar`` alone fits on a machine whose
    user cache happens to hold the files and fails with "SPICE kernel file
    'naif0012.tls' not found" on one that does not (a CI runner). ``LAYUP_CACHE_DIR``
    is layup's own override for that directory; pointing it at the staged
    directory makes the run read the staged files and nothing else.
    """
    env = dict(os.environ)
    if ar_data_path:
        env["LAYUP_CACHE_DIR"] = str(ar_data_path)
    return env


def run_bounded(cmd: list[str], timeout_s: float, cwd: Path, env: dict[str, str]):
    """``subprocess.run`` whose timeout also ends the fit's own children.

    layup fits through a spawn-context process pool even with one worker.
    ``subprocess.run`` kills only the direct child on a timeout, so the pool's
    child, orphaned, would go on fitting (CPU and memory) for the rest of the run.
    Each fit runs in a session of its own and the whole session is killed on a
    timeout or any other interruption. Raises ``subprocess.TimeoutExpired`` like
    ``subprocess.run``.
    """
    proc = subprocess.Popen(
        cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        cwd=cwd, env=env, start_new_session=True,
    )
    try:
        out, err = proc.communicate(timeout=timeout_s)
    except BaseException:
        os.killpg(proc.pid, signal.SIGKILL)
        proc.communicate()
        raise
    return subprocess.CompletedProcess(cmd, proc.returncode, out, err)


def _fit_one(
    psv_path: Path,
    obj_token: str,
    ar_data_path: str,
    timeout_s: float,
):
    """Run one layup fit through its API driver. Returns ``(payload, None)`` on
    success or ``(None, reason)``; a failure is a named row, never a skip."""
    with tempfile.TemporaryDirectory(prefix="layup_") as td:
        work = Path(td)
        prepared, sigma_report = _prepare_input(psv_path, obj_token, work)
        if prepared is None:
            return None, "PSV fixture could not be parsed"
        out_csv = work / "layup_orbit.csv"
        cmd = [
            sys.executable, str(FIT_ONE), str(prepared),
            "--ar", ar_data_path, "--primary-id", _PRIMARY_ID, "--out", str(out_csv),
        ]
        t0 = time.perf_counter()
        try:
            proc = run_bounded(cmd, timeout_s, work, layup_env(ar_data_path))
        except subprocess.TimeoutExpired:
            print(f"  {obj_token}: TIMEOUT after {timeout_s:.0f}s", file=sys.stderr)
            return None, f"timed out after {timeout_s:.0f}s"
        ms = (time.perf_counter() - t0) * 1000.0
        if proc.returncode != 0:
            tail = (proc.stderr or proc.stdout or "").strip().splitlines()[-3:]
            reason = f"orbitfit exit {proc.returncode}: {' | '.join(tail)}"[:400]
            print(f"  {obj_token}: {reason}", file=sys.stderr)
            return None, reason

        row = _read_orbitfit_csv(out_csv)
        if row is None:
            print(f"  {obj_token}: no fitted orbit in output", file=sys.stderr)
            return None, "orbitfit wrote no fitted orbit"

    def _f(key: str) -> float | None:
        v = row.get(key)
        try:
            f = float(v)
        except (TypeError, ValueError):
            return None
        return f if f == f else None  # drop NaN

    def _i(key: str) -> int | None:
        f = _f(key)
        return int(f) if f is not None else None

    csq = _f("csq")
    ndof = _f("ndof")
    reduced = csq / ndof if (csq is not None and ndof and ndof > 0) else None
    flag = _i("flag")
    state = [_f(k) for k in ("x", "y", "z", "xdot", "ydot", "zdot")]

    payload: dict = {
        "layup_chi2": csq,
        "layup_reduced_chi2": reduced,
        "layup_n_obs_used": _i("nobs_fit"),
        "layup_converged": (flag == 0) if flag is not None else None,
        "layup_time_ms": ms,
        "layup_epoch_mjd_tdb": _f("epochMJD_TDB"),
        "layup_niter": _i("niter"),
        "layup_method": (str(row.get("method")) if row.get("method") is not None else None),
        "layup_flag": flag,
        "layup_sigma_source": sigma_report,
    }
    # The non-grav model layup's auto ladder adopted (NaN = not adopted).
    adopted = {n: _f(n.lower()) for n in ("A1", "A2", "A3") if _f(n.lower()) is not None}
    payload["layup_nongrav_adopted"] = adopted
    if all(s is not None for s in state):
        payload["layup_state_bcart_eq"] = state
    return payload, None


def _layup_version() -> str | None:
    """The installed layup distribution's version (setup.sh pins the release).

    ``layup.__version__`` is the literal ``"unknown version"`` in a PyPI build,
    so the distribution metadata is the only source that identifies the fitter.
    """
    from importlib import metadata

    try:
        return metadata.version("layup")
    except metadata.PackageNotFoundError:
        return None


def _executive_summary(records: list[dict], n_files: int) -> str:
    fitted = [r for r in records if r.get("layup_chi2") is not None]
    conv = [r for r in fitted if r.get("layup_converged")]

    def med(vals: list[float]) -> str:
        return f"{statistics.median(vals):.3g}" if vals else "—"

    lines = []
    lines.append("\n──── layup external-reference OD summary ───────────────")
    lines.append(f"  Fixtures found:   {n_files}")
    lines.append(f"  Orbits fitted:    {len(fitted)}")
    lines.append(f"  Converged (flag0):{len(conv)}")
    if fitted:
        rchi2 = [r["layup_reduced_chi2"] for r in fitted if r.get("layup_reduced_chi2") is not None]
        nobs = [r["layup_n_obs_used"] for r in fitted if r.get("layup_n_obs_used") is not None]
        times = [r["layup_time_ms"] for r in fitted if r.get("layup_time_ms") is not None]
        lines.append(f"    reduced χ²      median={med(rchi2)}")
        lines.append(f"    n_obs used      median={med([float(x) for x in nobs])}")
        if times:
            lines.append(f"    wall clock      median={statistics.median(times) / 1000:.1f} s   max={max(times) / 1000:.1f} s")
    if not _HAVE_PANDAS:
        lines.append("  NOTE: pandas unavailable; all fixtures skipped. Run `./setup.sh` first.")
    lines.append("─" * 56)
    return "\n".join(lines)


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("psv_dir", type=Path, help="Directory of ADES PSV OD fixtures (fixtures/psv/)")
    p.add_argument(
        "--output",
        "-o",
        type=Path,
        default=Path("results/validation_layup.json"),
        help="Output JSON path",
    )
    p.add_argument(
        "--ar-data-path",
        type=str,
        default=str(_STAGED_DATA),
        help="layup's data directory, staged from the registered kernel set by stage_data.py "
        "(passed through as --ar). Every file layup checks for must be present: layup "
        "downloads a missing one at fit time, which this runner refuses to allow. "
        f"Default: {_STAGED_DATA.name}/ beside this script (what `make run-layup` stages).",
    )
    p.add_argument(
        "--timeout",
        type=float,
        default=900.0,
        help="Per-fixture fit timeout in seconds (skip + log on overrun).",
    )
    p.add_argument(
        "--only",
        type=str,
        default=None,
        help="Comma-separated object tokens (fixture stems, `/`→`_`) to fit; default all.",
    )
    args = p.parse_args()

    have_layup = importlib.util.find_spec("layup") is not None
    psv_files = sorted(args.psv_dir.glob("*.psv"))
    if not psv_files:
        # Without this, an empty fixture directory produced `[]` and exit 0:
        # reduce folded the file in because it EXISTED, and the report showed
        # a layup comparison that had compared nothing. The Makefile's
        # `$(LAYUP_OUT): | fixtures` prereq covers the `make` path, but
        # runners/README.md documents invoking this script directly, which
        # bypasses it — and since the fixtures left git, "directory present,
        # zero .psv" is the DEFAULT state of a fresh checkout rather than an
        # anomaly. A comparator with nothing to compare is a failure with a
        # cause, and the cause is named here (mirrors run_findorb.py).
        print(
            f"ERROR: no PSV files found in {args.psv_dir}\n"
            "       layup had nothing to fit. The fixtures come from the GCS "
            "snapshot pinned by fixtures/manifest.json;\n"
            "       `make fixtures` fetches + verifies them.",
            file=sys.stderr,
        )
        return 1

    only = None
    if args.only:
        only = {s.strip() for s in args.only.split(",") if s.strip()}

    timestamp = datetime.now(timezone.utc).isoformat()
    version = _layup_version()
    # Provenance stamped on every record. Reuse the version already resolved
    # from the layup venv above (the same interpreter this runs under).
    # No-hidden-fallbacks: an unresolved version stamps an explicit
    # "unknown (<reason>)" rather than a silent blank.
    source_version = (
        f"layup {version}"
        if version
        else "layup unknown (distribution version not resolved)"
    )

    records: list[dict] = []
    if not _HAVE_PANDAS or not have_layup:
        # This used to emit `[]` and return 0 "so the merge step still has
        # input". That is the shape this suite exists to catch: reduce folds the
        # file in because it EXISTS, and the report shows a layup comparison
        # that compared nothing, indistinguishable from one that agreed. The
        # Makefile deleted its own `echo '[]'` fallback for exactly this reason,
        # and run_findorb.py already fails on a missing binary — layup was the
        # inconsistent sibling. A comparator with nothing to compare is a
        # failure with a cause, and the cause is named here.
        missing = []
        if not have_layup:
            missing.append(f"layup does not import under this interpreter ({sys.executable})")
        if not _HAVE_PANDAS:
            missing.append("pandas failed to import (needed to normalize the PSV fixtures)")
        print(
            "ERROR: layup cannot run: " + "; ".join(missing) + ".\n"
            "       Build the environment with `make setup-layup`.\n"
            "       (Emitting an empty result would report 'layup compared nothing' as a pass.)",
            file=sys.stderr,
        )
        return 1

    # A missing data file would be downloaded mid-fit; refuse, naming the files.
    # Absolute: each fit runs with its scratch directory as the working directory.
    args.ar_data_path = str(Path(args.ar_data_path).absolute())
    missing = missing_data(args.ar_data_path)
    if missing:
        print(
            f"ERROR: layup's data directory {args.ar_data_path} lacks: {', '.join(missing)}.\n"
            "       Stage it from the registered kernel set first:\n"
            f"         {sys.executable} {Path(__file__).with_name('stage_data.py')} "
            f"--data-dir <synced set> --dest {args.ar_data_path}\n"
            "       (`make run-layup` does this before every run.)",
            file=sys.stderr,
        )
        return 1

    print(f"layup OD validation ({len(psv_files)} fixtures)", file=sys.stderr)
    print(f"  data:  {args.ar_data_path}", file=sys.stderr)
    print(f"  layup: {version or '?'} through {FIT_ONE.name} under {sys.executable}", file=sys.stderr)

    for psv_path in psv_files:
        stem = psv_path.stem
        obj_name = stem.replace("_", "/")  # undo the safe-name encoding find_orb uses
        if only is not None and stem not in only and obj_name not in only:
            continue
        print(f"{obj_name}", file=sys.stderr)
        record = {
            "object": obj_name,
            "test_type": "orbit_determination",
            "timestamp": timestamp,
            "source_version": source_version,
            "layup_version": version,
            # Every setting that shaped csq, each citing where layup documents it.
            "layup_settings": LAYUP_SETTINGS,
        }
        unsupported = unsupported_reason(obj_name)
        if unsupported is not None:
            # A named unsupported row: never fitted, never dropped.
            print(f"  {unsupported}", file=sys.stderr)
            record["layup_converged"] = False
            record["layup_unsupported"] = unsupported
            records.append(record)
            continue
        payload, failure = _fit_one(psv_path, stem, args.ar_data_path, args.timeout)
        if payload is None:
            # A named failed row, never a dropped object.
            record["layup_converged"] = False
            record["layup_error"] = failure
            records.append(record)
            continue
        record.update(payload)
        records.append(record)
        rc = record.get("layup_reduced_chi2")
        print(
            f"  reduced χ²={rc:.3g} " if rc is not None else "  (fit reported no χ²) ",
            f"n_obs={record.get('layup_n_obs_used')} converged={record.get('layup_converged')}",
            file=sys.stderr,
        )

    args.output.parent.mkdir(parents=True, exist_ok=True)
    try:
        _payload = json.dumps(records, indent=2, default=str, allow_nan=False)
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
    print(f"\nWrote {len(records)} layup records to {args.output}", file=sys.stderr)
    print(_executive_summary(records, len(psv_files)), file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
