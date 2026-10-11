//! L1：插件命令（`plugin:{plugin}:{cmd}`）的 ACP 路由层契约测试。
//!
//! 本文件覆盖 ACP 侧两条路由，二者都不依赖 loader 单测的结论：
//!
//! 1. **拦截路径（会话命令注册表命中）不吞输入**：handler 把用户原文整段
//!    `Inject` 回 agent 管线（与 `AgentPassthrough` 同语义）。历史缺陷是
//!    `Inject(String::new())`——原文被空串替换，命令无声丢失。
//! 2. **execute-command RPC 路径不假成功**：RPC 无 agent 管线，第二等级
//!    （plugin）条目在 handler 执行前被显式拒绝（-32602），不会返回一个
//!    「空 messages + end_turn」的成功结果。
//!
//! 注册表构建沿用会话构造面（`build_command_registry`）的做法：先内置、再
//! `register_all(plugin_route_entries(...))`。

use std::sync::Arc;

use super::*;
use peri_acp_types::event::{EventSink, ExecutorEvent};
use peri_acp_types::plugin::{CommandEntry, CommandSource as PluginCommandSource};

struct NoopEventSink;

#[async_trait::async_trait]
impl EventSink for NoopEventSink {
    async fn push_event(&self, _session_id: &str, _event: &ExecutorEvent, _context_window: u32) {}

    async fn push_done(&self, _session_id: &str, _stop_reason: &str, _request_id: Option<&str>) {}
}

const RAW_TEXT: &str = "/plugin:ecc:deploy --env prod";

fn plugin_routes() -> Vec<RouteEntry> {
    let entries = vec![CommandEntry {
        name: "plugin:ecc:deploy".into(),
        description: "Deploy to prod".into(),
        source: PluginCommandSource::Plugin {
            path: std::path::PathBuf::from("/tmp/ecc/commands/deploy.md"),
        },
    }];
    peri_middlewares::plugin::plugin_route_entries(&entries)
}

fn context_with_raw_text() -> CommandContext {
    let mut ctx = CommandContext::new(
        "l1-acp-route-session".to_string(),
        Vec::new(),
        "/tmp".to_string(),
        Arc::new(NoopEventSink),
        tokio_util::sync::CancellationToken::new(),
        Default::default(),
    );
    ctx.raw_text = RAW_TEXT.to_string();
    ctx.args = "--env prod".to_string();
    ctx.supports_inject = true;
    ctx
}

/// 会话注册表命中插件命令后，handler 必须把原文整段交还 agent 管线。
#[tokio::test]
async fn plugin_command_injects_original_text_through_the_session_registry() {
    let registry = CommandRegistry::new();
    let (added, errors) = registry.register_all(plugin_routes());
    assert_eq!(
        (added, errors.len()),
        (1, 0),
        "plugin 条目必须可注册进会话命令表"
    );
    let resolved = registry
        .resolve("plugin:ecc:deploy")
        .expect("会话注册表应命中插件命令");

    match resolved
        .entry
        .handler
        .execute(context_with_raw_text())
        .await
    {
        CommandOutcome::Inject(payload) => assert_eq!(
            payload, RAW_TEXT,
            "用户原文必须逐字透传（含命令 token 与 args），不得被吞"
        ),
        CommandOutcome::Done(_) => panic!("插件命令不得以 Done 伪报执行"),
        CommandOutcome::Delegate(_) => panic!("插件命令本 Phase 无 Delegate 语义"),
    }
}

/// RPC 路由：plugin（第二等级）条目执行前被显式拒绝，不会伪报成功。
#[tokio::test]
async fn plugin_command_is_rejected_by_the_rpc_route_without_fake_success() {
    let entry = plugin_routes()
        .into_iter()
        .next()
        .expect("fixture 必然产出一条 plugin 路由");

    let error = check_immediate_level(&entry).expect_err("plugin 条目必须被 RPC 路由拒绝");
    assert_eq!(error.code, -32602, "拒绝必须是显式参数错误: {error:?}");
    assert!(
        error.message.contains("非 Immediate"),
        "拒绝原因必须可诊断（不能静默返回空成功）: {}",
        error.message
    );
}
