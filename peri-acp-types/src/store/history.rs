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

impl ForkedHistory {
    /// 有效视图：已排除条目不在其中（与 `ActiveSessionSnapshot::payloads` 同一规则）。
    ///
    /// 复制结果本身保留完整 canonical 历史；只有发布到内存会话状态的那份集合
    /// 取有效视图，否则 Full Compact 释放的正文会随 fork 回到执行上下文。
    pub fn active_payloads(&self) -> Vec<PersistedPayload> {
        self.payloads
            .iter()
            .filter(|payload| {
                !self
                    .flags
                    .get(&payload.id())
                    .is_some_and(|flags| flags.excluded)
            })
            .cloned()
            .collect()
    }
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
/// - flags 只复制「payload 确实存在」的条目；投影里的消息引用（渲染条目与
///   `superseded_by` 取代关系）一并改写到新 ID，否则目标会话会引用 source 的消息。
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

/// rewind 有效视图重建的条目视图：id、flags 与「是否 System 角色消息」。
///
/// System 不参与排除（Full Compact 明确跳过它们），因此判定 excluded 段时也跳过。
pub type RewindFlagEntry = (MessageId, MessageFlags, bool);

/// rewind 后必须清零 `excluded` 的条目 id（按历史顺序）。
///
/// 输入是 rewind 后**保留的前缀**（canonical 顺序）与它的长度 `keep_len`；输出只列
/// `excluded` 要从 `true` 改回 `false` 的 id。本函数不读也不写 `truncated` 与 `projection`
/// 载荷：清零是否连带清掉载荷由存储路径决定（两侧都会清，见
/// [`crate::projection::MessageProjectionDirective::superseded_by`]），调用方不得把
/// 其余 flag 位当成「无标记」覆盖回去。
///
/// # 依据：每条排除的取代关系
///
/// Full Compact 每次提交都排除「当时所有未被排除的 own 非 System 条目」，再在尾部追加一条
/// 不被排除的摘要（见 `compact_v2/full.rs`），并在同一批里给每条被排除的条目写下**取代它的
/// 那条摘要 id**（`MessageFlags::superseded_by()`）。本次 rewind 保留前缀之后，一条排除
/// 是否仍有依据只取决于**它的那条摘要是否还在保留前缀里**：
///
/// - 摘要仍在 ⇒ 排除依旧有依据：被取代的原文不进有效视图。
/// - 摘要已被本次 rewind 删掉 ⇒ 依据消失：清零该条的 `excluded`，原文重新有效。
///
/// 这个判定逐条成立，因此两次以上 Full 之后回退到排除段内部时，**早先批次**的排除会连同
/// 它自己的摘要一起保留下来，不会出现「旧摘要与其覆盖的原文同时有效」的重复覆盖
/// （A19「无原文／摘要重复覆盖」）。
///
/// # 未记录取代关系的历史行（保守回退）
///
/// 取代关系落地之前归档的条目没有这个记录，无法逐条判定。对它们沿用原有的**前缀结构**
/// 判定：设保留前缀的 own 非 System 条目里最长 excluded 前缀长度为 k，位置 k 上存在非
/// System 条目（按 Full 的不变量，它是一条仍然有效的摘要）⇒ 前 k 条的排除仍有效；
/// 位置 k 上不存在 ⇒ 摘要已被本次 rewind 删除，前缀内所有 `excluded` 一律清零。
/// 该回退只在证据缺失时生效，方向始终是「不丢内容」（宁可让原文生效，也不把它留在视图外）。
///
/// # 调用方契约
///
/// 只把**本会话 own 历史**交给本函数，且必须携带被排除条目的 flags：非 System 的祖先条目
/// 虽不被排除，但它们的来源是继承上下文（父快照），不在本会话的 own 历史里；首个祖先条目的
/// 不排除会直接终结领导段。
///
/// `keep_len` 必须不超过 `entries.len()`：调用方由 [`rewind_keep_len`] 在同一份历史上取得，
/// 越界即调用方违约（切片越界会直接暴露），这里不静默截断。
pub fn rewind_clear_excluded_ids(entries: &[RewindFlagEntry], keep_len: usize) -> Vec<MessageId> {
    let retained = &entries[..keep_len];
    let retained_ids: HashSet<MessageId> = retained.iter().map(|(id, _, _)| *id).collect();
    // 领导段长度与「位置 k 上是否有存活的摘要」在**同一次**遍历里得出：
    // take_while 会吃掉那个不满足谓词的条目，分两步写就会丢位置。只服务无取代记录的行。
    let mut covered = 0usize;
    let mut summary_survives = false;
    for (_, flags, is_system) in retained {
        if *is_system {
            continue;
        }
        if !flags.excluded {
            summary_survives = true;
            break;
        }
        covered += 1;
    }
    let mut clear = Vec::new();
    let mut own_position = 0usize;
    for (id, flags, is_system) in retained {
        if *is_system {
            continue;
        }
        if flags.excluded {
            let still_covered = match flags.superseded_by() {
                // 精确判定：取代它的摘要仍在保留前缀里，排除依旧有依据。
                Some(summary) => retained_ids.contains(&summary),
                // 未记录取代关系：沿用前缀结构判定（保守，见上文）。
                None => summary_survives && own_position < covered,
            };
            if !still_covered {
                clear.push(*id);
            }
        }
        own_position += 1;
    }
    clear
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

/// 改写 flags 中指向已复制消息的投影引用：渲染条目与取代关系。
///
/// 取代关系也是消息引用：`superseded_by` 若留在 source 的摘要上，目标会话的排除判定就锚在
/// 别的会话——source rewind 掉那条摘要后，目标会误判自己的排除失去依据并复活旧正文。
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
        if let Some(summary) = projection.superseded_by.as_mut() {
            if let Some(remapped) = id_map.get(summary) {
                *summary = *remapped;
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
