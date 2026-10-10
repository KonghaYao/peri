use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::*;
use crate::permission::{PermissionMode, SharedPermissionMode};
use peri_agent::interaction::{
    ApprovalDecision, InteractionContext, InteractionResponse, UserInteractionBroker,
};

// === H4：PreToolUse 决策归并（deny 零执行 / ask 有界审批） ===

#[derive(Clone, Copy, PartialEq, Eq)]
enum BrokerReply {
    Approve,
    Reject,
    Pending,
}

struct RecordingBroker {
    calls: AtomicUsize,
    reply: BrokerReply,
    contexts: parking_lot::Mutex<Vec<InteractionContext>>,
}

impl RecordingBroker {
    fn new(reply: BrokerReply) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            reply,
            contexts: parking_lot::Mutex::new(Vec::new()),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl UserInteractionBroker for RecordingBroker {
    async fn request(&self, ctx: InteractionContext) -> InteractionResponse {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.contexts.lock().push(ctx);
        match self.reply {
            BrokerReply::Approve => {
                InteractionResponse::Decisions(vec![ApprovalDecision::Approve {
                    source: Some("test-broker".to_string()),
                }])
            }
            BrokerReply::Reject => InteractionResponse::Decisions(vec![ApprovalDecision::Reject {
                reason: "user rejected".to_string(),
                source: None,
            }]),
            BrokerReply::Pending => std::future::pending().await,
        }
    }
}

fn make_middleware_with_broker(
    hooks: Vec<RegisteredHook>,
    mode: PermissionMode,
    broker: Arc<RecordingBroker>,
) -> HookMiddleware {
    let broker: Arc<dyn UserInteractionBroker> = broker;
    HookMiddleware::new(
        hooks,
        make_llm_factory(),
        std::env::temp_dir().to_str().unwrap(),
        "test-session",
        "/test/transcript.json",
        SharedPermissionMode::new(mode),
        "opus",
    )
    .with_broker(broker)
}

fn pretooluse_command_hook(json: &str) -> RegisteredHook {
    // 单引号包裹，JSON 内的双引号原样交给 hook 脚本输出
    let command = format!("echo '{json}'");
    let hook: HookType = serde_json::from_value(serde_json::json!({
        "type": "command",
        "command": command
    }))
    .unwrap();
    make_registered(HookEvent::PreToolUse, hook)
}

fn bash_call(input: serde_json::Value) -> ToolCall {
    ToolCall::new("c1", "Bash", input)
}

#[cfg(unix)]
#[tokio::test]
async fn test_pretooluse_deny_with_updated_input_rejects_and_never_asks() {
    let broker = RecordingBroker::new(BrokerReply::Approve);
    let hook = pretooluse_command_hook(
        r#"{"hook_specific_output":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"hook says no","updatedInput":{"command":"safe-ls"}},"systemMessage":"client hint"}"#,
    );
    let mw = make_middleware_with_broker(vec![hook], PermissionMode::Bypass, broker.clone());

    let result = mw
        .before_tool(
            &mut peri_agent::agent::state::AgentState::new(std::env::temp_dir().to_str().unwrap()),
            &bash_call(serde_json::json!({"command": "rm -rf /"})),
        )
        .await;

    match result {
        Err(AgentError::ToolRejected { tool, reason }) => {
            assert_eq!(tool, "Bash");
            assert!(
                reason.contains("denied"),
                "deny 必须是固定安全反馈，实际: {reason}"
            );
            assert!(
                !reason.contains("hook says no"),
                "不得把 hook 自述文本当作拒绝反馈透传: {reason}"
            );
        }
        other => panic!("deny 必须零执行（before_tool 返回 Err），got {other:?}"),
    }
    assert_eq!(broker.calls(), 0, "deny 不得触发审批调用");
}

#[cfg(unix)]
#[tokio::test]
async fn test_pretooluse_deny_wins_over_allow_hook_in_chain() {
    let broker = RecordingBroker::new(BrokerReply::Approve);
    let allow_hook = pretooluse_command_hook(
        r#"{"hook_specific_output":{"hookEventName":"PreToolUse","permissionDecision":"allow","updatedInput":{"command":"echo ok"}}}"#,
    );
    let deny_hook = pretooluse_command_hook(
        r#"{"hook_specific_output":{"hookEventName":"PreToolUse","permissionDecision":"deny"}}"#,
    );
    let mw = make_middleware_with_broker(
        vec![allow_hook, deny_hook],
        PermissionMode::Bypass,
        broker.clone(),
    );

    let result = mw
        .before_tool(
            &mut peri_agent::agent::state::AgentState::new(std::env::temp_dir().to_str().unwrap()),
            &bash_call(serde_json::json!({"command": "rm -rf /"})),
        )
        .await;

    assert!(
        matches!(result, Err(AgentError::ToolRejected { .. })),
        "链上 deny 必须压过 allow/updatedInput，got {result:?}"
    );
    assert_eq!(broker.calls(), 0, "deny 不得触发审批调用");
}

#[cfg(unix)]
#[tokio::test]
async fn test_pretooluse_ask_without_broker_is_rejected_not_allowed() {
    // Bypass 模式：宿主权限路径不会弹窗；此时 ask 必须由 hook 层落实，
    // 无 broker 则明确拒绝，不能静默放行。
    let hook = pretooluse_command_hook(
        r#"{"hook_specific_output":{"hookEventName":"PreToolUse","permissionDecision":"ask"}}"#,
    );
    let mw = make_middleware_with_mode(vec![hook], PermissionMode::Bypass);

    let result = mw
        .before_tool(
            &mut peri_agent::agent::state::AgentState::new(std::env::temp_dir().to_str().unwrap()),
            &bash_call(serde_json::json!({"command": "ls"})),
        )
        .await;

    match result {
        Err(AgentError::ToolRejected { tool, .. }) => assert_eq!(tool, "Bash"),
        other => panic!("ask 无审批端口必须拒绝，got {other:?}"),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn test_pretooluse_ask_broker_approve_proceeds_once() {
    let broker = RecordingBroker::new(BrokerReply::Approve);
    let hook = pretooluse_command_hook(
        r#"{"hook_specific_output":{"hookEventName":"PreToolUse","permissionDecision":"ask"}}"#,
    );
    let mw = make_middleware_with_broker(vec![hook], PermissionMode::Bypass, broker.clone());

    let result = mw
        .before_tool(
            &mut peri_agent::agent::state::AgentState::new(std::env::temp_dir().to_str().unwrap()),
            &bash_call(serde_json::json!({"command": "ls"})),
        )
        .await;

    assert!(result.is_ok(), "broker 批准后应放行，got {result:?}");
    assert_eq!(broker.calls(), 1, "ask 只允许一次有界审批调用");
}

#[cfg(unix)]
#[tokio::test]
async fn test_pretooluse_ask_broker_reject_is_rejected() {
    let broker = RecordingBroker::new(BrokerReply::Reject);
    let hook = pretooluse_command_hook(
        r#"{"hook_specific_output":{"hookEventName":"PreToolUse","permissionDecision":"ask"}}"#,
    );
    let mw = make_middleware_with_broker(vec![hook], PermissionMode::Bypass, broker.clone());

    let result = mw
        .before_tool(
            &mut peri_agent::agent::state::AgentState::new(std::env::temp_dir().to_str().unwrap()),
            &bash_call(serde_json::json!({"command": "ls"})),
        )
        .await;

    assert!(result.is_err(), "broker 拒绝必须零执行，got {result:?}");
    assert_eq!(broker.calls(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn test_pretooluse_ask_broker_timeout_is_rejected() {
    let broker = RecordingBroker::new(BrokerReply::Pending);
    let hook = pretooluse_command_hook(
        r#"{"hook_specific_output":{"hookEventName":"PreToolUse","permissionDecision":"ask"}}"#,
    );
    let broker_dyn: Arc<dyn UserInteractionBroker> = broker.clone();
    let mw = HookMiddleware::new(
        vec![hook],
        make_llm_factory(),
        std::env::temp_dir().to_str().unwrap(),
        "test-session",
        "/test/transcript.json",
        SharedPermissionMode::new(PermissionMode::Bypass),
        "opus",
    )
    .with_broker(broker_dyn)
    .with_broker_timeout(std::time::Duration::from_millis(50));

    let result = mw
        .before_tool(
            &mut peri_agent::agent::state::AgentState::new(std::env::temp_dir().to_str().unwrap()),
            &bash_call(serde_json::json!({"command": "ls"})),
        )
        .await;

    assert!(result.is_err(), "ask 审批超时必须拒绝，got {result:?}");
    assert_eq!(broker.calls(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn test_pretooluse_ask_defers_to_host_dialog_without_double_approval() {
    // Default 模式 + 敏感工具 + 宿主审批路径在链上：宿主权限路径会弹审批，
    // hook 层不得再弹一次（装配点按 Permission 面存在性注入 host_approval_path）。
    let broker = RecordingBroker::new(BrokerReply::Approve);
    let hook = pretooluse_command_hook(
        r#"{"hook_specific_output":{"hookEventName":"PreToolUse","permissionDecision":"ask"}}"#,
    );
    let mw = make_middleware_with_broker(vec![hook], PermissionMode::Default, broker.clone())
        .with_host_approval_path(true);

    let result = mw
        .before_tool(
            &mut peri_agent::agent::state::AgentState::new(std::env::temp_dir().to_str().unwrap()),
            &bash_call(serde_json::json!({"command": "ls"})),
        )
        .await;

    assert!(result.is_ok(), "宿主会审批时应交宿主路径，got {result:?}");
    assert_eq!(broker.calls(), 0, "不得与宿主权限形成双审批");
}

#[cfg(unix)]
#[tokio::test]
async fn test_pretooluse_allow_does_not_bypass_host_permission() {
    // Default 模式 + 敏感工具：hook allow 只表示不拒绝，宿主上限仍然生效
    let broker = RecordingBroker::new(BrokerReply::Approve);
    let hook = pretooluse_command_hook(
        r#"{"hook_specific_output":{"hookEventName":"PreToolUse","permissionDecision":"allow"}}"#,
    );
    let mw = make_middleware_with_broker(vec![hook], PermissionMode::Default, broker.clone());

    let result = mw
        .before_tool(
            &mut peri_agent::agent::state::AgentState::new(std::env::temp_dir().to_str().unwrap()),
            &bash_call(serde_json::json!({"command": "ls"})),
        )
        .await;

    assert!(result.is_ok(), "allow 后仍交宿主权限路径，got {result:?}");
    assert_eq!(broker.calls(), 0, "hook allow 不得代替宿主审批");
}
