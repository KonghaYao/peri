//! MCP over ACP 桥接 transport。
//!
//! 把 rmcp 的 JSON-RPC 消息经 ACP `mcp/message` 双向转发：
//!
//! - rmcp 发出的请求 / 通知 → ACP 请求 / 通知（`connectionId` 定位连接）；
//! - rmcp 对 client 请求的响应 → 结算挂起的 ACP 请求；
//! - client 经 `mcp/message` 反向下发的请求 / 通知 → 注入 rmcp 的接收队列。
//!
//! 消息以 JSON 载荷中转（`mcp/message` 承载的本就是内层 MCP 消息），不复制
//! rmcp 的类型层次；内层请求 id 由 rmcp 生成并在本模块内配对。
//!
//! 关闭是单点信号（[`BridgeClose`]）：任一侧触发后，rmcp 的 `receive()` 与
//! 出站转发任务同时收敛，不会留下悬挂的通道或任务。

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use parking_lot::Mutex;
use peri_acp_types::acp_mcp::AcpMcpError;
use peri_acp_types::ports::AcpMcpGatewayPort;
use rmcp::model::{ErrorCode, ErrorData, NumberOrString, RequestId};
use rmcp::service::{RoleClient, RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::Transport;
use serde_json::{Map, Value};
use tokio::sync::{mpsc, oneshot};

/// ACP `mcp/message` 的方法名（协议常量）。
pub(crate) const MCP_MESSAGE_METHOD: &str = "mcp/message";

/// ACP `mcp/connect` 的方法名：agent → client，返回 `connectionId`。
pub(crate) const MCP_CONNECT_METHOD: &str = "mcp/connect";

/// ACP `mcp/disconnect` 的方法名：agent → client，关闭一条连接。
pub(crate) const MCP_DISCONNECT_METHOD: &str = "mcp/disconnect";

/// 桥接 transport 的错误类型（仅表达通道关闭）。
#[derive(Debug, thiserror::Error)]
pub(crate) enum AcpBridgeError {
    #[error("MCP over ACP 桥接通道已关闭")]
    Closed,
}

/// 单点关闭信号：`close()` 之后 rmcp 入站、出站转发与句柄注入同时失效。
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

/// 桥接连接句柄：外部（会话管理器 / ACP 面）经它向 rmcp 注入反向下发的消息。
pub(crate) struct AcpBridgeHandle {
    inbound_tx: mpsc::UnboundedSender<RxJsonRpcMessage<RoleClient>>,
    pending: Arc<Mutex<HashMap<RequestId, oneshot::Sender<Result<Value, ErrorData>>>>>,
    next_id: std::sync::atomic::AtomicI64,
    close: Arc<BridgeClose>,
}

impl AcpBridgeHandle {
    /// client 下发的 `mcp/message` 请求：注入 rmcp 并等待其响应。
    ///
    /// 无内置超时：连接静默与调用方的取消语义由 ACP 层持有。
    pub(crate) async fn request(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, ErrorData> {
        if self.close.is_closed() {
            return Err(connection_closed());
        }
        let id = RequestId::Number(
            self.next_id
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        );
        let message = build_request_message(&id, method, params)?;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(id.clone(), tx);
        if self.inbound_tx.send(message).is_err() {
            self.pending.lock().remove(&id);
            return Err(connection_closed());
        }
        match rx.await {
            Ok(result) => result,
            Err(_) => Err(connection_closed()),
        }
    }

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
    connection_id: String,
) -> (AcpBridgeTransport, Arc<AcpBridgeHandle>, BridgeRunner) {
    let (outbound_tx, outbound_rx) = mpsc::unbounded_channel();
    let (inbound_tx, inbound_rx) = mpsc::unbounded_channel();
    let pending: Arc<Mutex<HashMap<RequestId, oneshot::Sender<Result<Value, ErrorData>>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let close = BridgeClose::new();
    let transport = AcpBridgeTransport {
        outbound_tx,
        inbound_rx,
        close: Arc::clone(&close),
    };
    let handle = Arc::new(AcpBridgeHandle {
        inbound_tx: inbound_tx.clone(),
        pending: Arc::clone(&pending),
        next_id: std::sync::atomic::AtomicI64::new(1),
        close: Arc::clone(&close),
    });
    let runner = BridgeRunner {
        gateway,
        connection_id,
        outbound_rx,
        inbound_tx,
        pending,
        close,
    };
    (transport, handle, runner)
}

/// 出站转发任务：rmcp 发出的消息 → ACP `mcp/message`。
pub(crate) struct BridgeRunner {
    gateway: Arc<dyn AcpMcpGatewayPort>,
    connection_id: String,
    outbound_rx: mpsc::UnboundedReceiver<TxJsonRpcMessage<RoleClient>>,
    inbound_tx: mpsc::UnboundedSender<RxJsonRpcMessage<RoleClient>>,
    pending: Arc<Mutex<HashMap<RequestId, oneshot::Sender<Result<Value, ErrorData>>>>>,
    close: Arc<BridgeClose>,
}

impl BridgeRunner {
    /// 运行直到关闭信号触发或出站通道关闭（transport 被 rmcp 释放）。
    pub(crate) async fn run(self) {
        let Self {
            gateway,
            connection_id,
            mut outbound_rx,
            inbound_tx,
            pending,
            close,
        } = self;
        loop {
            let message = tokio::select! {
                biased;
                () = close.wait() => break,
                message = outbound_rx.recv() => match message {
                    Some(message) => message,
                    None => break,
                },
            };
            let Ok(value) = serde_json::to_value(&message) else {
                tracing::warn!(connection_id = %connection_id, "MCP over ACP 消息序列化失败");
                continue;
            };
            let Some(method) = value.get("method").and_then(Value::as_str) else {
                // 响应 / 错误：结算挂起的 client 请求。
                settle_pending(&value, &pending);
                continue;
            };
            let method = method.to_owned();
            let params = value.get("params").cloned();
            match value.get("id").and_then(json_request_id) {
                // 请求：转发为 ACP 请求，响应回填为 rmcp 的 Response / Error。
                Some(id) => {
                    let gateway = Arc::clone(&gateway);
                    let connection_id = connection_id.clone();
                    let inbound_tx = inbound_tx.clone();
                    tokio::spawn(async move {
                        let result =
                            forward_request(gateway.as_ref(), &connection_id, &method, params)
                                .await;
                        match build_response_message(&id, result) {
                            Some(message) => {
                                let _ = inbound_tx.send(message);
                            }
                            None => tracing::warn!(
                                connection_id = %connection_id,
                                "MCP over ACP 响应回填失败，请求将按取消结算"
                            ),
                        }
                    });
                }
                // 通知：无 id，不等待响应。
                None => {
                    if let Err(error) = gateway
                        .notify(
                            MCP_MESSAGE_METHOD,
                            message_params(&connection_id, &method, params.map(json_object)),
                        )
                        .await
                    {
                        tracing::warn!(
                            connection_id = %connection_id,
                            %error,
                            "MCP over ACP 通知转发失败"
                        );
                    }
                }
            }
        }
    }
}

/// 把 rmcp 请求经 ACP `mcp/message` 发出，返回内层 MCP 结果。
async fn forward_request(
    gateway: &dyn AcpMcpGatewayPort,
    connection_id: &str,
    method: &str,
    params: Option<Value>,
) -> Result<Value, ErrorData> {
    gateway
        .request(
            MCP_MESSAGE_METHOD,
            message_params(connection_id, method, params.map(json_object)),
        )
        .await
        .map_err(inner_error)
}

/// ACP 错误 → 内层 MCP 错误。
///
/// `mcp/message` 请求的错误码按协议约定就是内层 JSON-RPC 错误码
/// （`AcpMcpServerPort::request` 反向同理），因此原样还原码值：rmcp 的
/// lifecycle 协商依赖 `-32601` 等方法级错误码判定 legacy 回退，抹平成
/// `-32603` 会让「MCP 方法不存在」与「连接故障」不可区分。码值超出
/// rmcp 的 `i32` 表示时退回内部错误。
fn inner_error(error: AcpMcpError) -> ErrorData {
    let code = i32::try_from(error.code).unwrap_or(ErrorCode::INTERNAL_ERROR.0);
    ErrorData::new(ErrorCode(code), error.message, None)
}

/// `mcp/message` 载荷：`{ connectionId, method, params }`（params 缺省省略）。
fn message_params(connection_id: &str, method: &str, params: Option<Map<String, Value>>) -> Value {
    let mut object = Map::new();
    object.insert(
        "connectionId".to_string(),
        Value::String(connection_id.to_string()),
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

/// 结算挂起的 client 请求：Response → 结果，Error → 错误。
fn settle_pending(
    value: &Value,
    pending: &Mutex<HashMap<RequestId, oneshot::Sender<Result<Value, ErrorData>>>>,
) {
    let Some(id) = value.get("id").and_then(json_request_id) else {
        return;
    };
    let Some(sender) = pending.lock().remove(&id) else {
        return;
    };
    let result = match value.get("error") {
        Some(error) => Err(
            serde_json::from_value::<ErrorData>(error.clone()).unwrap_or_else(|_| {
                ErrorData::internal_error("MCP 错误载荷无法解析".to_string(), None)
            }),
        ),
        None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
    };
    let _ = sender.send(result);
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

/// 构造注入 rmcp 的请求消息。
fn build_request_message(
    id: &RequestId,
    method: &str,
    params: Option<Value>,
) -> Result<RxJsonRpcMessage<RoleClient>, ErrorData> {
    let mut object = Map::new();
    object.insert("jsonrpc".to_string(), Value::String("2.0".to_string()));
    object.insert("id".to_string(), request_id_value(id));
    object.insert("method".to_string(), Value::String(method.to_string()));
    if let Some(params) = params {
        object.insert("params".to_string(), params);
    }
    parse_inbound(Value::Object(object))
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
