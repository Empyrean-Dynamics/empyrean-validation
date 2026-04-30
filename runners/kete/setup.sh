#!/usr/bin/env bash
# Set up a venv with kete — Dar Dahlen's open-source NEO toolkit (originally
# developed at Caltech IPAC for NEO Surveyor mission simulation work; now an
# independent personal project at github.com/dahlend/kete) — for use as an
# external propagation/ephemeris/OD reference in the validation suite.
#
# The kete runner is intentionally standalone — its output is not merged
# into validation_report.html; it is consumed by the executive-summary
# script in this directory.
set -euo pipefail

cd "$(dirname "$0")"

if [ ! -d .venv ]; then
    python3 -m venv .venv
fi

. .venv/bin/activate
pip install --upgrade pip
# Pin to a known-good kete release. Update intentionally so we don't
# inadvertently change comparison reference between runs.
pip install "kete>=2.0"

echo
echo "kete venv ready: $(pwd)/.venv"
