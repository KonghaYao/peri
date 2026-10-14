#!/usr/bin/env bash
# S13 任务 B4：最终验证链（文件已冻结，顺序执行并对齐证据）
set -uo pipefail
cd "$(dirname "$0")/../.."
RAW=.recon/s13-size-tui-resources-raw

FILES="peri-tui/src/kit/acp_events/system.rs
peri-tui/src/kit/acp_events/system/interaction.rs
peri-tui/src/kit/acp_events/system/bg_task.rs
peri-tui/src/kit/acp_events_test/session_events_test.rs
peri-tui/src/kit/acp_events_test/session_rewind_events_test.rs
peri-tui/src/kit/acp_events_test/session_compact_events_test.rs
peri-resources/src/sessions/sqlite_store/session_history_perf_test.rs
peri-resources/src/sessions/sqlite_store/session_history_perf_cases_test.rs
peri-resources/src/sessions/sqlite_store/session_history_perf_audit_test.rs
peri-resources/src/sessions/remote/full_remote_test.rs
peri-resources/src/sessions/remote/full_remote_rewind_test.rs"

echo "=== 0. 行数 + sha256（最终文件） ==="
wc -l $FILES | tee "$RAW/after-wc.txt"
shasum -a 256 $FILES | tee "$RAW/after-sha256.txt"

echo "=== 1. 搬移块字节级比对 ==="
python3 "$RAW/verify_moves.py" > "$RAW/verify-moves.txt" 2>&1
echo "EXIT=$?" | tee -a "$RAW/verify-moves.txt"
tail -3 "$RAW/verify-moves.txt"

echo "=== 2. peri-tui 全量 lib 测试 ==="
./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib > "$RAW/after-peri-tui-lib.txt" 2>&1
echo "EXIT=$?" >> "$RAW/after-peri-tui-lib.txt"
grep -E "^test result" "$RAW/after-peri-tui-lib.txt" | tail -2

echo "=== 3. peri-resources 全量 lib 测试 ==="
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib > "$RAW/after-peri-resources-lib.txt" 2>&1
echo "EXIT=$?" >> "$RAW/after-peri-resources-lib.txt"
grep -E "^test result" "$RAW/after-peri-resources-lib.txt" | tail -2

echo "=== 4. 挂载证明 ==="
bash "$RAW/mount-proof.sh"
grep -E "^\\$ |^running |^test result" "$RAW/mount-proof.txt" | grep -cE "^test result: ok"

echo "=== 5. 规模全量扫描 ==="
bash scripts/check-file-size.sh > "$RAW/file-size-scan.txt" 2>&1
echo "EXIT=$?" >> "$RAW/file-size-scan.txt"
tail -12 "$RAW/file-size-scan.txt"
echo "FINAL CHECK DONE"
