//! MCP 自定义通知汇合点的线路级回归。
//!
//! 夹具策略：客户端走生产 `serve_client_auto_for_server`（真实 rmcp Auto 握手），
//! 服务端是行级 JSON-RPC 假 server，可在 `tools/call` 响应前插入任意通知帧。

use std::sync::Arc;
use std::time::Duration;

use rmcp::model::CallToolRequestParams;
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream},
    task::JoinHandle,
};

use super::{
    pending_notification_reminder, MAX_NOTIFICATION_PAYLOAD_BYTES, MAX_PENDING_NOTIFICATIONS,
};
use crate::mcp::client::{serve_client_auto_for_server, McpClientPool, McpServiceWrapper};
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderCategory, ReminderDelivery, ReminderSeverity,
};

#[allow(deprecated)]
use rmcp::model::LoggingLevel;

const SERVER: &str = "showcase";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
const CLOSE_TIMEOUT: Duration = Duration::from_millis(500);

// ─── fixtures ─────────────────────────────────────────────────────────────────

/// 行级假 MCP server：`server/discover` 走 2026-07-28；`tools/call` 先写 `frames`，
/// 再写成功结果。其余请求返回 method-not-found。
fn spawn_fake_server(server_io: DuplexStream, frames: Vec<Value>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(server_io);
        let mut lines = BufReader::new(read).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(request) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let Some(id) = request.get("id").cloned() else {
                continue;
            };
            let method = request["method"].as_str().unwrap_or_default();
            if method == "tools/call" {
                for frame in &frames {
                    if write
                        .write_all(format!("{frame}\n").as_bytes())
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }
            let response = match method {
                "server/discover" => json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": {
                        "resultType": "complete",
                        "supportedVersions": ["2026-07-28"],
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "notification-fixture", "version": "1"},
                        "ttlMs": 0,
                        "cacheScope": "private"
                    }
                }),
                "tools/call" => json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": {
                        "resultType": "complete",
                        "content": [{"type": "text", "text": "done"}],
                        "isError": false
                    }
                }),
                _ => json!({
                    "jsonrpc": "2.0", "id": id,
                    "error": {"code": -32601, "message": "method not found"}
                }),
            };
            if write
                .write_all(format!("{response}\n").as_bytes())
                .await
                .is_err()
            {
                return;
            }
        }
    })
}

struct Fixture {
    pool: Arc<McpClientPool>,
    service: McpServiceWrapper,
    server_task: JoinHandle<()>,
}

impl Fixture {
    async fn connect(frames: Vec<Value>) -> Self {
        let (client_io, server_io) = tokio::io::duplex(16 * 1024);
        let server_task = spawn_fake_server(server_io, frames);
        let pool = Arc::new(McpClientPool::new_pending());
        let service = serve_client_auto_for_server(client_io, &pool, SERVER, HANDSHAKE_TIMEOUT)
            .await
            .expect("握手不得超时")
            .expect("握手不得失败");
        Self {
            pool,
            service,
            server_task,
        }
    }

    async fn call_tool(&self) {
        self.service
            .peer()
            .call_tool(CallToolRequestParams::new("slow_task"))
            .await
            .expect("tools/call 必须成功");
    }

    async fn close(mut self) {
        let _ = self.service.close_with_timeout(CLOSE_TIMEOUT).await;
        let _ = self.server_task.await;
    }
}

fn custom_frame(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}

// ─── T1：tools/call 响应流上的自定义通知可达 ─────────────────────────────────

#[tokio::test]
async fn t1_custom_notification_on_tool_response_stream_is_buffered() {
    let fixture = Fixture::connect(vec![custom_frame(
        "demo/task-status",
        json!({"runId": "run-1", "status": "started"}),
    )])
    .await;

    fixture.call_tool().await;
    let pending = fixture.pool.drain_pending_notifications();
    assert_eq!(pending.len(), 1, "恰好一条自定义通知: {pending:?}");
    let notification = &pending[0];
    assert_eq!(notification.server, SERVER);
    assert_eq!(notification.method, "demo/task-status");
    assert_eq!(notification.occurrences, 1);
    assert!(
        notification.payload.contains("run-1"),
        "payload 必须保留 server 参数: {}",
        notification.payload
    );

    let trusted = pending_notification_reminder(notification);
    let reminder = trusted.as_reminder();
    assert_eq!(reminder.category, ReminderCategory::Diagnostic);
    assert_eq!(reminder.source.0, "mcp");
    assert_eq!(reminder.kind, "custom_notification");
    assert_eq!(reminder.severity, ReminderSeverity::Info);
    assert_eq!(reminder.delivery, ReminderDelivery::Configurable);
    assert_eq!(
        reminder.audiences.0,
        vec![ReminderAudience::Tui, ReminderAudience::Diagnostics],
        "自定义通知只进 TUI / 诊断，不进模型上下文"
    );
    assert_eq!(
        reminder.summary.as_deref(),
        Some("MCP 通知: showcase/demo/task-status")
    );
    assert_eq!(reminder.metadata["server"], SERVER);
    assert_eq!(reminder.metadata["method"], "demo/task-status");
    assert_eq!(reminder.metadata["count"], 1);
    assert!(reminder.body.contains("demo/task-status"));
    assert!(reminder.body.contains("run-1"));

    fixture.close().await;
}

// ─── T2：notifications/message 门控 ──────────────────────────────────────────

#[tokio::test]
async fn t2_logging_without_declared_level_is_ignored() {
    let fixture = Fixture::connect(vec![custom_frame(
        "notifications/message",
        json!({"level": "info", "logger": "showcase", "data": {"runId": "run-1"}}),
    )])
    .await;

    fixture.call_tool().await;
    assert!(
        fixture.pool.drain_pending_notifications().is_empty(),
        "请求 _meta 未声明 logLevel 时不得进入缓冲"
    );

    fixture.close().await;
}

#[tokio::test]
#[allow(deprecated)]
async fn t2_logging_with_declared_level_is_buffered() {
    let fixture = Fixture::connect(vec![custom_frame(
        "notifications/message",
        json!({"level": "info", "logger": "showcase", "data": {"runId": "run-1"}}),
    )])
    .await;
    fixture.pool.declare_log_level(SERVER, LoggingLevel::Info);

    fixture.call_tool().await;
    let pending = fixture.pool.drain_pending_notifications();
    assert_eq!(pending.len(), 1, "已声明 logLevel 时必须可见: {pending:?}");
    assert_eq!(pending[0].method, "notifications/message");
    assert!(pending[0].payload.contains("showcase"));
    assert!(pending[0].payload.contains("run-1"));

    fixture.close().await;
}

// ─── T4：未订阅类别不外溢 ────────────────────────────────────────────────────

#[tokio::test]
async fn t4_unsubscribed_list_changed_is_not_buffered() {
    let fixture = Fixture::connect(vec![json!({
        "jsonrpc": "2.0",
        "method": "notifications/tools/list_changed"
    })])
    .await;

    fixture.call_tool().await;
    assert!(
        fixture.pool.drain_pending_notifications().is_empty(),
        "未订阅类别不得写入自定义通知缓冲"
    );

    fixture.close().await;
}

// ─── 裁决 2：ACP 声明 server 只留 debug，不进入部署级缓冲 ──────────────────────

#[tokio::test]
async fn acp_declared_server_notifications_stay_out_of_deployment_buffer() {
    let fixture = Fixture::connect(vec![custom_frame(
        "demo/task-status",
        json!({"runId": "run-1"}),
    )])
    .await;
    fixture
        .pool
        .acp_owners
        .write()
        .insert(SERVER.to_string(), "session-1".to_string());

    fixture.call_tool().await;
    assert!(
        fixture.pool.drain_pending_notifications().is_empty(),
        "ACP 会话级连接的通知不得进入部署级缓冲"
    );

    fixture.close().await;
}

// ─── 缓冲语义：合并、上限、截断 ───────────────────────────────────────────────

#[test]
fn pending_notifications_merge_by_server_and_method() {
    let pool = McpClientPool::new_empty();
    pool.record_custom_notification("srv", "demo/status", Some(&json!({"step": 1})));
    pool.record_custom_notification("srv", "demo/status", Some(&json!({"step": 2})));
    pool.record_custom_notification("srv", "demo/other", None);

    let pending = pool.drain_pending_notifications();
    assert_eq!(pending.len(), 2, "同类通知合并，不同 method 分条");
    assert_eq!(pending[0].method, "demo/status");
    assert_eq!(pending[0].occurrences, 2);
    assert!(
        pending[0].payload.contains("\"step\":2"),
        "合并后保留最新 payload: {}",
        pending[0].payload
    );
    assert_eq!(pending[1].method, "demo/other");
    assert_eq!(pending[1].occurrences, 1);
    assert!(pending[1].payload.is_empty());
    assert!(
        pool.drain_pending_notifications().is_empty(),
        "drain 即清空"
    );
}

#[test]
fn pending_notification_buffer_is_bounded() {
    let pool = McpClientPool::new_empty();
    for index in 0..=MAX_PENDING_NOTIFICATIONS {
        pool.record_custom_notification("srv", &format!("demo/{index}"), None);
    }
    let pending = pool.drain_pending_notifications();
    assert_eq!(pending.len(), MAX_PENDING_NOTIFICATIONS);
    assert!(
        !pending
            .iter()
            .any(|entry| entry.method == format!("demo/{MAX_PENDING_NOTIFICATIONS}")),
        "超限的新通知必须丢弃"
    );
}

#[test]
fn oversized_notification_payload_is_truncated_on_utf8_boundary() {
    let pool = McpClientPool::new_empty();
    let payload = "界".repeat(3_000);
    pool.record_custom_notification("srv", "demo/large", Some(&json!(payload)));

    let pending = pool.drain_pending_notifications();
    assert_eq!(pending.len(), 1);
    let stored = &pending[0].payload;
    assert!(
        stored.len() <= MAX_NOTIFICATION_PAYLOAD_BYTES,
        "payload 必须受限: {} > {}",
        stored.len(),
        MAX_NOTIFICATION_PAYLOAD_BYTES
    );
    assert!(stored.ends_with("…（已截断）"), "截断必须留标记: {stored}");
    assert!(stored.is_char_boundary(stored.len()));

    let trusted = pending_notification_reminder(&pending[0]);
    assert!(trusted.as_reminder().body.len() <= 4 * 1024);
}
