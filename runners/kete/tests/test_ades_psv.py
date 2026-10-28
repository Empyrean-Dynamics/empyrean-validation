"""ades_psv: ADES PSV rows -> kete optical observations, with no network."""

import math
import pathlib
import sys

import pytest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import ades_psv

# Three real rows in ADES 2017 PSV layout: two from Kitt Peak (691) against
# USNO-B1, one from TESS (C57, a spacecraft with an ICRF_KM geocentric
# position) with reported sigmas.
PSV = """\
# version=2017
# observatory
! mpcCode 691
 permID|provID     |  trkSub|mode|stn |sys    | ctr|pos1       |pos2      |pos3      |obsTime                 | ra        |dec        |rmsRA|rmsDec|  astCat|mag  |band
  99942|2004 MN4   | K04M04N| CCD|691 |       |    |           |          |          |2004-03-15T02:35:21.696Z| 61.53367  |+16.91794  |     |      |  USNOB1|     |
  99942|2004 MN4   | K04M04N| CCD|691 |       |    |           |          |          |2004-03-15T02:38:57.696Z| 61.53575  |+16.91836  |     |      |  USNOB1|20.1 |  R
    588|           | LTNIECD| CCD|C57 |ICRF_KM| 399|-172793.449|242824.608|58275.018 |2021-09-13T01:13:58.384Z| 27.94205  |+23.80393  |0.99 |0.71  |   Gaia2|15.9 |  G
"""


def test_parse_reads_fields_and_leaves_empty_ones_out():
    recs = ades_psv.parse_psv(PSV)
    assert len(recs) == 3
    first = recs[0]
    assert first["stn"] == "691"
    assert first["obsTime"] == "2004-03-15T02:35:21.696Z"
    assert first["ra"] == "61.53367" and first["dec"] == "+16.91794"
    assert first["astCat"] == "USNOB1"
    assert "rmsRA" not in first and "sys" not in first and "mag" not in first
    tess = recs[2]
    assert (tess["sys"], tess["ctr"], tess["pos1"]) == ("ICRF_KM", "399", "-172793.449")


def test_parse_starts_a_new_block_at_a_repeated_header():
    text = PSV + "stn|obsTime|ra|dec\nF51|2020-01-01T00:00:00Z|10.0|-5.0\n"
    recs = ades_psv.parse_psv(text)
    assert len(recs) == 4
    assert recs[3] == {
        "stn": "F51",
        "obsTime": "2020-01-01T00:00:00Z",
        "ra": "10.0",
        "dec": "-5.0",
    }


def test_parse_refuses_malformed_rows():
    with pytest.raises(ValueError, match="before any header"):
        ades_psv.parse_psv("691|2004-03-15T02:35:21Z|1|2\n")
    with pytest.raises(ValueError, match="3 fields, header has 4"):
        ades_psv.parse_psv("stn|obsTime|ra|dec\n691|2004-03-15T02:35:21Z|1\n")


class _NoShift:
    """A debias table with no correction anywhere; records the lookups."""

    def __init__(self):
        self.calls = []

    def lookup(self, catalog, ra, dec, jd):
        self.calls.append(catalog)


class _Shift:
    def lookup(self, catalog, ra, dec, jd):
        return (1.8, -3.6) if catalog == "o" else None


def test_observations_carry_the_psv_position_time_and_station():
    kete = pytest.importorskip("kete")
    table = _NoShift()
    ing = ades_psv.to_observations(ades_psv.parse_psv(PSV), table)
    assert ing.n_records == 3 and not ing.dropped
    obs = ing.observations
    assert len(obs) == 3

    # RA/Dec in degrees, as written (no debias shift from this table).
    assert obs[0].ra == pytest.approx(61.53367, abs=1e-12)
    assert obs[0].dec == pytest.approx(16.91794, abs=1e-12)
    # USNO-B1 is looked up under its MPC code; Gaia DR2 is not debiased.
    assert table.calls == ["o", "o"]

    # obsTime is UTC; the observation epoch is TDB (TT-UTC = 64.184 s in 2004).
    t = kete.Time.from_iso("2004-03-15T02:35:21.696Z")
    assert obs[0].epoch.jd == pytest.approx(t.jd, abs=1e-9)
    assert (obs[0].epoch.jd - t.utc_jd) * 86400.0 == pytest.approx(64.184, abs=0.01)

    # Ground station: kete's own observatory table. kete keeps observers
    # Sun-centred, so both sides are re-centred on the SSB to compare.
    site = kete.spice.mpc_code_to_ecliptic("691", t.jd, center=0).as_equatorial
    got = obs[0].observer.change_center(0).as_equatorial.pos
    assert math.dist(list(got), list(site.pos)) < 1e-12

    # Spacecraft: Earth (ctr 399) plus the ADES ICRF_KM offset.
    t3 = kete.Time.from_iso("2021-09-13T01:13:58.384Z")
    earth = kete.spice.get_state(399, t3.jd, center=0).as_equatorial
    offset_au = [x / kete.constants.AU_KM for x in (-172793.449, 242824.608, 58275.018)]
    want = [e + d for e, d in zip(earth.pos, offset_au)]
    got = obs[2].observer.change_center(0).as_equatorial.pos
    assert math.dist(list(got), want) < 1e-9

    # Sigmas: reported rmsRA/rmsDec are used as given (sky-plane).
    assert obs[2].sigma_ra == pytest.approx(0.99) and obs[2].sigma_dec == pytest.approx(
        0.71
    )
    # Without them, kete's per-observatory table, times cos(Dec) for RA.
    s_ra, s_dec = kete.orbit_fitting.common.get_observatory_std("691")
    assert obs[0].sigma_ra == pytest.approx(s_ra * math.cos(math.radians(16.91794)))
    assert obs[0].sigma_dec == pytest.approx(s_dec)

    assert obs[1].band == "R" and obs[1].mag == pytest.approx(20.1)
    assert obs[0].band == "V" and math.isnan(obs[0].mag)


def test_catalog_debias_shift_is_applied_in_arcseconds():
    pytest.importorskip("kete")
    obs = ades_psv.to_observations(ades_psv.parse_psv(PSV), _Shift()).observations
    assert obs[0].ra == pytest.approx(61.53367 - 1.8 / 3600.0, abs=1e-12)
    assert obs[0].dec == pytest.approx(16.91794 + 3.6 / 3600.0, abs=1e-12)
    assert obs[2].ra == pytest.approx(27.94205, abs=1e-12)


def test_unusable_rows_are_counted_by_reason_not_dropped_silently():
    pytest.importorskip("kete")
    text = PSV + (
        "stn|obsTime|ra|dec\n"
        "ZZZ|2020-01-01T00:00:00Z|10.0|-5.0\n"
        "691|not-a-time|10.0|-5.0\n"
        "691|2020-01-01T00:00:00Z|10.0|\n"
    )
    ing = ades_psv.to_observations(ades_psv.parse_psv(text), _NoShift())
    assert ing.n_records == 6 and len(ing.observations) == 3
    assert ing.dropped == {
        "no observer state for stn ZZZ": 1,
        "unreadable obsTime": 1,
        "no RA/Dec": 1,
    }
