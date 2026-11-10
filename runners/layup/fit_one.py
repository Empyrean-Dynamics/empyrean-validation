"""Fit ONE object with layup's Python API, at layup's own documented best practice.

The ``layup orbitfit`` CLI exposes only the boolean ``--weight-data`` and
``--debias``; the settings the suite wants are API-only, so this thin driver (run
under the layup venv, one process per object or walk window, with the parent's
timeout) calls ``layup.orbitfit.orbitfit`` the way ``orbitfit_cli`` does
(layup 1.1.0, ``layup/orbitfit.py`` ``orbitfit_cli``), with:

* ``weight_data="supplied"``: layup uses the per-observation ``rmsRA`` / ``rmsDec``
  columns directly ("e.g. ADES-reported uncertainties", the ``orbitfit`` docstring).
  The parent has already written the observation's REPORTED sigma there, or, where
  none was reported, the shared public Vereš 2017 value (``runners/sigmafill``), so
  no observation falls to layup's flat C++ default;
* ``debias=True``: EFCC18 catalog debiasing (layup's ``generate_bias_dict`` reads
  the high-resolution ``bias.dat`` staged from ``debias_hires2018.tgz``);
* ``fit_nongrav="auto"``: layup's adaptive non-gravitational model (its issue #357,
  the ``orbitfit`` docstring): gravity-only is kept unless its reduced chi-square
  is unacceptable, otherwise the most parsimonious warranted A1/A2/A3 model is
  adopted, at layup's default ``NongravAutoThresholds`` and default g(r);
* ``iod="auto"`` and ``engine="cartesian"``, layup's documented defaults.

``--guess`` (the walk's warm start) passes a STATE-ONLY ``BCART_EQ`` guess as
``initial_guess``, read with the input reader exactly as ``orbitfit_cli`` reads
``-g``. Writes layup's own result csv (``<out>``), the CLI's output format
(``BCART_EQ``), so the parent reads it exactly as it read the CLI's.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("input", type=Path)
    p.add_argument("--ar", required=True)
    p.add_argument("--primary-id", required=True)
    p.add_argument("--out", type=Path, required=True)
    p.add_argument("--guess", type=Path, default=None,
                   help="State-only BCART_EQ initial guess (layup's -g), same primary id.")
    args = p.parse_args()

    from layup.orbitfit import (
        INPUT_FORMAT_READERS,
        REQUIRED_INPUT_GUESS_COLUMN_NAMES,
        REQUIRED_INPUT_OBSERVATIONS_COLUMN_NAMES,
        orbitfit,
    )
    from layup.utilities.data_processing_utilities import create_chunks
    from layup.utilities.file_io.file_output import write_csv

    reader_class, sep = INPUT_FORMAT_READERS["ADES_psv"]
    reader = reader_class(
        args.input,
        primary_id_column_name=args.primary_id,
        sep=sep,
        required_columns=REQUIRED_INPUT_OBSERVATIONS_COLUMN_NAMES,
    )
    guess_reader = None
    if args.guess is not None:
        guess_reader = reader_class(
            args.guess,
            primary_id_column_name=args.primary_id,
            sep=sep,
            required_columns=REQUIRED_INPUT_GUESS_COLUMN_NAMES,
        )
    n = 0
    for chunk in create_chunks(reader, 10_000):
        data = reader.read_objects(chunk)
        initial_guess = None
        if guess_reader is not None:
            initial_guess = guess_reader.read_objects(chunk)
            formats = {str(f) for f in initial_guess["FORMAT"]}
            if formats != {"BCART_EQ"}:
                print(f"guess must be BCART_EQ, got {sorted(formats)}", file=sys.stderr)
                return 4
        fit = orbitfit(
            data,
            cache_dir=args.ar,
            initial_guess=initial_guess,
            num_workers=1,
            primary_id_column_name=args.primary_id,
            debias=True,
            weight_data="supplied",
            iod="auto",
            engine="cartesian",
            fit_nongrav="auto",
        )
        write_csv(fit, str(args.out))
        n += len(fit)
    if n == 0:
        print("orbitfit returned no orbit", file=sys.stderr)
        return 3
    return 0


if __name__ == "__main__":
    sys.exit(main())
