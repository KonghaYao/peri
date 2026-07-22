//! Session 路由器：维护 session_id → 子进程 映射表
//!
//! SessionRouter 负责：
//! - session/new 时 spawn 子进程并注册映射
//! - session/close 时通知子进程并清理
//! - 根据 session_id 将请求/通知路由到正确的子进程

use crate::child::{spawn_child, ChildHandle};
use crate::error::{error_response, ok_response, SESSION_NOT_FOUND, SPAWN_FAILED};
use serde_json::Value;
use std::collections::HashMap;
use tokio::sync::mpsc;

/// Session 信息
pub struct SessionInfo {
    pub session_id: String,
    pub cwd: String,
    pub title: Option<String>,
    pub updated_at: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub status: SessionStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionStatus {
    Ready,
    Crashed,
}

/// 从子进程发往 Hub 主循环的事件
pub enum RouterEvent {
    /// 子进程消息：(session_id, json_value)
    ChildMessage(String, Value),
}

/// 子进程条目（包含句柄和元信息）
struct ChildEntry {
    handle: ChildHandle,
    cwd: String,
    created_at: chrono::DateTime<chrono::Utc>,
}

/// Session 路由器
pub struct SessionRouter {
    /// 子进程启动命令（不含 --）
    child_cmd: Vec<String>,
    /// session_id → ChildEntry
    children: HashMap<String, ChildEntry>,
    /// 聚合所有子进程通知的 sender
    child_msg_tx: mpsc::UnboundedSender<RouterEvent>,
    /// spawn 超时（秒）
    spawn_timeout: u64,
    /// 请求超时（秒）
    child_timeout: u64,
    /// IDE 发来的 initialize params（缓存后透传给子进程）
    client_init_params: Option<serde_json::Value>,
}

impl SessionRouter {
    pub fn new(
        child_cmd: Vec<String>,
        child_msg_tx: mpsc::UnboundedSender<RouterEvent>,
        spawn_timeout: u64,
        child_timeout: u64,
    ) -> Self {
        Self {
            child_cmd,
            children: HashMap::new(),
            child_msg_tx,
            spawn_timeout,
            child_timeout,
            client_init_params: None,
        }
    }

    /// 缓存 IDE 发来的 initialize params，后续创建子进程时透传
    pub fn set_client_init_params(&mut self, params: serde_json::Value) {
        self.client_init_params = Some(params);
    }

    /// 透传 IDE 缓存的 initialize params（无缓存时为空对象）
    fn init_params_for_child(&self) -> serde_json::Value {
        self.client_init_params
            .clone()
            .unwrap_or(serde_json::json!({}))
    }

    /// 创建新 session：spawn 子进程 → initialize → session/new
    pub async fn create_session(&mut self, ide_req_id: &Value, params: &Value) -> Value {
        let cwd = params.get("cwd").and_then(|v| v.as_str()).unwrap_or(".");

        // 1. spawn 子进程
        let temp_sid = uuid::Uuid::new_v4().to_string();
        let (child, child_rx) = match spawn_child(&self.child_cmd, cwd, &temp_sid).await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::error!(target: "acp_hub::router", "spawn 子进程失败: {}", e);
                return error_response(Some(ide_req_id), SPAWN_FAILED, &e.to_string());
            }
        };

        // 启动后台任务：将子进程通知汇聚到 Hub 主循环
        let session_id_for_task = temp_sid.clone();
        let tx = self.child_msg_tx.clone();
        tokio::spawn(async move {
            let mut rx = child_rx;
            while let Some(msg) = rx.recv().await {
                if tx
                    .send(RouterEvent::ChildMessage(session_id_for_task.clone(), msg))
                    .is_err()
                {
                    break;
                }
            }
        });

        // 2. initialize 子进程（透传 IDE 的 initialize params）
        if let Err(e) = child
            .send_request(
                "initialize",
                &self.init_params_for_child(),
                self.spawn_timeout,
            )
            .await
        {
            tracing::error!(target: "acp_hub::router", "子进程 initialize 失败: {}", e);
            let _ = child.kill().await;
            return error_response(
                Some(ide_req_id),
                SPAWN_FAILED,
                &format!("子进程 initialize 失败: {e}"),
            );
        }

        // 3. session/new 子进程：直接透传 IDE params
        let resp = match child
            .send_request("session/new", params, self.spawn_timeout)
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                tracing::error!(target: "acp_hub::router", "子进程 session/new 失败: {}", e);
                let _ = child.kill().await;
                return error_response(
                    Some(ide_req_id),
                    SPAWN_FAILED,
                    &format!("子进程 session/new 失败: {e}"),
                );
            }
        };

        // 4. 提取子进程返回的 sessionId（camelCase，符合 ACP 规范）
        let session_id = resp
            .get("result")
            .and_then(|r| r.get("sessionId"))
            .and_then(|v| v.as_str())
            .unwrap_or(&temp_sid)
            .to_string();

        // 5. 注册映射
        self.children.insert(
            session_id.clone(),
            ChildEntry {
                handle: child,
                cwd: cwd.to_string(),
                created_at: chrono::Utc::now(),
            },
        );

        tracing::info!(target: "acp_hub::router", session_id, "session 创建完成");

        ok_response(ide_req_id, resp["result"].clone())
    }

    /// 关闭 session：通知子进程 → kill
    pub async fn close_session(&mut self, ide_req_id: &Value, session_id: &str) -> Value {
        match self.children.remove(session_id) {
            Some(entry) => {
                let child = entry.handle;
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    child.send_notification(
                        "session/close",
                        &serde_json::json!({"sessionId": session_id}),
                    ),
                )
                .await;
                let _ = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await;
                let _ = child.kill().await;

                tracing::info!(target: "acp_hub::router", session_id, "session 已关闭");
                ok_response(ide_req_id, serde_json::json!({"closed": true}))
            }
            None => error_response(Some(ide_req_id), SESSION_NOT_FOUND, "session not found"),
        }
    }

    /// 将请求转发到指定 session 的子进程
    pub async fn forward_request(
        &self,
        ide_req_id: &Value,
        session_id: &str,
        method: &str,
        params: &Value,
    ) -> Value {
        match self.children.get(session_id) {
            Some(entry) => match entry
                .handle
                .send_request(method, params, self.child_timeout)
                .await
            {
                Ok(resp) => {
                    let result = resp.get("result").cloned().unwrap_or(Value::Null);
                    ok_response(ide_req_id, result)
                }
                Err(e) => {
                    tracing::error!(target: "acp_hub::router", session_id, "子进程请求失败: {}", e);
                    error_response(
                        Some(ide_req_id),
                        crate::error::CHILD_TIMEOUT,
                        &e.to_string(),
                    )
                }
            },
            None => error_response(Some(ide_req_id), SESSION_NOT_FOUND, "session not found"),
        }
    }

    /// 将通知转发到指定 session 的子进程
    pub async fn forward_notification(&self, session_id: &str, method: &str, params: &Value) {
        if let Some(entry) = self.children.get(session_id) {
            if let Err(e) = entry.handle.send_notification(method, params).await {
                tracing::warn!(target: "acp_hub::router", session_id, "转发通知到子进程失败: {}", e);
            }
        }
    }

    /// 列出所有活跃 session
    pub fn list_sessions(&self) -> Vec<SessionInfo> {
        self.children
            .iter()
            .map(|(sid, entry)| SessionInfo {
                session_id: sid.clone(),
                cwd: entry.cwd.clone(),
                title: None,
                updated_at: None,
                created_at: entry.created_at,
                status: SessionStatus::Ready,
            })
            .collect()
    }

    /// 检查子进程是否仍然存活
    pub fn has_session(&self, session_id: &str) -> bool {
        self.children.contains_key(session_id)
    }

    /// 标记 session 为崩溃状态，清理映射
    pub fn mark_crashed(&mut self, session_id: &str) {
        self.children.remove(session_id);
        tracing::warn!(target: "acp_hub::router", session_id, "session 已崩溃，已清理映射");
    }
}

#[cfg(test)]
#[path = "router_test.rs"]
mod tests;
