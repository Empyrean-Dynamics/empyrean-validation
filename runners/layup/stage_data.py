#!/usr/bin/env python3
"""Stage layup's data directory from the registered kernel set.

layup reads its SPICE kernels, its ASSIST ephemerides, the observatory codes
and the debiasing table from one directory (its ``--ar`` "ASSIST+Rebound" data
directory), each under the file name its own bootstrap downloads. Those names
are fixed inside the fit path: the C++ ASSIST loader, the Python residual
filter, the leap-second loader and Sorcha's kernel furnisher all read layup's
default names whatever ``-c`` config is passed, and ``layup orbitfit`` checks
for every one of them before it fits. A name that is absent is downloaded on
the spot from the address layup was released with. When NAIF renamed its
dated Earth-orientation kernels those downloads began to 404, and every layup
fit failed.

This script builds that directory from the engine data directory (the
registered kernel set the suite syncs, ``EMPYREAN_DATA_DIR``) and from nothing
else: no download, and never layup's per-user cache. The names come from the
installed layup's own defaults, so a layup bump that renames a file is staged
under the new name, and a file the bump adds that no role below fills is a
hard stop. Each name becomes a symbolic link to the one data-directory file
that fills its role, matched by the pattern its name follows across set
versions (the GRSS runner's rule). Two files layup wants in a form the set does
not ship are derived from it: the gzip of the observatory codes, and the
debiasing table extracted from its archive. The SPICE meta-kernel layup
furnishes is written here too and names the data directory's own files, so a
SPICE message names the kernel that was really loaded.

A role the data directory cannot fill (no match, more than one match, or an
empty file) is a hard stop that lists every such role at once, so one run
names everything a set update has to add.

Usage (with the layup venv's python, which supplies layup's names and the
spiceypy load check)::

    .venv/bin/python stage_data.py --data-dir "$DATA_DIR" --dest data
"""

from __future__ import annotations

import argparse
import gzip
import os
import sys
import tarfile
from dataclasses import dataclass
from pathlib import Path

LINK = "link"
GZIP = "gzip"
UNTAR = "untar"


@dataclass(frozen=True)
class Role:
    """One file layup reads from its data directory."""

    attr: str
    """layup's AuxiliaryConfigs attribute that holds the file's name."""
    what: str
    """What layup uses it for."""
    source: str
    """Glob matched at the top of the data directory; exactly one file."""
    make: str = LINK
    """How the staged file is made from the source: LINK, GZIP or UNTAR."""
    member_attr: str | None = None
    """UNTAR only: the attribute naming the archive member to extract."""


ROLES: tuple[Role, ...] = (
    # The full DE440 SPK in place of the short de440s subset layup defaults to:
    # the same ephemeris, over 1550-2650 rather than 1849-2150.
    Role("planet_ephemeris", "planetary SPK for observer positions", "de440.bsp"),
    Role("earth_predict", "Earth orientation, predicted", "earth_*_predict.bpc"),
    Role("earth_historical", "Earth orientation, historical", "earth_620120_*.bpc"),
    Role("earth_high_precision", "Earth orientation, high precision", "earth_latest_high_prec.bpc"),
    # Not an engine kernel and not in the registered set: the binary DE440 the
    # suite adds to the data directory for find_orb (its one documented direct
    # download). ASSIST reads it under layup's name for it.
    Role("jpl_planets", "ASSIST planetary ephemeris, binary DE440", "linux_p1550p2650.440"),
    Role("jpl_small_bodies", "ASSIST small-body perturbers", "sb441-n16.bsp"),
    Role("leap_seconds", "leap seconds, LSK", "naif0012.tls"),
    Role("orientation_constants", "orientation constants, text PCK", "pck00010.tpc"),
    # The MPC's extended observatory-code list, which layup's bootstrap
    # downloads gzipped and decompresses under its own name.
    Role("observatory_codes", "observatory codes", "obscodes_extended.json"),
    Role("observatory_codes_compressed", "observatory codes, compressed", "obscodes_extended.json", GZIP),
    # layup's debiasing table is the HIGH-resolution 2018 table (it indexes
    # HEALPix nside 256). The set's own bias.dat is the low-resolution 2018
    # table, so it is never staged under layup's name.
    Role("debiasing_data_compressed", "debiasing tables, archive", "debias_hires2018.tgz"),
    Role(
        "debiasing_data",
        "debiasing table, high resolution",
        "debias_hires2018.tgz",
        UNTAR,
        member_attr="debiasing_data",
    ),
)

# The kernel pool holds at most 80 characters per string value; longer values
# are split and continued with a trailing '+'.
_SPICE_CHUNK = 72


@dataclass(frozen=True)
class Layout:
    """layup's file names: one per role, plus the lists its fit path checks."""

    names: dict[str, str]
    """Role attribute -> the file name layup reads."""
    meta_kernel: str
    """The meta-kernel's file name."""
    required: tuple[str, ...]
    """Every file name layup's orbitfit checks for before it fits."""
    spice_order: tuple[str, ...]
    """The kernels the meta-kernel loads, in layup's order."""
    version: str | None = None
    """The installed layup's version, when read from it."""

    @classmethod
    def from_layup(cls) -> Layout:
        """Read the names from the installed layup's defaults."""
        from importlib import metadata

        from layup.utilities.layup_configs import AuxiliaryConfigs

        aux = AuxiliaryConfigs()
        return cls(
            names={r.attr: getattr(aux, r.attr) for r in ROLES},
            meta_kernel=aux.meta_kernel,
            required=tuple(aux.data_file_list),
            spice_order=tuple(aux.ordered_kernel_files),
            version=metadata.version("layup"),
        )

    def staged(self) -> list[str]:
        """Every name this script writes into the data directory."""
        return [self.names[r.attr] for r in ROLES] + [self.meta_kernel]

    def gaps(self) -> list[str]:
        """Names layup checks for (or loads) that no role fills."""
        made = set(self.staged())
        wanted = list(self.required) + [n for n in self.spice_order if n not in self.required]
        return [f"{n}: the installed layup reads it, but no staging role fills it" for n in wanted if n not in made]


class StagingError(Exception):
    """The data directory cannot fill every role; carries one line per problem."""

    def __init__(self, data_dir: Path, problems: list[str]):
        self.data_dir = data_dir
        self.problems = problems
        super().__init__("\n".join(problems))


def resolve(data_dir: Path, layout: Layout) -> tuple[dict[str, Path], list[str]]:
    """Match every role's source in ``data_dir``.

    Returns ``(sources, problems)``: the matched source per role attribute, and
    one line per role the directory cannot fill.
    """
    sources: dict[str, Path] = {}
    problems: list[str] = list(layout.gaps())
    for role in ROLES:
        matches = sorted(p for p in data_dir.glob(role.source) if p.is_file())
        label = f"{layout.names[role.attr]} ({role.what})"
        if not matches:
            problems.append(f"{label}: no '{role.source}' in the data directory")
        elif len(matches) > 1:
            names = ", ".join(p.name for p in matches)
            problems.append(
                f"{label}: '{role.source}' matches {len(matches)} files ({names}); "
                "a synced set holds exactly one (re-sync with --prune)"
            )
        elif matches[0].stat().st_size == 0:
            problems.append(f"{label}: {matches[0].name} is empty")
        else:
            sources[role.attr] = matches[0]
    return sources, problems


def _replace_link(target: Path, dest: Path) -> bool:
    """Point ``dest`` at ``target``; True when it changed."""
    if dest.is_symlink() and os.readlink(dest) == str(target):
        return False
    tmp = dest.with_name(dest.name + ".part")
    if tmp.is_symlink() or tmp.exists():
        tmp.unlink()
    os.symlink(target, tmp)
    os.replace(tmp, dest)
    return True


def _replace_gzip(source: Path, dest: Path) -> bool:
    """Write ``dest`` as the gzip of ``source``; True when it changed."""
    data = source.read_bytes()
    if dest.is_file() and not dest.is_symlink():
        try:
            if gzip.decompress(dest.read_bytes()) == data:
                return False
        except (OSError, EOFError):
            pass  # not a valid gzip: rewritten below
    tmp = dest.with_name(dest.name + ".part")
    tmp.write_bytes(gzip.compress(data, mtime=0))
    os.replace(tmp, dest)
    return True


def _replace_member(archive: Path, member: str, dest: Path) -> bool:
    """Extract ``member`` of ``archive`` to ``dest``; True when it changed.

    An existing file is kept only when it is a regular file with the member's
    size and modification time, which extraction stamps on it.
    """
    with tarfile.open(archive, "r:*") as tf:
        try:
            info = tf.getmember(member)
        except KeyError:
            raise StagingError(archive.parent, [f"{member}: {archive.name} has no member '{member}'"]) from None
        st = dest.stat() if dest.is_file() and not dest.is_symlink() else None
        if st is not None and st.st_size == info.size and int(st.st_mtime) == int(info.mtime):
            return False
        src = tf.extractfile(info)
        if src is None:
            raise StagingError(archive.parent, [f"{member}: '{member}' in {archive.name} is not a file"])
        tmp = dest.with_name(dest.name + ".part")
        with src, open(tmp, "wb") as out:
            while chunk := src.read(1 << 20):
                out.write(chunk)
        os.utime(tmp, (info.mtime, info.mtime))
        os.replace(tmp, dest)
    return True


def _spice_strings(value: str) -> list[str]:
    """Split one kernel-pool string value into continued pieces."""
    pieces = [value[i : i + _SPICE_CHUNK] for i in range(0, len(value), _SPICE_CHUNK)] or [""]
    return [f"'{p}+'" for p in pieces[:-1]] + [f"'{pieces[-1]}'"]


def meta_kernel_text(data_dir: Path, layout: Layout, sources: dict[str, Path]) -> str:
    """The meta-kernel layup furnishes, naming the data directory's own files."""
    by_name = {layout.names[r.attr]: sources[r.attr] for r in ROLES if r.make == LINK}
    path_values = "\n".join(f"    {s}" for s in _spice_strings(str(data_dir)))
    kernels = []
    for name in layout.spice_order:
        kernels.extend(f"    {s}" for s in _spice_strings(f"$SET/{by_name[name].name}"))
    return (
        "KPL/MK\n\n"
        "layup meta-kernel, written by the validation suite's stage_data.py from\n"
        "the registered kernel set; layup's own bootstrap never runs.\n\n"
        "\\begindata\n\n"
        f"PATH_VALUES = (\n{path_values}\n)\n\n"
        "PATH_SYMBOLS = ( 'SET' )\n\n"
        "KERNELS_TO_LOAD = (\n" + "\n".join(kernels) + "\n)\n\n"
        "\\begintext\n"
    )


def stage(data_dir: Path, dest: Path, layout: Layout) -> list[tuple[str, str, str]]:
    """Build layup's data directory at ``dest`` from ``data_dir``.

    Returns one ``(action, staged name, source)`` row per file. Raises
    StagingError, before touching ``dest``, when any role cannot be filled.
    """
    data_dir = data_dir.absolute()
    dest = dest.absolute()
    if not data_dir.is_dir():
        raise StagingError(data_dir, [f"the data directory {data_dir} does not exist"])
    sources, problems = resolve(data_dir, layout)
    if problems:
        raise StagingError(data_dir, problems)

    dest.mkdir(parents=True, exist_ok=True)
    rows = []
    for role in ROLES:
        src = sources[role.attr]
        name = layout.names[role.attr]
        out = dest / name
        if role.make == LINK:
            changed = _replace_link(src, out)
            rows.append(("linked" if changed else "ok", name, src.name))
        elif role.make == GZIP:
            changed = _replace_gzip(src, out)
            rows.append(("gzipped" if changed else "ok", name, f"{src.name} (gzip)"))
        elif role.make == UNTAR:
            member = layout.names[role.member_attr]
            changed = _replace_member(src, member, out)
            rows.append(("extracted" if changed else "ok", name, f"{src.name}:{member}"))
        else:  # pragma: no cover - ROLES is a literal
            raise ValueError(f"unknown make '{role.make}' for {role.attr}")

    text = meta_kernel_text(data_dir, layout, sources)
    mk = dest / layout.meta_kernel
    note = f"{len(layout.spice_order)} kernels from the data directory"
    if mk.is_symlink() or not mk.is_file() or mk.read_text() != text:
        tmp = mk.with_name(mk.name + ".part")
        tmp.write_text(text)
        os.replace(tmp, mk)
        rows.append(("wrote", layout.meta_kernel, note))
    else:
        rows.append(("ok", layout.meta_kernel, note))
    return rows


def verify(dest: Path, layout: Layout) -> list[str]:
    """Names layup checks for that are missing, dangling or empty in ``dest``."""
    bad = []
    for name in dict.fromkeys(list(layout.required) + layout.staged()):
        p = dest / name
        if not p.exists() or p.stat().st_size == 0:
            bad.append(name)
    return bad


def check_spice(dest: Path, data_dir: Path, layout: Layout) -> None:
    """Furnish the staged meta-kernel and confirm it loads exactly its kernels."""
    import spiceypy as spice

    mk = dest.absolute() / layout.meta_kernel
    spice.kclear()
    try:
        spice.furnsh(str(mk))
        loaded = [spice.kdata(i, "ALL")[0] for i in range(spice.ktotal("ALL"))]
        spice.str2et("2026 JAN 01 00:00:00 UTC")  # needs the leap-second kernel
    finally:
        spice.kclear()
    sources, _ = resolve(data_dir.absolute(), layout)
    by_name = {layout.names[r.attr]: sources[r.attr] for r in ROLES if r.make == LINK}
    expected = [str(mk)] + [str(data_dir.absolute() / by_name[n].name) for n in layout.spice_order]
    if loaded != expected:
        raise StagingError(data_dir, [f"{layout.meta_kernel} loaded {loaded}, expected {expected}"])


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--data-dir", type=Path, required=True, help="Engine data directory (the synced kernel set).")
    p.add_argument("--dest", type=Path, required=True, help="layup's data directory to build (passed to --ar).")
    args = p.parse_args(argv)

    layout = Layout.from_layup()
    print(f"layup {layout.version} data: {args.dest.absolute()}", flush=True)
    print(f"  from the data directory: {args.data_dir.absolute()}", flush=True)
    try:
        rows = stage(args.data_dir, args.dest, layout)
        check_spice(args.dest, args.data_dir, layout)
    except StagingError as e:
        print(f"ERROR: layup's data directory cannot be staged from {e.data_dir}:", file=sys.stderr)
        for line in e.problems:
            print(f"  - {line}", file=sys.stderr)
        print(
            "       layup reads each of these by name and would download a missing one from\n"
            "       its upstream address at fit time; this suite never lets it. Sync the kernel\n"
            "       set this checkout pins, or register the missing files in that set.",
            file=sys.stderr,
        )
        return 1
    for action, name, src in rows:
        print(f"  {action:<9} {name:<36} <- {src}")
    print(f"  checked   {layout.meta_kernel} loads {len(layout.spice_order)} kernels through SPICE")
    return 0


if __name__ == "__main__":
    sys.exit(main())
