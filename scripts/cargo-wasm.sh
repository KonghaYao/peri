#!/usr/bin/env bash
# Build the Emscripten target with the pinned Cargo patches and wasm-bindgen flags.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ -n "${EMSDK_PYTHON:-}" ]] && ! "$EMSDK_PYTHON" -c 'import sys; assert sys.version_info >= (3, 10)' >/dev/null 2>&1; then
    if python3 -c 'import sys; assert sys.version_info >= (3, 10)' >/dev/null 2>&1; then
        export EMSDK_PYTHON="$(command -v python3)"
    else
        echo "Emscripten requires Python 3.10 or newer" >&2
        exit 1
    fi
fi
"$repo_root/scripts/prepare-emscripten.sh"
export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }--cfg wasm_bindgen_unstable_tokio --cfg tokio_unstable -Cpanic=abort -Cllvm-args=-enable-emscripten-cxx-exceptions=0 -Crelocation-model=static -Clink-arg=-sWASM_BINDGEN -Clink-arg=-Wno-experimental -Clink-arg=-sMODULARIZE -Clink-arg=-sEXPORT_ES6 -Clink-arg=-sNODERAWSOCKETS -Clink-arg=-sDYNAMIC_EXECUTION=0 -Clink-arg=-sSTACK_SIZE=8MB -Clink-arg=-sALLOW_MEMORY_GROWTH"
export RUSTFLAGS="$RUSTFLAGS -Clink-arg=-sEXPORTED_RUNTIME_METHODS=ENV"
exec "$repo_root/scripts/cargo-rmcp-patched.sh" "$@"
