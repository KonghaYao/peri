#!/usr/bin/env bash
# A1 复现循环：直接跑已编译的 lib 测试二进制，记录每轮结果。
set -uo pipefail
cd /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history
BIN=target/debug/deps/peri_resources-5e5b2d6b81e29ee9
N=${1:-20}
THREADS=${2:-18}
OUT=.recon/a1-raw
for i in $(seq 1 "$N"); do
  start=$(date +%s)
  "$BIN" --test-threads="$THREADS" > "$OUT/run-$i.log" 2>&1
  code=$?
  end=$(date +%s)
  line=$(grep -E "^test result:" "$OUT/run-$i.log" | tail -1)
  printf "run=%02d exit=%d secs=%d %s\n" "$i" "$code" "$((end-start))" "$line"
  if [ "$code" -ne 0 ]; then
    grep -nE "^test .* FAILED|^failures:|^---- " "$OUT/run-$i.log" | head -20
  fi
done
