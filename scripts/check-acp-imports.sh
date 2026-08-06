#!/usr/bin/env bash
# Check that peri-acp/ protocol surface doesn't import middleware/lsp/workflow
# business crates.
#
# §0 依赖方向（docs/top-level.md）：ACP 是纯协议层，只能依赖契约层
# （peri-acp-types）与 Controller（依赖方向经装配注入的端口类型除外）；
# import peri-middlewares / peri-lsp / peri-workflow 业务面即越层。
#
# 豁免清单（明确写回 spec/issues/2026-08-05-3.0-acp-events-session-batch2.md）：
# - host/exec/ 执行本体（RCRA loop / stage 装配 / workflow agent / bg 发起），
#   过渡宿主豁免至 L5 executor 拆分（物理迁入 peri-agent 后引用随之消失）；
#   豁免期内禁止新增业务面直接调用，新代码一律经注入端口
# - 测试文件（*_test.rs / tests/）——验收 grep 豁免测试；测试经引用业务
#   crate 仅验证行为，不构成协议面依赖
#
# 装配边界收口：宿主装配点（peri-tui main.rs / launch.rs / cli_print.rs、
# peri-acp host/assemble.rs / host/stdio）构造具体实现后 upcast 注入端口
# （peri-acp-types::ports / cron::CronSchedulerPort 等），ACP 协议面只持
# 端口接口，不直接 new 资源类（McpClientPool / CronScheduler / ToolSearchIndex）。

set -e
cd "$(dirname "$0")/.."

VIOLATIONS=$(grep -rn "peri_middlewares\|peri_lsp\|peri_workflow" \
    peri-acp/src/ \
    --include="*.rs" \
    | grep -v "peri-acp/src/host/exec/" \
    | grep -v "_test.rs" \
    | grep -v "tests/" \
    || true)

if [ -n "$VIOLATIONS" ]; then
    echo "❌ Forbidden business-crate imports in peri-acp (only peri_acp_types / peri_controller / injected ports allowed):"
    echo "$VIOLATIONS"
    exit 1
fi

echo "✅ peri-acp imports OK (§0: ACP protocol surface holds injected ports only)"
