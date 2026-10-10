#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

# wasm-bindgen-cli must match the fixture's pinned Rust crate version.
if [[ "$(wasm-bindgen --version)" != "wasm-bindgen 0.2.129" ]]; then
  echo "wasm-bindgen-cli 0.2.129 is required" >&2
  exit 1
fi
export EMSDK_PYTHON="$(command -v python3)"
export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-Cpanic=abort -Cllvm-args=-enable-emscripten-cxx-exceptions=0 -Crelocation-model=static -Clink-arg=-sWASM_BINDGEN -Clink-arg=-Wno-experimental -Clink-arg=-sMODULARIZE -Clink-arg=-sEXPORT_ES6 -Clink-arg=-sDYNAMIC_EXECUTION=0 -Clink-arg=-sALLOW_MEMORY_GROWTH -Clink-arg=-sINCOMING_MODULE_JS_API=mainScriptUrlOrBlob,instantiateWasm,locateFile,print,printErr,preRun,postRun,onRuntimeInitialized,onAbort"
cargo build --target wasm32-unknown-emscripten
node run.mjs
