//! 请求作用域 MCP-over-ACP transport：每个 rmcp 请求对应一个 ACP 请求。
//!
//! 桥接持有全部转发任务与活跃 ID 映射。取消通过网关关联到外层 ACP 请求；
//! 其他 consumer 通知仍忽略。响应、订阅 ACK 与更新仅接受活跃 requestId。

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol_schema::{v1::MessageMcpResponse, MaybeUndefined};
use parking_lot::Mutex;
use peri_acp_types::acp_mcp::AcpMcpError;
use peri_acp_types::ports::AcpMcpGatewayPort;
use rmcp::model::{ErrorCode, ErrorData, RequestId};
use rmcp::service::{RoleClient, RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::Transport;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

struct Pending {
    rmcp_id: RequestId,
    cancellation: CancellationToken,
}

#[derive(Default)]
struct Requests {
    by_acp: HashMap<String, Pending>,
    by_rmcp: HashMap<RequestId, String>,
}

impl Requests {
    fn remove(&mut self, id: &str) -> Option<Pending> {
        let pending = self.by_acp.remove(id)?;
        self.by_rmcp.remove(&pending.rmcp_id);
        Some(pending)
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum AcpBridgeError {
    #[error("MCP over ACP 桥接通道已关闭")]
    Closed,
}

pub(crate) struct AcpBridgeTransport {
    outbound_tx: mpsc::UnboundedSender<TxJsonRpcMessage<RoleClient>>,
    inbound_rx: mpsc::UnboundedReceiver<RxJsonRpcMessage<RoleClient>>,
    close: CancellationToken,
    finished: CancellationToken,
}

impl Drop for AcpBridgeTransport {
    fn drop(&mut self) {
        self.close.cancel();
    }
}

impl Transport<RoleClient> for AcpBridgeTransport {
    type Error = AcpBridgeError;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let tx = self.outbound_tx.clone();
        let close = self.close.clone();
        async move {
            if close.is_cancelled() {
                return Err(AcpBridgeError::Closed);
            }
            tx.send(item).map_err(|_| AcpBridgeError::Closed)
        }
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleClient>> {
        tokio::select! {
            biased;
            message = self.inbound_rx.recv() => message,
            () = self.finished.cancelled() => None,
        }
    }

    fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        self.close.cancel();
        async move {
            self.finished.cancelled().await;
            Ok(())
        }
    }
}

pub(crate) struct AcpBridgeHandle {
    inbound_tx: mpsc::UnboundedSender<RxJsonRpcMessage<RoleClient>>,
    requests: Arc<Mutex<Requests>>,
    close: CancellationToken,
    runner: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl AcpBridgeHandle {
    pub(crate) fn notify(
        &self,
        request_id: &str,
        method: &str,
        mut params: Option<Value>,
    ) -> Result<(), ErrorData> {
        if self.close.is_cancelled() {
            return Err(connection_closed());
        }
        // 校验、转换和入队与请求结算共用短锁，防止完成后接受迟到通知。
        let requests = self.requests.lock();
        let pending = requests.by_acp.get(request_id).ok_or_else(|| {
            tracing::warn!(request_id, "拒绝未知或已结束请求的 MCP over ACP 通知");
            ErrorData::invalid_request("未知或已结束的 MCP over ACP requestId", None)
        })?;
        if let Some(params) = params.as_mut() {
            translate_ids(params, request_id, &pending.rmcp_id)?;
        }
        let message = serde_json::from_value(json!({
            "jsonrpc": "2.0", "method": method, "params": params
        }))
        .map_err(|error| ErrorData::invalid_request(format!("MCP 通知解析失败: {error}"), None))?;
        self.inbound_tx
            .send(message)
            .map_err(|_| connection_closed())
    }

    pub(crate) async fn close(&self) {
        self.close.cancel();
        // 持有异步锁直到 join 完成：并发 close 也必须观察到任务已结束。
        let mut runner = self.runner.lock().await;
        if let Some(task) = runner.as_mut() {
            if let Err(error) = task.await {
                tracing::error!(%error, "MCP over ACP 转发任务异常退出");
            }
            runner.take();
        }
    }
}

pub(crate) fn create_bridge(
    gateway: Arc<dyn AcpMcpGatewayPort>,
    server_id: String,
) -> (AcpBridgeTransport, Arc<AcpBridgeHandle>) {
    let (outbound_tx, outbound_rx) = mpsc::unbounded_channel();
    let (inbound_tx, inbound_rx) = mpsc::unbounded_channel();
    let close = CancellationToken::new();
    let finished = CancellationToken::new();
    let requests = Arc::new(Mutex::new(Requests::default()));
    let runner = BridgeRunner {
        gateway,
        server_id,
        outbound_rx,
        inbound_tx: inbound_tx.clone(),
        requests: requests.clone(),
        close: close.clone(),
        finished: finished.clone(),
    };
    let task = tokio::spawn(runner.run());
    (
        AcpBridgeTransport {
            outbound_tx,
            inbound_rx,
            close: close.clone(),
            finished,
        },
        Arc::new(AcpBridgeHandle {
            inbound_tx,
            requests,
            close,
            runner: tokio::sync::Mutex::new(Some(task)),
        }),
    )
}

struct BridgeRunner {
    gateway: Arc<dyn AcpMcpGatewayPort>,
    server_id: String,
    outbound_rx: mpsc::UnboundedReceiver<TxJsonRpcMessage<RoleClient>>,
    inbound_tx: mpsc::UnboundedSender<RxJsonRpcMessage<RoleClient>>,
    requests: Arc<Mutex<Requests>>,
    close: CancellationToken,
    finished: CancellationToken,
}

impl BridgeRunner {
    async fn run(mut self) {
        let _finished = self.finished.clone().drop_guard();
        let mut tasks = JoinSet::new();
        let mut next_id = 1u64;
        loop {
            let message = tokio::select! {
                biased;
                () = self.close.cancelled() => break,
                result = tasks.join_next(), if !tasks.is_empty() => {
                    if let Some(Err(error)) = result {
                        tracing::error!(%error, "MCP over ACP 请求任务异常退出");
                        break;
                    }
                    continue;
                }
                message = self.outbound_rx.recv() => match message {
                    Some(message) => message,
                    None => break,
                },
            };
            let value = match serde_json::to_value(message) {
                Ok(value) => value,
                Err(error) => {
                    tracing::error!(%error, "MCP over ACP 消息序列化失败");
                    break;
                }
            };
            let Some(method) = value.get("method").and_then(Value::as_str) else {
                continue;
            };
            let Some(rmcp_id) = value
                .get("id")
                .and_then(|id| serde_json::from_value::<RequestId>(id.clone()).ok())
            else {
                if method == "notifications/cancelled" {
                    self.cancel(&value);
                } else {
                    tracing::debug!(method, "MCP over ACP 忽略 consumer 侧通知");
                }
                continue;
            };
            let request_id = format!("peri-{next_id}");
            next_id += 1;
            let cancellation = CancellationToken::new();
            {
                let mut requests = self.requests.lock();
                requests.by_rmcp.insert(rmcp_id.clone(), request_id.clone());
                requests.by_acp.insert(
                    request_id.clone(),
                    Pending {
                        rmcp_id,
                        cancellation: cancellation.clone(),
                    },
                );
            }
            let gateway = self.gateway.clone();
            let requests = self.requests.clone();
            let inbound_tx = self.inbound_tx.clone();
            let params = json!({"serverId": self.server_id, "requestId": request_id, "method": method, "params": value.get("params")});
            let subscription = method == "subscriptions/listen";
            tasks.spawn(async move {
                // 网关先获得取消机会以发出官方取消；不合作的实现也不能阻塞关闭。
                let response = tokio::select! {
                    result = gateway.request("mcp/message", params, cancellation.clone()) => result.map_err(inner_error).and_then(parse_message_response),
                    () = async { cancellation.cancelled().await; tokio::time::sleep(Duration::from_secs(2)).await; } => {
                        tracing::error!(request_id, "MCP over ACP 网关取消超时，终止转发");
                        Err(connection_closed())
                    },
                };
                if let Err(error) = &response {
                    if error.code != ErrorCode(-32800) {
                        tracing::warn!(request_id, code = error.code.0, "MCP over ACP 请求失败");
                    }
                }
                let mut requests = requests.lock();
                if let Some(pending) = requests.remove(&request_id) {
                    let result = response.and_then(|mut value| {
                        if subscription {
                            translate_ids(&mut value, &request_id, &pending.rmcp_id)?;
                        }
                        Ok(value)
                    });
                    send_response(&inbound_tx, &pending.rmcp_id, result);
                }
            });
        }
        self.close.cancel();
        self.outbound_rx.close();
        {
            let mut requests = self.requests.lock();
            for (_, pending) in requests.by_acp.drain() {
                pending.cancellation.cancel();
                send_response(&self.inbound_tx, &pending.rmcp_id, Err(connection_closed()));
            }
            requests.by_rmcp.clear();
        }
        // 已入出站队列但未转发的请求同样必须结算。
        while let Ok(message) = self.outbound_rx.try_recv() {
            if let Ok(value) = serde_json::to_value(message) {
                if let Some(id) = value
                    .get("id")
                    .and_then(|id| serde_json::from_value(id.clone()).ok())
                {
                    send_response(&self.inbound_tx, &id, Err(connection_closed()));
                }
            }
        }
        while let Some(result) = tasks.join_next().await {
            if let Err(error) = result {
                tracing::error!(%error, "MCP over ACP 请求任务关闭失败");
            }
        }
    }

    fn cancel(&self, value: &Value) {
        let Some(id) = value
            .pointer("/params/requestId")
            .and_then(|id| serde_json::from_value::<RequestId>(id.clone()).ok())
        else {
            tracing::warn!("MCP 取消通知缺少有效 requestId");
            return;
        };
        let mut requests = self.requests.lock();
        let Some(request_id) = requests.by_rmcp.get(&id).cloned() else {
            tracing::warn!(?id, "拒绝未知或已结束 MCP 请求的取消");
            return;
        };
        if let Some(pending) = requests.remove(&request_id) {
            pending.cancellation.cancel();
            send_response(
                &self.inbound_tx,
                &id,
                Err(ErrorData::new(ErrorCode(-32800), "request cancelled", None)),
            );
        }
    }
}

/// 只转换协议定义的关联 ID，不递归改写工具结果或用户数据中的同名字段。
fn translate_ids(value: &mut Value, acp_id: &str, rmcp_id: &RequestId) -> Result<(), ErrorData> {
    for pointer in [
        "/requestId",
        "/_meta/io.modelcontextprotocol~1subscriptionId",
    ] {
        if let Some(id) = value.pointer_mut(pointer) {
            if id.as_str() != Some(acp_id) {
                return Err(ErrorData::invalid_request(
                    "MCP 关联 ID 与 ACP requestId 不一致",
                    None,
                ));
            }
            *id = serde_json::to_value(rmcp_id)
                .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
        }
    }
    Ok(())
}

fn parse_message_response(response: Value) -> Result<Value, ErrorData> {
    let response: MessageMcpResponse = serde_json::from_value(response).map_err(|error| {
        ErrorData::invalid_request(format!("无效 mcp/message 响应: {error}"), None)
    })?;
    match response {
        MessageMcpResponse::Result { result, .. } => Ok(result),
        MessageMcpResponse::Error { error, .. } => {
            let data = match error.data {
                MaybeUndefined::Undefined => None,
                MaybeUndefined::Null => Some(Value::Null),
                MaybeUndefined::Value(value) => Some(value),
            };
            Err(ErrorData::new(ErrorCode(error.code), error.message, data))
        }
        _ => Err(ErrorData::invalid_request(
            "未知 mcp/message 响应类型",
            None,
        )),
    }
}

fn inner_error(error: AcpMcpError) -> ErrorData {
    ErrorData::new(
        ErrorCode(i32::try_from(error.code).unwrap_or(ErrorCode::INTERNAL_ERROR.0)),
        error.message,
        None,
    )
}

fn connection_closed() -> ErrorData {
    ErrorData::internal_error("MCP over ACP 连接已关闭", None)
}

fn send_response(
    tx: &mpsc::UnboundedSender<RxJsonRpcMessage<RoleClient>>,
    id: &RequestId,
    result: Result<Value, ErrorData>,
) {
    let wire = match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
    };
    let message = serde_json::from_value(wire).unwrap_or_else(|error| {
        tracing::warn!(%error, "MCP 响应解析失败，以错误结算请求");
        rmcp::model::JsonRpcMessage::error(
            ErrorData::invalid_request("无效内层 MCP 响应", None),
            Some(id.clone()),
        )
    });
    let _ = tx.send(message);
}

#[cfg(test)]
#[path = "transport_test.rs"]
mod tests;
