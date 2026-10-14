#!/usr/bin/env bash
# B1 循环：用任务指定的 cargo 入口跑 peri-resources lib 测试，逐轮记录。
set -uo pipefail
cd /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history
N=${1:-5}
OUT=${2:-.recon/b1-raw}
mkdir -p "$OUT"
for i in $(seq 1 "$N"); do
  start=$(date +%s)
  ./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib > "$OUT/run-$i.log" 2>&1
  code=$?
  end=$(date +%s)
  line=$(grep -E "^test result:" "$OUT/run-$i.log" | tail -1)
  printf "run=%02d exit=%d secs=%d %s\n" "$i" "$code" "$((end-start))" "$line"
  if [ "$code" -ne 0 ]; then
    grep -E "panicked at" "$OUT/run-$i.log" | sed 's/^/    /' | sort | uniq -c | head -8
  fi
done
