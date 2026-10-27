#!/usr/bin/env bash
# Seal the running job from JPL and the MPC.
#
# The validation suite reads every JPL and MPC product from the pinned inputs
# snapshot on disk and from nothing else. Any client that would reach these
# hosts is reading past the snapshot, so it must fail at the socket rather than
# silently fetch a different input than the one the git SHA pins. This points
# the five upstream hosts at the loopback address in /etc/hosts and then PROVES
# the override took, failing the job if any host still resolves off-box.
#
# Position: run AFTER tool setup and BEFORE the suite's run step. find_orb's
# binary DE440 (engine-setup, writer) and GRSS's two kernels (its setup.sh)
# still download from ssd.jpl.nasa.gov during setup; once the kernel set
# carries them the seal can move before setup. Publishing to
# storage.googleapis.com is deliberately NOT sealed.
set -euo pipefail

HOSTS=(
  ssd.jpl.nasa.gov
  ssd-api.jpl.nasa.gov
  data.minorplanetcenter.net
  minorplanetcenter.net
  www.minorplanetcenter.net
)

echo "Sealing the job from JPL and the MPC (pinned-snapshot inputs only):"
# A container-generated hosts file need not end with a newline. Appending to
# such a file glues the first entry onto its last line, so the host lands on
# that line's address instead of 127.0.0.1 (seen under act, where the first
# host resolved to the ff02::2 line) and the verification below fails closed.
# Terminate the file first so every entry starts a line of its own.
if [ -n "$(tail -c1 /etc/hosts)" ]; then
  printf '\n' | sudo tee -a /etc/hosts >/dev/null
fi
for h in "${HOSTS[@]}"; do
  echo "127.0.0.1 ${h}" | sudo tee -a /etc/hosts >/dev/null
done

# Prove the seal: every host must now resolve to loopback, or the job fails
# here instead of reaching upstream from a later step.
fail=0
for h in "${HOSTS[@]}"; do
  addr="$(getent hosts "${h}" | awk '{print $1}' | head -n1 || true)"
  if [ "${addr}" = "127.0.0.1" ]; then
    echo "  sealed: ${h} -> ${addr}"
  else
    echo "  ERROR: ${h} resolves to '${addr:-<none>}', not 127.0.0.1 — seal failed." >&2
    fail=1
  fi
done

if [ "${fail}" -ne 0 ]; then
  echo "ERROR: upstream seal verification failed; refusing to run suite code that could reach JPL or the MPC." >&2
  exit 1
fi

echo "All ${#HOSTS[@]} upstream hosts sealed to 127.0.0.1."
