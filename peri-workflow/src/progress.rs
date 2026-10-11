//! Reducer-based progress store: processes `ProgressEvent` and maintains UI-queryable state.
//!
//! Key design: agentId EXACT matching (not LIFO stack) — concurrent agents interleave
//! events, so `set_or_update_agent` finds by `agent_id` field, not the last element.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use indexmap::IndexMap;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::protocol::{AgentRunResult, ProgressEvent};

/// 进度事件观察者：`apply_event` 提交后回调（在 `runs` 写锁与 observer 注入锁
/// 之外，但**仍持有 `commit` 串行锁**：实现不得重入提交，也不得调用任何会触发
/// `commit` 的 API，否则同线程自锁；含 `Log` 事件）。
///
/// 单一实现点：`WorkflowRuntime` 经此记录 run 事件日志并发订阅唤醒；
/// store 自身不解释观察者语义，也不因观察者缺失改变行为。
pub trait ProgressObserver: Send + Sync {
    fn on_progress_event(&self, event: &ProgressEvent);

    /// run 集合变化（清理淘汰）提交后回调；默认无操作。
    ///
    /// `workflow://runs` 索引正文包含 run 集合：清理是唯一不经进度事件的集合变更
    /// 路径，必须在此补发索引唤醒（对抗审查 F5）。
    fn on_runs_changed(&self) {}
}

// ─── Data structures ───────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunProgress {
    pub run_id: String,
    pub workflow_name: String,
    /// 启动时刻（wall clock 毫秒）：run 索引的稳定排序依据（「最近」不得依赖
    /// `HashMap` 迭代顺序）。历史快照缺该字段时反序列化为 0。
    #[serde(default)]
    pub started_at_ms: i64,
    pub status: RunStatus,
    #[serde(default)]
    pub execution_status: peri_acp_types::workflow::ExecutionStatus,
    #[serde(default)]
    pub acceptance_status: peri_acp_types::workflow::AcceptanceStatus,
    #[serde(default)]
    pub post_processing_status: peri_acp_types::workflow::PostProcessingStatus,
    #[serde(default)]
    pub delivery_status: peri_acp_types::workflow::DeliveryStatus,
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

/// One run owns both its public projection and this execution's accounting provenance.
struct TrackedRun {
    progress: RunProgress,
    /// AgentStarted is emitted only for live execution, not a resume cache hit.
    started_agents: HashSet<u64>,
    /// 相位墙钟起点：`PhaseStarted` 的**宿主接收时刻**（引擎事件不带时间戳，
    /// 事件日志的 `at_ms` 同源）。同一相位再次开始会覆盖起点。
    phase_started_at: HashMap<String, std::time::Instant>,
    /// 已完成相位的墙钟累计毫秒（同相位多次开始/结束则累加）。
    ///
    /// 与 `started_agents` 同属本次执行的记账：resume 命中的相位没有
    /// `PhaseStarted`，因此拿不到墙钟（`duration_ms = None`）。**不回落成
    /// 相位内 agent 时长求和**——那是另一维度的量，并行相位会被读成超长
    /// （2026-10-11 issue 缺陷 3）。
    phase_wall_ms: HashMap<String, u64>,
}

pub struct WorkflowProgressStore {
    runs: RwLock<HashMap<String, TrackedRun>>,
    /// 提交序列化锁（见 `commit`）：reducer 与观察者回调同序。
    commit: parking_lot::Mutex<()>,
    /// 提交后回调（装配期一次注入；`None` = 无观察者，行为与迁移前一致）。
    observer: RwLock<Option<Arc<dyn ProgressObserver>>>,
}

impl WorkflowProgressStore {
    pub fn new() -> Self {
        Self {
            runs: RwLock::new(HashMap::new()),
            commit: parking_lot::Mutex::new(()),
            observer: RwLock::new(None),
        }
    }

    /// 注入观察者（`WorkflowRuntime` 构造期一次调用；重复注入覆盖并记录告警由调用方负责）。
    pub fn set_observer(&self, observer: Arc<dyn ProgressObserver>) {
        *self.observer.write() = Some(observer);
    }

    /// THE reducer: apply a `ProgressEvent` and update internal state.
    pub fn apply_event(&self, event: &ProgressEvent) {
        self.commit(event, |_| {});
    }

    /// 终态提交：`RunDone` reducer 与四轴投影在**同一次提交**内落定，
    /// 观察者（事件日志 + 订阅通知）只在完整终态之后收到回调。
    ///
    /// 历史上投影由 `set_terminal_projection` 在通知之后补写，订阅方按通知
    /// 补读时会读到未投影的中间态（审查 Major）。
    pub fn apply_run_done(
        &self,
        run_id: &str,
        status: &str,
        return_value: Option<serde_json::Value>,
        error: Option<String>,
        post_processing_status: peri_acp_types::workflow::PostProcessingStatus,
        delivery_status: peri_acp_types::workflow::DeliveryStatus,
    ) {
        let event = ProgressEvent::RunDone {
            run_id: run_id.to_string(),
            status: status.to_string(),
            return_value,
            error,
        };
        self.commit(&event, |run| {
            run.post_processing_status = post_processing_status;
            run.delivery_status = delivery_status;
        });
    }

    /// 单一提交器：`apply_event` 序列化执行「reducer 提交 → 投影 → 观察者回调」，
    /// 保证事件日志顺序与状态提交顺序一致（审查 Major）。
    ///
    /// 观察者回调在 `runs` 写锁与 observer 注入锁之外执行，但**仍在 `commit`
    /// 串行锁内**（本函数作用域）：回调内可安全读 store / 改注入，但**不得**再提交
    /// 事件或调用任何会触发 `commit` 的 API（同线程重入必自锁）。
    fn commit(&self, event: &ProgressEvent, extra: impl FnOnce(&mut RunProgress)) {
        let _serial = self.commit.lock();
        {
            let mut runs = self.runs.write();

            match event {
                // Log 事件不修改 state；直接落到观察者（事件日志记录它）。
                ProgressEvent::Log { .. } => {}
                ProgressEvent::RunStarted { workflow_name, .. } => {
                    let run_id = event.run_id().to_string();
                    let run = RunProgress {
                        run_id: run_id.clone(),
                        workflow_name: workflow_name.clone(),
                        started_at_ms: chrono::Utc::now().timestamp_millis(),
                        status: RunStatus::Running,
                        execution_status: peri_acp_types::workflow::ExecutionStatus::Running,
                        acceptance_status: peri_acp_types::workflow::AcceptanceStatus::Unknown,
                        post_processing_status:
                            peri_acp_types::workflow::PostProcessingStatus::Unknown,
                        delivery_status: peri_acp_types::workflow::DeliveryStatus::Unknown,
                        meta: None, // meta is raw Value; conversion not required by spec
                        phases: Vec::new(),
                        agents: IndexMap::new(),
                        completed_at: None,
                    };
                    runs.insert(
                        run_id,
                        TrackedRun {
                            progress: run,
                            started_agents: HashSet::new(),
                            phase_started_at: HashMap::new(),
                            phase_wall_ms: HashMap::new(),
                        },
                    );
                }
                _ => {
                    let run_id = event.run_id().to_string();
                    apply_state_event(&mut runs, &run_id, event);
                }
            }
            if let Some(tracked) = runs.get_mut(event.run_id()) {
                extra(&mut tracked.progress);
            }
        }
        // 先克隆 `Arc` 并释放读锁：`if let` 的临时量会存活到整个分支，
        // 不能直接写 `self.observer.read().clone()`（回调内改注入会自锁）。
        let observer = self.observer.read().clone();
        if let Some(observer) = observer {
            observer.on_progress_event(event);
        }
    }
    pub fn get_run(&self, run_id: &str) -> Option<RunProgress> {
        self.runs.read().get(run_id).map(|run| run.progress.clone())
    }

    /// run 索引快照，按 `started_at_ms` 升序（旧 → 新）稳定排序。
    ///
    /// 「最近 N 个 run」由调用方取尾部 N（`split_off(len - limit)`）表达；
    /// 排序依据不得依赖 `HashMap` 迭代顺序，同一毫秒内按 `run_id` 定序。
    pub fn list_runs(&self) -> Vec<RunProgress> {
        let mut runs: Vec<RunProgress> = self
            .runs
            .read()
            .values()
            .map(|run| run.progress.clone())
            .collect();
        runs.sort_by(|a, b| {
            a.started_at_ms
                .cmp(&b.started_at_ms)
                .then_with(|| a.run_id.cmp(&b.run_id))
        });
        runs
    }

    /// 获取所有 runs 的快照（供 ACP handler 序列化用）；与 `list_runs` 同序，
    /// 避免消费方（TUI 面板）因 `HashMap` 顺序抖动而重排。
    pub fn get_all_runs_snapshot(&self) -> Vec<RunProgress> {
        self.list_runs()
    }

    pub fn active_runs(&self) -> Vec<RunProgress> {
        self.runs
            .read()
            .values()
            .filter(|r| matches!(r.progress.status, RunStatus::Running))
            .map(|run| run.progress.clone())
            .collect()
    }

    /// 完成状态的 runs 保留时间（5 分钟），过期后清理以释放内存。
    const COMPLETED_RETENTION: std::time::Duration = std::time::Duration::from_secs(300);

    pub fn cleanup_completed(&self) {
        self.cleanup_completed_at(peri_time::monotonic_now());
    }

    /// 清理过保留窗口的终态 run；集合确有移除时，在提交（`retain` 完成）之后
    /// 通知观察者（先提交后通知；无移除不制造唤醒）。
    pub(crate) fn cleanup_completed_at(&self, now: std::time::Instant) {
        let removed = {
            let mut runs = self.runs.write();
            let before = runs.len();
            runs.retain(|_, tracked| {
                let run = &tracked.progress;
                matches!(run.status, RunStatus::Running)
                    || run
                        .completed_at
                        .map(|at| now.saturating_duration_since(at) < Self::COMPLETED_RETENTION)
                        .unwrap_or(true)
            });
            runs.len() != before
        };
        if !removed {
            return;
        }
        // 与 `commit` 同款：先克隆 `Arc` 并释放读锁，再回调。
        let observer = self.observer.read().clone();
        if let Some(observer) = observer {
            observer.on_runs_changed();
        }
    }
}

/// 非 `RunStarted` / `Log` 分支的状态更新（`apply_event` 的 reducer 主体）。
fn apply_state_event(runs: &mut HashMap<String, TrackedRun>, run_id: &str, event: &ProgressEvent) {
    match event {
        ProgressEvent::PhaseStarted { phase, .. } => {
            if let Some(tracked) = runs.get_mut(run_id) {
                tracked
                    .phase_started_at
                    .insert(phase.clone(), peri_time::monotonic_now());
                let run = &mut tracked.progress;
                set_or_update_phase(&mut run.phases, phase, PhaseStatus::Active);
            }
        }
        ProgressEvent::PhaseDone { phase, .. } => {
            if let Some(tracked) = runs.get_mut(run_id) {
                // 墙钟 = 宿主收到 PhaseStarted 到收到 PhaseDone 的时长（并行相位
                // 因此重合而非相加）；无起点（resume 命中 / 缺 PhaseStarted）不计。
                if let Some(started_at) = tracked.phase_started_at.remove(phase) {
                    let elapsed_ms = peri_time::monotonic_now()
                        .saturating_duration_since(started_at)
                        .as_millis() as u64;
                    *tracked.phase_wall_ms.entry(phase.clone()).or_insert(0) += elapsed_ms;
                }
                let run = &mut tracked.progress;
                set_or_update_phase(&mut run.phases, phase, PhaseStatus::Done);
            }
        }
        ProgressEvent::AgentStarted {
            agent_id,
            label,
            phase,
            ..
        } => {
            if let Some(tracked) = runs.get_mut(run_id) {
                tracked.started_agents.insert(*agent_id);
                let run = &mut tracked.progress;
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
            if let Some(tracked) = runs.get_mut(run_id) {
                let run = &mut tracked.progress;
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
            if let Some(tracked) = runs.get_mut(run_id) {
                let executed_in_this_run = tracked.started_agents.contains(agent_id);
                let run = &mut tracked.progress;
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
            if let Some(tracked) = runs.get_mut(run_id) {
                let run = &mut tracked.progress;
                run.status = match status.as_str() {
                    "completed" => RunStatus::Completed,
                    "killed" => RunStatus::Killed,
                    _ => RunStatus::Failed,
                };
                run.execution_status = match status.as_str() {
                    "completed" => peri_acp_types::workflow::ExecutionStatus::Completed,
                    "killed" => peri_acp_types::workflow::ExecutionStatus::Killed,
                    _ => peri_acp_types::workflow::ExecutionStatus::Failed,
                };
                run.acceptance_status = peri_acp_types::workflow::AcceptanceStatus::Unknown;
                run.post_processing_status =
                    peri_acp_types::workflow::PostProcessingStatus::Unknown;
                run.delivery_status = if status == "completed" {
                    peri_acp_types::workflow::DeliveryStatus::Unknown
                } else {
                    peri_acp_types::workflow::DeliveryStatus::Blocked
                };
                run.completed_at = Some(peri_time::monotonic_now());
            }
        }
        // RunStarted / Log 已在 `apply_event` 处理，此处不可达。
        ProgressEvent::RunStarted { .. } | ProgressEvent::Log { .. } => {
            unreachable!("RunStarted / Log handled by apply_event")
        }
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
        self.runs.read().get(run_id).map(|tracked| {
            let run = &tracked.progress;
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
    ///
    /// `duration_ms` 是该相位的**墙钟**（`PhaseStarted` → `PhaseDone` 的宿主接收
    /// 时刻差），并行相位因此重合而非相加；无相位边界事件（resume 命中、引擎未
    /// 发相位事件、无 `phase()` 的 agent 归组）时为 `None`，不回落成 agent 时长
    /// 求和（2026-10-11 issue 缺陷 3）。
    ///
    /// **顺序** = 引擎相位顺序（`run.phases` 的 `PhaseStarted` 到达序）；不在该
    /// 列表里的归组（无 `phase()` 的 agent 落在 workflow_name 桶）按名排序垫后。
    /// 不得依赖 `HashMap` 迭代序：那是随机的，用户可见的相位行会乱序
    /// （2026-10-11 真实 run 里 Digest 排在 Recon 之前）。
    pub fn get_phase_summaries(&self, run_id: &str) -> Vec<PhaseSummary> {
        let runs = self.runs.read();
        let Some(tracked) = runs.get(run_id) else {
            return Vec::new();
        };
        let run = &tracked.progress;
        let started_agents = &tracked.started_agents;
        let phase_wall_ms = &tracked.phase_wall_ms;
        let engine_order: HashMap<&str, usize> = run
            .phases
            .iter()
            .enumerate()
            .map(|(index, phase)| (phase.title.as_str(), index))
            .collect();
        let mut phase_map: std::collections::HashMap<String, (usize, u64)> =
            std::collections::HashMap::new();
        for agent in run.agents.values() {
            let phase = agent.phase.as_deref().unwrap_or(run.workflow_name.as_str());
            let entry = phase_map.entry(phase.to_string()).or_insert((0, 0));
            entry.0 += 1;
            if started_agents.contains(&agent.agent_id) {
                entry.1 += agent
                    .token_count
                    .or_else(|| agent.result.as_ref().and_then(|r| r.token_count()))
                    .unwrap_or(0);
            }
        }
        let mut summaries: Vec<PhaseSummary> = phase_map
            .into_iter()
            .map(|(name, (agent_count, token_count))| PhaseSummary {
                duration_ms: phase_wall_ms
                    .get(&name)
                    .copied()
                    .filter(|millis| *millis > 0),
                name,
                agent_count,
                token_count,
            })
            .collect();
        summaries.sort_by(|left, right| {
            let left_rank = engine_order
                .get(left.name.as_str())
                .copied()
                .unwrap_or(usize::MAX);
            let right_rank = engine_order
                .get(right.name.as_str())
                .copied()
                .unwrap_or(usize::MAX);
            // 同秩（含都无相位事件）时按名排序，保证任意输入下顺序都确定。
            left_rank
                .cmp(&right_rank)
                .then_with(|| left.name.cmp(&right.name))
        });
        summaries
    }

    /// 获取指定 agent 的 phase（供 journal 注入）。
    pub fn get_agent_phase(&self, run_id: &str, agent_id: u64) -> Option<String> {
        let runs = self.runs.read();
        let run = &runs.get(run_id)?.progress;
        let agent = run.agents.get(&agent_id)?;
        agent.phase.clone()
    }
}

// ─── Tests ──────────────────────────────────────────────────

#[cfg(test)]
#[path = "progress_test.rs"]
mod tests;
