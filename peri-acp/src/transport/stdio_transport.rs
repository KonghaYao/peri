//! Stdio-based ACP transport for IDE integration.
//!
//! Reads JSON-RPC messages from stdin (one per line) and writes to stdout.
//! Background pump task dispatches Response messages to the pending request map,
//! forwards Requests/Notifications to the bounded incoming channel. Overflow
//! requests are rejected with an explicit backpressure error instead of being
//! queued without limit or silently dropped. While the pump waits on the writer
//! lock or on queue capacity it keeps reading stdin into a bounded deferred
//! buffer, so EOF still reaches the same terminal settlement.

use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::io::BufWriter;
use tokio::sync::mpsc;
use tokio::sync::Mutex;

use super::router;
use super::router::RequestRouter;
use super::types::AcpError;
use super::types::IncomingMessage;
use super::types::RequestId;
use super::AcpTransport;

/// 入站队列容量。stdio 单连接的请求/通知积压必须有界：host 循环被生命周期
/// 锁或存储 I/O 占住时，不能把 stdin 无界读进宿主内存。量级与外部帧桥（256）
/// 一致，按单连接交互突发设置。
const STDIO_INCOMING_CAPACITY: usize = 256;

/// JSON-RPC 2.0 保留的 server error 段（-32000..=-32099）中表达「传输层背压」
/// 的首值，与进程内 mpsc 传输的 `TRANSPORT_BACKPRESSURE_CODE` 同义。
const STDIO_BACKPRESSURE_CODE: i64 = -32000;

const STDIO_BACKPRESSURE_MESSAGE: &str = "ACP transport backpressure: inbound queue is full";

/// pump 等待 writer 锁或入站容量时暂存 stdin 行的上限。暂存让 EOF 不被等待
/// 挡住，又不必把积压挪进无界缓冲；达到上限即判定对端不再排水并终止传输。
const STDIO_DEFERRED_CAPACITY: usize = 256;

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
    incoming_rx: tokio::sync::Mutex<mpsc::Receiver<IncomingMessage>>,
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
    /// continuation 武装）行为不受影响。
    pub fn with_cancel_hook(self, hook: Option<CancelHook>) -> Self {
        *self
            .cancel_hook
            .lock()
            .expect("cancel hook mutex must remain unpoisoned") = hook;
        self
    }

    /// Create a transport reading from `reader` and writing to `writer`.
    ///
    /// `new()`/`Default` 行为不变（进程 stdin/stdout）；本构造器仅注入
    /// reader/writer 以便集成测试驱动（可测性重构，批 0）。pump 语义与
    /// `new()` 完全一致：逐行解析 stdin、响应按 id 分派到 router、其余消息
    /// 转发到 `recv()` 通道；reader EOF 后 pump 退出（通道关闭 → `recv()`
    /// 返回 `None`）。
    #[must_use]
    pub fn from_reader_writer<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        Self::from_reader_writer_with_cancel_hook(reader, writer, None)
    }

    /// 同 [`Self::from_reader_writer`]，另注入 legacy `{"type":"cancel"}` 拦截回调
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
        let (incoming_tx, incoming_rx) = mpsc::channel(STDIO_INCOMING_CAPACITY);
        let router = RequestRouter::new();
        let pump_router = router.clone();
        let writer: Arc<Mutex<BufWriter<Box<dyn AsyncWrite + Send + Unpin>>>> =
            Arc::new(Mutex::new(BufWriter::new(Box::new(writer))));
        let pump_writer = Arc::clone(&writer);
        let cancel_hook = Arc::new(std::sync::Mutex::new(cancel_hook));
        let pump_cancel_hook = Arc::clone(&cancel_hook);

        // Background pump: read stdin → dispatch responses / forward messages
        tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            // 等待 writer/容量期间读到的行：按到达顺序重放，保证入站全序。
            let mut deferred: VecDeque<String> = VecDeque::new();

            loop {
                let line = match deferred.pop_front() {
                    Some(line) => line,
                    None => {
                        let next_line = tokio::select! {
                            result = lines.next_line() => Some(result),
                            () = pump_router.wait_closed() => None,
                        };
                        let Some(next_line) = next_line else {
                            break;
                        };
                        match next_line {
                            Ok(Some(line)) => line,
                            Ok(None) => break,
                            Err(error) => {
                                tracing::warn!(%error, "Stdio transport: stdin read failed");
                                break;
                            }
                        }
                    }
                };
                if line.is_empty() {
                    continue;
                }

                // legacy `{"type":"cancel"}` 兜底中断（非 JSON-RPC 行）：
                // 在 JSON parse 失败分支之前拦截——精确 trim 匹配（与迁移前
                // `host/stdio/transport.rs::cancel_debug_hook` 一致）。hook
                // 消费该行后不产生 IncomingMessage；未注入 hook 时该行静默
                // 跳过（不误报 invalid JSON），其余非法行仍走 error 日志跳过。
                if line.trim() == r#"{"type":"cancel"}"# {
                    let hook = {
                        pump_cancel_hook
                            .lock()
                            .expect("cancel hook mutex must remain unpoisoned")
                            .clone()
                    };
                    if let Some(hook) = hook {
                        let raw = line.clone();
                        hook(&raw);
                    } else {
                        tracing::debug!("type:cancel line ignored (no cancel hook installed)");
                    }
                    continue;
                }

                let raw: Value = match serde_json::from_str(&line) {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::warn!(%error, "Stdio transport: malformed JSON-RPC input");
                        if !send_protocol_error_while_reading(
                            &mut lines,
                            &mut deferred,
                            &pump_writer,
                            &pump_router,
                            Value::Null,
                            -32700,
                            "Parse error",
                        )
                        .await
                        {
                            break;
                        }
                        continue;
                    }
                };
                let invalid_id = raw
                    .get("id")
                    .filter(|id| is_domain_id(id))
                    .cloned()
                    .unwrap_or(Value::Null);
                let mut envelope: JsonRpcEnvelope = match serde_json::from_value(raw) {
                    Ok(envelope) => envelope,
                    Err(error) => {
                        tracing::warn!(%error, "Stdio transport: invalid JSON-RPC envelope");
                        if !send_protocol_error_while_reading(
                            &mut lines,
                            &mut deferred,
                            &pump_writer,
                            &pump_router,
                            invalid_id,
                            -32600,
                            "Invalid Request",
                        )
                        .await
                        {
                            break;
                        }
                        continue;
                    }
                };

                let has_method = envelope.method.is_some();
                let result_val = envelope.result.take();
                let error_val = envelope.error.take();

                if envelope.jsonrpc != "2.0"
                    || (has_method && (result_val.is_some() || error_val.is_some()))
                    || (!has_method && result_val.is_some() == error_val.is_some())
                {
                    let id = envelope
                        .id
                        .as_ref()
                        .filter(|id| is_domain_id(id))
                        .cloned()
                        .unwrap_or(Value::Null);
                    if !send_protocol_error_while_reading(
                        &mut lines,
                        &mut deferred,
                        &pump_writer,
                        &pump_router,
                        id,
                        -32600,
                        "Invalid Request",
                    )
                    .await
                    {
                        break;
                    }
                    continue;
                }

                // JSON-RPC 2.0 §2.2：Request 的 id 成员存在但为 null 时视为
                // 通知（客户端无兴趣于对应响应，等同「无 id」）——进入
                // `(None, Some(method))` 通知分支，而非压 0 成请求（决策点 7 收口）。
                let id = match envelope.id {
                    Some(Value::Null) if has_method => None,
                    id => id,
                };

                match (id, envelope.method) {
                    // Response to a server-initiated request (has id, no method)
                    (Some(id), None) => {
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
                        if !pump_router.dispatch(&msg) {
                            // 未匹配响应主机侧不消费（ServerLoop 忽略），队列满时
                            // 不为它阻塞读取：匹配响应已在上面直接交给 router。
                            match incoming_tx.try_send(msg) {
                                Ok(()) => {}
                                Err(mpsc::error::TrySendError::Closed(_)) => break,
                                Err(mpsc::error::TrySendError::Full(_)) => {
                                    tracing::warn!(
                                        event = "acp_stdio_backpressure",
                                        "stdio 入站队列已满，未匹配响应被丢弃"
                                    );
                                }
                            }
                        }
                    }
                    // Request (has id + method)
                    (Some(id), Some(method)) => {
                        if !is_domain_id(&id) {
                            tracing::warn!(
                                id = %id,
                                "Rejecting JSON-RPC request with out-of-domain id"
                            );
                            if !send_protocol_error_while_reading(
                                &mut lines,
                                &mut deferred,
                                &pump_writer,
                                &pump_router,
                                Value::Null,
                                -32600,
                                "Invalid Request",
                            )
                            .await
                            {
                                break;
                            }
                            continue;
                        }
                        let req_id = value_to_request_id(&id);
                        match incoming_tx.try_send(IncomingMessage::Request {
                            id: req_id,
                            method: method.clone(),
                            params: envelope.params.unwrap_or(Value::Null),
                        }) {
                            Ok(()) => {}
                            Err(mpsc::error::TrySendError::Closed(_)) => break,
                            // 背压：不静默丢请求，也不停读 stdin——显式回错误后
                            // 继续处理其后的响应、取消和 EOF。
                            Err(mpsc::error::TrySendError::Full(_)) => {
                                tracing::warn!(
                                    event = "acp_stdio_backpressure",
                                    id = %id,
                                    method = %method,
                                    "stdio 入站队列已满，显式拒绝请求"
                                );
                                if !send_protocol_error_while_reading(
                                    &mut lines,
                                    &mut deferred,
                                    &pump_writer,
                                    &pump_router,
                                    id,
                                    STDIO_BACKPRESSURE_CODE,
                                    STDIO_BACKPRESSURE_MESSAGE,
                                )
                                .await
                                {
                                    break;
                                }
                            }
                        }
                    }
                    // Notification (no id, has method)
                    (None, Some(method)) => {
                        // 通知没有应答通道，不能像请求那样显式拒绝：队列满时等待
                        // 消费者排水（取消等控制消息不丢）。等待期间继续读 stdin，
                        // EOF 与 router close 都不必等消费者排水才被发现。
                        let notification = IncomingMessage::Notification {
                            method,
                            params: envelope.params.unwrap_or(Value::Null),
                        };
                        match incoming_tx.try_send(notification) {
                            Ok(()) => {}
                            Err(mpsc::error::TrySendError::Closed(_)) => break,
                            Err(mpsc::error::TrySendError::Full(notification)) => {
                                if !matches!(
                                    wait_while_reading(
                                        &mut lines,
                                        &mut deferred,
                                        &pump_router,
                                        incoming_tx.send(notification),
                                    )
                                    .await,
                                    PumpWait::Done(Ok(()))
                                ) {
                                    break;
                                }
                            }
                        }
                    }
                    _ => {
                        tracing::warn!("Unhandled JSON-RPC message structure, ignoring");
                    }
                }
            }

            pump_router.close();
            tracing::info!("Stdio transport closed");
        });

        Self {
            incoming_rx: tokio::sync::Mutex::new(incoming_rx),
            router,
            writer,
            cancel_hook,
        }
    }
}

#[async_trait]
impl AcpTransport for StdioTransport {
    async fn send_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        self.send_cancellable_request(method, params, tokio_util::sync::CancellationToken::new())
            .await
    }

    async fn send_cancellable_request(
        &self,
        method: &str,
        params: Value,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<Value, AcpError> {
        let pending = self.router.register()?;

        let envelope = JsonRpcEnvelope {
            jsonrpc: "2.0".to_string(),
            id: Some(Value::Number(pending.id().into())),
            method: Some(method.to_string()),
            params: Some(params),
            result: None,
            error: None,
        };
        write_envelope(&self.writer, &self.router, &envelope).await?;

        super::wait_cancellable(self, pending, cancellation).await
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
        write_envelope(&self.writer, &self.router, &envelope).await
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
                write_envelope(&self.writer, &self.router, &envelope).await
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
                write_envelope(&self.writer, &self.router, &envelope).await
            }
        }
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

async fn send_protocol_error(
    writer: &Arc<Mutex<BufWriter<Box<dyn AsyncWrite + Send + Unpin>>>>,
    router: &RequestRouter,
    id: Value,
    code: i64,
    message: &'static str,
) -> Result<(), AcpError> {
    let envelope = JsonRpcEnvelope {
        jsonrpc: "2.0".to_string(),
        id: Some(id),
        method: None,
        params: None,
        result: None,
        error: Some(AcpError::new(code, message)),
    };
    write_envelope(writer, router, &envelope).await
}

/// [`wait_while_reading`] 的结果：被等待操作完成，或 pump 必须终止（EOF、
/// read error、router close、暂存已满）。
enum PumpWait<T> {
    Done(T),
    Stop,
}

/// 等待 `operation` 完成期间继续读 stdin：读到的行按序暂存到 `deferred`，由主
/// 循环重放。等待不再挡住 EOF，暂存上限保证积压有界。
async fn wait_while_reading<R, T, F>(
    lines: &mut tokio::io::Lines<BufReader<R>>,
    deferred: &mut VecDeque<String>,
    router: &RequestRouter,
    operation: F,
) -> PumpWait<T>
where
    R: AsyncRead + Unpin,
    F: std::future::Future<Output = T>,
{
    tokio::pin!(operation);
    loop {
        if deferred.len() >= STDIO_DEFERRED_CAPACITY {
            tracing::warn!(
                event = "acp_stdio_backpressure",
                "stdio 等待期暂存已满，终止传输以结算 pending"
            );
            return PumpWait::Stop;
        }
        tokio::select! {
            biased;
            () = router.wait_closed() => return PumpWait::Stop,
            value = &mut operation => return PumpWait::Done(value),
            line = lines.next_line() => match line {
                Ok(Some(line)) => deferred.push_back(line),
                Ok(None) => return PumpWait::Stop,
                Err(error) => {
                    tracing::warn!(%error, "Stdio transport: stdin read failed");
                    return PumpWait::Stop;
                }
            },
        }
    }
}

/// pump 内协议错误应答的写入：等待期间继续读 stdin，stdout 阻塞不再挡住 EOF。
async fn send_protocol_error_while_reading<R>(
    lines: &mut tokio::io::Lines<BufReader<R>>,
    deferred: &mut VecDeque<String>,
    writer: &Arc<Mutex<BufWriter<Box<dyn AsyncWrite + Send + Unpin>>>>,
    router: &RequestRouter,
    id: Value,
    code: i64,
    message: &'static str,
) -> bool
where
    R: AsyncRead + Unpin,
{
    matches!(
        wait_while_reading(
            lines,
            deferred,
            router,
            send_protocol_error(writer, router, id, code, message),
        )
        .await,
        PumpWait::Done(Ok(()))
    )
}

async fn write_envelope(
    writer: &Arc<Mutex<BufWriter<Box<dyn AsyncWrite + Send + Unpin>>>>,
    router: &RequestRouter,
    envelope: &JsonRpcEnvelope,
) -> Result<(), AcpError> {
    router.ensure_open()?;
    let mut line = serde_json::to_string(envelope)
        .map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))?;
    line.push('\n');
    let writer_operation = async {
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
    };

    tokio::select! {
        biased;
        result = writer_operation => match result {
            Ok(()) => Ok(()),
            Err(error) => {
                router.close();
                Err(error)
            }
        },
        () = router.wait_closed() => Err(router::transport_closed_error()),
    }
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
