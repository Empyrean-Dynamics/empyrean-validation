#!/usr/bin/env bash
# Seal the running job from JPL, NAIF and the MPC.
#
# No validation job reaches JPL, NAIF or the MPC: the inputs snapshot, the
# registered kernel set and the third-party tool set (all in our own buckets,
# minted by the manual mint-inputs run) carry every product they publish. Any
# client that would reach these hosts is reading past those sets, so it must
# fail at the socket rather than silently fetch a different input than the one
# the git SHA pins. This points the six upstream hosts at the loopback address
# in /etc/hosts and then PROVES the override took, failing the job if any host
# still resolves off-box.
#
# It takes no arguments and always seals all six hosts. Every engine job (prep,
# reference, reference-heavy, assemble, assemble-final, empyrean,
# empyrean-heavy, reduce) runs it before any setup, and so does every external
# leg whose tool is not on the upstream-fetch allowlist.
#
# The tools whose setup or import still reaches one of these hosts are named in
# the workflow's UPSTREAM_FETCH_ALLOWLIST and in
# scripts/tests/test_no_upstream_fetch.py, which fails if the two disagree or
# the list grows. Their legs seal after setup instead of before it.
# Publishing to storage.googleapis.com is deliberately NOT sealed.
set -euo pipefail

HOSTS=(
  ssd.jpl.nasa.gov
  ssd-api.jpl.nasa.gov
  naif.jpl.nasa.gov
  data.minorplanetcenter.net
  minorplanetcenter.net
  www.minorplanetcenter.net
)
if [ "$#" -ne 0 ]; then
  echo "usage: seal-upstream.sh (no arguments: every host is sealed)" >&2
  exit 2
fi

echo "Sealing the job from ${#HOSTS[@]} JPL, NAIF and MPC hosts (pinned inputs only):"
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
  echo "ERROR: upstream seal verification failed; refusing to run suite code that could reach JPL, NAIF or the MPC." >&2
  exit 1
fi

echo "All ${#HOSTS[@]} upstream hosts sealed to 127.0.0.1."
