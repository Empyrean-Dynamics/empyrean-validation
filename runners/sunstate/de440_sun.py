"""The Sun's barycentric state from the registered DE440, for the OrbFit and OpenOrb runners.

Both tools work in heliocentric coordinates while the plan's initial conditions
are solar-system-barycentric, so each runner subtracts the Sun's barycentric
state at the epoch it converts. The plan the suite runs carries no Sun state, so
this helper reads it from the DE440 SPK the tool's folder of the third-party
tool set lists (``orbfit/de440.bsp``, ``oorb/de440.bsp``): the engine set's own
bytes, never a download and never a raw-barycentric fallback.

Dependency-free (``struct`` + ``math``), so each runner imports it under its own
interpreter:

    import sys; sys.path.insert(0, "<repo>/runners")
    from sunstate.de440_sun import SunEphemeris

    sun = SunEphemeris("<third-party>/orbfit/de440.bsp")
    pos_au, vel_au_d = sun.state(jd_tdb)

Reads are positional (``os.pread``), so one instance is safe to share between
threads. It reads exactly the NAIF DAF/SPK structures DE440 uses: the file record, the
summary records, and a type 2 (Chebyshev position) segment for target 10 (Sun)
relative to centre 0 (solar-system barycentre) in frame 1 (J2000, the ICRF
axes of DE440). Positions are evaluated from the Chebyshev series and velocities
from its derivative, the way SPICE's SPKE02 does. Reference: NAIF, "DAF Required
Reading" and "SPK Required Reading", segment type 2.
"""

from __future__ import annotations

import hashlib
import math
import os
import struct
from pathlib import Path

AU_KM = 149_597_870.7          # IAU 2012 Resolution B2
SECONDS_PER_DAY = 86_400.0
J2000_JD = 2_451_545.0
SUN, SSB, J2000_FRAME, CHEBYSHEV_POSITION = 10, 0, 1, 2
RECORD_BYTES = 1024


class SpkError(ValueError):
    """The file is not a DE SPK this helper can read, named with the reason."""


class SunEphemeris:
    """Sun-relative-to-SSB segments of one DE SPK, evaluated on demand."""

    def __init__(self, path: str | Path):
        self.path = Path(path)
        if not self.path.is_file():
            raise SpkError(f"{self.path}: no such file (the tool's third-party folder must list de440.bsp)")
        self._fd = os.open(self.path, os.O_RDONLY)
        head = os.pread(self._fd, RECORD_BYTES, 0)
        if head[:8] != b"DAF/SPK ":
            raise SpkError(f"{self.path}: not a DAF/SPK file (id {head[:8]!r})")
        fmt = head[88:96]
        if fmt == b"LTL-IEEE":
            self._e = "<"
        elif fmt == b"BIG-IEEE":
            self._e = ">"
        else:
            raise SpkError(f"{self.path}: unsupported binary format {fmt!r}")
        nd, ni = struct.unpack(self._e + "ii", head[8:16])
        if (nd, ni) != (2, 6):
            raise SpkError(f"{self.path}: ND/NI {nd}/{ni} is not an SPK's 2/6")
        fward = struct.unpack(self._e + "i", head[76:80])[0]
        self.segments = []  # (start_et, end_et, start_addr, end_addr)
        record = fward
        while record:
            buf = os.pread(self._fd, RECORD_BYTES, (record - 1) * RECORD_BYTES)
            nxt, _prev, nsum = struct.unpack(self._e + "3d", buf[:24])
            for k in range(int(nsum)):
                off = 24 + k * 40  # 2 doubles + 6 ints packed as 3 doubles = 40 bytes
                et0, et1 = struct.unpack(self._e + "2d", buf[off:off + 16])
                target, centre, frame, typ, a0, a1 = struct.unpack(self._e + "6i", buf[off + 16:off + 40])
                if (target, centre) == (SUN, SSB):
                    if frame != J2000_FRAME or typ != CHEBYSHEV_POSITION:
                        raise SpkError(
                            f"{self.path}: Sun/SSB segment is frame {frame}, type {typ}; "
                            "this helper reads frame 1, type 2 only")
                    self.segments.append((et0, et1, a0, a1))
            record = int(nxt)
        if not self.segments:
            raise SpkError(f"{self.path}: no Sun (10) relative to SSB (0) segment")

    def sha256(self) -> str:
        h = hashlib.sha256()
        with self.path.open("rb") as fh:
            for chunk in iter(lambda: fh.read(1 << 20), b""):
                h.update(chunk)
        return h.hexdigest()

    def _doubles(self, first_addr: int, count: int) -> tuple[float, ...]:
        raw = os.pread(self._fd, 8 * count, (first_addr - 1) * 8)
        return struct.unpack(f"{self._e}{count}d", raw)

    def state_km(self, et: float) -> tuple[list[float], list[float]]:
        """(km, km/s) of the Sun relative to the SSB at TDB seconds past J2000."""
        for et0, et1, a0, a1 in self.segments:
            if et0 <= et <= et1:
                break
        else:
            raise SpkError(f"{self.path}: epoch ET {et:.1f} s is outside every Sun/SSB segment")
        init, intlen, rsize, n = self._doubles(a1 - 3, 4)
        rsize, n = int(rsize), int(n)
        idx = min(int((et - init) // intlen), n - 1)
        rec = self._doubles(a0 + idx * rsize, rsize)
        mid, radius = rec[0], rec[1]
        ncoef = (rsize - 2) // 3
        x = (et - mid) / radius
        # Chebyshev T_k(x) and dT_k/dx by the standard recurrences.
        t = [1.0, x] + [0.0] * (ncoef - 2)
        dt = [0.0, 1.0] + [0.0] * (ncoef - 2)
        for k in range(2, ncoef):
            t[k] = 2.0 * x * t[k - 1] - t[k - 2]
            dt[k] = 2.0 * t[k - 1] + 2.0 * x * dt[k - 1] - dt[k - 2]
        pos, vel = [], []
        for axis in range(3):
            c = rec[2 + axis * ncoef: 2 + (axis + 1) * ncoef]
            pos.append(math.fsum(ck * tk for ck, tk in zip(c, t)))
            vel.append(math.fsum(ck * dk for ck, dk in zip(c, dt)) / radius)
        return pos, vel

    def state(self, jd_tdb: float) -> tuple[list[float], list[float]]:
        """(au, au/day) of the Sun relative to the SSB, ICRF axes, at a TDB Julian date."""
        et = (jd_tdb - J2000_JD) * SECONDS_PER_DAY
        pos_km, vel_kms = self.state_km(et)
        return ([p / AU_KM for p in pos_km], [v * SECONDS_PER_DAY / AU_KM for v in vel_kms])
