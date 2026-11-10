"""validate_external.py: a malformed or empty comparator file fails its own leg.

One malformed file and one empty file per comparator class, each proven to be
refused (the test fails if the validator accepts them), plus the agreement with
the Rust merge's own shape check when the harness binary is built.
"""

import json
import pathlib
import subprocess
import sys

import pytest

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
import validate_external as ve  # noqa: E402

GOOD = json.dumps([{"object": "Eros", "test_type": "orbit_determination", "x": 1.0}, {"object": "Bennu"}])
# The comparator classes the merge distinguishes: the propagation reference with
# a variational arm file, the two-pass OD tools (a radar pass beside the main
# one), the one-pass OD tools, and the timing-only tools.
TOOLS = ["assist", "findorb", "grss", "layup", "orbfit", "oorb", "kete", "jorbit"]
TWO_PASS = ["findorb", "grss"]
BAD_BODIES = {
    "empty": "",
    "whitespace": " \n\t\n",
    "truncated": '[{"object":"Eros","test_ty',
    "not_an_array": '{"object":"Eros"}',
    "record_without_object": '[{"object":"Eros"},{"test_type":"orbit_determination"}]',
    "record_not_an_object": "[1, 2]",
    "object_not_a_string": '[{"object": 7}]',
}


def write(d, name, body):
    p = d / name
    p.write_text(body)
    return p


@pytest.mark.parametrize("tool", TOOLS)
def test_a_good_file_passes(tool, tmp_path):
    write(tmp_path, f"validation_{tool}.json", GOOD)
    assert ve.validate(tool, tmp_path) == []
    assert ve.main(["--tool", tool, "--results-dir", str(tmp_path)]) == 0


@pytest.mark.parametrize("tool", TOOLS)
@pytest.mark.parametrize("kind", sorted(BAD_BODIES))
def test_a_bad_main_file_fails_the_leg_naming_the_tool_and_the_file(tool, kind, tmp_path, capsys):
    write(tmp_path, f"validation_{tool}.json", BAD_BODIES[kind])
    problems = ve.validate(tool, tmp_path)
    assert len(problems) == 1, f"{tool}/{kind} was accepted"
    assert f"validation_{tool}.json" in problems[0]
    if kind in ("empty", "whitespace"):
        assert "file is empty" in problems[0], "an empty file must be named as empty, not as a parse error"
    assert ve.main(["--tool", tool, "--results-dir", str(tmp_path)]) == 1
    err = capsys.readouterr().err
    assert f"external comparator `{tool}` produced an unusable file" in err
    assert "never reaches reduce" in err


@pytest.mark.parametrize("tool", TWO_PASS)
@pytest.mark.parametrize("kind", ["empty", "truncated"])
def test_a_bad_radar_pass_fails_the_leg_too(tool, kind, tmp_path):
    write(tmp_path, f"validation_{tool}.json", GOOD)
    write(tmp_path, f"validation_{tool}_radar.json", BAD_BODIES[kind])
    problems = ve.validate(tool, tmp_path)
    assert len(problems) == 1 and f"validation_{tool}_radar.json" in problems[0]


def test_a_missing_main_file_fails_and_a_missing_radar_pass_is_not_required(tmp_path):
    assert "validation_kete.json" in ve.validate("kete", tmp_path)[0]
    write(tmp_path, "validation_findorb.json", GOOD)
    assert ve.validate("findorb", tmp_path) == []


def test_an_empty_array_is_a_valid_tool_that_compared_nothing(tmp_path):
    write(tmp_path, "validation_layup.json", "[]")
    assert ve.validate("layup", tmp_path) == []


HARNESS = ROOT / "target" / "release" / "empyrean-validation"


@pytest.mark.skipif(not HARNESS.exists(), reason="harness binary not built")
@pytest.mark.parametrize("kind", sorted(BAD_BODIES))
def test_the_rust_merge_refuses_exactly_what_this_validator_refuses(kind, tmp_path):
    """The two shape checks are mirrors; this keeps them from drifting apart."""
    base = tmp_path / "base.json"
    base.write_text("[]")
    bad = write(tmp_path, "validation_layup.json", BAD_BODIES[kind])
    r = subprocess.run(
        [str(HARNESS), "merge-external", "-i", str(base), "-o", str(tmp_path / "m.json"), "--layup", str(bad)],
        capture_output=True,
        text=True,
    )
    assert r.returncode == 0, r.stderr
    assert "COMPARATOR FAILED --layup" in r.stderr, f"{kind}: the Rust merge accepted what the validator refuses"
    side = json.loads((tmp_path / "m_comparators.json").read_text())
    assert side[0]["status"] == "failed" and ve.validate("layup", tmp_path)
    good = write(tmp_path, "validation_layup.json", GOOD)
    r = subprocess.run(
        [str(HARNESS), "merge-external", "-i", str(base), "-o", str(tmp_path / "m2.json"), "--layup", str(good)],
        capture_output=True,
        text=True,
    )
    assert "COMPARATOR FAILED" not in r.stderr and ve.validate("layup", tmp_path) == []
