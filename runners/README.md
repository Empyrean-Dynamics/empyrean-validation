# External Reference Runners

Standalone Python runners that exercise three independent astrodynamics
implementations (ASSIST, find_orb, kete) against the same canonical
validation plan and emit JSON conforming to the
[`ValidationResult`](../src/schema.rs) schema. The empyrean validation
report folds these into a multi-channel comparison so each empyrean
distribution channel can be benchmarked against an independent
reference.

Each runner ships its own `setup.sh` (installs the upstream dependency
into a per-runner virtualenv or build tree) and a single `run_*.py`
entry point. They're intentionally self-contained — none of them
import any empyrean code, none of them reach back into the parent
repository, and each can be invoked directly with `--help` to see its
flags.

## Runners

| Directory | Upstream | License | What it exercises |
|---|---|---|---|
| [`assist/`](assist/) | [ASSIST](https://github.com/matthewholman/assist) (REBOUND-based N-body propagator) | GPL-3.0 | Propagation. ASSIST is GPL and never linked into empyrean — runs in its own venv. |
| [`findorb/`](findorb/) | [find_orb](https://github.com/Bill-Gray/find_orb) (Bill Gray) | GPL-2.0 | Orbit determination from MPC astrometry. Built from source, runs as a CLI subprocess. |
| [`kete/`](kete/) | [kete](https://github.com/dahlend/kete) (Dahl & friends) | BSD-3-Clause | Propagation, ephemeris generation, and OD. Pure Python (rebuild-from-PyPI). |
| [`oorb/`](oorb/) | [OpenOrb](https://github.com/oorb/oorb) (Granvik et al., Fortran orbit-computation library) | GPL-3.0 | Propagation and ephemeris generation. Built from upstream Fortran source via `setup.sh`; the runner shells out to the `oorb` CLI binary. The pip-installable Python wrapper (`pyoorb`) is intentionally not used — it has the same Fortran build dependency anyway and its sdist breaks on Python 3.12. OD skipped (Ranging/LSL doesn't fit per-row replay). |
| [`jorbit/`](jorbit/) | [jorbit](https://github.com/ben-cassese/jorbit) (Cassese, JAX-based N-body integrator) | GPL-3.0 | Propagation and ephemeris generation. jorbit is GPL and never linked into empyrean — runs in its own venv (also keeps the JAX dep tree off the rest of the suite). OD skipped. |

## Workflow

The runners assume a [validation plan JSON](../src/plan.rs) has
already been generated (typically by the empyrean rust runner or the
empyrean-validation `plan` subcommand once it exists). Each runner
reads the plan, replays the test cases through its upstream
implementation, and writes its own per-channel JSON.

```bash
# One-time per runner.
./assist/setup.sh
./findorb/setup.sh
./kete/setup.sh
./oorb/setup.sh
./jorbit/setup.sh

# Run.
./assist/.venv/bin/python assist/run_assist.py   --plan  validation_plan.json --output validation_assist.json
./findorb/install/bin/fo                                                                  # find_orb is invoked from run_findorb.py
./kete/.venv/bin/python   kete/run_kete.py       --input validation_plan.json --output validation_kete.json
python3                   oorb/run_oorb.py       --input validation_plan.json --output validation_oorb.json
./jorbit/.venv/bin/python jorbit/run_jorbit.py   --input validation_plan.json --output validation_jorbit.json
```

Outputs are merged by the empyrean-validation report renderer into a
single multi-channel HTML page.
