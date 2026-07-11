# Radar-augmented OD fixtures (staging — not yet active)

Each `<name>.psv` here is the curated optical fixture from `../psv/<name>.psv`
with a `<radar>` table appended — JPL `sb_radar` delay/Doppler astrometry — so a
single file carries an object's combined optical + radar arc for an OD fit.

| object            | optical | radar (delay / Doppler) | stations                  |
|-------------------|--------:|------------------------:|---------------------------|
| Apophis (99942)   |    9520 |          50 (20 / 30)   | Goldstone DSS-14, Arecibo |
| Bennu (101955)    |     603 |          29 (22 / 7)    | Goldstone DSS-14, Arecibo |
| Didymos (65803)   |    6034 |           9 (9 / 0)     | Goldstone, Arecibo        |
| Eros (433)        |   13150 |           6 (4 / 2)     | Goldstone, Arecibo        |
| Toutatis (4179) † |    7155 |          62 (35 / 27)   | DSS-14, DSS-13, Arecibo   |

† **Toutatis carries the only bistatic geometry** in the set: 11 of its 62 records
are a Goldstone DSS-14 → DSS-13 transmit/receive pair (`trx=253`, `rcv=252`;
`trx != rcv`), independently verified 1:1 against the live JPL API. It was
originally radar-only; it is now a fittable catalog object — `src/catalog.rs`
carries a `Toutatis` NEO entry and `../psv/Toutatis.psv` holds its optical arc
(7155 post-1972 MPC records; pre-1972 precovery is dropped because the engine's
UTC→TDB conversion requires post-1972 epochs).

The optical table is byte-identical to `../psv/<name>.psv`; the radar table is a
second ADES `<radar>` block (`trx`/`rcv`/`obsTime`/`delay`|`doppler`/`rms*`/`frq`/
`com`) under the same object designation. Delay values are in seconds, `rmsDelay`
in microseconds, Doppler/`rmsDoppler` in Hz, `frq` in MHz; JPL station codes are
mapped to MPC obs codes (−14→253 Goldstone DSS-14, −1→251 Arecibo, …).

## Why these are staged here, not in `../psv/`

The validation runner reads `fixtures/psv/<name>.psv`, so the OD comparison
matrix is optical-only today. The distribution itself already carries radar
end-to-end (`read_ades` → `determine` — guarded by
`runners/rust/tests/radar_regression.rs`, which fits the Apophis
optical+radar arc through the wrapper); these fixtures are staged until the
OD comparison rows and report columns are extended to radar residuals.

## Activating

Point the Rust runner's `fixtures_dir` (`runners/rust/src/main.rs`) at
`fixtures/psv-radar/` (or copy these over `../psv/`). Then:

- the **empyrean channel** fits optical + radar (e.g. Apophis ~60× position
  tightening, 1576.8 km → 26.1 km 1σ), and
- **find_orb** — which already ingests ADES radar — cross-checks the
  radar-inclusive orbit as an external reference.

The validation `schema.rs` / `report.rs` residual model is RA/Dec-only today;
displaying radar fits will want new delay/Doppler residual columns.

## Regenerating / extending

Built through the engine's own JPL `sb_radar` client and ADES writer, so the
result round-trips by construction:

```
JPL sb_radar query for "<des>"       # live delay/Doppler astrometry
  → JPL → ADES-native conversion     # µs→s, station-code map
  → emit the <radar> PSV table
  → append to the curated optical ../psv/<name>.psv
```

Add a candidate by running that pipeline for its designation against its optical
fixture (or with the optical path `-` for a radar-only file, as Toutatis was).
Candidates with substantial radar not yet seeded: 1950 DA (29075),
Geographos (1620).
