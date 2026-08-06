//! BgCommand 单元测试（L5：自 peri-acp/src/host/exec/bg_test.rs 随迁，
//! 断言语义不重写；注册表装配面测试（default_command_registry 含 bg /
//! find 解析）留 ACP——依赖 ACP 命令注册表）。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use peri_acp_types::command::{AgentCommand, CommandContext, CommandKind, PromptStopReason};
use peri_acp_types::compact::CompactConfig;
use peri_acp_types::event::{EventSink, ExecutorEvent};

use super::BgCommand;

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
impl EventSink for MockEventSink {
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

fn make_ctx(sink: Arc<dyn EventSink>, args: &str) -> CommandContext {
    CommandContext {
        session_id: "test-session".to_string(),
        history: vec![],
        cwd: "/tmp".to_string(),
        compact_config: CompactConfig::default(),
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
    // bg_spawner / bg_event_sender / task_manager 均 None（RPC 直调缺装配面）
    let ctx = make_ctx(sink.clone(), "整理周报");
    let cmd = BgCommand;

    let result = cmd.execute(ctx).await;

    // 不 panic，正常返回 EndTurn
    assert_eq!(result.stop_reason, PromptStopReason::EndTurn);
    assert_eq!(result.messages.len(), 0);

    // 应 emit 一条错误提示，指明缺失的装配面（bg_spawner 注入面
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
