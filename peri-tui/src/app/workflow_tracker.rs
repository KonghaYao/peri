//! WorkflowProgressTracker — TUI 侧 workflow 事件累积器。
//!
//! 接收 AgentEvent::WorkflowProgress(WorkflowProgressPayload)，
//! 更新内存中的 WorkflowRunSnapshot 列表，供 WorkflowPanel 渲染。

use std::collections::HashMap;

use peri_acp::event::WorkflowProgressDto;

/// Workflow agent 快照（单个 agent 的状态）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkflowAgentSnapshot {
    pub agent_id: u64,
    pub label: Option<String>,
    pub phase: Option<String>,
    pub status: String,
    pub token_count: Option<u64>,
    pub tool_count: Option<u64>,
}

/// Workflow phase 快照（单个 phase 的状态）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkflowPhaseSnapshot {
    pub title: String,
    pub status: String,
}

/// Workflow run 快照（一个 workflow 运行的整体状态）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkflowRunSnapshot {
    pub run_id: String,
    pub workflow_name: String,
    pub status: String,
    pub phases: Vec<WorkflowPhaseSnapshot>,
    pub agents: Vec<WorkflowAgentSnapshot>,
}

/// TUI 侧 workflow 进度追踪器。
pub struct WorkflowProgressTracker {
    runs: HashMap<String, WorkflowRunSnapshot>,
}

impl WorkflowProgressTracker {
    pub fn new() -> Self {
        Self {
            runs: HashMap::new(),
        }
    }

    /// 应用一个 WorkflowProgressDto 事件，更新内部快照。
    pub fn apply(&mut self, payload: &WorkflowProgressDto) {
        let run = self
            .runs
            .entry(payload.run_id.clone())
            .or_insert_with(|| WorkflowRunSnapshot {
                run_id: payload.run_id.clone(),
                workflow_name: payload.workflow_name.clone(),
                status: "running".to_string(),
                phases: Vec::new(),
                agents: Vec::new(),
            });

        run.workflow_name = payload.workflow_name.clone();

        match payload.event_type.as_str() {
            "run_started" => {
                run.status = "running".to_string();
            }
            "run_done" => {
                run.status = payload
                    .run_status
                    .clone()
                    .unwrap_or_else(|| "completed".to_string());
            }
            "phase_started" | "phase_done" => {
                if let Some(ref phase_name) = payload.phase {
                    let status = if payload.event_type == "phase_done" {
                        "done"
                    } else {
                        "running"
                    };
                    if let Some(existing) = run.phases.iter_mut().find(|p| &p.title == phase_name) {
                        existing.status = status.to_string();
                    } else {
                        run.phases.push(WorkflowPhaseSnapshot {
                            title: phase_name.clone(),
                            status: status.to_string(),
                        });
                    }
                }
            }
            "agent_started" | "agent_progress" | "agent_done" => {
                if let Some(agent_id) = payload.agent_id {
                    let status = payload.agent_status.clone().unwrap_or_else(|| {
                        if payload.event_type == "agent_done" {
                            "done"
                        } else {
                            "running"
                        }
                        .to_string()
                    });
                    if let Some(existing) = run.agents.iter_mut().find(|a| a.agent_id == agent_id) {
                        existing.status = status;
                        if let Some(ref label) = payload.label {
                            existing.label = Some(label.clone());
                        }
                        if let Some(ref phase) = payload.phase {
                            existing.phase = Some(phase.clone());
                        }
                        if let Some(tc) = payload.token_count {
                            existing.token_count = Some(tc);
                        }
                        if let Some(tc) = payload.tool_count {
                            existing.tool_count = Some(tc);
                        }
                    } else {
                        run.agents.push(WorkflowAgentSnapshot {
                            agent_id,
                            label: payload.label.clone(),
                            phase: payload.phase.clone(),
                            status,
                            token_count: payload.token_count,
                            tool_count: payload.tool_count,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    /// 获取所有 run 的快照（活跃优先，按 run_id 排序）。
    pub fn snapshots(&self) -> Vec<WorkflowRunSnapshot> {
        let mut runs: Vec<_> = self.runs.values().cloned().collect();
        runs.sort_by(|a, b| {
            let a_active = a.status == "running";
            let b_active = b.status == "running";
            b_active
                .cmp(&a_active)
                .then_with(|| a.run_id.cmp(&b.run_id))
        });
        runs
    }

    /// 用 server 端 progress_store 拉取的全量快照直接替换本地数据。
    /// 拉模型下：push 路径已删，apply() 不会被调用，所以 polling 必须用此方法注入。
    pub fn replace_runs(&mut self, runs: Vec<WorkflowRunSnapshot>) {
        self.runs.clear();
        for run in runs {
            self.runs.insert(run.run_id.clone(), run);
        }
    }

    /// 清空所有数据。
    pub fn clear(&mut self) {
        self.runs.clear();
    }
}

impl Default for WorkflowProgressTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_payload(run_id: &str, event_type: &str) -> WorkflowProgressDto {
        WorkflowProgressDto {
            run_id: run_id.to_string(),
            workflow_name: "test-workflow".to_string(),
            event_type: event_type.to_string(),
            agent_id: None,
            phase: None,
            label: None,
            agent_status: None,
            token_count: None,
            tool_count: None,
            run_status: None,
            message: None,
        }
    }

    #[test]
    fn test_apply_run_started() {
        let mut tracker = WorkflowProgressTracker::new();
        let payload = make_payload("run-1", "run_started");
        tracker.apply(&payload);
        let snaps = tracker.snapshots();
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].run_id, "run-1");
        assert_eq!(snaps[0].status, "running");
    }

    #[test]
    fn test_apply_run_done() {
        let mut tracker = WorkflowProgressTracker::new();
        let payload = make_payload("run-1", "run_started");
        tracker.apply(&payload);
        let mut done_payload = make_payload("run-1", "run_done");
        done_payload.run_status = Some("completed".to_string());
        tracker.apply(&done_payload);
        let snaps = tracker.snapshots();
        assert_eq!(snaps[0].status, "completed");
    }

    #[test]
    fn test_snapshots_sort_active_first() {
        let mut tracker = WorkflowProgressTracker::new();
        // 添加一个完成的 run
        let mut done_p = make_payload("run-a", "run_done");
        done_p.run_status = Some("completed".to_string());
        tracker.apply(&done_p);
        // 添加一个活跃的 run
        tracker.apply(&make_payload("run-b", "run_started"));
        let snaps = tracker.snapshots();
        // 活跃的在前
        assert_eq!(snaps[0].run_id, "run-b");
        assert_eq!(snaps[1].run_id, "run-a");
    }

    #[test]
    fn test_phase_tracking() {
        let mut tracker = WorkflowProgressTracker::new();
        tracker.apply(&make_payload("run-1", "run_started"));
        let mut phase_p = make_payload("run-1", "phase_started");
        phase_p.phase = Some("Phase 1".to_string());
        tracker.apply(&phase_p);
        let mut done_p = make_payload("run-1", "phase_done");
        done_p.phase = Some("Phase 1".to_string());
        tracker.apply(&done_p);
        let snaps = tracker.snapshots();
        assert_eq!(snaps[0].phases.len(), 1);
        assert_eq!(snaps[0].phases[0].status, "done");
    }

    #[test]
    fn test_agent_tracking() {
        let mut tracker = WorkflowProgressTracker::new();
        tracker.apply(&make_payload("run-1", "run_started"));
        let mut agent_p = make_payload("run-1", "agent_started");
        agent_p.agent_id = Some(1);
        agent_p.label = Some("coder".to_string());
        agent_p.phase = Some("Phase 1".to_string());
        tracker.apply(&agent_p);
        let mut progress_p = make_payload("run-1", "agent_progress");
        progress_p.agent_id = Some(1);
        progress_p.token_count = Some(500);
        tracker.apply(&progress_p);
        let snaps = tracker.snapshots();
        assert_eq!(snaps[0].agents.len(), 1);
        assert_eq!(snaps[0].agents[0].label.as_deref(), Some("coder"));
        assert_eq!(snaps[0].agents[0].token_count, Some(500));
    }

    #[test]
    fn test_clear() {
        let mut tracker = WorkflowProgressTracker::new();
        tracker.apply(&make_payload("run-1", "run_started"));
        tracker.clear();
        assert!(tracker.snapshots().is_empty());
    }

    fn make_snapshot(run_id: &str, agents: u64) -> WorkflowRunSnapshot {
        WorkflowRunSnapshot {
            run_id: run_id.to_string(),
            workflow_name: "test".to_string(),
            status: "running".to_string(),
            phases: Vec::new(),
            agents: (0..agents)
                .map(|i| WorkflowAgentSnapshot {
                    agent_id: i,
                    label: None,
                    phase: None,
                    status: "running".to_string(),
                    token_count: None,
                    tool_count: None,
                })
                .collect(),
        }
    }

    #[test]
    fn test_replace_runs_overwrites_existing() {
        let mut tracker = WorkflowProgressTracker::new();
        tracker.apply(&make_payload("run-1", "run_started"));
        // 用全量快照替换：原 run-1 应消失，run-a / run-b 应出现
        tracker.replace_runs(vec![make_snapshot("run-a", 2), make_snapshot("run-b", 3)]);
        let snaps = tracker.snapshots();
        assert_eq!(snaps.len(), 2);
        assert!(snaps.iter().any(|r| r.run_id == "run-a"));
        assert!(snaps.iter().any(|r| r.run_id == "run-b"));
        assert!(!snaps.iter().any(|r| r.run_id == "run-1"));
        // agent 总数 = 2 + 3 = 5
        let total: usize = snaps.iter().map(|r| r.agents.len()).sum();
        assert_eq!(total, 5);
    }

    #[test]
    fn test_replace_runs_empty_clears_all() {
        let mut tracker = WorkflowProgressTracker::new();
        tracker.apply(&make_payload("run-1", "run_started"));
        tracker.replace_runs(Vec::new());
        assert!(tracker.snapshots().is_empty());
    }
}
