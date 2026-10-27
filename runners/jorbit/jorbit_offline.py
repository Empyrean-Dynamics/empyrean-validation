"""Run jorbit with no network access: kernels from the registered set, never a download.

jorbit reads its two SPICE kernels through astropy's download cache, keyed by
the ``ssd.jpl.nasa.gov`` URLs in ``jorbit.data.constants``
(``DEFAULT_PLANET_EPHEMERIS_URL`` -> ``de440.bsp``,
``DEFAULT_ASTEROID_EPHEMERIS_URL`` -> ``sb441-n16.bsp``), and ``import jorbit``
itself runs ``initialize_jorbit()``, which downloads both (about 760 MB) plus
the jorbit-mpchecker bundle (about 630 MB, from Hugging Face) into
``~/.astropy/cache`` whenever they are absent. None of that is visible to a
grep of this repository, and under the suite's seal any such fetch fails at the
socket.

This module makes the run-time path self-contained:

1. Reads jorbit's kernel URLs from ``jorbit/data/constants.py`` by parsing the
   file, without importing the package (whose ``__init__`` downloads).
2. Resolves each kernel to the file of the same name in the registered kernel
   set (``DATA_DIR``) every empyrean repo pins. A missing or empty file, or a
   kernel the set does not carry, is a hard stop that names the file.
3. Points astropy at a runner-private download cache and stages the set's files
   into it under jorbit's own URL keys (``import_file_to_cache``, astropy's
   documented way to pre-populate the cache), verifying the bytes on every run.
4. Turns astropy's internet access off for the process (``allow_internet``,
   IERS ``auto_download``), so anything else that would download raises at once
   instead of reaching out.
5. Imports jorbit with the mpchecker bundle's import-time download suppressed:
   no code path the runner uses reads it, and with internet off any later read
   of it raises. What was suppressed is returned for the runner to report.

Only the standard library is imported at module level, so the resolution logic
is unit-testable without jorbit, JAX or astropy installed.
"""

from __future__ import annotations

import ast
import filecmp
import importlib.util
import sys
import warnings
from collections.abc import Iterator
from contextlib import contextmanager
from pathlib import Path

# The kernels the registered kernel set carries under the names jorbit's URLs
# end in. jorbit reading anything else (a new default, DE441, the n373 set) is
# a deliberate decision to make here, never a silent download.
REGISTERED_SET_KERNELS = ("de440.bsp", "sb441-n16.bsp")

# The names in jorbit/data/constants.py whose URLs Ephemeris() reads through
# astropy's cache for the DE440 case the runner uses.
KERNEL_URL_CONSTANTS = (
    "DEFAULT_PLANET_EPHEMERIS_URL",
    "DEFAULT_ASTEROID_EPHEMERIS_URL",
)

# The URL root of the jorbit-mpchecker bundle that initialize_jorbit() fetches
# on import. Not a kernel; nothing this runner calls reads it.
MPCHECKER_URL_CONSTANT = "JORBIT_EPHEM_URL_BASE"

SETUP_HINT = (
    "The validation suite syncs the registered kernel set into DATA_DIR (the CI "
    "prep job does; locally `empyrean-data.py sync`). jorbit never downloads "
    "what the set carries."
)


class OfflineSetupError(RuntimeError):
    """jorbit cannot run from local files alone; the message names what is missing."""


def jorbit_constants_path() -> Path:
    """Locate ``jorbit/data/constants.py`` without importing the jorbit package."""
    spec = importlib.util.find_spec("jorbit")
    if spec is None or not spec.submodule_search_locations:
        raise OfflineSetupError(
            "jorbit is not installed in this interpreter; run runners/jorbit/setup.sh"
        )
    path = Path(next(iter(spec.submodule_search_locations))) / "data" / "constants.py"
    if not path.is_file():
        raise OfflineSetupError(
            f"{path} does not exist: this jorbit release moved its constants, so "
            "the kernel URLs it reads cannot be checked against the registered set"
        )
    return path


def read_string_constants(path: Path, names: tuple[str, ...]) -> dict[str, str]:
    """Read top-level string assignments from a Python file without executing it.

    Only plain ``NAME = "literal"`` assignments (implicit concatenation
    included) are read; a name that is absent or not a string literal is left
    out, and the caller decides whether that is fatal.
    """
    tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    found: dict[str, str] = {}
    for node in tree.body:
        if isinstance(node, ast.Assign):
            targets, value = node.targets, node.value
        elif isinstance(node, ast.AnnAssign) and node.value is not None:
            targets, value = [node.target], node.value
        else:
            continue
        for target in targets:
            if isinstance(target, ast.Name) and target.id in names:
                try:
                    literal = ast.literal_eval(value)
                except ValueError:
                    continue
                if isinstance(literal, str):
                    found[target.id] = literal
    return found


def kernel_sources(urls: dict[str, str], data_dir: Path) -> dict[str, Path]:
    """Map every kernel URL jorbit reads to its file in the registered kernel set.

    Raises:
        OfflineSetupError: a URL constant is missing, names a kernel the set
            does not carry, or the set's file is missing or empty.
    """
    sources: dict[str, Path] = {}
    for name in KERNEL_URL_CONSTANTS:
        url = urls.get(name)
        if not url:
            raise OfflineSetupError(
                f"jorbit's constants no longer define {name} as a string; the "
                "kernels it reads cannot be resolved to the registered set. "
                "Update runners/jorbit/offline.py for this jorbit release."
            )
        filename = url.rstrip("/").rsplit("/", 1)[-1]
        if filename not in REGISTERED_SET_KERNELS:
            raise OfflineSetupError(
                f"jorbit reads {filename} ({name} = {url}), which the registered "
                f"kernel set does not carry (it carries {', '.join(REGISTERED_SET_KERNELS)}). "
                "Adding a kernel is a decision for the set, not a download at run time."
            )
        src = data_dir / filename
        if not src.is_file() or src.stat().st_size == 0:
            raise OfflineSetupError(
                f"{src} is missing or empty. jorbit reads {filename} for {name}. {SETUP_HINT}"
            )
        sources[url] = src
    return sources


def disable_astropy_downloads() -> None:
    """Make every astropy download in this process raise instead of connecting."""
    from astropy.utils import data, iers

    data.conf.allow_internet = False
    iers.conf.auto_download = False


def use_private_cache(cache_root: Path) -> Path:
    """Point astropy's download cache at ``cache_root`` for the rest of the process.

    Returns the cache directory astropy now resolves, after checking that it is
    inside ``cache_root``.
    """
    from astropy.config import paths

    cache_root.mkdir(parents=True, exist_ok=True)
    # Entered and never exited: the cache must stay redirected for every
    # Ephemeris() jorbit builds later in the run.
    paths.set_temp_cache(str(cache_root)).__enter__()
    resolved = Path(paths.get_cache_dir()).resolve()
    root = cache_root.resolve()
    if resolved != root and root not in resolved.parents:
        raise OfflineSetupError(
            f"astropy resolves its cache to {resolved}, not inside {root}; "
            "refusing to let jorbit read kernels from an unverified cache"
        )
    return resolved


def _cached_path(url: str) -> Path | None:
    from astropy.utils import data

    with warnings.catch_warnings():
        # An empty private cache warns that its download/ directory is missing.
        warnings.simplefilter("ignore", data.CacheMissingWarning)
        if not data.is_url_in_cache(url):
            return None
        return Path(data.download_file(url, cache=True))


def stage_kernels(sources: dict[str, Path], cache_dir: Path) -> list[str]:
    """Stage each set file into the active astropy cache under jorbit's URL key.

    A cached copy is kept only when it is byte-identical to the set's file;
    anything else is replaced. Every staged entry is then re-read through
    ``download_file`` (exactly what jorbit calls) and must resolve inside
    ``cache_dir`` with the set's bytes. Returns one status line per kernel.
    """
    from astropy.utils import data

    lines = []
    for url, src in sources.items():
        cached = _cached_path(url)
        if cached is not None and filecmp.cmp(cached, src, shallow=False):
            status = "ok      "
        else:
            status = "staged  " if cached is None else "restaged"
            data.import_file_to_cache(
                url, str(src), remove_original=False, replace=True
            )
        got = _cached_path(url)
        if got is None or cache_dir.resolve() not in got.resolve().parents:
            raise OfflineSetupError(
                f"staging {src.name} under {url} did not land in {cache_dir}"
            )
        if not filecmp.cmp(got, src, shallow=False):
            raise OfflineSetupError(
                f"the staged {src.name} differs from the set's {src}"
            )
        lines.append(f"{status} {src.name:<16} {src} -> astropy cache key {url}")
    return lines


@contextmanager
def suppress_url_check(url_prefix: str, suppressed: list[str]) -> Iterator[None]:
    """Within the block, astropy reports every URL under ``url_prefix`` as cached.

    Used only around ``import jorbit``, whose ``initialize_jorbit()`` downloads
    the mpchecker bundle when ``is_url_in_cache`` says it is absent. Each URL so
    answered is appended to ``suppressed``. On exit the real function is
    restored, in astropy and in every jorbit module that captured the stand-in
    with ``from astropy.utils.data import is_url_in_cache``.
    """
    from astropy.utils import data

    real = data.is_url_in_cache

    def answer(url, *args, **kwargs):
        if isinstance(url, str) and url.startswith(url_prefix):
            suppressed.append(url)
            return True
        return real(url, *args, **kwargs)

    data.is_url_in_cache = answer
    try:
        yield
    finally:
        data.is_url_in_cache = real
        for name, module in list(sys.modules.items()):
            if (name == "jorbit" or name.startswith("jorbit.")) and getattr(
                module, "is_url_in_cache", None
            ) is answer:
                module.is_url_in_cache = real


def import_jorbit_offline(data_dir: Path, cache_root: Path):
    """Import jorbit reading only local files.

    Returns ``(jorbit, status_lines, suppressed_urls)``. Raises
    ``OfflineSetupError`` with the reason when that is not possible.
    """
    if "jorbit" in sys.modules:
        raise OfflineSetupError(
            "jorbit was imported before its kernels were staged; its import-time "
            "initializer may already have tried to download"
        )
    constants = jorbit_constants_path()
    urls = read_string_constants(
        constants, (*KERNEL_URL_CONSTANTS, MPCHECKER_URL_CONSTANT)
    )
    sources = kernel_sources(urls, data_dir)
    mpchecker_base = urls.get(MPCHECKER_URL_CONSTANT)
    if not mpchecker_base:
        raise OfflineSetupError(
            f"jorbit's constants no longer define {MPCHECKER_URL_CONSTANT}; its import-time "
            "download cannot be identified. Update runners/jorbit/offline.py for this release."
        )
    disable_astropy_downloads()
    cache_dir = use_private_cache(cache_root)
    lines = stage_kernels(sources, cache_dir)
    suppressed: list[str] = []
    with suppress_url_check(mpchecker_base, suppressed):
        import jorbit
    return jorbit, lines, suppressed
