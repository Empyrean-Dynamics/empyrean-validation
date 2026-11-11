#!/usr/bin/env bash
# Build the adam-core Rust-API runner: adam_core 0.8.0 + adam_assist 0.7.0 from
# crates.io, exactly as pinned in Cargo.toml and locked in Cargo.lock.
#
# adam_assist (GPL-3.0) and its vendored ASSIST and REBOUND C sources are
# compiled into this runner's binary only; nothing here is linked into empyrean.
# The build fetches crates from crates.io and nothing else: no data file is
# downloaded (every kernel, the observatory table and the EFCC18 table come
# from the adam-core-rust folder of the third-party tool set at run time).
#
# CARGO_TARGET_DIR (default runners/adam-core-rust/build) is where the binary
# lands: $CARGO_TARGET_DIR/release/run-adam-core-rust.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HERE/build}"

command -v cargo >/dev/null 2>&1 || { echo "ERROR: cargo not found (install Rust >= 1.87)." >&2; exit 1; }
# The OD sigma fill runs the shared runners/sigmafill through any python3
# (standard library only).
command -v python3 >/dev/null 2>&1 || { echo "ERROR: python3 not found (needed for runners/sigmafill)." >&2; exit 1; }

echo "──── cargo build --release --locked (adam-core-rust) ───────"
cargo build --release --locked --manifest-path "$HERE/Cargo.toml"
BIN="$CARGO_TARGET_DIR/release/run-adam-core-rust"
test -x "$BIN" || { echo "ERROR: build produced no $BIN" >&2; exit 1; }
echo "──── setup complete: $BIN ───────"
