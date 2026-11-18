//! 插件域 slash 拦截。
//!
//! `Inject` 携带路由表 provenance 声明的插件归属，Reason 准备阶段据此复用
//! 显式选择的指引过程；命令文本不参与反推。

use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::command::command_handler::CommandHandler;
use peri_acp_types::command::command_route::CommandEntryKind;
use peri_acp_types::command::command_route::CommandLifecycle;
use peri_acp_types::command::command_route::CommandProvenance;
use peri_acp_types::command::command_route::CommandSource;
use peri_acp_types::command::command_route::RouteEntry;
use peri_acp_types::command::CommandContext;
use peri_acp_types::command::CommandOutcome;
use peri_acp_types::command::ResolvedCommand;
use peri_acp_types::event::EventSink;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::messages::MessageContent;
use tokio_util::sync::CancellationToken as AgentCancellationToken;

use super::make_bg_infra;
use super::make_intercept_request;
use super::MockEventSink;
use crate::session::exec::executor_helpers::intercept::CommandLookupFn;
use crate::session::exec::executor_helpers::intercept::InjectPayload;
use crate::session::exec::executor_helpers::intercept::InterceptOutcome;
use crate::session::exec::executor_helpers::intercept_immediate_command;

/// 插件域 slash 在拦截层解析为同一种插件使用上下文：Inject 携带 provenance
/// 声明的插件名，Reason 准备阶段据此复用显式选择的指引过程。
#[tokio::test]
async fn test_intercept_plugin_slash_carries_plugin_provenance() {
    struct PluginPassthroughHandler;
    #[async_trait]
    impl CommandHandler for PluginPassthroughHandler {
        async fn execute(&self, ctx: CommandContext) -> CommandOutcome {
            CommandOutcome::Inject(ctx.raw_text)
        }
    }
    let lookup: CommandLookupFn = Arc::new(|text: &str| {
        if text == "plugin:research:report weekly" {
            Some(ResolvedCommand {
                entry: Arc::new(RouteEntry {
                    fullname: "plugin:research:report".to_string(),
                    aliases: vec![],
                    description: "plugin command".to_string(),
                    kind: CommandEntryKind::Command,
                    category: None,
                    args_schema: None,
                    handler: Arc::new(PluginPassthroughHandler),
                    provenance: CommandProvenance {
                        source: CommandSource::Plugin {
                            name: "research".to_string(),
                        },
                        lifecycle: CommandLifecycle::Connected,
                    },
                }),
                args: "weekly".to_string(),
            })
        } else {
            None
        }
    });

    let content = MessageContent::text("/plugin:research:report weekly");
    let history: Vec<BaseMessage> = vec![];
    let cancel = AgentCancellationToken::new();
    let mock_sink = Arc::new(MockEventSink::new());
    let sink: Arc<dyn EventSink> = Arc::clone(&mock_sink) as Arc<dyn EventSink>;
    let (bg_tx, bg_reg) = make_bg_infra();
    let req = make_intercept_request(
        &content,
        &history,
        "test-session",
        &cancel,
        &sink,
        &bg_tx,
        &bg_reg,
        lookup,
    );

    let result = intercept_immediate_command(req).await;

    let InterceptOutcome::Inject(InjectPayload { text, plugin }) = result else {
        panic!("插件 slash 必须透传原文");
    };
    assert_eq!(text, "/plugin:research:report weekly");
    assert_eq!(
        plugin.as_deref(),
        Some("research"),
        "插件归属必须来自路由表 provenance，而不是命令文本反推"
    );
}
