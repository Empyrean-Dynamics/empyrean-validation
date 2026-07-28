# Optical OD fixtures (ADES PSV)

One `<name>.psv` per catalog object (`src/catalog.rs`), carrying that object's
curated optical astrometry as an ADES 2017 pipe-separated-value table. These are
the inputs to every orbit-determination channel: the rust reference
(`validate od`), the python / C / CLI / core replays, and the external OD
references (find_orb, OrbFit, layup).

Object names containing `/` — the comets and the interstellars — are stored with
the slash rewritten to `_` (`2P_Encke.psv`, `103P_Hartley 2.psv`,
`1I_'Oumuamua.psv`) so the name is never read as a path separator.

`../psv-radar/` holds the radar-augmented variants for the five objects with
radar astrometry; their optical tables are byte-identical to the files here.

## Why these are tracked in git

They were `.gitignore`d ("carried out-of-band") for the life of the split-out
repo, which made the entire OD channel structurally dead in CI: no workflow step
staged them, the rust runner treated every missing fixture as a skip, and
`validation_rust_od.json` was an empty list in every artifact ever published.

The alternative — a checksummed fetch step in the `prep` job — was rejected
because the fixtures are not needed only by `prep`. The `external` matrix legs
(find_orb, OrbFit, layup) fit these same files, and those legs deliberately
carry **no secrets and no private checkout**: they run untrusted upstream tool
code and are isolated from both the `GH_PAT` and the GCP workload identity
(`id-token: write` is scoped to `reduce` alone). A fetch from any private
location would have to hand each of those legs a credential, dissolving the
security boundary the matrix was built around. Tracking is the only option that
delivers the fixtures to a secretless leg via the checkout it already does.

Cost, measured: 50 files, 42 MB in the working tree; **6.02 MiB of git objects
on disk** as written, deltaing down to **4.51 MiB** when packed. Everything else
in the repo packs to 1.72 MiB, so these fixtures are now the bulk of it — still
two orders of magnitude below any GitHub limit, and one refresh per quarter
keeps it there for years. `../psv-radar/` adds nothing on top: its optical
tables are byte-identical to these, so packing both together is still 4.51 MiB.

> **Correction.** The commit that staged these files (`23479ea`) reported
> "~4.5 MB compressed against a 6 MB pack". The first number is the packed
> figure and is right, but it is not what the commit put on disk — loose
> objects are zlib-only, with no delta compression, so the tree gained 6.02 MiB
> until something repacked it. The second number was simply wrong: the repo
> without these fixtures packs to 1.72 MiB, not 6 MB, so this is not a 75%
> addition to the repo, it is a tripling of it. The decision above stands on
> either figure; the record should not.
>
> Reproduce: `git ls-tree -r HEAD -- fixtures/psv | awk '{print $3}'`, then
> `git cat-file --batch-check='%(objectsize:disk)'` summed for the on-disk
> figure, or `git pack-objects --stdout > /dev/null` piped through `wc -c` for
> the packed one.

Redistribution is not in question: the astrometry is MPC-published optical
observation records (freely available for scientific use) and, in the radar
fixtures, JPL `sb_radar` delay/Doppler astrometry (US government work). This
repo already tracked `../psv-radar/`, whose optical tables are byte-identical to
the files here, so the precedent was set in-tree.

Reproducibility settles it: `Cargo.toml` pins `empyrean` and `hyperjet` exactly
so "a checkout of a validation tag reproduces the validation-of-record". Without
the fixtures in the tree that claim was false for every OD row.

## Refreshing

Observations churn — arcs extend, records are debiased, catalogs are re-reduced.
Re-fetch a fixture through the engine's own MPC/ADES client and commit the
result as its own revision; the diff is the observation delta and the tag is the
arc-of-record for that validation run. Fixtures are only meaningful alongside
the catalog entry that names them, so add or remove them in the same commit as
the corresponding `src/catalog.rs` change.

`make check-fixtures` asserts the set is present and non-empty; every OD target
depends on it, so a missing set fails with that message instead of silently
producing zero rows.
