//! 批 0 wire capture 基线测试：锁定双路径在**通知/响应构造层**的最终 wire
//! JSON 形态，供统一后对照。
//!
//! 背景（docs/design/acp-host-unify.md §8.3）：`dispatch::build_available_commands_update`
//! 与 `dispatch::build_initialize_response` 是 TUI/notify 路径与 typed stdio 路径
//! **共享**的单一实现。两侧差异只在**外层封装**：
//! - notify 路径（`host/notify.rs`）：手工 `json!({"sessionId", "update"})` 封装，
//!   经 `AcpTransport::send_notification("session/update", payload)` 发送；
//! - stdio 路径（host/stdio/commands.rs 等）：`SessionNotification::new(session_id,
//!   update)`（agent-client-protocol-schema 类型，camelCase + skip_serializing_none），
//!   经 `ConnectionTo<Client>::send_notification` 发送 → 序列化为同一 JSON 结构。
//!
//! schema 侧 `SESSION_UPDATE_NOTIFICATION = "session/update"`（v1/client.rs:2282）。
//!
//! 目的：证明「外层封装一致」（`{sessionId, update}` 结构、`sessionUpdate` 判别
//! 字符串、`_meta` 省略规则、camelCase 命名），并提供 initialize 响应 wire 基线。

use std::sync::Arc;

use agent_client_protocol::schema::v1::{SessionId, SessionNotification, SessionUpdate};
use async_trait::async_trait;
use peri_acp_types::command::command_handler::{CommandHandler, CommandOutcome};
use peri_acp_types::command::command_route::{
    CommandEntryKind, CommandLifecycle, CommandProvenance, CommandSource, RouteEntry, UiCommandSpec,
};
use peri_acp_types::command::{CommandContext, CommandResult, PromptStopReason};
use peri_acp_types::command_registry::CommandRegistry;
use peri_acp_types::PeriCaps;
use serde_json::{json, Value};

use crate::dispatch::commands::build_available_commands_update;
use crate::host::notify::{send_available_commands_update, send_session_info_update_with_title};
use crate::session::command::register_builtins;
use crate::transport::types::{AcpError, IncomingMessage, RequestId};
use crate::transport::AcpTransport;

// ── 本文件局部 mock（§5.1 不共享原则）───────────────────────────────────────

/// 记录全部 `send_notification` 的 mock transport（捕获 (method, params)）。
#[derive(Default)]
struct MockTransport {
    notifications: std::sync::Mutex<Vec<(String, Value)>>,
}

impl MockTransport {
    fn notifications(&self) -> Vec<(String, Value)> {
        self.notifications.lock().unwrap().clone()
    }
}

#[async_trait]
impl AcpTransport for MockTransport {
    async fn send_request(&self, _method: &str, _params: Value) -> Result<Value, AcpError> {
        Ok(json!({}))
    }
    async fn send_notification(&self, method: &str, params: Value) -> Result<(), AcpError> {
        self.notifications
            .lock()
            .unwrap()
            .push((method.to_string(), params));
        Ok(())
    }
    async fn recv(&self) -> Option<IncomingMessage> {
        None
    }
    async fn send_response(
        &self,
        _id: RequestId,
        _result: Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        Ok(())
    }
}

/// 假 handler：仅占位（投影测试只断言元数据，不触发执行）。
struct FakeHandler;

#[async_trait]
impl CommandHandler for FakeHandler {
    async fn execute(&self, _ctx: CommandContext) -> CommandOutcome {
        CommandOutcome::Done(CommandResult {
            messages: Vec::new(),
            stop_reason: PromptStopReason::EndTurn,
            feedback: None,
        })
    }
}

/// mcp 域条目（Level2：Level1 处 `{server}:{name}` 全名原样投影）。
fn mcp_entry(fullname: &str, server: &str) -> RouteEntry {
    RouteEntry {
        fullname: fullname.into(),
        aliases: Vec::new(),
        description: format!("desc of {fullname}"),
        kind: CommandEntryKind::McpSkill,
        category: None,
        args_schema: None,
        handler: Arc::new(FakeHandler),
        provenance: CommandProvenance {
            source: CommandSource::Mcp {
                server: server.into(),
            },
            lifecycle: CommandLifecycle::Discovered,
        },
    }
}

/// 掩码 `update.updatedAt`（内部 `chrono::Utc::now()` 时间戳不可在两条路径
/// 同时固定），归一后逐字段比较其余结构。
fn mask_updated_at(mut v: Value) -> Value {
    if let Some(obj) = v.get_mut("update").and_then(|u| u.as_object_mut()) {
        if let Some(ts) = obj.get_mut("updatedAt") {
            *ts = json!("MASKED-TS");
        }
    }
    v
}

// ── AvailableCommandsUpdate：外层封装一致性 ─────────────────────────────────

/// 同一业务数据（注册表 snapshot 投影）经 notify 路径（`send_available_commands_update`）
/// 与 stdio 路径（`SessionNotification` 序列化）产出的**最终 wire JSON 逐字段相同**。
///
/// 共享实现：`build_available_commands_update`（dispatch/commands.rs:120）——本测试
/// 锁外层封装：`{sessionId, update}` 结构、`sessionUpdate` 判别字符串、`_meta` 省略
/// 规则、camelCase 字段命名。
#[tokio::test]
async fn test_available_commands_update_wire_identical_between_paths() {
    // 同一业务数据：协商 skill_names + ui 明细；注册表 = 基座内置 + mcp 条目
    let caps = PeriCaps {
        skill_names: true,
        ui_commands: vec![UiCommandSpec {
            name: "gallery".into(),
            aliases: vec!["gal".into()],
            description: "Open the gallery panel".into(),
            args: None,
        }],
        ..Default::default()
    };
    let reg = Arc::new(CommandRegistry::new());
    register_builtins(&reg);
    reg.register(mcp_entry("demo:hello", "demo"))
        .expect("mcp 条目注册应成功");

    // —— notify 路径：驱动真实发送，捕获最终 payload ——
    let transport = Arc::new(MockTransport::default());
    let dyn_transport: Arc<dyn AcpTransport> = transport.clone();
    send_available_commands_update(&dyn_transport, "s-wire", &caps, Some(Arc::clone(&reg))).await;
    let notifs = transport.notifications();
    assert_eq!(notifs.len(), 1, "首发广播恰一条");
    let (method, notify_payload) = &notifs[0];
    assert_eq!(
        method, "session/update",
        "notify 路径 method 与 schema 侧 SESSION_UPDATE_NOTIFICATION 一致"
    );

    // —— stdio 路径：同一 snapshot 投影重建 → SessionNotification 序列化 ——
    let update = build_available_commands_update(&reg.snapshot(), &caps);
    let stdio_wire = serde_json::to_value(SessionNotification::new(
        SessionId::new("s-wire"),
        SessionUpdate::AvailableCommandsUpdate(update),
    ))
    .expect("SessionNotification 序列化不应失败");

    assert_eq!(
        notify_payload, &stdio_wire,
        "两条路径最终 wire JSON 必须逐字段一致"
    );

    // 显式锁判别字符串与字段命名/省略规则（失败信息可读）
    assert_eq!(notify_payload["sessionId"], "s-wire");
    assert_eq!(
        notify_payload["update"]["sessionUpdate"], "available_commands_update",
        "SessionUpdate 判别字段为 snake_case 变体名"
    );
    let commands = notify_payload["update"]["availableCommands"]
        .as_array()
        .unwrap();
    assert!(!commands.is_empty(), "availableCommands 应为投影条目数组");
}

/// `_meta`（update 级）省略规则：未协商 `skill_names` 时整个 update 级 `_meta`
/// 不出现；条目级 `_meta.periKind/periLevel` 恒有。两条路径一致（共享同一
/// `build_available_commands_update`，此处只锁 notify 路径 wire）。
#[tokio::test]
async fn test_available_commands_update_meta_omission_rules() {
    let caps = PeriCaps::default(); // skill_names = false
    let reg = Arc::new(CommandRegistry::new());
    register_builtins(&reg);

    let transport = Arc::new(MockTransport::default());
    let dyn_transport: Arc<dyn AcpTransport> = transport.clone();
    send_available_commands_update(&dyn_transport, "s-meta", &caps, Some(Arc::clone(&reg))).await;
    let (_, payload) = &transport.notifications()[0];

    assert!(
        payload["update"].get("_meta").is_none(),
        "未协商 skillNames 时 update 级 _meta 应省略: {}",
        payload["update"]
    );
    for c in payload["update"]["availableCommands"].as_array().unwrap() {
        assert!(c["_meta"].is_object(), "条目级 _meta 恒有: {c}");
        assert_eq!(c["_meta"]["periKind"], "command", "基座内置 kind: {c}");
        assert_eq!(c["_meta"]["periLevel"], 1, "core 域 Level1: {c}");
        assert!(c.get("input").is_none(), "无参数条目 input 省略: {c}");
    }
}

// ── SessionInfoUpdate：通知携带会话元信息（外层封装一致）──────────────────

/// stdio 侧（`prompt_exec.rs:587-589`）与 notify 侧（`send_session_info_update_*`）
/// 同用 `SessionUpdate::SessionInfoUpdate`；锁定外层 `{sessionId, update}` 结构、
/// `session_info_update` 判别字符串、camelCase 字段命名（`updatedAt`/`title`）。
#[tokio::test]
async fn test_session_info_update_wire_identical_between_paths() {
    // —— notify 路径：带 title ——
    let transport = Arc::new(MockTransport::default());
    let dyn_transport: Arc<dyn AcpTransport> = transport.clone();
    send_session_info_update_with_title(dyn_transport.as_ref(), "s-info", Some("新标题")).await;
    let (method, notify_payload) = &transport.notifications()[0];
    assert_eq!(method, "session/update");
    assert_eq!(notify_payload["sessionId"], "s-info");

    // —— stdio 路径：prompt_exec.rs 同款构造（updated_at(now)，可选 title）——
    let info = agent_client_protocol::schema::v1::SessionInfoUpdate::new()
        .updated_at("FIXED-TS")
        .title("新标题".to_string());
    let stdio_wire = serde_json::to_value(SessionNotification::new(
        SessionId::new("s-info"),
        SessionUpdate::SessionInfoUpdate(info),
    ))
    .unwrap();

    assert_eq!(
        mask_updated_at(notify_payload.clone()),
        mask_updated_at(stdio_wire),
        "updatedAt 掩码后两条路径 wire 逐字段一致"
    );
    assert_eq!(
        notify_payload["update"]["sessionUpdate"], "session_info_update",
        "SessionInfoUpdate 判别字符串"
    );
    assert_eq!(notify_payload["update"]["title"], "新标题");
    assert!(
        notify_payload["update"].get("updatedAt").is_some(),
        "updatedAt 恒有（rfc3339 字符串）"
    );
}

/// title 缺省时 `title` 字段整体省略（MaybeUndefined skip 语义），
/// 谓两条路径一致地遵守省略规则。
#[tokio::test]
async fn test_session_info_update_title_omitted_when_unset() {
    let transport = Arc::new(MockTransport::default());
    let dyn_transport: Arc<dyn AcpTransport> = transport.clone();
    send_session_info_update_with_title(dyn_transport.as_ref(), "s-info", None).await;
    let (_, payload) = &transport.notifications()[0];

    assert_eq!(payload["update"]["sessionUpdate"], "session_info_update");
    assert!(
        payload["update"].get("title").is_none(),
        "未设 title 时 title 字段应省略: {}",
        payload["update"]
    );
    assert!(
        payload["update"].get("updatedAt").is_some(),
        "updatedAt 仍应存在"
    );
}

// ── initialize：`build_initialize_response` wire 基线 ──────────────────────

/// initialize 响应基线（TUI 侧 `serde_json::to_value(resp)`、stdio 侧
/// `responder.respond(resp)` 序列化同一 `InitializeResponse`）：
/// - 顶层仅 `{protocolVersion, agentCapabilities}`（authMethods/agentInfo/_meta
///   空时省略，`skip_serializing_none`）；
/// - `protocolVersion` = 数字 1；
/// - `_meta.peri.*` 完整回显协商 caps；
/// - 全部 session 生命周期能力声明（list/close/resume/fork/delete）。
#[test]
fn test_initialize_response_wire_baseline() {
    let caps = PeriCaps {
        token_stats: true,
        skill_names: true,
        ..Default::default()
    };
    let resp = crate::dispatch::build_initialize_response(&caps);
    let v = serde_json::to_value(resp).expect("InitializeResponse 序列化不应失败");

    // 顶层键（空值省略规则）
    let mut top_keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    top_keys.sort_unstable();
    assert_eq!(
        top_keys, ["agentCapabilities", "authMethods", "protocolVersion"],
        "顶层为 agentCapabilities + authMethods + protocolVersion（agentInfo/_meta 为 None 时省略）"
    );
    assert_eq!(
        v["protocolVersion"], 1,
        "ProtocolVersion::V1 序列化为数字 1"
    );

    let caps_v = &v["agentCapabilities"];
    assert_eq!(caps_v["loadSession"], true);
    for k in [
        "promptCapabilities",
        "mcpCapabilities",
        "sessionCapabilities",
        "auth",
    ] {
        assert!(
            caps_v[k].is_object(),
            "agentCapabilities.{k} 应声明（非 Option 恒序列化）"
        );
    }
    for k in ["list", "close", "resume", "fork", "delete"] {
        assert!(
            caps_v["sessionCapabilities"][k].is_object(),
            "sessionCapabilities.{k} 应声明全部生命周期能力"
        );
    }

    // `_meta.peri.*` 回显（to_agent_meta 全量）
    let meta = &caps_v["_meta"];
    assert_eq!(meta["peri.tokenStats"], true);
    assert_eq!(meta["peri.skillNames"], true);
    assert_eq!(meta["peri.replay"], false);
    assert_eq!(meta["peri.agentEvent"], false);
    assert_eq!(meta["peri.rewind"], false);
    assert_eq!(meta["peri.uiCommands"], json!([]));
}

// ── 批 2 stdio notify adapter：Value payload → SessionNotification 往返保真 ──

/// 批 2 `StdioNotifyTransport` 的转换无损性基线：requests 侧通知 helper
/// （`send_available_commands_update` / `send_config_option_update` /
/// `TuiReplaySender`）经 `AcpTransport::send_notification("session/update",
/// payload)` 发出的 payload 是 `{sessionId, update}`（`update` = `SessionUpdate`
/// 序列化值）。stdio 侧 adapter 把它 `serde_json::from_value::<SessionNotification>`
/// 还原后发送——本测试锁定该转换往返后 **wire JSON 逐字段不变**（migration 不因
/// Value↔typed 转换引入新的外层封装差异）。
#[tokio::test]
async fn test_stdio_notify_adapter_payload_roundtrip_preserves_wire() {
    // requests 侧 `send_available_commands_update` 首发广播的 payload 形态
    let caps = PeriCaps {
        skill_names: true,
        ui_commands: vec![UiCommandSpec {
            name: "gallery".into(),
            description: "Open the gallery panel".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let reg = Arc::new(CommandRegistry::new());
    register_builtins(&reg);
    let update_value = serde_json::to_value(SessionUpdate::AvailableCommandsUpdate(
        build_available_commands_update(&reg.snapshot(), &caps),
    ))
    .expect("AvailableCommandsUpdate 序列化不应失败");
    let payload = json!({
        "sessionId": "s-adapter",
        "update": update_value,
    });

    // adapter 内部等价转换：payload → SessionNotification → 重新序列化
    let notif: SessionNotification =
        serde_json::from_value(payload.clone()).expect("payload 应可还原为 SessionNotification");
    let rebuilt = serde_json::to_value(&notif).expect("SessionNotification 序列化不应失败");

    assert_eq!(
        rebuilt, payload,
        "StdioNotifyTransport 转换往返必须保持 wire JSON 逐字段不变"
    );
    assert_eq!(rebuilt["sessionId"], "s-adapter");
    assert_eq!(
        rebuilt["update"]["sessionUpdate"], "available_commands_update",
        "update 判别字符串保持不变"
    );
}

/// 批 2 adapter 同样承载 `session/update` 的其余 `SessionUpdate` 变体
/// （`send_config_option_update` / `TuiReplaySender` 同款外层封装）——
/// 以 `SessionInfoUpdate` 为例锁定往返保真。
#[tokio::test]
async fn test_stdio_notify_adapter_info_payload_roundtrip_preserves_wire() {
    let info_update = serde_json::to_value(SessionUpdate::SessionInfoUpdate(
        agent_client_protocol::schema::v1::SessionInfoUpdate::new()
            .updated_at("FIXED-TS")
            .title("新标题".to_string()),
    ))
    .expect("SessionInfoUpdate 序列化不应失败");
    let payload = json!({
        "sessionId": "s-adapter",
        "update": info_update,
    });

    let notif: SessionNotification =
        serde_json::from_value(payload.clone()).expect("payload 应可还原为 SessionNotification");
    let rebuilt = serde_json::to_value(&notif).expect("SessionNotification 序列化不应失败");

    assert_eq!(
        rebuilt, payload,
        "StdioNotifyTransport 对 SessionInfoUpdate payload 往返保真"
    );
    assert_eq!(rebuilt["update"]["sessionUpdate"], "session_info_update");
}
