"""Put the cli channel driver (``runners/cli/drive.py``) on ``sys.path`` so the
tests can ``import drive`` directly, independent of the CWD pytest runs from.

Mirrors ``runners/python/tests/conftest.py``. ``drive.py`` is pure stdlib (it
shells out to the built binary rather than importing the engine), so importing
it is offline and needs no wheel installed.
"""

from __future__ import annotations

import sys
from pathlib import Path

_RUNNER_DIR = Path(__file__).resolve().parent.parent
if str(_RUNNER_DIR) not in sys.path:
    sys.path.insert(0, str(_RUNNER_DIR))
