"""Write runners/adamcore/.venv/adamcore-versions.json.

Provenance stamp for the adam-core channels: the resolved package versions and
the exact git commits actually installed (read from each distribution's
dist-info/direct_url.json, so the stamp reflects what pip/uv built, not what the
pin string claimed), the EFCC18 bias-table SHA-256, and the ASSIST/REBOUND
versions behind adam-assist. The proprietary B612 data package is reported by
VERSION STRING ONLY — never its contents.

Run by setup.sh under the venv interpreter, with ADAM_CORE_EFCC18_DIR,
ADAMCORE_REV, ADAMASSIST_REV, B612_TAG and VENV in the environment.
"""

from __future__ import annotations

import hashlib
import json
import os
from importlib import metadata
from pathlib import Path


def _dist_commit(dist_name: str) -> str | None:
    """The git commit a distribution was installed from (PEP 610 direct_url.json)."""
    try:
        dist = metadata.distribution(dist_name)
    except metadata.PackageNotFoundError:
        return None
    try:
        raw = dist.read_text("direct_url.json")
    except Exception:  # noqa: BLE001
        raw = None
    if not raw:
        return None
    try:
        info = json.loads(raw)
        return info.get("vcs_info", {}).get("commit_id")
    except Exception:  # noqa: BLE001
        return None


def _version(dist_name: str) -> str | None:
    try:
        return metadata.version(dist_name)
    except metadata.PackageNotFoundError:
        return None


def _module_version(module_name: str) -> str | None:
    try:
        mod = __import__(module_name)
        return getattr(mod, "__version__", None)
    except Exception:  # noqa: BLE001
        return None


def _native_crate_versions(adamassist_rev: str | None) -> dict:
    """libassist-sys / librebound-sys versions from adam-assist's Cargo.lock at the
    pinned commit (ASSIST/REBOUND are vendored as these Rust crates). Read-only fetch;
    on any failure returns {'status': 'not exposed (<reason>)'}."""
    import re
    import urllib.request

    if not adamassist_rev:
        return {"status": "not exposed (no ADAMASSIST_REV)"}
    url = (
        f"https://raw.githubusercontent.com/B612-Asteroid-Institute/adam-assist/"
        f"{adamassist_rev}/rust/adam_assist_rs/Cargo.lock"
    )
    try:
        with urllib.request.urlopen(url, timeout=20) as r:
            txt = r.read().decode(errors="replace")
    except Exception as e:  # noqa: BLE001
        return {"status": f"not exposed (Cargo.lock fetch failed: {e})"}
    out: dict = {"source": "adam-assist rust/adam_assist_rs/Cargo.lock @ " + adamassist_rev}
    for crate in ("libassist-sys", "librebound-sys"):
        m = re.search(rf'name = "{re.escape(crate)}"\nversion = "([^"]*)"', txt)
        out[crate] = m.group(1) if m else "not found"
    return out


def _efcc18_sha() -> dict:
    """SHA-256 of every file under ADAM_CORE_EFCC18_DIR (sorted, relative paths)."""
    d = Path(os.environ.get("ADAM_CORE_EFCC18_DIR", ""))
    out: dict[str, str] = {}
    if not d.is_dir():
        return {"dir": str(d), "status": "absent", "files": {}}
    for f in sorted(d.rglob("*")):
        if f.is_file():
            h = hashlib.sha256(f.read_bytes()).hexdigest()
            out[str(f.relative_to(d))] = h
    return {"dir": str(d), "status": "present" if out else "empty", "files": out}


def main() -> int:
    venv = Path(os.environ["VENV"])

    # ASSIST / REBOUND provenance: adam-assist @ce52ce1e exposes NO Python
    # ASSIST/REBOUND version (it vendors them natively through the Rust crates
    # libassist-sys / librebound-sys), so these stay null. Instead record those
    # crate versions from adam-assist's Cargo.lock at the pinned commit, fetched
    # read-only from GitHub.
    assist_v = _version("assist") or _module_version("assist")
    rebound_v = _version("rebound") or _module_version("rebound")
    assist_native = _native_crate_versions(os.environ.get("ADAMASSIST_REV"))

    b612_version = _version("adam-observatory-uncertainties")

    stamp = {
        "generated": __import__("datetime").datetime.now(__import__("datetime").timezone.utc).isoformat(),
        "adam_core": {
            "version": _version("adam-core"),
            "commit": _dist_commit("adam-core"),
            "pin_requested": os.environ.get("ADAMCORE_REV"),
        },
        "adam_assist": {
            "version": _version("adam-assist"),
            "commit": _dist_commit("adam-assist"),
            "pin_requested": os.environ.get("ADAMASSIST_REV"),
        },
        "b612_data_package": {
            "name": "adam-observatory-uncertainties",
            "version": b612_version if b612_version else "absent",
            "tag_requested": os.environ.get("B612_TAG"),
            "note": "PROPRIETARY; version string only, contents never recorded",
        },
        "assist": assist_v,
        "rebound": rebound_v,
        "assist_native_crates": assist_native,
        "efcc18": _efcc18_sha(),
        "python": __import__("platform").python_version(),
    }

    out = venv / "adamcore-versions.json"
    out.write_text(json.dumps(stamp, indent=2) + "\n")
    print(json.dumps(stamp, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
