//! 对同一工作单元的 Full 重试使用真实后续请求 usage 验证是否恢复预算。

use crate::agent::compact_v2::config::CompactConfig;
use crate::agent::token::ContextBudget;
use crate::error::{AgentError, AgentResult};
use crate::messages::BaseMessage;
use crate::session::MessageTranscript;
use peri_model::TokenUsage;

/// 一次成功 Full 之后实际发出的 Reason 请求凭据。
#[derive(Debug, Clone, Copy)]
pub(crate) struct FullBudgetProbe {
    full_generation: u64,
    request_generation: u64,
}

/// 同一工作单元最多接受两次未恢复预算的 Full 后观测。
///
/// 第二次 Full 仍可能回收第一次的文件 re-inject 或进一步缩短摘要，故保留
/// 一次重试。本状态不推断不可压缩 token 下限，也不修改控制消息生命周期。
#[derive(Debug, Default)]
pub(crate) struct CompactBudgetRecovery {
    full_generation: u64,
    request_generation: u64,
    pending_generation: Option<u64>,
    full_end: usize,
    unrecovered_observations: u32,
}

impl CompactBudgetRecovery {
    pub(crate) fn record_full_applied(
        &mut self,
        transcript: &MessageTranscript,
        before_entries_len: usize,
    ) {
        // 此时 Full 已排除旧消息；新 work 检查保留这些刚被压缩的原消息，
        // 但不计入 Full 自己追加的摘要、文件和 skills。
        if self.has_new_work(transcript, before_entries_len, false) {
            self.unrecovered_observations = 0;
        }
        self.full_end = transcript.len();
        self.full_generation = self.full_generation.saturating_add(1);
        self.pending_generation = Some(self.full_generation);
    }

    pub(crate) fn begin_request(
        &mut self,
        transcript: &MessageTranscript,
    ) -> Option<FullBudgetProbe> {
        if self.has_new_work(transcript, transcript.len(), true) {
            // Full 后、请求前又注入可压缩内容：该请求已不代表 Full 的结果。
            self.unrecovered_observations = 0;
            self.pending_generation = None;
            self.full_end = transcript.len();
        }
        let full_generation = self.pending_generation?;
        self.request_generation = self.request_generation.saturating_add(1);
        Some(FullBudgetProbe {
            full_generation,
            request_generation: self.request_generation,
        })
    }

    pub(crate) fn observe_response(
        &mut self,
        probe: Option<FullBudgetProbe>,
        usage: Option<&TokenUsage>,
        config: &CompactConfig,
        budget: &ContextBudget,
    ) -> AgentResult<()> {
        let Some(probe) = probe else {
            return Ok(());
        };
        if self.pending_generation != Some(probe.full_generation)
            || self.request_generation != probe.request_generation
        {
            return Ok(());
        }
        let Some(usage) = usage.filter(|usage| usage.input_tokens > 0) else {
            // 缺失/零 usage 不能证明恢复，也不能借用之前请求的 tracker usage。
            return Ok(());
        };
        self.pending_generation = None;
        if budget.context_window == 0
            || f64::from(usage.input_tokens) / f64::from(budget.context_window)
                < config.auto_compact_threshold
        {
            self.unrecovered_observations = 0;
            return Ok(());
        }
        self.unrecovered_observations = self.unrecovered_observations.saturating_add(1);
        if self.unrecovered_observations < 2 {
            return Ok(());
        }
        tracing::warn!(
            full_generation = probe.full_generation,
            full_attempts = self.unrecovered_observations,
            input_tokens = usage.input_tokens,
            context_window = budget.context_window,
            "Full Compact 后同一工作单元仍未恢复预算"
        );
        Err(AgentError::CompactBudgetUnrecovered {
            input_tokens: usage.input_tokens,
            context_window: budget.context_window,
            full_attempts: self.unrecovered_observations,
        })
    }

    fn has_new_work(&self, transcript: &MessageTranscript, end: usize, visible_only: bool) -> bool {
        // Rewind/rebuild 后旧边界不再有效，允许重新压缩恢复后的工作。
        if end < self.full_end {
            return true;
        }
        transcript.entries()[self.full_end.max(transcript.ancestor_len()).min(end)..end]
            .iter()
            .filter(|entry| !visible_only || !transcript.flags(entry.id()).excluded)
            .filter_map(|entry| entry.as_message())
            .any(|message| {
                matches!(
                    message,
                    BaseMessage::Human { .. } | BaseMessage::Tool { .. }
                ) && !message.message_content().is_empty()
            })
    }
}

#[cfg(test)]
#[path = "compact_progress_test.rs"]
mod tests;
