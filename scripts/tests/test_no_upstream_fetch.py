"""No validation job reaches JPL, NAIF or the MPC.

Owner ruling (bead empyrean-qf03y): those hosts publish the products the suite
reads, and every one of those products comes into a validation run from our own
buckets (the inputs snapshot, the registered kernel set, the third-party tool
set), minted by the manual mint-inputs run, which is the only workflow allowed
to reach them. These tests read the files that make up a run (no YAML
dependency) and fail on each way to break that:

  * a JPL (ssd.jpl.nasa.gov, ssd-api.jpl.nasa.gov), NAIF (naif.jpl.nasa.gov) or
    MPC (*minorplanetcenter.net) URL in the workflows, the actions, the
    Makefile, the scripts or the runner setup files, outside mint-inputs.yml,
    full-line comments (provenance) and the named exemptions below;
  * an engine job that does not seal those hosts before its first setup step;
  * an external leg that seals late without being on the allowlist, or a
    change to the allowlist that the workflow and this file do not both make.

The allowlist names the external tools whose setup or import still fetches
from one of these hosts. It can only shrink: CEILING is the list as it stood
when the rule was written, and a tool outside it fails here.
"""

import json
import pathlib
import re

ROOT = pathlib.Path(__file__).resolve().parents[2]
WF = ROOT / ".github" / "workflows"
ACTIONS = ROOT / ".github" / "actions"
VALIDATION = (WF / "validation.yml").read_text()
SEAL = ROOT / "scripts" / "seal-upstream.sh"

# The external tools that still fetch from JPL, NAIF or the MPC, with what they
# fetch. Each is sealed after its setup instead of before it.
ALLOWLIST = {
    "grss": "setup fetches the debiasing tables from ssd.jpl.nasa.gov and the observatory codes from "
    "data.minorplanetcenter.net (grss's own get_debiasing_data.py and _download_codes_file); no "
    "third-party tool-set folder carries them yet",
}
# The list may lose tools, never gain one.
CEILING = frozenset({"grss"})

HOSTS = ("ssd.jpl.nasa.gov", "ssd-api.jpl.nasa.gov", "naif.jpl.nasa.gov", "minorplanetcenter.net")
# A URL: a scheme, or a host followed by a path. A host named in prose ("reads
# them from ssd.jpl.nasa.gov") is neither and is not a fetch.
URL_RE = re.compile(
    r"(?:(?:https?|ftp)://(?:[\w-]+\.)*(?:" + "|".join(re.escape(h) for h in HOSTS) + r"))"
    r"|(?:(?<![\w.-])(?:[\w-]+\.)*(?:" + "|".join(re.escape(h) for h in HOSTS) + r")/)"
)

ENGINE_JOBS = ["prep", "reference", "reference-heavy", "assemble", "assemble-final", "empyrean", "empyrean-heavy", "reduce"]


def scanned_files():
    files = [p for p in WF.glob("*.yml") if p.name != "mint-inputs.yml"]
    files += sorted(ACTIONS.rglob("*.yml"))
    files.append(ROOT / "Makefile")
    files += sorted(p for p in (ROOT / "scripts").glob("*") if p.suffix in (".py", ".sh"))
    files += sorted((ROOT / "runners").glob("*/setup.sh"))
    files += sorted((ROOT / "runners").glob("*/stage_data.py"))
    files += sorted((ROOT / "runners").glob("*/*_offline.py"))
    return files


def makefile_mint_recipe_lines(text):
    """Line numbers of the `mint-inputs` recipe: the local twin of the manual
    mint workflow, run by hand and never by a validation job."""
    lines = text.splitlines()
    start = next(i for i, ln in enumerate(lines) if ln.startswith("mint-inputs:"))
    out = set()
    for i in range(start + 1, len(lines)):
        if not lines[i].startswith("\t"):
            break
        out.add(i)
    return out


def upstream_urls(path):
    text = path.read_text()
    exempt = makefile_mint_recipe_lines(text) if path.name == "Makefile" else set()
    hits = []
    for i, ln in enumerate(text.splitlines()):
        if ln.lstrip().startswith("#") or i in exempt:
            continue
        for m in URL_RE.finditer(ln):
            hits.append(f"{path.relative_to(ROOT)}:{i + 1}: {m.group(0)}")
    return hits


def jobs(text):
    body = text.split("\njobs:\n", 1)[1]
    parts = re.split(r"^  ([A-Za-z0-9_-]+):\n", body, flags=re.MULTILINE)
    return {parts[i]: parts[i + 1] for i in range(1, len(parts), 2)}


def steps(job_text):
    """The job's steps in order, each as its text."""
    body = job_text.split("\n    steps:\n", 1)[1]
    return [s for s in re.split(r"^      - ", body, flags=re.MULTILINE)[1:]]


def is_checkout(step):
    return "uses: actions/checkout@" in step


def is_seal(step):
    return "./scripts/seal-upstream.sh" in step


def test_the_url_scan_catches_a_fetch_and_passes_prose():
    for bad in (
        "curl -o x https://ssd.jpl.nasa.gov/ftp/eph/planets/x.440",
        'NAIF="https://naif.jpl.nasa.gov/pub/naif/generic_kernels"',
        "wget naif.jpl.nasa.gov/pub/naif/x.bsp",
        "requests.get('https://data.minorplanetcenter.net/api/obscodes')",
        "https://ssd-api.jpl.nasa.gov/sbdb.api",
    ):
        assert URL_RE.search(bad), bad
    for ok in ("staged from ssd.jpl.nasa.gov's names", "storage.googleapis.com/x", "HOSTS=(naif.jpl.nasa.gov)"):
        assert not URL_RE.search(ok), ok


def test_no_run_file_names_a_jpl_naif_or_mpc_url():
    hits = [h for p in scanned_files() for h in upstream_urls(p)]
    assert not hits, "a validation run would reach JPL, NAIF or the MPC:\n  " + "\n  ".join(hits)


def test_the_scan_covers_the_files_that_used_to_fetch():
    names = {str(p.relative_to(ROOT)) for p in scanned_files()}
    for needed in (
        ".github/workflows/validation.yml",
        ".github/actions/engine-setup/action.yml",
        "Makefile",
        "scripts/seal-upstream.sh",
        "runners/assist/setup.sh",
        "runners/grss/setup.sh",
        "runners/findorb/setup.sh",
    ):
        assert needed in names, needed
    assert ".github/workflows/mint-inputs.yml" not in names


def test_the_allowlist_only_shrinks_and_the_workflow_agrees():
    assert set(ALLOWLIST) <= CEILING, f"the upstream-fetch allowlist grew: {sorted(set(ALLOWLIST) - CEILING)}"
    m = re.search(r"^  UPSTREAM_FETCH_ALLOWLIST: '(\[.*\])'$", VALIDATION, re.MULTILINE)
    assert m, "the workflow env names no UPSTREAM_FETCH_ALLOWLIST"
    assert sorted(json.loads(m.group(1))) == sorted(ALLOWLIST), "the workflow's allowlist and this file's disagree"


def test_every_engine_job_seals_all_six_hosts_before_any_setup():
    all_jobs = jobs(VALIDATION)
    for name in ENGINE_JOBS:
        st = steps(all_jobs[name])
        i = next(i for i, s in enumerate(st) if is_checkout(s))
        for before in st[:i]:
            run = before.split("run:", 1)[-1] if "run:" in before else "uses"
            cmds = [ln.strip() for ln in run.splitlines() if ln.strip() and ln.strip() != "|" and not ln.strip().startswith("#")]
            assert cmds and all(c.startswith("echo ") for c in cmds), f"{name}: only logging may precede the checkout and the seal"
        assert is_seal(st[i + 1]), f"{name}: the step after the checkout must be the seal"
        assert re.search(r"run: \./scripts/seal-upstream\.sh\s*$", st[i + 1], re.MULTILINE), f"{name}: an engine job seals all six hosts"
        assert sum(is_seal(s) for s in st) == 1, f"{name}: one seal, before setup"


def test_external_legs_seal_before_setup_unless_allowlisted():
    st = steps(jobs(VALIDATION)["external"])
    assert is_checkout(st[0])
    early, late = st[1], [s for s in st if is_seal(s)]
    assert is_seal(early), "an external leg seals right after its checkout"
    assert "if: ${{ !contains(fromJSON(env.UPSTREAM_FETCH_ALLOWLIST), matrix.tool) }}" in early
    assert re.search(r"run: \./scripts/seal-upstream\.sh\s*$", early, re.MULTILINE), "the early seal covers all six hosts"
    assert len(late) == 2
    late = late[1]
    assert "if: ${{ contains(fromJSON(env.UPSTREAM_FETCH_ALLOWLIST), matrix.tool) }}" in late
    names = [s.split("\n", 1)[0] for s in st]
    i_setup = next(i for i, n in enumerate(names) if n.startswith("name: Setup ${{ matrix.tool }}"))
    i_run = next(i for i, n in enumerate(names) if n.startswith("name: Run ${{ matrix.tool }}"))
    i_late = st.index(late)
    assert i_setup < i_late < i_run, "the allowlisted tools seal after setup and before the run"
    assert re.search(r"run: \./scripts/seal-upstream\.sh\s*$", late, re.MULTILINE), "the late seal covers all six hosts too"
    assert "--allow-naif" not in VALIDATION, "no leg leaves NAIF open"


def test_the_seal_script_always_covers_all_six_hosts():
    text = SEAL.read_text()
    hosts = re.search(r"HOSTS=\(\n(.*?)\n\)", text, re.DOTALL).group(1).split()
    assert sorted(hosts) == sorted(
        ["ssd.jpl.nasa.gov", "ssd-api.jpl.nasa.gov", "naif.jpl.nasa.gov", "data.minorplanetcenter.net", "minorplanetcenter.net", "www.minorplanetcenter.net"]
    )
    assert "HOSTS+=" not in text and "--allow-naif" not in text, "the seal has no way to leave a host open"


def test_find_orb_assist_and_grss_read_the_third_party_set():
    setup = (ACTIONS / "engine-setup" / "action.yml").read_text()
    assert "linux_p1550p2650.440" not in "\n".join(ln for ln in setup.splitlines() if not ln.lstrip().startswith("#"))
    make = (ROOT / "Makefile").read_text()
    assert "ASSIST_DATA_DIR ?= $(DATA_DIR)/third-party/assist" in make
    assert "FINDORB_DATA_DIR ?= $(DATA_DIR)/third-party/findorb" in make
    assert "GRSS_DATA_DIR ?= $(DATA_DIR)/third-party/grss" in make
    for tool in ("assist", "grss"):
        text = (ROOT / "runners" / tool / "setup.sh").read_text()
        code = "\n".join(ln for ln in text.splitlines() if not ln.lstrip().startswith("#"))
        assert "curl" not in code and "wget" not in code, f"{tool}'s setup downloads"
    restore = re.search(r"- name: Restore the third-party tool set\n\s+if: (.*)\n", VALIDATION.split("\n  external:\n", 1)[1]).group(1)
    for tool in ("layup", "assist", "findorb", "grss"):
        assert f'"{tool}"' in restore, f"the {tool} leg does not restore the third-party set"


def test_the_grss_setup_never_imports_grss_with_its_shell_outs_live():
    """`import grss` runs grss.utils.initialize(), which shells out to get_kernels.py
    and pulls the full kernel set from NAIF and the SSD (and replaces a day-old
    earth_latest.bpc with NAIF's). The setup finds the package with find_spec, and
    the one import it keeps (the observatory-codes step) neuters os.system first."""
    text = (ROOT / "runners" / "grss" / "setup.sh").read_text()
    code = [ln for ln in text.splitlines() if not ln.lstrip().startswith("#")]
    assert any("importlib.util.find_spec(\"grss\")" in ln for ln in code), "the setup locates grss without importing it"
    for i, ln in enumerate(code):
        if re.search(r"\bimport grss\b|from grss\b|grss\.utils as", ln):
            guarded = any("os.system = lambda" in prev for prev in code[max(0, i - 3) : i])
            assert guarded, f"runners/grss/setup.sh imports grss with its shell-outs live: {ln.strip()}"
