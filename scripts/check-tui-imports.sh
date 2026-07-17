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
#
# P0-1 whitelist（合法运行时桥接——区分桥接与真正违规）:
# - service_registry.rs     TUI service registry 管理 middleware 服务生命周期
# - cron_state.rs           TUI cron state 持有 cron scheduler 句柄
# - submit_consumer.rs      Submit consumer 调用 acp_client.prompt() 需 agent 类型
# - event_handlers.rs       键盘快捷键循环切换 PermissionMode
# - service_snapshot.rs     Service snapshot 读取所有服务状态
# - panels/config.rs        Config 面板管理 PermissionMode 状态
# - panels/plugin.rs        Plugin 管理面板
# - launch.rs               App 启动初始化 PermissionMode
# - cli_plugin.rs           CLI plugin 入口点
# - app/mod.rs              App 入口初始化 MCP 连接池（McpClientPool 运行时依赖）

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
    | grep -v "peri-tui/src/app/service_registry\.rs" \
    | grep -v "peri-tui/src/app/cron_state\.rs" \
    | grep -v "peri-tui/src/kit/submit_consumer\.rs" \
    | grep -v "peri-tui/src/kit/event_handlers\.rs" \
    | grep -v "peri-tui/src/kit/service_snapshot\.rs" \
    | grep -v "peri-tui/src/kit/panels/config\.rs" \
    | grep -v "peri-tui/src/kit/panels/plugin\.rs" \
    | grep -v "peri-tui/src/launch\.rs" \
    | grep -v "peri-tui/src/cli_plugin\.rs" \
    | grep -v "peri-tui/src/app/mod\.rs" \
    || true)

if [ -n "$VIOLATIONS" ]; then
    echo "❌ Forbidden peri_agent/peri_middlewares imports in peri-tui:"
    echo "$VIOLATIONS"
    echo ""
    echo "Allowed bridge files: acp_server/, acp_client/, acp_stdio/, main.rs,"
    echo "cli_print.rs, message_pipeline/, ui/message_view/, state_machine/,"
    echo "thread/mod.rs, *_test.rs / *_test/ (transitional)"
    echo "P0-1 runtime bridges: service_registry.rs, cron_state.rs, submit_consumer.rs,"
    echo "event_handlers.rs, service_snapshot.rs, panels/config.rs, panels/plugin.rs,"
    echo "launch.rs, cli_plugin.rs, app/mod.rs"
    exit 1
fi

echo "✅ TUI imports OK"
