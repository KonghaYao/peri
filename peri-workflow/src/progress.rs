//! Reducer-based progress store: processes `ProgressEvent` and maintains UI-queryable state.
//!
//! Key design: agentId EXACT matching (not LIFO stack) — concurrent agents interleave
//! events, so `set_or_update_agent` finds by `agent_id` field, not the last element.

use std::collections::HashMap;

use indexmap::IndexMap;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::protocol::{AgentRunResult, ProgressEvent};

// ─── Data structures ───────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunProgress {
    pub run_id: String,
    pub workflow_name: String,
    pub status: RunStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<WorkflowMeta>,
    pub phases: Vec<PhaseProgress>,
    #[serde(with = "agents_as_map")]
    pub agents: IndexMap<u64, AgentProgress>,
    /// 完成时间戳（仅 server 侧用于清理过期 runs，不序列化到 JSON）。
    #[serde(skip)]
    pub completed_at: Option<std::time::Instant>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Completed,
    Failed,
    Killed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowMeta {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub phases: Vec<MetaPhase>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaPhase {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseProgress {
    pub title: String,
    pub status: PhaseStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseStatus {
    Pending,
    Active,
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProgress {
    pub agent_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    pub status: AgentStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<AgentRunResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Pending,
    Running,
    Done,
    Dead,
    Skipped,
}

// ─── Store ──────────────────────────────────────────────────

pub struct WorkflowProgressStore {
    runs: RwLock<HashMap<String, RunProgress>>,
}

impl WorkflowProgressStore {
    pub fn new() -> Self {
        Self {
            runs: RwLock::new(HashMap::new()),
        }
    }

    /// THE reducer: apply a `ProgressEvent` and update internal state.
    pub fn apply_event(&self, event: &ProgressEvent) {
        // Log 事件不修改 state，提前返回避免无意义持写锁阻塞所有并发读
        if matches!(event, ProgressEvent::Log { .. }) {
            return;
        }

        let run_id = event.run_id().to_string();
        let mut runs = self.runs.write();

        match event {
            ProgressEvent::RunStarted {
                workflow_name,
                meta: _,
                ..
            } => {
                let run = RunProgress {
                    run_id: run_id.clone(),
                    workflow_name: workflow_name.clone(),
                    status: RunStatus::Running,
                    meta: None, // meta is raw Value; conversion not required by spec
                    phases: Vec::new(),
                    agents: IndexMap::new(),
                    completed_at: None,
                };
                runs.insert(run_id, run);
            }
            ProgressEvent::PhaseStarted { phase, .. } => {
                if let Some(run) = runs.get_mut(&run_id) {
                    set_or_update_phase(&mut run.phases, phase, PhaseStatus::Active);
                }
            }
            ProgressEvent::PhaseDone { phase, .. } => {
                if let Some(run) = runs.get_mut(&run_id) {
                    set_or_update_phase(&mut run.phases, phase, PhaseStatus::Done);
                }
            }
            ProgressEvent::AgentStarted {
                agent_id,
                label,
                phase,
                ..
            } => {
                if let Some(run) = runs.get_mut(&run_id) {
                    set_or_update_agent(&mut run.agents, *agent_id, |agent| {
                        if label.is_some() {
                            agent.label = label.clone();
                        }
                        if phase.is_some() {
                            agent.phase = phase.clone();
                        }
                        agent.status = AgentStatus::Running;
                    });
                }
            }
            ProgressEvent::AgentProgress {
                agent_id,
                token_count,
                tool_count,
                ..
            } => {
                if let Some(run) = runs.get_mut(&run_id) {
                    set_or_update_agent(&mut run.agents, *agent_id, |agent| {
                        agent.token_count = Some(*token_count);
                        agent.tool_count = Some(*tool_count);
                        agent.status = AgentStatus::Running;
                    });
                }
            }
            ProgressEvent::AgentDone {
                agent_id, result, ..
            } => {
                if let Some(run) = runs.get_mut(&run_id) {
                    set_or_update_agent(&mut run.agents, *agent_id, |agent| {
                        agent.status = match result {
                            AgentRunResult::Ok { .. } => AgentStatus::Done,
                            AgentRunResult::Skipped => AgentStatus::Skipped,
                            AgentRunResult::Dead { .. } => AgentStatus::Dead,
                        };
                        agent.result = Some(result.clone());
                        // 只在 result 携带 tool_count 时才更新，保留 AgentProgress 已设的值
                        agent.tool_count = result.tool_count().or(agent.tool_count);
                    });
                }
            }
            ProgressEvent::RunDone { status, .. } => {
                if let Some(run) = runs.get_mut(&run_id) {
                    run.status = match status.as_str() {
                        "completed" => RunStatus::Completed,
                        "killed" => RunStatus::Killed,
                        _ => RunStatus::Failed,
                    };
                    run.completed_at = Some(std::time::Instant::now());
                }
            }
            // Log 事件已在函数入口处提前返回（不持写锁），此处 unreachable
            ProgressEvent::Log { .. } => unreachable!("Log event handled by early return"),
        }
    }

    pub fn get_run(&self, run_id: &str) -> Option<RunProgress> {
        self.runs.read().get(run_id).cloned()
    }

    pub fn list_runs(&self) -> Vec<RunProgress> {
        self.runs.read().values().cloned().collect()
    }

    /// 获取所有 runs 的快照（供 ACP handler 序列化用）。
    pub fn get_all_runs_snapshot(&self) -> Vec<RunProgress> {
        self.runs.read().values().cloned().collect()
    }

    pub fn active_runs(&self) -> Vec<RunProgress> {
        self.runs
            .read()
            .values()
            .filter(|r| matches!(r.status, RunStatus::Running))
            .cloned()
            .collect()
    }

    /// 完成状态的 runs 保留时间（5 分钟），过期后清理以释放内存。
    const COMPLETED_RETENTION: std::time::Duration = std::time::Duration::from_secs(300);

    pub fn cleanup_completed(&self) {
        let now = std::time::Instant::now();
        self.runs.write().retain(|_, r| {
            matches!(r.status, RunStatus::Running)
                || r.completed_at
                    .map(|at| now.duration_since(at) < Self::COMPLETED_RETENTION)
                    .unwrap_or(true)
        });
    }
}

impl Default for WorkflowProgressStore {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Helper functions ──────────────────────────────────────

/// Find phase by title (exact match) or push a new Pending one, then set status.
fn set_or_update_phase(phases: &mut Vec<PhaseProgress>, title: &str, status: PhaseStatus) {
    if let Some(p) = phases.iter_mut().find(|p| p.title == title) {
        p.status = status;
    } else {
        phases.push(PhaseProgress {
            title: title.to_string(),
            status,
        });
    }
}

/// Find agent by agent_id (O(1) lookup with IndexMap) or insert new, then apply `f`.
fn set_or_update_agent<F>(agents: &mut IndexMap<u64, AgentProgress>, agent_id: u64, f: F)
where
    F: FnOnce(&mut AgentProgress),
{
    agents.entry(agent_id).or_insert_with(|| AgentProgress {
        agent_id,
        label: None,
        phase: None,
        status: AgentStatus::Pending,
        token_count: None,
        tool_count: None,
        result: None,
    });
    f(agents.get_mut(&agent_id).unwrap());
}

// ─── Serde helper: serialize IndexMap<u64, AgentProgress> as JSON array ───

mod agents_as_map {
    use indexmap::IndexMap;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::AgentProgress;

    /// 将 IndexMap 序列化为 JSON 数组，保持与 Vec<AgentProgress> 相同的输出格式。
    pub fn serialize<S>(
        map: &IndexMap<u64, AgentProgress>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let values: Vec<&AgentProgress> = map.values().collect();
        values.serialize(serializer)
    }

    /// 将 JSON 数组反序列化为 IndexMap，以 agent_id 为 key。
    pub fn deserialize<'de, D>(deserializer: D) -> Result<IndexMap<u64, AgentProgress>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let vec: Vec<AgentProgress> = Vec::deserialize(deserializer)?;
        let map: IndexMap<u64, AgentProgress> = vec.into_iter().map(|a| (a.agent_id, a)).collect();
        Ok(map)
    }
}

impl WorkflowProgressStore {
    /// 获取 run 的统计数据，避免 clone 整个 RunProgress。
    pub fn get_run_stats(&self, run_id: &str) -> Option<(usize, usize)> {
        self.runs.read().get(run_id).map(|run| {
            let agent_count = run.agents.len();
            let tool_calls_count = run
                .agents
                .values()
                .filter_map(|a| {
                    a.tool_count
                        .or_else(|| a.result.as_ref().and_then(|r| r.tool_count()))
                })
                .sum::<u64>() as usize;
            (agent_count, tool_calls_count)
        })
    }
}

// ─── Tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{AgentRunResult, ProgressEvent, Usage};

    fn make_store() -> WorkflowProgressStore {
        WorkflowProgressStore::new()
    }

    #[test]
    fn test_run_started_creates_run() {
        let store = make_store();
        store.apply_event(&ProgressEvent::RunStarted {
            run_id: "r1".into(),
            workflow_name: "test".into(),
            meta: None,
        });
        let run = store.get_run("r1").expect("run 应存在");
        assert_eq!(run.run_id, "r1");
        assert_eq!(run.workflow_name, "test");
        assert!(matches!(run.status, RunStatus::Running));
        assert!(run.agents.is_empty());
        assert!(run.phases.is_empty());
    }

    #[test]
    fn test_agent_lifecycle_started_progress_done() {
        let store = make_store();
        // 启动 run
        store.apply_event(&ProgressEvent::RunStarted {
            run_id: "r1".into(),
            workflow_name: "test".into(),
            meta: None,
        });
        // agent 启动
        store.apply_event(&ProgressEvent::AgentStarted {
            run_id: "r1".into(),
            agent_id: 0,
            label: Some("review".into()),
            phase: Some("Review".into()),
        });
        // agent 进度
        store.apply_event(&ProgressEvent::AgentProgress {
            run_id: "r1".into(),
            agent_id: 0,
            label: None,
            phase: None,
            token_count: 100,
            tool_count: 2,
        });
        // agent 完成
        store.apply_event(&ProgressEvent::AgentDone {
            run_id: "r1".into(),
            agent_id: 0,
            label: None,
            phase: None,
            result: AgentRunResult::Ok {
                output: serde_json::json!("done"),
                usage: Usage { output_tokens: 50 },
                model: None,
                tool_count: None,
                token_count: None,
            },
        });
        let run = store.get_run("r1").expect("run 应存在");
        assert_eq!(run.agents.len(), 1);
        let agent = run.agents.get(&0).expect("agent 0 应存在");
        assert_eq!(agent.agent_id, 0);
        assert_eq!(agent.label.as_deref(), Some("review"));
        assert_eq!(agent.phase.as_deref(), Some("Review"));
        assert!(matches!(agent.status, AgentStatus::Done));
        assert_eq!(agent.token_count, Some(100));
        assert_eq!(agent.tool_count, Some(2));
        assert!(agent.result.is_some());
    }

    #[test]
    fn test_concurrent_agents_no_race() {
        let store = make_store();
        // 启动 run
        store.apply_event(&ProgressEvent::RunStarted {
            run_id: "r1".into(),
            workflow_name: "test".into(),
            meta: None,
        });
        // agent 0 启动
        store.apply_event(&ProgressEvent::AgentStarted {
            run_id: "r1".into(),
            agent_id: 0,
            label: Some("coder".into()),
            phase: Some("Implement".into()),
        });
        // agent 1 启动（并发）
        store.apply_event(&ProgressEvent::AgentStarted {
            run_id: "r1".into(),
            agent_id: 1,
            label: Some("reviewer".into()),
            phase: Some("Review".into()),
        });
        // agent 0 进度（交错事件）
        store.apply_event(&ProgressEvent::AgentProgress {
            run_id: "r1".into(),
            agent_id: 0,
            label: None,
            phase: None,
            token_count: 200,
            tool_count: 5,
        });
        // agent 1 进度
        store.apply_event(&ProgressEvent::AgentProgress {
            run_id: "r1".into(),
            agent_id: 1,
            label: None,
            phase: None,
            token_count: 50,
            tool_count: 1,
        });
        // agent 1 完成（先完成）
        store.apply_event(&ProgressEvent::AgentDone {
            run_id: "r1".into(),
            agent_id: 1,
            label: None,
            phase: None,
            result: AgentRunResult::Ok {
                output: serde_json::json!("approved"),
                usage: Usage { output_tokens: 30 },
                model: None,
                tool_count: None,
                token_count: None,
            },
        });
        // agent 0 完成
        store.apply_event(&ProgressEvent::AgentDone {
            run_id: "r1".into(),
            agent_id: 0,
            label: None,
            phase: None,
            result: AgentRunResult::Ok {
                output: serde_json::json!("implemented"),
                usage: Usage { output_tokens: 100 },
                model: None,
                tool_count: None,
                token_count: None,
            },
        });

        let run = store.get_run("r1").expect("run 应存在");
        assert_eq!(run.agents.len(), 2);

        // agent 0：验证精确匹配，不被 LIFO 搞乱
        let agent0 = run.agents.get(&0).expect("agent 0 应存在");
        assert_eq!(agent0.label.as_deref(), Some("coder"));
        assert_eq!(agent0.token_count, Some(200));
        assert_eq!(agent0.tool_count, Some(5));
        assert!(matches!(agent0.status, AgentStatus::Done));

        // agent 1
        let agent1 = run.agents.get(&1).expect("agent 1 应存在");
        assert_eq!(agent1.label.as_deref(), Some("reviewer"));
        assert_eq!(agent1.token_count, Some(50));
        assert_eq!(agent1.tool_count, Some(1));
        assert!(matches!(agent1.status, AgentStatus::Done));
    }

    #[test]
    fn test_run_done_updates_status() {
        let store = make_store();
        store.apply_event(&ProgressEvent::RunStarted {
            run_id: "r1".into(),
            workflow_name: "test".into(),
            meta: None,
        });
        assert!(matches!(
            store.get_run("r1").unwrap().status,
            RunStatus::Running
        ));

        store.apply_event(&ProgressEvent::RunDone {
            run_id: "r1".into(),
            status: "completed".into(),
            return_value: None,
            error: None,
        });
        assert!(matches!(
            store.get_run("r1").unwrap().status,
            RunStatus::Completed
        ));
    }

    #[test]
    fn test_cleanup_completed_keeps_running_runs() {
        let store = make_store();
        store.apply_event(&ProgressEvent::RunStarted {
            run_id: "r1".into(),
            workflow_name: "test".into(),
            meta: None,
        });
        store.cleanup_completed();
        // Running 状态的 run 始终保留
        assert!(store.get_run("r1").is_some());
    }

    #[test]
    fn test_cleanup_completed_keeps_recently_completed_runs() {
        let store = make_store();
        store.apply_event(&ProgressEvent::RunStarted {
            run_id: "r1".into(),
            workflow_name: "test".into(),
            meta: None,
        });
        // 刚完成 → completed_at 为当前时间，应在保留期内
        store.apply_event(&ProgressEvent::RunDone {
            run_id: "r1".into(),
            status: "completed".into(),
            return_value: None,
            error: None,
        });
        store.cleanup_completed();
        // 刚完成的 run 不应被清理（completed_at 在 5 分钟保留期内）
        assert!(store.get_run("r1").is_some());
    }
}
