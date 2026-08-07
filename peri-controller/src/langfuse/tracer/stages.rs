//! ReAct 5 阶段 Span 生命周期管理器。
//!
//! 管理从 Receive → Reason → Act → Compact → End 5 个阶段的 span 生命周期：
//! - on_stage_start：开始新阶段 span（自动结束同一 agent 的前一个阶段）
//! - on_stage_end：结束当前阶段（校验 handle 匹配，避免并行 subagent 交错误清）
//! - on_mq_drained：记录消息队列排空计数（Receive 阶段专用）
//! - on_workflow_start / on_workflow_end：管理 Act 阶段的 Workflow 子 span
//!
//! ## 并行 SubAgent 支持
//!
//! stage slot 按 `agent_id` 隔离（`HashMap<String, ActiveStage>`）：主 agent 与各
//! subagent 的 StageStarted/StageEnded 事件交错到达时互不覆盖，解决并行 subagent
//! 场景下 stage span 父子关系错乱（span 挂错、成对丢失）的问题。

use peri_agent::agent::events::{Stage, StageStatus};
use std::collections::HashMap;

/// 无 agent 标识的 v1 事件（ExecutorEvent 路径 / tracer 直调）使用的固定 slot key。
/// v2 ObserveEvent 路径使用事件自带的 agent_id 字符串。
pub(crate) const MAIN_AGENT_KEY: &str = "main";

#[derive(Clone)]
pub struct StageHandle {
    pub span_id: String,
    pub stage: Stage,
    pub start_time: String,
    pub trace_id: String,
    pub parent_observation_id: String,
}

pub(crate) struct WorkflowStartRecord {
    pub span_id: String,
}

pub(crate) struct WorkflowEndRecord {
    pub span_id: String,
    pub agents_spawned: usize,
    pub tool_calls: usize,
}

struct ActiveStage {
    handle: StageHandle,
    workflow_spans: HashMap<String, String>,
    mq_counts: Option<(usize, usize, usize)>,
}

pub(crate) struct StageSpans {
    /// 各 agent 的活跃 stage（key = agent_id）。并行 subagent 各自独立 slot。
    active: HashMap<String, ActiveStage>,
}

impl StageSpans {
    pub(crate) fn new() -> Self {
        Self {
            active: HashMap::new(),
        }
    }

    pub(crate) fn on_stage_start(
        &mut self,
        agent_id: &str,
        stage: Stage,
        trace_id: &str,
        _turn_id: &str,
        parent_observation_id: &str,
    ) -> StageHandle {
        // 自动结束同一 agent 的前一个 stage（清理状态，事件构造在外层）。
        // 只清理该 agent 自己的 slot——并行 subagent 的 stage 互不影响。
        self.active.remove(agent_id);
        let span_id = format!("span_{}", uuid::Uuid::now_v7());
        let start_time = chrono::Utc::now().to_rfc3339();
        let handle = StageHandle {
            span_id: span_id.clone(),
            stage,
            start_time: start_time.clone(),
            trace_id: trace_id.to_string(),
            parent_observation_id: parent_observation_id.to_string(),
        };
        let mq_counts = if stage == Stage::Receive {
            Some((0, 0, 0))
        } else {
            None
        };
        self.active.insert(
            agent_id.to_string(),
            ActiveStage {
                handle,
                workflow_spans: HashMap::new(),
                mq_counts,
            },
        );
        StageHandle {
            span_id,
            stage,
            start_time,
            trace_id: trace_id.to_string(),
            parent_observation_id: parent_observation_id.to_string(),
        }
    }

    pub(crate) fn on_stage_end(
        &mut self,
        agent_id: &str,
        handle: &StageHandle,
        _status: StageStatus,
    ) {
        // 仅当 handle 匹配该 agent 当前活跃 stage 时才清理。
        // 不匹配说明该 span 已被同一 agent 的新 stage 覆盖（或事件乱序），
        // 不误清其他 agent 的活跃 slot。
        let mismatched = match self.active.get(agent_id) {
            Some(a) => a.handle.span_id != handle.span_id,
            None => true,
        };
        if mismatched {
            tracing::warn!(
                target: "langfuse::stages",
                agent_id,
                span_id = %handle.span_id,
                "StageEnded handle 与活跃 stage 不匹配，跳过清理"
            );
            return;
        }
        self.active.remove(agent_id);
    }

    pub(crate) fn active_stage(&self, agent_id: &str) -> Option<Stage> {
        self.active.get(agent_id).map(|a| a.handle.stage)
    }

    pub(crate) fn active_handle(&self, agent_id: &str) -> Option<&StageHandle> {
        self.active.get(agent_id).map(|a| &a.handle)
    }

    pub(crate) fn on_mq_drained(
        &mut self,
        agent_id: &str,
        prompt: usize,
        defer: usize,
        info: usize,
    ) {
        if let Some(a) = self.active.get_mut(agent_id) {
            if a.handle.stage == Stage::Receive {
                a.mq_counts = Some((prompt, defer, info));
            }
        }
    }

    pub(crate) fn mq_counts(&self, agent_id: &str) -> Option<(usize, usize, usize)> {
        self.active.get(agent_id).and_then(|a| a.mq_counts)
    }

    pub(crate) fn on_workflow_start(
        &mut self,
        workflow_id: &str,
        _plan: &str,
    ) -> WorkflowStartRecord {
        // Workflow 是 v1 主 agent 概念，无 agent_id 事件来源。固定使用
        // MAIN_AGENT_KEY slot：并行 subagent 的 Act stage 与主 agent 同时
        // 活跃时，workflow span 不再可能挂到任意 Act slot（此前可能存错或
        // 在 on_workflow_end 时因选到 subagent 的 slot 而无法关闭）。
        let span_id = match self.active.get_mut(MAIN_AGENT_KEY) {
            Some(a) if a.handle.stage == Stage::Act => {
                let span_id = format!("span_{}", uuid::Uuid::now_v7());
                a.workflow_spans
                    .insert(workflow_id.to_string(), span_id.clone());
                span_id
            }
            _ => String::new(),
        };
        WorkflowStartRecord { span_id }
    }

    pub(crate) fn on_workflow_end(
        &mut self,
        workflow_id: &str,
        agents_spawned: usize,
        tool_calls: usize,
    ) -> Option<WorkflowEndRecord> {
        let a = self.active.get_mut(MAIN_AGENT_KEY)?;
        if a.handle.stage != Stage::Act {
            return None;
        }
        let span_id = a.workflow_spans.get(workflow_id)?.clone();
        Some(WorkflowEndRecord {
            span_id,
            agents_spawned,
            tool_calls,
        })
    }
}

#[cfg(test)]
#[path = "stages_test.rs"]
mod tests;
