//! Tests for execute_command

use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::{event::ExecutorEvent, messages::BaseMessage};
use peri_agent::thread::FilesystemThreadStore;
use peri_controller::Controller;
use tokio_util::sync::CancellationToken as AgentCancellationToken;

use super::*;
use crate::{provider::PeriConfig, session::event_sink::EventSink};

/// 令 `/compact` 的无模型错误事件保持 pending，确保外层取消分支获选。
struct PendingEventSink;

#[async_trait]
impl EventSink for PendingEventSink {
    async fn push_event(&self, _session_id: &str, _event: &ExecutorEvent, _context_window: u32) {
        std::future::pending::<()>().await;
    }

    async fn push_done(&self, _session_id: &str, _stop_reason: &str, _request_id: Option<&str>) {}
}

#[test]
fn test_extract_params_basic() {
    let params = serde_json::json!({
        "sessionId": "s1",
        "command": "/clear",
        "args": "do something"
    });
    let (sid, cmd, args) = extract_execute_command_params(&params).unwrap();
    assert_eq!(sid, "s1");
    assert_eq!(cmd, "/clear");
    assert_eq!(args.as_str().unwrap(), "do something");
}

#[test]
fn test_extract_params_session_id_underscore() {
    let params = serde_json::json!({
        "session_id": "s2",
        "command": "/compact"
    });
    let (sid, cmd, args) = extract_execute_command_params(&params).unwrap();
    assert_eq!(sid, "s2");
    assert_eq!(cmd, "/compact");
    assert!(args.is_null());
}

#[test]
fn test_extract_params_missing_session_id() {
    let params = serde_json::json!({
        "command": "/clear"
    });
    let err = extract_execute_command_params(&params).unwrap_err();
    assert_eq!(err.code, -32602);
    assert!(err.message.contains("sessionId"));
}

#[test]
fn test_extract_params_missing_command() {
    let params = serde_json::json!({
        "sessionId": "s1"
    });
    let err = extract_execute_command_params(&params).unwrap_err();
    assert_eq!(err.code, -32602);
    assert!(err.message.contains("command"));
}

#[test]
fn test_extract_params_json_args() {
    let params = serde_json::json!({
        "sessionId": "s1",
        "command": "/rewind",
        "args": { "target_message_id": "abc", "revert_files": true }
    });
    let (sid, cmd, args) = extract_execute_command_params(&params).unwrap();
    assert_eq!(sid, "s1");
    assert_eq!(cmd, "/rewind");
    assert_eq!(args["target_message_id"], "abc");
    assert_eq!(args["revert_files"], true);
}

/// P2-2：RPC 路径 resolve 严格精确——未注册命令 / 前缀缩写 resolve 为 None
/// （execute_command 内建注册表，Inject/Delegate 分支不可达；锁此断言即锁定
/// 「RPC 显式报错 ≠ prompt fall-through」的语义区分，设计 §55/§78）。
#[test]
fn test_resolve_unknown_command_is_none() {
    let registry = CommandRegistry::new();
    register_builtins(&registry);
    assert!(registry.resolve("/unknown_cmd").is_none());
    // 旧 `find` 唯一前缀展开下 `/rew` 命中 rewind；resolve 严格精确下为 None
    // （行为变化已明示于 phase2-integration.md「五、行为变化明示」第 2 条）。
    assert!(registry.resolve("/rew").is_none());
    // Step 8 同步核对：prompt 路径 fall-through 的两个代表输入在 RPC 路径
    // resolve 同样为 None → execute_command 显式报 AcpError（`unknown command`），
    // 与 prompt 拦截的 PassThrough 语义区分（行为保持，设计 §78）。
    assert!(
        registry.resolve("/etc/hosts").is_none(),
        "绝对路径形态 RPC resolve 应为 None（无前缀匹配，非命令）"
    );
    assert!(
        registry.resolve("/mcp__demo__hello").is_none(),
        "mcp__ 遗留形态 RPC resolve 应为 None（词法非法，解析即失败）"
    );
}

/// Step 8 同步核对（行为保持锁定）：RPC 路径（execute-command）unknown
/// command 仍返回 AcpError（code -32602，`unknown command`）——与 prompt
/// 拦截的 PassThrough fall-through 语义区分（设计 §78）；`/etc/hosts` 与
/// `/mcp__demo__hello` 两个 fall-through 代表输入在 RPC 路径均显式报错。
#[tokio::test]
async fn test_execute_command_unknown_command_returns_acp_error() {
    for command in ["/unknown_cmd", "/etc/hosts", "/mcp__demo__hello"] {
        let params = serde_json::json!({
            "sessionId": "unknown-rpc",
            "command": command,
        });
        let history: Vec<BaseMessage> = vec![];
        let cancel = AgentCancellationToken::new();
        let peri_config = Arc::new(PeriConfig::default());
        let event_sink: Arc<dyn EventSink> = Arc::new(RecordingEventSink {
            events: Arc::new(std::sync::Mutex::new(Vec::new())),
        });
        let tmp = tempfile::tempdir().unwrap();
        let store: Arc<dyn peri_acp_types::store::ThreadStore> =
            Arc::new(FilesystemThreadStore::new(tmp.path().join("threads")));
        let controller = Controller::new(store);

        let err = execute_command(
            &params,
            history,
            "/tmp",
            &peri_config,
            &event_sink,
            None,
            &cancel,
            &controller,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .expect_err("unknown command 应返回 AcpError");

        assert_eq!(err.code, -32602);
        assert!(
            err.message.contains("unknown command"),
            "错误消息应含 `unknown command`，实际: {}",
            err.message
        );
    }
}

/// 记录所有事件的 EventSink（Phase 5 Step 3：RPC /clear 不再产生事件断言）。
struct RecordingEventSink {
    events: Arc<std::sync::Mutex<Vec<(String, String)>>>,
}

#[async_trait]
impl EventSink for RecordingEventSink {
    async fn push_event(&self, session_id: &str, event: &ExecutorEvent, _context_window: u32) {
        let json = serde_json::to_string(event).unwrap_or_default();
        self.events
            .lock()
            .unwrap()
            .push((session_id.to_string(), json));
    }

    async fn push_done(&self, _session_id: &str, _stop_reason: &str, _request_id: Option<&str>) {}
}

/// Phase 5 Step 3：RPC 路径（execute-command）执行 /clear——
/// 返回空 messages + EndTurn；不再产生 CompactCompleted 占位事件
/// （占位发射已删除）；反馈经编排层 emit_command_feedback 以
/// CommandFeedback 事件发射（Step 1 接线，UiOnly 不进会话）。
#[tokio::test]
async fn test_execute_command_clear_returns_empty_messages_no_compact_event() {
    let params = serde_json::json!({
        "sessionId": "clear-rpc",
        "command": "/clear",
    });
    let history = vec![
        BaseMessage::human("first message"),
        BaseMessage::ai("second message"),
    ];
    let cancel = AgentCancellationToken::new();
    let peri_config = Arc::new(PeriConfig::default());
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let event_sink: Arc<dyn EventSink> = Arc::new(RecordingEventSink {
        events: events.clone(),
    });
    let tmp = tempfile::tempdir().unwrap();
    let store: Arc<dyn peri_acp_types::store::ThreadStore> =
        Arc::new(FilesystemThreadStore::new(tmp.path().join("threads")));
    let controller = Controller::new(store);

    let result = execute_command(
        &params,
        history,
        "/tmp",
        &peri_config,
        &event_sink,
        None,
        &cancel,
        &controller,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();

    // 空 messages 序列化返回调用方（不写 thread store）
    assert_eq!(result["stop_reason"], "EndTurn");
    assert_eq!(result["messages"], serde_json::json!([]));
    // 不再产生 CompactCompleted 占位事件（旧占位发射已删除）
    let recorded = events.lock().unwrap();
    assert!(
        recorded
            .iter()
            .all(|(_, json)| !json.contains("compact_completed")),
        "clear 不应产生 CompactCompleted 事件（占位发射已删除），实际: {recorded:?}"
    );
    // 反馈经编排层 emit_command_feedback 发射 CommandFeedback（Step 1 接线）
    assert!(
        recorded
            .iter()
            .any(|(_, json)| json.contains("command_feedback")),
        "反馈应以 CommandFeedback 事件发射，实际: {recorded:?}"
    );
}

/// 外层取消应保留调用方传入的完整 history，而不是返回空消息列表。
#[tokio::test]
async fn test_execute_command_outer_cancel_preserves_history() {
    let params = serde_json::json!({
        "sessionId": "cancelled-compact",
        "command": "/compact",
    });
    let history = vec![
        BaseMessage::human("first message"),
        BaseMessage::ai("second message"),
    ];
    let expected_messages = serde_json::to_value(&history).unwrap();
    let cancel = AgentCancellationToken::new();
    cancel.cancel();
    let peri_config = Arc::new(PeriConfig::default());
    let event_sink: Arc<dyn EventSink> = Arc::new(PendingEventSink);
    let tmp = tempfile::tempdir().unwrap();
    let store: Arc<dyn peri_acp_types::store::ThreadStore> =
        Arc::new(FilesystemThreadStore::new(tmp.path().join("threads")));
    let controller = Controller::new(store);

    let result = execute_command(
        &params,
        history,
        "/tmp",
        &peri_config,
        &event_sink,
        None,
        &cancel,
        &controller,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(result["stop_reason"], "Cancelled");
    assert_eq!(result["messages"], expected_messages);
}

// ── Phase 5 Step 6：RouteEntry 域/等级检查（Immediate 语义 = core/ui 第一等级）──

/// Level1（core 域）条目通过。
#[test]
fn test_check_immediate_level_accepts_core_level1() {
    use peri_acp_types::command::command_route::{
        CommandEntryKind, CommandLifecycle, CommandProvenance, CommandSource, RouteEntry,
    };
    use peri_acp_types::command::{CommandHandler, CommandOutcome};
    struct DoneHandler;
    #[async_trait]
    impl CommandHandler for DoneHandler {
        async fn execute(&self, _ctx: peri_acp_types::command::CommandContext) -> CommandOutcome {
            unreachable!("仅构造条目，不执行");
        }
    }
    let entry = RouteEntry {
        fullname: "core:compact".to_string(),
        aliases: vec![],
        description: "".to_string(),
        kind: CommandEntryKind::Command,
        category: None,
        args_schema: None,
        handler: Arc::new(DoneHandler),
        provenance: CommandProvenance {
            source: CommandSource::Core,
            lifecycle: CommandLifecycle::Connected,
        },
    };
    assert!(check_immediate_level(&entry).is_ok());
}

/// Level2（mcp 域）条目放行（决策 D）：McpSkill 条目在 RPC 上下文由
/// McpSkillReleaser 直返 skill 全文（不依赖 agent 管线），check 通过。
#[test]
fn test_check_immediate_level_allows_mcp_skill_level2() {
    use peri_acp_types::command::command_route::{
        CommandEntryKind, CommandLifecycle, CommandProvenance, CommandSource, RouteEntry,
    };
    use peri_acp_types::command::{CommandHandler, CommandOutcome};
    struct DoneHandler;
    #[async_trait]
    impl CommandHandler for DoneHandler {
        async fn execute(&self, _ctx: peri_acp_types::command::CommandContext) -> CommandOutcome {
            unreachable!("仅构造条目，不执行");
        }
    }
    let entry = RouteEntry {
        fullname: "demo:hello".to_string(),
        aliases: vec![],
        description: "".to_string(),
        kind: CommandEntryKind::McpSkill,
        category: None,
        args_schema: None,
        handler: Arc::new(DoneHandler),
        provenance: CommandProvenance {
            source: CommandSource::Mcp {
                server: "demo".to_string(),
            },
            lifecycle: CommandLifecycle::Connected,
        },
    };
    assert!(
        check_immediate_level(&entry).is_ok(),
        "McpSkill 条目应放行（决策 D：RPC 直返全文）"
    );
}

/// Level2 非 McpSkill 条目（plugin/user）仍拒绝：handler 无 RPC 直返语义，
/// Inject/Delegate 分支会显式报错（非 Immediate，RPC 显式报错）。
#[test]
fn test_check_immediate_level_rejects_plugin_level2() {
    use peri_acp_types::command::command_route::{
        CommandEntryKind, CommandLifecycle, CommandProvenance, CommandSource, RouteEntry,
    };
    use peri_acp_types::command::{CommandHandler, CommandOutcome};
    struct DoneHandler;
    #[async_trait]
    impl CommandHandler for DoneHandler {
        async fn execute(&self, _ctx: peri_acp_types::command::CommandContext) -> CommandOutcome {
            unreachable!("仅构造条目，不执行");
        }
    }
    let entry = RouteEntry {
        fullname: "plugin:ecc:plan".to_string(),
        aliases: vec![],
        description: "".to_string(),
        kind: CommandEntryKind::Command,
        category: None,
        args_schema: None,
        handler: Arc::new(DoneHandler),
        provenance: CommandProvenance {
            source: CommandSource::Plugin {
                name: "ecc".to_string(),
            },
            lifecycle: CommandLifecycle::Connected,
        },
    };
    let err = check_immediate_level(&entry).unwrap_err();
    assert_eq!(err.code, -32602);
    assert!(
        err.message.contains("非 Immediate 命令"),
        "错误应说明非 Immediate 语义，实际: {}",
        err.message
    );
}

// ── Phase 5 Review P2-5：rewind 经 execute-command RPC（slash 形态 args）──

/// P2-5 测试共用执行器：构造 execute-command RPC 调用环境，返回
/// (result, events)，事件经 RecordingEventSink 记录。
async fn run_execute_command(
    params: serde_json::Value,
    history: Vec<BaseMessage>,
) -> (
    serde_json::Value,
    Arc<std::sync::Mutex<Vec<(String, String)>>>,
) {
    let cancel = AgentCancellationToken::new();
    let peri_config = Arc::new(PeriConfig::default());
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let event_sink: Arc<dyn EventSink> = Arc::new(RecordingEventSink {
        events: events.clone(),
    });
    let tmp = tempfile::tempdir().unwrap();
    let store: Arc<dyn peri_acp_types::store::ThreadStore> =
        Arc::new(FilesystemThreadStore::new(tmp.path().join("threads")));
    let controller = Controller::new(store);
    let result = execute_command(
        &params,
        history,
        "/tmp",
        &peri_config,
        &event_sink,
        None,
        &cancel,
        &controller,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    (result, events)
}

/// 从事件记录中取出 CommandFeedback 事件 JSON（按 message 子串定位）。
fn find_feedback_json(events: &[(String, String)], needle: &str) -> Option<String> {
    events
        .iter()
        .find(|(_, json)| json.contains(needle))
        .map(|(_, json)| json.clone())
}

/// `/rewind <target> --no-revert-files` 经 execute_command（slash 形态 args）：
/// 截断目标消息及其之后的消息 + feedback(Info, UiOnly)；RewindCompleted
/// 重建信号保留发射。
#[tokio::test]
async fn test_execute_command_rewind_slash_args_truncates_messages() {
    let m1 = BaseMessage::human("第一问");
    let m2 = BaseMessage::ai("第一答");
    let m3 = BaseMessage::human("第二问");
    let target_id = m3.id().as_uuid().to_string();
    let history = vec![m1, m2, m3];

    let (result, events) = run_execute_command(
        serde_json::json!({
            "sessionId": "rewind-rpc",
            "command": format!("/rewind {target_id} --no-revert-files"),
        }),
        history,
    )
    .await;

    assert_eq!(result["stop_reason"], "EndTurn");
    let msgs = result["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 2, "rewind 应截断目标消息及其之后的消息");
    assert_eq!(msgs[0]["role"], "user");
    assert_eq!(msgs[1]["role"], "assistant");

    // RewindCompleted（重建信号）+ CommandFeedback(Info, UiOnly) 双事件
    let recorded = events.lock().unwrap();
    assert!(
        recorded
            .iter()
            .any(|(_, json)| json.contains("rewind_completed")),
        "应发射 RewindCompleted 重建信号，实际: {recorded:?}"
    );
    let fb_json = find_feedback_json(&recorded, "command_feedback")
        .expect("应有 CommandFeedback 事件，实际: {recorded:?}");
    assert!(fb_json.contains("已回滚 1 条消息"), "实际: {fb_json}");
    assert!(fb_json.contains("\"level\":\"info\""), "实际: {fb_json}");
    assert!(
        fb_json.contains("\"channel\":\"uiOnly\""),
        "UiOnly 不进会话，实际: {fb_json}"
    );
}

/// 缺参（`/rewind`）经 execute_command：args_schema.parse 前置校验拦截
/// （P1-1，与拦截层同构），不进入 handler——原 history 原样返回 +
/// feedback(Error, UiOnly)。
#[tokio::test]
async fn test_execute_command_rewind_missing_target_returns_error_feedback() {
    let history = vec![BaseMessage::human("第一问"), BaseMessage::ai("第一答")];

    let (result, events) = run_execute_command(
        serde_json::json!({
            "sessionId": "rewind-rpc",
            "command": "/rewind",
        }),
        history.clone(),
    )
    .await;

    // 校验失败不修改 history（原样返回），EndTurn
    assert_eq!(result["stop_reason"], "EndTurn");
    assert_eq!(
        result["messages"],
        serde_json::to_value(&history).unwrap(),
        "参数校验失败应返回原 history"
    );

    // feedback(Error, UiOnly)，错误消息与拦截层同构（{name} 参数解析失败: ...）
    let recorded = events.lock().unwrap();
    let fb_json = find_feedback_json(&recorded, "rewind 参数解析失败")
        .expect("应有 CommandFeedback 错误事件，实际: {recorded:?}");
    assert!(
        fb_json.contains("missing required positional argument: target_message_id"),
        "实际: {fb_json}"
    );
    assert!(fb_json.contains("\"level\":\"error\""), "实际: {fb_json}");
    assert!(
        fb_json.contains("\"channel\":\"uiOnly\""),
        "UiOnly 不进会话，实际: {fb_json}"
    );
}

/// 未知 option（`/rewind <id> --nonsense`）经 execute_command：前置校验拦截
/// unknown option（与拦截层词法严格性一致，权威解析器拒绝），不进入 handler。
#[tokio::test]
async fn test_execute_command_rewind_unknown_option_returns_error_feedback() {
    let m1 = BaseMessage::human("第一问");
    let m2 = BaseMessage::ai("第一答");
    let m3 = BaseMessage::human("第二问");
    let target_id = m3.id().as_uuid().to_string();
    let history = vec![m1, m2, m3];

    let (result, events) = run_execute_command(
        serde_json::json!({
            "sessionId": "rewind-rpc",
            "command": format!("/rewind {target_id} --nonsense"),
        }),
        history.clone(),
    )
    .await;

    assert_eq!(result["stop_reason"], "EndTurn");
    assert_eq!(
        result["messages"],
        serde_json::to_value(&history).unwrap(),
        "参数校验失败应返回原 history"
    );

    let recorded = events.lock().unwrap();
    let fb_json = find_feedback_json(&recorded, "rewind 参数解析失败")
        .expect("应有 CommandFeedback 错误事件，实际: {recorded:?}");
    assert!(
        fb_json.contains("unknown option: --nonsense"),
        "实际: {fb_json}"
    );
    assert!(fb_json.contains("\"level\":\"error\""), "实际: {fb_json}");
}
