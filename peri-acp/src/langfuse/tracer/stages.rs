//! ReAct 5 阶段 Span 生命周期管理器。
//!
//! 管理从 Receive → Reason → Act → Compact → End 5 个阶段的 span 生命周期：
//! - on_stage_start：开始新阶段 span（自动结束前一个阶段）
//! - on_stage_end：结束当前阶段
//! - on_mq_drained：记录消息队列排空计数（Receive 阶段专用）
//! - on_workflow_start / on_workflow_end：管理 Act 阶段的 Workflow 子 span

use peri_agent::agent::events::{Stage, StageStatus};
use std::collections::HashMap;

#[derive(Clone)]
pub(crate) struct StageHandle {
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
    active: Option<ActiveStage>,
}

impl StageSpans {
    pub(crate) fn new() -> Self {
        Self { active: None }
    }

    pub(crate) fn on_stage_start(
        &mut self,
        stage: Stage,
        trace_id: &str,
        _turn_id: &str,
        parent_observation_id: &str,
    ) -> StageHandle {
        // 自动结束前一个 stage（清理状态，事件构造在外层）
        self.active = None;
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
        self.active = Some(ActiveStage {
            handle,
            workflow_spans: HashMap::new(),
            mq_counts,
        });
        StageHandle {
            span_id,
            stage,
            start_time,
            trace_id: trace_id.to_string(),
            parent_observation_id: parent_observation_id.to_string(),
        }
    }

    pub(crate) fn on_stage_end(&mut self, _handle: &StageHandle, _status: StageStatus) {
        self.active = None;
    }

    pub(crate) fn active_stage(&self) -> Option<Stage> {
        self.active.as_ref().map(|a| a.handle.stage)
    }

    pub(crate) fn active_handle(&self) -> Option<&StageHandle> {
        self.active.as_ref().map(|a| &a.handle)
    }

    pub(crate) fn on_mq_drained(&mut self, prompt: usize, defer: usize, info: usize) {
        if let Some(a) = &mut self.active {
            if a.handle.stage == Stage::Receive {
                a.mq_counts = Some((prompt, defer, info));
            }
        }
    }

    pub(crate) fn mq_counts(&self) -> Option<(usize, usize, usize)> {
        self.active.as_ref().and_then(|a| a.mq_counts)
    }

    pub(crate) fn on_workflow_start(
        &mut self,
        workflow_id: &str,
        _plan: &str,
    ) -> WorkflowStartRecord {
        let span_id = match &mut self.active {
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
        let a = self.active.as_ref()?;
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
