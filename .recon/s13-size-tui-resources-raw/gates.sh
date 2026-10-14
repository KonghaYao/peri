#!/usr/bin/env bash
# S13 任务 B4：拆分后门禁与最终验证（peri-tui 重跑 + fmt + clippy + 规模扫描）
set -uo pipefail
cd "$(dirname "$0")/../.."
RAW=.recon/s13-size-tui-resources-raw

echo "=== 1. peri-tui 全量 lib 测试（重跑，最终文件状态） ==="
./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib > "$RAW/after-peri-tui-lib.txt" 2>&1
echo "EXIT=$?" >> "$RAW/after-peri-tui-lib.txt"
grep -E "^test result|^error" "$RAW/after-peri-tui-lib.txt" | tail -3

echo "=== 2. 挂载证明（最终文件状态） ==="
bash "$RAW/mount-proof.sh"
grep -cE "^test result: ok" "$RAW/mount-proof.txt"

echo "=== 3. fmt --check ==="
./scripts/cargo-rmcp-patched.sh fmt --check > "$RAW/fmt-check.txt" 2>&1
echo "EXIT=$?" >> "$RAW/fmt-check.txt"
tail -3 "$RAW/fmt-check.txt"

echo "=== 4. clippy（改动 crate，-W clippy::all） ==="
changed="$(bash scripts/precommit-changed-crates.sh)"
echo "changed crates: $changed" | tee "$RAW/clippy.txt"
if [ -n "$changed" ]; then
    ./scripts/cargo-rmcp-patched.sh clippy --locked $changed -- -W clippy::all >> "$RAW/clippy.txt" 2>&1
    echo "EXIT=$?" >> "$RAW/clippy.txt"
fi
grep -cE "^(warning|error)" "$RAW/clippy.txt"
tail -3 "$RAW/clippy.txt"

echo "=== 5. 文件规模全量扫描 ==="
bash scripts/check-file-size.sh > "$RAW/file-size-scan.txt" 2>&1
echo "EXIT=$?" >> "$RAW/file-size-scan.txt"
cat "$RAW/file-size-scan.txt"
echo "GATES DONE"
