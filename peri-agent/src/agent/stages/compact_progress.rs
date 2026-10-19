//! 对同一工作单元的 Full 重试使用真实后续请求 usage 验证是否恢复预算。

use crate::agent::compact_v2::config::CompactConfig;
use crate::agent::token::ContextBudget;
use crate::error::{AgentError, AgentResult};
use crate::messages::{BaseMessage, MessageId};
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
/// 第二次 Full 仍可能进一步缩短摘要，故保留一次重试。
/// 本状态不推断不可压缩 token 下限，也不修改控制消息生命周期。
#[derive(Debug, Default)]
pub(crate) struct CompactBudgetRecovery {
    full_generation: u64,
    request_generation: u64,
    pending_generation: Option<u64>,
    /// 上次 Full 提交后末尾摘要消息的 id：稳定身份水印，代替「提交时的条目数」。
    /// 收缩会释放条目，位置不再可比；水印丢失（rewind/重建）等价于旧边界失效。
    full_watermark: Option<MessageId>,
    unrecovered_observations: u32,
}

impl CompactBudgetRecovery {
    /// 记录一次已成功的 Full。
    ///
    /// `new_work_before_full` 必须在 Full 改动 transcript **之前**采集（见
    /// [`Self::new_work_since_full`]）：Full 自己追加的摘要不是新工作，而收缩后
    /// 位置不再可比，事后只能由调用方在提交前判定。
    pub(crate) fn record_full_applied(
        &mut self,
        transcript: &MessageTranscript,
        new_work_before_full: bool,
    ) {
        if new_work_before_full {
            self.unrecovered_observations = 0;
        }
        // 水印取末尾条目：它覆盖 Full 本次追加的摘要（及同批尾部条目），
        // 使这些生成内容不参与后续「新工作」判定。
        self.full_watermark = transcript.entries().last().map(|entry| entry.id());
        self.full_generation = self.full_generation.saturating_add(1);
        self.pending_generation = Some(self.full_generation);
    }

    /// 自上次 Full 以来是否出现真正的新工作（非空 Human / Tool 消息）。
    ///
    /// 用稳定身份定位上次 Full 的末尾水印，只看其后的条目——语义等价于
    /// 「`full_end..len`」但对收缩鲁棒。水印不在 transcript 里（rewind/重建释放了
    /// 它）等价于旧实现的 `end < full_end`：旧边界失效，按有新工作处理。
    pub(crate) fn new_work_since_full(&self, transcript: &MessageTranscript) -> bool {
        self.has_new_work(transcript, false)
    }

    pub(crate) fn begin_request(
        &mut self,
        transcript: &MessageTranscript,
    ) -> Option<FullBudgetProbe> {
        if self.has_new_work(transcript, true) {
            // Full 后、请求前又注入可压缩内容：该请求已不代表 Full 的结果。
            self.unrecovered_observations = 0;
            self.pending_generation = None;
            // 水印前移吞掉这批新工作：下个 epoch 从当前末尾重新起算。
            self.full_watermark = transcript.entries().last().map(|entry| entry.id());
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

    fn has_new_work(&self, transcript: &MessageTranscript, visible_only: bool) -> bool {
        // 水印缺席有两种含义，都按「旧边界失效，允许重新压缩」处理：从未发生过 Full，
        // 或 rewind/重建释放了水印条目（祖先区不受影响——水印是 own 条目）。
        let Some(watermark) = self.full_watermark else {
            return true;
        };
        let Some(rest) = transcript.entries_after(watermark) else {
            return true;
        };
        rest.iter()
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
