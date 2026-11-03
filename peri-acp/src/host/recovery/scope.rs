//! 恢复范围的纯分区：根、子候选与「某 thread 的直接 working 子任务」。
//!
//! 这里不读存储、不建收件地址：把「哪些 thread 是候选、哪些是某个 thread 的直接子」
//! 从协调器里分出来，使边界判定可以单独断言。

use peri_acp_types::recovery::{RecoveryCandidate, RecoveryScope};
use peri_acp_types::thread::ThreadId;

/// 一次进入的范围分区（候选均已在存储侧过滤为 `working`）。
pub(super) struct RecoveryPartition {
    pub(super) root_thread_id: ThreadId,
    /// 根自身仍为 `working` 时的候选；否则根不启动模型。
    pub(super) root: Option<RecoveryCandidate>,
    /// 根之外的候选，保持存储返回的创建时间顺序。
    pub(super) children: Vec<RecoveryCandidate>,
}

impl RecoveryPartition {
    /// `thread_id` 的直接 `working` 子候选：提醒的任务列表只写这些。
    pub(super) fn direct_children_of(&self, thread_id: &ThreadId) -> Vec<ThreadId> {
        self.children
            .iter()
            .filter(|candidate| candidate.parent_thread_id.as_ref() == Some(thread_id))
            .map(|candidate| candidate.thread_id.clone())
            .collect()
    }
}

/// 按根切分范围：根候选与其余候选分开，顺序不变。
pub(super) fn partition(scope: RecoveryScope) -> RecoveryPartition {
    let mut root = None;
    let mut children = Vec::new();
    for candidate in scope.candidates {
        if candidate.thread_id == scope.root_thread_id {
            root = Some(candidate);
        } else {
            children.push(candidate);
        }
    }
    RecoveryPartition {
        root_thread_id: scope.root_thread_id,
        root,
        children,
    }
}

#[cfg(test)]
#[path = "scope_test.rs"]
mod tests;
