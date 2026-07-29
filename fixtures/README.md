# Fixtures — the snapshot contract

The ADES PSV astrometry that the OD channels fit (`psv/`, `psv-radar/`) is
**not stored in git**. It lives in an immutable, content-addressed GCS
snapshot, and the tracked `manifest.json` in this directory pins it
bit-exactly — so a git SHA still identifies its exact validation inputs, and
the validation-of-record contract survives the data leaving the repository.

```
manifest.json      tracked   pins the snapshot: id, base URL, per-file
                             {path, sha256, bytes, data_rows}
psv/*.psv          fetched   optical OD fixtures     (see psv/README.md)
psv-radar/*.psv    fetched   radar-augmented variant (see psv-radar/README.md)
psv*/README.md     tracked   per-directory documentation
```

## Fetching

```sh
make fixtures            # or directly: scripts/fetch-fixtures.sh
```

Plain HTTPS via `curl` from

```
https://storage.googleapis.com/empyrean-validation/fixtures/<snapshot_id>/<path>
```

The objects are public-read — MPC astrometry is public data — so readers
(local checkouts, CI, forks, external-matrix legs with no secrets) need no
gcloud and no credential of any kind. `gcloud` is used only by the snapshot
*upload* (`scripts/upload-fixture-snapshot.sh`).

The fetch script verifies **every** file's sha256 against the manifest on
**every** invocation — cache hit or miss — re-fetches anything missing or
stale (atomic temp-file-then-rename, never a partial write), fails loudly
naming each offending path with its expected/actual hash, and additionally
rejects any `*.psv` on disk that the manifest does not list (the runners glob
these directories, so an unlisted file would be silently fitted). Nothing
downstream runs fits unless it exits 0. Every fixture-consuming Makefile
target depends on it.

## The manifest

- `snapshot_id` — `<UTC date>-<12 hex>`, where the hex is the leading 12
  characters of the sha256 over the UTF-8 bytes of one line per file,
  `"<path>\t<sha256>\n"`, sorted by `path`. Content-addressed: any change to
  any file (or to the set of files) produces a new id.
- `base_url` — `https://storage.googleapis.com/empyrean-validation/fixtures`.
- `files[]` — per file: `path` (relative to this directory), `sha256`,
  `bytes`, and `data_rows` (ADES PSV data lines: total lines minus
  `#`-prefixed comment/version lines minus the pipe-header line(s); the
  radar-augmented files carry two headers — optical table + `<radar>` table).

## Immutability and refresh

Snapshots are **never overwritten**. `scripts/upload-fixture-snapshot.sh`
refuses to run if anything already exists under its destination prefix, and
requires `--yes` after echoing its full plan.

A fixture refresh (new astrometry, new objects) mints a **new** snapshot:
regenerate the manifest from the updated working tree (new content → new
`snapshot_id`), upload the new prefix, and land the manifest change in a
reviewed PR. The PR diff *is* the data diff, hash by hash. Refresh tooling is
tracked as empyrean-svgl.

## Old releases

The fixtures were tracked in git until this contract landed. **History is
untouched**: every tag from the in-git era still carries its fixtures in the
checkout itself and needs no fetch step. Tags from this contract onward pin
their inputs through `manifest.json` instead.

## CI

Every fixture-consuming job restores an `actions/cache` entry keyed
`fixtures-<snapshot_id>`, then runs the fetch script **unconditionally** — on
a cache hit it is a pure verify, so a poisoned or truncated cache fails the
job instead of reaching a fit — and saves the cache only on a miss, after a
fully verified fetch. This mirrors the kernel-cache poison discipline in
`.github/workflows/validation.yml`.
