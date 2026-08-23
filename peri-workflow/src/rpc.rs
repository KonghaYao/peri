//! Workflow Adapter 的 RPC 扩展。
//!
//! 通用 NDJSON、pending request 与 process I/O 由 `peri-js-runtime` 承担；本模块仅
//! 保留 Workflow 特有的 active agent ownership/kill 语义。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::{mapref::entry::Entry, DashMap};
use serde_json::Value;
use tokio::sync::oneshot;

use crate::error::WorkflowError;

pub use peri_js_runtime::{parse_message, IncomingMessage, ParsedMessage};

struct PendingAgent {
    rpc_id: Option<u64>,
    cancel_tx: oneshot::Sender<()>,
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
    transport: Arc<peri_js_runtime::RpcChannel>,
    pending_agents: Arc<DashMap<(String, u64), PendingAgent>>,
    agent_token: AtomicU64,
}

impl RpcChannel {
    pub fn new(transport: Arc<peri_js_runtime::RpcChannel>) -> Self {
        Self {
            transport,
            pending_agents: Arc::new(DashMap::new()),
            agent_token: AtomicU64::new(0),
        }
    }

    pub async fn send_request(&self, method: &str, params: Value) -> Result<Value, WorkflowError> {
        self.transport
            .send_request(method, params)
            .await
            .map_err(WorkflowError::from)
    }

    pub async fn send_notification(
        &self,
        method: &str,
        params: Value,
    ) -> Result<(), WorkflowError> {
        self.transport
            .send_notification(method, params)
            .await
            .map_err(WorkflowError::from)
    }

    pub async fn send_response(&self, id: u64, result: Value) -> Result<(), WorkflowError> {
        self.transport
            .send_response(id, result)
            .await
            .map_err(WorkflowError::from)
    }

    pub async fn send_error(&self, id: u64, code: i32, message: &str) -> Result<(), WorkflowError> {
        self.transport
            .send_error(id, code, message, None)
            .await
            .map_err(WorkflowError::from)
    }

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

    pub fn deregister_agent(&self, run_id: &str, agent_id: u64, token: u64) -> bool {
        let key = (run_id.to_string(), agent_id);
        let owned = self
            .pending_agents
            .get(&key)
            .is_some_and(|entry| entry.token == token);
        if owned {
            self.pending_agents.remove(&key);
        }
        owned
    }

    pub async fn kill_agent(&self, run_id: &str, agent_id: u64) -> bool {
        if let Some((_, agent)) = self.pending_agents.remove(&(run_id.to_string(), agent_id)) {
            if let Some(rpc_id) = agent.rpc_id {
                let _ = self
                    .send_error(rpc_id, -32000, "agent killed by user")
                    .await;
            }
            let _ = agent.cancel_tx.send(());
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
#[path = "rpc_test.rs"]
mod tests;
