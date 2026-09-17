#!/usr/bin/env bash
# Build the 32-bit x86 Linux distribution (i686 CPU baseline, static musl).
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

if [[ $# -ne 0 ]]; then
    echo "Usage: $0" >&2
    exit 2
fi

# Environment flags override .cargo/config.toml and can silently disable Zig
# or static linking. Keep this distribution build on the checked-in flags.
if [[ -n ${RUSTFLAGS:-} || -n ${CARGO_ENCODED_RUSTFLAGS:-} ]]; then
    echo "Unset RUSTFLAGS and CARGO_ENCODED_RUSTFLAGS for this build." >&2
    exit 2
fi

cargo zigbuild --locked --release --target i686-unknown-linux-musl -p peri-tui --bin peri
