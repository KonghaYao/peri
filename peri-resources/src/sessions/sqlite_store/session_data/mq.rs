//! Thread MQ 的本机 SQLite 行为：入队、投影查询、取消、原子移交与清理。
//!
//! 写路径只有一种形状：`BEGIN IMMEDIATE` … `COMMIT`，读判定与写入在同一事务里，提交成功才
//! 返回「已接纳 / 已移交」。这不是实现细节——「先写历史再删 MQ」或反过来都会留下半移交，
//! 而 `docs/design/thread-mq.md` §5.4 要求数据库负责全有或全无。
//!
//! 判定顺序固定为「历史 → MQ → 插入」：历史事实优先于 MQ 事实（已移交的 ID 不会再入队），
//! 条件插入再挡一次与 Receive 的并发。

use super::*;
use crate::sessions::failure::conflict;
use crate::sessions::mq_rows::{
    decode_entry, enqueued_at_now, filter_entries, is_same_watermark, item_outcome, kind_token,
    payload_json, policy_json, same_payload, source_token, title_of, RawMqRow,
    DELETE_THREAD_MQ_ROW_SQL, INSERT_MESSAGE_SQL, INSERT_THREAD_MQ_SQL, SELECT_MESSAGE_ROW_SQL,
    SELECT_THREAD_MQ_BY_ID_SQL, SELECT_THREAD_MQ_SQL,
};
use peri_acp_types::session::{
    MessageSource, MqCancelOutcome, MqEnqueueDisposition, MqEnqueueOutcome, MqEntry, MqFilter,
    MqInput, MqReceiveDisposition, MqReceiveItem, MqReceiveItemOutcome, MqReceiveOutcome,
    MqReceiveSelection, MqReceiveStatus,
};
/// 单行读取的列（`thread_id` 在最前，用于跨 thread 判定）。
type MqRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
);

/// 列表读取的列（不带 `thread_id`：查询本身就按它过滤）。
type MqListRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
);

fn entry_of(row: &MqRow) -> SessionResourceResult<MqEntry> {
    decode_entry(RawMqRow {
        id: &row.1,
        enqueued_at: &row.2,
        kind: &row.3,
        source: &row.4,
        policy_json: &row.5,
        payload_json: &row.6,
        original_draft: row.7.as_deref(),
    })
}

fn listed_entry_of(row: &MqListRow) -> SessionResourceResult<MqEntry> {
    decode_entry(RawMqRow {
        id: &row.0,
        enqueued_at: &row.1,
        kind: &row.2,
        source: &row.3,
        policy_json: &row.4,
        payload_json: &row.5,
        original_draft: row.6.as_deref(),
    })
}

impl SqliteSessionData {
    pub(super) async fn enqueue_mq(
        &self,
        thread_id: &ThreadId,
        input: MqInput,
    ) -> SessionResourceResult<MqEnqueueOutcome> {
        self.writable()?;
        // MQ 与最终 own history 必须用同一个稳定 ID：载荷自带别的 id 会让「删除 MQ 行」
        // 与「历史里那一条」指向两个身份，移交后既查不回也去重不了。
        if input.payload.id() != input.id {
            return Err(invalid_input(
                "MQ payload id must match the queued message id",
            ));
        }
        let payload = payload_json(&input.payload)?;
        let policy = policy_json(&input.policy);
        let kind = kind_token(input.kind)?;
        let source = source_token(&input.source)?;
        let message_id = input.id.as_uuid().to_string();
        let incoming = RawMqRow {
            id: &message_id,
            enqueued_at: "",
            kind: &kind,
            source: &source,
            policy_json: &policy,
            payload_json: &payload,
            original_draft: input.original_draft.as_deref(),
        };
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        self.require_session(&mut tx, thread_id).await?;
        // 「历史 → MQ → 条件插入」一次走完：条件插入 0 行只可能来自并发 Receive 抢先
        // （本机写入由 `BEGIN IMMEDIATE` 串行化，跨进程的竞争者也被同一把锁挡住）。
        // 这时不重试出新行，而是回到事实判定。
        if let Some(outcome) = self.mq_enqueue_fact(&mut tx, thread_id, &incoming).await? {
            tx.commit()
                .await
                .map_err(|_| commit_failure(Some(thread_id.clone())))?;
            return Ok(outcome);
        }
        let enqueued_at = enqueued_at_now();
        let inserted = sqlx::query(INSERT_THREAD_MQ_SQL)
            .bind(thread_id.as_str())
            .bind(incoming.id)
            .bind(&enqueued_at)
            .bind(incoming.kind)
            .bind(incoming.source)
            .bind(incoming.policy_json)
            .bind(incoming.payload_json)
            .bind(incoming.original_draft)
            .execute(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        let outcome = if inserted.rows_affected() == 1 {
            MqEnqueueOutcome {
                disposition: MqEnqueueDisposition::Inserted,
                entry: decode_entry(RawMqRow {
                    enqueued_at: &enqueued_at,
                    ..incoming
                })?,
            }
        } else {
            // 判定的三条事实在插入时都不成立、插入又没落行：只能是对手在两次语句之间提交了
            // 同 ID。如实报告冲突，由调用方按稳定 ID 重读，不制造第二行。
            return Err(conflict(
                "MQ enqueue raced the same message id; read the id back before retrying",
            ));
        };
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(thread_id.clone())))?;
        Ok(outcome)
    }

    /// 入队的事实判定：已经成立的「同 ID」事实优先于本次插入。
    ///
    /// 返回 `None` 表示没有既有事实，调用方继续尝试条件插入。
    async fn mq_enqueue_fact(
        &self,
        connection: &mut SqliteConnection,
        thread_id: &ThreadId,
        incoming: &RawMqRow<'_>,
    ) -> SessionResourceResult<Option<MqEnqueueOutcome>> {
        if let Some((owner, _content)) = message_fact(connection, incoming.id).await? {
            if owner != thread_id.as_str() {
                return Err(conflict(
                    "message id already belongs to another thread's history",
                ));
            }
            return Ok(Some(MqEnqueueOutcome {
                disposition: MqEnqueueDisposition::AlreadyHandedOff,
                // 历史行不保存入队时间，也没有 MQ 行：这里回的是请求内容的投影。
                entry: decode_entry(RawMqRow {
                    enqueued_at: &enqueued_at_now(),
                    ..*incoming
                })?,
            }));
        }
        let Some(row) = mq_row_by_id(connection, incoming.id).await? else {
            return Ok(None);
        };
        if row.0 != thread_id.as_str() {
            return Err(conflict("message id already belongs to another thread"));
        }
        // 逐列比对**已序列化的形态**：两边的 token 由同一份序列化派生，文本相等即值相等。
        let same = row.3 == incoming.kind
            && row.4 == incoming.source
            && row.5 == incoming.policy_json
            && same_payload(&row.6, incoming.payload_json)
            && row.7.as_deref() == incoming.original_draft;
        if !same {
            return Err(conflict(
                "a different message already uses this id in the thread MQ",
            ));
        }
        Ok(Some(MqEnqueueOutcome {
            disposition: MqEnqueueDisposition::Existing,
            entry: entry_of(&row)?,
        }))
    }

    pub(super) async fn query_mq(
        &self,
        thread_id: &ThreadId,
        filter: MqFilter,
    ) -> SessionResourceResult<Vec<MqEntry>> {
        let rows: Vec<MqListRow> = sqlx::query_as(SELECT_THREAD_MQ_SQL)
            .bind(thread_id.as_str())
            .fetch_all(&self.database.pool)
            .await
            .map_err(|error| map_sqlx(&error))?;
        let entries = rows
            .iter()
            .map(listed_entry_of)
            .collect::<SessionResourceResult<Vec<_>>>()?;
        Ok(filter_entries(entries, &filter))
    }

    pub(super) async fn cancel_mq(
        &self,
        thread_id: &ThreadId,
        id: MessageId,
    ) -> SessionResourceResult<MqCancelOutcome> {
        self.writable()?;
        let message_id = id.as_uuid().to_string();
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        self.require_session(&mut tx, thread_id).await?;
        let Some(row) = mq_row_by_id(&mut tx, &message_id).await? else {
            // 没有 MQ 行时只有两种事实：已在历史（并发 Receive 已提交）或当前不存在。
            let status = match message_fact(&mut tx, &message_id).await? {
                Some((owner, _)) if owner == thread_id.as_str() => {
                    MqCancelOutcome::AlreadyHandedOff
                }
                _ => MqCancelOutcome::NotFound,
            };
            tx.commit()
                .await
                .map_err(|_| commit_failure(Some(thread_id.clone())))?;
            return Ok(status);
        };
        if row.0 != thread_id.as_str() {
            tx.commit()
                .await
                .map_err(|_| commit_failure(Some(thread_id.clone())))?;
            return Ok(MqCancelOutcome::NotFound);
        }
        let entry = entry_of(&row)?;
        if entry.source != MessageSource::UserInput {
            return Err(invalid_input(
                "only pending user input can be cancelled from the thread MQ",
            ));
        }
        // 删除条件带 `enqueued_at`：同 ID 被取消后重新入队的行不会被这次操作误删。
        let deleted = sqlx::query(DELETE_THREAD_MQ_ROW_SQL)
            .bind(thread_id.as_str())
            .bind(&message_id)
            .bind(&entry.enqueued_at)
            .execute(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        let outcome = if deleted.rows_affected() == 1 {
            MqCancelOutcome::Cancelled(entry)
        } else {
            MqCancelOutcome::NotFound
        };
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(thread_id.clone())))?;
        Ok(outcome)
    }

    pub(super) async fn receive_mq(
        &self,
        thread_id: &ThreadId,
        selection: MqReceiveSelection,
    ) -> SessionResourceResult<MqReceiveOutcome> {
        self.writable()?;
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        self.require_session(&mut tx, thread_id).await?;
        let mut items = Vec::with_capacity(selection.items.len());
        let mut inserted = Vec::new();
        for item in &selection.items {
            items.push(
                self.receive_item(&mut tx, thread_id, item, &mut inserted)
                    .await?,
            );
        }
        if !inserted.is_empty() {
            // 计数与更新时间是历史的派生事实：与本次写入同一事务，热态与冷恢复不会分叉。
            refresh_history_derivations(&mut tx, thread_id).await?;
            if let Some(title) = title_of(&inserted) {
                sqlx::query("UPDATE threads SET title = ?1 WHERE id = ?2 AND title IS NULL")
                    .bind(&title)
                    .bind(thread_id.as_str())
                    .execute(&mut *tx)
                    .await
                    .map_err(|error| map_sqlx(&error))?;
            }
        }
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(thread_id.clone())))?;
        Ok(MqReceiveOutcome { items })
    }

    async fn receive_item(
        &self,
        connection: &mut SqliteConnection,
        thread_id: &ThreadId,
        item: &MqReceiveItem,
        inserted: &mut Vec<MqEntry>,
    ) -> SessionResourceResult<MqReceiveItemOutcome> {
        let message_id = item.entry.id.as_uuid().to_string();
        let Some(row) = mq_row_by_id(&mut *connection, &message_id).await? else {
            return Ok(item_outcome(item.entry.id, MqReceiveStatus::Missing, None));
        };
        let entry = entry_of(&row)?;
        if row.0 != thread_id.as_str() || !is_same_watermark(&entry, &item.entry.watermark()) {
            // 选择基于旧快照：行已被取消/移交，或同 ID 重新入队。什么都不写。
            return Ok(item_outcome(item.entry.id, MqReceiveStatus::Missing, None));
        }
        let status = match item.disposition {
            MqReceiveDisposition::Suppress => MqReceiveStatus::Suppressed,
            MqReceiveDisposition::Commit => {
                match message_fact(&mut *connection, &message_id).await? {
                    Some((owner, content)) => {
                        if owner == thread_id.as_str()
                            && same_payload(&content, &payload_json(&entry.payload)?)
                        {
                            MqReceiveStatus::AlreadyPresent
                        } else {
                            // 身份冲突：删掉待发副本，但绝不把这次内容写进历史。
                            MqReceiveStatus::Conflict
                        }
                    }
                    None => {
                        let written = sqlx::query(INSERT_MESSAGE_SQL)
                            .bind(&message_id)
                            .bind(thread_id.as_str())
                            .bind(crate::sessions::canonical::payload_role(&entry.payload))
                            .bind(payload_json(&entry.payload)?)
                            .execute(&mut *connection)
                            .await
                            .map_err(|error| map_sqlx(&error))?;
                        if written.rows_affected() == 1 {
                            inserted.push(entry.clone());
                            MqReceiveStatus::Committed
                        } else {
                            MqReceiveStatus::Conflict
                        }
                    }
                }
            }
        };
        let deleted = sqlx::query(DELETE_THREAD_MQ_ROW_SQL)
            .bind(thread_id.as_str())
            .bind(&message_id)
            .bind(&entry.enqueued_at)
            .execute(&mut *connection)
            .await
            .map_err(|error| map_sqlx(&error))?;
        if deleted.rows_affected() != 1 {
            // `BEGIN IMMEDIATE` 下读到的行不可能在事务内被别的写者删掉。真的发生 0 行时，
            // Commit 历史已经写入，不能再报 Missing 让调用方重试；Suppress 没有写入，按缺失。
            return Ok(item_outcome(
                item.entry.id,
                if status == MqReceiveStatus::Suppressed {
                    MqReceiveStatus::Missing
                } else {
                    status
                },
                if status == MqReceiveStatus::Suppressed {
                    None
                } else {
                    Some(entry)
                },
            ));
        }
        Ok(item_outcome(item.entry.id, status, Some(entry)))
    }

    /// 本地 child 终态结算：终态、工作意图与结果接纳共用一次 `BEGIN IMMEDIATE`。
    ///
    /// 三步分开提交会留下「子任务已结束、父会话没收到结果」：中途崩溃后终态已写、
    /// 结果永远不落库（`docs/design/session-durability.md` §5.3）。
    pub(super) async fn finalize_child_terminal_mq(
        &self,
        child: &ThreadId,
        status: peri_acp_types::thread::AgentStatus,
        work_state: peri_acp_types::thread::WorkState,
        target: &ThreadId,
        input: MqInput,
    ) -> SessionResourceResult<MqEnqueueOutcome> {
        self.writable()?;
        if input.payload.id() != input.id {
            return Err(invalid_input(
                "MQ payload id must match the queued message id",
            ));
        }
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        self.require_session(&mut tx, child).await?;
        // 幂等判据是「这次结算是否已落地」，不是「状态是否相同」：同状态但不同
        // 正文的 panic 兜底重放也必须被吞掉。判定与写入同在本次 `BEGIN IMMEDIATE`
        // 内，ID 只按主键等值查询（`thread_mq.id` / `messages.message_id`）。
        let message_id = input.id.as_uuid().to_string();
        let current = stored_agent_status(&mut tx, child).await?;
        if current.is_terminal() && settlement_landed(&mut tx, target, &message_id).await? {
            tx.commit()
                .await
                .map_err(|_| commit_failure(Some(child.clone())))?;
            return Ok(settled_without_write(input));
        }
        if target != child {
            self.require_session(&mut tx, target).await?;
        }
        let payload = payload_json(&input.payload)?;
        let policy = policy_json(&input.policy);
        let kind = kind_token(input.kind)?;
        let source = source_token(&input.source)?;
        let incoming = RawMqRow {
            id: &message_id,
            enqueued_at: "",
            kind: &kind,
            source: &source,
            policy_json: &policy,
            payload_json: &payload,
            original_draft: input.original_draft.as_deref(),
        };
        // 终态：与 `update_meta(status)` 同列同时间戳语义，只是不再单独提交。
        sqlx::query("UPDATE threads SET agent_status = ?1, updated_at = ?2 WHERE id = ?3")
            .bind(status.as_str())
            .bind(peri_time::now_utc_rfc3339())
            .bind(child.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        session_rows::write_work_state_on(&mut tx, child, work_state)
            .await
            .map_err(write_failure)?;
        // 结果接纳：与 `enqueue_mq` 同一判定顺序（历史 → MQ → 条件插入）。
        let outcome = match self.mq_enqueue_fact(&mut tx, target, &incoming).await? {
            Some(outcome) => outcome,
            None => {
                let enqueued_at = enqueued_at_now();
                let inserted = sqlx::query(INSERT_THREAD_MQ_SQL)
                    .bind(target.as_str())
                    .bind(incoming.id)
                    .bind(&enqueued_at)
                    .bind(incoming.kind)
                    .bind(incoming.source)
                    .bind(incoming.policy_json)
                    .bind(incoming.payload_json)
                    .bind(incoming.original_draft)
                    .execute(&mut *tx)
                    .await
                    .map_err(|error| map_sqlx(&error))?;
                if inserted.rows_affected() != 1 {
                    return Err(conflict(
                        "MQ enqueue raced the same message id; read the id back before retrying",
                    ));
                }
                MqEnqueueOutcome {
                    disposition: MqEnqueueDisposition::Inserted,
                    entry: decode_entry(RawMqRow {
                        enqueued_at: &enqueued_at,
                        ..incoming
                    })?,
                }
            }
        };
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(child.clone())))?;
        Ok(outcome)
    }

    /// 关闭结算：写终态提醒与删除 MQ 行共用一次 `BEGIN IMMEDIATE`。
    ///
    /// 两次独立提交会留下「提醒已写、待发行还在」或反过来的半完成状态；这里失败即
    /// 全部不生效，下一次 close 从同一份事实重试。
    pub(super) async fn settle_retained_mq_rows(
        &self,
        thread_id: &ThreadId,
        reminders: &[peri_acp_types::session::RetainedReminder],
        rows: &[MqEntry],
    ) -> SessionResourceResult<()> {
        self.writable()?;
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        self.require_session(&mut tx, thread_id).await?;
        for reminder in reminders {
            self.append_terminal_reminder_on(&mut tx, thread_id, reminder.id, &reminder.reminder)
                .await?;
        }
        for row in rows {
            let message_id = row.id.as_uuid().to_string();
            let Some(current) = mq_row_by_id(&mut tx, &message_id).await? else {
                // 并发 Receive 已结算：与 `receive` 的 `Missing` 同义，不是失败。
                continue;
            };
            let entry = entry_of(&current)?;
            if current.0 != thread_id.as_str() || !is_same_watermark(&entry, &row.watermark()) {
                continue;
            }
            sqlx::query(DELETE_THREAD_MQ_ROW_SQL)
                .bind(thread_id.as_str())
                .bind(&message_id)
                .bind(&entry.enqueued_at)
                .execute(&mut *tx)
                .await
                .map_err(|error| map_sqlx(&error))?;
        }
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(thread_id.clone())))?;
        Ok(())
    }

    pub(super) async fn clear_mq(&self, thread_id: &ThreadId) -> SessionResourceResult<()> {
        self.writable()?;
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        self.require_session(&mut tx, thread_id).await?;
        sqlx::query(crate::sessions::canonical::DELETE_THREAD_MQ_BY_THREAD_SQL)
            .bind(thread_id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(thread_id.clone())))?;
        Ok(())
    }
}

async fn mq_row_by_id(
    connection: &mut SqliteConnection,
    id: &str,
) -> SessionResourceResult<Option<MqRow>> {
    sqlx::query_as(SELECT_THREAD_MQ_BY_ID_SQL)
        .bind(id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| map_sqlx(&error))
}

pub(super) async fn message_fact(
    connection: &mut SqliteConnection,
    id: &str,
) -> SessionResourceResult<Option<(String, String)>> {
    sqlx::query_as(SELECT_MESSAGE_ROW_SQL)
        .bind(id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| map_sqlx(&error))
}

/// 当前 `threads.agent_status`；调用方必须落在同一个事务里（判定与写入同一快照）。
async fn stored_agent_status(
    connection: &mut SqliteConnection,
    id: &ThreadId,
) -> SessionResourceResult<peri_acp_types::thread::AgentStatus> {
    let row: Option<(String,)> = sqlx::query_as("SELECT agent_status FROM threads WHERE id = ?1")
        .bind(id.as_str())
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| map_sqlx(&error))?;
    let Some((status,)) = row else {
        return Err(not_found());
    };
    status
        .parse()
        .map_err(|_| corrupt("stored agent status is not readable"))
}

/// 按稳定消息 ID 判断这次结算是否已经落在目标 thread：MQ 行或历史行恰好一处成立。
///
/// 只做主键等值查询，不扫描；调用方必须已在同一事务内。
async fn settlement_landed(
    connection: &mut SqliteConnection,
    target: &ThreadId,
    message_id: &str,
) -> SessionResourceResult<bool> {
    if mq_row_by_id(&mut *connection, message_id)
        .await?
        .is_some_and(|row| row.0 == target.as_str())
    {
        return Ok(true);
    }
    Ok(message_fact(&mut *connection, message_id)
        .await?
        .is_some_and(|(owner, _)| owner == target.as_str()))
}

/// 另一个终态已经写下时的成功结果：本次没有写入任何行，entry 只是请求投影。
fn settled_without_write(input: MqInput) -> MqEnqueueOutcome {
    MqEnqueueOutcome {
        disposition: MqEnqueueDisposition::Existing,
        entry: MqEntry {
            id: input.id,
            enqueued_at: enqueued_at_now(),
            kind: input.kind,
            source: input.source,
            payload: input.payload,
            policy: input.policy,
            original_draft: input.original_draft,
        },
    }
}
