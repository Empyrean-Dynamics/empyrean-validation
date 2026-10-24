#!/usr/bin/env python3
"""Mint, verify, pin, and fetch the validation inputs snapshot.

One snapshot carries everything the validation suite reads from JPL and the
MPC: the SBDB responses and Horizons vectors/ephemerides in the engine's own
disk-cache format (`horizons/`, `sbdb/`), and the ADES PSV astrometry
(`psv/`, `psv-radar/`). It lives, immutable and content-addressed, at
`gs://empyrean-data/validation/<snapshot_id>/`, and the suite only ever reads
it from there (design: lang/design/validation-inputs-snapshot.md).

This tool is the mint PR's producer and the consumption PR's reader; both code
against the same contract (`.claude/drafts/validation-inputs/contract.md`). It
replaces the PSV-only `fetch-fixtures.sh` / `upload-fixture-snapshot.sh`.

A snapshot tree on disk::

    <tree>/psv/<name>.psv          <tree>/horizons/<file>.json   <tree>/MANIFEST.json
    <tree>/psv-radar/<name>.psv    <tree>/sbdb/<file>.json

The four product directories are the products, exactly. Anything else under
<tree> (e.g. fixtures/windows.json) is never listed, verified, or pruned. The
`fetch` extra/prune pass acts only on snapshot-shaped files — `*.psv` directly
in psv/ and psv-radar/, `*.json` directly in horizons/ and sbdb/ — so a tracked
companion that lives inside a product directory (psv/README.md, a dotfile) and
any subdirectory is left alone, never reported as extra and never pruned.

Verbs (see the contract for the authoritative spec)::

    inputs.py manifest --tree T --provenance P.json [--partial] [--out ...]
    inputs.py summary  --tree T
    inputs.py diff     OLD_MANIFEST NEW_MANIFEST
    inputs.py upload   --tree T [--bucket ...] [--yes]
    inputs.py pin      --manifest T/MANIFEST.json --out fixtures/manifest.json
                       --minted-by URL [--bucket ...] [--allow-partial]
    inputs.py fetch    --pin fixtures/manifest.json --dest fixtures
                       [--prune] [--offline] [--allow-partial]

Exit codes: 0 ok, 1 content/verification failure, 2 usage. Content failures
are listed one per line on stderr, each naming the path and the
expected/actual value. Nothing is ever written partially: a temp file is
written beside the target, verified, then `os.replace`d into place.

gcloud: every bucket operation is `gcloud storage` (never curl); credentials
are the caller's (WIF in CI, the owner locally). The binary is injectable for
tests (a fake `gcloud` via INPUTS_GCLOUD_BIN, or a runner passed to the verb).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

PRODUCTS = ("psv", "psv-radar", "horizons", "sbdb")
PSV_PRODUCTS = ("psv", "psv-radar")
# The snapshot-shaped file extension per product. The fetch extra/prune pass
# acts only on files with this suffix directly inside the product directory;
# a README.md, a dotfile, or a subdirectory is never extra and never pruned.
PRODUCT_EXT = {"psv": ".psv", "psv-radar": ".psv", "horizons": ".json", "sbdb": ".json"}
DEFAULT_BUCKET = "gs://empyrean-data/validation"
SCHEMA = 1

# A PSV header line is a `|`-joined line whose fields are all bare column-name
# identifiers (`permID`, `obsTime`, `rmsRA`, ...). Optical files carry one such
# line (after `# version`); radar files carry a second one at the start of the
# `<radar>` table. Data rows never qualify: every observation carries an
# obsTime, ra/dec or delay/doppler field that is not a bare identifier. This
# reproduces the `data_rows` values in fixtures/manifest.json on main exactly.
_IDENT = re.compile(r"^[A-Za-z][A-Za-z0-9]*$")


class ContentError(Exception):
    """A content or verification failure (exit 1). Carries one message per line."""

    def __init__(self, messages: str | list[str]):
        self.messages = [messages] if isinstance(messages, str) else list(messages)
        super().__init__("\n".join(self.messages))


# ── hashing + ids ──────────────────────────────────────────────────────────


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def tree_hash(files: list[dict]) -> str:
    """sha256 over ``"<path>\\t<sha256>\\n"`` for every file, sorted by path."""
    h = hashlib.sha256()
    for f in sorted(files, key=lambda r: r["path"]):
        h.update(f"{f['path']}\t{f['sha256']}\n".encode())
    return h.hexdigest()


def snapshot_id(th: str, created: datetime) -> str:
    """``<UTC date YYYY-MM-DD>-<first 12 hex of tree_hash>`` — content-addressed."""
    return f"{created.strftime('%Y-%m-%d')}-{th[:12]}"


def is_header_line(line: str) -> bool:
    if "|" not in line:
        return False
    fields = [c.strip() for c in line.split("|")]
    return all(c and _IDENT.match(c) for c in fields)


def data_rows(text: str) -> int:
    """PSV observation rows: total lines - comment lines - header lines."""
    lines = text.splitlines()
    hashes = sum(1 for ln in lines if ln.startswith("#"))
    headers = sum(1 for ln in lines if is_header_line(ln))
    return len(lines) - hashes - headers


# ── atomic write ─────────────────────────────────────────────────────────────


def atomic_write_json(path: Path, obj: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + f".tmp.{os.getpid()}")
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(obj, f, indent=2, sort_keys=False)
        f.write("\n")
    os.replace(tmp, path)


def load_json(path: Path) -> dict:
    try:
        with open(path, encoding="utf-8") as f:
            return json.load(f)
    except FileNotFoundError as e:
        raise ContentError(f"{path}: not found") from e
    except json.JSONDecodeError as e:
        raise ContentError(f"{path}: not valid JSON ({e})") from e


# ── gcloud ───────────────────────────────────────────────────────────────────


class Gcloud:
    """Thin `gcloud storage` wrapper. The binary is injectable for tests."""

    def __init__(self, binary: str | None = None):
        self.binary = binary or os.environ.get("INPUTS_GCLOUD_BIN", "gcloud")

    def _run(self, args: list[str]) -> subprocess.CompletedProcess:
        return subprocess.run(
            [self.binary, "storage", *args],
            capture_output=True,
            text=True,
            check=False,
        )

    def ls(self, uri: str) -> list[str]:
        """Object URIs under ``uri``; [] when the prefix matched nothing."""
        proc = self._run(["ls", uri])
        if proc.returncode != 0:
            err = (proc.stderr or "").strip()
            if "matched no objects" in err or "NotFound" in err or "not found" in err.lower():
                return []
            raise ContentError(f"gcloud storage ls {uri} failed (exit {proc.returncode}): {err}")
        return [ln.strip() for ln in proc.stdout.splitlines() if ln.strip()]

    def cp(self, src: str, dst: str, recursive: bool = False) -> None:
        args = ["cp"]
        if recursive:
            args.append("--recursive")
        args += [src, dst]
        proc = self._run(args)
        if proc.returncode != 0:
            raise ContentError(
                f"gcloud storage cp {src} {dst} failed (exit {proc.returncode}): "
                f"{(proc.stderr or '').strip()}"
            )


# ── manifest ───────────────────────────────────────────────────────────────


def _list_product_files(tree: Path, partial: bool) -> list[Path]:
    out: list[Path] = []
    errors: list[str] = []
    for product in PRODUCTS:
        d = tree / product
        if not d.is_dir():
            errors.append(f"{d}: product directory is missing")
            continue
        files = sorted(p for p in d.rglob("*") if p.is_file())
        if not files and not partial:
            errors.append(f"{d}: product directory is empty (pass --partial for a partial snapshot)")
        out.extend(files)
    if errors:
        raise ContentError(errors)
    return out


def build_manifest(tree: Path, provenance: dict, partial: bool) -> dict:
    tree = tree.resolve()
    paths = _list_product_files(tree, partial)
    files: list[dict] = []
    errors: list[str] = []
    counts = {p: 0 for p in PRODUCTS}
    for p in paths:
        rel = p.relative_to(tree).as_posix()
        product = rel.split("/", 1)[0]
        try:
            raw = p.read_bytes()
        except OSError as e:
            errors.append(f"{rel}: unreadable ({e})")
            continue
        entry = {
            "path": rel,
            "sha256": hashlib.sha256(raw).hexdigest(),
            "bytes": len(raw),
        }
        if rel.endswith(".psv"):
            entry["data_rows"] = data_rows(raw.decode("utf-8", errors="replace"))
        files.append(entry)
        counts[product] += 1
    if errors:
        raise ContentError(errors)
    files.sort(key=lambda r: r["path"])
    th = tree_hash(files)
    created = datetime.now(timezone.utc)
    return {
        "schema": SCHEMA,
        "snapshot_id": snapshot_id(th, created),
        "created_utc": created.strftime("%Y-%m-%dT%H:%M:%SZ"),
        "tree_hash": th,
        "partial": partial,
        "products": counts,
        "provenance": provenance,
        "files": files,
    }


def cmd_manifest(args: argparse.Namespace) -> int:
    tree = Path(args.tree)
    provenance = load_json(Path(args.provenance))
    manifest = build_manifest(tree, provenance, args.partial)
    out = Path(args.out) if args.out else tree / "MANIFEST.json"
    atomic_write_json(out, manifest)
    print(manifest["snapshot_id"])
    return 0


# ── summary ──────────────────────────────────────────────────────────────────


def jd_to_mjd(jd: float) -> float:
    return jd - 2400000.5


def summary(tree: Path) -> str:
    tree = tree.resolve()
    lines: list[str] = []
    lines.append(f"Validation inputs snapshot: {tree}")
    lines.append("")
    lines.append(f"{'product':<12}{'files':>8}{'bytes':>16}")
    lines.append(f"{'-' * 12}{'-' * 8:>8}{'-' * 16:>16}")
    for product in PRODUCTS:
        d = tree / product
        files = [p for p in d.rglob("*") if p.is_file()] if d.is_dir() else []
        total = sum(p.stat().st_size for p in files)
        lines.append(f"{product:<12}{len(files):>8}{total:>16,}")
    lines.append("")
    lines.append("SBDB epochs (MJD TDB):")
    sbdb = tree / "sbdb"
    if sbdb.is_dir():
        for p in sorted(sbdb.glob("*.json")):
            try:
                epoch_jd = load_json(p)["response"]["orbit"]["epoch"]
                epoch = f"{jd_to_mjd(float(epoch_jd)):.6f}"
            except (ContentError, KeyError, TypeError, ValueError):
                epoch = "(no response.orbit.epoch)"
            lines.append(f"  {p.name:<48} {epoch}")
    lines.append("")
    lines.append("PSV data rows:")
    for product in PSV_PRODUCTS:
        d = tree / product
        if not d.is_dir():
            continue
        for p in sorted(d.glob("*.psv")):
            lines.append(f"  {product}/{p.name:<44} {data_rows(p.read_text()):>8,}")
    return "\n".join(lines)


def cmd_summary(args: argparse.Namespace) -> int:
    print(summary(Path(args.tree)))
    return 0


# ── diff ─────────────────────────────────────────────────────────────────────


def diff(old: dict, new: dict) -> str:
    old_files = {f["path"]: f for f in old.get("files", [])}
    new_files = {f["path"]: f for f in new.get("files", [])}
    lines: list[str] = []
    lines.append(f"snapshot {old.get('snapshot_id', '?')}  ->  {new.get('snapshot_id', '?')}")
    totals = {"added": 0, "removed": 0, "changed": 0}
    for product in PRODUCTS:
        o = {p: f for p, f in old_files.items() if p.split("/", 1)[0] == product}
        n = {p: f for p, f in new_files.items() if p.split("/", 1)[0] == product}
        added = sorted(set(n) - set(o))
        removed = sorted(set(o) - set(n))
        changed = sorted(p for p in set(o) & set(n) if o[p]["sha256"] != n[p]["sha256"])
        totals["added"] += len(added)
        totals["removed"] += len(removed)
        totals["changed"] += len(changed)
        lines.append("")
        lines.append(f"[{product}]  +{len(added)}  -{len(removed)}  ~{len(changed)}")
        for p in added:
            lines.append(f"  + {p}")
        for p in removed:
            lines.append(f"  - {p}")
        for p in changed:
            msg = f"  ~ {p}"
            if "data_rows" in o[p] or "data_rows" in n[p]:
                od = o[p].get("data_rows", 0)
                nd = n[p].get("data_rows", 0)
                msg += f"  ({od} -> {nd} rows, {nd - od:+d})"
            lines.append(msg)
    lines.append("")
    lines.append(
        f"totals: +{totals['added']} added, -{totals['removed']} removed, "
        f"~{totals['changed']} changed  "
        f"({len(old_files)} -> {len(new_files)} files)"
    )
    return "\n".join(lines)


def cmd_diff(args: argparse.Namespace) -> int:
    print(diff(load_json(Path(args.old)), load_json(Path(args.new))))
    return 0


# ── upload ───────────────────────────────────────────────────────────────────


def upload(tree: Path, bucket: str, yes: bool, gcloud: Gcloud) -> int:
    tree = tree.resolve()
    manifest = load_json(tree / "MANIFEST.json")
    sid = manifest["snapshot_id"]
    dest = f"{bucket}/{sid}"

    errors: list[str] = []
    for f in manifest["files"]:
        p = tree / f["path"]
        if not p.is_file():
            errors.append(f"{f['path']}: listed in MANIFEST.json but missing from the tree")
            continue
        actual = sha256_file(p)
        if actual != f["sha256"]:
            errors.append(f"{f['path']}: sha256 expected {f['sha256']} actual {actual}")
    if errors:
        raise ContentError(errors)

    existing = gcloud.ls(f"{dest}/**")
    if existing:
        raise ContentError(
            [f"{dest}/ already holds {len(existing)} object(s); a snapshot is immutable — refusing to upload."]
            + [f"  existing: {u}" for u in existing[:5]]
        )

    expected = len(manifest["files"]) + 1
    if not yes:
        print(f"Would upload {len(manifest['files'])} file(s) + MANIFEST.json to {dest}/")
        print(f"  (re-run with --yes to upload; post-upload object count must be {expected})")
        return 0

    for product in PRODUCTS:
        d = tree / product
        if d.is_dir() and any(d.iterdir()):
            gcloud.cp(str(d), f"{dest}/", recursive=True)
    gcloud.cp(str(tree / "MANIFEST.json"), f"{dest}/MANIFEST.json")

    listed = gcloud.ls(f"{dest}/**")
    if len(listed) != expected:
        raise ContentError(
            f"{dest}/ holds {len(listed)} object(s) after upload, expected {expected} "
            f"({len(manifest['files'])} files + MANIFEST.json) — upload is incomplete."
        )
    print(sid)
    print(f"uploaded {len(manifest['files'])} file(s) + MANIFEST.json to {dest}/")
    return 0


def cmd_upload(args: argparse.Namespace) -> int:
    return upload(Path(args.tree), args.bucket, args.yes, Gcloud())


# ── pin ──────────────────────────────────────────────────────────────────────


def build_pin(manifest: dict, minted_by: str, bucket: str, allow_partial: bool) -> dict:
    if manifest.get("partial") and not allow_partial:
        raise ContentError(
            f"manifest {manifest.get('snapshot_id', '?')} is partial; "
            f"refusing to pin it for a full run (pass --allow-partial to override)."
        )
    return {
        "schema": SCHEMA,
        "snapshot_id": manifest["snapshot_id"],
        "bucket": bucket,
        "tree_hash": manifest["tree_hash"],
        "files_total": len(manifest["files"]),
        "products": manifest["products"],
        "partial": manifest.get("partial", False),
        "minted_utc": manifest["created_utc"],
        "minted_by": minted_by,
    }


def cmd_pin(args: argparse.Namespace) -> int:
    manifest = load_json(Path(args.manifest))
    pin = build_pin(manifest, args.minted_by, args.bucket, args.allow_partial)
    atomic_write_json(Path(args.out), pin)
    print(pin["snapshot_id"])
    return 0


# ── fetch ────────────────────────────────────────────────────────────────────


def _verified(path: Path, entry: dict) -> bool:
    try:
        if path.stat().st_size != entry["bytes"]:
            return False
    except OSError:
        return False
    return sha256_file(path) == entry["sha256"]


def _resolve_manifest(pin: dict, dest: Path, base: str, offline: bool, gcloud: Gcloud) -> dict:
    sid = pin["snapshot_id"]
    local = dest / "MANIFEST.json"
    if local.is_file():
        m = load_json(local)
        if m.get("snapshot_id") == sid and m.get("tree_hash") == pin["tree_hash"]:
            return m
    if offline:
        raise ContentError(
            f"MANIFEST.json for {sid} is absent or does not match the pin, and --offline was given."
        )
    tmp = local.with_name(local.name + f".part.{os.getpid()}")
    dest.mkdir(parents=True, exist_ok=True)
    gcloud.cp(f"{base}/MANIFEST.json", str(tmp))
    try:
        m = load_json(tmp)
        recomputed = tree_hash(m.get("files", []))
        errs = []
        if recomputed != m.get("tree_hash"):
            errs.append(
                f"MANIFEST.json: tree_hash over files[] is {recomputed} "
                f"but the manifest records {m.get('tree_hash')}"
            )
        if m.get("tree_hash") != pin["tree_hash"]:
            errs.append(
                f"MANIFEST.json: tree_hash {m.get('tree_hash')} does not match the pin's {pin['tree_hash']}"
            )
        if errs:
            raise ContentError(errs)
        os.replace(tmp, local)
    finally:
        if tmp.exists():
            tmp.unlink()
    return m


def fetch(
    pin_path: Path,
    dest: Path,
    prune: bool,
    offline: bool,
    gcloud: Gcloud,
    allow_partial: bool = False,
) -> int:
    pin = load_json(pin_path)
    if pin.get("partial") and not allow_partial:
        raise ContentError(
            f"pin {pin.get('snapshot_id', '?')} is partial; refusing to fetch for a full run "
            f"(pass --allow-partial to override)."
        )
    dest = dest.resolve()
    sid = pin["snapshot_id"]
    base = f"{pin['bucket']}/{sid}"

    manifest = _resolve_manifest(pin, dest, base, offline, gcloud)
    if manifest.get("partial") and not allow_partial:
        raise ContentError(f"snapshot {sid} is partial; refusing to fetch for a full run.")

    errors: list[str] = []
    kept = fetched = pruned = 0
    for entry in manifest["files"]:
        p = dest / entry["path"]
        if _verified(p, entry):
            kept += 1
            continue
        if offline:
            errors.append(f"{entry['path']}: missing or stale and --offline was given")
            continue
        tmp = p.with_name(p.name + f".part.{os.getpid()}")
        p.parent.mkdir(parents=True, exist_ok=True)
        gcloud.cp(f"{base}/{entry['path']}", str(tmp))
        actual = sha256_file(tmp)
        if actual != entry["sha256"]:
            tmp.unlink(missing_ok=True)
            errors.append(f"{entry['path']}: sha256 expected {entry['sha256']} actual {actual}")
            continue
        os.replace(tmp, p)
        fetched += 1

    listed = {entry["path"] for entry in manifest["files"]}
    extra_psv: list[str] = []
    for product in PRODUCTS:
        d = dest / product
        if not d.is_dir():
            continue
        suffix = PRODUCT_EXT[product]
        for p in sorted(d.iterdir()):
            # Snapshot-shaped files directly in the product dir only: a tracked
            # README.md, a dotfile, or a subdirectory is ignored outright.
            if not p.is_file() or not p.name.endswith(suffix):
                continue
            rel = p.relative_to(dest).as_posix()
            if rel in listed:
                continue
            if prune:
                p.unlink()
                pruned += 1
            else:
                print(f"extra: {rel}", file=sys.stderr)
                if suffix == ".psv":
                    extra_psv.append(rel)

    print(f"{sid}: kept {kept}, fetched {fetched}, pruned {pruned}")
    if extra_psv:
        errors += [f"{rel}: unlisted *.psv in a product directory (runners glob these) — prune or remint" for rel in extra_psv]
    if errors:
        raise ContentError(errors)
    return 0


def cmd_fetch(args: argparse.Namespace) -> int:
    return fetch(
        Path(args.pin),
        Path(args.dest),
        args.prune,
        args.offline,
        Gcloud(),
        args.allow_partial,
    )


# ── cli ──────────────────────────────────────────────────────────────────────


def build_parser() -> argparse.ArgumentParser:
    ap = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    sub = ap.add_subparsers(dest="cmd", required=True)

    m = sub.add_parser("manifest", help="build MANIFEST.json from a snapshot tree")
    m.add_argument("--tree", required=True)
    m.add_argument("--provenance", required=True)
    m.add_argument("--partial", action="store_true")
    m.add_argument("--out", default=None)
    m.set_defaults(func=cmd_manifest)

    s = sub.add_parser("summary", help="human table of a snapshot tree")
    s.add_argument("--tree", required=True)
    s.set_defaults(func=cmd_summary)

    d = sub.add_parser("diff", help="per-product diff of two manifests")
    d.add_argument("old")
    d.add_argument("new")
    d.set_defaults(func=cmd_diff)

    u = sub.add_parser("upload", help="upload a tree to its content-addressed prefix")
    u.add_argument("--tree", required=True)
    u.add_argument("--bucket", default=DEFAULT_BUCKET)
    u.add_argument("--yes", action="store_true")
    u.set_defaults(func=cmd_upload)

    pn = sub.add_parser("pin", help="write the tracked pin from a manifest")
    pn.add_argument("--manifest", required=True)
    pn.add_argument("--out", required=True)
    pn.add_argument("--minted-by", required=True)
    pn.add_argument("--bucket", default=DEFAULT_BUCKET)
    pn.add_argument("--allow-partial", action="store_true")
    pn.set_defaults(func=cmd_pin)

    fe = sub.add_parser("fetch", help="materialize + verify every pinned file")
    fe.add_argument("--pin", required=True)
    fe.add_argument("--dest", required=True)
    fe.add_argument("--prune", action="store_true")
    fe.add_argument("--offline", action="store_true")
    fe.add_argument("--allow-partial", action="store_true")
    fe.set_defaults(func=cmd_fetch)

    return ap


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        return args.func(args)
    except ContentError as e:
        for line in e.messages:
            print(f"ERROR: {line}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
