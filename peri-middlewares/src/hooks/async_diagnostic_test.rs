//! M10 异步 hook 终态诊断（红→绿）。
//!
//! 异步 hook 是**非决策** hook：结果不追溯改变授权、不投递未定义的 context，
//! 但也不能"只把所有结果当 Allow"静默丢弃。本文件用 warn 捕获订阅者断言
//! 完成/决策忽略/未定义投递的可诊断性（诊断不得携带 hook 正文）。
//!
//! 本文件共 4 例（3 个 `#[tokio::test]` + 1 个分类器 `#[test]`）。
//! [TRAP] 证据命令必须用**模块过滤** `--lib async_diagnostic_tests`；用
//! `async_hook` 名称前缀过滤只会跑到 3 个 tokio 用例，分类器用例漏网，
//! 日志与"4 passed"的文字声明对不上。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::*;
use crate::permission::{PermissionMode, SharedPermissionMode};

/// 只捕获 WARN 事件（同 `mcp/initialize_test.rs` 既有形态）。
struct WarnCaptureSubscriber {
    warns: Arc<Mutex<Vec<String>>>,
}

impl tracing::Subscriber for WarnCaptureSubscriber {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        *metadata.level() == tracing::Level::WARN
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
        self.warns.lock().unwrap().push(fields.0);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

fn registered(event: HookEvent, hook: HookType) -> RegisteredHook {
    RegisteredHook {
        hook,
        event,
        matcher: None,
        plugin_name: "async-test-plugin".to_string(),
        plugin_id: "async-test-plugin-id".to_string(),
        plugin_source: None,
        plugin_root: PathBuf::from("/tmp/async-test-plugin"),
        plugin_data_dir: PathBuf::from("/tmp/async-test-plugin-data"),
        plugin_options: HashMap::new(),
    }
}

fn async_command_hook(command: &str) -> HookType {
    HookType::Command {
        command: command.to_string(),
        shell: None,
        timeout: Some(1000),
        status_message: None,
        once: false,
        async_run: true,
        async_rewake: false,
        matcher: None,
        condition: None,
    }
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

fn session_input() -> HookInput {
    HookInput::session_start(
        "s",
        "/test/transcript.json",
        std::env::temp_dir().to_str().unwrap(),
        "startup",
        "opus",
    )
}

async fn wait_for_warn(warns: &Arc<Mutex<Vec<String>>>) -> String {
    for _ in 0..200 {
        {
            let captured = warns.lock().unwrap();
            if !captured.is_empty() {
                return captured.join("\n");
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    warns.lock().unwrap().join("\n")
}

/// 异步 hook 产出 `systemMessage`（异步路径没有投递定义）：不投递、不生效，
/// 但必须留下可诊断的 warn，且诊断不得携带正文。
#[tokio::test]
async fn async_hook_undelivered_output_is_diagnosed_without_body() {
    let warns = Arc::new(Mutex::new(Vec::new()));
    let _guard = tracing::subscriber::set_default(WarnCaptureSubscriber {
        warns: Arc::clone(&warns),
    });
    let mw = make_middleware(vec![registered(
        HookEvent::Notification,
        async_command_hook(r#"echo '{"systemMessage":"secret-body"}'"#),
    )]);

    let action = mw
        .fire_event(HookEvent::Notification, &session_input(), None, None)
        .await;
    assert!(
        matches!(action, HookAction::Allow),
        "异步 hook 不得即时产生决策: {action:?}"
    );

    let captured = wait_for_warn(&warns).await;
    assert!(
        captured.contains("undelivered_output"),
        "异步未定义投递的输出必须留下结构化诊断: {captured}"
    );
    assert!(
        !captured.contains("secret-body"),
        "诊断不得携带 hook 正文: {captured}"
    );
}

/// 异步 hook 产出决策（`decision=block`）：异步是非决策路径，结果不生效，
/// 但必须记录为被忽略的决策而不是静默丢弃。
#[tokio::test]
async fn async_hook_decision_is_recorded_as_ignored() {
    let warns = Arc::new(Mutex::new(Vec::new()));
    let _guard = tracing::subscriber::set_default(WarnCaptureSubscriber {
        warns: Arc::clone(&warns),
    });
    let mw = make_middleware(vec![registered(
        HookEvent::Notification,
        async_command_hook(r#"echo '{"decision":"block","reason":"block-body"}'"#),
    )]);

    let action = mw
        .fire_event(HookEvent::Notification, &session_input(), None, None)
        .await;
    assert!(
        matches!(action, HookAction::Allow),
        "异步决策不得影响本次归并: {action:?}"
    );

    let captured = wait_for_warn(&warns).await;
    assert!(
        captured.contains("decision_ignored"),
        "异步决策必须记录为被忽略（非决策 hook）: {captured}"
    );
    assert!(
        !captured.contains("block-body"),
        "诊断不得携带 hook 正文: {captured}"
    );
}

/// 无输出的异步 hook 正常完成：不得产生 warn 噪声（完成记录走 debug）。
#[tokio::test]
async fn async_hook_without_output_completes_without_warn_noise() {
    let warns = Arc::new(Mutex::new(Vec::new()));
    let _guard = tracing::subscriber::set_default(WarnCaptureSubscriber {
        warns: Arc::clone(&warns),
    });
    let mw = make_middleware(vec![registered(
        HookEvent::Notification,
        async_command_hook("echo done"),
    )]);

    let action = mw
        .fire_event(HookEvent::Notification, &session_input(), None, None)
        .await;
    assert!(matches!(action, HookAction::Allow));

    // 等待异步任务有时间完成（并确保它没有写 warn）
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let captured = warns.lock().unwrap().join("\n");
    assert!(
        captured.is_empty(),
        "无输出的异步完成不得产生 warn 噪声: {captured}"
    );
}

/// 分类器：决策与未投递字段同时存在时都必须记录（不互相吞并），且无输出保持空摘要。
#[test]
fn async_output_summary_keeps_decision_and_undelivered_fields_together() {
    use crate::hooks::dispatcher::async_hook_output_summary;

    let combined = HookAction::PermissionOverride {
        decision: PermissionDecision::Deny,
        reason: Some("ignored-body".to_string()),
        updated_input: None,
        additional_context: Some("abcd".to_string()),
        system_message: Some("xy".to_string()),
    };
    let summary = async_hook_output_summary(&combined);
    assert_eq!(summary.decision, Some("permission_override"));
    assert_eq!(
        summary.undelivered,
        vec!["additionalContext", "systemMessage"],
        "未定义投递字段必须逐一记录"
    );
    assert_eq!(summary.undelivered_bytes, 6, "只记录长度，不记录正文");

    let none = async_hook_output_summary(&HookAction::Allow);
    assert_eq!(
        none,
        crate::hooks::dispatcher::AsyncHookOutputSummary::default()
    );

    let block = async_hook_output_summary(&HookAction::Block {
        reason: "r".to_string(),
    });
    assert_eq!(block.decision, Some("block"));
    assert!(block.undelivered.is_empty());

    let context = async_hook_output_summary(&HookAction::AdditionalContext {
        context: "ctx".to_string(),
    });
    assert_eq!(context.decision, None);
    assert_eq!(context.undelivered, vec!["additionalContext"]);
    assert_eq!(context.undelivered_bytes, 3);
}
