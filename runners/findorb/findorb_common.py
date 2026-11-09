"""Settings and staged data shared by the find_orb OD runner and the walk runner.

find_orb is run at its own documented best practice, fed the same data products
as every other tool (owner ruling D4). Every setting below cites find_orb's own
`environ.def` (the commented defaults file in the find_orb checkout) or its
source, so the two runners cannot drift apart:

* **Asteroid perturbers.** `ASTEROID_PERT_LIST=` is EMPTY, find_orb's default:
  "we consider all 300 asteroids listed in BC-405", culled by a proximity test
  (`ASTEROID_THRESH`). A non-empty list is "to the exclusion of others"
  (`environ.def`), meant for replicating another program. find_orb already
  refuses to let a numbered asteroid perturb itself (`bc405.cpp:502`, "don't let
  an asteroid perturb itself!"), so Iris, Vesta, Pallas and Hygiea need no list.
* **Integrator.** `ENCKE=0`, Cowell: `environ.def` says "you will probably be
  best off leaving this value at zero, meaning Cowell is used".
* **Debiasing.** The last field of `SETTINGS2` is 1: "FCCT14 or EFCC18
  astrometric debiasing ... their use is highly recommended" (`environ.def`).
  It needs the EFCC18 `bias.dat` (`bias.cpp`: "You should really use the
  latter"). The walk runner applies catalog debiasing once, centrally, from its
  manifest, so it passes `debiasing=False` and says why; the OD runner feeds raw
  ADES and lets find_orb debias.
* **Outlier rejection.** `OUTLIER_REJECTION_LIMIT=3`, find_orb's default.

`bc405.dat` (the converted BC-405 asteroid ephemeris, exactly 52,617,600 bytes)
is staged from the registered set's `findorb/` folder into a private config
directory handed to `fo` with `-x`. A missing or wrong-sized file is a refusal
to start, never a quiet fall-back to `~/.find_orb` (which would change the force
model without a word).
"""

from __future__ import annotations

import hashlib
import os
from pathlib import Path

# bc405.cpp fixes every one of these numbers, so the size is an exact integrity
# check: 300 asteroids x 3654 40-day chunks x 6 elements x 8 bytes.
BC405_DAT = "bc405.dat"
BC405_DAT_BYTES = 6 * 300 * 3654 * 8  # 52_617_600
BIAS_DAT = "bias.dat"
JPL_DE_FILE = "linux_p1550p2650.440"
# Bill Gray's binary DE440 for 1550-2650: the same planetary ephemeris as the
# suite's de440.bsp in a different container, named here so the row can say so.
JPL_DE_NOTE = "DE440 (Gray's binary linux_p1550p2650.440, 1550-2650)"

PERTURBERS = "1007fe"  # 0x7fe planets + Moon, bit 20 = asteroids (runge.cpp)


def sha256_file(path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def environ_lines(
    *,
    debiasing: bool,
    perturbers: str = PERTURBERS,
    pert_list: str = "",
    extra: tuple[str, ...] = (),
) -> list[str]:
    """find_orb's `environ.dat` lines, identical for both runners except for what
    the caller passes: `debiasing`, an explicit `perturbers` mask or `pert_list`
    (to replicate another integrator on purpose), and `extra` (the walk's
    ephemeris-epoch lines)."""
    return [
        # Symlinked into the working directory by the caller; a bare relative
        # name because find_orb copies it into a 94-byte buffer.
        "LINUX_JPL_FILENAME=jpl_de.440",
        f"PERTURBERS={perturbers}",
        "BC405_ASTEROIDS=300",
        "ASTEROID_THRESH=10",
        f"ASTEROID_PERT_LIST={pert_list}",
        # use_sigmas=1 (honour the ADES rmsRA/rmsDec), ephemeris_mag_limit=22,
        # sigmas_in_columns=0, forced_central_body=-2 (auto), apply_debiasing.
        f"SETTINGS2=1 22.00 0 -2 {1 if debiasing else 0}",
        "ENCKE=0",
        "OUTLIER_REJECTION_LIMIT=3",
        *extra,
    ]


def stage_cfg_dir(
    base: Path, data_dir: Path | None, *, debiasing: bool, asteroids: bool = True
) -> Path:
    """A private find_orb config directory (`fo -x <dir>/`) holding links to the
    registered `bc405.dat` (and `bias.dat` when debiasing). find_orb writes
    `bc405pre.dat` and a crunched bias file here, so the directory is private to
    this process; it is NOT pre-seeded.

    `asteroids=False` (a deliberate planets-only run, PERTURBERS without bit 20)
    does not require or link `bc405.dat`.

    Raises SystemExit naming the file for every missing or wrong-sized input."""
    if data_dir is None:
        raise SystemExit(
            "ERROR: no find_orb data directory. Pass --data-dir <third-party>/findorb "
            f"(it must hold {BC405_DAT}, {JPL_DE_FILE}"
            + (f" and {BIAS_DAT}" if debiasing else "") + ")."
        )
    data_dir = Path(data_dir)
    bc405 = data_dir / BC405_DAT
    if asteroids and not bc405.is_file():
        raise SystemExit(
            f"ERROR: {bc405} is missing. find_orb would print 'asteroid perturbations cannot "
            "be included' and integrate with planets only, a quietly different force model. "
            "Refusing to start."
        )
    size = bc405.stat().st_size if asteroids else BC405_DAT_BYTES
    if size != BC405_DAT_BYTES:
        raise SystemExit(
            f"ERROR: {bc405} is {size} bytes, expected {BC405_DAT_BYTES}: a truncated "
            "conversion puts wrong asteroid positions mid-file. Refusing to start."
        )
    jpl = data_dir / JPL_DE_FILE
    if not jpl.is_file():
        raise SystemExit(f"ERROR: {jpl} is missing; find_orb has no planetary ephemeris without it.")
    needed = {BC405_DAT: bc405} if asteroids else {}
    if debiasing:
        bias = data_dir / BIAS_DAT
        if not bias.is_file():
            raise SystemExit(
                f"ERROR: debiasing is on (SETTINGS2 last field 1) but {bias} is missing; "
                "find_orb would run undebiased without saying so. Refusing to start."
            )
        needed[BIAS_DAT] = bias
    cfg = Path(base) / "cfg"
    cfg.mkdir(parents=True, exist_ok=True)
    for name, src in needed.items():
        link = cfg / name
        if not os.path.lexists(link):
            os.symlink(src.absolute(), link)
    return cfg


def settings_record(data_dir: Path, *, debiasing: bool, pert_list: str = "") -> dict[str, str]:
    """What shaped the fit, as strings: the environ settings with their upstream
    reasons, the data files by hash, and the model differences from the other tools."""
    data_dir = Path(data_dir)
    rec = {
        "integrator": "Cowell (ENCKE=0; environ.def advises leaving it at zero)",
        "asteroid_perturbers": (
            "BC-405, 300 bodies, culled by proximity (ASTEROID_THRESH=10); "
            "ASTEROID_PERT_LIST empty (find_orb default); the target never perturbs itself"
            if not pert_list else f"fixed list {pert_list} (replicating another integrator)"
        ),
        "perturbers_mask": PERTURBERS,
        "debiasing": (
            "EFCC18 by find_orb (SETTINGS2 last field 1; environ.def: highly recommended)"
            if debiasing else "off in find_orb; debiased centrally by the walk manifest"
        ),
        "weighting": "ADES rmsRA/rmsDec honoured (SETTINGS2 first field 1)",
        "outlier_rejection": "3 sigma (OUTLIER_REJECTION_LIMIT=3, find_orb default)",
        "planetary_ephemeris": f"{JPL_DE_NOTE} (sha256 {sha256_file(data_dir / JPL_DE_FILE)})",
        "bc405_dat": f"sha256 {sha256_file(data_dir / BC405_DAT)}",
        "model_differences": (
            "asteroid perturbers BC-405 (300, proximity-culled), not SB441-N16; "
            "planetary ephemeris is DE440 in Gray's binary format"
        ),
    }
    if debiasing:
        rec["bias_dat"] = f"EFCC18 bias.dat sha256 {sha256_file(data_dir / BIAS_DAT)}"
    return rec
