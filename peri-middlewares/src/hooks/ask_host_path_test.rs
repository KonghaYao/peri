//! PreToolUse `ask` 的宿主审批路径存在性契约（红→绿）。
//!
//! `should_fire_permission_request_*` 只回答"这个工具/模式在宿主权限面存在时
//! 会不会弹窗"；它不回答"宿主权限面是否真的在链上"。MetaHarness 关闭
//! PermissionMiddleware 后，Default 模式也不会有人弹审批——此时 hook 层若把
//! ask 交宿主，就是零审批放行。本文件固化：宿主审批路径缺失时 ask 必须走
//! broker；无 broker 必须明确拒绝。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::*;
use crate::hooks::types::{HookEvent, HookType, RegisteredHook};
use crate::permission::{PermissionMode, SharedPermissionMode};
use peri_agent::agent::react::ToolCall;
use peri_agent::error::AgentError;
use peri_agent::interaction::{
    ApprovalDecision, InteractionContext, InteractionResponse, UserInteractionBroker,
};
use peri_agent::middleware::r#trait::Middleware;

/// 只记录调用次数的审批端口；批准回复。
struct RecordingBroker {
    calls: AtomicUsize,
}

impl RecordingBroker {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl UserInteractionBroker for RecordingBroker {
    async fn request(&self, _ctx: InteractionContext) -> InteractionResponse {
        self.calls.fetch_add(1, Ordering::SeqCst);
        InteractionResponse::Decisions(vec![ApprovalDecision::Approve {
            source: Some("ask-host-path-test".to_string()),
        }])
    }
}

/// PreToolUse hook 固定返回 `ask`。
fn ask_hook() -> RegisteredHook {
    let command = r#"echo '{"hook_specific_output":{"hookEventName":"PreToolUse","permissionDecision":"ask"}}'"#;
    RegisteredHook {
        hook: HookType::Command {
            command: command.to_string(),
            shell: None,
            timeout: Some(1000),
            status_message: None,
            once: false,
            async_run: false,
            async_rewake: false,
            matcher: None,
            condition: None,
        },
        event: HookEvent::PreToolUse,
        matcher: None,
        plugin_name: "ask-host-path-plugin".to_string(),
        plugin_id: "ask-host-path-plugin-id".to_string(),
        plugin_root: std::path::PathBuf::from("/tmp/ask-host-path-plugin"),
        plugin_data_dir: std::path::PathBuf::from("/tmp/ask-host-path-plugin-data"),
        plugin_options: std::collections::HashMap::new(),
    }
}

fn make_middleware(hooks: Vec<RegisteredHook>, mode: PermissionMode) -> HookMiddleware {
    let llm_factory: Arc<dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync> =
        Arc::new(|| unimplemented!("no LLM needed in ask-host-path tests"));
    HookMiddleware::new(
        hooks,
        llm_factory,
        std::env::temp_dir().to_str().unwrap(),
        "ask-host-path-session",
        "/test/transcript.json",
        SharedPermissionMode::new(mode),
        "opus",
    )
}

fn bash_call() -> ToolCall {
    ToolCall::new("c1", "Bash", serde_json::json!({"command": "ls"}))
}

/// Permission 面不在链上（MetaHarness 关闭）+ Default + ask + 无 broker：
/// 没有人会弹审批，必须明确拒绝，绝不能静默放行。
#[cfg(unix)]
#[tokio::test]
async fn ask_without_host_approval_path_and_without_broker_is_rejected() {
    let mw = make_middleware(vec![ask_hook()], PermissionMode::Default);

    let result = mw
        .before_tool(
            &mut peri_agent::agent::state::AgentState::new(std::env::temp_dir().to_str().unwrap()),
            &bash_call(),
        )
        .await;

    assert!(
        matches!(result, Err(AgentError::ToolRejected { .. })),
        "宿主审批路径缺失时 ask 必须拒绝，got {result:?}"
    );
}

/// 宿主审批路径缺失 + Default + ask + 有 broker：必须由 hook 层有界审批，
/// 不能因为"模式是 Default"就假装宿主会弹窗。
#[cfg(unix)]
#[tokio::test]
async fn ask_without_host_approval_path_uses_broker_when_present() {
    let broker = RecordingBroker::new();
    let mw = make_middleware(vec![ask_hook()], PermissionMode::Default)
        .with_broker(Arc::clone(&broker) as Arc<dyn UserInteractionBroker>);

    let result = mw
        .before_tool(
            &mut peri_agent::agent::state::AgentState::new(std::env::temp_dir().to_str().unwrap()),
            &bash_call(),
        )
        .await;

    assert!(result.is_ok(), "broker 批准后应放行，got {result:?}");
    assert_eq!(broker.calls(), 1, "ask 必须恰好走一次有界审批");
}

/// 宿主审批路径在链上 + Default + ask：交宿主弹窗，hook 层不得双审批。
#[cfg(unix)]
#[tokio::test]
async fn ask_with_host_approval_path_defers_to_host_without_broker_call() {
    let broker = RecordingBroker::new();
    let mw = make_middleware(vec![ask_hook()], PermissionMode::Default)
        .with_host_approval_path(true)
        .with_broker(Arc::clone(&broker) as Arc<dyn UserInteractionBroker>);

    let result = mw
        .before_tool(
            &mut peri_agent::agent::state::AgentState::new(std::env::temp_dir().to_str().unwrap()),
            &bash_call(),
        )
        .await;

    assert!(result.is_ok(), "宿主会审批时应交宿主路径，got {result:?}");
    assert_eq!(broker.calls(), 0, "不得与宿主权限形成双审批");
}
