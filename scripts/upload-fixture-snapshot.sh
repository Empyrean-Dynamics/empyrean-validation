#!/usr/bin/env bash
# Mint the fixture snapshot pinned by fixtures/manifest.json into GCS.
#
# Uploads every manifest-listed file, plus the manifest itself, to
#   gs://empyrean-validation/fixtures/<snapshot_id>/...
# and ensures those objects are public-read, so readers (CI legs, forks,
# contributors) fetch over plain HTTPS with no auth (the data is public MPC
# astrometry). gcloud is needed HERE only — never by the fetch path.
#
# "Ensures" rather than "grants", because the two GCS access-control modes
# demand opposite actions and the script must not assume either: with uniform
# bucket-level access OFF it GRANTS a per-object ACL; with UBLA ON per-object
# ACLs are inert, so it instead REQUIRES a pre-existing bucket-wide allUsers
# object-read binding and refuses to upload without one. Either way the last
# word is an anonymous HTTPS read of real objects (step 7), never an assumption.
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

# Same digest over stdin — used to hash a downloaded object without staging
# it to disk during the post-upload read-path proof.
sha256_stdin() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum | awk '{print $1}'
    else
        shasum -a 256 | awk '{print $1}'
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

# ── 3. Decide how the snapshot becomes public-read — BEFORE uploading ────
# Readers (CI legs, forks, contributors) fetch by anonymous HTTPS, so the
# objects MUST end up world-readable. How that is granted depends on the
# bucket's access-control mode, and the two modes are mutually exclusive:
#
#   uniform bucket-level access OFF → per-object ACLs work; grant AllUsers
#                                     READER on this prefix only.
#   uniform bucket-level access ON  → per-object ACLs are DISABLED; object
#                                     visibility comes solely from bucket
#                                     IAM, so a bucket-wide `allUsers`
#                                     object-read binding must already exist.
#
# This is settled here, before a single byte moves. Discovering it after the
# upload would strand the operator: the objects would be live, unreadable or
# not, and every retry would hit the immutability guard above.
echo "Resolving the public-read path for $BUCKET ..."

bucket_meta=""
if ! bucket_meta="$(gcloud storage buckets describe "$BUCKET" \
        --format="value(uniform_bucket_level_access,public_access_prevention)" 2>&1)"; then
    echo "ERROR: could not describe $BUCKET — refusing to upload without knowing" >&2
    echo "       whether the snapshot can be made publicly readable:" >&2
    printf '%s\n' "$bucket_meta" | sed 's/^/         /' >&2
    exit 1
fi
ubla="$(printf '%s\n' "$bucket_meta" | awk -F'\t' '{print tolower($1)}')"
pap="$(printf '%s\n' "$bucket_meta" | awk -F'\t' '{print tolower($2)}')"

# Public access prevention outranks both modes: enforced means no anonymous
# read is possible at all, so the whole contract is unsatisfiable.
if [ "$pap" = "enforced" ]; then
    echo "ERROR: $BUCKET has public_access_prevention=enforced — anonymous reads are" >&2
    echo "       impossible, so the fetch path (curl, no credentials) cannot work." >&2
    echo "       Uploading would produce a snapshot nobody but this project can read." >&2
    exit 1
fi

case "$ubla" in
    true)
        # Per-object ACLs are inert under UBLA. Require the bucket-wide grant
        # to already be in place; do NOT add it here — it would widen access
        # to every unrelated object in the bucket (validation reports under
        # latest/ and archive/), which is the operator's call, not a side
        # effect of minting a fixture snapshot.
        iam_json=""
        if ! iam_json="$(gcloud storage buckets get-iam-policy "$BUCKET" --format=json 2>&1)"; then
            echo "ERROR: $BUCKET uses uniform bucket-level access, but its IAM policy could" >&2
            echo "       not be read — public readability is undetermined:" >&2
            printf '%s\n' "$iam_json" | sed 's/^/         /' >&2
            exit 1
        fi
        public_role=""
        public_role="$(printf '%s' "$iam_json" | python3 -c '
import json, sys
READ_ROLES = {
    "roles/storage.objectViewer",
    "roles/storage.objectUser",
    "roles/storage.legacyObjectReader",
    "roles/storage.admin",
}
try:
    policy = json.load(sys.stdin)
except Exception as exc:            # malformed policy → say so, grant nothing
    print("PARSE_ERROR: %s" % exc)
    raise SystemExit(0)
for binding in policy.get("bindings", []):
    if "allUsers" in binding.get("members", []) and binding.get("role") in READ_ROLES:
        print(binding["role"])
        break
')"
        case "$public_role" in
            PARSE_ERROR*)
                echo "ERROR: could not parse the IAM policy of $BUCKET ($public_role)." >&2
                echo "       Refusing to upload on an undetermined public-read path." >&2
                exit 1
                ;;
            "")
                echo "ERROR: $BUCKET uses uniform bucket-level access (per-object ACLs are" >&2
                echo "       disabled), and no bucket-wide allUsers object-read binding exists." >&2
                echo "       The snapshot would upload but be unreadable by the fetch path." >&2
                echo "       Nothing has been uploaded." >&2
                echo "       Grant bucket-wide public read — note this exposes EVERYTHING in" >&2
                echo "       the bucket, so decide deliberately:" >&2
                echo "         gcloud storage buckets add-iam-policy-binding $BUCKET \\" >&2
                echo "           --member=allUsers --role=roles/storage.objectViewer" >&2
                echo "       Or disable uniform bucket-level access to re-enable per-object ACLs." >&2
                exit 1
                ;;
        esac
        grant_mode="bucket-iam"
        echo "  Uniform bucket-level access is ON; allUsers already holds $public_role."
        echo "  Uploaded objects are public on arrival — no per-object grant needed."
        ;;
    *)
        grant_mode="object-acl"
        echo "  Uniform bucket-level access is OFF; per-object ACLs will grant public read."
        ;;
esac

# ── 4. The plan ──────────────────────────────────────────────────────────
echo
echo "Snapshot upload plan ($n files + manifest):"
echo "  gcloud storage cp 'fixtures/psv/*.psv'        → $dest_prefix/psv/"
echo "  gcloud storage cp 'fixtures/psv-radar/*.psv'  → $dest_prefix/psv-radar/"
echo "  gcloud storage cp  fixtures/manifest.json     → $dest_prefix/manifest.json"
if [ "$grant_mode" = "object-acl" ]; then
    echo "  gcloud storage objects update --add-acl-grant=entity=AllUsers,role=READER '$dest_prefix/**'"
else
    echo "  (no ACL step — bucket IAM already grants allUsers $public_role)"
fi
echo "  curl (anonymous) → manifest.json + one psv/ object + one psv-radar/ object, hash-checked"
echo
if [ "$YES" -ne 1 ]; then
    echo "DRY RUN — nothing uploaded. Re-run with --yes to execute."
    exit 0
fi

# ── 5. Upload ────────────────────────────────────────────────────────────
# Per-directory glob copies upload exactly the .psv data files (the tracked
# per-directory READMEs stay in git, not in the snapshot); step 1 already
# proved the glob set == the manifest set.
#
# If a copy dies partway the prefix is left non-empty, and step 2 will refuse
# every retry — by design, since a resumed upload cannot prove it produced
# the same snapshot. Recovery is deliberate and manual: delete the partial
# prefix, then re-run. The command is printed on failure rather than run
# automatically, because deleting under a snapshot prefix is exactly the
# operation immutability exists to prevent.
upload_failed() {
    echo "ERROR: upload failed partway — $dest_prefix/ may now hold a partial snapshot." >&2
    echo "       Snapshots are all-or-nothing; step 2 will refuse to retry over a" >&2
    echo "       non-empty prefix. To recover, delete the partial prefix and re-run:" >&2
    echo "         gcloud storage rm --recursive '$dest_prefix/'" >&2
    echo "         $0 --yes" >&2
    echo "       (This is the ONLY sanctioned delete under fixtures/ — it removes a" >&2
    echo "       snapshot that was never completed, never one that readers may have.)" >&2
    exit 1
}
echo "Uploading ..."
gcloud storage cp "$ROOT/fixtures/psv/"*.psv "$dest_prefix/psv/" || upload_failed
gcloud storage cp "$ROOT/fixtures/psv-radar/"*.psv "$dest_prefix/psv-radar/" || upload_failed
gcloud storage cp "$MANIFEST" "$dest_prefix/manifest.json" || upload_failed

# ── 6. Public read ───────────────────────────────────────────────────────
# Only meaningful when per-object ACLs are live; under UBLA step 3 already
# proved the bucket-wide allUsers binding, and step 7 proves it end-to-end.
if [ "$grant_mode" = "object-acl" ]; then
    echo "Granting public read on $dest_prefix/** ..."
    if ! gcloud storage objects update --add-acl-grant=entity=AllUsers,role=READER "$dest_prefix/**"; then
        echo "ERROR: per-object public-read grant failed on a bucket that step 3 saw as" >&2
        echo "       ACL-enabled — its access mode may have changed mid-run." >&2
        echo "       The snapshot objects are uploaded but NOT publicly readable." >&2
        echo "       Re-run the grant once the bucket's access mode is settled:" >&2
        echo "         gcloud storage objects update --add-acl-grant=entity=AllUsers,role=READER '$dest_prefix/**'" >&2
        exit 1
    fi
else
    echo "Public read comes from bucket IAM (allUsers $public_role) — no ACL step."
fi

# ── 7. Prove the public HTTPS read path end-to-end ───────────────────────
# Anonymous, credential-free, exactly as a fork's CI would read it. Checked
# on one object from EACH upload invocation — manifest.json, a psv/ file and
# a psv-radar/ file were each copied by a separate `gcloud storage cp`, so
# manifest.json alone would prove only that one of the three landed readable.
# The .psv samples are hash-checked against the manifest, which also exercises
# the URL-encoding of names carrying spaces and apostrophes.
base_url="$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['base_url'])" "$MANIFEST")"
echo "Spot-checking anonymous HTTPS reads ..."
if ! curl -fsSL "$base_url/$snapshot_id/manifest.json" | cmp -s - "$MANIFEST"; then
    echo "ERROR: anonymous fetch of $base_url/$snapshot_id/manifest.json did not" >&2
    echo "       round-trip the local manifest — the public read path is broken." >&2
    exit 1
fi
echo "  manifest.json OK."

# One sample per fixture directory: path, sha256 and URL-encoded path.
samples="$(python3 - "$MANIFEST" <<'PY'
import json, sys, urllib.parse
files = json.load(open(sys.argv[1]))["files"]
for prefix in ("psv/", "psv-radar/"):
    for f in files:
        if f["path"].startswith(prefix):
            print("\t".join([f["path"], f["sha256"], urllib.parse.quote(f["path"])]))
            break
PY
)"
while IFS=$'\t' read -r s_path s_sha s_enc; do
    [ -n "$s_path" ] || continue
    got="$(curl -fsSL "$base_url/$snapshot_id/$s_enc" </dev/null | sha256_stdin)" || {
        echo "ERROR: anonymous fetch of $s_path failed — the public read path is broken" >&2
        echo "       for the fixture objects themselves (manifest.json alone read fine)." >&2
        exit 1
    }
    if [ "$got" != "$s_sha" ]; then
        echo "ERROR: anonymous fetch of $s_path hashed to $got, manifest pins $s_sha." >&2
        exit 1
    fi
    echo "  $s_path OK."
done <<<"$samples"

echo
echo "Snapshot $snapshot_id is live. Full verification: run scripts/fetch-fixtures.sh"
echo "from a clean checkout (or move fixtures/psv* aside first) to fetch + verify all $n files."
