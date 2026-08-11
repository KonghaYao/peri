//! Reducer-based progress store: processes `ProgressEvent` and maintains UI-queryable state.
//!
//! Key design: agentId EXACT matching (not LIFO stack) — concurrent agents interleave
//! events, so `set_or_update_agent` finds by `agent_id` field, not the last element.

use std::collections::{HashMap, HashSet};

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

// 3.0 批 2 波 1：协议投影类型迁入契约层 `peri_acp_types::workflow`
// （`RunStatus` / `WorkflowMeta` / `MetaPhase` / `PhaseProgress` /
// `PhaseStatus` / `AgentProgress` / `AgentStatus` / `PhaseSummary`）；
// 本模块保留 re-export 保兼容。`RunProgress`（含 `IndexMap` 字段）保留在本文件。
pub use peri_acp_types::workflow::{
    AgentProgress, AgentStatus, MetaPhase, PhaseProgress, PhaseStatus, PhaseSummary, RunStatus,
    WorkflowMeta,
};

// ─── Store ──────────────────────────────────────────────────

pub struct WorkflowProgressStore {
    runs: RwLock<HashMap<String, RunProgress>>,
    /// AgentStarted 仅由本次真实执行产生；resume cache-hit 只会收到 AgentDone。
    started_agents: RwLock<HashSet<(String, u64)>>,
}

impl WorkflowProgressStore {
    pub fn new() -> Self {
        Self {
            runs: RwLock::new(HashMap::new()),
            started_agents: RwLock::new(HashSet::new()),
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
            ProgressEvent::RunStarted { workflow_name, .. } => {
                self.started_agents
                    .write()
                    .retain(|(started_run_id, _)| started_run_id != &run_id);
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
                self.started_agents
                    .write()
                    .insert((run_id.clone(), *agent_id));
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
                model,
                model_tier,
                token_count,
                tool_count,
                ..
            } => {
                if let Some(run) = runs.get_mut(&run_id) {
                    set_or_update_agent(&mut run.agents, *agent_id, |agent| {
                        // model 仅在 Some 时更新：运行中由 agent 侧 0 token/0 tool
                        // 的专用更新携带，后续不带 model 的进度事件不得覆盖。
                        if model.is_some() {
                            agent.model = model.clone();
                        }
                        // model_tier 同理（仅在 Some 时更新，保留已设的档位）。
                        if model_tier.is_some() {
                            agent.model_tier = model_tier.clone();
                        }
                        if let Some(token_count) = token_count {
                            agent.token_count = Some(*token_count);
                        }
                        if let Some(tool_count) = tool_count {
                            agent.tool_count = Some(*tool_count);
                        }
                        agent.status = AgentStatus::Running;
                    });
                }
            }
            ProgressEvent::AgentDone {
                agent_id,
                phase,
                result,
                ..
            } => {
                let executed_in_this_run = self
                    .started_agents
                    .read()
                    .contains(&(run_id.clone(), *agent_id));
                if let Some(run) = runs.get_mut(&run_id) {
                    set_or_update_agent(&mut run.agents, *agent_id, |agent| {
                        agent.status = match result {
                            AgentRunResult::Ok { .. } => AgentStatus::Done,
                            AgentRunResult::Skipped => AgentStatus::Skipped,
                            AgentRunResult::Dead { .. } => AgentStatus::Dead,
                        };
                        agent.result = Some(result.clone());
                        if agent.phase.is_none() {
                            agent.phase = phase.clone().or_else(|| match result {
                                AgentRunResult::Ok { phase, .. } => phase.clone(),
                                _ => None,
                            });
                        }
                        if executed_in_this_run {
                            agent.duration_ms = match result {
                                AgentRunResult::Ok { duration_ms, .. } => *duration_ms,
                                _ => None,
                            };
                            agent.tool_count = result.tool_count().or(agent.tool_count);
                            agent.token_count = result.token_count().or(agent.token_count);
                        }
                        // 完成快照模型名以 AgentRunResult::Ok.model 为准
                        // （仅在 Some 时更新，保留 AgentProgress 已设的值）
                        if let AgentRunResult::Ok { model: Some(m), .. } = result {
                            agent.model = Some(m.clone());
                        }
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
        // 已清理 run 的执行标记一并释放（P2-2026-08-11：否则随
        // 每个完成 workflow 的 agent marker 永久残留，长期运行无界增长）。
        self.started_agents.write().retain(|(run_id, _)| {
            self.runs
                .read()
                .get(run_id)
                .is_some_and(|r| matches!(r.status, RunStatus::Running))
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
        model: None,
        model_tier: None,
        status: AgentStatus::Pending,
        token_count: None,
        tool_count: None,
        duration_ms: None,
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

    /// 获取按 phase 分组的统计摘要（供通知格式化）。
    pub fn get_phase_summaries(&self, run_id: &str) -> Vec<PhaseSummary> {
        let runs = self.runs.read();
        let Some(run) = runs.get(run_id) else {
            return Vec::new();
        };
        let started_agents = self.started_agents.read();
        let mut phase_map: std::collections::HashMap<String, (usize, u64, u64)> =
            std::collections::HashMap::new();
        for agent in run.agents.values() {
            let phase = agent.phase.as_deref().unwrap_or(run.workflow_name.as_str());
            let entry = phase_map.entry(phase.to_string()).or_insert((0, 0, 0));
            entry.0 += 1;
            if started_agents.contains(&(run_id.to_string(), agent.agent_id)) {
                entry.1 += agent
                    .token_count
                    .or_else(|| agent.result.as_ref().and_then(|r| r.token_count()))
                    .unwrap_or(0);
                entry.2 += agent
                    .duration_ms
                    .or(
                        if let Some(AgentRunResult::Ok {
                            duration_ms: Some(d),
                            ..
                        }) = &agent.result
                        {
                            Some(*d)
                        } else {
                            None
                        },
                    )
                    .unwrap_or(0);
            }
        }
        phase_map
            .into_iter()
            .map(
                |(name, (agent_count, token_count, duration_sum))| PhaseSummary {
                    name,
                    agent_count,
                    token_count,
                    duration_ms: if duration_sum > 0 {
                        Some(duration_sum)
                    } else {
                        None
                    },
                },
            )
            .collect()
    }

    /// 获取指定 agent 的 phase（供 journal 注入）。
    pub fn get_agent_phase(&self, run_id: &str, agent_id: u64) -> Option<String> {
        let runs = self.runs.read();
        let run = runs.get(run_id)?;
        let agent = run.agents.get(&agent_id)?;
        agent.phase.clone()
    }
}

// ─── Tests ──────────────────────────────────────────────────

#[cfg(test)]
#[path = "progress_test.rs"]
mod tests;
