"""Put the python channel runner (``runners/python/run.py``) on ``sys.path`` so
the tests can ``import run`` directly, independent of the CWD pytest runs from.
"""

from __future__ import annotations

import sys
from pathlib import Path

_RUNNER_DIR = Path(__file__).resolve().parent.parent
if str(_RUNNER_DIR) not in sys.path:
    sys.path.insert(0, str(_RUNNER_DIR))
