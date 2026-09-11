//! 单次 workflow agent 的观测统计与实时进度；Langfuse 始终在本地投影之后调用。

use std::sync::Arc;

use parking_lot::Mutex;
use peri_acp_types::{
    event::{AgentEventHandler, ExecutorEvent, FnEventHandler},
    workflow::{AgentRunParams, ProgressEvent},
};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, warn};

use super::WorkflowLangfuseEventHandler;

#[derive(Clone, Default)]
pub(super) struct RunStats {
    pub output_tokens: u64,
    pub last_model: Option<String>,
    pub tool_count: u64,
}

pub(super) struct WorkflowObservation {
    stats: Mutex<RunStats>,
    progress_tx: Option<UnboundedSender<ProgressEvent>>,
    run_id: String,
    agent_id: u64,
    langfuse: Option<WorkflowLangfuseEventHandler>,
}

impl WorkflowObservation {
    pub fn new(
        params: &AgentRunParams,
        progress_tx: Option<UnboundedSender<ProgressEvent>>,
        langfuse: Option<WorkflowLangfuseEventHandler>,
    ) -> Arc<Self> {
        Arc::new(Self {
            stats: Mutex::new(RunStats::default()),
            progress_tx,
            run_id: params.run_id.clone(),
            agent_id: params.agent_id,
            langfuse,
        })
    }

    pub fn handler(self: &Arc<Self>) -> Arc<dyn AgentEventHandler> {
        let observation = self.clone();
        Arc::new(FnEventHandler(move |event| observation.on_event(event)))
    }

    pub fn snapshot(&self) -> RunStats {
        self.stats.lock().clone()
    }

    /// 有效模型尽早单独投影；None 计数不会抹掉引擎此前尝试的统计。
    pub fn report_model(&self, model_name: &str, model_tier: Option<String>) {
        self.send_progress(Some(model_name.to_string()), model_tier, None, "model");
    }

    fn report_stats(&self, source: &str) {
        if self.progress_tx.is_some() {
            self.send_progress(None, None, Some(self.snapshot()), source);
        }
    }

    fn send_progress(
        &self,
        model: Option<String>,
        model_tier: Option<String>,
        stats: Option<RunStats>,
        source: &str,
    ) {
        if let Some(tx) = &self.progress_tx {
            if let Err(error) = tx.send(ProgressEvent::AgentProgress {
                run_id: self.run_id.clone(),
                agent_id: self.agent_id,
                label: None,
                phase: None,
                model,
                model_tier,
                token_count: stats.as_ref().map(|stats| stats.output_tokens),
                tool_count: stats.as_ref().map(|stats| stats.tool_count),
            }) {
                warn!(target: "workflow", run_id = %self.run_id, agent_id = self.agent_id, %error, source, "progress_tx.send failed");
            }
        }
    }

    fn on_event(&self, event: ExecutorEvent) {
        match &event {
            ExecutorEvent::ToolStart { name, .. } => {
                self.stats.lock().tool_count += 1;
                debug!(tool = %name, "workflow agent: tool started");
                self.report_stats("ToolStart");
            }
            ExecutorEvent::ToolEnd { name, is_error, .. } => {
                if *is_error {
                    warn!(tool = %name, "workflow agent: tool failed");
                } else {
                    debug!(tool = %name, "workflow agent: tool completed");
                }
            }
            ExecutorEvent::LlmCallEnd { model, usage, .. } => {
                debug!(model = %model, tokens = ?usage.as_ref().map(|u| (u.input_tokens, u.output_tokens)), "workflow agent: llm call completed");
                {
                    let mut stats = self.stats.lock();
                    if let Some(usage) = usage {
                        stats.output_tokens += usage.output_tokens as u64;
                    }
                    stats.last_model = Some(model.clone());
                }
                self.report_stats("LlmCallEnd");
            }
            ExecutorEvent::LlmRetrying {
                attempt,
                max_attempts,
                error,
                ..
            } => {
                warn!(attempt, max_attempts, error = %error, "workflow agent: llm retrying");
            }
            _ => {}
        }
        if let Some(langfuse) = &self.langfuse {
            langfuse(&event);
        }
    }
}

#[cfg(test)]
#[path = "observation_test.rs"]
mod tests;
