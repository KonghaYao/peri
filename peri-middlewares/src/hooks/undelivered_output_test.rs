//! 已解析但投递未接线字段的出口诊断契约（红→绿）。
//!
//! `additionalContext` / `systemMessage` 在 C 组只完成类型与可诊断状态（投递由
//! F 组接线）。任何事件（UserPromptSubmit / SessionStart / PreToolUse / …）的
//! 归并结果都必须在 `fire_event` 唯一出口可诊断一次：不得静默丢弃，不得重复
//! 诊断，也不得把正文写进日志。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::*;
use crate::permission::{PermissionMode, SharedPermissionMode};

/// 捕获 DEBUG 事件字段（正文只以字段名 + 字节数出现）。
struct DebugCaptureSubscriber {
    events: Arc<Mutex<Vec<String>>>,
}

impl tracing::Subscriber for DebugCaptureSubscriber {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        *metadata.level() == tracing::Level::DEBUG
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(0)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Fields(String);
        impl tracing::field::Visit for Fields {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if !self.0.is_empty() {
                    self.0.push(' ');
                }
                self.0.push_str(&format!("{}={value:?}", field.name()));
            }
        }
        let mut fields = Fields(String::new());
        event.record(&mut fields);
        self.events.lock().unwrap().push(fields.0);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

fn hook_with_command(event: HookEvent, json: &str) -> RegisteredHook {
    RegisteredHook {
        hook: HookType::Command {
            command: format!("echo '{json}'"),
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
        plugin_name: "undelivered-plugin".to_string(),
        plugin_id: "undelivered-plugin-id".to_string(),
        plugin_source: None,
        plugin_root: PathBuf::from("/tmp/undelivered-plugin"),
        plugin_data_dir: PathBuf::from("/tmp/undelivered-plugin-data"),
        plugin_options: HashMap::new(),
    }
}

fn make_middleware(hooks: Vec<RegisteredHook>) -> HookMiddleware {
    let llm_factory: Arc<dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync> =
        Arc::new(|| unimplemented!("no LLM needed in undelivered-output tests"));
    HookMiddleware::new(
        hooks,
        llm_factory,
        std::env::temp_dir().to_str().unwrap(),
        "undelivered-session",
        "/test/transcript.json",
        SharedPermissionMode::new(PermissionMode::Bypass),
        "opus",
    )
}

fn captured() -> (Arc<Mutex<Vec<String>>>, DebugCaptureSubscriber) {
    let events = Arc::new(Mutex::new(Vec::new()));
    (
        Arc::clone(&events),
        DebugCaptureSubscriber {
            events: Arc::clone(&events),
        },
    )
}

fn joined(events: &Arc<Mutex<Vec<String>>>) -> String {
    events.lock().unwrap().join("\n")
}

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

/// UserPromptSubmit 的 additionalContext 不得静默丢弃：出口必须诊断字段名与字节数。
#[cfg(unix)]
#[tokio::test]
async fn user_prompt_submit_additional_context_is_diagnosed_without_body() {
    let (events, subscriber) = captured();
    let _guard = tracing::subscriber::set_default(subscriber);
    let hook = hook_with_command(
        HookEvent::UserPromptSubmit,
        r#"{"hook_specific_output":{"hookEventName":"UserPromptSubmit","additionalContext":"secret-body"}}"#,
    );
    let mw = make_middleware(vec![hook]);

    mw.fire_event(
        HookEvent::UserPromptSubmit,
        &HookInput::user_prompt_submit("s", "/t", "/tmp", "prompt"),
        None,
        None,
    )
    .await;

    let text = joined(&events);
    assert!(
        count_occurrences(&text, "additionalContext") >= 1,
        "UserPromptSubmit 的 additionalContext 必须可诊断，captured: {text}"
    );
    assert!(
        !text.contains("secret-body"),
        "诊断不得携带 hook 正文: {text}"
    );
}

/// PreToolUse 的 PermissionOverride 字段只能在出口诊断一次（不得站点重复诊断）。
#[cfg(unix)]
#[tokio::test]
async fn permission_override_fields_are_diagnosed_exactly_once() {
    let (events, subscriber) = captured();
    let _guard = tracing::subscriber::set_default(subscriber);
    let hook = hook_with_command(
        HookEvent::PreToolUse,
        r#"{"hook_specific_output":{"hookEventName":"PreToolUse","permissionDecision":"allow","additionalContext":"ctx-body"},"systemMessage":"msg-body"}"#,
    );
    let mw = make_middleware(vec![hook]);

    mw.fire_event(
        HookEvent::PreToolUse,
        &HookInput::tool_call(
            "s",
            "/t",
            "/tmp",
            "Default",
            "Bash",
            &serde_json::json!({"command": "ls"}),
            "c1",
        ),
        Some("Bash"),
        Some(&serde_json::json!({"command": "ls"})),
    )
    .await;

    let text = joined(&events);
    assert_eq!(
        count_occurrences(&text, "additionalContext"),
        1,
        "additionalContext 必须恰好诊断一次，captured: {text}"
    );
    assert_eq!(
        count_occurrences(&text, "systemMessage"),
        1,
        "systemMessage 必须恰好诊断一次，captured: {text}"
    );
    assert!(
        !text.contains("ctx-body") && !text.contains("msg-body"),
        "诊断不得携带 hook 正文: {text}"
    );
}
