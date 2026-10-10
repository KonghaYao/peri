#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

: "${WRANGLER:=wrangler}"
: "${PERI_TIME_SMOKE_PORT:=8791}"
./run.sh
mkdir -p workerd/dist
cp target/wasm32-unknown-emscripten/debug/peri-time-emscripten-smoke.js workerd/dist/
cp target/wasm32-unknown-emscripten/debug/peri_time_emscripten_smoke.wasm workerd/dist/
cp workerd/worker.js workerd/dist/
log_file="$(mktemp /tmp/peri-time-workerd.XXXXXX)"
"$WRANGLER" dev --local --port "$PERI_TIME_SMOKE_PORT" --config workerd/wrangler.toml > "$log_file" 2>&1 &
server_pid=$!
trap 'kill "$server_pid" 2>/dev/null || true; rm -f "$log_file"' EXIT

response=''
for _ in {1..50}; do
  if response="$(curl --silent --show-error --fail --max-time 2 "http://127.0.0.1:$PERI_TIME_SMOKE_PORT/" 2>/dev/null)"; then
    break
  fi
  sleep 0.2
done
if [[ "$response" != 'peri-time workerd smoke: ok' ]]; then
  cat "$log_file" >&2
  echo "unexpected workerd response: $response" >&2
  exit 1
fi
echo "$response"
