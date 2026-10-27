"""Kete external-reference runner for the empyrean validation suite.

Reads the canonical test plan (`validation_plan.json`) and replays each row
through `kete` — Dar Dahlen's open-source NEO toolkit (originally developed
at Caltech IPAC for NEO Surveyor mission simulation work; now an independent
personal project at github.com/dahlend/kete). Emits one row per input row
with kete-specific fields populated, plus an executive-summary print at the
end. Output JSON is opt-in for the validation report (not in the headline
ASSIST / OrbFit / OpenOrb / find_orb set) — kete is a sanity-check sibling
covering propagation, ephemeris, and OD in one tool.

Schema mirrors the ValidationResult shape used by every other channel.
Kete-specific fields:
    kete_pos_au              — propagated/fitted Cartesian state at t_mjd_tdb
    kete_vs_horizons_km      — |kete_pos - ref_pos| (propagation rows)
    kete_separation_arcsec   — angular separation vs Horizons (ephemeris rows)
    kete_d_ra_arcsec         — dRA·cos(Dec) vs Horizons (ephemeris rows)
    kete_d_dec_arcsec        — dDec vs Horizons (ephemeris rows)
    kete_d_rho_km            — range diff vs Horizons (ephemeris rows)
    kete_time_ms             — wall-clock per row
    kete_od_converged        — bool (OD rows)
    kete_od_rms_ra_arcsec    — post-fit RMS of dRA·cos(Dec), included obs (OD rows)
    kete_od_rms_dec_arcsec   — post-fit RMS of dDec, included obs (OD rows)
    kete_od_rms_weighted     — kete's reduced weighted RMS (OD rows)
    kete_od_n_records        — data rows in the pinned PSV (OD rows)
    kete_od_n_obs_ingested   — rows that became kete observations (OD rows)
    kete_od_dropped          — {reason: count} for rows that did not (OD rows)
    kete_od_n_obs_used       — observations kept after outlier rejection (OD rows)
    kete_od_fit_epoch_mjd_tdb — epoch kete fitted at, before propagation to
                               t_mjd_tdb (OD rows)
    kete_od_error            — why an OD row has no fit (OD failure rows)

kete exposes no iteration count or chi-squared for a fit, so OD rows carry
neither.

Orbit determination fits the pinned ADES PSV fixture offline: `ades_psv`
turns its rows into kete observations under kete's own ADES ingestion rules
(observatory table, sigmas, EFCC18 debiasing from the data directory's
`bias.dat`), then kete's initial_orbit_determination seeds fit_orbit. The
fitted state is propagated to the row's epoch and reported SSB-centred in
the equatorial (ICRF) frame, like `emp_pos_au`. An OD row kete cannot fit is
emitted with `kete_od_error` naming the cause.

Propagation and ephemeris rows are best-effort: when kete cannot process
one, the row is emitted without `kete_*` fields and a note is logged to
stderr.
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

import ades_psv

try:
    import kete
    _HAVE_KETE = True
except Exception as e:
    print(f"warning: kete import failed: {e}", file=sys.stderr)
    _HAVE_KETE = False


_AU_KM = 149_597_870.700


def _kete_source_version() -> str:
    """Provenance string stamped on every kete-channel row.

    Reports the installed ``kete`` distribution version. No-hidden-fallbacks:
    when kete is absent or its version can't be read, stamp an explicit
    ``kete unknown (<reason>)`` rather than a silent blank.
    """
    try:
        return f"kete {version('kete')}"
    except PackageNotFoundError:
        return "kete unknown (distribution not found)"
    except Exception as e:  # noqa: BLE001
        return f"kete unknown ({e})"


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


def _kete_state(epoch_mjd_tdb: float, pos_au, vel_au_d):
    """Build a kete State for a Cartesian SSB-centered IC in AU/AU·d⁻¹.

    Empyrean's plan stores ic_pos_au / ic_vel_au_d in the ICRF frame
    relative to the Solar System Barycenter (NAIF id 0). Kete defaults
    to Sun-centered (id 10); we explicitly pass center_id=0 and the
    Equatorial frame.
    """
    jd_tdb = epoch_mjd_tdb + 2_400_000.5
    return kete.State(
        desig="empyrean_test",
        jd=jd_tdb,
        pos=kete.Vector(list(pos_au), kete.Frames.Equatorial),
        vel=kete.Vector(list(vel_au_d), kete.Frames.Equatorial),
        frame=kete.Frames.Equatorial,
        center_id=0,
    )


# kete's include_asteroids force set is Ceres, Pallas, Interamnia, Hygiea and
# Vesta — three of which the suite propagates AS test objects. kete has no
# per-body exclusion, so for those objects the massive-asteroid set would
# include the object itself (a 1/r² self-singularity, the same failure mode
# measured at ~7e8 km/30 d with ASSIST). Propagate them planets-only.
_KETE_MASSIVE = {"Vesta", "Pallas", "Hygiea"}


def _kete_prop_opts(row: dict) -> dict:
    """include_asteroids + non_gravs options for one plan row.

    Non-grav: the plan's Marsden A1/A2/A3 + g(r) (alpha, r0, m, n, k) + dt
    delay map 1:1 onto kete's NonGravModel.new_comet — without this, comets
    propagate gravity-only while Empyrean models the full non-grav term
    (1e3–1e5 km of apples-to-oranges over ±15 yr).
    """
    opts: dict = {"include_asteroids": row.get("object") not in _KETE_MASSIVE}
    if not opts["include_asteroids"]:
        print(
            f"  {row.get('object')}: kete runs planets-only "
            "(object is in kete's massive-asteroid set; no per-body exclusion)",
            file=sys.stderr,
        )
    a1 = row.get("ic_a1") or 0.0
    a2 = row.get("ic_a2") or 0.0
    a3 = row.get("ic_a3") or 0.0
    if a1 != 0.0 or a2 != 0.0 or a3 != 0.0:
        opts["non_gravs"] = [
            kete.propagation.NonGravModel.new_comet(
                a1=a1,
                a2=a2,
                a3=a3,
                alpha=row.get("ic_g_alpha") or 1.0,
                r_0=row.get("ic_g_r0") or 1.0,
                m=row.get("ic_g_m") or 2.0,
                n=row.get("ic_g_n") or 0.0,
                k=row.get("ic_g_k") or 0.0,
                dt=row.get("ic_non_grav_dt") or 0.0,
            )
        ]
    return opts


def _propagate_with_kete(row: dict) -> dict | None:
    if not _HAVE_KETE:
        return None
    ic_pos = row.get("ic_pos_au")
    ic_vel = row.get("ic_vel_au_d")
    if ic_pos is None or ic_vel is None:
        return None
    target_jd = row["t_mjd_tdb"] + 2_400_000.5
    t0 = time.perf_counter()
    try:
        state = _kete_state(row["epoch_mjd_tdb"], ic_pos, ic_vel)
        out = kete.propagate_n_body([state], target_jd, **_kete_prop_opts(row))
    except Exception as e:
        print(f"  {row['object']} dt={row.get('dt_days', 0):+.0f} prop FAIL: {e}", file=sys.stderr)
        return None
    ms = (time.perf_counter() - t0) * 1000.0
    if not out:
        return None
    final = out[0]
    if not final.is_finite:
        return None
    # Kete normalizes State internally to the Ecliptic frame regardless
    # of the `frame=` arg; convert back to Equatorial to compare against
    # empyrean's ICRF (= J2000 equatorial) reference vectors.
    pos = list(final.as_equatorial.pos)
    res: dict = {"kete_pos_au": pos, "kete_time_ms": ms}
    ref = row.get("ref_pos_au")
    if ref is not None:
        d = math.sqrt(sum((pos[i] - ref[i]) ** 2 for i in range(3)))
        res["kete_vs_horizons_km"] = d * _AU_KM
    return res


def _ephemeris_with_kete(row: dict) -> dict | None:
    if not _HAVE_KETE:
        return None
    ic_pos = row.get("ic_pos_au")
    ic_vel = row.get("ic_vel_au_d")
    obs_code = row.get("observer")
    if ic_pos is None or ic_vel is None or not obs_code:
        return None
    target_jd = row["t_mjd_tdb"] + 2_400_000.5
    t0 = time.perf_counter()
    try:
        state = _kete_state(row["epoch_mjd_tdb"], ic_pos, ic_vel)
        prop = kete.propagate_n_body([state], target_jd, **_kete_prop_opts(row))
        if not prop or not prop[0].is_finite:
            return None
        # Observer state at the target epoch — re-center to SSB (NAIF 0)
        # so the line-of-sight subtraction below is in one frame. Note
        # that State.change_center returns a NEW state (it does not
        # mutate). mpc_code_to_ecliptic defaults to Sun-centered (NAIF 10).
        obs_state = kete.spice.mpc_code_to_ecliptic(obs_code, target_jd)
        obs_state = obs_state.change_center(0)
        obs_state_eq = obs_state.as_equatorial
        ox, oy, oz = obs_state_eq.pos.x, obs_state_eq.pos.y, obs_state_eq.pos.z
        # Astrometric direction: iterate the light-time correction (the
        # object is seen where it WAS τ = ρ/c ago). A same-instant geometric
        # subtraction is off by v·τ ≈ 15-20″ at 1-2 AU — measured as a
        # uniform ~19″ offset vs the Horizons astrometric reference before
        # this fix. The τ back-step uses two-body propagation from the
        # n-body state, which is exact to far below a µas over ~15 min.
        c_au_per_day = 173.144632674240
        tau = 0.0
        vec = None
        for _ in range(3):
            lagged = kete.propagate_two_body([prop[0]], target_jd - tau)[0].as_equatorial
            tx, ty, tz = lagged.pos.x, lagged.pos.y, lagged.pos.z
            vec = kete.Vector([tx - ox, ty - oy, tz - oz], kete.Frames.Equatorial)
            tau = vec.r / c_au_per_day
        # Vector.ra / .dec are degrees; convert to radians to match
        # empyrean's ref_ra_rad / ref_dec_rad units.
        ra_rad = math.radians(vec.ra)
        dec_rad = math.radians(vec.dec)
        rho_au = vec.r
    except Exception as e:
        print(f"  {row['object']} dt={row.get('dt_days', 0):+.0f} eph FAIL: {e}", file=sys.stderr)
        return None
    # kete can return a non-finite position without raising — the light-time
    # loop above diverges rather than throwing. Propagating that onward turned
    # every derived comparison NaN (separation, dRA, dDec, dRho) and, because
    # Python's json.dump writes a bare `NaN` that is not valid JSON, the failure
    # surfaced two jobs later as `Error("expected value", line: 4790)` when the
    # Rust merger parsed the channel file. A position that is not finite is a
    # FAILED row with a cause, handled the same way as a raised exception.
    if not all(math.isfinite(v) for v in (ra_rad, dec_rad, rho_au)):
        print(
            f"  {row['object']} dt={row.get('dt_days', 0):+.0f} eph FAIL: "
            f"kete returned a non-finite position (ra={ra_rad}, dec={dec_rad}, rho={rho_au}); "
            "refusing to emit NaN comparisons",
            file=sys.stderr,
        )
        return None
    ms = (time.perf_counter() - t0) * 1000.0
    res: dict = {"kete_time_ms": ms}
    ref_ra = row.get("ref_ra_rad")
    ref_dec = row.get("ref_dec_rad")
    if ref_ra is not None and ref_dec is not None:
        sep_arcsec = _angular_sep_arcsec(ra_rad, dec_rad, ref_ra, ref_dec)
        res["kete_separation_arcsec"] = sep_arcsec
        res["kete_d_ra_arcsec"] = math.degrees((ra_rad - ref_ra) * math.cos(dec_rad)) * 3600.0
        res["kete_d_dec_arcsec"] = math.degrees(dec_rad - ref_dec) * 3600.0
    ref_rho = row.get("ref_rho_au")
    if ref_rho is not None:
        res["kete_d_rho_km"] = (rho_au - ref_rho) * _AU_KM
    return res


def _load_debias(data_dir: Path):
    """kete's EFCC18 debias table from the data directory, or why it is missing.

    Loaded once per run. A failure is returned as a message that every OD row
    then carries as its `kete_od_error`: kete's ADES ingestion debiases every
    record, so a fit without the table would be a different fit, not a
    degraded one.
    """
    try:
        return ades_psv.load_debias_table(data_dir)
    except Exception as e:  # noqa: BLE001
        return f"EFCC18 debias table unavailable: {type(e).__name__}: {e}"


def _od_failure(row: dict, msg: str, extra: dict | None = None) -> dict:
    print(f"  {row['object']} OD FAIL: {msg}", file=sys.stderr)
    return {**(extra or {}), "kete_od_error": msg}


def _od_with_kete(row: dict, fixtures_dir: Path, debias) -> dict | None:
    if not _HAVE_KETE:
        return None
    if isinstance(debias, str):
        return _od_failure(row, debias)
    # Fixture files replace the "/" of comet and interstellar designations.
    psv_path = fixtures_dir / f"{row['object'].replace('/', '_')}.psv"
    if not psv_path.is_file():
        return _od_failure(row, f"no pinned PSV fixture at {psv_path}")
    t0 = time.perf_counter()
    try:
        records = ades_psv.parse_psv(psv_path.read_text())
        ing = ades_psv.to_observations(records, debias)
    except Exception as e:  # noqa: BLE001  (any cause becomes the row's kete_od_error)
        return _od_failure(row, f"PSV ingestion failed: {type(e).__name__}: {e}")
    counts: dict = {
        "kete_od_n_records": ing.n_records,
        "kete_od_n_obs_ingested": len(ing.observations),
    }
    if ing.dropped:
        counts["kete_od_dropped"] = dict(ing.dropped)
        print(
            f"  {row['object']} OD: {sum(ing.dropped.values())} of {ing.n_records} "
            f"PSV rows not ingested: {dict(ing.dropped)}",
            file=sys.stderr,
        )
    # The fit uses the force model the propagation arm uses for this object.
    prop_opts = _kete_prop_opts(row)
    fit_opts: dict = {"include_asteroids": prop_opts["include_asteroids"]}
    if "non_gravs" in prop_opts:
        fit_opts["non_grav"] = prop_opts["non_gravs"][0]
    target_jd = row["t_mjd_tdb"] + 2_400_000.5
    try:
        iod = kete.orbit_fitting.initial_orbit_determination(ing.observations)
        if not iod:
            raise RuntimeError("initial_orbit_determination returned no candidate orbit")
        result = kete.orbit_fitting.fit_orbit(iod[0][1], ing.observations, **fit_opts)
        at_epoch = kete.propagate_n_body([result.state], target_jd, **prop_opts)[0]
    except Exception as e:  # noqa: BLE001  (any cause becomes the row's kete_od_error)
        return _od_failure(row, f"{type(e).__name__}: {e}", counts)
    ms = (time.perf_counter() - t0) * 1000.0
    pos = list(at_epoch.change_center(0).as_equatorial.pos)
    residuals = result.residuals
    if not residuals:
        return _od_failure(row, "fit kept no observations", counts)
    rms_ra = math.sqrt(sum(r[0] ** 2 for r in residuals) / len(residuals))
    rms_dec = math.sqrt(sum(r[1] ** 2 for r in residuals) / len(residuals))
    if not all(math.isfinite(v) for v in (*pos, rms_ra, rms_dec, result.rms)):
        return _od_failure(row, f"kete returned a non-finite fit (pos={pos})", counts)
    return {
        **counts,
        "kete_pos_au": pos,
        "kete_time_ms": ms,
        "kete_od_converged": bool(result.converged),
        "kete_od_rms_ra_arcsec": rms_ra,
        "kete_od_rms_dec_arcsec": rms_dec,
        "kete_od_rms_weighted": float(result.rms),
        "kete_od_n_obs_used": len(residuals),
        "kete_od_fit_epoch_mjd_tdb": result.state.jd - 2_400_000.5,
    }


def _executive_summary(rows: list[dict]) -> str:
    prop = [r for r in rows if r["test_type"] == "propagation" and r.get("kete_vs_horizons_km") is not None]
    eph = [r for r in rows if r["test_type"] == "ephemeris" and r.get("kete_separation_arcsec") is not None]
    od = [r for r in rows if r["test_type"] == "orbit_determination" and r.get("kete_pos_au") is not None]
    od_failed = [r for r in rows if r.get("kete_od_error") is not None]
    od_conv = [r for r in od if r.get("kete_od_converged")]

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
    lines.append("\n──── Kete external-reference summary ───────────────────")
    lines.append(f"  Total rows: {len(rows)}")
    lines.append(f"  Propagation rows compared: {len(prop)}")
    if prop:
        vals = [r["kete_vs_horizons_km"] for r in prop]
        lines.append(f"    |kete - Horizons|   p50={km_pct(vals, 0.5)}   p95={km_pct(vals, 0.95)}   max={km_pct(vals, 1.0)}")
        times = [r["kete_time_ms"] for r in prop if r.get("kete_time_ms")]
        if times:
            lines.append(f"    Wall clock          p50={statistics.median(times):.1f} ms   max={max(times):.0f} ms")
    lines.append(f"  Ephemeris rows compared: {len(eph)}")
    if eph:
        sep = [r["kete_separation_arcsec"] for r in eph]
        lines.append(f"    Angular sep         p50={asec_pct(sep, 0.5)}   p95={asec_pct(sep, 0.95)}   max={asec_pct(sep, 1.0)}")
    lines.append(f"  OD rows compared: {len(od)}   failed: {len(od_failed)}")
    if od:
        lines.append(f"    Converged           {len(od_conv)} of {len(od)}")
        rms = [r["kete_od_rms_ra_arcsec"] for r in od_conv]
        if rms:
            lines.append(f"    Post-fit RMS RA     p50={statistics.median(rms):.3f}″   max={max(rms):.3f}″ (converged)")
    for r in od:
        if not r.get("kete_od_converged"):
            lines.append(f"    NOT CONVERGED {r['object']} ({r.get('propagation_uncertainty')})")
    for r in od_failed:
        lines.append(f"    FAIL {r['object']} ({r.get('propagation_uncertainty')}): {r['kete_od_error']}")
    if not _HAVE_KETE:
        lines.append("  NOTE: kete is not installed; all rows skipped. Run `make setup-kete` first.")
    lines.append("─" * 56)
    return "\n".join(lines)


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--input", required=True, type=Path, help="validation plan JSON")
    p.add_argument(
        "--output",
        type=Path,
        default=Path("results/validation_kete.json"),
        help="output JSON",
    )
    p.add_argument(
        "--fixtures-dir",
        type=Path,
        default=Path(__file__).parent.parent.parent / "fixtures" / "psv",
        help="PSV fixture directory for OD rows",
    )
    p.add_argument(
        "--data-dir",
        type=Path,
        default=Path.home() / ".empyrean" / "data",
        help="engine data directory; OD reads its EFCC18 bias.dat (default: ~/.empyrean/data)",
    )
    args = p.parse_args()

    plan = json.loads(args.input.read_text())
    if not plan:
        print("input plan empty", file=sys.stderr)
        return 1

    print(f"Loaded {len(plan)} plan rows; running through kete...", file=sys.stderr)

    timestamp = datetime.now(timezone.utc).isoformat()
    source_version = _kete_source_version()
    out_rows: list[dict] = []
    n_skipped = 0
    n_od_failed = 0
    debias = None
    if _HAVE_KETE and any(r.get("test_type") == "orbit_determination" for r in plan):
        debias = _load_debias(args.data_dir)
    # kete has no uncertainty method, so an object's OD rows (the untagged one
    # and each `none_*` arm) are one fit: it runs once per object and epoch.
    od_fits: dict = {}

    for r in plan:
        # Uncertainty axis: skip covariance-bearing rows. The kete runner
        # currently propagates state only (no covariance); cross-tool Jet1
        # parity via kete.state_transition is a follow-up. The tag is a
        # composite `<method>_<arm>`, so the method is read off the prefix: a
        # `None` tag (OD rows) and every arm of `none` are covariance-free.
        _u = r.get("propagation_uncertainty")
        if _u is not None and not (_u == "none" or _u.startswith("none_")):
            n_skipped += 1
            continue
        new = dict(r)
        new["channel"] = "kete"
        new["timestamp"] = timestamp
        new["source_version"] = source_version
        tt = r.get("test_type")
        if tt == "propagation":
            update = _propagate_with_kete(r)
        elif tt == "ephemeris":
            update = _ephemeris_with_kete(r)
        elif tt == "orbit_determination":
            key = (r["object"], r["t_mjd_tdb"])
            if key not in od_fits:
                od_fits[key] = _od_with_kete(r, args.fixtures_dir, debias)
            update = od_fits[key]
        else:
            update = None
        if update is None:
            n_skipped += 1
        else:
            new.update(update)
            if "kete_od_error" in update:
                n_od_failed += 1
        out_rows.append(new)

    args.output.parent.mkdir(parents=True, exist_ok=True)
    # allow_nan=False: Python's json writes bare `NaN` / `Infinity` by default,
    # which are NOT valid JSON. Rust's serde_json rejects them, so a single
    # non-finite float here produced a file that parsed fine in Python and blew
    # up two jobs later in the reduce merge, pointing at a line number instead of
    # a cause. Failing at the point of production names the runner that did it.
    try:
        payload = json.dumps(out_rows, indent=2, default=str, allow_nan=False)
    except ValueError as e:
        print(
            f"ERROR: refusing to write non-finite values to {args.output}: {e}\n"
            "       Bare NaN/Infinity is invalid JSON; the Rust merger would reject the\n"
            "       whole channel. A quantity that could not be computed must be null,\n"
            "       and a row whose observable is not finite is a failure with a cause.",
            file=sys.stderr,
        )
        return 1
    args.output.write_text(payload)
    print(
        f"Wrote {len(out_rows)} kete rows to {args.output} "
        f"({n_skipped} skipped, {n_od_failed} OD failures)",
        file=sys.stderr,
    )

    print(_executive_summary(out_rows), file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
