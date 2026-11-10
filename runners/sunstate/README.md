# Shared DE440 Sun state

A standalone, dependency-free helper (`struct` + `math` only) that returns the
Sun's position and velocity relative to the solar-system barycentre from the
registered DE440 SPK. The OrbFit and OpenOrb runners work in heliocentric
coordinates while the plan's initial conditions are barycentric, so each
subtracts the Sun's state at the epoch it converts. The plan the suite runs
carries no Sun state, and a runner must never fall back to a raw barycentric
state, so both read it here from the `de440.bsp` their own folder of the
third-party tool set lists (`orbfit/de440.bsp`, `oorb/de440.bsp`): the engine
set's bytes, never a download.

```python
import sys; sys.path.insert(0, "<repo>/runners")
from sunstate.de440_sun import SunEphemeris

sun = SunEphemeris("<third-party>/orbfit/de440.bsp")
pos_au, vel_au_d = sun.state(jd_tdb)      # ICRF axes, au and au/day
```

## What it reads

Exactly the NAIF structures DE440 uses (NAIF, "DAF Required Reading" and "SPK
Required Reading"): the file record, the summary records, and the type 2
(Chebyshev position) segment for target 10 (Sun) relative to centre 0
(solar-system barycentre) in frame 1 (J2000, the ICRF axes of DE440). Positions
come from the Chebyshev series, velocities from its derivative, as SPICE's
SPKE02 evaluates them. Any other layout (another frame, segment type, an epoch
outside the file, a file that is not a DAF/SPK) is refused by name.

## Agreement with SPICE

Measured against `spiceypy.spkezr('10', et, 'J2000', 'NONE', '0')` on the same
file at six epochs from 1858 to 2133: positions identical to the last bit and
velocities within 2e-21 au/day. The tests pin two of those SPICE states.

```sh
DE440_BSP=<third-party>/orbfit/de440.bsp <python with pytest> -m pytest runners/sunstate/tests -q
```
