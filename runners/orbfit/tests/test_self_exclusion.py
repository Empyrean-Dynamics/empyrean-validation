"""run_orbfit: a body the plan excludes from an object's forces is left out by neofit2.x's
own self-perturbation check (the object runs under that body's AST17 name), confirmed from
neofit2.x's output; two excluded AST17 bodies make a named unsupported row, never run."""

import importlib.util
import pathlib
import subprocess

import pytest

RUNNER = pathlib.Path(__file__).resolve().parents[1] / "run_orbfit.py"

# The image's tables at the pinned digest: the option files' massive-asteroid lines and
# ast_files/AST17.bai (count, header, then `<mass> <name>`).
NOP = "propag.\n\t.iast=17\t! 0=no asteroids with mass n=no. of massive asteroids\n\t.filbe='AST17'\n!\t.dmast=0.02d0\n"
AST17_BAI = "   17\n -21139.00000 416811.00000     50.00000\n" + "".join(
    f"  1.0E-11 {n}\n" for n in (1, 2, 3, 4, 6, 7, 10, 15, 16, 29, 52, 65, 87, 88, 511, 704, 134340)
)
MASSIVE = ["1", "2", "3", "4", "6", "7", "10", "15", "16", "29", "52", "65", "87", "88", "511", "704", "134340"]


@pytest.fixture(scope="module")
def runner():
    spec = importlib.util.spec_from_file_location("run_orbfit_exclusion", RUNNER)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def od(obj, excluded=None):
    row = {"object": obj, "test_type": "orbit_determination", "propagation_uncertainty": None}
    if excluded is not None:
        row["excluded_perturbers_naif"] = excluded
    return row


def test_the_massive_asteroids_are_the_tables_first_iast_names(runner):
    assert runner.parse_massive_asteroids([NOP, NOP], {"AST17": AST17_BAI}) == MASSIVE
    fewer = NOP.replace(".iast=17", ".iast=3")
    assert runner.parse_massive_asteroids([fewer, fewer], {"AST17": AST17_BAI}) == ["1", "2", "3"]


def test_option_files_that_disagree_or_a_short_table_are_refused(runner):
    with pytest.raises(ValueError, match="disagree"):
        runner.parse_massive_asteroids([NOP, NOP.replace(".iast=17", ".iast=16")], {"AST17": AST17_BAI})
    short = "\n".join(AST17_BAI.splitlines()[:10])
    with pytest.raises(ValueError, match="fewer than"):
        runner.parse_massive_asteroids([NOP, NOP], {"AST17": short})


def test_an_excluded_massive_asteroid_runs_the_object_under_its_name(runner):
    assert runner.orbfit_exclusion(od("Vesta", [2000004]), MASSIVE) == ("4", None)
    assert runner.orbfit_exclusion(od("Pluto", [2134340]), MASSIVE) == ("134340", None)


def test_the_decision_reads_the_row_never_the_designation(runner):
    # Named like a massive asteroid but excluding nothing: nothing is left out.
    assert runner.orbfit_exclusion(od("Vesta"), MASSIVE) == (None, None)
    assert runner.orbfit_exclusion(od("4", []), MASSIVE) == (None, None)
    # Any object whose row excludes Vesta runs under Vesta's name.
    assert runner.orbfit_exclusion(od("Achilles", [2000004]), MASSIVE) == ("4", None)


def test_an_excluded_body_outside_the_table_needs_nothing(runner):
    # (5) Astraea and (588) Achilles are not among the seventeen; 4 is no NAIF asteroid id.
    assert runner.orbfit_exclusion(od("Astraea", [2000005]), MASSIVE) == (None, None)
    assert runner.orbfit_exclusion(od("X", [2000588, 4, 399]), MASSIVE) == (None, None)


def test_two_excluded_massive_asteroids_are_named_unsupported(runner):
    name, why = runner.orbfit_exclusion(od("X", [2000004, 2000001, 2000005]), MASSIVE)
    assert name is None
    assert "1, 4" in why and "self-perturbation" in why


def test_an_unsupported_row_is_named_and_never_run(runner, tmp_path, monkeypatch):
    def never(*a, **k):
        raise AssertionError("an unsupported row must not reach neofit2.x")

    monkeypatch.setattr(runner, "run_neofit_container", never)
    out = runner.process_od_row(od("X", [2000004, 2000002]), {}, tmp_path, image="img", platform="p",
                                timeout_s=1.0, source_version="orbfit abc123", sun=None, massive=MASSIVE)
    assert out["orbfit_unsupported"] and "orbfit_error" not in out
    assert out["orbfit_version"] == "orbfit abc123" and out["orbfit_settings"] is runner.ORBFIT_SETTINGS


class Sun:
    def state(self, jd):
        return [0.0, 0.0, 0.0], [0.0, 0.0, 0.0]


FIT = {"rms_arcsec": 0.8, "n_obs_used": 3559, "n_obs_rejected": 150, "n_obs_total": 3709,
       "orbit_pos_au": [1.0, 0.0, 0.0], "orbit_vel_au_d": [0.0, 0.01, 0.0], "epoch_mjd_tdb": 51725.3,
       "covariance_6x6_ecl": None, "nongrav_a": None, "error_model": "gaiaDR2_mix"}


@pytest.mark.parametrize("excluded,desig,left_out", [([2000004], "4", "4"), (None, "Vesta", None)])
def test_the_object_runs_under_the_excluded_bodys_name(runner, tmp_path, monkeypatch, excluded, desig,
                                                       left_out):
    (tmp_path / "Vesta.psv").write_text("permID|obsTime\n")
    ic_row = {"object": "Vesta", "ic_pos_au": [2.0, 0.0, 0.0], "ic_vel_au_d": [0.0, 0.01, 0.0],
              "epoch_mjd_tdb": 51202.0}
    seen = {}

    def fake(**kw):
        seen.update(kw)
        return dict(FIT), None

    monkeypatch.setattr(runner, "run_neofit_container", fake)
    out = runner.process_od_row(od("Vesta", excluded), {"Vesta": [ic_row]}, tmp_path, image="img",
                                platform="p", timeout_s=1.0, source_version="v", sun=Sun(),
                                massive=MASSIVE)
    assert seen["desig"] == desig and seen["leave_out"] == left_out
    assert seen["seed_eq0"].splitlines()[4] == desig  # the seed names the object as neofit2.x reads it
    assert out.get("orbfit_left_out_naif") == ([2000004] if left_out else None)


def run_with_stdout(runner, monkeypatch, stdout, leave_out):
    def fake_run(cmd, **kw):
        return subprocess.CompletedProcess(cmd, 0, stdout=stdout, stderr="")

    monkeypatch.setattr(runner.subprocess, "run", fake_run)
    return runner.run_neofit_container("4", "", "", False, "img", "linux/amd64", 1.0, leave_out=leave_out)


def test_a_left_out_body_must_be_reported_by_neofit2(runner, monkeypatch):
    res, err = run_with_stdout(runner, monkeypatch, " Name (Ctrl-D to quit)?\n", "4")
    assert res is None and "did not report leaving out massive asteroid 4" in err
    # Reported (neofit2.x's own line): the check passes and the output files decide.
    res, err = run_with_stdout(runner, monkeypatch, "  self perturbation of 4 avoided\n", "4")
    assert res is None and "no post-fit orbit" in err
    # Nothing to leave out: no line is required.
    res, err = run_with_stdout(runner, monkeypatch, "", None)
    assert "no post-fit orbit" in err
