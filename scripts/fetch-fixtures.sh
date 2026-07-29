#!/usr/bin/env bash
# Fetch + verify the OD fixture set pinned by fixtures/manifest.json.
#
# The ADES PSV fixture data lives in an immutable, public-read GCS snapshot
# (see fixtures/README.md for the contract); this script materializes it into
# fixtures/psv/ + fixtures/psv-radar/ over plain HTTPS — no gcloud, no auth.
#
# Behavior, in the no-hidden-fallbacks discipline this repo runs on:
#   - EVERY file's sha256 is verified on EVERY invocation, cache hit or miss.
#     A file that is present-but-wrong is re-fetched; a fetch that yields the
#     wrong bytes is an error naming the path + expected/actual hash.
#   - Downloads land in a unique temp name and are atomically renamed into
#     place only AFTER their hash checks out, so a killed or concurrent run
#     can never leave a partial file where a fixture belongs.
#   - Any *.psv on disk that the manifest does not list is an error: the
#     runners glob these directories, so an unlisted file would be silently
#     fitted alongside the pinned set.
#   - Exit is nonzero if ANY file could not be brought to a verified state;
#     every failure is reported, none is skipped.
#
# Dependencies: bash, curl, python3 (manifest parsing + URL-encoding; the
# Makefile already requires a bare python3), and sha256sum (Linux) or
# shasum -a 256 (macOS).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOT/fixtures/manifest.json"

[ -f "$MANIFEST" ] || {
    echo "ERROR: fixture manifest not found: $MANIFEST" >&2
    echo "       The manifest is tracked in git and pins the fixture snapshot;" >&2
    echo "       a checkout without it cannot fetch or verify anything." >&2
    exit 1
}

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        echo "ERROR: neither sha256sum nor shasum is available — cannot verify fixtures." >&2
        exit 1
    fi
}

# Manifest → header (snapshot_id, base_url) + one TSV line per file:
#   <path> \t <sha256> \t <url-encoded path>
# Paths contain spaces and apostrophes, hence the explicit URL-encoding and
# the tab-separated parse below.
parsed="$(python3 - "$MANIFEST" <<'PY'
import json, sys, urllib.parse
m = json.load(open(sys.argv[1]))
print(m["snapshot_id"])
print(m["base_url"])
for f in m["files"]:
    print("\t".join([f["path"], f["sha256"], urllib.parse.quote(f["path"])]))
PY
)"

snapshot_id="$(printf '%s\n' "$parsed" | sed -n '1p')"
base_url="$(printf '%s\n' "$parsed" | sed -n '2p')"
entries="$(printf '%s\n' "$parsed" | sed -n '3,$p')"

n_total=0
n_verified=0
n_fetched=0
failures=()

while IFS=$'\t' read -r path expected enc_path; do
    [ -n "$path" ] || continue
    n_total=$((n_total + 1))
    dest="$ROOT/fixtures/$path"
    reason="missing"

    if [ -f "$dest" ]; then
        actual="$(sha256_file "$dest")"
        if [ "$actual" = "$expected" ]; then
            n_verified=$((n_verified + 1))
            continue
        fi
        reason="stale (local sha256 $actual != manifest $expected)"
    fi

    # Fetch into a unique temp name; rename into place only after the hash
    # checks out. --fail turns HTTP errors into curl exit codes (a 404 — the
    # snapshot was never uploaded — fails immediately; transient network
    # errors and 408/429/5xx retry).
    url="$base_url/$snapshot_id/$enc_path"
    mkdir -p "$(dirname "$dest")"
    tmp="$(mktemp "${dest}.fetch.XXXXXX")"
    rc=0
    curl -fsSL --retry 5 --retry-delay 2 --connect-timeout 30 -o "$tmp" "$url" || rc=$?
    if [ "$rc" -ne 0 ]; then
        rm -f "$tmp"
        failures+=("$path: $reason, and the download failed (curl exit $rc) from $url")
        continue
    fi
    got="$(sha256_file "$tmp")"
    if [ "$got" = "$expected" ]; then
        mv -f "$tmp" "$dest"
        n_fetched=$((n_fetched + 1))
    else
        rm -f "$tmp"
        failures+=("$path: downloaded bytes hash to $got, manifest pins $expected — refusing to install them")
    fi
done <<<"$entries"

# Unlisted *.psv files would be globbed into fits alongside the pinned set —
# surface them instead of fitting them.
while IFS= read -r -d '' extra; do
    rel="${extra#"$ROOT"/fixtures/}"
    # Anchor the match at line start + field end so one path can never pass
    # as the tail of another ("psv/X.psv" vs "psv-radar/X.psv").
    case $'\n'"$entries" in
        *$'\n'"$rel"$'\t'*) ;;
        *) failures+=("$rel: present on disk but NOT in the manifest — an unlisted fixture would be silently fitted. Remove it, or mint a new snapshot (fixtures/README.md).") ;;
    esac
done < <(find "$ROOT/fixtures/psv" "$ROOT/fixtures/psv-radar" -maxdepth 1 -name '*.psv' -print0 2>/dev/null)

if [ "${#failures[@]}" -gt 0 ]; then
    echo "ERROR: fixture set is NOT usable — ${#failures[@]} problem(s) against snapshot $snapshot_id:" >&2
    for f in "${failures[@]}"; do
        echo "  - $f" >&2
    done
    echo "       Nothing may run fits on a partial or stale fixture set." >&2
    exit 1
fi

echo "fixtures OK: $n_total files ($n_verified verified in place, $n_fetched fetched) — snapshot $snapshot_id"
