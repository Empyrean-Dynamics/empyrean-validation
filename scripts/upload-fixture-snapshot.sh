#!/usr/bin/env bash
# Mint the fixture snapshot pinned by fixtures/manifest.json into GCS.
#
# Uploads every manifest-listed file, plus the manifest itself, to
#   gs://empyrean-validation/fixtures/<snapshot_id>/...
# and grants public-read on those objects so readers (CI legs, forks,
# contributors) fetch over plain HTTPS with no auth (the data is public MPC
# astrometry). gcloud is needed HERE only — never by the fetch path.
#
# Snapshots are IMMUTABLE: this script refuses to run if anything already
# exists under the destination prefix. A fixture refresh mints a NEW snapshot
# (new manifest → new snapshot_id → new prefix) in a reviewed PR; it never
# rewrites an existing one (see fixtures/README.md).
#
# Echo-first: without --yes it prints exactly what it would do and exits.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOT/fixtures/manifest.json"
BUCKET="gs://empyrean-validation"

YES=0
for arg in "$@"; do
    case "$arg" in
        --yes) YES=1 ;;
        *)
            echo "usage: $0 [--yes]" >&2
            echo "  Without --yes: dry run (prints the plan, uploads nothing)." >&2
            exit 2
            ;;
    esac
done

[ -f "$MANIFEST" ] || { echo "ERROR: manifest not found: $MANIFEST" >&2; exit 1; }
command -v gcloud >/dev/null 2>&1 || {
    echo "ERROR: gcloud is required to upload a snapshot (readers never need it)." >&2
    exit 1
}

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

parsed="$(python3 - "$MANIFEST" <<'PY'
import json, sys
m = json.load(open(sys.argv[1]))
print(m["snapshot_id"])
for f in m["files"]:
    print("\t".join([f["path"], f["sha256"]]))
PY
)"
snapshot_id="$(printf '%s\n' "$parsed" | sed -n '1p')"
entries="$(printf '%s\n' "$parsed" | sed -n '2,$p')"
dest_prefix="$BUCKET/fixtures/$snapshot_id"

# ── 1. The local tree must match the manifest bit-exactly ────────────────
# A snapshot minted from a drifted tree would pin hashes the objects don't
# have, and every fetch everywhere would fail. Verify first, loudly.
echo "Verifying the local fixture tree against $MANIFEST ..."
n=0
failures=()
while IFS=$'\t' read -r path expected; do
    [ -n "$path" ] || continue
    n=$((n + 1))
    f="$ROOT/fixtures/$path"
    if [ ! -f "$f" ]; then
        failures+=("$path: missing on disk")
        continue
    fi
    actual="$(sha256_file "$f")"
    [ "$actual" = "$expected" ] || failures+=("$path: local sha256 $actual != manifest $expected")
done <<<"$entries"
while IFS= read -r -d '' extra; do
    rel="${extra#"$ROOT"/fixtures/}"
    case $'\n'"$entries" in
        *$'\n'"$rel"$'\t'*) ;;
        *) failures+=("$rel: on disk but not in the manifest — regenerate the manifest before minting") ;;
    esac
done < <(find "$ROOT/fixtures/psv" "$ROOT/fixtures/psv-radar" -maxdepth 1 -name '*.psv' -print0 2>/dev/null)
if [ "${#failures[@]}" -gt 0 ]; then
    echo "ERROR: local tree does not match the manifest — refusing to mint snapshot $snapshot_id:" >&2
    for f in "${failures[@]}"; do echo "  - $f" >&2; done
    exit 1
fi
echo "  $n files verified."

# ── 2. Immutability: the destination prefix must not exist ───────────────
# `gcloud storage ls` exits nonzero BOTH when the prefix is empty and on
# auth/network trouble; only the recognized no-match error may proceed —
# anything else aborts rather than risking an overwrite decision on a
# failed listing.
echo "Checking that $dest_prefix/ is unclaimed ..."
ls_out=""
ls_rc=0
ls_out="$(gcloud storage ls "$dest_prefix/**" 2>&1)" || ls_rc=$?
if [ "$ls_rc" -eq 0 ] && [ -n "$ls_out" ]; then
    echo "ERROR: $dest_prefix/ already holds objects — snapshots are immutable." >&2
    echo "       A refresh mints a NEW snapshot id via a manifest change; it never" >&2
    echo "       rewrites an existing prefix. First objects found:" >&2
    printf '%s\n' "$ls_out" | head -5 | sed 's/^/         /' >&2
    exit 1
elif [ "$ls_rc" -ne 0 ]; then
    case "$ls_out" in
        *"matched no objects"*|*"One or more URLs matched no objects"*) ;;
        *)
            echo "ERROR: could not list $dest_prefix/ (gcloud exit $ls_rc):" >&2
            printf '%s\n' "$ls_out" | sed 's/^/         /' >&2
            echo "       Refusing to decide immutability from a failed listing." >&2
            exit 1
            ;;
    esac
fi
echo "  Prefix is unclaimed."

# ── 3. The plan ──────────────────────────────────────────────────────────
echo
echo "Snapshot upload plan ($n files + manifest):"
echo "  gcloud storage cp 'fixtures/psv/*.psv'        → $dest_prefix/psv/"
echo "  gcloud storage cp 'fixtures/psv-radar/*.psv'  → $dest_prefix/psv-radar/"
echo "  gcloud storage cp  fixtures/manifest.json     → $dest_prefix/manifest.json"
echo "  gcloud storage objects update --add-acl-grant=entity=AllUsers,role=READER '$dest_prefix/**'"
echo
if [ "$YES" -ne 1 ]; then
    echo "DRY RUN — nothing uploaded. Re-run with --yes to execute."
    exit 0
fi

# ── 4. Upload ────────────────────────────────────────────────────────────
# Per-directory glob copies upload exactly the .psv data files (the tracked
# per-directory READMEs stay in git, not in the snapshot); step 1 already
# proved the glob set == the manifest set.
echo "Uploading ..."
gcloud storage cp "$ROOT/fixtures/psv/"*.psv "$dest_prefix/psv/"
gcloud storage cp "$ROOT/fixtures/psv-radar/"*.psv "$dest_prefix/psv-radar/"
gcloud storage cp "$MANIFEST" "$dest_prefix/manifest.json"

# Public-read on the snapshot's objects only — reader CI and forks fetch by
# HTTPS with no credential. If the bucket enforces uniform bucket-level
# access this per-object grant fails; say so and stop rather than leaving
# the snapshot silently unreadable.
echo "Granting public read on $dest_prefix/** ..."
if ! gcloud storage objects update --add-acl-grant=entity=AllUsers,role=READER "$dest_prefix/**"; then
    echo "ERROR: per-object public-read grant failed." >&2
    echo "       If this bucket uses uniform bucket-level access, per-object ACLs are" >&2
    echo "       disabled; either re-enable fine-grained ACLs on $BUCKET, or grant" >&2
    echo "       bucket-wide public read (affects EVERYTHING in the bucket — decide" >&2
    echo "       deliberately):" >&2
    echo "         gcloud storage buckets add-iam-policy-binding $BUCKET \\" >&2
    echo "           --member=allUsers --role=roles/storage.objectViewer" >&2
    echo "       The snapshot objects are uploaded but NOT publicly readable until then." >&2
    exit 1
fi

# ── 5. Prove the public HTTPS read path end-to-end ───────────────────────
base_url="$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['base_url'])" "$MANIFEST")"
echo "Spot-checking anonymous HTTPS fetch ..."
if ! curl -fsSL "$base_url/$snapshot_id/manifest.json" | cmp -s - "$MANIFEST"; then
    echo "ERROR: anonymous fetch of $base_url/$snapshot_id/manifest.json did not" >&2
    echo "       round-trip the local manifest — the public read path is broken." >&2
    exit 1
fi
echo "  Anonymous HTTPS read OK."
echo
echo "Snapshot $snapshot_id is live. Full verification: run scripts/fetch-fixtures.sh"
echo "from a clean checkout (or move fixtures/psv* aside first) to fetch + verify all $n files."
