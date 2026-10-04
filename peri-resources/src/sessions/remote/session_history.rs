//! 远程会话历史写入：追加、投影/flags、compact、rewind、精确移除。
//!
//! 形状与 [`super::session_write`] 一致：**一次端口调用 = 一个托管事务批**（资格先于效果），
//! 因此不存在「一半历史生效」的中间可见状态。
//!
//! ## 批内守卫：为什么不能只靠「受影响行数」
//!
//! 远端语句是静态 SQL + 绑定参数，引擎不会因为 `UPDATE ... WHERE` 匹配 0 行而失败——
//! 只看提交后的行数就等于让「目标不存在」以成功收场。这里的做法是**批内守卫语句**：
//! `peri_store_meta` 是单行表（`CHECK (singleton = 0)`），谓词成立时这条
//! `INSERT ... SELECT` 命中主键冲突，整批随之回滚。它不新增表、不新增失败类别，
//! 也不留下可观察的半状态。
//!
//! 守卫负责「整批不生效」，**错误类别**按本机 adapter 的归类给出：
//! 前置一致读把「不属于本会话」「不存在」「会话不存在」分成不同结果，守卫保证读与写之间
//! 有第三方改动时仍然不半成功（那一类会以 `Unknown`/未决上报，不降级成成功）。
//!
//! ## 派生规则
//!
//! `message_count` 是存储列，按本机同一规则**重数**（不是自增）；`content_size` 不落列，
//! 读取时由投影现算。
//! 远端没有这个消费者，因此历史变更不产生缓存失效动作。

use std::collections::HashSet;

use peri_acp_types::messages::{BaseMessage, MessageId};
use peri_acp_types::session_resources::{RewindBoundary, SessionResourceResult};
use peri_acp_types::store::{
    deserialize_persisted_payload, serialize_persisted_payload, CompactionChange, MessageFlags,
    PersistedPayload,
};
use peri_acp_types::system_reminder::TrustedSystemReminder;
use peri_acp_types::thread::ThreadId;
use turso_serverless::Value;

use super::session_codec as codec;
use super::session_data::{invalid_input, not_found, RemoteSessionData};
use super::sql::{int_at, StatementSpec};
use crate::sessions::canonical;
use crate::sessions::sqlite_store::role_of_message;

// ─── 批内守卫 ─────────────────────────────────────────────────────────────────

/// 会话不存在：中止整批（归 `NotFound`）。
const GUARD_SESSION_ABSENT_SQL: &str = "INSERT INTO peri_store_meta(singleton)
    SELECT 0 WHERE NOT EXISTS (SELECT 1 FROM threads WHERE id = ?1)";

/// 目标条目不属于本会话（不存在，或属于别的会话）：中止整批。
///
/// `pub(super)` 只为了让云端机制实验用**生产同一条语句**验证守卫在真引擎上的两条分支
/// （谓词成立 → 整批回滚；不成立 → 一行都不插）。
pub(super) const GUARD_MESSAGE_NOT_IN_SESSION_SQL: &str = "INSERT INTO peri_store_meta(singleton)
    SELECT 0 WHERE NOT EXISTS (
        SELECT 1 FROM messages WHERE thread_id = ?1 AND message_id = ?2)";

/// 条目存在但属于别的会话：中止整批（「精确移除」不得跨会话命中）。
const GUARD_MESSAGE_FOREIGN_SQL: &str = "INSERT INTO peri_store_meta(singleton)
    SELECT 0 WHERE EXISTS (
        SELECT 1 FROM messages WHERE message_id = ?1 AND thread_id <> ?2)";

// ─── 效果语句 ─────────────────────────────────────────────────────────────────

/// 追加一条历史行：列清单与 `session_sql::INSERT_MESSAGE_SQL`（以及本机 `messages` 的插入）
/// 同形，`role` 由 canonical payload 派生。
///
/// canonical 顺序由 `rowid` 承载：同批内语句按顺序执行，插入序即历史序，不需要
/// 「先读序号再写」的往返，也不需要远端曾经那列显式 `ordinal`。
const APPEND_MESSAGE_SQL: &str = "INSERT INTO messages
    (message_id, thread_id, role, content, truncated, excluded, projection)
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";

/// 改写一条条目的 flags（投影与 compact 的 flag 更新共用同一形状）。
pub(super) const UPDATE_FLAGS_SQL: &str = "UPDATE messages
    SET truncated = ?1, excluded = ?2, projection = ?3
    WHERE thread_id = ?4 AND message_id = ?5";

/// 重数派生计数并推进 `updated_at`（与本机 `refresh_history_derivations` 同一规则，
/// 同一条语句在两种执行器上执行）。
const REFRESH_COUNTS_SQL: &str = "UPDATE threads SET updated_at = ?1,
    message_count = (SELECT COUNT(*) FROM messages WHERE thread_id = ?2)
    WHERE id = ?2";

/// 自动标题：只在标题仍缺失时补一次（本机同一规则：`title IS NULL` 才写）。
const SET_TITLE_IF_ABSENT_SQL: &str =
    "UPDATE threads SET title = ?1 WHERE id = ?2 AND title IS NULL";

/// rewind 两个显式边界：比较按 `rowid`（canonical 历史顺序的载体，与本机同一句形态）。
const REWIND_KEEP_THROUGH_SQL: &str = "DELETE FROM messages WHERE thread_id = ?1 AND rowid > ?2";
const REWIND_REMOVE_FROM_SQL: &str = "DELETE FROM messages WHERE thread_id = ?1 AND rowid >= ?2";

/// 精确移除单条条目。
const REMOVE_MESSAGE_SQL: &str = "DELETE FROM messages WHERE thread_id = ?1 AND message_id = ?2";

// ─── 前置一致读（只读） ────────────────────────────────────────────────────────

const SELECT_MESSAGE_OWNER_SQL: &str = "SELECT thread_id FROM messages WHERE message_id = ?1";
const SELECT_ROWID_SQL: &str =
    "SELECT rowid FROM messages WHERE thread_id = ?1 AND message_id = ?2";

impl RemoteSessionData {
    /// 全局消息主键承担并发去重；冲突后的精确读区分重放与内容碰撞。
    pub(super) async fn write_reminder_if_absent(
        &self,
        id: &ThreadId,
        message_id: MessageId,
        reminder: &TrustedSystemReminder,
    ) -> SessionResourceResult<bool> {
        let payload = PersistedPayload::SystemReminder {
            id: message_id,
            reminder: reminder.clone(),
        };
        let content = payload_content(&payload)?;
        if let Some(existing) = self.read_reminder_row(message_id).await? {
            return check_reminder_row(id, &content, existing).map(|()| false);
        }
        match self.write_history_append(id, &[payload]).await {
            Ok(()) => Ok(true),
            Err(error) => match self.read_reminder_row(message_id).await? {
                Some(existing) => check_reminder_row(id, &content, existing).map(|()| false),
                None => Err(error),
            },
        }
    }

    async fn read_reminder_row(
        &self,
        message_id: MessageId,
    ) -> SessionResourceResult<Option<(String, String)>> {
        let statement = StatementSpec::new(
            "SELECT thread_id, content FROM messages WHERE message_id = ?1",
            vec![Value::Text(message_label(message_id))],
        );
        let store = self.store().await?;
        let Some(row) = store.fetch_row(&statement).await? else {
            return Ok(None);
        };
        let [Value::Text(owner), Value::Text(content), ..] = row.as_slice() else {
            return Err(codec::corrupt("invalid reminder row"));
        };
        Ok(Some((owner.clone(), content.clone())))
    }

    /// 追加 canonical payload 批次：顺序稳定、计数重数、自动标题按本机同一规则补齐。
    ///
    /// 冲突语义与本机一致：批次内重复 id 在发请求前拒绝（`InvalidInput`，同一文案）；
    /// 与已有行（含别会话的行）撞全局主键由约束拒绝整批，同样归 `InvalidInput`
    /// （本机走唯一键冲突映射）。会话不存在则 `NotFound`。
    pub(super) async fn write_history_append(
        &self,
        id: &ThreadId,
        payloads: &[PersistedPayload],
    ) -> SessionResourceResult<()> {
        if payloads.is_empty() {
            return Ok(());
        }
        let mut seen = HashSet::with_capacity(payloads.len());
        for payload in payloads {
            if !seen.insert(payload.id()) {
                return Err(invalid_input("history batch repeats a message id"));
            }
        }
        if !self.exists(id).await? {
            return Err(not_found());
        }
        let contents = payloads
            .iter()
            .map(payload_content)
            .collect::<SessionResourceResult<Vec<_>>>()?;
        let entries = payloads
            .iter()
            .zip(&contents)
            .map(|(payload, content)| (payload.id(), content.clone()))
            .collect::<Vec<_>>();
        let mut effects = Vec::with_capacity(payloads.len() + 2);
        for (payload, (message, content)) in payloads.iter().zip(&entries) {
            effects.push(append_statement(
                id,
                *message,
                content,
                canonical::payload_role(payload),
            ));
        }
        if let Some(title) = title_statement(id, payloads) {
            effects.push(title);
        }
        effects.push(refresh_statement(id, &timestamp()));
        let mut inputs = vec![id.as_str().to_owned()];
        inputs.extend(entry_inputs(&entries));
        self.commit_effects("append_history", &inputs, effects, id)
            .await
            .map(|_| ())
    }

    /// 应用投影/flags 变更集：整批生效或整批不生效。
    ///
    /// 目标必须全部是本会话的条目（本机同一归类：`InvalidInput`），一个都不改一半。
    pub(super) async fn write_projections(
        &self,
        id: &ThreadId,
        updates: &[(MessageId, MessageFlags)],
    ) -> SessionResourceResult<()> {
        if updates.is_empty() {
            return Ok(());
        }
        let messages = updates
            .iter()
            .map(|(message, _)| *message)
            .collect::<Vec<_>>();
        if !self
            .missing_session_messages(id, &messages)
            .await?
            .is_empty()
        {
            return Err(invalid_input(
                "projection target is not a history entry of this session",
            ));
        }
        let mut effects = Vec::with_capacity(updates.len() * 2 + 1);
        let mut inputs = vec![id.as_str().to_owned()];
        for (message, flags) in updates {
            effects.push(guard_message_statement(id, *message));
            effects.push(flag_statement(id, *message, flags));
            inputs.push(flags_label(*message, flags));
        }
        effects.push(refresh_statement(id, &timestamp()));
        self.commit_effects("apply_message_projections", &inputs, effects, id)
            .await
            .map(|_| ())
    }

    /// 应用一次 compaction 变更：flags、追加条目与派生计数一起生效或一起不生效。
    ///
    /// `flag_updates` 的目标必须都在本会话里。本机把「目标不在会话内」折成 `Corrupt`
    /// （`write_failure` 的兜底归类），这里**照实对齐**，不在远端自创类别：若要改归类，
    /// 应同时改两个 adapter，而不是让远端先分叉。
    pub(super) async fn write_compaction_change(
        &self,
        id: &ThreadId,
        change: &CompactionChange,
    ) -> SessionResourceResult<()> {
        let flagged = change
            .flag_updates
            .iter()
            .map(|(message, _)| *message)
            .collect::<Vec<_>>();
        if !self
            .missing_session_messages(id, &flagged)
            .await?
            .is_empty()
        {
            return Err(codec::corrupt(
                "compaction flag target is not a history entry of this session",
            ));
        }
        let appended = change
            .appended_messages
            .iter()
            .map(appended_content)
            .collect::<SessionResourceResult<Vec<_>>>()?;
        let mut effects = Vec::with_capacity(change.flag_updates.len() * 2 + appended.len() + 2);
        // 计数重数与标题补齐都作用在会话行上：会话不存在要明确失败，不让 0 行更新成成功。
        effects.push(guard_session_statement(id));
        let mut inputs = vec![id.as_str().to_owned()];
        for (message, flags) in &change.flag_updates {
            effects.push(flag_statement(id, *message, flags));
            inputs.push(flags_label(*message, flags));
        }
        let entries = change
            .appended_messages
            .iter()
            .zip(&appended)
            .map(|(message, content)| (message.id(), content.clone()))
            .collect::<Vec<_>>();
        for (message, (message_id, content)) in change.appended_messages.iter().zip(&entries) {
            effects.push(append_statement(
                id,
                *message_id,
                content,
                role_of_message(message),
            ));
        }
        inputs.extend(entry_inputs(&entries));
        effects.push(refresh_statement(id, &timestamp()));
        self.commit_effects("apply_compaction", &inputs, effects, id)
            .await
            .map(|_| ())
    }

    /// 按显式边界 rewind。
    ///
    /// 边界不在本会话里时**保持无变更并成功**——这是本机的既有语义（未知截止点不动历史），
    /// 不是「静默忽略失败」。边界存在时以它的 `rowid` 为界：`KeepThrough` 保留到该条为止，
    /// `RemoveFrom` 从该条起删除，随后重数计数。
    pub(super) async fn write_rewind(
        &self,
        id: &ThreadId,
        boundary: RewindBoundary,
    ) -> SessionResourceResult<()> {
        let message = boundary.message_id();
        let Some(rowid) = self.boundary_rowid(id, message).await? else {
            return Ok(());
        };
        let (sql, direction) = match boundary {
            RewindBoundary::KeepThrough(_) => (REWIND_KEEP_THROUGH_SQL, "keep_through"),
            RewindBoundary::RemoveFrom(_) => (REWIND_REMOVE_FROM_SQL, "remove_from"),
        };
        let effects = vec![
            StatementSpec::new(
                sql,
                vec![Value::Text(id.as_str().to_owned()), codec::int_value(rowid)],
            ),
            refresh_statement(id, &timestamp()),
        ];
        // 方向进摘要：同一个边界上的 `KeepThrough` 与 `RemoveFrom` 是**两个操作**，
        // 只按 (会话, 边界, rowid) 摘要会让后者撞上前者的操作 id，被当成重放静默跳过。
        let inputs = vec![
            id.as_str().to_owned(),
            direction.to_owned(),
            message_label(message),
            rowid.to_string(),
        ];
        self.commit_effects("rewind_history", &inputs, effects, id)
            .await
            .map(|_| ())
    }

    /// 按 id 集合精确移除历史条目。
    ///
    /// 与本机同一组语义：不存在的条目是**幂等删除**（不报错），属于别的会话的条目是
    /// `InvalidInput`（不静默跳过，也不跨会话删除）；去重后一次成批。
    pub(super) async fn write_history_removal(
        &self,
        id: &ThreadId,
        ids: &[MessageId],
    ) -> SessionResourceResult<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let mut seen = HashSet::with_capacity(ids.len());
        let unique = ids
            .iter()
            .copied()
            .filter(|message| seen.insert(*message))
            .collect::<Vec<_>>();
        let owners = self.message_owners(&unique).await?;
        for (_, owner) in unique.iter().zip(&owners) {
            if owner.as_deref().is_some_and(|owner| owner != id.as_str()) {
                return Err(invalid_input("history entry belongs to another session"));
            }
        }
        let mut effects = Vec::with_capacity(unique.len() * 2 + 1);
        let mut inputs = vec![id.as_str().to_owned()];
        for message in &unique {
            effects.push(StatementSpec::new(
                GUARD_MESSAGE_FOREIGN_SQL,
                vec![
                    Value::Text(message_label(*message)),
                    Value::Text(id.as_str().to_owned()),
                ],
            ));
            effects.push(StatementSpec::new(
                REMOVE_MESSAGE_SQL,
                vec![
                    Value::Text(id.as_str().to_owned()),
                    Value::Text(message_label(*message)),
                ],
            ));
            inputs.push(message_label(*message));
        }
        effects.push(refresh_statement(id, &timestamp()));
        self.commit_effects("remove_history_entries", &inputs, effects, id)
            .await
            .map(|_| ())
    }

    /// 本会话里缺失的消息 id（不存在，或属于别的会话）。
    async fn missing_session_messages(
        &self,
        id: &ThreadId,
        ids: &[MessageId],
    ) -> SessionResourceResult<Vec<MessageId>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let owners = self.message_owners(ids).await?;
        Ok(ids
            .iter()
            .zip(&owners)
            .filter(|(_, owner)| owner.as_deref() != Some(id.as_str()))
            .map(|(message, _)| *message)
            .collect())
    }

    /// 一次只读请求读回每个消息 id 的归属会话。
    async fn message_owners(
        &self,
        ids: &[MessageId],
    ) -> SessionResourceResult<Vec<Option<String>>> {
        let statements = ids
            .iter()
            .map(|message| {
                StatementSpec::new(
                    SELECT_MESSAGE_OWNER_SQL,
                    vec![Value::Text(message_label(*message))],
                )
            })
            .collect::<Vec<_>>();
        let store = self.store().await?;
        let batches = store.read_batch(statements).await?;
        Ok(batches
            .into_iter()
            .map(|mut rows| {
                rows.pop()
                    .and_then(|mut row| row.pop())
                    .and_then(|value| match value {
                        Value::Text(owner) => Some(owner),
                        _ => None,
                    })
            })
            .collect())
    }

    /// 边界条目在本会话内的 `rowid`；不存在时为 `None`。
    async fn boundary_rowid(
        &self,
        id: &ThreadId,
        message: MessageId,
    ) -> SessionResourceResult<Option<i64>> {
        let store = self.store().await?;
        let row = store
            .fetch_row(&StatementSpec::new(
                SELECT_ROWID_SQL,
                vec![
                    Value::Text(id.as_str().to_owned()),
                    Value::Text(message_label(message)),
                ],
            ))
            .await?;
        Ok(row.and_then(|values| int_at(&values, 0)))
    }
}

fn check_reminder_row(
    id: &ThreadId,
    content: &str,
    existing: (String, String),
) -> SessionResourceResult<()> {
    if existing.0 == id.as_str() && existing.1 == content {
        Ok(())
    } else {
        Err(invalid_input(
            "reminder id conflicts with another history entry",
        ))
    }
}

// ─── 语句组装 ─────────────────────────────────────────────────────────────────

/// 追加语句：`?1` 消息 id、`?2` 会话 id、`?3` role、`?4` 内容、`?5..?7` 默认 flags。
fn append_statement(id: &ThreadId, message: MessageId, content: &str, role: &str) -> StatementSpec {
    StatementSpec::new(
        APPEND_MESSAGE_SQL,
        vec![
            Value::Text(message_label(message)),
            Value::Text(id.as_str().to_owned()),
            Value::Text(role.to_owned()),
            Value::Text(content.to_owned()),
            codec::int_value(0),
            codec::int_value(0),
            Value::Null,
        ],
    )
}

/// flags 更新语句（`projection` 为已序列化 JSON 或 NULL）。
fn flag_statement(id: &ThreadId, message: MessageId, flags: &MessageFlags) -> StatementSpec {
    StatementSpec::new(
        UPDATE_FLAGS_SQL,
        vec![
            codec::int_value(i64::from(flags.truncated)),
            codec::int_value(i64::from(flags.excluded)),
            codec::optional_text(encode_projection(flags).as_deref()),
            Value::Text(id.as_str().to_owned()),
            Value::Text(message_label(message)),
        ],
    )
}

fn guard_session_statement(id: &ThreadId) -> StatementSpec {
    StatementSpec::new(
        GUARD_SESSION_ABSENT_SQL,
        vec![Value::Text(id.as_str().to_owned())],
    )
}

fn guard_message_statement(id: &ThreadId, message: MessageId) -> StatementSpec {
    StatementSpec::new(
        GUARD_MESSAGE_NOT_IN_SESSION_SQL,
        vec![
            Value::Text(id.as_str().to_owned()),
            Value::Text(message_label(message)),
        ],
    )
}

fn refresh_statement(id: &ThreadId, now: &str) -> StatementSpec {
    StatementSpec::new(
        REFRESH_COUNTS_SQL,
        vec![
            Value::Text(now.to_owned()),
            Value::Text(id.as_str().to_owned()),
        ],
    )
}

/// 自动标题语句：没有可用的首条 Human 文本时返回 `None`（不写、也不假装写入）。
///
/// 提取规则直接复用本机 adapter 的同一份纯规则（`extract_title`），不在这里重写一遍。
fn title_statement(id: &ThreadId, payloads: &[PersistedPayload]) -> Option<StatementSpec> {
    let messages = payloads
        .iter()
        .filter_map(PersistedPayload::as_message)
        .cloned()
        .collect::<Vec<_>>();
    // 领域纯规则：直接调用本机 adapter 用的同一份 `extract_title`，不复制一份到远端。
    let title = crate::sessions::sqlite_store::row_mapping::extract_title(&messages)?;
    Some(StatementSpec::new(
        SET_TITLE_IF_ABSENT_SQL,
        vec![Value::Text(title), Value::Text(id.as_str().to_owned())],
    ))
}

// ─── 编码与标签 ───────────────────────────────────────────────────────────────

/// 追加内容的持久化形态：复用 `peri_acp_types::store` 的 envelope，远端不另写格式。
fn payload_content(payload: &PersistedPayload) -> SessionResourceResult<String> {
    serialize_persisted_payload(payload).map_err(|_| corrupt_payload())
}

/// compact 追加的是领域消息：先按同一 envelope 定型，读不回来就在发请求前拒绝。
fn appended_content(message: &BaseMessage) -> SessionResourceResult<String> {
    let json = serde_json::to_string(message).map_err(|_| corrupt_payload())?;
    deserialize_persisted_payload(&json).map_err(|_| corrupt_payload())?;
    Ok(json)
}

fn corrupt_payload() -> peri_acp_types::session_resources::SessionResourceError {
    codec::corrupt("history entry is not serializable")
}

fn encode_projection(flags: &MessageFlags) -> Option<String> {
    flags
        .projection
        .as_ref()
        .and_then(|projection| serde_json::to_string(projection).ok())
}

/// 操作摘要里的 flags 标签：保证「同一内容重试命中同一操作 id」。
fn flags_label(message: MessageId, flags: &MessageFlags) -> String {
    let bits = format!(
        "{}{}{}",
        u8::from(flags.truncated),
        u8::from(flags.excluded),
        u8::from(flags.projection.is_some())
    );
    format!("{}:flags:{bits}", message_label(message))
}

fn message_label(message: MessageId) -> String {
    message.as_uuid().to_string()
}

/// 历史条目的操作身份输入：**消息 id 与内容都要进摘要**。
///
/// 只按内容摘要会出现「同内容的两批追加撞同一个操作 id」：第二次会被当成重放静默跳过，
/// 而追加本来不是幂等操作（每次追加都是新的条目）。id 进摘要后，重试同一批（同一批
/// payload）仍然命中同一个操作 id，重放语义不受影响。
fn entry_inputs(entries: &[(MessageId, String)]) -> Vec<String> {
    let mut inputs = Vec::with_capacity(entries.len() * 2);
    for (message, content) in entries {
        inputs.push(message_label(*message));
        inputs.push(content.clone());
    }
    inputs
}

fn timestamp() -> String {
    peri_time::now_utc_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每条语句的占位符个数必须与它绑定的参数个数一致：这是「静态 SQL + 全绑定」的可核对形式。
    fn placeholders(sql: &str) -> usize {
        sql.matches('?').count()
    }

    /// 值只能走绑定参数：SQL 文本里不得出现字符串字面量（那才是把内容拼进语句）。
    fn has_no_inline_literal(sql: &str) -> bool {
        !sql.contains('\'')
    }

    #[test]
    fn every_statement_is_static_and_fully_bound() {
        let cases: [(&str, usize); 10] = [
            (GUARD_SESSION_ABSENT_SQL, 1),
            (GUARD_MESSAGE_NOT_IN_SESSION_SQL, 2),
            (GUARD_MESSAGE_FOREIGN_SQL, 2),
            // 追加语句里 `?2`（会话 id）被主查询与取序号的子查询共用，因此是 7 个占位符。
            (APPEND_MESSAGE_SQL, 7),
            (UPDATE_FLAGS_SQL, 5),
            // 同一次「重数 + 推进时间戳」里 `?2` 出现两次。
            (REFRESH_COUNTS_SQL, 3),
            (SET_TITLE_IF_ABSENT_SQL, 2),
            (REWIND_KEEP_THROUGH_SQL, 2),
            (REMOVE_MESSAGE_SQL, 2),
            (SELECT_ROWID_SQL, 2),
        ];
        for (sql, expected) in cases {
            assert_eq!(placeholders(sql), expected, "绑定量与占位符不一致: {sql}");
            assert!(has_no_inline_literal(sql), "语句里出现了字面量: {sql}");
        }
    }

    /// 守卫必须落在**单行表**上：`peri_store_meta` 的主键冲突才是「整批回滚」的触发点。
    #[test]
    fn guards_abort_the_whole_batch_via_single_row_primary_key() {
        for sql in [
            GUARD_SESSION_ABSENT_SQL,
            GUARD_MESSAGE_NOT_IN_SESSION_SQL,
            GUARD_MESSAGE_FOREIGN_SQL,
        ] {
            assert!(
                sql.starts_with("INSERT INTO peri_store_meta(singleton)"),
                "{sql}"
            );
            assert!(sql.contains("SELECT 0 WHERE"), "{sql}");
        }
        // 会话缺失：谓词是「不存在」；跨会话命中：谓词是「存在且属于别人」。
        assert!(GUARD_SESSION_ABSENT_SQL.contains("NOT EXISTS"));
        assert!(GUARD_MESSAGE_NOT_IN_SESSION_SQL.contains("thread_id = ?1 AND message_id = ?2"));
        assert!(GUARD_MESSAGE_FOREIGN_SQL.contains("thread_id <> ?2"));
    }

    /// 两个 rewind 边界只差一个比较符：`KeepThrough` 保留到该条、`RemoveFrom` 从该条起删。
    #[test]
    fn rewind_boundaries_differ_only_in_the_comparison() {
        assert!(REWIND_KEEP_THROUGH_SQL.ends_with("rowid > ?2"));
        assert!(REWIND_REMOVE_FROM_SQL.ends_with("rowid >= ?2"));
    }

    /// 操作身份必须区分「同内容的两批追加」：只按内容摘要会让第二批被当成重放静默跳过。
    #[test]
    fn append_identity_inputs_include_message_ids() {
        let first = MessageId::new();
        let second = MessageId::new();
        let left = entry_inputs(&[(first, "same content".to_owned())]);
        let right = entry_inputs(&[(second, "same content".to_owned())]);
        assert_ne!(left, right, "同内容不同消息 id 必须是不同的操作身份");
        assert!(left.contains(&message_label(first)));
        assert!(left.contains(&"same content".to_owned()));
    }

    /// 追加语句与本机 `messages` 的插入同形：列清单一致、`role` 显式写入，
    /// 顺序交给 `rowid`（不再有显式序号列，也就不需要「先读序号再写」）。
    #[test]
    fn append_matches_the_canonical_message_insert() {
        assert!(APPEND_MESSAGE_SQL.contains("(message_id, thread_id, role, content"));
        assert!(!APPEND_MESSAGE_SQL.contains("ordinal"));
        assert!(!APPEND_MESSAGE_SQL.contains("MAX("));
    }

    /// 计数是**重数**而不是自增：任何一条历史路径都不会把计数带偏。
    #[test]
    fn counts_are_recomputed_not_incremented() {
        assert!(REFRESH_COUNTS_SQL.contains("message_count = (SELECT COUNT(*)"));
        assert!(!REFRESH_COUNTS_SQL.contains("message_count + 1"));
    }

    /// 自动标题只在缺失时补齐：已有标题永远不会被后来的追加改写。
    #[test]
    fn title_is_only_written_when_absent() {
        assert!(SET_TITLE_IF_ABSENT_SQL.contains("AND title IS NULL"));
    }
}
