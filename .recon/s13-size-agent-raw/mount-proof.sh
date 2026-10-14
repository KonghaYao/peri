#!/usr/bin/env bash
# S13 mount proof：逐子模块过滤运行，证明挂载真实成立（非 0 filtered 空跑）。
set -u
cd /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history || exit 1
OUT=.recon/s13-size-agent-raw/mount-proof.txt
: > "$OUT"

run() {
  local filter="$1"
  echo "=== filter: $filter ===" >> "$OUT"
  ./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- "$filter" 2>&1 \
    | grep -E "^running|^test result" >> "$OUT"
  echo "exit=$?" >> "$OUT"
  echo "" >> "$OUT"
}

run "session::transcript::tests::persistence_tests::"
run "session::transcript::tests::memory_tests::"
run "session::transcript::tests::reminder_tests::"
run "session::transcript::tests::persistence_tests::test_drop_flushes_pending_appends_to_store"
run "session::transcript::"
echo "ALL DONE" >> "$OUT"
