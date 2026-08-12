//! JSON-RPC 2.0 双向通道，通过 stdio 与 Node.js 子进程通信。
//!
//! newline-delimited JSON 帧：每行一条消息，`\n` 分隔。
//! `parse_message()` 为纯函数，便于测试；`RpcChannel` 管理请求/响应路由。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{oneshot, Mutex};

use crate::error::WorkflowError;
use crate::protocol::JsonRpcError;

// ─── 纯解析（无副作用，可独立测试）────────────────────────

/// 解析后的消息：区分 Response（有 id + result/error）和 Request（有 method）。
#[derive(Debug)]
pub enum ParsedMessage {
    Response {
        id: u64,
        result: Option<Value>,
        error: Option<JsonRpcError>,
    },
    Request {
        id: Option<u64>,
        method: String,
        params: Option<Value>,
    },
}

/// 纯函数：解析一行 JSON-RPC 文本。
///
/// 判定规则：
/// - 有 `id`（数值）且包含 `result` 或 `error` → Response
/// - 有 `method` → Request / Notification
/// - 否则 → None
pub fn parse_message(raw: &str) -> Option<ParsedMessage> {
    let v: Value = serde_json::from_str(raw).ok()?;
    let id = v.get("id").and_then(|v| v.as_u64());

    // 有 result 或 error 字段 → Response
    if v.get("result").is_some() || v.get("error").is_some() {
        let id = id?;
        let result = v.get("result").cloned();
        let error = v
            .get("error")
            .cloned()
            .and_then(|e| serde_json::from_value::<JsonRpcError>(e).ok());
        return Some(ParsedMessage::Response { id, result, error });
    }

    // 有 method → Request / Notification
    if let Some(method) = v.get("method").and_then(|m| m.as_str()) {
        let params = v.get("params").cloned();
        return Some(ParsedMessage::Request {
            id,
            method: method.to_owned(),
            params,
        });
    }

    None
}

// ─── 路由后消息（Node → Rust 的 Request / Notification）──────

/// 经 `handle_incoming` 路由后的消息：Response 被 pending map 消费后不会出现。
#[derive(Debug)]
pub enum IncomingMessage {
    Response {
        id: u64,
        result: Option<Value>,
        error: Option<JsonRpcError>,
    },
    Request {
        id: Option<u64>,
        method: String,
        params: Option<Value>,
    },
}

// ─── RpcChannel ────────────────────────────────────────────

use dashmap::{mapref::entry::Entry, DashMap};

/// Pending agent tracking entry for single-agent kill (GAP-07).
/// Holds the RPC id (to send error response to Node), cancel channel,
/// and a registration token for ownership-verified deregistration.
struct PendingAgent {
    rpc_id: Option<u64>,
    cancel_tx: tokio::sync::oneshot::Sender<()>,
    token: u64,
}

fn insert_pending_agent(
    pending_agents: &DashMap<(String, u64), PendingAgent>,
    key: (String, u64),
    pending: PendingAgent,
) -> bool {
    match pending_agents.entry(key) {
        Entry::Vacant(entry) => {
            entry.insert(pending);
            true
        }
        Entry::Occupied(_) => false,
    }
}

pub struct RpcChannel {
    stdin: Mutex<ChildStdin>,
    pending_requests: Arc<DashMap<u64, oneshot::Sender<Result<Value, JsonRpcError>>>>,
    /// Active workflow agents keyed by (run_id, agent_id) for single-agent kill (GAP-07).
    pending_agents: Arc<DashMap<(String, u64), PendingAgent>>,
    next_id: AtomicU64,
    /// 单调递增注册 token：deregister 时校验所有权，防止旧注册误删同 key 的新实例
    /// （kill 句柄被清空 → 后续 kill 漏杀）。
    agent_token: AtomicU64,
}

impl RpcChannel {
    pub fn new(stdin: ChildStdin) -> Self {
        Self {
            stdin: Mutex::new(stdin),
            pending_requests: Arc::new(DashMap::new()),
            pending_agents: Arc::new(DashMap::new()),
            next_id: AtomicU64::new(1),
            agent_token: AtomicU64::new(0),
        }
    }

    /// 分配 id → 插入 pending → 写入 stdin → 等待 oneshot。
    ///
    /// GAP-11 修复：先 insert pending 再 write_line，避免响应在 insert 前到达的竞态。
    pub async fn send_request(&self, method: &str, params: Value) -> Result<Value, WorkflowError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();

        let mut msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
        });
        if !params.is_null() {
            msg["params"] = params;
        }

        // GAP-11: insert BEFORE write to close the race window.
        self.pending_requests.insert(id, tx);
        if let Err(e) = self.write_line(&msg).await {
            self.pending_requests.remove(&id);
            return Err(e);
        }

        match rx.await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(err)) => Err(WorkflowError::Rpc(err.message)),
            Err(_) => Err(WorkflowError::Rpc("pending request cancelled".to_owned())),
        }
    }

    /// 通知（无 id，不期待响应）。
    pub async fn send_notification(
        &self,
        method: &str,
        params: Value,
    ) -> Result<(), WorkflowError> {
        let mut msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
        });
        if !params.is_null() {
            msg["params"] = params;
        }
        self.write_line(&msg).await
    }

    /// 响应 Node 的请求（如 agent/run 回调）。
    pub async fn send_response(&self, id: u64, result: Value) -> Result<(), WorkflowError> {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        });
        self.write_line(&msg).await
    }

    /// 错误响应（如 kill 被拒绝）。
    pub async fn send_error(&self, id: u64, code: i32, message: &str) -> Result<(), WorkflowError> {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": code,
                "message": message,
            }
        });
        self.write_line(&msg).await
    }

    /// 序列化 JSON + 追加 '\n' + 写入 + flush。
    async fn write_line(&self, value: &Value) -> Result<(), WorkflowError> {
        let mut line = serde_json::to_string(value).map_err(WorkflowError::Json)?;
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(WorkflowError::Io)?;
        stdin.flush().await.map_err(WorkflowError::Io)?;
        Ok(())
    }

    /// 排空所有 pending requests 并返回错误（Node 进程退出时调用）。
    ///
    /// 防止 `send_request()` 调用方永久挂起——当 stdout 关闭时，
    /// 所有未完成的请求（如 `workflow/start`）会收到 RpcError。
    pub fn drain_pending(&self, reason: &str) {
        // 收集所有 key，避免在迭代中修改
        let keys: Vec<u64> = self.pending_requests.iter().map(|e| *e.key()).collect();
        for key in keys {
            if let Some((_, tx)) = self.pending_requests.remove(&key) {
                let _ = tx.send(Err(JsonRpcError {
                    code: -32000,
                    message: reason.to_string(),
                    data: None,
                }));
            }
        }
    }

    /// 解析一行原始文本，路由 Response 到 pending sender。
    ///
    /// - Response 且匹配 pending → 消费，返回 None
    /// - Response 但无 pending → 转发为 IncomingMessage::Response
    /// - Request / Notification → 转发为 IncomingMessage::Request
    /// - 解析失败 → 返回 None
    pub fn handle_incoming(&self, raw: &str) -> Option<IncomingMessage> {
        let parsed = parse_message(raw)?;

        match parsed {
            ParsedMessage::Response { id, result, error } => {
                // 尝试匹配 pending request
                if let Some((_, tx)) = self.pending_requests.remove(&id) {
                    let res = if let Some(ref err) = error {
                        Err(err.clone())
                    } else {
                        Ok(result.unwrap_or(Value::Null))
                    };
                    let _ = tx.send(res); // 接收端可能已 drop
                    None
                } else {
                    // 无匹配 pending，转发
                    Some(IncomingMessage::Response { id, result, error })
                }
            }
            ParsedMessage::Request { id, method, params } => {
                Some(IncomingMessage::Request { id, method, params })
            }
        }
    }

    // ─── 单 agent kill 追踪（GAP-07）──────────────────────────

    /// 注册一个活跃 agent，返回 cancel receiver 与注册 token；同一 run 内重复的
    /// active agent ID 会被拒绝，避免覆盖已有任务的取消句柄。
    ///
    /// 必须在 `agent/run` 分支调用（spawn 之前）：kill_agent 与注册之间不再有
    /// 空窗（此前注册在 spawn 内，kill 先到会漏杀且返回 false）。
    /// spawn 的 task 持有 receiver，通过 `select!` 与 `exec.execute()` 竞速；
    /// `kill_agent()` 触发 cancel_tx → receiver 触发 → agent task 返回 Dead。
    /// `token` 用于 [`Self::deregister_agent`] 的所有权校验。
    pub fn register_agent(
        &self,
        run_id: &str,
        agent_id: u64,
        rpc_id: Option<u64>,
    ) -> Option<(oneshot::Receiver<()>, u64)> {
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let token = self.agent_token.fetch_add(1, Ordering::Relaxed);
        insert_pending_agent(
            &self.pending_agents,
            (run_id.to_string(), agent_id),
            PendingAgent {
                rpc_id,
                cancel_tx,
                token,
            },
        )
        .then_some((cancel_rx, token))
    }

    /// 正常完成后注销 agent（kill_agent 已移除时 no-op）。
    ///
    /// 仅当注册仍由本 task 持有（token 匹配、未被 kill_agent 取走）时才移除并
    /// 返回 `true`。返回 `false` 表示条目已被 kill_agent 移除——此时 error
    /// response 已由 kill 分支发送，本 task 不得再发送成功响应（防双重响应）。
    pub fn deregister_agent(&self, run_id: &str, agent_id: u64, token: u64) -> bool {
        let key = (run_id.to_string(), agent_id);
        // 注意：get 的 Ref 必须在使用后立即释放（is_some_and 消费临时值），
        // 不能在持有 Ref 的同时 remove —— DashMap 同 shard 先读后写会死锁
        // （写者优先 RwLock：写锁等待本线程持有的读锁）。
        let owned = self
            .pending_agents
            .get(&key)
            .is_some_and(|e| e.token == token);
        if owned {
            self.pending_agents.remove(&key);
            true
        } else {
            false
        }
    }

    /// 杀死指定 agent：向 Node 发送 error response + 触发 cancel。
    ///
    /// 返回 `true` 表示找到并杀死了 agent，`false` 表示 agent 不存在（已完成或未注册）。
    pub async fn kill_agent(&self, run_id: &str, agent_id: u64) -> bool {
        if let Some((_, agent)) = self.pending_agents.remove(&(run_id.to_string(), agent_id)) {
            // 向 Node 发送 error response（让 Node 的 agent/run Promise reject）
            if let Some(rpc_id) = agent.rpc_id {
                let _ = self
                    .send_error(rpc_id, -32000, "agent killed by user")
                    .await;
            }
            // 触发 cancel（让 Rust 侧的 agent task 停止执行）
            let _ = agent.cancel_tx.send(());
            true
        } else {
            false
        }
    }
}

// ─── stdout 读取器 ────────────────────────────────────────

use tokio::sync::mpsc;

/// 从 Node stdout 逐行读取 JSON-RPC 消息，经 `handle_incoming` 路由后
/// 将非 None 结果转发到 `sender`。
///
/// stdout 关闭时（Node 进程退出）排空所有 pending requests，
/// 防止 `send_request()` 调用方永久挂起。
pub fn spawn_stdout_reader(
    stdout: ChildStdout,
    channel: Arc<RpcChannel>,
    sender: mpsc::Sender<IncomingMessage>,
) {
    tokio::spawn(async move {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        let mut line_count: usize = 0;
        while let Ok(Some(line)) = lines.next_line().await {
            line_count += 1;
            if let Some(msg) = channel.handle_incoming(&line) {
                if sender.send(msg).await.is_err() {
                    break; // 接收端已关闭
                }
            }
        }
        tracing::info!(
            target: "workflow",
            total_lines = line_count,
            "stdout reader exited (stdout closed or read error)"
        );
        // stdout 关闭 → Node 进程已退出 → 排空 pending requests 防止挂起
        channel.drain_pending("node process exited");
    });
}

// ─── 测试 ──────────────────────────────────────────────────

#[cfg(test)]
#[path = "rpc_test.rs"]
mod tests;
