//! BgCommand 单元测试。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use peri_acp_types::event::ExecutorEvent;

use super::BgCommand;
use crate::session::command::{AgentCommand, CommandContext, CommandKind};
use crate::session::executor::PromptStopReason;

// ── Mock EventSink ────────────────────────────────────────────────────────

struct MockEventSink {
    events: Mutex<Vec<(String, String)>>,
    push_done_count: Mutex<usize>,
}

impl MockEventSink {
    fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
            push_done_count: Mutex::new(0),
        }
    }

    fn events(&self) -> Vec<(String, String)> {
        self.events.lock().unwrap().clone()
    }

    fn push_done_count(&self) -> usize {
        *self.push_done_count.lock().unwrap()
    }
}

#[async_trait]
impl crate::session::event_sink::EventSink for MockEventSink {
    async fn push_event(&self, session_id: &str, event: &ExecutorEvent, _context_window: u32) {
        let json = serde_json::to_string(event).unwrap_or_default();
        self.events
            .lock()
            .unwrap()
            .push((session_id.to_string(), json));
    }

    async fn push_done(&self, _session_id: &str, _stop_reason: &str, _request_id: Option<&str>) {
        *self.push_done_count.lock().unwrap() += 1;
    }
}

fn make_ctx(sink: Arc<dyn crate::session::event_sink::EventSink>, args: &str) -> CommandContext {
    CommandContext {
        session_id: "test-session".to_string(),
        history: vec![],
        cwd: "/tmp".to_string(),
        peri_config: Arc::new(Default::default()),
        auxiliary_model: None,
        event_sink: sink,
        args: args.to_string(),
        cancel_token: tokio_util::sync::CancellationToken::new(),
        thread_store: None,
        thread_id: None,
        bg_event_sender: None,
        task_manager: None,
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        frozen_system_prompt: None,
        bg_spawner: None,
    }
}

/// 构造带有效 provider 配置的 CommandContext（LLM 构造成功，能越过
/// `LlmProvider::from_config` 提前返回路径，直达 bg_event_sender/task_manager 检查）。
/// 两个 Option 字段仍为 None——复现公开 RPC 直调 /bg 传 None 的场景。
fn make_ctx_with_provider(
    sink: Arc<dyn crate::session::event_sink::EventSink>,
    args: &str,
) -> CommandContext {
    let mut peri_config = crate::provider::PeriConfig::default();
    peri_config.config.active_alias = "sonnet".to_string();
    peri_config.config.providers = vec![crate::provider::ProviderConfig {
        id: "a".to_string(),
        provider_type: "openai".to_string(),
        api_key: "sk-test".to_string(),
        models: crate::provider::ProviderModels {
            sonnet: "gpt-4o".to_string(),
            ..Default::default()
        },
        ..Default::default()
    }];
    peri_config.config.profiles = crate::provider::Profiles {
        sonnet: crate::provider::ProfileConfig {
            provider: "a".to_string(),
            ..Default::default()
        },
        ..Default::default()
    };

    CommandContext {
        session_id: "test-session".to_string(),
        history: vec![],
        cwd: "/tmp".to_string(),
        peri_config: Arc::new(peri_config),
        auxiliary_model: None,
        event_sink: sink,
        args: args.to_string(),
        cancel_token: tokio_util::sync::CancellationToken::new(),
        thread_store: None,
        thread_id: None,
        bg_event_sender: None,
        task_manager: None,
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        frozen_system_prompt: None,
        bg_spawner: None,
    }
}

// ── BgCommand 属性测试 ────────────────────────────────────────────────────

#[test]
fn test_bg_command_name_and_aliases() {
    let cmd = BgCommand;

    assert_eq!(cmd.name(), "bg");
    let aliases = cmd.aliases();
    assert!(aliases.contains(&"background"), "应包含 background 别名");
    assert_eq!(cmd.kind(), CommandKind::Immediate);
    assert!(!cmd.description().is_empty());
}

// ── 空参数测试 ────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_bg_command_empty_prompt_shows_usage() {
    let sink = Arc::new(MockEventSink::new());
    let ctx = make_ctx(sink.clone(), "");
    let cmd = BgCommand;

    let result = cmd.execute(ctx).await;

    // 应返回空消息 + EndTurn
    assert_eq!(result.messages.len(), 0);
    assert_eq!(result.stop_reason, PromptStopReason::EndTurn);

    // 应推送 TextChunk 事件包含用法信息
    let events = sink.events();
    assert_eq!(events.len(), 1);
    assert!(
        events[0].1.contains("用法"),
        "空参数应推送用法提示，实际: {}",
        events[0].1
    );
    assert!(
        events[0].1.contains("/bg"),
        "用法提示应包含命令名 /bg，实际: {}",
        events[0].1
    );
}

#[tokio::test]
async fn test_bg_command_does_not_call_push_done_itself() {
    let sink = Arc::new(MockEventSink::new());
    let ctx = make_ctx(sink.clone(), "");
    let cmd = BgCommand;

    let _result = cmd.execute(ctx).await;

    // BgCommand 自身不应调用 push_done（由 executor 负责）
    let count = sink.push_done_count();
    assert_eq!(
        count, 0,
        "BgCommand 自身不应调用 push_done，由 executor 负责"
    );
}

// ── 缺省 bg 上下文优雅降级测试（S1.2）───────────────────────────────────────

/// [S1.2] 公开 RPC（session/execute-command / session/rewind）传 None 时
/// /bg 不得 panic——两个 expect 改为 emit 错误提示 + EndTurn 返回。
#[tokio::test]
async fn test_bg_command_missing_bg_context_gracefully_fails() {
    let sink = Arc::new(MockEventSink::new());
    // 有效 provider（越过 LLM 构造检查）+ bg_event_sender/task_manager 均 None
    let ctx = make_ctx_with_provider(sink.clone(), "整理周报");
    let cmd = BgCommand;

    let result = cmd.execute(ctx).await;

    // 不 panic，正常返回 EndTurn
    assert_eq!(result.stop_reason, PromptStopReason::EndTurn);
    assert_eq!(result.messages.len(), 0);

    // 应 emit 一条错误提示，指明缺失的装配面（3.0 批 2：bg_spawner 注入面
    // 先于 bg_event_sender/thread_store 被检查——RPC 直调缺少 executor 装配面
    // 是 /bg 无法执行的根因）。
    let events = sink.events();
    assert_eq!(events.len(), 1, "应恰好 emit 一条错误提示");
    assert!(
        events[0].1.contains("后台任务启动失败"),
        "错误提示应包含失败前缀，实际: {}",
        events[0].1
    );
    assert!(
        events[0].1.contains("未配置"),
        "错误提示应指明缺失字段，实际: {}",
        events[0].1
    );
}

// ── 默认注册表测试 ────────────────────────────────────────────────────────

#[test]
fn test_default_registry_contains_bg() {
    let reg = crate::session::command::default_command_registry();
    let names: Vec<&str> = reg.list().iter().map(|(n, _, _)| *n).collect();
    assert!(names.contains(&"bg"), "默认注册表应包含 bg 命令");
}

#[test]
fn test_bg_command_registry_find() {
    let reg = crate::session::command::default_command_registry();

    // 通过名称查找
    let (cmd, args) = reg.find("/bg 帮我搜索 Rust 2026 roadmap").unwrap();
    assert_eq!(cmd.name(), "bg");
    assert_eq!(args, "帮我搜索 Rust 2026 roadmap");

    // 通过别名查找
    let (cmd, args) = reg.find("/background 调研 tokio 最新版本").unwrap();
    assert_eq!(cmd.name(), "bg");
    assert_eq!(args, "调研 tokio 最新版本");
}
