#!/usr/bin/env python3
"""layup external-reference OD runner for the empyrean validation suite.

Fits every optical orbit-determination fixture in ``fixtures/psv/`` through
``layup`` — Matthew Holman's (Smithsonian / CfA) MIT-licensed, ASSIST-backed
orbit fitter — and emits one JSON record per object with layup-specific fields.
The output is consumed by ``empyrean-validation merge-external --layup``, which
folds the fields onto the ``orbit_determination`` rows of the reference channel
(the same shape as the find_orb and OrbFit external OD references).

layup is invoked as a CLI subprocess (``layup orbitfit <input> ADES_psv``) so no
layup code is imported into empyrean; it runs entirely inside its own venv (see
``setup.sh``). Its N-body fit uses ASSIST with DE441 + the SB441-N16 asteroid
set — the same dynamical model the ASSIST propagation channel uses — which makes
it a strong independent cross-check of empyrean's OD pipeline.

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

Why χ² and not an arcsec RMS: layup reports a χ² (``csq``) and its degrees of
freedom, not a post-fit residual RMS in arcsec. Deriving an arcsec figure would
need the per-observation weights layup does not emit, so — rather than fabricate
one — the χ²/reduced-χ² layup does report are folded instead.

Comparability caveat: ``csq`` depends on each tool's observation weighting,
debiasing, and astrometric error model, so layup's χ² is NOT strictly
apples-to-apples with empyrean's ``od_chi2`` (nor find_orb's / OrbFit's RMS).
The OD fixtures carry blank ``rmsRA`` / ``rmsDec``, and layup's stock default
then applies a single flat σ to every observation — not comparable to a real
per-observation OD weighting — so this runner defaults to layup's **Veres-2017**
per-observatory weighting (``--weight-data`` ON), giving a principled per-station
σ. ``--no-weight-data`` reverts to layup's flat default; ``--debias`` is off by
default (matching the find_orb runner). The recorded ``layup_weighting`` field
notes which was used. The most tool-agnostic quantity is reduced-χ² (≈1 for a
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
import json
import shutil
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


def _orbitfit_binary() -> Path | None:
    """Resolve the ``layup-orbitfit`` console script next to this interpreter.

    run_layup.py is executed by the layup venv's python (see the Makefile /
    setup.sh), so the entry point is a sibling of ``sys.executable``. We call
    the direct ``layup-orbitfit`` entry point rather than the ``layup orbitfit``
    dispatcher: the dispatcher discovers verbs by scanning ``$PATH`` for
    ``layup-*`` executables, which fails when the venv bin dir is not on
    ``$PATH`` (as when invoked by absolute path). The direct entry point has no
    such dependency.
    """
    cand = Path(sys.executable).parent / "layup-orbitfit"
    if cand.exists():
        return cand
    found = shutil.which("layup-orbitfit")
    return Path(found) if found else None


def _prepare_input(psv_path: Path, obj_token: str, work_dir: Path) -> Path | None:
    """Normalize an ADES PSV fixture into a layup-ingestible PSV.

    Strips the space padding from names and values, and prepends a constant
    ``layupID`` column so the whole file fits as one object. Returns the written
    path, or ``None`` if the file could not be parsed.
    """
    if not _HAVE_PANDAS:
        return None
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
        return None
    # Strip padding from column names and every value.
    df.columns = [c.strip() for c in df.columns]
    for col in df.columns:
        df[col] = df[col].str.strip()
    # Drop any pre-existing column that would shadow the injected primary id,
    # then prepend the constant id so it is unambiguously the first column.
    df = df.drop(columns=[c for c in df.columns if c == _PRIMARY_ID], errors="ignore")
    df.insert(0, _PRIMARY_ID, obj_token)

    out = work_dir / "layup_input.psv"
    with open(out, "w") as f:
        for line in pre_header:
            f.write(line + "\n")
        df.to_csv(f, sep="|", index=False)
    return out


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


def _fit_one(
    psv_path: Path,
    obj_token: str,
    orbitfit_bin: Path,
    ar_data_path: str | None,
    timeout_s: float,
    weight_data: bool = False,
    debias: bool = False,
) -> dict | None:
    """Run one layup orbit fit; return the layup_* payload or ``None``."""
    with tempfile.TemporaryDirectory(prefix="layup_") as td:
        work = Path(td)
        prepared = _prepare_input(psv_path, obj_token, work)
        if prepared is None:
            return None
        out_stem = work / "layup_orbit"
        cmd = [
            str(orbitfit_bin),
            str(prepared),
            "ADES_psv",
            "--primary-id-column-name",
            _PRIMARY_ID,
            "-o",
            str(out_stem),
            "-f",
        ]
        if weight_data:
            cmd.append("--weight-data")
        if debias:
            cmd.append("--debias")
        if ar_data_path:
            cmd += ["--ar", ar_data_path]

        t0 = time.perf_counter()
        try:
            proc = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                timeout=timeout_s,
                cwd=work,
            )
        except subprocess.TimeoutExpired:
            print(f"  {obj_token}: TIMEOUT after {timeout_s:.0f}s", file=sys.stderr)
            return None
        ms = (time.perf_counter() - t0) * 1000.0
        if proc.returncode != 0:
            tail = (proc.stderr or proc.stdout or "").strip().splitlines()[-3:]
            print(f"  {obj_token}: orbitfit exit {proc.returncode}: {' | '.join(tail)}", file=sys.stderr)
            return None

        row = _read_orbitfit_csv(Path(f"{out_stem}.csv"))
        if row is None:
            print(f"  {obj_token}: no fitted orbit in output", file=sys.stderr)
            return None

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
    }
    if all(s is not None for s in state):
        payload["layup_state_bcart_eq"] = state
    return payload


def _layup_version(orbitfit_bin: Path) -> str | None:
    try:
        out = subprocess.run(
            [str(sys.executable), "-c", "import layup, sys; sys.stdout.write(getattr(layup,'__version__','?'))"],
            capture_output=True,
            text=True,
            timeout=30,
        )
        v = (out.stdout or "").strip()
        return v or None
    except Exception:  # noqa: BLE001
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
        default=None,
        help="ASSIST+Rebound data directory from `layup bootstrap` (passed through as --ar). "
        "Default: let layup resolve its own bootstrap cache.",
    )
    p.add_argument(
        "--timeout",
        type=float,
        default=900.0,
        help="Per-fixture fit timeout in seconds (skip + log on overrun).",
    )
    p.add_argument(
        "--weight-data",
        action=argparse.BooleanOptionalAction,
        default=True,
        help="Apply layup's Veres-2017 per-observatory astrometric weighting (layup's "
        "--weight-data). ON by default: the OD fixtures carry blank rmsRA/rmsDec, and "
        "layup's stock default (--no-weight-data) then falls back to a single flat σ on "
        "every observation, which is not comparable to a real per-observation OD weighting. "
        "Veres-2017 gives a principled per-station σ instead. Pass --no-weight-data for "
        "stock layup flat weighting.",
    )
    p.add_argument(
        "--debias",
        action="store_true",
        help="Pass layup's --debias (catalog/epoch astrometry debiasing). Off by default "
        "(matches the find_orb reference runner, which also runs without debiasing).",
    )
    p.add_argument(
        "--only",
        type=str,
        default=None,
        help="Comma-separated object tokens (fixture stems, `/`→`_`) to fit; default all.",
    )
    args = p.parse_args()

    orbitfit_bin = _orbitfit_binary()
    psv_files = sorted(args.psv_dir.glob("*.psv"))

    only = None
    if args.only:
        only = {s.strip() for s in args.only.split(",") if s.strip()}

    timestamp = datetime.now(timezone.utc).isoformat()
    version = _layup_version(orbitfit_bin) if orbitfit_bin else None
    # Provenance stamped on every record. Reuse the version already resolved
    # from the layup venv above (the same interpreter this runs under).
    # No-hidden-fallbacks: an unresolved version stamps an explicit
    # "unknown (<reason>)" rather than a silent blank.
    source_version = (
        f"layup {version}"
        if version
        else "layup unknown (layup-orbitfit version not resolved)"
    )

    records: list[dict] = []
    if not _HAVE_PANDAS or orbitfit_bin is None:
        if orbitfit_bin is None:
            print(
                "warning: `layup-orbitfit` not found next to this interpreter; emitting empty result.",
                file=sys.stderr,
            )
        # Emit an empty (but valid) result so the merge step still has input.
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(records, indent=2))
        print(_executive_summary(records, len(psv_files)), file=sys.stderr)
        return 0

    print(f"layup OD validation ({len(psv_files)} fixtures)", file=sys.stderr)
    print(f"  layup: {orbitfit_bin} (version {version or '?'})", file=sys.stderr)

    for psv_path in psv_files:
        stem = psv_path.stem
        obj_name = stem.replace("_", "/")  # undo the safe-name encoding find_orb uses
        if only is not None and stem not in only and obj_name not in only:
            continue
        print(f"{obj_name}", file=sys.stderr)
        payload = _fit_one(
            psv_path,
            stem,
            orbitfit_bin,
            args.ar_data_path,
            args.timeout,
            weight_data=args.weight_data,
            debias=args.debias,
        )
        if payload is None:
            continue
        record = {
            "object": obj_name,
            "test_type": "orbit_determination",
            "timestamp": timestamp,
            "source_version": source_version,
            "layup_version": version,
            # Provenance for the χ² interpretation: which weighting produced csq.
            "layup_weighting": ("veres2017" if args.weight_data else "flat_default")
            + ("+debias" if args.debias else ""),
        }
        record.update(payload)
        records.append(record)
        rc = record.get("layup_reduced_chi2")
        print(
            f"  reduced χ²={rc:.3g} " if rc is not None else "  (fit reported no χ²) ",
            f"n_obs={record.get('layup_n_obs_used')} converged={record.get('layup_converged')}",
            file=sys.stderr,
        )

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(records, indent=2, default=str))
    print(f"\nWrote {len(records)} layup records to {args.output}", file=sys.stderr)
    print(_executive_summary(records, len(psv_files)), file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
