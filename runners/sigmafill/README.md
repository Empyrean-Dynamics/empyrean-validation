# Shared public sigma-fill (Vereš et al. 2017)

A standalone, dependency-light helper (`csv` + `math` only) that fills **missing**
per-observation astrometric uncertainties from one **public** station model, so an
external orbit-determination runner whose tool refuses observations without
reported sigmas can still fit the catalog as-shipped. Any runner imports it under
its own venv:

```python
import sys; sys.path.insert(0, "<repo>/runners")
from sigmafill.veres2017 import Veres2017SigmaModel, fill_missing

model = Veres2017SigmaModel.from_csv()
filled_ra, filled_dec, report = fill_missing(obs_codes, astcats, ra_sigma, dec_sigma, model)
```

Consumers: the `adam-core` runner (adam-core's public `CORE_OD_V1` profile has no
sigma-fill stage and requires finite, positive input variances) and the `layup`
runners (layup's `weight_data="supplied"` uses each observation's `rmsRA` /
`rmsDec` and gives a missing one its flat C++ default). Both fill from this one
model, which is why it lives here and not inside either runner.

## Discipline

- **Reported sigmas are never overwritten.** Only a missing (None / NaN / non-positive)
  sigma is filled. An observation with one of the two reported keeps it and has only
  the other filled; it counts under its fill level and in `partially_reported`.
- **Every filled observation is labelled** by fill level and the counts (`reported`,
  `filled`, `partially_reported`, `filled_by_level`, `total`, `table_sha256`) are
  carried on the OD row.
  No sigma value is ever silently defaulted, and there is no invented fallback
  number: the table holds only values the paper states.
- **The model is public and provenance-tracked**, never inline magic numbers: the
  table is `veres2017_sigma_table.csv`, whose header cites the paper (Icarus 296,
  139-149) and records the transcription re-check (latest-epoch values for the
  three epoch-dependent stations 703 / 691 / 644). This is **not** the proprietary
  Asteroid-Institute `v2_sigma_fill` table.

## The four fill levels and the paper behind each

Vereš, Farnocchia, Chesley & Chamberlin (2017), Icarus 296, 139-149; arXiv
1703.03479v2. Precedence is the order below.

| Level | Rule | Paper |
|---|---|---|
| `station` | the station's own sigma (13 stations) | Section 3, "Tables 2 and 3 give the weighting scheme ... For Catalina, Spacewatch and NEAT there is a time dependence that we account for (Table 2). The remaining surveys did not exhibit statistically significant changes over time (Table 3)." The latest epoch is used for 703 (0.8"), 691 (0.5"), 644 (0.4"). |
| `catalog` | 1.0" for a star catalog this table names | Section 3: "all the other CCD observations ... get a conservative weight at 1.00 [arcsec] if the star catalog is known"; Table 3 row "other w/ catalog 1.000". |
| `no_catalog` | 1.5" for an empty or `UNK` catalog | Section 3: "If the catalog is unknown, there could be regional biases as large as 1.00 (Farnocchia et al., 2015) and so observations are further deweighted at 1.500"; Table 3 row "other w/o catalog 1.500". |
| `catalog_unrecognized` | 1.5" for a non-empty catalog code this table does not name | **The paper is silent**: it distinguishes only "known" from "unknown" and gives no list. The conservative unknown-catalog weight is used and the observation is labelled, never folded into `catalog`. |

The 45 named catalogs are the ADES astCat vocabulary the suite's fixtures use; the
paper does not list catalog names. There is no `global` level: the 0.75" row of the
October transcription was adam-core's structural fallback, not the paper's, and was
removed.

## Fixture branch counts

Measured by `runners/adam-core/count_fill_branches.py` over the 50 optical fixtures
(space-based stations dropped first, as the OD rows do): 167,323 observations, of
which 46,291 carry a reported sigma and 121,032 are filled:

| Level | Observations |
|---|---|
| reported (untouched) | 46,291 |
| `station` | 16,178 |
| `catalog` | 89,607 |
| `no_catalog` | 15,247 |
| `catalog_unrecognized` | 0 |

No fixture observation names a catalog outside the table's vocabulary today, and
none carries only one of its two sigmas (`partially_reported` is 0 on every object);
both branches exist so an observation that needs one is labelled, not absorbed.

The layup runners keep space-based rows (layup places them from `sys` / `pos1..3`),
so over the same 50 fixtures they see 183,464 observations: 58,764 reported, and
124,700 filled (18,575 `station`, 90,539 `catalog`, 15,586 `no_catalog`, 0
`catalog_unrecognized`), measured through `run_layup._prepare_input`.

Per-object counts are recorded on every OD row (`adam_core_sigma_source`,
`layup_sigma_source`).
