#!/usr/bin/env bash
# 参数：标签 运行次数 测试过滤参数...
set -uo pipefail
cd /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history
BIN=target/debug/deps/peri_resources-5e5b2d6b81e29ee9
label=$1; n=$2; shift 2
out=.recon/a1-raw
red=0; green=0
for i in $(seq 1 "$n"); do
  "$BIN" "$@" > "$out/${label}-$i.log" 2>&1
  code=$?
  if [ "$code" -ne 0 ]; then red=$((red+1)); else green=$((green+1)); fi
  if [ "$code" -ne 0 ]; then
    site=$(grep -hoE "connection_open_test\.rs:[0-9]+:[0-9]+" "$out/${label}-$i.log" | sort -u | tr '\n' ' ')
    echo "  ${label} run=$i RED site=[$site]"
  fi
done
echo "EXP ${label}: N=$n RED=$red GREEN=$green  args=[$*]"
