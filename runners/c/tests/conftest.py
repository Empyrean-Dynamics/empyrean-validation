"""Put the c channel driver (``runners/c/drive.py``) on ``sys.path`` so the
tests can ``import drive`` directly, independent of the CWD pytest runs from,
AND build the C runner binary where ``drive.py`` expects it
(``runners/c/runner``) so the end-to-end tests exercise the real binary rather
than skipping.

``drive.py`` is pure stdlib (it shells out to the built binary rather than
importing the engine), so importing it is offline and needs no wheel installed.
The runner build is driven through the channel's own ``Makefile``; the engine
checkout comes from ``$EMPYREAN_ROOT`` (the top-level Makefile's override, which
the validation gate sets), falling back to the C Makefile's default sibling. If
the build cannot run (no compiler, no engine checkout) the binary is simply left
absent and the end-to-end tests ``skipif`` on it — the offline unit tests still
run.
"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

_RUNNER_DIR = Path(__file__).resolve().parent.parent  # runners/c
if str(_RUNNER_DIR) not in sys.path:
    sys.path.insert(0, str(_RUNNER_DIR))


def _build_runner() -> None:
    """Build ``runners/c/runner`` via the channel Makefile. EMPYREAN_ROOT is
    propagated from the environment when set (the gate sets it to the pinned
    engine checkout); otherwise the Makefile's default sibling is used."""
    cmd = ["make"]
    root = os.environ.get("EMPYREAN_ROOT")
    if root:
        cmd.append(f"EMPYREAN_ROOT={root}")
    try:
        subprocess.run(
            cmd, cwd=_RUNNER_DIR, check=True, capture_output=True, text=True
        )
    except (subprocess.CalledProcessError, FileNotFoundError) as e:
        # Leave the binary absent; the end-to-end tests skipif it is missing.
        # Print the cause so a failed gate build is diagnosable rather than silent.
        sys.stderr.write(f"conftest: C runner build skipped/failed: {e}\n")


_build_runner()
