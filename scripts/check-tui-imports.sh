#!/usr/bin/env bash
# Check that peri-tui/src/ doesn't import from peri-agent or peri-middlewares
# except for explicitly whitelisted bridge files.
#
# Whitelist (合法桥接):
# - acp_server/             TUI 内嵌 ACP server
# - acp_client/             ACP client（类型桥接）
# - acp_stdio/              TUI 内嵌 ACP stdio 模式（server side）
# - main.rs                 bin entry
# - cli_print.rs            -p 打印模式
# - message_pipeline/       P5 会整体删除，过渡期保留
# - ui/message_view/        P5 会迁移到 ACP，过渡期保留
# - state_machine/          过渡期保留
# - thread/mod.rs           ThreadStore 类型桥接（pub re-export）
# - *_test.rs / *_test/     测试文件（测试数据需要 BaseMessage 等类型）

set -e
cd "$(dirname "$0")/.."

VIOLATIONS=$(grep -rn "use peri_agent::\|use peri_middlewares::" \
    peri-tui/src/ \
    --include="*.rs" \
    | grep -v "peri-tui/src/acp_server/" \
    | grep -v "peri-tui/src/acp_client/" \
    | grep -v "peri-tui/src/acp_stdio/" \
    | grep -v "peri-tui/src/main.rs" \
    | grep -v "peri-tui/src/cli_print.rs" \
    | grep -v "peri-tui/src/app/message_pipeline/" \
    | grep -v "peri-tui/src/ui/message_view/" \
    | grep -v "peri-tui/src/state_machine/" \
    | grep -v "peri-tui/src/thread/mod.rs" \
    | grep -v "_test.rs" \
    | grep -v "_test/" \
    || true)

if [ -n "$VIOLATIONS" ]; then
    echo "❌ Forbidden peri_agent/peri_middlewares imports in peri-tui:"
    echo "$VIOLATIONS"
    echo ""
    echo "Allowed bridge files: acp_server/, acp_client/, acp_stdio/, main.rs,"
    echo "cli_print.rs, message_pipeline/, ui/message_view/, state_machine/,"
    echo "thread/mod.rs, *_test.rs / *_test/ (transitional)"
    exit 1
fi

echo "✅ TUI imports OK"
