//! 对同一工作单元的 Full 重试使用真实后续请求 usage 验证是否恢复预算。

use crate::agent::compact_v2::config::CompactConfig;
use crate::agent::compact_v2::planner::ResidentHistoryPressure;
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

/// 上一次**实际应用**的 Full 之后的驻留压力及其失效依据。
#[derive(Debug, Clone, Copy)]
pub(crate) struct ResidentFullSnapshot {
    pub(crate) pressure: ResidentHistoryPressure,
    /// 记录时的 transcript 实例身份。
    ///
    /// 内容 revision 随每次追加变化，不能用它失效快照——门控要评估的新工作本身就会
    /// 改变 revision。实例身份只在 transcript 被重建/换出时变化。
    pub(crate) transcript_instance: u64,
    /// 记录时的末尾水印：新工作判定的起点。
    pub(crate) watermark: Option<MessageId>,
    /// watermark 条目的内容指纹（kind + 长度 + 稳定哈希）。
    ///
    /// 只锚定 watermark：替换/删除 watermark 会让快照失效；替换其他条目（包括
    /// post-watermark 的新工作）不影响。追加天然不改变 watermark 指纹。
    watermark_fingerprint: Option<u64>,
}

impl ResidentFullSnapshot {
    /// 在当前 transcript 上采集 Full 后快照。
    pub(crate) fn capture(
        transcript: &MessageTranscript,
        pressure: ResidentHistoryPressure,
    ) -> Self {
        let watermark = transcript.entries().last().map(|entry| entry.id());
        Self {
            pressure,
            transcript_instance: transcript.view_revision().instance,
            watermark,
            watermark_fingerprint: watermark.and_then(|id| Self::fingerprint(transcript, id)),
        }
    }

    /// 快照是否仍描述当前 transcript 对象与 watermark 内容。
    fn is_valid_for(&self, transcript: &MessageTranscript) -> bool {
        self.transcript_instance == transcript.view_revision().instance
            && self.watermark.is_some_and(|watermark| {
                self.watermark_fingerprint.is_some_and(|recorded| {
                    Self::fingerprint(transcript, watermark) == Some(recorded)
                })
            })
    }

    /// 条目内容指纹：kind + 正文长度 + 稳定哈希（reminder 用对象字节下界）。
    fn fingerprint(transcript: &MessageTranscript, id: MessageId) -> Option<u64> {
        use std::hash::{Hash, Hasher};

        let entry = transcript.get(id)?;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        match entry.as_message() {
            Some(BaseMessage::Human { .. }) => 1u8.hash(&mut hasher),
            Some(BaseMessage::Ai { .. }) => 2u8.hash(&mut hasher),
            Some(BaseMessage::System { .. }) => 3u8.hash(&mut hasher),
            Some(BaseMessage::Tool { .. }) => 4u8.hash(&mut hasher),
            None => 5u8.hash(&mut hasher),
        }
        if let Some(message) = entry.as_message() {
            let text = message.content();
            text.len().hash(&mut hasher);
            text.hash(&mut hasher);
        } else {
            let bytes = crate::session::memory_probe::transcript_entry_bytes(entry);
            bytes.body.hash(&mut hasher);
            bytes.reserved.hash(&mut hasher);
        }
        Some(hasher.finish())
    }
}

/// 门控所需 recovery 状态的**拷贝**。
///
/// 调用方在 `budget_recovery` 锁内构造本结构后必须立即释放锁，再读取 transcript；
/// 判定不持有任何 recovery 锁，从而避免与 transcript guard 嵌套。
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ResidentGateState {
    /// 当前新工作判定的起点（可能已被 `begin_request` 前移）。
    pub(crate) watermark: Option<MessageId>,
    pub(crate) last_full: Option<ResidentFullSnapshot>,
}

impl ResidentGateState {
    /// 纯判定：调用方不得持有 `budget_recovery` 锁；可以持有 transcript 读锁。
    ///
    /// 残余主导成立时按**可回收字节增长**判定：当前可回收 own 必须严格大于
    /// 「上次 Full 后可回收 own + 完整 limit」。这样即使 Full 后摘要自身已经超过
    /// 极小 limit，只要后续只追加小结果就不会重复 Full。
    pub(crate) fn decide(
        &self,
        current: ResidentHistoryPressure,
        transcript: &MessageTranscript,
    ) -> ResidentHistoryPressure {
        if !current.is_exceeded() {
            return ResidentHistoryPressure::DISABLED;
        }
        let new_work = CompactBudgetRecovery::has_new_work_after(self.watermark, transcript, false);
        let residual = self.last_full.filter(|snapshot| {
            snapshot.is_valid_for(transcript)
                && snapshot.pressure.limit_bytes == current.limit_bytes
                && snapshot.pressure.total_reserved_bytes() > current.limit_bytes
        });
        if let Some(snapshot) = residual {
            let last_full_reclaimable = snapshot.pressure.reclaimable_own_reserved_bytes();
            let growth_threshold = last_full_reclaimable.saturating_add(current.limit_bytes);
            let growth = current
                .reclaimable_own_reserved_bytes()
                .saturating_sub(last_full_reclaimable);
            if current.reclaimable_own_reserved_bytes() > growth_threshold && new_work {
                return current;
            }
            tracing::debug!(
                resident_limit_bytes = current.limit_bytes,
                resident_reclaimable_own_bytes = current.reclaimable_own_reserved_bytes(),
                resident_irreducible_own_bytes = current.irreducible_own_reserved_bytes(),
                resident_irreducible_inherited_bytes =
                    current.irreducible_inherited_reserved_bytes(),
                resident_total_reserved_bytes = current.total_reserved_bytes(),
                last_full_reclaimable_own_bytes = last_full_reclaimable,
                reclaimable_growth_bytes = growth,
                reclaimable_growth_threshold_bytes = current.limit_bytes,
                new_work_since_full = new_work,
                "驻留超限但残余主导：可回收 own 相对上次 Full 的增长未超过完整预算，跳过 Full"
            );
            return ResidentHistoryPressure::DISABLED;
        }
        if let Some(snapshot) = self.last_full {
            if !snapshot.is_valid_for(transcript)
                || snapshot.pressure.limit_bytes != current.limit_bytes
            {
                tracing::debug!(
                    resident_limit_bytes = current.limit_bytes,
                    snapshot_limit_bytes = snapshot.pressure.limit_bytes,
                    snapshot_transcript_instance = snapshot.transcript_instance,
                    current_transcript_instance = transcript.view_revision().instance,
                    snapshot_watermark = ?snapshot.watermark,
                    snapshot_watermark_fingerprint = ?snapshot.watermark_fingerprint,
                    "驻留快照失效：limit 或 transcript 实例/watermark 内容已变化，回到普通门控"
                );
            }
        }
        if new_work {
            current
        } else {
            ResidentHistoryPressure::DISABLED
        }
    }
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
    /// 上一次**实际应用**的 Full 之后的驻留压力快照。
    ///
    /// 用于区分两种超限：Full 后已回到预算内（正常门控），以及 Full 后仍超限
    /// （残余主导）。shadow、失败、未提交与 Interrupted 不更新；InterruptedAfterCommit
    /// 等已提交路径必须记录。快照同时绑定记录时的 limit、transcript 实例与末尾水印，
    /// 三者任一不一致即失效并回到普通门控。
    resident_after_full: Option<ResidentFullSnapshot>,
    unrecovered_observations: u32,
}

impl CompactBudgetRecovery {
    /// 记录一次已成功的 Full。
    ///
    /// `new_work_before_full` 必须在 Full 改动 transcript **之前**采集（见
    /// [`Self::new_work_since_full`]）：Full 自己追加的摘要不是新工作，而收缩后
    /// 位置不再可比，事后只能由调用方在提交前判定。
    ///
    /// `snapshot` 由调用方在 transcript guard 内通过 [`ResidentFullSnapshot::capture`]
    /// 采集并释放 guard 后传入；本方法只持 `budget_recovery` 锁，不得反向读取
    /// transcript。只有实际应用的 Full 才能调用（shadow/failure/未提交不调用）。
    pub(crate) fn record_full_applied(
        &mut self,
        new_work_before_full: bool,
        snapshot: ResidentFullSnapshot,
    ) {
        if new_work_before_full {
            self.unrecovered_observations = 0;
        }
        // 水印取 Full 提交后的末尾条目：它覆盖本次追加的摘要（及同批尾部条目），
        // 使这些生成内容不参与后续「新工作」判定。
        self.full_watermark = snapshot.watermark;
        self.full_generation = self.full_generation.saturating_add(1);
        self.pending_generation = Some(self.full_generation);
        let pressure = snapshot.pressure;
        tracing::debug!(
            resident_limit_bytes = pressure.limit_bytes,
            resident_reclaimable_own_bytes = pressure.reclaimable_own_reserved_bytes(),
            resident_irreducible_own_bytes = pressure.irreducible_own_reserved_bytes(),
            resident_irreducible_inherited_bytes = pressure.irreducible_inherited_reserved_bytes(),
            resident_total_reserved_bytes = pressure.total_reserved_bytes(),
            "记录已应用 Full 后的驻留压力快照"
        );
        self.resident_after_full = Some(snapshot);
    }

    /// 拷贝门控所需的 recovery 状态；调用方随后必须释放锁再读 transcript。
    pub(crate) fn resident_gate_state(&self) -> ResidentGateState {
        ResidentGateState {
            watermark: self.full_watermark,
            last_full: self.resident_after_full,
        }
    }

    /// 当前记录的上一次应用 Full 后的驻留压力（测试断言用）。
    #[cfg(test)]
    pub(crate) fn resident_after_full(&self) -> Option<ResidentHistoryPressure> {
        self.resident_after_full.map(|snapshot| snapshot.pressure)
    }

    /// 自上次 Full 以来是否出现真正的新工作（非空 Human / Tool 消息）。
    ///
    /// 用稳定身份定位上次 Full 的末尾水印，只看其后的条目——语义等价于
    /// 「`full_end..len`」但对收缩鲁棒。水印不在 transcript 里（rewind/重建释放了
    /// 它）等价于旧实现的 `end < full_end`：旧边界失效，按有新工作处理。
    pub(crate) fn new_work_since_full(&self, transcript: &MessageTranscript) -> bool {
        self.has_new_work(transcript, false)
    }

    /// 开始一次请求。`new_visible_work` 与 `last_entry` 由调用方在 transcript
    /// guard 内采集；本方法只持 `budget_recovery` 锁，避免与 transcript 嵌套。
    pub(crate) fn begin_request(
        &mut self,
        new_visible_work: bool,
        last_entry: Option<MessageId>,
    ) -> Option<FullBudgetProbe> {
        if new_visible_work {
            // Full 后、请求前又注入可压缩内容：该请求已不代表 Full 的结果。
            self.unrecovered_observations = 0;
            self.pending_generation = None;
            // 水印前移吞掉这批新工作：下个 epoch 从当前末尾重新起算。
            self.full_watermark = last_entry;
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
        Self::has_new_work_after(self.full_watermark, transcript, visible_only)
    }

    /// 以给定水印判定其后的新工作；不访问 `budget_recovery` 状态。
    pub(crate) fn has_new_work_after(
        watermark: Option<MessageId>,
        transcript: &MessageTranscript,
        visible_only: bool,
    ) -> bool {
        // 水印缺席有两种含义，都按「旧边界失效，允许重新压缩」处理：从未发生过 Full，
        // 或 rewind/重建释放了水印条目（祖先区不受影响——水印是 own 条目）。
        let Some(watermark) = watermark else {
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
