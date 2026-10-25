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

The `.psv` files here are **working copies, not tracked in git**: they are one
product of a single immutable inputs snapshot (optical + radar PSVs alongside
the SBDB and Horizons responses), pinned bit-exactly by the tracked
`../manifest.json` and materialized + sha256-verified by `scripts/inputs.py`
(`make fixtures`). See `../README.md` for the full contract — the four
products, the pin, the fetch, the CI roles, the upstream seal, and the refresh
procedure.

History, briefly, because this directory has now lived in all four states:

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
3. **Public-read GCS snapshot + tracked manifest.** Public-read objects let
   the secretless legs fetch over plain HTTPS, and the manifest kept the git
   SHA pinning bit-exact inputs.
4. **Unified inputs snapshot** (current). The PSVs are now one product of a
   single inputs snapshot — the SBDB and Horizons responses the suite used to
   query live, alongside the optical and radar PSVs — in the same public
   validation bucket, under `inputs/<snapshot_id>/`. Reads stay plain HTTPS
   with no credential; CI fetches once in `prep` and the fan-out legs restore
   from the actions cache. The fetch's always-verify-everything discipline, the
   hard-fail on any snapshot miss, and the upstream seal keep the "no hidden
   fallbacks" property. Tags from eras 2–3 still fetch (or carry) their
   fixtures as they did; history was not rewritten.
