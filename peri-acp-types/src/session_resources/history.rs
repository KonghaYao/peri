//! 会话资源门面的历史读取与 rewind 契约类型（自 `session_resources` 按职责拆出）。
//!
//! 拆出范围：有界有效视图 [`ActiveSessionSnapshot`]（有界恢复路径的读取结果）
//! 与 rewind 的边界/效力契约 [`RewindBoundary`] / [`RewindOutcome`]。这里只有类型
//! 与纯访问器，没有行为：门面行为仍在父模块的 `SessionResources` trait。
//!
//! 公开路径不变：父模块 `pub use` 重导出全部条目，消费方继续用
//! `crate::session_resources::ActiveSessionSnapshot` 等既有路径。

use std::collections::HashMap;

use super::{BindingState, FrozenState};
use crate::messages::{strip_system_reminders, BaseMessage, MessageId};
use crate::store::{InheritedContext, MessageFlags, PersistedPayload};
use crate::thread::ThreadMeta;

/// 执行关键的有效视图：恢复后真正要跑的那一份上下文。
///
/// 与 [`super::SessionSnapshot`] 的差别只在**范围**，不在一致性要求：两者都是一次一致读取。
/// 被 Full/Smart compact 排除的 own 条目**不在这里返回**——它们仍存在于存储中，
/// 完整历史（含已排除正文）是另一条接口，见 [`super::SessionResources::load_session_history`]。
///
/// 排除与投影判定属于存储事实，由后端在读取时给出：调用方直接消费这里的集合语义，
/// 不重新推导「哪些条目还在有效范围里」。
#[derive(Clone, Debug)]
pub struct ActiveSessionSnapshot {
    pub meta: ThreadMeta,
    pub binding: BindingState,
    pub frozen: FrozenState,
    pub inherited: InheritedContext,
    /// 仅未排除的 own 条目，顺序与 [`super::SessionSnapshot::payloads`] 一致。
    pub payloads: Vec<PersistedPayload>,
    /// 仅活跃 id 的非默认 flags：已排除条目的标记不返回。
    pub flags: HashMap<MessageId, MessageFlags>,
}

/// rewind 边界：区分「保留到目标」与「从目标开始移除」。
///
/// transcript rewind 保留目标本身，ACP 用户 rewind 移除目标及以后；两者语义不可
/// 合并，合并会把「保留到目标」失真成「删掉目标」。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RewindBoundary {
    KeepThrough(MessageId),
    RemoveFrom(MessageId),
}

impl RewindBoundary {
    pub fn message_id(&self) -> MessageId {
        match self {
            Self::KeepThrough(id) | Self::RemoveFrom(id) => *id,
        }
    }
}

/// rewind 提交的**实际效力**：边界是否真的在存储里生效。
///
/// 「未知边界保持无变更」是历史语义（目标不在本会话时既不删也不清零），但它是**没有
/// 生效**，不是「生效了」：调用方据此决定能不能把内存热态改写成「已截断 + 已清零」的
/// 那份。两者分开表达，存储与热态才不会分叉。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RewindOutcome {
    /// 边界成立：删除与清零都在同一次提交里落定。
    Applied,
    /// 边界目标不在本会话历史里：本次没有删除、也没有清零，存储保持原样。
    BoundaryMissing,
}

/// 回退候选：user 消息的权威身份与**原始**正文。
///
/// `text` 与 `BaseMessage::content()` 同一口径，尚未做预览清洗——剥离
/// `system-reminder` 注入块、200 字符截断属于 ACP 展示层，不进入存储契约。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RewindCandidate {
    pub message_id: MessageId,
    pub text: String,
}

/// 回退候选的资格规则：`Human` 且剥离 legacy `<system-reminder>` 后仍有用户文本。
///
/// 返回**原始**正文（未剥离），调用方仍要按展示规则清洗；纯注入消息返回 `None`。
/// 这条规则是存储读取与 ACP 纯函数共用的唯一一份，避免两个 adapter 各自判断一次。
pub fn rewind_candidate_text(message: &BaseMessage) -> Option<String> {
    if !matches!(message, BaseMessage::Human { .. }) {
        return None;
    }
    let text = message.content();
    if strip_system_reminders(&text).trim().is_empty() {
        return None;
    }
    Some(text)
}

/// 从一个持久化条目投影回退候选；非消息/非 user/纯注入条目返回 `None`。
pub fn rewind_candidate(payload: &PersistedPayload) -> Option<RewindCandidate> {
    let message = payload.as_message()?;
    let text = rewind_candidate_text(message)?;
    Some(RewindCandidate {
        message_id: message.id(),
        text,
    })
}

/// 从完整逻辑上下文收集候选（默认回退实现）：最新在前，最多 `limit` 条。
///
/// `payloads` 的顺序与 [`super::SessionResources::load_session_history`] 一致
/// （继承区在前、自有区在后），因此逆序迭代先给出最新自有条目，再给出继承区条目。
pub fn collect_rewind_candidates(
    payloads: &[PersistedPayload],
    limit: usize,
) -> Vec<RewindCandidate> {
    payloads
        .iter()
        .rev()
        .filter_map(rewind_candidate)
        .take(limit)
        .collect()
}
