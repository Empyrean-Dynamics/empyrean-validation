#!/usr/bin/env bash
# Setup OrbFit for OD comparison via the MPC's official Docker container.
#
# OrbFit (https://adams.dm.unipi.it/orbfit/) is the orbit-determination
# software developed by the OrbFit Consortium (University of Pisa) and
# adopted by the IAU Minor Planet Center as their production OD pipeline.
# It is the canonical reference for the Carpino-Milani-Chesley (2003)
# χ²-with-hysteresis rejection scheme that empyrean implements as
# `RejectionKind::CMC2003`.
#
# OrbFit is GPL-licensed and never linked into empyrean. This script
# pulls the MPC's pre-built container for comparison purposes only.
#
# Container: minorplanetcenter/orbfit, pinned by digest
#   - Maintained by the MPC (Federica Spoto et al.)
#   - ~3.1 GB compressed, ~6 GB on disk
#   - Includes OrbFit 5.0.x compiled with gfortran and OrbFit's own data
#     (AST17.bai / AST17.bep, the neofit options), so the digest pins those.
#   - The digest is what minorplanetcenter/orbfit:latest resolved to on
#     2026-11-09 (docker buildx imagetools inspect); run_orbfit.py's
#     DEFAULT_IMAGE names the same one. Bump both together, deliberately.
#
# Usage:
#   ./setup.sh                # pull the pinned digest
#   ./setup.sh <image ref>    # another image (testing only)
#
# Prerequisites: a running Docker daemon. On macOS install Docker
# Desktop or colima; on Linux ensure the docker daemon is up.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
IMAGE="${1:-minorplanetcenter/orbfit@sha256:0a72ed7c2c528e0e3fe24a0f280a251f6ab610eb3d5e1d63dbf0f2bd20104f64}"

# ── Sanity: Docker available + daemon reachable ─────────────────
if ! command -v docker >/dev/null 2>&1; then
    echo "error: docker is not on PATH." >&2
    echo "  install Docker Desktop (macOS / Windows) or the docker engine (Linux)" >&2
    echo "  before running this script." >&2
    exit 1
fi

if ! docker info >/dev/null 2>&1; then
    echo "error: cannot reach the Docker daemon." >&2
    echo "  start Docker Desktop (macOS) or 'sudo systemctl start docker' (Linux)" >&2
    echo "  and re-run this script." >&2
    exit 1
fi

# ── Pull the image ──────────────────────────────────────────────
echo "──── Pulling ${IMAGE} ────"
docker pull --platform linux/amd64 "${IMAGE}"

# ── Smoke test ──────────────────────────────────────────────────
echo
echo "──── Smoke-testing the container ────"
echo "Image metadata:"
docker inspect "${IMAGE}" --format '  ENTRYPOINT: {{.Config.Entrypoint}}'
docker inspect "${IMAGE}" --format '  CMD:        {{.Config.Cmd}}'
docker inspect "${IMAGE}" --format '  WORKDIR:    {{.Config.WorkingDir}}'

echo
echo "OrbFit binaries available inside the container:"
docker run --rm --network none --platform linux/amd64 "${IMAGE}" sh -c 'ls -1 /usr/local/bin 2>/dev/null | grep -iE "orbfit|fitobs|catpro" || ls -1 $(find / -name "fitobs*" -type f 2>/dev/null | head -1 | xargs dirname 2>/dev/null) 2>/dev/null | head -20' || \
    echo "  (could not auto-detect binaries — inspect the container manually)"

echo
echo "OrbFit setup complete."
echo "  image: ${IMAGE}"
echo
echo "Run OD comparison with:"
echo "  python3 $SCRIPT_DIR/run_orbfit.py \\"
echo "      --plan ../../results/validation_plan.json \\"
echo "      --de440 <third-party>/orbfit/de440.bsp \\"
echo "      --output ../../results/validation_orbfit.json"
