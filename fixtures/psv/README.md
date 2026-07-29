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

## Where the data lives

The `.psv` files here are **working copies, not tracked in git**: the data
lives in an immutable public-read GCS snapshot, pinned bit-exactly by the
tracked `../manifest.json`, and materialized + sha256-verified by
`scripts/fetch-fixtures.sh` (`make fixtures`). See `../README.md` for the
full contract — immutability, refresh procedure, CI caching.

History, briefly, because this directory has now lived in all three states:

1. **`.gitignore`d** ("carried out-of-band") for the life of the split-out
   repo — which made the entire OD channel structurally dead in CI: no
   workflow step staged the fixtures, the rust runner treated every missing
   one as a skip, and `validation_rust_od.json` was an empty list in every
   artifact ever published.
2. **Tracked in git** as the fix. A checksummed fetch step was rejected at
   the time because the fixtures are not needed only by `prep`: the
   `external` matrix legs (find_orb, OrbFit, layup) fit these same files
   while deliberately carrying **no secrets and no private checkout**, and a
   fetch from any *private* location would have handed each of those legs a
   credential, dissolving the security boundary the matrix was built around.
3. **GCS snapshot + tracked manifest** (current). Public-read objects
   dissolve the objection to fetching: this is public MPC astrometry, so the
   secretless legs fetch over plain HTTPS with no credential at all, and the
   ~50 MB working set no longer dominates the repository's git objects. The
   fetch script's always-verify-everything discipline keeps the "no hidden
   fallbacks" property that tracking used to provide for free, and the
   manifest keeps the git SHA pinning bit-exact inputs. Tags from era 2
   still carry their fixtures in-checkout; history was not rewritten.
