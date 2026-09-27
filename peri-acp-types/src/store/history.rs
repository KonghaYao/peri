//! 会话历史纯变换 — fork / compact / 投影 / rewind 的领域规则唯一实现。
//!
//! 这里的函数不读时钟、不生成 UUID、不访问环境与数据库：新 ID、时间戳与持久化
//! 一律由调用方以显式输入提供（fork 的 ID 分配经 `allocate_id` 注入）。同一输入
//! 必然得到同一输出，因此 ACP、transcript 与资源 adapter 可以共用同一份规则，
//! 而不是各自复制一份「差不多」的实现。
//!
//! 这里只做变换，不做 I/O，也不决定事务边界：整体生效/不生效由调用这些函数的
//! adapter 负责（SQLite 用同一事务，远程 adapter 见其自身保证）。

use std::collections::{HashMap, HashSet};

use crate::messages::{BaseMessage, MessageId};
use crate::projection::MessageProjectionDirective;
use crate::session_resources::RewindBoundary;
use crate::store::{CompactionChange, MessageFlags, PersistedPayload};

/// 纯变换的输入无法解释，或与既有历史冲突。
///
/// 调用方必须在产生任何副作用之前处理这些错误：它们都表示「没做任何事」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HistoryError {
    /// rewind/删除的边界目标不在给定历史中。
    #[error("history target {id:?} is not present")]
    TargetMissing { id: MessageId },
    /// 追加的 payload id 已存在，或同一批次内重复。
    #[error("history payload id {id:?} already exists or repeats within the batch")]
    DuplicateId { id: MessageId },
}

/// fork 目标历史：payload 与 flags 都已完成 ID 重映射。
#[derive(Clone, Debug, Default)]
pub struct ForkedHistory {
    pub payloads: Vec<PersistedPayload>,
    pub flags: HashMap<MessageId, MessageFlags>,
}

/// rewind 结果：保留的 payload 与此前存在、现在被移除的 id（按历史顺序）。
#[derive(Clone, Debug)]
pub struct RewoundHistory {
    pub kept: Vec<PersistedPayload>,
    pub removed: Vec<MessageId>,
}

/// 复制 source 的 payload/flags 到 fork 目标，逐条分配新 ID 并重写投影引用。
///
/// - `allocate_id` 注入新 ID：生产用 `MessageId::new()`，测试可用计数器，本函数
///   自身不产生随机性。
/// - flags 只复制「payload 确实存在」的条目；投影条目里指向被复制消息的引用
///   一并改写到新 ID，否则目标会话会引用 source 的消息。
/// - source 只读，本函数不修改任何输入。
pub fn remap_fork_history(
    source_payloads: &[PersistedPayload],
    source_flags: &HashMap<MessageId, MessageFlags>,
    mut allocate_id: impl FnMut() -> MessageId,
) -> ForkedHistory {
    let mut payloads = Vec::with_capacity(source_payloads.len());
    let mut id_map: HashMap<MessageId, MessageId> = HashMap::with_capacity(source_payloads.len());
    for source in source_payloads {
        let source_id = source.id();
        let forked = match source {
            PersistedPayload::Message(message) => {
                PersistedPayload::Message(message.clone().with_message_id(allocate_id()))
            }
            PersistedPayload::SystemReminder { reminder, .. } => PersistedPayload::SystemReminder {
                id: allocate_id(),
                reminder: reminder.clone(),
            },
        };
        id_map.insert(source_id, forked.id());
        payloads.push(forked);
    }
    let flags = source_payloads
        .iter()
        .filter_map(|source| {
            let source_id = source.id();
            let flags = source_flags.get(&source_id)?.clone();
            let forked_id = id_map[&source_id];
            Some((forked_id, remap_projection_references(flags, &id_map)))
        })
        .collect();
    ForkedHistory { payloads, flags }
}

/// 投影 flag 规则：设置 directive 的同时标记 `truncated`（Micro compact 语义）。
///
/// 保留 `excluded` 等既有标记：投影与排除是两个独立事实，不互相清除。
pub fn flags_with_projection(
    existing: &MessageFlags,
    directive: MessageProjectionDirective,
) -> MessageFlags {
    let mut flags = existing.clone();
    flags.truncated = true;
    flags.projection = Some(directive);
    flags
}

/// 批量应用 flags 变更：默认 flags 表示「无标记」，从视图移除而不是存入默认值。
///
/// 顺序即后写覆盖先写；同一 id 在批次内出现多次时以最后一次为准。
pub fn apply_flag_updates(
    flags: &mut HashMap<MessageId, MessageFlags>,
    updates: &[(MessageId, MessageFlags)],
) {
    for (id, value) in updates {
        if *value == MessageFlags::default() {
            flags.remove(id);
        } else {
            flags.insert(*id, value.clone());
        }
    }
}

/// 追加前的 ID 冲突检测：已知 id 或批次内重复都必须在副作用前失败。
///
/// `is_known` 由调用方给出自己的视图（transcript 用内存索引，adapter 用持久化事实）。
pub fn ensure_distinct_ids(
    payloads: &[PersistedPayload],
    is_known: impl Fn(MessageId) -> bool,
) -> Result<(), HistoryError> {
    let mut seen = HashSet::with_capacity(payloads.len());
    for payload in payloads {
        let id = payload.id();
        if !seen.insert(id) || is_known(id) {
            return Err(HistoryError::DuplicateId { id });
        }
    }
    Ok(())
}

/// rewind 边界对应的保留长度（不含移除部分）。
///
/// `KeepThrough` 保留目标本身（transcript rewind），`RemoveFrom` 移除目标及以后
/// （用户 rewind）。两个语义不可合并：合并会把「保留到目标」失真成「删掉目标」。
pub fn rewind_keep_len(ids: &[MessageId], boundary: RewindBoundary) -> Result<usize, HistoryError> {
    let target = boundary.message_id();
    let index = ids
        .iter()
        .position(|id| *id == target)
        .ok_or(HistoryError::TargetMissing { id: target })?;
    Ok(match boundary {
        RewindBoundary::KeepThrough(_) => index + 1,
        RewindBoundary::RemoveFrom(_) => index,
    })
}

/// 按边界裁剪 canonical payload，返回保留部分与被移除的 id。
///
/// 不修改输入：调用方拿到结果后再决定如何提交（内存替换 / 持久化）。
pub fn apply_rewind(
    payloads: &[PersistedPayload],
    boundary: RewindBoundary,
) -> Result<RewoundHistory, HistoryError> {
    let ids: Vec<MessageId> = payloads.iter().map(PersistedPayload::id).collect();
    let keep_len = rewind_keep_len(&ids, boundary)?;
    Ok(RewoundHistory {
        kept: payloads[..keep_len].to_vec(),
        removed: ids[keep_len..].to_vec(),
    })
}

/// 把一次 compaction 变更应用到 canonical 历史视图。
///
/// 追加消息的 id 必须与既有历史不冲突（同一 id 已存在或批次内重复即失败），
/// flags 更新按 [`apply_flag_updates`] 的规则合并。返回 `Err` 时输入未被修改。
pub fn apply_compaction_change(
    payloads: &mut Vec<PersistedPayload>,
    flags: &mut HashMap<MessageId, MessageFlags>,
    change: &CompactionChange,
) -> Result<(), HistoryError> {
    let appended = appended_payloads(&change.appended_messages);
    {
        let known: HashSet<MessageId> = payloads.iter().map(PersistedPayload::id).collect();
        ensure_distinct_ids(&appended, |id| known.contains(&id))?;
    }
    apply_flag_updates(flags, &change.flag_updates);
    payloads.extend(appended);
    Ok(())
}

/// 改写 flags 中指向已复制消息的投影条目。
fn remap_projection_references(
    mut flags: MessageFlags,
    id_map: &HashMap<MessageId, MessageId>,
) -> MessageFlags {
    if let Some(projection) = flags.projection.as_mut() {
        for entry in &mut projection.entries {
            if let Some(remapped) = id_map.get(&entry.message_id) {
                entry.message_id = *remapped;
            }
        }
    }
    flags
}

/// 追加消息的 canonical payload 形式（供只需要 payload 视图的调用方复用）。
pub fn appended_payloads(messages: &[BaseMessage]) -> Vec<PersistedPayload> {
    messages
        .iter()
        .map(|message| PersistedPayload::Message(message.clone()))
        .collect()
}

#[cfg(test)]
#[path = "history_test.rs"]
mod tests;
