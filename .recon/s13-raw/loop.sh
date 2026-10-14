#!/bin/bash
# usage: loop.sh <rounds> <tag>
cd /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history
rounds="$1"; tag="$2"
: > ".recon/s13-raw/${tag}-summary.txt"
for i in $(seq 1 "$rounds"); do
  log=".recon/s13-raw/${tag}-run-${i}.log"
  ./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- release_probe_waits_out_an_external_holder > "$log" 2>&1
  exit_code=$?
  result=$(grep -E "^test result:" "$log" | tail -1)
  echo "run=$(printf '%02d' "$i") exit=$exit_code $result" >> ".recon/s13-raw/${tag}-summary.txt"
done
cat ".recon/s13-raw/${tag}-summary.txt"
