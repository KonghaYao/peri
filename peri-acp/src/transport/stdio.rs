//! Stdio-based ACP transport for IDE integration.
//!
//! Reads JSON-RPC messages from stdin (one per line) and writes to stdout.
//! Background pump task dispatches Response messages to the pending request map,
//! forwards Requests/Notifications to the incoming channel.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, BufWriter},
    sync::{mpsc, Mutex},
};

use super::{
    router::RequestRouter,
    types::{AcpError, IncomingMessage, RequestId},
    AcpTransport,
};

/// JSON-RPC 2.0 envelope for (de)serialization over stdio.
#[derive(serde::Serialize, serde::Deserialize)]
struct JsonRpcEnvelope {
    jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<AcpError>,
}

/// legacy `{"type":"cancel"}` 行拦截回调（批 3 §7 #10 移植；pump 逐行精确
/// trim 匹配后调用，host 侧实现遍历全部 session 的 cancel_token）。
pub type CancelHook = Arc<dyn Fn(&str) + Send + Sync>;

/// Stdio-based ACP transport.
///
/// Communicates with an external client (IDE) over stdin/stdout using
/// newline-delimited JSON-RPC 2.0 messages. A background pump task reads
/// stdin lines, dispatches responses to pending requests, and forwards
/// requests/notifications to the `recv()` channel.
pub struct StdioTransport {
    incoming_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<IncomingMessage>>,
    router: RequestRouter,
    writer: Arc<Mutex<BufWriter<Box<dyn AsyncWrite + Send + Unpin>>>>,
    /// legacy `{"type":"cancel"}` 逐行拦截回调（全 session 兜底中断，移植自
    /// `host/stdio/transport.rs::cancel_debug_hook`，批 3 §7 #10）。pump 读到
    /// 原始行为该 JSON（精确 trim 匹配）时调用之；消费后不产生 IncomingMessage。
    /// `None` = 不拦截（type:cancel 行按非 JSON 行静默跳过，pump 解析不中断）。
    cancel_hook: Arc<std::sync::Mutex<Option<CancelHook>>>,
}

impl Default for StdioTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl StdioTransport {
    /// Create a new stdio transport. Must be called within a tokio runtime.
    pub fn new() -> Self {
        Self::from_reader_writer(tokio::io::stdin(), tokio::io::stdout())
    }

    /// 注入 legacy `{"type":"cancel"}` 拦截回调（构造后可调用；pump 在遇到
    /// type:cancel 行时经由共享槽位读取，构造完成后写入的 hook 同样生效）。
    ///
    /// 语义（批 3 §7 #10，与标准 `session/cancel` 并存）：type:cancel 是无
    /// sessionId 的全 session 兜底中断——host 侧回调负责遍历全部 SessionState
    /// 对所有 `cancel_token.cancel()`。标准 `session/cancel`（按 sessionId +
    /// writer lease + continuation 武装）行为不受影响。
    pub fn with_cancel_hook(self, hook: Option<CancelHook>) -> Self {
        *self.cancel_hook.lock().unwrap() = hook;
        self
    }

    /// Create a transport reading from `reader` and writing to `writer`.
    ///
    /// `new()`/`Default` 行为不变（进程 stdin/stdout）；本构造器仅注入
    /// reader/writer 以便集成测试驱动（可测性重构，批 0）。pump 语义与
    /// `new()` 完全一致：逐行解析 stdin、响应按 id 分派到 router、其余消息
    /// 转发到 `recv()` 通道；reader EOF 后 pump 退出（通道关闭 → `recv()`
    /// 返回 `None`）。
    pub fn from_reader_writer<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        Self::from_reader_writer_with_cancel_hook(reader, writer, None)
    }

    /// 同 [`from_reader_writer`]，另注入 legacy `{"type":"cancel"}` 拦截回调
    /// （等价构造期注入；`with_cancel_hook` 亦可事后设置）。
    pub fn from_reader_writer_with_cancel_hook<R, W>(
        reader: R,
        writer: W,
        cancel_hook: Option<CancelHook>,
    ) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let router = RequestRouter::new();
        let pump_router = router.clone();
        let cancel_hook = Arc::new(std::sync::Mutex::new(cancel_hook));
        let pump_cancel_hook = Arc::clone(&cancel_hook);

        // Background pump: read stdin → dispatch responses / forward messages
        tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();

            while let Ok(Some(line)) = lines.next_line().await {
                if line.is_empty() {
                    continue;
                }

                // legacy `{"type":"cancel"}` 兜底中断（非 JSON-RPC 行）：
                // 在 JSON parse 失败分支之前拦截——精确 trim 匹配（与迁移前
                // `host/stdio/transport.rs::cancel_debug_hook` 一致）。hook
                // 消费该行后不产生 IncomingMessage；未注入 hook 时该行静默
                // 跳过（不误报 invalid JSON），其余非法行仍走 error 日志跳过。
                if line.trim() == r#"{"type":"cancel"}"# {
                    let hook = { pump_cancel_hook.lock().unwrap().clone() };
                    if let Some(hook) = hook {
                        let raw = line.clone();
                        hook(&raw);
                    } else {
                        tracing::debug!("type:cancel line ignored (no cancel hook installed)");
                    }
                    continue;
                }

                let mut envelope: JsonRpcEnvelope = match serde_json::from_str(&line) {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::error!(error = %e, "Failed to parse JSON-RPC from stdin");
                        continue;
                    }
                };

                let has_method = envelope.method.is_some();
                let result_val = envelope.result.take();
                let error_val = envelope.error.take();

                // JSON-RPC 2.0 §2.2：Request 的 id 成员存在但为 null 时视为
                // 通知（客户端无兴趣于对应响应，等同「无 id」）——进入
                // `(None, true)` 通知分支，而非压 0 成请求（决策点 7 收口）。
                let id = match envelope.id {
                    Some(Value::Null) if has_method => None,
                    id => id,
                };

                match (id, has_method) {
                    // Response to a server-initiated request (has id, no method)
                    (Some(id), false) => {
                        if !is_domain_id(&id) {
                            // 域外 id（小数/u64 溢出 i64/null/bool 等）→ 协议
                            // 违规：拒绝该行（warn + 丢弃，pump 不中断；与非法
                            // JSON 行处理语义一致）。不压 0 转发——压 0 会把合法
                            // id 0 与域外 id 混淆，router 配对/宿主
                            // `send_response(0, ...)` 将响错对象（决策点 7）。
                            tracing::warn!(
                                id = %id,
                                "Ignoring JSON-RPC response with out-of-domain id"
                            );
                            continue;
                        }
                        let req_id = value_to_request_id(&id);
                        let result = if let Some(error) = error_val {
                            Err(error)
                        } else {
                            Ok(result_val.unwrap_or(Value::Null))
                        };
                        let msg = IncomingMessage::Response { id: req_id, result };
                        if !pump_router.dispatch(&msg).await {
                            let _ = incoming_tx.send(msg);
                        }
                    }
                    // Request (has id + method)
                    (Some(id), true) => {
                        if !is_domain_id(&id) {
                            tracing::warn!(
                                id = %id,
                                "Ignoring JSON-RPC request with out-of-domain id"
                            );
                            continue;
                        }
                        let method = envelope.method.unwrap();
                        let req_id = value_to_request_id(&id);
                        let _ = incoming_tx.send(IncomingMessage::Request {
                            id: req_id,
                            method,
                            params: envelope.params.unwrap_or(Value::Null),
                        });
                    }
                    // Notification (no id, has method)
                    (None, true) => {
                        let method = envelope.method.unwrap();
                        let _ = incoming_tx.send(IncomingMessage::Notification {
                            method,
                            params: envelope.params.unwrap_or(Value::Null),
                        });
                    }
                    _ => {
                        tracing::warn!("Unhandled JSON-RPC message structure, ignoring");
                    }
                }
            }

            tracing::info!("Stdio transport: stdin closed");
        });

        Self {
            incoming_rx: tokio::sync::Mutex::new(incoming_rx),
            router,
            writer: Arc::new(Mutex::new(BufWriter::new(
                Box::new(writer) as Box<dyn AsyncWrite + Send + Unpin>
            ))),
            cancel_hook,
        }
    }
}

#[async_trait]
impl AcpTransport for StdioTransport {
    async fn send_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        let (id_num, response_rx) = self.router.register().await;

        let envelope = JsonRpcEnvelope {
            jsonrpc: "2.0".to_string(),
            id: Some(Value::Number(id_num.into())),
            method: Some(method.to_string()),
            params: Some(params),
            result: None,
            error: None,
        };
        write_envelope(&self.writer, &envelope).await?;

        response_rx
            .await
            .map_err(|_| AcpError::new(-32603, "Request cancelled (client disconnected)"))?
    }

    async fn send_notification(&self, method: &str, params: Value) -> Result<(), AcpError> {
        let envelope = JsonRpcEnvelope {
            jsonrpc: "2.0".to_string(),
            id: None,
            method: Some(method.to_string()),
            params: Some(params),
            result: None,
            error: None,
        };
        write_envelope(&self.writer, &envelope).await
    }

    async fn recv(&self) -> Option<IncomingMessage> {
        self.incoming_rx.lock().await.recv().await
    }

    async fn send_response(
        &self,
        id: RequestId,
        result: Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        let id_val = request_id_to_value(&id);
        match result {
            Ok(value) => {
                let envelope = JsonRpcEnvelope {
                    jsonrpc: "2.0".to_string(),
                    id: Some(id_val),
                    method: None,
                    params: None,
                    result: Some(value),
                    error: None,
                };
                write_envelope(&self.writer, &envelope).await
            }
            Err(error) => {
                let envelope = JsonRpcEnvelope {
                    jsonrpc: "2.0".to_string(),
                    id: Some(id_val),
                    method: None,
                    params: None,
                    result: None,
                    error: Some(error),
                };
                write_envelope(&self.writer, &envelope).await
            }
        }
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

async fn write_envelope(
    writer: &Arc<Mutex<BufWriter<Box<dyn AsyncWrite + Send + Unpin>>>>,
    envelope: &JsonRpcEnvelope,
) -> Result<(), AcpError> {
    let mut line = serde_json::to_string(envelope)
        .map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))?;
    line.push('\n');
    let mut guard = writer.lock().await;
    guard
        .write_all(line.as_bytes())
        .await
        .map_err(|e| AcpError::new(-32603, format!("Write failed: {e}")))?;
    guard
        .flush()
        .await
        .map_err(|e| AcpError::new(-32603, format!("Flush failed: {e}")))?;
    Ok(())
}

/// JSON-RPC 2.0 id 域校验（决策点 7 收口）：合法 id 仅 String 或整数
/// Number（§2.2：Number 不应含小数，脚注 2）。`null` 由 pump 按通知语义
/// 单独处理（§2.2：id 为 null 的 Request 视为通知）；布尔/小数/u64 溢出
/// i64/数组等一律视为域外（协议违规 → pump 拒绝该消息，不压 0 转发）。
fn is_domain_id(v: &Value) -> bool {
    match v {
        Value::String(_) => true,
        Value::Number(n) => n.as_i64().is_some(),
        _ => false,
    }
}

fn value_to_request_id(v: &Value) -> RequestId {
    // 调用方（pump）已做 `is_domain_id` 校验，此处仅处理域内值；压 0 分支
    // 为防御性兜底（正常不可达）。
    match v {
        Value::String(s) => RequestId::String(s.clone()),
        Value::Number(n) => RequestId::Number(n.as_i64().unwrap_or(0)),
        _ => RequestId::Number(0),
    }
}

fn request_id_to_value(id: &RequestId) -> Value {
    match id {
        RequestId::String(s) => Value::String(s.clone()),
        RequestId::Number(n) => Value::Number((*n).into()),
    }
}

#[cfg(test)]
#[path = "stdio_test.rs"]
mod tests;
