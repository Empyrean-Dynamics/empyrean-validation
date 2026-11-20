"""prebuilt.py and the Makefile's PREBUILT_BIN=1 mode: build once, assert everywhere.

The second half drives the real Makefile in a scratch workspace with a `cargo`
that records and fails when called, so a passing test proves PREBUILT_BIN=1
neither compiled nor fell back to compiling.
"""

import os
import pathlib
import shutil
import stat
import subprocess
import sys

import pytest

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
import prebuilt as pb  # noqa: E402

PINNED = "0.11.0-rc.1"


def make_workspace(base, built=True):
    """<base>/empyrean-validation (+ sibling core and engine dirs); with `built`,
    every component exists with distinct bytes."""
    val = base / "empyrean-validation"
    (val / "runners" / "rust").mkdir(parents=True)
    (val / "runners" / "rust" / "Cargo.toml").write_text(f'[dependencies]\nempyrean = "={PINNED}"\n')
    (base / "empyrean-core").mkdir()
    (base / "empyrean").mkdir()
    anc = pb.anchors(val)
    if built:
        for name, (anchor, rel, _exe) in pb.components().items():
            path = anc[anchor] / rel.replace("*", "1.0")
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(f"bytes of {name}\n".encode())
    return val


def test_pack_then_unpack_places_every_component_with_hash_and_exec_bit(tmp_path):
    src = make_workspace(tmp_path / "a")
    out = tmp_path / "staged"
    m = pb.pack(out, PINNED, root=src)
    assert set(m["components"]) == set(pb.components())
    assert m["engine_version"] == PINNED

    dst = make_workspace(tmp_path / "b", built=False)
    names = pb.unpack(out, None, root=dst)
    assert sorted(names) == sorted(pb.components())
    for name, c in m["components"].items():
        placed = pb.anchors(dst)[c["anchor"]] / c["path"]
        assert placed.read_bytes() == f"bytes of {name}\n".encode()
        assert os.access(placed, os.X_OK) == c["executable"], name
    assert pb.verify(out / pb.MANIFEST, None, PINNED, root=dst) == []


def test_pack_refuses_a_missing_component_by_name(tmp_path):
    src = make_workspace(tmp_path / "a")
    (src.parent / "empyrean-core" / "target" / "release" / "validate-core").unlink()
    with pytest.raises(pb.PrebuiltError) as exc:
        pb.pack(tmp_path / "staged", PINNED, root=src)
    assert "core-runner" in str(exc.value) and "was not built" in str(exc.value)


def test_unpack_refuses_a_tampered_staged_file_and_an_unknown_component(tmp_path):
    src = make_workspace(tmp_path / "a")
    out = tmp_path / "staged"
    pb.pack(out, PINNED, root=src)
    (out / pb.FILES / "rust-runner").write_bytes(b"tampered")
    dst = make_workspace(tmp_path / "b", built=False)
    with pytest.raises(pb.PrebuiltError, match="component `rust-runner`: staged file hashes to"):
        pb.unpack(out, ["rust-runner"], root=dst)
    assert not (dst / "runners/rust/target/release/validate").exists(), "a refused file must not be placed"
    with pytest.raises(pb.PrebuiltError, match="no component"):
        pb.unpack(out, ["nonesuch"], root=dst)


def test_verify_names_a_missing_an_altered_a_non_executable_binary_and_a_version_mismatch(tmp_path):
    src = make_workspace(tmp_path / "a")
    out = tmp_path / "staged"
    pb.pack(out, PINNED, root=src)
    dst = make_workspace(tmp_path / "b", built=False)
    pb.unpack(out, None, root=dst)
    man = out / pb.MANIFEST

    placed = dst / "runners/rust/target/release/validate"
    placed.write_bytes(b"rebuilt into something else")
    assert any("rust-runner" in p and "no longer matches" in p for p in pb.verify(man, ["rust-runner"], None, root=dst))
    placed.unlink()
    assert any("is missing" in p and "never builds" in p for p in pb.verify(man, ["rust-runner"], None, root=dst))
    cli = dst / "runners/cli/target/release/empyrean-cli-runner"
    cli.chmod(cli.stat().st_mode & ~(stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH))
    assert any("not executable" in p for p in pb.verify(man, ["cli-runner"], None, root=dst))
    assert any("built against empyrean 0.11.0-rc.1" in p for p in pb.verify(man, ["harness"], "0.99.0", root=dst))
    assert any("not in" in p for p in pb.verify(man, ["nonesuch"], None, root=dst))


def test_the_cli_exit_codes(tmp_path, capsys):
    src = make_workspace(tmp_path / "a")
    out = tmp_path / "staged"
    pb.pack(out, PINNED, root=src)
    assert pb.main(["verify", "--manifest", str(out / pb.MANIFEST), "--component", "harness"]) == 1  # not in this repo
    assert "PREBUILT_BIN=1" in capsys.readouterr().err
    assert pb.main(["unpack", "--from", str(tmp_path / "nowhere")]) == 1
    assert "no prebuilt manifest" in capsys.readouterr().err


# ── the Makefile mode ─────────────────────────────────────────────────────────


@pytest.fixture
def leg(tmp_path):
    """A scratch workspace holding the real Makefile and scripts, with the prebuilt
    artifact already unpacked, and a `cargo` that records any call and fails."""
    built = make_workspace(tmp_path / "prep")
    staged = tmp_path / "staged"
    pb.pack(staged, PINNED, root=built)
    val = tmp_path / "leg" / "empyrean-validation"
    (val / "runners" / "rust").mkdir(parents=True)
    (val / "scripts").mkdir()
    shutil.copy(ROOT / "Makefile", val / "Makefile")
    shutil.copy(ROOT / "scripts" / "prebuilt.py", val / "scripts" / "prebuilt.py")
    (val / "runners" / "rust" / "Cargo.toml").write_text(f'[dependencies]\nempyrean = "={PINNED}"\n')
    (tmp_path / "leg" / "empyrean-core").mkdir()
    shutil.copytree(staged, val / "prebuilt")
    pb.unpack(val / "prebuilt", None, root=val)
    bindir = tmp_path / "bin"
    bindir.mkdir()
    cargo_log = tmp_path / "cargo.log"
    stub = bindir / "cargo"
    stub.write_text(f'#!/bin/sh\necho "$@" >> {cargo_log}\nexit 97\n')
    stub.chmod(0o755)
    env = dict(os.environ, PATH=f"{bindir}{os.pathsep}{os.environ['PATH']}")
    return val, env, cargo_log


def run_make(leg, *targets, extra=()):
    val, env, _ = leg
    return subprocess.run(
        ["make", "-C", str(val), "--no-print-directory", *targets, "PREBUILT_BIN=1", *extra],
        capture_output=True,
        text=True,
        env=env,
    )


def test_make_prebuilt_mode_accepts_the_unpacked_binaries_and_never_compiles(leg):
    r = run_make(leg, "build-reference", "build-c", "build-cli", "build-core", "build-empyrean-validation")
    assert r.returncode == 0, r.stdout + r.stderr
    assert "engine version OK: 0.11.0-rc.1" in r.stdout
    assert "verified rust-runner" in r.stdout and "verified harness" in r.stdout and "verified core-runner" in r.stdout
    assert "Building" not in r.stdout
    assert not leg[2].exists(), "cargo was invoked in PREBUILT_BIN=1 mode"


@pytest.mark.parametrize(
    "target,victim",
    [
        ("build-rust", "runners/rust/target/release/validate"),
        ("build-cli", "runners/cli/target/release/empyrean-cli-runner"),
        ("build-c", "runners/c/runner"),
        ("build-empyrean-validation", "target/release/empyrean-validation"),
    ],
)
def test_make_prebuilt_mode_fails_loudly_on_a_missing_binary_instead_of_rebuilding(leg, target, victim):
    (leg[0] / victim).unlink()
    r = run_make(leg, target)
    assert r.returncode != 0
    assert "PREBUILT_BIN=1" in r.stderr and "is missing" in r.stderr and victim in r.stderr
    assert not leg[2].exists(), "a missing binary was silently rebuilt"


def test_make_prebuilt_mode_fails_on_a_mismatched_binary_and_a_core_that_is_absent(leg):
    (leg[0] / "runners/rust/target/release/validate").write_bytes(b"not what prep built")
    r = run_make(leg, "build-rust")
    assert r.returncode != 0 and "no longer matches the recorded sha256" in r.stderr
    core = leg[0].parent / "empyrean-core" / "target" / "release" / "validate-core"
    core.unlink()
    r = run_make(leg, "build-core")
    assert r.returncode != 0 and "core-runner" in r.stderr and not leg[2].exists()


def test_make_prebuilt_mode_refuses_binaries_built_against_another_engine(leg):
    cargo = leg[0] / "runners" / "rust" / "Cargo.toml"
    cargo.write_text('[dependencies]\nempyrean = "=0.12.0"\n')
    r = run_make(leg, "build-reference")
    assert r.returncode != 0
    assert "built against empyrean 0.11.0-rc.1" in r.stderr and "pin 0.12.0" in r.stderr


def test_make_without_the_artifact_names_the_missing_manifest(leg):
    shutil.rmtree(leg[0] / "prebuilt")
    r = run_make(leg, "build-empyrean-validation")
    assert r.returncode != 0 and "no prebuilt manifest" in r.stderr


def test_with_core_is_detected_from_the_prebuilt_runner_when_there_is_no_core_checkout(leg):
    val = leg[0]
    r = subprocess.run(
        ["make", "-C", str(val), "-pn", "PREBUILT_BIN=1", "--no-print-directory", "build-core"],
        capture_output=True, text=True, env=leg[1],
    )
    assert "WITH_CORE := 1" in r.stdout
    r = subprocess.run(
        ["make", "-C", str(val), "-pn", "--no-print-directory", "build-core"],
        capture_output=True, text=True, env=leg[1],
    )
    assert "WITH_CORE := \n" in r.stdout or "WITH_CORE :=\n" in r.stdout
