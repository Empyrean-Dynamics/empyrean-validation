#!/usr/bin/env python3
"""Build once: pack the compiled channel binaries in prep, unpack and assert them in every leg.

A leg used to compile what it ran (the rust runner, the CLI runner, the C
runner, `validate-core`, the engine's C library, the wheel, the harness) at
about four minutes a leg, over a hundred legs a run, and every leg compiled
its own copy: the bytes that were measured were never the bytes that were
built anywhere else. Prep builds them once; this tool stages them with their
sha256 and every leg runs exactly those bytes.

    prebuilt.py pack   --out prebuilt --engine-version V
    prebuilt.py unpack --from prebuilt [--only NAME ...]
    prebuilt.py verify --manifest prebuilt/PREBUILT.json [--component NAME ...] [--engine-version V]

`pack` refuses by name when a component is missing. `unpack` verifies each
staged file against the manifest before placing it, restores the executable
bit (artifact upload drops it), and refuses a mismatch. `verify` is what the
Makefile's PREBUILT_BIN=1 mode runs in place of a build: it checks the placed
file exists and still hashes to the manifest, and NEVER builds, so a missing or
altered binary is a loud failure and not a silent rebuild.

Components live under three anchors, the sibling layout every job shares:
`validation` (this repo), `core` (../empyrean-core) and `engine` (../empyrean).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import stat
import sys
from pathlib import Path

SCHEMA = 1
MANIFEST = "PREBUILT.json"
FILES = "files"

ROOT = Path(__file__).resolve().parents[1]


def anchors(root: Path = ROOT) -> dict[str, Path]:
    return {"validation": root, "core": root.parent / "empyrean-core", "engine": root.parent / "empyrean"}


def pinned_engine_version(root: Path = ROOT) -> str:
    """The empyrean version the manifests pin, read where the Makefile reads it."""
    text = (root / "runners" / "rust" / "Cargo.toml").read_text()
    m = re.search(r'^empyrean = "=(.*)"', text, re.MULTILINE)
    if not m:
        raise PrebuiltError(f"could not read the pinned empyrean version from {root / 'runners/rust/Cargo.toml'}")
    return m.group(1)


def engine_lib_name() -> str:
    return "libempyrean.dylib" if sys.platform == "darwin" else "libempyrean.so"


# name -> (anchor, relative path or glob, executable)
def components() -> dict[str, tuple[str, str, bool]]:
    return {
        "harness": ("validation", "target/release/empyrean-validation", True),
        "rust-runner": ("validation", "runners/rust/target/release/validate", True),
        "cli-runner": ("validation", "runners/cli/target/release/empyrean-cli-runner", True),
        "c-runner": ("validation", "runners/c/runner", True),
        "core-runner": ("core", "target/release/validate-core", True),
        "engine-c": ("engine", f"target/release/{engine_lib_name()}", False),
        "wheel": ("engine", "empyrean-py/.venv/wheelhouse/*.whl", False),
    }


class PrebuiltError(Exception):
    pass


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def _resolve(anchor_dir: Path, rel: str, name: str) -> Path:
    if "*" in rel:
        found = sorted(anchor_dir.glob(rel))
        if len(found) != 1:
            raise PrebuiltError(
                f"component `{name}`: expected exactly one file matching {anchor_dir / rel}, found {len(found)}"
            )
        return found[0]
    return anchor_dir / rel


def pack(out: Path, engine_version: str, root: Path = ROOT) -> dict:
    anc = anchors(root)
    if out.exists():
        shutil.rmtree(out)
    (out / FILES).mkdir(parents=True)
    manifest = {"schema": SCHEMA, "engine_version": engine_version, "components": {}}
    missing = []
    for name, (anchor, rel, executable) in components().items():
        try:
            src = _resolve(anc[anchor], rel, name)
        except PrebuiltError as e:
            missing.append(str(e))
            continue
        if not src.is_file():
            missing.append(f"component `{name}`: {src} was not built")
            continue
        shutil.copy2(src, out / FILES / name)
        manifest["components"][name] = {
            "anchor": anchor,
            "path": src.relative_to(anc[anchor]).as_posix(),
            "sha256": sha256_file(src),
            "bytes": src.stat().st_size,
            "executable": executable,
        }
    if missing:
        raise PrebuiltError("cannot pack the prebuilt binaries:\n  " + "\n  ".join(missing))
    (out / MANIFEST).write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    return manifest


def load_manifest(path: Path) -> dict:
    try:
        m = json.loads(path.read_text())
    except OSError as e:
        raise PrebuiltError(f"no prebuilt manifest at {path}: {e}. The prebuilt-bin artifact was not downloaded here") from e
    except json.JSONDecodeError as e:
        raise PrebuiltError(f"{path} is not valid JSON: {e}") from e
    if m.get("schema") != SCHEMA or not isinstance(m.get("components"), dict):
        raise PrebuiltError(f"{path} is not a schema-{SCHEMA} prebuilt manifest")
    return m


def unpack(src: Path, only: list[str] | None, root: Path = ROOT) -> list[str]:
    anc = anchors(root)
    m = load_manifest(src / MANIFEST)
    names = only or sorted(m["components"])
    unknown = [n for n in names if n not in m["components"]]
    if unknown:
        raise PrebuiltError(f"the manifest has no component(s) {', '.join(unknown)}")
    for name in names:
        c = m["components"][name]
        staged = src / FILES / name
        if not staged.is_file():
            raise PrebuiltError(f"component `{name}`: staged file {staged} is missing")
        actual = sha256_file(staged)
        if actual != c["sha256"]:
            raise PrebuiltError(
                f"component `{name}`: staged file hashes to {actual}, manifest records {c['sha256']}; refusing it"
            )
        dest = anc[c["anchor"]] / c["path"]
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(staged, dest)
        dest.chmod(dest.stat().st_mode | (stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH if c["executable"] else 0))
    return names


def verify(manifest_path: Path, names: list[str] | None, engine_version: str | None, root: Path = ROOT) -> list[str]:
    anc = anchors(root)
    m = load_manifest(manifest_path)
    problems = []
    if engine_version is not None and m.get("engine_version") != engine_version:
        problems.append(
            f"the prebuilt binaries were built against empyrean {m.get('engine_version')}, "
            f"but the manifests pin {engine_version}"
        )
    for name in names or sorted(m["components"]):
        c = m["components"].get(name)
        if c is None:
            problems.append(f"component `{name}` is not in {manifest_path}")
            continue
        path = anc[c["anchor"]] / c["path"]
        if not path.is_file():
            problems.append(f"component `{name}`: {path} is missing (PREBUILT_BIN=1 never builds; unpack the prebuilt-bin artifact)")
        elif sha256_file(path) != c["sha256"]:
            problems.append(f"component `{name}`: {path} no longer matches the recorded sha256 {c['sha256']}")
        elif c["executable"] and not os.access(path, os.X_OK):
            problems.append(f"component `{name}`: {path} is not executable")
    return problems


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("pack")
    p.add_argument("--out", type=Path, required=True)
    p.add_argument("--engine-version", default=None, help="default: the rust runner's pin")
    p = sub.add_parser("unpack")
    p.add_argument("--from", dest="src", type=Path, required=True)
    p.add_argument("--only", nargs="+")
    p = sub.add_parser("verify")
    p.add_argument("--manifest", type=Path, required=True)
    p.add_argument("--component", nargs="+")
    p.add_argument("--engine-version")
    args = ap.parse_args(argv)
    try:
        if args.cmd == "pack":
            m = pack(args.out, args.engine_version or pinned_engine_version())
            for name, c in sorted(m["components"].items()):
                print(f"prebuilt: packed {name:12} {c['bytes']:>12} bytes sha256={c['sha256']}")
        elif args.cmd == "unpack":
            for name in unpack(args.src, args.only):
                print(f"prebuilt: unpacked {name} (hash verified)")
        else:
            problems = verify(args.manifest, args.component, args.engine_version)
            if problems:
                for why in problems:
                    print(f"ERROR: PREBUILT_BIN=1: {why}", file=sys.stderr)
                return 1
            print(f"prebuilt: verified {', '.join(args.component or ['all components'])}")
    except PrebuiltError as e:
        print(f"ERROR: prebuilt: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
