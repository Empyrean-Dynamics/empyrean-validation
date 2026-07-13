# Vendored data assets

## `obscodes_extended.json.gz`

MPC observatory codes (site name + parallax constants), used to place the
five observer stations in the validation catalog (`W84`, `F51`, `X05`,
`500`, `I41`).

**Vendored, not fetched.** The Minor Planet Center
(`minorplanetcenter.net`) throttles GitHub Actions IP ranges, so a live
download reliably times out in CI. The file is ~77 KB and the
observatories used are long-established (their coordinates do not move),
so pinning a copy both unblocks CI and makes the validation inputs
reproducible for a given release.

**Refresh** (only needed if a catalog observatory isn't in the file):

```sh
curl -sSfL -o assets/obscodes_extended.json.gz \
  https://minorplanetcenter.net/Extended_Files/obscodes_extended.json.gz
```

Source: <https://minorplanetcenter.net/Extended_Files/obscodes_extended.json.gz>
