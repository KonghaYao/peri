//! M10 字段路由定向测试：`additionalContext` / `systemMessage` /
//! `initialUserMessage` 必须投递到声明受众，而不是"解析成功即假装生效"。
//!
//! 红→绿：实现前这些字段只被诊断后丢弃；实现后必须出现在 canonical queue 里，
//! 且受众、来源与有界性可断言。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use super::*;
use crate::permission::{PermissionMode, SharedPermissionMode};
use peri_acp_types::session::QueuedPayload;
use peri_acp_types::system_reminder::ReminderAudience;
use peri_agent::agent::state::AgentState;

fn registered(event: HookEvent, stdout_json: &str) -> RegisteredHook {
    RegisteredHook {
        hook: HookType::Command {
            command: format!("echo '{stdout_json}'"),
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
        plugin_name: "delivery-plugin".to_string(),
        plugin_id: "delivery-plugin-id".to_string(),
        plugin_root: PathBuf::from("/tmp/delivery-plugin"),
        plugin_data_dir: PathBuf::from("/tmp/delivery-plugin-data"),
        plugin_options: HashMap::new(),
    }
}

fn make_middleware(hooks: Vec<RegisteredHook>, session_start: Option<&str>) -> HookMiddleware {
    let llm_factory: Arc<dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync> =
        Arc::new(|| unimplemented!("no LLM needed in delivery tests"));
    HookMiddleware::with_session_start(
        hooks,
        llm_factory,
        std::env::temp_dir().to_str().unwrap(),
        "delivery-session",
        "/test/transcript.json",
        SharedPermissionMode::new(PermissionMode::Bypass),
        "opus",
        session_start.map(str::to_string),
    )
}

fn state_with_prompt() -> AgentState {
    let mut state = AgentState::new(std::env::temp_dir().to_str().unwrap());
    state.add_message(BaseMessage::human("hello"));
    state
}

fn reminders(state: &AgentState) -> Vec<peri_acp_types::system_reminder::SystemReminder> {
    state
        .v2_queue()
        .drain_all()
        .into_iter()
        .filter_map(|message| match message.payload {
            QueuedPayload::SystemReminder(reminder) => Some(reminder.into_inner()),
            QueuedPayload::Message(_) => None,
        })
        .collect()
}

/// `additionalContext` 必须成为带 hook 来源的 Model reminder。
#[cfg(unix)]
#[tokio::test]
async fn additional_context_becomes_a_sourced_model_reminder() {
    let hook = registered(
        HookEvent::UserPromptSubmit,
        r#"{"hook_specific_output":{"hookEventName":"UserPromptSubmit","additionalContext":"remember the rule"}}"#,
    );
    let mw = make_middleware(vec![hook], None);
    let mut state = state_with_prompt();

    mw.before_agent(&mut state).await.unwrap();

    let reminders = reminders(&state);
    assert_eq!(reminders.len(), 1, "additionalContext 必须投递恰好一次");
    let reminder = &reminders[0];
    assert_eq!(reminder.source.0, "hook", "投递必须保留 hook 来源");
    assert_eq!(reminder.kind, "user_prompt_submit_additional_context");
    assert!(reminder.audiences.contains(ReminderAudience::Model));
    assert!(!reminder.audiences.contains(ReminderAudience::Tui));
    assert!(reminder.body.contains("remember the rule"));
    assert_eq!(reminder.metadata["bytes"], 17);
}

/// `systemMessage` 是客户端提示：Tui 受众、不进模型上下文。
#[cfg(unix)]
#[tokio::test]
async fn system_message_is_a_client_notice_without_model_audience() {
    let hook = registered(
        HookEvent::UserPromptSubmit,
        r#"{"systemMessage":"heads up"}"#,
    );
    let mw = make_middleware(vec![hook], None);
    let mut state = state_with_prompt();

    mw.before_agent(&mut state).await.unwrap();

    let reminders = reminders(&state);
    assert_eq!(reminders.len(), 1, "systemMessage 必须投递恰好一次");
    let reminder = &reminders[0];
    assert_eq!(reminder.source.0, "hook");
    assert_eq!(reminder.kind, "user_prompt_submit_system_message");
    assert!(reminder.audiences.contains(ReminderAudience::Tui));
    assert!(!reminder.audiences.contains(ReminderAudience::Model));
    assert!(reminder.body.contains("heads up"));
}

/// 超长 hook 输出按字符边界截断并显式标记，不静默裁掉。
#[cfg(unix)]
#[tokio::test]
async fn oversized_hook_output_is_bounded_with_an_explicit_marker() {
    let long = "汉".repeat(MAX_HOOK_OUTPUT_BYTES);
    let hook = registered(
        HookEvent::UserPromptSubmit,
        &format!(
            r#"{{"hook_specific_output":{{"hookEventName":"UserPromptSubmit","additionalContext":"{long}"}}}}"#
        ),
    );
    let mw = make_middleware(vec![hook], None);
    let mut state = state_with_prompt();

    mw.before_agent(&mut state).await.unwrap();

    let reminders = reminders(&state);
    assert_eq!(reminders.len(), 1);
    let body = &reminders[0].body;
    assert!(body.contains("[hook output truncated:"), "截断必须显式标记");
    assert!(body.len() < long.len(), "截断后的正文必须真的更小");
    assert!(
        body.starts_with(&"汉".repeat(MAX_HOOK_OUTPUT_BYTES / 3)),
        "保留前缀必须落在字符边界且不超过承载预算"
    );
    assert_eq!(
        reminders[0].metadata["bytes"].as_u64().unwrap(),
        long.len() as u64,
        "metadata 记录原始字节数"
    );
    assert!(body.is_char_boundary(body.len()));
}

/// `initialUserMessage` 每个会话至多准入一次，且不伪装成用户输入。
#[cfg(unix)]
#[tokio::test]
async fn initial_user_message_is_admitted_once_per_session() {
    let hook = registered(
        HookEvent::SessionStart,
        r#"{"hook_specific_output":{"hookEventName":"SessionStart","initialUserMessage":"bootstrap instruction"}}"#,
    );
    let mw = make_middleware(vec![hook], Some("startup"));
    let mut first = state_with_prompt();
    let mut second = state_with_prompt();

    mw.before_agent(&mut first).await.unwrap();
    mw.before_agent(&mut second).await.unwrap();

    let first_reminders = reminders(&first);
    let second_reminders = reminders(&second);
    assert_eq!(first_reminders.len(), 1, "首次 SessionStart 必须准入");
    assert_eq!(
        first_reminders[0].kind, "session_start_initial_user_message",
        "初始消息保留 hook 来源的可路由身份"
    );
    assert_eq!(first_reminders[0].source.0, "hook");
    assert!(first_reminders[0]
        .audiences
        .contains(ReminderAudience::Model));
    assert!(
        second_reminders.is_empty(),
        "同一会话第二次 SessionStart 不得重复准入"
    );
}
