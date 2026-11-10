#!/usr/bin/env python3
"""Fetch the prebuilt engine library once, through the authenticated API.

Every job that builds or runs against `empyrean` needs `libempyrean`.
`empyrean-sys`'s build script would download it from the public release URL
(`github.com/<repo>/releases/download/...`), once per job and with no retry. On
2026-11-09 that URL answered HTTP 504 for every asset while the authenticated
API (`gh release download`) worked, which would have failed every job of a run
that had already spent hours on the science.

So the run fetches the library exactly once, in prep, and every other job
restores it from the cache:

    engine_lib.py fetch   --version V --dest DIR --checksums crate/checksums.txt
    engine_lib.py restore --version V --dest DIR            (offline, verify only)

`fetch` downloads `libempyrean-<platform>.tar.gz` for the pinned version through
the authenticated GitHub API (GH_TOKEN; `gh release download` calls the same API,
and this needs no `gh`, which neither a minimal container nor `gh act` carries),
retrying with backoff, verifies its sha256 against the `empyrean-sys` crate's `checksums.txt` and REFUSES BY NAME on a
mismatch, extracts it, and records what it did beside it. `restore` never
touches the network: it re-verifies the cached tarball against the checksums
copied beside it and extracts. Both print one `engine-lib:` log line that names
the version, the platform, the sha256 and the source (api or cache), so each
job's log proves where its engine came from.

The directory is what the workflow exports as EMPYREAN_LIB_DIR (build-time) and,
as the library file itself, EMPYREAN_LIB (run-time); `empyrean-sys` then never
reaches the public URL, because EMPYREAN_LIB_DIR wins its resolution order and
an unpopulated directory is a build panic, not a download.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from collections.abc import Callable
from pathlib import Path

REPO = "Empyrean-Dynamics/empyrean"
API_URL = "https://api.github.com"
RECORD = "engine-lib.json"
CHECKSUMS_COPY = "checksums.txt"
RETRIES = 6
BACKOFF_S = 5.0
BACKOFF_CAP_S = 60.0


class EngineLibError(Exception):
    """A refusal, named: the message says what was wrong and what was not used."""


def platform_stem(system: str | None = None, machine: str | None = None) -> str:
    system = (system or sys.platform).lower()
    machine = (machine or os.uname().machine).lower()
    arch = {"x86_64": "x86_64", "amd64": "x86_64", "arm64": "aarch64", "aarch64": "aarch64"}.get(machine)
    os_name = {"darwin": "macos", "linux": "linux"}.get(system)
    if not arch or not os_name:
        raise EngineLibError(f"no prebuilt libempyrean is published for {system}/{machine}")
    return f"libempyrean-{os_name}-{arch}"


def lib_filename(stem: str) -> str:
    return "libempyrean.dylib" if "-macos-" in stem else "libempyrean.so"


def parse_checksums(text: str) -> dict[str, str]:
    """`<asset-stem> <sha256>` per line, `#` comments ignored (the crate's format)."""
    out: dict[str, str] = {}
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split(None, 1)
        if len(parts) == 2:
            out[parts[0]] = parts[1].strip()
    return out


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def expected_sha(checksums: Path, stem: str) -> str:
    try:
        table = parse_checksums(checksums.read_text())
    except OSError as e:
        raise EngineLibError(f"cannot read the empyrean-sys checksums at {checksums}: {e}") from e
    if stem not in table:
        raise EngineLibError(f"{checksums} pins no checksum for {stem}; refusing to use an unverified binary")
    return table[stem]


class _DropAuthOnCrossHostRedirect(urllib.request.HTTPRedirectHandler):
    """The asset endpoint answers 302 to a pre-signed CDN URL. That URL carries its
    own credential in the query string and rejects a request that also bears our
    token, and our token has no business at another host: drop it on the hop."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        new = super().redirect_request(req, fp, code, msg, headers, newurl)
        if new is not None and urllib.parse.urlsplit(newurl).netloc != urllib.parse.urlsplit(req.full_url).netloc:
            new.remove_header("Authorization")
        return new


class _ApiError(Exception):
    def __init__(self, url: str, status: int | None, detail: str):
        super().__init__(f"{url}: {'HTTP ' + str(status) if status else 'no response'} {detail}".strip())
        self.status = status


def _api_get(url: str, token: str, accept: str, timeout: float = 120.0) -> bytes:
    req = urllib.request.Request(
        url,
        headers={
            "Authorization": f"Bearer {token}",
            "Accept": accept,
            "X-GitHub-Api-Version": "2022-11-28",
            "User-Agent": "empyrean-validation-engine-lib",
        },
    )
    opener = urllib.request.build_opener(_DropAuthOnCrossHostRedirect)
    try:
        with opener.open(req, timeout=timeout) as resp:
            return resp.read()
    except urllib.error.HTTPError as e:
        raise _ApiError(url, e.code, str(e.reason)) from e
    except (urllib.error.URLError, TimeoutError, OSError) as e:
        raise _ApiError(url, None, str(e)) from e


def api_download(
    version: str,
    stem: str,
    out_dir: Path,
    *,
    repo: str = REPO,
    api_url: str = API_URL,
    retries: int = RETRIES,
    sleep: Callable[[float], None] = time.sleep,
    log: Callable[[str], None] = lambda m: print(m, file=sys.stderr),
) -> Path:
    """Download the release asset through the AUTHENTICATED GitHub API, with backoff
    on a server error or a dropped connection. Never the public release URL."""
    token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    if not token:
        raise EngineLibError(
            "no GH_TOKEN/GITHUB_TOKEN in the environment: the engine library is fetched through the "
            "authenticated API only, and this job will not fall back to the public release URL"
        )
    asset = f"{stem}.tar.gz"
    last = ""
    for attempt in range(1, retries + 1):
        try:
            release = json.loads(_api_get(f"{api_url}/repos/{repo}/releases/tags/v{version}", token, "application/vnd.github+json"))
            match = [a for a in release.get("assets", []) if a.get("name") == asset]
            if not match:
                raise EngineLibError(f"release v{version} of {repo} has no asset {asset}")
            data = _api_get(f"{api_url}/repos/{repo}/releases/assets/{match[0]['id']}", token, "application/octet-stream")
            out_dir.mkdir(parents=True, exist_ok=True)
            got = out_dir / asset
            got.write_bytes(data)
            return got
        except _ApiError as e:
            last = str(e)
            if e.status is not None and e.status < 500 and e.status != 429:
                raise EngineLibError(f"the authenticated API refused the request: {last}; not falling back to the public release URL") from e
            log(f"engine-lib: attempt {attempt}/{retries} failed for {asset} (v{version}): {last}")
            if attempt < retries:
                sleep(min(BACKOFF_S * 2 ** (attempt - 1), BACKOFF_CAP_S))
    raise EngineLibError(
        f"could not download {asset} for v{version} from {repo} through the authenticated API after "
        f"{retries} attempts (last: {last}); not falling back to the public release URL"
    )


def _safe_extract(tar_path: Path, dest: Path) -> None:
    with tarfile.open(tar_path, "r:gz") as tar:
        for m in tar.getmembers():
            name = Path(m.name)
            if name.is_absolute() or ".." in name.parts or not (m.isfile() or m.isdir()):
                raise EngineLibError(f"{tar_path.name} holds an unsafe member {m.name!r}; refusing to extract it")
        tar.extractall(dest)


def ensure(
    version: str,
    dest: Path,
    *,
    checksums: Path | None,
    offline: bool,
    stem: str | None = None,
    repo: str = REPO,
    api_url: str = API_URL,
    retries: int = RETRIES,
    sleep: Callable[[float], None] = time.sleep,
    log: Callable[[str], None] = print,
) -> Path:
    """Make `dest` hold the verified, extracted library; return the library file."""
    stem = stem or platform_stem()
    dest.mkdir(parents=True, exist_ok=True)
    tar_path = dest / f"{stem}.tar.gz"
    copy = dest / CHECKSUMS_COPY
    if checksums is None:
        if not copy.is_file():
            raise EngineLibError(
                f"no checksums to verify against: {copy} is absent. The engine-library cache for v{version} "
                "was not restored; prep is its only writer and this job will not download it"
            )
        checksums = copy
    want = expected_sha(checksums, stem)

    source = "cache"
    if tar_path.is_file() and sha256_file(tar_path) == want:
        pass
    else:
        if tar_path.is_file():
            bad = sha256_file(tar_path)
            tar_path.unlink()
            if offline:
                raise EngineLibError(
                    f"cached {tar_path.name} for v{version} has sha256 {bad}, expected {want}; refusing it"
                )
        if offline:
            raise EngineLibError(
                f"no cached {tar_path.name} for v{version} in {dest}; this job restores the engine library, "
                "it does not download it"
            )
        with tempfile.TemporaryDirectory(dir=dest) as tmp:
            got = api_download(version, stem, Path(tmp), repo=repo, api_url=api_url, retries=retries, sleep=sleep, log=lambda m: print(m, file=sys.stderr))
            actual = sha256_file(got)
            if actual != want:
                raise EngineLibError(
                    f"libempyrean checksum mismatch for {stem}.tar.gz (v{version}): expected {want} (from "
                    f"{checksums}), got {actual}. Refusing to use an unverified binary."
                )
            shutil.move(str(got), tar_path)
        source = "api"
    if checksums != copy:
        shutil.copyfile(checksums, copy)

    lib = dest / lib_filename(stem)
    record = dest / RECORD
    recorded = json.loads(record.read_text()) if record.is_file() else {}
    if not lib.is_file() or recorded.get("sha256") != want:
        if lib.is_file():
            lib.unlink()
        _safe_extract(tar_path, dest)
        if not lib.is_file():
            raise EngineLibError(f"{tar_path.name} did not contain {lib.name}")
    record.write_text(
        json.dumps({"version": version, "stem": stem, "sha256": want, "lib": lib.name, "source": source}, indent=2) + "\n"
    )
    log(
        f"engine-lib: v{version} {stem} sha256={want} source={source} dir={dest} "
        "(authenticated API or cache; the public release URL is not used)"
    )
    return lib


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in ("fetch", "restore"):
        p = sub.add_parser(name)
        p.add_argument("--version", required=True)
        p.add_argument("--dest", type=Path, required=True)
        p.add_argument("--stem", default=None, help="asset stem (default: this platform's)")
        if name == "fetch":
            p.add_argument("--checksums", type=Path, required=True, help="the empyrean-sys crate's checksums.txt")
            p.add_argument("--repo", default=REPO)
            p.add_argument("--api-url", default=API_URL)
            p.add_argument("--retries", type=int, default=RETRIES)
    args = ap.parse_args(argv)
    try:
        if args.cmd == "fetch":
            ensure(args.version, args.dest, checksums=args.checksums, offline=False, stem=args.stem, repo=args.repo, api_url=args.api_url, retries=args.retries)
        else:
            ensure(args.version, args.dest, checksums=None, offline=True, stem=args.stem)
    except EngineLibError as e:
        print(f"ERROR: engine-lib: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
