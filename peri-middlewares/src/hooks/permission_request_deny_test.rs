//! PermissionRequest hook 拒绝语义契约（红→绿）。
//!
//! PermissionRequest 的 `permissionDecision: deny` 归并为
//! [`HookAction::PermissionOverride`]（不是 Block），`resolve_action_to_toolcall`
//! 不认识该变体——若不显式处理，hook 拒绝了权限但工具仍然放行。本文件固化：
//! deny/非法判定零执行，并触发 PermissionDenied；allow 不得被连坐拒绝。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use super::*;
use crate::permission::{PermissionMode, SharedPermissionMode};
use peri_agent::agent::react::ToolCall;
use peri_agent::error::AgentError;
use peri_agent::middleware::r#trait::Middleware;

fn hook_with_command(event: HookEvent, command: &str) -> RegisteredHook {
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
        event,
        matcher: None,
        plugin_name: "permission-deny-plugin".to_string(),
        plugin_id: "permission-deny-plugin-id".to_string(),
        plugin_root: PathBuf::from("/tmp/permission-deny-plugin"),
        plugin_data_dir: PathBuf::from("/tmp/permission-deny-plugin-data"),
        plugin_options: HashMap::new(),
    }
}

fn json_hook(event: HookEvent, json: &str) -> RegisteredHook {
    hook_with_command(event, &format!("echo '{json}'"))
}

fn unique_marker(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "peri-permission-request-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("permission-denied-fired")
}

fn make_middleware(hooks: Vec<RegisteredHook>) -> HookMiddleware {
    let llm_factory: Arc<dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync> =
        Arc::new(|| unimplemented!("no LLM needed in permission-deny tests"));
    HookMiddleware::new(
        hooks,
        llm_factory,
        std::env::temp_dir().to_str().unwrap(),
        "permission-deny-session",
        "/test/transcript.json",
        SharedPermissionMode::new(PermissionMode::Default),
        "opus",
    )
}

async fn before_tool(mw: &HookMiddleware, call: &ToolCall) -> AgentResult<ToolCall> {
    mw.before_tool(
        &mut peri_agent::agent::state::AgentState::new(std::env::temp_dir().to_str().unwrap()),
        call,
    )
    .await
}

/// PermissionRequest 的 deny 必须零执行，并触发 PermissionDenied。
#[cfg(unix)]
#[tokio::test]
async fn permission_request_deny_is_zero_execution_and_fires_permission_denied() {
    let marker = unique_marker("deny");
    // PO 形状的 PermissionOverride（output_parser 只为 PreToolUse 形状产出该变体）；
    // 归并结果到达 PermissionRequest 分支时同样必须零执行。
    let deny = json_hook(
        HookEvent::PermissionRequest,
        r#"{"hook_specific_output":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"hook says no"}}"#,
    );
    let denied_watch = hook_with_command(
        HookEvent::PermissionDenied,
        &format!("touch {}", marker.display()),
    );
    let mw = make_middleware(vec![deny, denied_watch]);

    let result = before_tool(
        &mw,
        &ToolCall::new("c1", "Bash", serde_json::json!({"command": "rm -rf /"})),
    )
    .await;

    match result {
        Err(AgentError::ToolRejected { reason, .. }) => {
            assert!(
                !reason.contains("hook says no"),
                "拒绝反馈不得透传 hook 自述文本: {reason}"
            );
        }
        other => panic!("PermissionRequest deny 必须零执行，got {other:?}"),
    }

    for _ in 0..100 {
        if marker.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        marker.exists(),
        "hook 拒绝权限必须触发 PermissionDenied 事件"
    );
}

/// PermissionRequest 的 allow 仍交宿主（本层不得连坐拒绝）。
#[cfg(unix)]
#[tokio::test]
async fn permission_request_allow_still_proceeds_to_host() {
    let allow = json_hook(
        HookEvent::PermissionRequest,
        r#"{"hook_specific_output":{"hookEventName":"PreToolUse","permissionDecision":"allow"}}"#,
    );
    let mw = make_middleware(vec![allow]);

    let result = before_tool(
        &mw,
        &ToolCall::new("c1", "Bash", serde_json::json!({"command": "ls"})),
    )
    .await;

    assert!(
        result.is_ok(),
        "PermissionRequest allow 不得代替/阻断宿主权限路径，got {result:?}"
    );
}
