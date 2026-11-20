//! Full 后首个 Reason 的恢复入口重算。
//!
//! 硬记忆正文在压缩采样时写入，其中的工具名与调用形式只反映**采样时**的目录。
//! 首个 Reason 的工具目录还要经过 `before_reason_catalog`、动态工具发布与最终
//! 权限过滤，可能与采样时不同（手动 `/compact` 甚至没有目录）。这里在最终请求的
//! 派生视图上按当次目录重算可执行入口，不改 canonical transcript、不写 flags：
//!
//! - 有结构化事实且作用域相同：按最终目录重新渲染整段正文；
//! - 缺结构化事实（旧记录）、作用域无法证明或事实为空：保留观察值，追加校正说明，
//!   明确“上文的工具名可能已失效、id 与状态仍是采样值”；
//! - 运行时 revision 已推进：追加说明，要求先消费对应完成回执。

use peri_acp_types::messages::{BaseMessage, MessageContent, MessageId};
use peri_acp_types::projection::RecoveryMemoryProjection;

use crate::agent::compact_v2::hard_memory::{
    is_hard_memory_content, HardMemorySnapshot, RecoveryTools,
};
use crate::session::transcript::MessageTranscript;

/// 旧硬记忆（无结构化事实或作用域无法证明）在派生请求中的校正说明。
///
/// 只说“上文可能过期”，不替换 id 与状态：这些值仍是采样时的观察记录，不能凭
/// 当前目录重写，也不能伪装成实时状态。
pub const STALE_RECOVERY_HINT_CORRECTION: &str = "[Recovery hint correction: the tool names recorded above were captured under an earlier tool catalog and may be unavailable now. Resolve each name in the current catalog before calling. The recorded ids and statuses above are the values captured at compaction, not a live check. No call above is promised executable.]";

/// 采样后任务注册表已推进时的说明（结论仍以真实回执为准）。
pub const ADVANCED_SNAPSHOT_NOTE: &str = "[Recovery snapshot note: the runtime task registry advanced after this snapshot was captured, so a listed task may have finished. Consume the matching completion receipt for that task before acting on it; do not infer its outcome from this snapshot.]";

/// 一次派生请求的恢复入口重算结果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecoveryRefreshReport {
    /// 按最终目录重算并替换正文的硬记忆条数。
    pub rendered: usize,
    /// 追加校正说明的硬记忆条数（旧记录或作用域不可证明）。
    pub corrected: usize,
    /// 采样后运行时已推进、追加说明的条数。
    pub advanced: usize,
}

/// 请求里是否存在硬记忆消息；决定是否需要读取运行时 revision。
///
/// 只读首块文本的首行：普通历史不因这次检查产生整份正文副本。
#[must_use]
pub fn has_hard_memory(messages: &[BaseMessage]) -> bool {
    messages.iter().any(is_hard_memory_message)
}

/// 硬记忆只由 Harness 以纯文本（或单个文本块）写入；其余形态按未知跳过。
fn is_hard_memory_message(message: &BaseMessage) -> bool {
    let BaseMessage::Human { content, .. } = message else {
        return false;
    };
    match content {
        MessageContent::Text(text) => is_hard_memory_content(text),
        MessageContent::Blocks(blocks) => blocks
            .first()
            .and_then(|block| block.as_text())
            .is_some_and(is_hard_memory_content),
        MessageContent::Raw(_) => false,
    }
}

/// 在最终请求消息上重算硬记忆的可执行入口。
///
/// 返回 `Some(messages)` 仅当至少一条硬记忆正文需要改变；内容不变时返回 `None`，
/// 让目录未变的常见路径保持请求字节稳定。
#[must_use]
pub fn refresh_recovery_memory(
    messages: &[BaseMessage],
    transcript: &MessageTranscript,
    tools: &RecoveryTools,
    scope: Option<&str>,
    live_revision: Option<u64>,
) -> (Option<Vec<BaseMessage>>, RecoveryRefreshReport) {
    let mut report = RecoveryRefreshReport::default();
    let mut rebuilt: Option<Vec<BaseMessage>> = None;
    for (index, message) in messages.iter().enumerate() {
        let BaseMessage::Human { id, .. } = message else {
            continue;
        };
        if !is_hard_memory_message(message) {
            continue;
        }
        let body = message.content();
        let projection = transcript
            .projection_ref(*id)
            .and_then(|directive| directive.recovery_memory.as_deref());
        let from_facts = projection.is_some_and(|projection| same_runtime_scope(projection, scope));
        // 结构化事实渲染不出正文（空快照）时同样不可核实：保留原观察值并校正，
        // 不能因为持久化数据残缺就删掉采样时写下的任务记录。
        let rendered = projection
            .filter(|_| from_facts)
            .and_then(|projection| rendered_body(projection, tools, live_revision, &mut report));
        let (replacement, verified) = match rendered {
            Some(rendered) => (rendered, true),
            None => {
                report.corrected += 1;
                (format!("{body}\n\n{STALE_RECOVERY_HINT_CORRECTION}"), false)
            }
        };
        if replacement == body {
            continue;
        }
        if verified {
            report.rendered += 1;
        }
        if let Some(target) = rebuilt
            .get_or_insert_with(|| messages.to_vec())
            .get_mut(index)
        {
            *target = replace_body(*id, replacement);
        }
    }
    (rebuilt, report)
}

/// 按最终目录重渲染；事实为空时退回校正说明（不删除无法核实的正文）。
fn rendered_body(
    projection: &RecoveryMemoryProjection,
    tools: &RecoveryTools,
    live_revision: Option<u64>,
    report: &mut RecoveryRefreshReport,
) -> Option<String> {
    let rendered = HardMemorySnapshot::from_projection(projection).render_with(tools)?;
    if live_revision.is_some_and(|live| live != projection.sampled_revision) {
        report.advanced += 1;
        return Some(format!("{rendered}\n\n{ADVANCED_SNAPSHOT_NOTE}"));
    }
    Some(rendered)
}

/// 只有两侧都记录且相等时才证明同一运行时；缺任一侧都按旧记录处理。
fn same_runtime_scope(projection: &RecoveryMemoryProjection, scope: Option<&str>) -> bool {
    matches!(
        (projection.scope.as_deref(), scope),
        (Some(recorded), Some(current)) if recorded == current
    )
}

fn replace_body(id: MessageId, body: String) -> BaseMessage {
    BaseMessage::Human {
        id,
        content: MessageContent::text(body),
    }
}

#[cfg(test)]
#[path = "recovery_refresh_test.rs"]
mod tests;
