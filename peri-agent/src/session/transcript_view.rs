//! `MessageTranscript` 的视图与标记查询面。
//!
//! 从 `transcript.rs` 按职责拆出（STD-SIZE-001 文件规模上限）：内容修订标识、
//! 模型视图复用所需的只读标记访问，以及客户端可见快照构造。作为 `transcript`
//! 的 `#[path]` 子模块，保持对 `MessageTranscript` 私有字段的访问；方法签名与
//! 语义与原实现一致。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use peri_acp_types::store::MessageFlags;
use peri_acp_types::system_reminder::ReminderAudience;

use crate::agent::compact_v2::projection::MessageProjectionDirective;
use crate::messages::{BaseMessage, MessageId};

use super::MessageTranscript;

// ─── TranscriptRevision ───────────────────────────────────────────────────────

/// Transcript 内容修订标识：实例身份 + 单调修订号。
///
/// 用于判定同一执行准备范围内的模型视图能否复用。实例身份必须参与比较：
/// Compact 阶段会把 transcript 整体从 `RwLock` 中换出（`std::mem::take`），
/// 换入的是全新空实例；只比较修订号会把新实例的内容误判成旧实例的内容。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TranscriptRevision {
    /// 实例身份（进程内唯一，不随内容变化）
    pub instance: u64,
    /// 内容修订号：任何改变 entries / flags / reminder 的写入都会自增
    pub revision: u64,
}

/// 实例身份分配器：仅用于区分不同的 [`MessageTranscript`] 实例。
static TRANSCRIPT_INSTANCE_SEQ: AtomicU64 = AtomicU64::new(1);

/// 分配一个新的 transcript 实例身份（`MessageTranscript::new` 调用）。
pub(super) fn next_instance_id() -> u64 {
    TRANSCRIPT_INSTANCE_SEQ.fetch_add(1, Ordering::Relaxed)
}

impl MessageTranscript {
    /// 内容修订标识（模型视图复用的判定输入）。
    pub fn view_revision(&self) -> TranscriptRevision {
        TranscriptRevision {
            instance: self.view_instance,
            revision: self.view_revision,
        }
    }

    /// 内容发生可能改变模型视图的变更后自增修订号。
    ///
    /// 只增不改，且不能用量代理：同长度替换（`replace_by_id`）、flag 覆写
    /// （truncated/excluded/projection）、reminder 镜像都会改变模型可见内容。
    /// Staging（`stage_ai_message` / `stage_tool_result`）不进入模型视图，
    /// 提交（[`Self::commit_staged`]）时才递增。
    pub(super) fn bump_view_revision(&mut self) {
        self.view_revision = self.view_revision.wrapping_add(1);
    }
    /// 获取所有**可见**消息的 owned Arc 快照（跳过 excluded 标记的消息）
    ///
    /// 用于在事件边界（如 `RenderEvent::TurnCompleted`）向 TUI/ACP 消费方传递
    /// 权威 transcript 快照。这是客户端出口：只有声明
    /// [`ReminderAudience::Tui`] 的 canonical reminder 进入快照，Model-only 内容
    /// （如 recall）不得出现在客户端 wire 上。
    ///
    /// **注意**：构建快照时仍会逐条深拷贝消息本体（需要过滤 excluded 并取得
    /// 独立所有权，无法与内部 `entries` 直接共享）；Arc 只保证快照在后续
    /// 事件管道多级传递时不再被重复深拷贝。
    pub fn visible_snapshot(&self) -> Arc<Vec<BaseMessage>> {
        let filtered: Vec<BaseMessage> = self
            .entries
            .iter()
            .filter(|entry| {
                let f = self.flags.get(&entry.id());
                match f {
                    None => true,
                    Some(flags) => !flags.excluded,
                }
            })
            .filter(|entry| entry.visible_to(ReminderAudience::Tui))
            .map(|entry| entry.project_message().expect("validated transcript entry"))
            .collect();
        Arc::new(filtered)
    }
    /// 获取消息标记（无标记时返回默认值）
    pub fn flags(&self, id: MessageId) -> MessageFlags {
        self.flags.get(&id).cloned().unwrap_or_default()
    }

    /// 只读获取消息标记；无标记时返回 `None`。
    ///
    /// 与 [`Self::flags`] 的区别：不克隆 `projection` directive 本体。
    /// 只读查询（是否 excluded/truncated、取 directive 引用）一律走此接口。
    pub fn flags_ref(&self, id: MessageId) -> Option<&MessageFlags> {
        self.flags.get(&id)
    }

    /// 该消息是否被排除在模型视图之外（不克隆 flags）。
    pub fn is_excluded(&self, id: MessageId) -> bool {
        self.flags_ref(id).is_some_and(|flags| flags.excluded)
    }

    /// 该消息是否已被 Micro Compact 标记截断（不克隆 flags）。
    pub fn is_truncated(&self, id: MessageId) -> bool {
        self.flags_ref(id).is_some_and(|flags| flags.truncated)
    }

    /// 只读获取该消息的投影指令（不克隆 directive）。
    pub fn projection_ref(&self, id: MessageId) -> Option<&MessageProjectionDirective> {
        self.flags_ref(id)?.projection.as_ref()
    }

    /// 按 id 获取消息标记，消息不存在时返回 None
    ///
    /// 与 `flags()` 不同：此方法先确认 id 存在于索引表中，
    /// 不存在则返回 `None`（而非返回默认标记）。
    pub fn get_flags(&self, id: MessageId) -> Option<MessageFlags> {
        self.id_index.get(&id)?;
        Some(self.flags(id))
    }
}

#[cfg(test)]
#[path = "transcript_revision_test.rs"]
mod revision_tests;
