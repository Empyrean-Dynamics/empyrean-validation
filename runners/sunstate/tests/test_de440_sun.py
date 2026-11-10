"""The DE440 Sun-state helper: SPICE's numbers, and loud refusals.

    <any venv with pytest>/bin/python -m pytest runners/sunstate/tests -q

The SPICE values were read with spiceypy (``spkezr('10', et, 'J2000', 'NONE',
'0')``) from the same de440.bsp; the helper must reproduce them to rounding.
DE440_BSP names the file (the orbfit/ or oorb/ folder of the third-party set);
without it only the refusal tests run.
"""

import os
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from sunstate.de440_sun import SpkError, SunEphemeris  # noqa: E402

DE440 = os.environ.get("DE440_BSP")
# (jd_tdb, km, km/s) from SPICE.
SPICE = [
    (2451545.0, (-1067706.8053809535, -396036.18479594623, -138065.18428688092),
     (0.009312571926520472, -0.01170150612817771, -0.005251266205200356)),
    (2461000.5, (-503220.89542956353, -767539.4370043159, -310387.7724509639),
     (0.01261855383248632, -0.00025997737603385807, -0.0003716674648710343)),
]


@pytest.mark.skipif(not DE440, reason="set DE440_BSP to a de440.bsp")
@pytest.mark.parametrize("jd, pos, vel", SPICE)
def test_reproduces_spice_at_the_epoch(jd, pos, vel):
    s = SunEphemeris(DE440)
    p, v = s.state_km((jd - 2451545.0) * 86400.0)
    assert max(abs(a - b) for a, b in zip(p, pos)) < 1e-6   # km: below a millimetre
    assert max(abs(a - b) for a, b in zip(v, vel)) < 1e-12  # km/s


@pytest.mark.skipif(not DE440, reason="set DE440_BSP to a de440.bsp")
def test_au_and_au_per_day_units():
    s = SunEphemeris(DE440)
    jd, pos, vel = SPICE[0]
    p, v = s.state(jd)
    assert abs(p[0] - pos[0] / 149_597_870.7) < 1e-15
    assert abs(v[1] - vel[1] * 86400.0 / 149_597_870.7) < 1e-18


@pytest.mark.skipif(not DE440, reason="set DE440_BSP to a de440.bsp")
def test_an_epoch_outside_the_ephemeris_is_refused():
    s = SunEphemeris(DE440)
    with pytest.raises(SpkError, match="outside"):
        s.state(1_000_000.5)  # 1975 BC


def test_a_missing_file_is_refused_by_name(tmp_path):
    with pytest.raises(SpkError, match="no such file"):
        SunEphemeris(tmp_path / "de440.bsp")


def test_a_file_that_is_not_an_spk_is_refused(tmp_path):
    f = tmp_path / "x.bsp"
    f.write_bytes(b"NAIF/DAF" + b"\0" * 2048)
    with pytest.raises(SpkError, match="not a DAF/SPK"):
        SunEphemeris(f)


@pytest.mark.skipif(not DE440, reason="set DE440_BSP to a de440.bsp")
def test_one_instance_is_safe_to_share_between_threads():
    from concurrent.futures import ThreadPoolExecutor

    s = SunEphemeris(DE440)
    jds = [2440000.5 + 37.3 * k for k in range(400)]
    serial = [s.state(jd) for jd in jds]
    with ThreadPoolExecutor(max_workers=8) as ex:
        threaded = list(ex.map(s.state, jds))
    assert threaded == serial
