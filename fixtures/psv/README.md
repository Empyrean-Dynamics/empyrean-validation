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
on disk** as written, deltaing down to **4.51 MiB** when packed. A full pack of
HEAD with all history is **5.20 MiB**, so these fixtures are the bulk of the
repository — still two orders of magnitude below any GitHub limit, and one
refresh per quarter keeps it there for years. `../psv-radar/` adds nothing on
top: its optical tables are byte-identical to these, so packing both together
is still 4.51 MiB.

> **Correction.** The commit that staged these files (`23479ea`) reported
> "~4.5 MB compressed against a 6 MB pack". The first number is the packed
> figure and is right, but it is not what the commit put on disk — loose
> objects are zlib-only, with no delta compression, so the tree gained 6.02 MiB
> until something repacked it. The "6 MB pack" it was compared against was
> wrong: the whole repository at HEAD packs to 5.20 MiB *including* these
> fixtures, so the fixtures are not a fraction of the repo, they are most of
> it. The decision above stands on any of these figures; the record should not.
>
> A figure for "the repo without the fixtures" is deliberately not quoted here:
> it varies by half a megabyte depending on whether you pack HEAD or `--all`
> and on how the excluded object set is built, and two people measuring it got
> two different answers. The two numbers above are the ones that reproduce.
>
> Reproduce (each command is standalone; the pack ones need the `git rev-parse`
> upstream, since `pack-objects` reads revisions from stdin):
>
> ```sh
> # on-disk size of the fixture blobs (6.02 MiB)
> git ls-tree -r HEAD -- fixtures/psv | awk '{print $3}' \
>   | git cat-file --batch-check='%(objectsize:disk)' \
>   | awk '{s+=$1} END {printf "%.2f MiB\n", s/1048576}'
>
> # full pack of HEAD with history (5.20 MiB)
> git rev-parse HEAD | git pack-objects --stdout --revs \
>   | wc -c | awk '{printf "%.2f MiB\n", $1/1048576}'
> ```
