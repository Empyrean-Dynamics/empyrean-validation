# Validation inputs — the snapshot contract

Every JPL and MPC product the suite reads is **not stored in git**. All of it —
SBDB records, Horizons state vectors and ephemerides, and the ADES PSV
astrometry the OD channels fit — lives in one immutable, content-addressed
snapshot in the suite's own public-read bucket (`gs://empyrean-validation`,
under `inputs/<snapshot_id>/`, beside the published `latest/` and `archive/`
reports), and the tracked `manifest.json` in this directory pins it bit-exactly.
So a git SHA still identifies the exact validation inputs, and the
validation-of-record contract survives the data leaving the repository.

The snapshot is minted once by a manual action and consumed offline: the
validation workflow never queries JPL or the MPC. SBDB solutions move, and with
them the Horizons epochs the whole plan hangs on, so pinning the responses is
what makes a run reproducible rather than merely repeatable.

## The four products

```
sbdb/<query>.json          one SBDB response per catalog object (orbit,
                           covariance, non-grav + physical parameters)
horizons/<file>.json       Horizons state vectors (the IC at the SBDB epoch,
                           one per dt of each object's grid, and the Sun's
                           barycentric state at every target epoch) and
                           Horizons observer ephemerides (RA/Dec, light-time
                           corrected) per observer per dt
psv/<name>.psv             optical OD astrometry, ADES 2017 PSV
psv-radar/<name>.psv       optical + <radar> table for the radar objects
MANIFEST.json              per-file manifest (fetched beside the data; not
                           tracked), verified against the pin before any file
                           is trusted
```

`sbdb/` and `horizons/` files are the engine's own disk-cache files, named by
the engine's sanitized lookup key; the suite treats them as opaque bytes. The
plan builder, the reference runner, the ASSIST runner and `merge-external` all
read these four directories and nothing else — `CACHE_DIR` defaults to this
`fixtures/` tree, so a developer's `~/.empyrean/cache` is no longer consulted.
`fixtures/windows.json` (the local, never-regenerated walk manifest) and these
READMEs are not part of the snapshot and are never touched by the fetch.

## Fetching

```sh
make fixtures                  # python3 scripts/inputs.py fetch --pin … --prune
make fixtures INPUTS_OFFLINE=1 # verify the on-disk snapshot without the bucket
```

`scripts/inputs.py fetch` materializes whatever is missing or stale (atomic
temp-file-then-rename, never a partial write), verifies **every** file's sha256
against the manifest on **every** invocation — cache hit or miss — prunes any
file inside the four product directories that the manifest does not list, and
exits nonzero naming each offending path with its expected and actual hash.
Nothing downstream runs a fit or a fetch unless it exits 0. `INPUTS_OFFLINE=1`
does the same verification pass without touching the network: it is the CI
reader path, where a missing or stale file is a hard failure.

The bucket is public-read — this is public MPC astrometry and public JPL data —
so readers fetch over plain HTTPS with no credential of any kind
(`scripts/inputs.py fetch` uses `urllib`, never `gcloud`). Only the mint
*writes* the snapshot, with the suite's existing CI identity; the upload is the
one operation that uses `gcloud storage`.

## The pin — `manifest.json`

```json
{"schema": 1, "snapshot_id": "<UTC date>-<12 hex>",
 "bucket": "gs://empyrean-validation/inputs", "tree_hash": "<64 hex>",
 "files_total": 0, "products": {"psv": 0, "psv-radar": 0, "horizons": 0, "sbdb": 0},
 "partial": false, "minted_utc": "<iso>", "minted_by": "<mint run url>"}
```

- `snapshot_id` — `<UTC date>-<12 hex>`, the hex being the leading 12 characters
  of the sha256 over one line per file, `"<path>\t<sha256>\n"`, sorted by path.
  Content-addressed: any change to any file, or to the set of files, is a new
  id.
- `tree_hash` — the full 64-hex digest of those same lines. Before any file is
  trusted, the fetch recomputes it from the downloaded per-file manifest and
  requires it to equal the manifest's own `tree_hash` and the pin's.
- `bucket`, `files_total`, per-product counts, `minted_utc`, `minted_by` — the
  small, reviewable summary; the per-file manifest lives beside the data in the
  bucket.

A placeholder pin (`snapshot_id: "PENDING-FIRST-MINT"`, empty `tree_hash`) is
refused loudly by the fetch, which is the intended state until the first mint
fills it in.

## Roles in CI

The inputs snapshot follows the same cache discipline as the kernel set:

- **prep** is the sole fetcher: it reads the pinned snapshot once over plain
  HTTPS (no credential) and saves it to the actions cache under
  `inputs-<snapshot_id>`, so the fan-out legs do not each hit the bucket.
- **every other job** restores that exact key and refuses to run on a miss,
  with no credential, then re-verifies the snapshot offline. The external
  comparator legs therefore keep carrying no secret and no identity.

## Only ever from there — the seal

The suite reading its inputs only from the snapshot is enforced, not assumed.
Immediately before its run step, every suite job points the JPL and MPC hosts
(`ssd.jpl.nasa.gov`, `ssd-api.jpl.nasa.gov`, `data.minorplanetcenter.net`,
`minorplanetcenter.net`, `www.minorplanetcenter.net`) at `127.0.0.1` and proves
the override took (`scripts/seal-upstream.sh`). A client that would have reached
one of these then fails at the socket, naming the host, instead of silently
fetching an input the git SHA does not pin. Any snapshot miss in the plan
builder, the reference runner, `merge-external` or the ASSIST runner is an
error that fails the leg — naming the object, the product and the lookup key —
not a skip that quietly shrinks the reference. Publishing the report to Google
Cloud Storage is deliberately not sealed.

## Immutability and refresh

Snapshots are **never overwritten**: a `snapshot_id` is content-addressed, and
the mint refuses to upload into a prefix that already holds anything, so a
refresh always writes a new prefix and an existing one is left untouched.

A refresh (moved SBDB solutions, new objects, refreshed astrometry) mints a
**new** snapshot and adopts it in a reviewed PR:

1. Dispatch the manual "Mint validation inputs" action on `main`.
2. Read its summary artifact — per-product counts, SBDB epoch moves, PSV row
   deltas against the pinned snapshot.
3. Open the bump PR: it changes only this `manifest.json`, with the mint's
   `inputs.py diff <pinned> <new>` output in the description. The PR's own
   validation run is the acceptance test.
4. Merge; the next full run on `main` publishes the report built on the new
   snapshot.

Each new snapshot is a new content-addressed id, so the PR diff *is* the data
diff. Old snapshots stay in the bucket as history; a tag pins its snapshot.

## Old releases

The inputs were tracked in git, then carried in a public bucket, before this
contract. **History is untouched**: tags from the in-git era carry their
fixtures in the checkout, and tags from the public-bucket era fetch from it.
Tags from this contract onward pin their inputs through `manifest.json`.
