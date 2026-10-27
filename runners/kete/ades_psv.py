"""ADES PSV astrometry -> kete optical ``Observation`` objects, offline.

The kete runner fits the suite's pinned ADES 2017 PSV fixtures
(``fixtures/psv/<object>.psv``). kete's own ADES ingestion lives inside
``kete.orbit_fitting.fetch_mpc_observations``, which reads the records from the
MPC web API, so it cannot run under the sealed suite. This module feeds the
pinned PSV rows through the same per-record rules, calling kete's own ingestion
helpers so the rules stay kete's and are not re-derived here:

* time: ``obsTime`` is ISO UTC; ``kete.Time.from_iso`` converts to TDB.
* observer: ``kete.orbit_fitting.mpc_api._build_observer``. Ground stations
  come from kete's built-in observatory table; spacecraft and roving observers
  from the record's ``sys`` / ``ctr`` / ``pos1..3``; Earth, Sun and spacecraft
  centres from kete's loaded kernels. No lookup goes to the network.
* sigmas: the record's ``rmsRA`` / ``rmsDec`` when both are present, else kete's
  per-observatory residual table, else kete's epoch default. ``rmsCorr`` sets
  the correlation and ``rmsTime`` the timing sigma; kete's 10 mas floor on
  the error ellipse's principal axes applies.
* catalog debiasing: EFCC18 through kete's ``DebiasTable``, built from the
  ``bias.dat`` in the engine's data directory (the same file kete's own
  ingestion downloads inside ``debias_2018.tgz``).
* over-observed nights: kete's Veres et al. (2017) sigma inflation.

The helpers are private to kete and are those of the release ``setup.sh``
pins (3.3.0). A release that renames one fails ingestion with the missing name
instead of fitting under different rules.

A record that cannot become an observation (no position, unreadable time, no
observer state) is counted under a named reason and returned with the
observations, so the caller can report what was left out.
"""

from __future__ import annotations

import math
from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path

# The PSV line that names the columns. Data rows carry an ISO time in this
# column, so a line whose fields include the bare name is a header.
_TIME_FIELD = "obsTime"


def parse_psv(text: str) -> list[dict[str, str]]:
    """Parse ADES PSV text into one dict per observation row.

    Header metadata (``#`` block lines and ``!`` keyword lines) and blank
    lines are skipped. The first other line is the header; a later line that
    again names ``obsTime`` starts a new block with its own header. Values are
    stripped and empty fields are left out, so ``rec.get(name)`` is ``None``
    for a field the row does not carry.

    Raises
    ------
    ValueError
        If a data row comes before any header, or a row's field count differs
        from its header's.
    """
    header: list[str] | None = None
    records: list[dict[str, str]] = []
    for lineno, line in enumerate(text.splitlines(), start=1):
        if not line.strip() or line.lstrip().startswith(("#", "!")):
            continue
        fields = [f.strip() for f in line.split("|")]
        if _TIME_FIELD in fields:
            header = fields
            continue
        if header is None:
            raise ValueError(f"PSV line {lineno}: data row before any header line")
        if len(fields) != len(header):
            raise ValueError(
                f"PSV line {lineno}: {len(fields)} fields, header has {len(header)}"
            )
        records.append({k: v for k, v in zip(header, fields) if v})
    return records


def load_debias_table(data_dir: Path):
    """Load kete's EFCC18 ``DebiasTable`` from ``<data_dir>/bias.dat``.

    Raises
    ------
    FileNotFoundError
        If the data directory has no ``bias.dat``.
    """
    from kete.orbit_fitting.common import DebiasTable

    path = Path(data_dir) / "bias.dat"
    if not path.is_file():
        raise FileNotFoundError(
            f"EFCC18 debias table not found at {path}; kete's ADES ingestion "
            "debiases every record, and the table must come from the data "
            "directory, not a download"
        )
    return DebiasTable.from_ascii(path.read_text(encoding="ascii"))


@dataclass
class Ingested:
    """kete observations built from a PSV, with the records left out by reason."""

    observations: list = field(default_factory=list)
    n_records: int = 0
    dropped: Counter = field(default_factory=Counter)


def _float(value) -> float | None:
    try:
        x = float(value)
    except (TypeError, ValueError):
        return None
    return x if math.isfinite(x) else None


def to_observations(records: list[dict[str, str]], debias_table) -> Ingested:
    """Convert parsed PSV records into kete optical observations.

    ``debias_table`` is the kete ``DebiasTable`` from :func:`load_debias_table`.
    See the module docstring for the per-record rules.
    """
    import kete
    from kete.orbit_fitting import common, mpc_api

    out = Ingested(n_records=len(records))
    valid = []
    for rec in records:
        ra_deg = _float(rec.get("ra"))
        dec_deg = _float(rec.get("dec"))
        if ra_deg is None or dec_deg is None:
            out.dropped["no RA/Dec"] += 1
            continue
        try:
            epoch = kete.Time.from_iso(rec["obsTime"])
        except (KeyError, ValueError):
            out.dropped["unreadable obsTime"] += 1
            continue
        jd = epoch.jd
        stn = rec.get("stn")
        if not stn:
            out.dropped["no stn"] += 1
            continue
        observer = mpc_api._build_observer(stn, jd, rec)
        if observer is None:
            out.dropped[f"no observer state for stn {stn}"] += 1
            continue

        # ADES has no note2: spacecraft are the ICRF position systems and video
        # is mode VID, as in kete's own ADES ingestion.
        if (rec.get("sys") or "").upper() in ("ICRF_KM", "ICRF_AU"):
            note2 = "S"
        elif rec.get("mode") == "VID":
            note2 = "n"
        else:
            note2 = ""
        time_std, std = common._time_sigma_for_obs(note2, epoch.ymd[0])
        rms_time = _float(rec.get("rmsTime"))
        if rms_time is not None and rms_time > 0:
            time_std = rms_time

        cos_dec = math.cos(math.radians(dec_deg))
        s_ra, s_dec = std * cos_dec, std
        rms_ra, rms_dec = _float(rec.get("rmsRA")), _float(rec.get("rmsDec"))
        if rms_ra is not None and rms_dec is not None:
            # ADES rmsRA is already sky-plane (includes cos(dec)).
            s_ra, s_dec = rms_ra, rms_dec
        elif (station := common.get_observatory_std(stn)) is not None:
            s_ra, s_dec = station[0] * cos_dec, station[1]

        corr = _float(rec.get("rmsCorr")) or 0.0
        s_ra, s_dec, corr = common._floor_error_ellipse(
            s_ra, s_dec, corr, mpc_api._ADES_OPTICAL_SIGMA_FLOOR
        )

        astcat = rec.get("astCat")
        if astcat:
            code = mpc_api._ADES_TO_MPC_CODE.get(astcat.upper())
            if code:
                shift = debias_table.lookup(code, ra_deg, dec_deg, jd)
                if shift is not None:
                    ra_deg -= shift[0] / 3600.0
                    dec_deg -= shift[1] / 3600.0

        mag = _float(rec.get("mag"))
        valid.append(
            {
                "stn": stn,
                "jd": jd,
                "ra": ra_deg,
                "dec": dec_deg,
                "s_ra": s_ra,
                "s_dec": s_dec,
                "corr": corr,
                "time_std": time_std,
                "band": rec.get("band") or "V",
                "mag": math.nan if mag is None else mag,
                "observer": observer,
            }
        )

    factors = common._over_obs_reweight_factors(
        [v["stn"] for v in valid], [v["jd"] for v in valid], [False] * len(valid)
    )
    for v, factor in zip(valid, factors):
        try:
            obs = kete.orbit_fitting.Observation.optical(
                observer=v["observer"],
                ra=v["ra"],
                dec=v["dec"],
                sigma_ra=float(v["s_ra"]) * factor,
                sigma_dec=float(v["s_dec"]) * factor,
                sigma_corr=v["corr"],
                band=v["band"],
                mag=v["mag"],
                time_sigma=v["time_std"],
                is_occultation=False,
            )
        except ValueError as e:
            out.dropped[f"rejected by Observation.optical: {e}"] += 1
            continue
        out.observations.append(obs)
    return out
