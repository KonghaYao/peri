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
use crate::messages::MessageId;
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
