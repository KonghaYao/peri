//! PermissionRequest hook 拒绝语义契约（红→绿）。
//!
//! 本地 PermissionRequest 输出使用顶层 decision=block/approve。
//! 未定义的事件特定输出须拒绝；拒绝触发 PermissionDenied，approve 仍交宿主。

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
        plugin_source: None,
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
    let deny = json_hook(
        HookEvent::PermissionRequest,
        r#"{"decision":"block","reason":"hook says no"}"#,
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
            assert_eq!(reason, "hook says no");
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
    let allow = json_hook(HookEvent::PermissionRequest, r#"{"decision":"approve"}"#);
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
