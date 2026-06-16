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
(7155 post-1972 MPC records; pre-1972 precovery is dropped because villeneuve's
UTC→TDB conversion requires post-1972 epochs).

The optical table is byte-identical to `../psv/<name>.psv`; the radar table is a
second ADES `<radar>` block (`trx`/`rcv`/`obsTime`/`delay`|`doppler`/`rms*`/`frq`/
`com`) under the same object designation. Delay values are in seconds, `rmsDelay`
in microseconds, Doppler/`rmsDoppler` in Hz, `frq` in MHz; JPL station codes are
mapped to MPC obs codes (−14→253 Goldstone DSS-14, −1→251 Arecibo, …).

## Why these are staged here, not in `../psv/`

The validation runner reads `fixtures/psv/<name>.psv`. The empyrean OD channel
currently builds against an **optical-only scott** whose PSV reader does not know
the radar `frq`/`com` columns and **hard-errors** on a radar table
(`ParseFloat … "rmsDoppler"`). So these live outside the runner's path until the
suite builds against a radar-capable scott. Each file is round-trip-verified
through the radar-branch scott parser (optical + radar counts above).

## Activating (the P6 "radar distribution exposure" step)

Once scott PR #63 (radar OD) is merged and empyrean-core / the wrapper / the C
ABI carry radar through `parse_ades` → `determine`, point the Rust runner's
`fixtures_dir` (`runners/rust/src/main.rs`) at `fixtures/psv-radar/` (or copy
these over `../psv/`). Then:

- the **empyrean channel** fits optical + radar (e.g. Apophis ~60× position
  tightening, 1576.8 km → 26.1 km 1σ), and
- **find_orb** — which already ingests ADES radar — cross-checks the
  radar-inclusive orbit as an external reference.

The validation `schema.rs` / `report.rs` residual model is RA/Dec-only today;
displaying radar fits will want new delay/Doppler residual columns.

## Regenerating / extending

Built from the scott radar branch (`feature/radar-observation-type`) via scott's
own I/O so the result round-trips by construction:

```
villeneuve::io::jpl::query_radar(["<des>"])   // live JPL sb_radar astrometry
  → RadarObservation::from_jpl_radar           // JPL → ADES-native (µs→s, station map)
  → ADESData{ block.radar = … }
  → write_psv_string                           // emit the <radar> table
  → append to the curated optical ../psv/<name>.psv
```

Add a candidate by running that pipeline for its designation against its optical
fixture (or with the optical path `-` for a radar-only file, as Toutatis was).
Candidates with substantial radar not yet seeded: 1950 DA (29075),
Geographos (1620).
