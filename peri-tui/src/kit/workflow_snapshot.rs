//! Workflow snapshot DTO types and background polling task.
//!
//! Polls `workflow/list_runs` every 2 seconds and writes the result to the
//! `WORKFLOW_SNAPSHOT` atom, which the `WorkflowPanel` component subscribes to.
//!
//! The DTO types mirror `peri_workflow::progress::RunProgress` but avoid
//! a direct crate dependency. The `agents` field uses a JSON array format
//! (the server's `agents_as_map` serializer converts `IndexMap` to/from Vec).

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::acp_client::client::AcpTuiClient;
use crate::kit::atoms::WORKFLOW_SNAPSHOT;

// ── DTO types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct WorkflowSnapshot {
    #[serde(default)]
    pub runs: Vec<TuiRunProgress>,
}

/// Mirrors `peri_workflow::progress::RunProgress`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TuiRunProgress {
    pub run_id: String,
    pub workflow_name: String,
    pub status: String, // "running" | "completed" | "failed" | "killed"
    #[serde(default)]
    pub phases: Vec<TuiPhaseProgress>,
    /// Server serializes IndexMap<u64, AgentProgress> as JSON array.
    #[serde(default)]
    pub agents: Vec<TuiAgentProgress>,
}

/// Mirrors `peri_workflow::progress::PhaseProgress`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TuiPhaseProgress {
    pub title: String,
    pub status: String, // "pending" | "active" | "done"
}

/// Mirrors `peri_workflow::progress::AgentProgress`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TuiAgentProgress {
    pub agent_id: u64,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub phase: Option<String>,
    /// 有效/解析后的模型名（旧版快照无此字段 → None，面板渲染 '-'）。
    #[serde(default)]
    pub model: Option<String>,
    /// 请求的模型档位（alias，如 sonnet/haiku；alias 解析成功才有值）。
    /// 面板优先显示档位，缺失时回退 `model`。
    #[serde(default)]
    pub model_tier: Option<String>,
    pub status: String, // "pending" | "running" | "done" | "dead" | "skipped"
    #[serde(default)]
    pub token_count: Option<u64>,
    #[serde(default)]
    pub tool_count: Option<u64>,
}

// ── Polling task ───────────────────────────────────────────────────────────

/// Spawn a background task that polls `workflow/list_runs` every 2 seconds.
///
/// Writes the deserialized `WorkflowSnapshot` into `WORKFLOW_SNAPSHOT` atom.
/// When no session is active, writes an empty snapshot so the WorkflowPanel
/// transitions from loading → empty state.
pub fn spawn_workflow_poll(
    client: Arc<AcpTuiClient>,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    break;
                }
                _ = interval.tick() => {
                    let session_id = client.current_session_id();
                    let snapshot = match session_id {
                        Some(sid) => {
                            match client.send_raw_request(
                                "workflow/list_runs",
                                json!({ "sessionId": sid }),
                            ).await {
                                Ok(value) => {
                                    match serde_json::from_value::<WorkflowSnapshot>(value) {
                                        Ok(snap) => snap,
                                        Err(e) => {
                                            warn!(error = %e, "workflow poll: deserialization failed");
                                            WorkflowSnapshot { runs: vec![] }
                                        }
                                    }
                                }
                                Err(e) => {
                                    warn!(error = %e, "workflow poll: RPC error");
                                    WorkflowSnapshot { runs: vec![] }
                                }
                            }
                        }
                        None => WorkflowSnapshot { runs: vec![] },
                    };
                    *WORKFLOW_SNAPSHOT.state().write() = Some(snapshot);
                }
            }
        }
    })
}
