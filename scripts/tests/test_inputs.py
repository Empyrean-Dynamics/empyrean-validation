"""inputs.py: manifest/summary/diff/upload/pin/fetch, with a failing path per verb.

Writes (`upload`) go through a dir-backed fake gcloud (FakeGcloud). Reads
(`fetch`) go through a dir-backed fake HTTP reader (FakeHttp) that serves the
same directory over the public-URL mapping — no network, no credential. The
`--offline` path invokes neither.
"""

import importlib.util
import json
import pathlib
import shutil
import urllib.parse

import pytest

HERE = pathlib.Path(__file__).resolve()
SCRIPT = HERE.parents[1] / "inputs.py"
REPO = HERE.parents[2]
MAIN_FIXTURES = pathlib.Path("/Users/moeyensj/projects/empyrean/empyrean-validation/fixtures")


@pytest.fixture(scope="module")
def ip():
    spec = importlib.util.spec_from_file_location("inputs", SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


# ── a dir-backed fake bucket ─────────────────────────────────────────────────


class FakeGcloud:
    """gs://X/Y lives at <root>/X/Y. Mirrors the cp/ls semantics inputs.py uses."""

    def __init__(self, root):
        self.root = pathlib.Path(root)

    def _loc(self, uri):
        assert uri.startswith("gs://"), uri
        return self.root / uri[len("gs://"):]

    def ls(self, uri):
        prefix = uri.removesuffix("/**")
        base = self._loc(prefix.rstrip("/"))
        if not base.exists():
            return []
        if base.is_file():
            return ["gs://" + base.relative_to(self.root).as_posix()]
        return sorted(
            "gs://" + p.relative_to(self.root).as_posix()
            for p in base.rglob("*")
            if p.is_file()
        )

    def cp(self, src, dst, recursive=False):
        if src.startswith("gs://"):
            s = self._loc(src)
            d = pathlib.Path(dst)
            if recursive:
                shutil.copytree(s, d, dirs_exist_ok=True)
            else:
                d.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(s, d)
        else:
            s = pathlib.Path(src)
            if recursive:
                d = self._loc(dst.rstrip("/")) / s.name
                shutil.copytree(s, d, dirs_exist_ok=True)
            else:
                d = self._loc(dst)
                d.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(s, d)


class FakeHttp:
    """Serves a FakeGcloud's dir over 'HTTPS': public URL -> bytes, 404 if absent.

    The public-URL mapping is the inverse of FakeGcloud's gs:// layout, so one
    root dir feeds both an upload (FakeGcloud) and the fetch that reads it back.
    """

    def __init__(self, ip, root):
        self._ip = ip
        self.root = pathlib.Path(root)
        self.calls = []

    def get(self, url):
        self.calls.append(url)
        origin = self._ip.PUBLIC_ORIGIN
        assert url.startswith(origin), url
        rel = urllib.parse.unquote(url[len(origin):])
        p = self.root / rel
        if not p.is_file():
            raise self._ip.HttpError(url, 404, "Not Found")
        return p.read_bytes()


OPTICAL_PSV = (
    "# version=2017\n"
    "permID|provID|obsTime|ra|dec\n"
    "   99942||2021-01-01T00:00:00Z|1.0|2.0\n"
    "   99942||2021-01-02T00:00:00Z|1.1|2.1\n"
)
RADAR_PSV = (
    "# version=2017\n"
    "permID|provID|obsTime|ra|dec\n"
    "   99942||2021-01-01T00:00:00Z|1.0|2.0\n"
    "permID|trx|rcv|obsTime|delay\n"
    "   99942|253|253|2021-01-01T00:00:00Z|113.0\n"
)


def make_tree(root):
    root = pathlib.Path(root)
    (root / "psv").mkdir(parents=True)
    (root / "psv-radar").mkdir(parents=True)
    (root / "horizons").mkdir(parents=True)
    (root / "sbdb").mkdir(parents=True)
    (root / "psv" / "Apophis.psv").write_text(OPTICAL_PSV)
    (root / "psv-radar" / "Apophis.psv").write_text(RADAR_PSV)
    (root / "horizons" / "cmd_10.json").write_text(json.dumps({"key": "10|1", "response": {"x": 1}}))
    (root / "sbdb" / "Apophis.json").write_text(
        json.dumps({"key": "Apophis", "response": {"orbit": {"epoch": 2461000.5}}})
    )
    # A non-product file that must never be listed/verified/pruned.
    (root / "windows.json").write_text("{}")
    return root


PROV = {"catalog_commit": "abc", "distribution_version": "0.11.0-rc.0", "psv_source": "pinned:x"}


def mint(ip, tree, partial=False):
    """manifest -> pin, returning (manifest dict, pin dict, pin_path)."""
    manifest = ip.build_manifest(pathlib.Path(tree), PROV, partial)
    ip.atomic_write_json(pathlib.Path(tree) / "MANIFEST.json", manifest)
    pin = ip.build_pin(manifest, "http://run/1", ip.DEFAULT_BUCKET, partial)
    pin_path = pathlib.Path(tree).parent / "pin.json"
    ip.atomic_write_json(pin_path, pin)
    return manifest, pin, pin_path


# ── data_rows ────────────────────────────────────────────────────────────────


def test_data_rows_optical_and_radar(ip):
    assert ip.data_rows(OPTICAL_PSV) == 2
    assert ip.data_rows(RADAR_PSV) == 2  # one header per block, both skipped


@pytest.mark.skipif(not MAIN_FIXTURES.exists(), reason="main-checkout PSVs not present")
def test_data_rows_matches_main_manifest(ip, tmp_path):
    with open(MAIN_FIXTURES / "manifest.json") as fh:
        man = json.load(fh)
    rows = {f["path"]: f["data_rows"] for f in man["files"] if "data_rows" in f}
    checked = 0
    for rel, expected in rows.items():
        src = MAIN_FIXTURES / rel
        if not src.exists():
            continue
        local = tmp_path / src.name  # copy out; never write into the main checkout
        shutil.copy2(src, local)
        assert ip.data_rows(local.read_text()) == expected, rel
        checked += 1
    assert checked > 0


# ── manifest ─────────────────────────────────────────────────────────────────


def test_manifest_counts_and_data_rows(ip, tmp_path):
    tree = make_tree(tmp_path / "t")
    m = ip.build_manifest(tree, PROV, partial=False)
    assert m["products"] == {"psv": 1, "psv-radar": 1, "horizons": 1, "sbdb": 1}
    assert len(m["files"]) == 4
    assert [f["path"] for f in m["files"]] == sorted(f["path"] for f in m["files"])
    assert not any(f["path"] == "windows.json" for f in m["files"])
    psv = next(f for f in m["files"] if f["path"] == "psv/Apophis.psv")
    assert psv["data_rows"] == 2
    assert "data_rows" not in next(f for f in m["files"] if f["path"].startswith("sbdb/"))


def test_manifest_refuses_missing_and_empty_product_dir(ip, tmp_path):
    tree = make_tree(tmp_path / "t")
    shutil.rmtree(tree / "sbdb")
    with pytest.raises(ip.ContentError):
        ip.build_manifest(tree, PROV, partial=False)

    tree2 = make_tree(tmp_path / "t2")
    for p in (tree2 / "horizons").iterdir():
        p.unlink()
    with pytest.raises(ip.ContentError):
        ip.build_manifest(tree2, PROV, partial=False)
    # --partial tolerates an empty product dir.
    m = ip.build_manifest(tree2, PROV, partial=True)
    assert m["partial"] is True and m["products"]["horizons"] == 0


def test_content_addressed_id_changes_on_one_byte(ip, tmp_path):
    tree = make_tree(tmp_path / "t")
    first = ip.build_manifest(tree, PROV, partial=False)["snapshot_id"]
    f = tree / "horizons" / "cmd_10.json"
    f.write_text(f.read_text() + " ")  # one byte
    second = ip.build_manifest(tree, PROV, partial=False)["snapshot_id"]
    assert first != second


def test_manifest_ignores_non_product_files_in_product_dirs(ip, tmp_path):
    tree = make_tree(tmp_path / "t")
    before = ip.build_manifest(tree, PROV, partial=False)["snapshot_id"]
    # A tracked README.md inside psv/ (copied in by `cp -R fixtures/psv …`) and a
    # non-.json note inside horizons/ are not snapshot-shaped: never listed, and
    # the snapshot id does not move.
    (tree / "psv" / "README.md").write_text("tracked companion, not a product")
    (tree / "horizons" / "notes.txt").write_text("not snapshot-shaped")
    m = ip.build_manifest(tree, PROV, partial=False)
    assert not any(f["path"].endswith("README.md") for f in m["files"])
    assert not any(f["path"].endswith("notes.txt") for f in m["files"])
    assert m["products"] == {"psv": 1, "psv-radar": 1, "horizons": 1, "sbdb": 1}
    assert m["snapshot_id"] == before


# ── pin ──────────────────────────────────────────────────────────────────────


def test_pin_refuses_partial_without_flag(ip, tmp_path):
    tree = make_tree(tmp_path / "t")
    m = ip.build_manifest(tree, PROV, partial=True)
    with pytest.raises(ip.ContentError):
        ip.build_pin(m, "url", ip.DEFAULT_BUCKET, allow_partial=False)
    pin = ip.build_pin(m, "url", ip.DEFAULT_BUCKET, allow_partial=True)
    assert pin["partial"] is True and pin["files_total"] == len(m["files"])


# ── diff ─────────────────────────────────────────────────────────────────────


def test_diff_added_removed_changed_and_row_deltas(ip, tmp_path):
    tree = make_tree(tmp_path / "t")
    (tree / "horizons" / "cmd_11.json").write_text("{}")  # second file so removal leaves the dir non-empty
    old = ip.build_manifest(tree, PROV, partial=False)
    (tree / "psv" / "Bennu.psv").write_text(OPTICAL_PSV)  # add
    (tree / "horizons" / "cmd_11.json").unlink()  # remove
    (tree / "psv" / "Apophis.psv").write_text(OPTICAL_PSV + "   99942||2021-01-03T00:00:00Z|1.2|2.2\n")  # change
    new = ip.build_manifest(tree, PROV, partial=False)
    out = ip.diff(old, new)
    assert "+ psv/Bennu.psv" in out
    assert "- horizons/cmd_11.json" in out
    assert "~ psv/Apophis.psv" in out
    assert "(2 -> 3 rows, +1)" in out


# ── merge (payload-identical dedup of shared cache keys) ─────────────────────


def hor_entry(ts, soe_body):
    """A Horizons cache entry whose generation time (ts) is in the header/footer
    of `result`, outside the $$SOE…$$EOE data block."""
    result = (
        "*******************************************************\n"
        f"Ephemeris / API_USER {ts} Pasadena, USA      / Horizons\n"
        "Target body name: Sun (10)\n"
        "$$SOE\n"
        f"{soe_body}\n"
        "$$EOE\n"
        f"footer generated {ts}\n"
    )
    return {"key": "10|61380.00000000", "response": {"result": result, "signature": {"version": "1.2", "source": "NASA/JPL Horizons API"}}}


def sbdb_entry(epoch):
    return {"key": "sbdb.api?des=99942", "response": {"object": {"des": "99942"}, "orbit": {"epoch": epoch}}}


def write_shard_file(root, shard, product, name, entry):
    d = root / shard / product
    d.mkdir(parents=True, exist_ok=True)
    (d / name).write_text(json.dumps(entry))
    return root / shard


SUN_BODY = "2461000.500000000 = A.D. 2025-Dot  X = 1.0 Y = 2.0 Z = 3.0  VX= 0.1 VY= 0.2 VZ= 0.3"


def test_merge_resolves_timestamp_only_horizons_duplicate(ip, tmp_path):
    shards = tmp_path / "shards"
    a = write_shard_file(shards, "inputs-shard-A", "horizons", "10_61380.json", hor_entry("Sun Oct 25 10:38:22 2026", SUN_BODY))
    b = write_shard_file(shards, "inputs-shard-B", "horizons", "10_61380.json", hor_entry("Sun Oct 25 10:38:05 2026", SUN_BODY))
    out = tmp_path / "tree"
    assert ip.merge(out, [a, b]) == 0
    # One copy only — the second shard's byte-different (timestamp) file is the
    # same input, so it is resolved, not copied. A sorts first, so its bytes win.
    kept = (out / "horizons" / "10_61380.json").read_text()
    assert "10:38:22" in kept and "10:38:05" not in kept


def test_merge_fails_on_horizons_payload_difference(ip, tmp_path):
    shards = tmp_path / "shards"
    a = write_shard_file(shards, "inputs-shard-A", "horizons", "10_61380.json", hor_entry("ts-1", SUN_BODY))
    b = write_shard_file(shards, "inputs-shard-B", "horizons", "10_61380.json",
                         hor_entry("ts-2", SUN_BODY.replace("X = 1.0", "X = 9.9")))
    with pytest.raises(ip.ContentError) as exc:
        ip.merge(tmp_path / "tree", [a, b])
    msg = "\n".join(exc.value.messages)
    assert "horizons/10_61380.json" in msg and "inputs-shard-A" in msg and "inputs-shard-B" in msg


def test_merge_sbdb_equal_kept_different_fails(ip, tmp_path):
    shards = tmp_path / "shards"
    a = write_shard_file(shards, "inputs-shard-A", "sbdb", "Apophis.json", sbdb_entry(2461000.5))
    b = write_shard_file(shards, "inputs-shard-B", "sbdb", "Apophis.json", sbdb_entry(2461000.5))
    assert ip.merge(tmp_path / "tree", [a, b]) == 0
    shards2 = tmp_path / "shards2"
    c = write_shard_file(shards2, "inputs-shard-A", "sbdb", "Apophis.json", sbdb_entry(2461000.5))
    d = write_shard_file(shards2, "inputs-shard-B", "sbdb", "Apophis.json", sbdb_entry(2462000.5))
    with pytest.raises(ip.ContentError) as exc:
        ip.merge(tmp_path / "tree2", [c, d])
    assert "sbdb/Apophis.json" in "\n".join(exc.value.messages)


# ── upload (fake gcloud) ─────────────────────────────────────────────────────


def test_upload_plan_without_yes_uploads_nothing(ip, tmp_path, capsys):
    tree = make_tree(tmp_path / "t")
    mint(ip, tree)
    bucket = f"gs://{(tmp_path / 'bkt').as_posix().lstrip('/')}"
    fake = FakeGcloud(tmp_path / "root")
    assert ip.upload(tree, bucket, yes=False, gcloud=fake) == 0
    assert fake.ls(f"{bucket}/**") == []


def test_upload_yes_roundtrip_count(ip, tmp_path):
    tree = make_tree(tmp_path / "t")
    manifest, _, _ = mint(ip, tree)
    bucket = f"gs://{(tmp_path / 'bkt').as_posix().lstrip('/')}"
    fake = FakeGcloud(tmp_path / "root")
    assert ip.upload(tree, bucket, yes=True, gcloud=fake) == 0
    dest = f"{bucket}/{manifest['snapshot_id']}"
    assert len(fake.ls(f"{dest}/**")) == len(manifest["files"]) + 1


def test_upload_refuses_existing_prefix(ip, tmp_path):
    tree = make_tree(tmp_path / "t")
    manifest, _, _ = mint(ip, tree)
    bucket = f"gs://{(tmp_path / 'bkt').as_posix().lstrip('/')}"
    fake = FakeGcloud(tmp_path / "root")
    # Pre-seed one object under the destination prefix.
    (fake.root / bucket[len('gs://'):] / manifest["snapshot_id"] / "psv").mkdir(parents=True)
    (fake.root / bucket[len('gs://'):] / manifest["snapshot_id"] / "psv" / "x.psv").write_text("x")
    with pytest.raises(ip.ContentError):
        ip.upload(tree, bucket, yes=True, gcloud=fake)


def test_upload_refuses_sha_mismatch_in_tree(ip, tmp_path):
    tree = make_tree(tmp_path / "t")
    mint(ip, tree)
    (tree / "psv" / "Apophis.psv").write_text(OPTICAL_PSV + "tampered\n")  # manifest now stale
    bucket = f"gs://{(tmp_path / 'bkt').as_posix().lstrip('/')}"
    with pytest.raises(ip.ContentError):
        ip.upload(tree, bucket, yes=True, gcloud=FakeGcloud(tmp_path / "root"))


def test_upload_post_count_mismatch_fails(ip, tmp_path):
    tree = make_tree(tmp_path / "t")
    mint(ip, tree)
    bucket = f"gs://{(tmp_path / 'bkt').as_posix().lstrip('/')}"
    fake = FakeGcloud(tmp_path / "root")
    fake.ls = lambda uri: []  # pretend nothing landed after the copies
    with pytest.raises(ip.ContentError):
        ip.upload(tree, bucket, yes=True, gcloud=fake)


# ── fetch (offline = no reader; online = fake HTTP reader) ────────────────────


def _publish(ip, tmp_path):
    """Mint a tree and upload it to a dir-backed bucket at the default gs:// path;
    the pin already names that bucket. Returns a FakeHttp reading the same dir
    back over the public-URL mapping (root/empyrean-validation/inputs/<id>/...)."""
    src = make_tree(tmp_path / "src")
    manifest, pin, pin_path = mint(ip, src)  # pin bucket == DEFAULT_BUCKET already
    fake = FakeGcloud(tmp_path / "root")
    assert ip.upload(src, ip.DEFAULT_BUCKET, yes=True, gcloud=fake) == 0
    return manifest, pin, pin_path, fake, FakeHttp(ip, tmp_path / "root")


def test_fetch_offline_roundtrip_all_kept(ip, tmp_path):
    tree = make_tree(tmp_path / "t")
    _, _, pin_path = mint(ip, tree)
    assert ip.fetch(pin_path, tree, prune=False, offline=True) == 0


def test_fetch_online_roundtrip(ip, tmp_path):
    _, _, pin_path, _, http = _publish(ip, tmp_path)
    dest = tmp_path / "dest"
    assert ip.fetch(pin_path, dest, prune=False, offline=False, http_get=http.get) == 0
    assert (dest / "psv" / "Apophis.psv").read_text() == OPTICAL_PSV
    assert (dest / "MANIFEST.json").exists()
    assert http.calls  # the reader was actually used


def test_fetch_offline_never_calls_the_reader(ip, tmp_path):
    tree = make_tree(tmp_path / "t")
    _, _, pin_path = mint(ip, tree)

    def boom(url):
        raise AssertionError(f"reader called under --offline: {url}")

    assert ip.fetch(pin_path, tree, prune=False, offline=True, http_get=boom) == 0


def test_fetch_listed_file_404_fails_naming_the_url(ip, tmp_path):
    manifest, _, pin_path, fake, http = _publish(ip, tmp_path)
    # Remove one listed object from the bucket -> its URL 404s.
    gone = fake.root / "empyrean-validation" / "inputs" / manifest["snapshot_id"] / "sbdb" / "Apophis.json"
    gone.unlink()
    dest = tmp_path / "dest"
    with pytest.raises(ip.ContentError) as exc:
        ip.fetch(pin_path, dest, prune=False, offline=False, http_get=http.get)
    msg = "\n".join(exc.value.messages)
    assert "sbdb/Apophis.json" in msg
    assert "HTTP 404" in msg and "storage.googleapis.com" in msg


def test_fetch_bad_hash_fails_and_leaves_no_part_or_target(ip, tmp_path):
    manifest, _, pin_path, fake, http = _publish(ip, tmp_path)
    # Corrupt one object's bytes in the bucket so the download hashes wrong.
    bad = fake.root / "empyrean-validation" / "inputs" / manifest["snapshot_id"] / "psv" / "Apophis.psv"
    bad.write_text(OPTICAL_PSV + "tampered-in-the-bucket\n")
    dest = tmp_path / "dest"
    with pytest.raises(ip.ContentError) as exc:
        ip.fetch(pin_path, dest, prune=False, offline=False, http_get=http.get)
    assert "psv/Apophis.psv" in "\n".join(exc.value.messages)
    assert not (dest / "psv" / "Apophis.psv").exists()
    assert list((dest / "psv").glob("*.part*")) == []


def test_fetch_stale_file_offline_fails(ip, tmp_path):
    tree = make_tree(tmp_path / "t")
    _, _, pin_path = mint(ip, tree)
    (tree / "psv" / "Apophis.psv").write_text(OPTICAL_PSV + "drifted\n")
    with pytest.raises(ip.ContentError):
        ip.fetch(pin_path, tree, prune=False, offline=True)


def test_fetch_missing_file_offline_fails(ip, tmp_path):
    tree = make_tree(tmp_path / "t")
    _, _, pin_path = mint(ip, tree)
    (tree / "sbdb" / "Apophis.json").unlink()
    with pytest.raises(ip.ContentError):
        ip.fetch(pin_path, tree, prune=False, offline=True)


def test_fetch_extra_psv_fails_but_prune_clears(ip, tmp_path):
    tree = make_tree(tmp_path / "t")
    _, _, pin_path = mint(ip, tree)
    (tree / "psv" / "Stray.psv").write_text(OPTICAL_PSV)
    with pytest.raises(ip.ContentError):
        ip.fetch(pin_path, tree, prune=False, offline=True)
    # --prune removes the unlisted file and the run passes.
    assert ip.fetch(pin_path, tree, prune=True, offline=True) == 0
    assert not (tree / "psv" / "Stray.psv").exists()


def test_fetch_extra_nonpsv_is_reported_not_fatal(ip, tmp_path):
    tree = make_tree(tmp_path / "t")
    _, _, pin_path = mint(ip, tree)
    (tree / "horizons" / "stray.json").write_text("{}")  # not a .psv
    assert ip.fetch(pin_path, tree, prune=False, offline=True) == 0


def test_fetch_prune_keeps_readme_but_flags_stray_psv(ip, tmp_path):
    tree = make_tree(tmp_path / "t")
    _, _, pin_path = mint(ip, tree)
    # Tracked companions inside a product dir are not snapshot-shaped files:
    # a README.md (psv/) and a non-.json note (horizons/) must survive --prune
    # and never be reported as extra.
    (tree / "psv" / "README.md").write_text("tracked companion, not a product")
    (tree / "horizons" / "notes.txt").write_text("not snapshot-shaped")
    assert ip.fetch(pin_path, tree, prune=True, offline=True) == 0
    assert (tree / "psv" / "README.md").exists()
    assert (tree / "horizons" / "notes.txt").exists()
    # A stray unlisted *.psv is still flagged ...
    (tree / "psv" / "Stray.psv").write_text(OPTICAL_PSV)
    with pytest.raises(ip.ContentError):
        ip.fetch(pin_path, tree, prune=False, offline=True)
    # ... and pruned with --prune, while the README still survives.
    assert ip.fetch(pin_path, tree, prune=True, offline=True) == 0
    assert not (tree / "psv" / "Stray.psv").exists()
    assert (tree / "psv" / "README.md").exists()


def test_fetch_treehash_mismatch_pin_vs_manifest_offline_fails(ip, tmp_path):
    tree = make_tree(tmp_path / "t")
    _, pin, pin_path = mint(ip, tree)
    pin["tree_hash"] = "0" * 64  # pin disagrees with the on-disk manifest
    ip.atomic_write_json(pin_path, pin)
    with pytest.raises(ip.ContentError):
        ip.fetch(pin_path, tree, prune=False, offline=True)


def test_fetch_treehash_mismatch_online_fails(ip, tmp_path):
    manifest, _, pin_path, fake, http = _publish(ip, tmp_path)
    # Corrupt the bucket manifest's recorded tree_hash; its files[] no longer hash to it.
    man_loc = fake.root / "empyrean-validation" / "inputs" / manifest["snapshot_id"] / "MANIFEST.json"
    with open(man_loc) as fh:
        bad = json.load(fh)
    bad["tree_hash"] = "f" * 64
    man_loc.write_text(json.dumps(bad))
    dest = tmp_path / "dest"
    with pytest.raises(ip.ContentError):
        ip.fetch(pin_path, dest, prune=False, offline=False, http_get=http.get)


def test_fetch_partial_pin_refused(ip, tmp_path):
    tree = make_tree(tmp_path / "t")
    _, _, pin_path = mint(ip, tree, partial=True)
    with pytest.raises(ip.ContentError):
        ip.fetch(pin_path, tree, prune=False, offline=True)
    # --allow-partial lets it through.
    assert ip.fetch(pin_path, tree, prune=False, offline=True, allow_partial=True) == 0


# ── summary ──────────────────────────────────────────────────────────────────


def test_summary_lists_products_rows_and_epoch(ip, tmp_path):
    tree = make_tree(tmp_path / "t")
    out = ip.summary(tree)
    assert "psv" in out and "horizons" in out
    assert "Apophis.psv" in out
    # 2461000.5 JD -> 61000.0 MJD
    assert "61000.000000" in out
