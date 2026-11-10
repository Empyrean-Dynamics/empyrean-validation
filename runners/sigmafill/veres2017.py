"""Shared public Vereš et al. (2017) astrometric sigma-fill for the validation suite.

Any external orbit-determination runner whose tool refuses observations without
reported per-observation uncertainties, or would give them a flat default, fills
the missing sigmas from this ONE public model before handing the astrometry to the
tool (adam-core's public CORE_OD_V1 profile; layup's weight_data="supplied"). It
is deliberately a standalone, dependency-light module (``csv`` + ``math`` only) so
each runner can import it under its own venv:

    import sys; sys.path.insert(0, "<repo>/runners")
    from sigmafill.veres2017 import Veres2017SigmaModel, fill_missing

The model fills ONLY missing sigmas; a reported sigma is NEVER overwritten. Every
filled observation is labelled by fill level (station / catalog / no_catalog / catalog_unrecognized), and the
counts are reported on the OD row — no sigma value is ever silently defaulted.
The four levels and the paper sentence behind each are in README.md.

Provenance: Vereš, Farnocchia, Chesley & Chamberlin (2017), "Statistical analysis
of astrometric errors for the most productive asteroid surveys", Icarus 296,
139-149 (Tables 2-3 + Section 5 generic rules). The table and its transcription
check live in ``veres2017_sigma_table.csv`` next to this module. This is the PUBLIC
scheme only; it is not the proprietary Asteroid-Institute v2_sigma_fill table.
"""

from __future__ import annotations

import csv
import hashlib
import math
from pathlib import Path

HERE = Path(__file__).resolve().parent
DEFAULT_TABLE = HERE / "veres2017_sigma_table.csv"
MODEL_NAME = "Veres2017SigmaFill"

#: ADES encodes a missing / unknown star catalog as this; it resolves to the paper's
#: "other w/o catalog" weight (1.5"), which the CSV carries as the astcat="UNK" row.
UNKNOWN_CATALOG = "UNK"

#: Fill levels, one per branch of the rule (README.md cites the paper for each):
#: station (Tables 2-3), catalog (Section 3, known catalog), no_catalog (Section 3,
#: empty/UNK catalog), catalog_unrecognized (code outside the table: the paper is
#: silent, so the conservative UNK weight is used and the observation labelled).
LEVELS = ("station", "catalog", "no_catalog", "catalog_unrecognized")


def _is_missing(v) -> bool:
    """A sigma is missing when it is None or non-finite or non-positive."""
    if v is None:
        return True
    try:
        f = float(v)
    except (TypeError, ValueError):
        return True
    return not math.isfinite(f) or f <= 0.0


class Veres2017SigmaModel:
    """The VFC2017 station / catalog / no-catalog sigma rules loaded from the CSV."""

    def __init__(self, station: dict, catalog: dict, source: str = str(DEFAULT_TABLE)):
        if UNKNOWN_CATALOG not in catalog:
            raise ValueError("sigma table lacks the UNK (unknown-catalog) row")
        self.station = station          # {obs_code: (sigma_ra, sigma_dec)}
        self.catalog = catalog          # {astcat: (sigma_ra, sigma_dec)}, includes UNK
        self.source = source

    @classmethod
    def from_csv(cls, path: Path | str = DEFAULT_TABLE) -> "Veres2017SigmaModel":
        path = Path(path)
        station: dict[str, tuple[float, float]] = {}
        catalog: dict[str, tuple[float, float]] = {}
        with path.open() as fh:
            reader = csv.reader(line for line in fh if not line.lstrip().startswith("#"))
            header = next(reader)
            if header[:4] != ["obs_code", "astcat", "sigma_ra_arcsec", "sigma_dec_arcsec"]:
                raise ValueError(f"unexpected sigma-table header: {header!r}")
            for row in reader:
                if not row or not any(c.strip() for c in row):
                    continue
                code, cat, sra, sdec = (row[0].strip(), row[1].strip(),
                                        float(row[2]), float(row[3]))
                if code and not cat:
                    station[code] = (sra, sdec)
                elif cat and not code:
                    catalog[cat] = (sra, sdec)
                else:
                    raise ValueError(
                        f"row is neither a station nor a catalog rule (no global row "
                        f"exists in the public scheme): {code!r}/{cat!r}"
                    )
        return cls(station, catalog, source=str(path))

    def lookup(self, obs_code: str | None, astcat: str | None) -> tuple[float, float, str]:
        """(sigma_ra_arcsec, sigma_dec_arcsec, level) for one observation.

        Precedence: a station rule wins (Tables 2-3); otherwise an empty or UNK star
        catalog takes the paper's unknown-catalog weight (level ``no_catalog``);
        otherwise a catalog named in the table takes the known-catalog weight (level
        ``catalog``); otherwise a non-empty code the table does not name takes the
        conservative UNK weight and is labelled ``catalog_unrecognized``.
        """
        code = (obs_code or "").strip()
        if code in self.station:
            s = self.station[code]
            return s[0], s[1], "station"
        key = (astcat or "").strip() or UNKNOWN_CATALOG
        if key == UNKNOWN_CATALOG:
            s = self.catalog[UNKNOWN_CATALOG]
            return s[0], s[1], "no_catalog"
        if key in self.catalog:
            s = self.catalog[key]
            return s[0], s[1], "catalog"
        s = self.catalog[UNKNOWN_CATALOG]
        return s[0], s[1], "catalog_unrecognized"


def fill_missing(obs_codes, astcats, ra_sigma, dec_sigma,
                 model: Veres2017SigmaModel):
    """Fill the missing sigmas of a set of observations from the VFC2017 model.

    Inputs are equal-length sequences; ``ra_sigma``/``dec_sigma`` are arcseconds,
    with None/NaN/non-positive marking a missing (to-be-filled) value. Returns
    ``(filled_ra, filled_dec, report)`` where the filled lists leave every REPORTED
    sigma untouched and carry the VFC2017 value only where one was missing, and
    ``report`` is::

        {"model": MODEL_NAME, "table": <csv path>, "reported": int,
         "filled": int, "partially_reported": int, "filled_by_level":
         {"station": int, "catalog": int, "no_catalog": int,
         "catalog_unrecognized": int}, "total": int, "table_sha256": str}

    ``partially_reported`` counts observations with one of the two sigmas reported
    (kept) and the other filled; each is also counted under its fill level.
    """
    n = len(obs_codes)
    out_ra = [None] * n
    out_dec = [None] * n
    reported = 0
    partial = 0
    by_level = {lvl: 0 for lvl in LEVELS}
    for i in range(n):
        r, d = ra_sigma[i], dec_sigma[i]
        r_ok, d_ok = not _is_missing(r), not _is_missing(d)
        if r_ok and d_ok:
            out_ra[i], out_dec[i] = float(r), float(d)
            reported += 1
            continue
        sra, sdec, level = model.lookup(obs_codes[i], astcats[i])
        # A reported half is NEVER overwritten: only the missing component is filled.
        out_ra[i] = float(r) if r_ok else sra
        out_dec[i] = float(d) if d_ok else sdec
        by_level[level] += 1
        partial += 1 if (r_ok or d_ok) else 0
    report = {
        "model": MODEL_NAME,
        "table": model.source,
        "table_sha256": hashlib.sha256(Path(model.source).read_bytes()).hexdigest(),
        "reported": reported,
        "filled": sum(by_level.values()),
        "partially_reported": partial,
        "filled_by_level": by_level,
        "total": n,
    }
    return out_ra, out_dec, report
