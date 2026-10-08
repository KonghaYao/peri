//! M10 阻断反馈定向测试：PostToolBatch 的 Block 回注有界反馈、continue:false
//! 走显式停止意图；两者都不得把已提交的工具结果伪装成 ToolRejected。
//!
//! 红→绿：本文件先于实现落地，Block/continue:false 在当前实现下返回
//! `Err(ToolRejected)`（伪装拒绝），断言失败；实现后必须转绿。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use super::*;
use crate::permission::{PermissionMode, SharedPermissionMode};
use peri_acp_types::system_reminder::ReminderAudience;
use peri_agent::agent::state::AgentState;

fn make_registered(event: HookEvent, hook: HookType) -> RegisteredHook {
    RegisteredHook {
        hook,
        event,
        matcher: None,
        plugin_name: "test-plugin".to_string(),
        plugin_id: "test-plugin-id".to_string(),
        plugin_root: PathBuf::from("/tmp/test-plugin"),
        plugin_data_dir: PathBuf::from("/tmp/test-plugin-data"),
        plugin_options: HashMap::new(),
    }
}

fn command_hook(command: &str) -> HookType {
    HookType::Command {
        command: command.to_string(),
        shell: None,
        timeout: Some(1000),
        status_message: None,
        once: false,
        async_run: false,
        async_rewake: false,
        matcher: None,
        condition: None,
    }
}

fn post_tool_batch_hook(stdout_json: &str) -> RegisteredHook {
    make_registered(
        HookEvent::PostToolBatch,
        command_hook(&format!("echo '{stdout_json}'")),
    )
}

fn make_middleware(hooks: Vec<RegisteredHook>) -> HookMiddleware {
    let llm_factory: Arc<dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync> =
        Arc::new(|| unimplemented!("no LLM needed in unit tests"));
    HookMiddleware::new(
        hooks,
        llm_factory,
        std::env::temp_dir().to_str().unwrap(),
        "test-session",
        "/test/transcript.json",
        SharedPermissionMode::new(PermissionMode::Bypass),
        "opus",
    )
}

fn make_state() -> AgentState {
    AgentState::new(std::env::temp_dir().to_str().unwrap())
}

fn reminder_of(
    message: &peri_agent::session::QueuedMessage,
) -> &peri_acp_types::system_reminder::SystemReminder {
    match &message.payload {
        peri_agent::session::QueuedPayload::SystemReminder(reminder) => reminder.as_reminder(),
        other => panic!("expected system reminder payload, got {other:?}"),
    }
}

/// Block：工具结果已提交后回注有界反馈，不得返回伪装拒绝（ToolRejected）。
#[tokio::test]
async fn test_post_tool_batch_block_reinjects_bounded_feedback() {
    let mw = make_middleware(vec![post_tool_batch_hook(
        r#"{"decision":"block","reason":"fix the failing test"}"#,
    )]);
    let mut state = make_state();

    let result = mw.fire_post_tool_batch(&mut state).await;
    assert!(
        result.is_ok(),
        "Block 不得伪装 ToolRejected（工具已执行）: {result:?}"
    );

    let drained = state.v2_queue().drain_batch(64);
    assert_eq!(drained.len(), 1, "Block 必须回注一条反馈");
    let message = &drained[0];
    assert_eq!(message.kind, MessageKind::Defer);
    assert_eq!(message.source, MessageSource::SystemInjected);
    assert!(
        message.policy.notifies_execution(),
        "反馈必须可见于下一次模型请求"
    );
    let reminder = reminder_of(message);
    assert!(
        reminder.audiences.0.contains(&ReminderAudience::Model),
        "反馈必须投递给 Model 受众"
    );
    assert!(
        reminder.body.contains("fix the failing test"),
        "反馈正文必须包含 hook 给出的原因: {}",
        reminder.body
    );
}

/// 防循环：连续 Block 沿用 stop_block_guard 计数，超过上限后忽略 block 不再回注。
#[tokio::test]
async fn test_post_tool_batch_block_is_bounded_by_guard() {
    let mw = make_middleware(vec![post_tool_batch_hook(
        r#"{"decision":"block","reason":"still broken"}"#,
    )]);
    let mut state = make_state();

    for attempt in 1..=8 {
        let result = mw.fire_post_tool_batch(&mut state).await;
        assert!(result.is_ok(), "第 {attempt} 次 Block 不得报错: {result:?}");
    }
    assert_eq!(
        state.v2_queue().drain_batch(64).len(),
        8,
        "上限内每次 Block 回注一条反馈"
    );

    let result = mw.fire_post_tool_batch(&mut state).await;
    assert!(result.is_ok(), "超限后忽略 block，不得报错: {result:?}");
    assert!(
        state.v2_queue().drain_batch(64).is_empty(),
        "超过防循环上限后不得继续回注"
    );
}

/// continue:false：显式停止意图经 Receive 唯一出口停止，不唤醒、不发起额外请求，
/// 也不伪装 ToolRejected。
#[tokio::test]
async fn test_post_tool_batch_continue_false_enqueues_stop_intent() {
    let mw = make_middleware(vec![post_tool_batch_hook(
        r#"{"continue":false,"stopReason":"hook requests stop"}"#,
    )]);
    let mut state = make_state();

    let result = mw.fire_post_tool_batch(&mut state).await;
    assert!(
        result.is_ok(),
        "continue:false 不得伪装 ToolRejected: {result:?}"
    );

    let drained = state.v2_queue().drain_batch(64);
    assert_eq!(drained.len(), 1, "必须投递一条显式停止意图");
    let message = &drained[0];
    assert!(
        !message.policy.notifies_execution(),
        "停止意图不得发起额外模型请求"
    );
    let reminder = reminder_of(message);
    assert!(
        !reminder.audiences.0.contains(&ReminderAudience::Model),
        "停止意图不得进入模型受众"
    );
    assert!(
        reminder.body.contains("hook requests stop"),
        "停止意图必须携带可诊断的 stop 原因: {}",
        reminder.body
    );
}

/// 无 hook 命中时 PostToolBatch 保持 no-op，也不得误用防循环计数。
#[tokio::test]
async fn test_post_tool_batch_without_hooks_is_noop() {
    let mw = make_middleware(vec![]);
    let mut state = make_state();
    let result = mw.fire_post_tool_batch(&mut state).await;
    assert!(result.is_ok());
    assert!(state.v2_queue().drain_batch(64).is_empty());
}
