//! Stage/middleware 句柄配对和旁路指标；不持有第二套观测归属状态。

use super::{LangfuseBridge, LangfuseTracer, StageHandle};
use peri_agent::agent::events::{MiddlewareHook, Stage, StageStatus};
use std::collections::{HashMap, HashSet};

impl LangfuseBridge {
    pub(super) fn start_stage(
        &self,
        agent_id: &str,
        stage: Stage,
        turn_id: &str,
        active_stage: &mut HashMap<String, StageHandle>,
    ) {
        let handle = {
            let mut tracer = self.tracer.lock();
            tracer.on_stage_start_gated(agent_id, stage, turn_id)
        };
        if let Some(handle) = handle {
            active_stage.insert(agent_id.to_string(), handle);
        }
    }

    pub(super) fn finish_middleware(
        &self,
        mw_name: &str,
        hook: MiddlewareHook,
        status: StageStatus,
        error: &Option<String>,
    ) {
        // 先释放 MutexGuard，查询活跃 middleware span
        let span_id = {
            let t2 = self.tracer.lock();
            t2.middleware.find_active(mw_name, hook)
        };
        if let Some(span_id) = span_id {
            let handle = crate::langfuse::tracer::middleware::MiddlewareSpanHandle {
                span_id,
                name: mw_name.to_string(),
                hook,
            };
            self.tracer
                .lock()
                .on_middleware_end(&handle, status, error.clone());
        } else {
            tracing::warn!(
                target: "langfuse::forward",
                %mw_name,
                ?hook,
                "MiddlewareEnded without active middleware span, skipping"
            );
        }
    }
}

pub(super) fn finish_stage(
    t: &mut LangfuseTracer,
    agent_id: &str,
    status: StageStatus,
    active_stage: &mut HashMap<String, StageHandle>,
) {
    // 按 agent_id 精确配对：只结束该 agent 自己的 handle，
    // 其他并行 subagent 的活跃 stage 不受影响。
    if let Some(handle) = active_stage.remove(agent_id) {
        t.on_stage_end(agent_id, &handle, status);
    } else {
        // 乱序场景:StageStarted 被注册闸门缓存后重放,handle 在 tracer 侧
        if let Some(handle) = t.take_replayed_stage_handle(agent_id) {
            t.on_stage_end(agent_id, &handle, status);
        } else {
            tracing::warn!(
                target: "langfuse::forward",
                %agent_id,
                "StageEnded 无匹配的活跃 stage handle（可能事件乱序或已结束），跳过"
            );
        }
    }
}

#[derive(Default)]
pub(super) struct SubagentTelemetry {
    active: HashSet<String>,
    starts: u64,
    stops: u64,
}

impl SubagentTelemetry {
    pub(super) fn on_start(
        &mut self,
        parent_agent_id: &str,
        child_agent_id: &str,
        agent_name: &str,
        is_background: bool,
    ) {
        if self.active.contains(child_agent_id) {
            tracing::warn!(
                target: "langfuse::subagent",
                %child_agent_id,
                "SubagentStart 重复（child_agent_id 已有活跃记录），覆盖注册"
            );
        }
        self.active.insert(child_agent_id.to_string());
        self.starts += 1;
        tracing::info!(
            target: "langfuse::subagent",
            event = "subagent_start",
            %parent_agent_id,
            %child_agent_id,
            %agent_name,
            is_background,
            active = self.active.len(),
            "SubagentStart 注册"
        );
    }

    pub(super) fn on_stop(
        &mut self,
        parent_agent_id: &str,
        child_agent_id: &str,
        agent_name: &str,
        result: &str,
        is_error: bool,
    ) {
        let was_registered = self.active.remove(child_agent_id);
        self.stops += 1;
        tracing::info!(
            target: "langfuse::subagent",
            event = "subagent_stop",
            %parent_agent_id,
            %child_agent_id,
            %agent_name,
            is_error,
            was_registered,
            active = self.active.len(),
            result_len = result.len(),
            "SubagentStop 注销"
        );
        if !was_registered {
            tracing::warn!(
                target: "langfuse::subagent",
                %child_agent_id,
                "SubagentStop 无对应 Start（丢失/乱序），tracer registry 记录 incomplete"
            );
        }
    }

    #[cfg(test)]
    pub(super) fn active_count(&self) -> usize {
        self.active.len()
    }

    #[cfg(test)]
    pub(super) fn event_counts(&self) -> (u64, u64) {
        (self.starts, self.stops)
    }
}
