"""engine_lib.py: the engine library is fetched once, through the authenticated
API, verified against the crate's checksums, and never from the public URL.

The GitHub API is a local HTTP server that behaves like it: the release lookup and
the asset endpoint demand the bearer token, the asset endpoint answers 302 to a
"CDN" path that REJECTS a request still carrying the token (as the real
pre-signed URL does), and failures can be injected. Every non-loopback
connection is refused for the whole module, so a test that passes proves the code
path needs no other network.
"""

import hashlib
import io
import json
import pathlib
import socket
import sys
import tarfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
import engine_lib as el  # noqa: E402

VERSION = "0.11.0-rc.1"
STEM = "libempyrean-linux-x86_64"
TOKEN = "test-token"
REPO = "Empyrean-Dynamics/empyrean"


@pytest.fixture(autouse=True)
def only_loopback(monkeypatch):
    real = socket.create_connection

    def guarded(address, *a, **k):
        if address[0] not in ("127.0.0.1", "localhost"):
            raise AssertionError(f"the engine-library path reached {address[0]}; only the (local) authenticated API is allowed")
        return real(address, *a, **k)

    monkeypatch.setattr(socket, "create_connection", guarded)
    monkeypatch.setenv("GH_TOKEN", TOKEN)


def make_tarball(path, members=None):
    members = members or {"libempyrean.so": b"ELF-not-really", "empyrean.h": b"/* header */", "LICENSE": b"MIT"}
    with tarfile.open(path, "w:gz") as tar:
        for name, data in members.items():
            info = tarfile.TarInfo(name)
            info.size = len(data)
            tar.addfile(info, io.BytesIO(data))
    return hashlib.sha256(pathlib.Path(path).read_bytes()).hexdigest()


class FakeApi:
    """A GitHub-shaped release API on 127.0.0.1."""

    def __init__(self, asset_path):
        self.asset_path = pathlib.Path(asset_path)
        self.requests = []  # (path, authorization header or None)
        self.fail_first = 0  # answer this many requests with 503 before behaving
        self.fail_status = None  # or always answer this status
        api = self

        class H(BaseHTTPRequestHandler):
            def log_message(self, *a):
                pass

            def reply(self, code, body=b"", headers=None):
                self.send_response(code)
                for k, v in (headers or {}).items():
                    self.send_header(k, v)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_GET(self):
                auth = self.headers.get("Authorization")
                api.requests.append((self.path, auth))
                if api.fail_status:
                    return self.reply(api.fail_status, b"{}")
                if api.fail_first > 0:
                    api.fail_first -= 1
                    return self.reply(503, b"{}")
                if self.path.startswith("/cdn/"):
                    if auth is not None:
                        return self.reply(400, b"the pre-signed URL refuses a bearer token")
                    return self.reply(200, api.asset_path.read_bytes())
                if auth != f"Bearer {TOKEN}":
                    return self.reply(401, b"{}")
                if self.path == f"/repos/{REPO}/releases/tags/v{VERSION}":
                    assets = [{"name": f"{STEM}.tar.gz", "id": 77}, {"name": "other.tar.gz", "id": 78}]
                    return self.reply(200, json.dumps({"assets": assets}).encode())
                if self.path == f"/repos/{REPO}/releases/assets/77":
                    assert self.headers.get("Accept") == "application/octet-stream"
                    return self.reply(302, headers={"Location": f"http://localhost:{api.server.server_port}/cdn/asset"})
                return self.reply(404, b"{}")

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), H)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.url = f"http://127.0.0.1:{self.server.server_port}"

    def close(self):
        self.server.shutdown()
        self.server.server_close()

    @property
    def asset_fetches(self):
        return [r for r in self.requests if r[0] == f"/repos/{REPO}/releases/assets/77"]


@pytest.fixture
def world(tmp_path):
    asset = tmp_path / "release" / f"{STEM}.tar.gz"
    asset.parent.mkdir()
    sha = make_tarball(asset)
    checksums = tmp_path / "checksums.txt"
    checksums.write_text(
        "# SHA-256 of each libempyrean-<target>.tar.gz release asset.\n"
        f"# Header-SHA256: {'0' * 64}\n"
        f"libempyrean-linux-aarch64 {'1' * 64}\n"
        f"{STEM} {sha}\n"
    )
    api = FakeApi(asset)
    yield tmp_path, asset, sha, checksums, api
    api.close()


def ensure(w, dest, **kw):
    tmp, asset, sha, checksums, api = w
    args = dict(checksums=checksums, offline=False, stem=STEM, api_url=api.url, sleep=lambda s: None, log=lambda m: None)
    args.update(kw)
    return el.ensure(VERSION, dest, **args)


def test_fetch_downloads_once_through_the_authenticated_api_and_verifies(world, tmp_path):
    w = world
    lines = []
    lib = ensure(w, tmp_path / "lib", log=lines.append)
    assert lib.read_bytes() == b"ELF-not-really"
    api = w[4]
    assert len(api.asset_fetches) == 1
    assert api.asset_fetches[0][1] == f"Bearer {TOKEN}", "the asset endpoint is called with the token"
    cdn = [r for r in api.requests if r[0].startswith("/cdn/")]
    assert len(cdn) == 1 and cdn[0][1] is None, "the token must not follow the redirect to another host's URL"
    assert all(path.startswith(("/repos/", "/cdn/")) for path, _ in api.requests)
    rec = json.loads((tmp_path / "lib" / el.RECORD).read_text())
    assert rec["sha256"] == w[2] and rec["source"] == "api" and rec["version"] == VERSION
    assert len(lines) == 1 and lines[0].startswith("engine-lib: v0.11.0-rc.1 libempyrean-linux-x86_64 sha256=")
    assert "source=api" in lines[0] and "public release URL is not used" in lines[0]


def test_a_second_fetch_uses_the_cache_and_does_not_call_the_api(world, tmp_path):
    w = world
    dest = tmp_path / "lib"
    ensure(w, dest)
    lines = []
    ensure(w, dest, log=lines.append)
    assert len(w[4].asset_fetches) == 1, "the cached engine must not be fetched again"
    assert "source=cache" in lines[0]


def test_a_checksum_mismatch_is_refused_by_name_and_nothing_is_cached(world, tmp_path):
    w = world
    w[3].write_text(f"{STEM} {'f' * 64}\n")
    dest = tmp_path / "lib"
    with pytest.raises(el.EngineLibError) as exc:
        ensure(w, dest)
    msg = str(exc.value)
    assert "checksum mismatch" in msg and STEM in msg and "f" * 64 in msg and w[2] in msg
    assert "Refusing to use an unverified binary" in msg
    assert not (dest / f"{STEM}.tar.gz").exists() and not (dest / "libempyrean.so").exists()
    assert not (dest / el.RECORD).exists()


def test_a_cached_tarball_that_no_longer_matches_is_refetched_not_trusted(world, tmp_path):
    w = world
    dest = tmp_path / "lib"
    ensure(w, dest)
    (dest / f"{STEM}.tar.gz").write_bytes(b"corrupted cache")
    ensure(w, dest)
    assert len(w[4].asset_fetches) == 2
    assert (dest / "libempyrean.so").read_bytes() == b"ELF-not-really"


def test_retries_with_backoff_then_succeeds(world, tmp_path):
    w = world
    w[4].fail_first = 3
    sleeps = []
    ensure(w, tmp_path / "lib", sleep=sleeps.append)
    assert len(w[4].asset_fetches) == 1, "the three 503s were retried, then it succeeded once"
    assert sleeps == [5.0, 10.0, 20.0]


def test_exhausted_retries_fail_naming_the_api_and_refusing_the_public_url(world, tmp_path):
    w = world
    w[4].fail_first = 99
    with pytest.raises(el.EngineLibError) as exc:
        ensure(w, tmp_path / "lib", retries=3)
    msg = str(exc.value)
    assert "after 3 attempts" in msg and "authenticated API" in msg and "not falling back to the public release URL" in msg
    assert "HTTP 503" in msg
    assert len(w[4].requests) == 3


def test_a_client_error_is_named_at_once_and_not_retried(world, tmp_path):
    w = world
    w[4].fail_status = 404
    sleeps = []
    with pytest.raises(el.EngineLibError, match="refused the request.*HTTP 404.*not falling back"):
        ensure(w, tmp_path / "lib", sleep=sleeps.append)
    assert sleeps == [] and len(w[4].requests) == 1


def test_a_release_without_the_asset_is_refused_by_name(world, tmp_path):
    w = world
    with pytest.raises(el.EngineLibError, match="has no asset libempyrean-linux-aarch64.tar.gz"):
        ensure(w, tmp_path / "lib", stem="libempyrean-linux-aarch64")


def test_no_token_refuses_rather_than_falling_back(world, tmp_path, monkeypatch):
    monkeypatch.delenv("GH_TOKEN")
    monkeypatch.delenv("GITHUB_TOKEN", raising=False)
    with pytest.raises(el.EngineLibError, match="no GH_TOKEN"):
        ensure(world, tmp_path / "lib")
    assert world[4].requests == []


def test_restore_is_offline_verifies_and_extracts(world, tmp_path):
    w = world
    dest = tmp_path / "lib"
    ensure(w, dest)
    (dest / "libempyrean.so").unlink()  # a cache restored without the extracted file
    (dest / el.RECORD).unlink()
    lines = []
    lib = el.ensure(VERSION, dest, checksums=None, offline=True, stem=STEM, log=lines.append)
    assert lib.read_bytes() == b"ELF-not-really"
    assert len(w[4].requests) == 3, "restore must never call the API"
    assert "source=cache" in lines[0]


def test_restore_refuses_a_miss_and_a_corrupted_cache_by_name(world, tmp_path):
    w = world
    empty = tmp_path / "empty"
    with pytest.raises(el.EngineLibError, match="no checksums to verify against"):
        el.ensure(VERSION, empty, checksums=None, offline=True, stem=STEM, log=lambda m: None)
    dest = tmp_path / "lib"
    ensure(w, dest)
    (dest / f"{STEM}.tar.gz").write_bytes(b"tampered")
    with pytest.raises(el.EngineLibError, match="refusing it"):
        el.ensure(VERSION, dest, checksums=None, offline=True, stem=STEM, log=lambda m: None)
    assert len(w[4].asset_fetches) == 1
    dest2 = tmp_path / "lib2"
    dest2.mkdir()
    (dest2 / el.CHECKSUMS_COPY).write_text(w[3].read_text())
    with pytest.raises(el.EngineLibError, match="does not download it"):
        el.ensure(VERSION, dest2, checksums=None, offline=True, stem=STEM, log=lambda m: None)


def test_a_checksums_file_without_the_platform_is_refused(world, tmp_path):
    w = world
    w[3].write_text(f"libempyrean-linux-aarch64 {'1' * 64}\n")
    with pytest.raises(el.EngineLibError, match="pins no checksum for libempyrean-linux-x86_64"):
        ensure(w, tmp_path / "lib")


def test_an_unsafe_tar_member_is_refused(world, tmp_path):
    w = world
    sha = make_tarball(w[1], {"../escape.so": b"x", "libempyrean.so": b"y"})
    w[3].write_text(f"{STEM} {sha}\n")
    with pytest.raises(el.EngineLibError, match="unsafe member"):
        ensure(w, tmp_path / "lib")
    assert not (tmp_path / "escape.so").exists()


def test_a_tarball_without_the_library_is_refused(world, tmp_path):
    w = world
    sha = make_tarball(w[1], {"LICENSE": b"x"})
    w[3].write_text(f"{STEM} {sha}\n")
    with pytest.raises(el.EngineLibError, match="did not contain libempyrean.so"):
        ensure(w, tmp_path / "lib")


def test_platform_stems():
    assert el.platform_stem("linux", "x86_64") == "libempyrean-linux-x86_64"
    assert el.platform_stem("linux", "aarch64") == "libempyrean-linux-aarch64"
    assert el.platform_stem("darwin", "arm64") == "libempyrean-macos-aarch64"
    assert el.lib_filename("libempyrean-macos-aarch64") == "libempyrean.dylib"
    with pytest.raises(el.EngineLibError, match="no prebuilt"):
        el.platform_stem("win32", "x86_64")


def test_cli_fetch_then_restore_and_error_exit(world, tmp_path, capsys):
    w = world
    api = ["--api-url", w[4].url]
    dest = tmp_path / "lib"
    assert el.main(["fetch", "--version", VERSION, "--dest", str(dest), "--stem", STEM, "--checksums", str(w[3]), *api]) == 0
    assert el.main(["restore", "--version", VERSION, "--dest", str(dest), "--stem", STEM]) == 0
    assert capsys.readouterr().out.count("engine-lib: v0.11.0-rc.1") == 2
    w[3].write_text(f"{STEM} {'0' * 64}\n")
    shutil_dest = tmp_path / "lib-bad"
    assert el.main(["fetch", "--version", VERSION, "--dest", str(shutil_dest), "--stem", STEM, "--checksums", str(w[3]), *api]) == 1
    assert "ERROR: engine-lib: libempyrean checksum mismatch" in capsys.readouterr().err


CRATE = list(pathlib.Path.home().glob(f".cargo/registry/src/*/empyrean-sys-{VERSION}/checksums.txt"))


@pytest.mark.skipif(not CRATE, reason="empyrean-sys crate not in the local cargo registry")
def test_the_real_crate_checksums_parse_to_the_four_platforms():
    table = el.parse_checksums(CRATE[0].read_text())
    assert set(table) >= {
        "libempyrean-linux-aarch64",
        "libempyrean-linux-x86_64",
        "libempyrean-macos-aarch64",
        "libempyrean-macos-x86_64",
    }
    assert all(len(v) == 64 for v in table.values())
