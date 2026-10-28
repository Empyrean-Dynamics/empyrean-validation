# Vendored data assets

## `empyrean-tokens.css`

Design tokens referenced by the report renderer (`src/report.rs`).

## Observatory codes — no longer vendored

Until 2026-10-26 this directory carried `obscodes_extended.json.gz`, a July
2026 copy of the MPC observatory codes that CI wrote over the registered
kernel set's file. The copy differed from the registered file at two stations
the fixtures use (Z84 by about 38 m, B42 by about 300 m), enough to flip the
marginal Apophis optical+radar fit on one platform. The engine now reads the
registered set's
`obscodes_extended.json`, synced by `.github/actions/engine-setup`, the same
file every other empyrean repo's CI reads; refreshing it is a kernel-set
release, not a repository change.
