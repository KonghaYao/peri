//! MCP over ACP 桥接 transport。
//!
//! schema 1.10 起 `mcp/message` 是请求作用域的：agent 的每次内层 MCP 请求都
//! 落成一条独立 ACP 请求（`serverId` + `requestId` + `method` + `params`），
//! client 只反向下发通知。本模块保留 rmcp `Transport<RoleClient>` 形态，让
//! 上层 MCP 池与工具桥接无需感知协议形态：
//!
//! - rmcp 请求 → ACP `mcp/message` 请求（每请求生成新的 `requestId`）；
//! - 成功响应的 `result` / `error` → rmcp 响应 / 内层 MCP 错误；
//! - client 反向下发的 `mcp/message` 通知 → 注入 rmcp 接收队列。
//!
//! rmcp 的 lifecycle 协商（先 `server/discover`，失败回退 `initialize`）原样
//! 复用：两类方法都是普通请求，桥接不需要特殊分支。consumer → provider 的
//! 通知在 schema 1.12 没有承载方向（`AgentNotification` 无 `mcp/message`），
//! 因此丢弃并记 debug；现代 lifecycle 不依赖 `notifications/initialized`。
//!
//! 消息以 JSON 载荷中转（`mcp/message` 承载的本就是内层 MCP 消息），不复制
//! rmcp 的类型层次；内层请求 id 由 rmcp 生成，ACP `requestId` 与它独立。
//!
//! 关闭是单点信号（[`BridgeClose`]）：任一侧触发后，rmcp 的 `receive()` 与
//! 出站转发任务同时收敛，不会留下悬挂的通道或任务。

use std::future::Future;
use std::sync::Arc;

use peri_acp_types::acp_mcp::AcpMcpError;
use peri_acp_types::ports::AcpMcpGatewayPort;
use rmcp::model::{ErrorCode, ErrorData, NumberOrString, RequestId};
use rmcp::service::{RoleClient, RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::Transport;
use serde_json::{Map, Value};
use tokio::sync::mpsc;

/// ACP `mcp/message` 的方法名（协议常量）。
pub(crate) const MCP_MESSAGE_METHOD: &str = "mcp/message";

/// 桥接 transport 的错误类型（仅表达通道关闭）。
#[derive(Debug, thiserror::Error)]
pub(crate) enum AcpBridgeError {
    #[error("MCP over ACP 桥接通道已关闭")]
    Closed,
}

/// 单点关闭信号：`close()` 之后 rmcp 入站与出站转发同时失效。
///
/// 用「标志位 + `notify_waiters`」而非直接关通道：tokio 的 mpsc 关闭只能由
/// 接收端发起，而关闭可能来自任一侧（rmcp 释放 transport、会话关闭、池关闭）。
struct BridgeClose {
    closed: std::sync::atomic::AtomicBool,
    notify: tokio::sync::Notify,
}

impl BridgeClose {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            closed: std::sync::atomic::AtomicBool::new(false),
            notify: tokio::sync::Notify::new(),
        })
    }

    fn close(&self) {
        self.closed
            .store(true, std::sync::atomic::Ordering::Release);
        self.notify.notify_waiters();
    }

    fn is_closed(&self) -> bool {
        self.closed.load(std::sync::atomic::Ordering::Acquire)
    }

    /// 等待关闭；已关闭时立即返回（先登记再复查，避免错过 `notify_waiters`）。
    async fn wait(&self) {
        while !self.is_closed() {
            let notified = self.notify.notified();
            if self.is_closed() {
                return;
            }
            notified.await;
        }
    }
}

/// rmcp 侧的 transport 实现：出站经 channel 交给桥接任务，入站从 channel 读取。
pub(crate) struct AcpBridgeTransport {
    outbound_tx: mpsc::UnboundedSender<TxJsonRpcMessage<RoleClient>>,
    inbound_rx: mpsc::UnboundedReceiver<RxJsonRpcMessage<RoleClient>>,
    close: Arc<BridgeClose>,
}

impl Transport<RoleClient> for AcpBridgeTransport {
    type Error = AcpBridgeError;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let tx = self.outbound_tx.clone();
        let close = Arc::clone(&self.close);
        async move {
            if close.is_closed() {
                return Err(AcpBridgeError::Closed);
            }
            tx.send(item).map_err(|_| AcpBridgeError::Closed)
        }
    }

    fn receive(&mut self) -> impl Future<Output = Option<RxJsonRpcMessage<RoleClient>>> + Send {
        let rx = &mut self.inbound_rx;
        let close = Arc::clone(&self.close);
        async move {
            tokio::select! {
                biased;
                // 关闭优先：会话结束时不再等待 client 的下一条消息。
                () = close.wait() => None,
                message = rx.recv() => message,
            }
        }
    }

    fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        // 关闭信号驱动：出站转发任务与入站接收同时收敛。
        self.close.close();
        async { Ok(()) }
    }
}

/// 桥接连接句柄：宿主经它把 client 反向下发的通知注入 rmcp。
pub(crate) struct AcpBridgeHandle {
    inbound_tx: mpsc::UnboundedSender<RxJsonRpcMessage<RoleClient>>,
    close: Arc<BridgeClose>,
}

impl AcpBridgeHandle {
    /// client 下发的 `mcp/message` 通知：投递后即返回。
    pub(crate) fn notify(&self, method: &str, params: Option<Value>) -> Result<(), ErrorData> {
        if self.close.is_closed() {
            return Err(connection_closed());
        }
        let message = build_notification_message(method, params)?;
        self.inbound_tx
            .send(message)
            .map_err(|_| connection_closed())
    }

    /// 关闭桥接：rmcp 的 `receive()` 在排空入站队列后返回 `None`，出站转发任务退出。
    pub(crate) fn close(&self) {
        self.close.close();
    }
}

/// 建立一次桥接：返回 rmcp 侧 transport、外部句柄与出站转发任务。
pub(crate) fn create_bridge(
    gateway: Arc<dyn AcpMcpGatewayPort>,
    server_id: String,
) -> (AcpBridgeTransport, Arc<AcpBridgeHandle>, BridgeRunner) {
    let (outbound_tx, outbound_rx) = mpsc::unbounded_channel();
    let (inbound_tx, inbound_rx) = mpsc::unbounded_channel();
    let close = BridgeClose::new();
    let transport = AcpBridgeTransport {
        outbound_tx,
        inbound_rx,
        close: Arc::clone(&close),
    };
    let handle = Arc::new(AcpBridgeHandle {
        inbound_tx: inbound_tx.clone(),
        close: Arc::clone(&close),
    });
    let runner = BridgeRunner {
        gateway,
        server_id,
        next_request_id: std::sync::atomic::AtomicU64::new(1),
        outbound_rx,
        inbound_tx,
        close,
    };
    (transport, handle, runner)
}

/// 出站转发任务：rmcp 发出的请求 → ACP `mcp/message`。
pub(crate) struct BridgeRunner {
    gateway: Arc<dyn AcpMcpGatewayPort>,
    server_id: String,
    next_request_id: std::sync::atomic::AtomicU64,
    outbound_rx: mpsc::UnboundedReceiver<TxJsonRpcMessage<RoleClient>>,
    inbound_tx: mpsc::UnboundedSender<RxJsonRpcMessage<RoleClient>>,
    close: Arc<BridgeClose>,
}

impl BridgeRunner {
    /// 运行直到关闭信号触发或出站通道关闭（transport 被 rmcp 释放）。
    pub(crate) async fn run(mut self) {
        loop {
            let message = tokio::select! {
                biased;
                () = self.close.wait() => break,
                message = self.outbound_rx.recv() => match message {
                    Some(message) => message,
                    None => break,
                },
            };
            let Ok(value) = serde_json::to_value(&message) else {
                tracing::warn!(server_id = %self.server_id, "MCP over ACP 消息序列化失败");
                continue;
            };
            let Some(method) = value.get("method").and_then(Value::as_str) else {
                // 响应 / 错误只可能属于 client → agent 请求；该方向已被协议移除。
                tracing::debug!(
                    server_id = %self.server_id,
                    "忽略无方法的 MCP over ACP 出站消息"
                );
                continue;
            };
            let method = method.to_owned();
            let params = value.get("params").cloned();
            if value.get("id").and_then(json_request_id).is_none() {
                // schema 1.12 没有 consumer → provider 通知；现代 lifecycle 也不
                // 依赖 `notifications/initialized`。丢弃而不是写成反向通知。
                tracing::debug!(
                    server_id = %self.server_id,
                    method,
                    "MCP over ACP 丢弃 consumer 侧通知"
                );
                continue;
            }

            let request_id = format!(
                "peri-{}",
                self.next_request_id
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            );
            let gateway = Arc::clone(&self.gateway);
            let server_id = self.server_id.clone();
            let inbound_tx = self.inbound_tx.clone();
            let rmcp_id = value.get("id").and_then(json_request_id);
            tokio::spawn(async move {
                let result = forward_request(
                    gateway.as_ref(),
                    &server_id,
                    &request_id,
                    &method,
                    params.map(json_object),
                )
                .await;
                if let Some(id) = rmcp_id {
                    if let Some(message) = build_response_message(&id, result) {
                        let _ = inbound_tx.send(message);
                    } else {
                        tracing::warn!(
                            server_id = %server_id,
                            "MCP over ACP 响应回填失败，请求将按取消结算"
                        );
                    }
                }
            });
        }
    }
}

/// 把 rmcp 请求经 ACP `mcp/message` 发出，返回内层 MCP 结果。
async fn forward_request(
    gateway: &dyn AcpMcpGatewayPort,
    server_id: &str,
    request_id: &str,
    method: &str,
    params: Option<Map<String, Value>>,
) -> Result<Value, ErrorData> {
    let response = gateway
        .request(
            MCP_MESSAGE_METHOD,
            message_params(server_id, request_id, method, params),
        )
        .await
        .map_err(inner_error)?;
    parse_message_response(response)
}

/// `MessageMcpResponse` → 内层 MCP 结果。
///
/// 协议把内层结果与内层错误分成两个互斥分支：`{ result }` 与
/// `{ error: { code, message, data? } }`。外层 ACP 错误已在
/// [`forward_request`] 中处理，这里只解析成功的外层响应。
fn parse_message_response(response: Value) -> Result<Value, ErrorData> {
    let Value::Object(mut object) = response else {
        return Err(ErrorData::invalid_request(
            "mcp/message 响应必须是对象",
            None,
        ));
    };
    if let Some(error) = object.remove("error") {
        return Err(parse_inner_error(error));
    }
    Ok(object.remove("result").unwrap_or(Value::Null))
}

fn parse_inner_error(error: Value) -> ErrorData {
    let code = error
        .get("code")
        .and_then(Value::as_i64)
        .and_then(|code| i32::try_from(code).ok())
        .unwrap_or(ErrorCode::INTERNAL_ERROR.0);
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("MCP 错误载荷缺少 message")
        .to_string();
    let data = error.get("data").cloned().filter(|data| !data.is_null());
    ErrorData::new(ErrorCode(code), message, data)
}

/// ACP 外层错误 → 内层 MCP 错误。
///
/// `mcp/message` 的 ACP 错误码按协议约定就是绑定 / 运行面错误；内层 MCP
/// 方法级错误走成功响应里的 `error` 分支（[`parse_message_response`]）。
/// 码值超出 rmcp 的 `i32` 表示时退回内部错误。
fn inner_error(error: AcpMcpError) -> ErrorData {
    let code = i32::try_from(error.code).unwrap_or(ErrorCode::INTERNAL_ERROR.0);
    ErrorData::new(ErrorCode(code), error.message, None)
}

/// `mcp/message` 载荷：`{ serverId, requestId, method, params }`
/// （params 缺省省略）。
fn message_params(
    server_id: &str,
    request_id: &str,
    method: &str,
    params: Option<Map<String, Value>>,
) -> Value {
    let mut object = Map::new();
    object.insert("serverId".to_string(), Value::String(server_id.to_string()));
    object.insert(
        "requestId".to_string(),
        Value::String(request_id.to_string()),
    );
    object.insert("method".to_string(), Value::String(method.to_string()));
    if let Some(params) = params {
        object.insert("params".to_string(), Value::Object(params));
    }
    Value::Object(object)
}

/// 非对象 params（如数组）按缺省处理：MCP 请求参数约定为对象或省略。
fn json_object(params: Value) -> Map<String, Value> {
    match params {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// JSON id → rmcp `RequestId`（数字或字符串）。
fn json_request_id(value: &Value) -> Option<RequestId> {
    match value {
        Value::Number(number) => number.as_i64().map(NumberOrString::Number),
        Value::String(text) => Some(NumberOrString::String(text.as_str().into())),
        _ => None,
    }
}

fn connection_closed() -> ErrorData {
    ErrorData::internal_error("MCP over ACP 连接已关闭".to_string(), None)
}

fn request_id_value(id: &RequestId) -> Value {
    serde_json::to_value(id).unwrap_or(Value::Null)
}

/// 构造注入 rmcp 的通知消息。
fn build_notification_message(
    method: &str,
    params: Option<Value>,
) -> Result<RxJsonRpcMessage<RoleClient>, ErrorData> {
    let mut object = Map::new();
    object.insert("jsonrpc".to_string(), Value::String("2.0".to_string()));
    object.insert("method".to_string(), Value::String(method.to_string()));
    if let Some(params) = params {
        object.insert("params".to_string(), params);
    }
    parse_inbound(Value::Object(object))
}

/// 构造回填 rmcp 的响应 / 错误消息；解析失败返回 `None`（调用方丢弃并 warn）。
fn build_response_message(
    id: &RequestId,
    result: Result<Value, ErrorData>,
) -> Option<RxJsonRpcMessage<RoleClient>> {
    let mut object = Map::new();
    object.insert("jsonrpc".to_string(), Value::String("2.0".to_string()));
    object.insert("id".to_string(), request_id_value(id));
    match result {
        Ok(result) => {
            object.insert("result".to_string(), result);
        }
        Err(error) => {
            object.insert(
                "error".to_string(),
                serde_json::to_value(&error).unwrap_or(Value::Null),
            );
        }
    }
    parse_inbound(Value::Object(object)).ok()
}

/// JSON → rmcp 入站消息；解析失败返回描述性错误，由调用方决定降级方式。
fn parse_inbound(value: Value) -> Result<RxJsonRpcMessage<RoleClient>, ErrorData> {
    serde_json::from_value(value).map_err(|error| {
        ErrorData::invalid_request(format!("MCP over ACP 消息解析失败: {error}"), None)
    })
}
