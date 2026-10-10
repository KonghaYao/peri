#!/usr/bin/env bash
# Run Cargo through scripts/cargo-rmcp-patched.sh with a local sccache.
#
# Opt-in: only invocations of this script set RUSTC_WRAPPER. Nothing global
# (shell profile, .cargo/config.toml, CI) is changed, so developers without
# sccache keep using scripts/cargo-rmcp-patched.sh unchanged.
#
# See docs/reference/dev-build-cache.md for measurements, limits and cleanup.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if ! command -v sccache >/dev/null 2>&1; then
    cat >&2 <<'EOF'
cargo-sccache.sh: sccache is not on PATH.
  macOS:  brew install sccache
  other:  cargo install sccache --locked
Or run scripts/cargo-rmcp-patched.sh without the wrapper.
EOF
    exit 1
fi

# Honor a caller-provided wrapper; otherwise use sccache. The patched-rmcp
# script only adds --config, so RUSTC_WRAPPER passes straight through to cargo.
export RUSTC_WRAPPER="${RUSTC_WRAPPER:-sccache}"

# --cross runs cargo inside a container that does not have sccache.
if [[ "${1:-}" == "--cross" ]]; then
    echo "cargo-sccache.sh: --cross builds inside a container without sccache; ignoring the wrapper." >&2
    unset RUSTC_WRAPPER
fi

exec "$repo_root/scripts/cargo-rmcp-patched.sh" "$@"
