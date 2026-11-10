//! Thread MQ 的本机 SQLite 行为：入队、投影查询、取消、原子移交与清理。
//!
//! 写路径只有一种形状：`BEGIN IMMEDIATE` … `COMMIT`，读判定与写入在同一事务里，提交成功才
//! 返回「已接纳 / 已移交」。这不是实现细节——「先写历史再删 MQ」或反过来都会留下半移交，
//! 而 `docs/design/thread-mq.md` §5.4 要求数据库负责全有或全无。
//!
//! 判定顺序固定为「历史 → MQ → 插入」：历史事实优先于 MQ 事实（已移交的 ID 不会再入队），
//! 条件插入再挡一次与 Receive 的并发。

use super::*;
use crate::sessions::failure::{conflict, corrupt};
use crate::sessions::mq_rows::{
    decode_entry, decode_header, enqueued_at_now, is_same_watermark, item_outcome, kind_token,
    payload_json, policy_ensures_processing, policy_json, same_payload, source_token, title_of,
    RawMqHeader, RawMqRow, DELETE_THREAD_MQ_ROW_SQL, INSERT_MESSAGE_SQL, INSERT_THREAD_MQ_SQL,
    SELECT_MESSAGE_ROW_SQL, SELECT_THREAD_MQ_BY_ID_SQL, SELECT_THREAD_MQ_POLICIES_SQL,
};
use peri_acp_types::session::{
    MessageSource, MqCancelOutcome, MqEnqueueDisposition, MqEnqueueOutcome, MqEntry, MqFilter,
    MqHeader, MqInput, MqReceiveDisposition, MqReceiveItem, MqReceiveItemOutcome, MqReceiveOutcome,
    MqReceiveSelection, MqReceiveStatus, MqWatermark,
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

/// 控制面读取的列：没有 `payload_json`。
type MqHeaderRow = (String, String, String, String, String);

/// 把 `MqFilter` 翻译成 SQL：过滤与上限都在数据库侧完成，正文查询不会先取全表。
///
/// 顺序键与 [`MqWatermark`] 一致：`(enqueued_at, id)` 字典序对固定格式时间与
/// 小写 UUID 就等于消费顺序。
fn filtered_mq_select(
    columns: &str,
    filter: &MqFilter,
) -> SessionResourceResult<(String, Vec<String>)> {
    let mut sql = format!("SELECT {columns} FROM thread_mq WHERE thread_id = ?1");
    let mut binds: Vec<String> = Vec::new();
    if let Some(after) = &filter.after {
        sql.push_str(" AND (enqueued_at > ?2 OR (enqueued_at = ?2 AND id > ?3))");
        binds.push(after.enqueued_at.clone());
        binds.push(after.id.as_uuid().to_string());
    }
    if filter.user_input_only {
        let placeholder = binds.len() + 2;
        sql.push_str(&format!(" AND source = ?{placeholder}"));
        binds.push(source_token(&MessageSource::UserInput)?);
    }
    if !filter.sources.is_empty() {
        let placeholders = (0..filter.sources.len())
            .map(|offset| format!("?{}", binds.len() + offset + 2))
            .collect::<Vec<_>>()
            .join(", ");
        sql.push_str(&format!(" AND source IN ({placeholders})"));
        for source in &filter.sources {
            binds.push(source_token(source)?);
        }
    }
    if !filter.kinds.is_empty() {
        let placeholders = (0..filter.kinds.len())
            .map(|offset| format!("?{}", binds.len() + offset + 2))
            .collect::<Vec<_>>()
            .join(", ");
        sql.push_str(&format!(" AND kind IN ({placeholders})"));
        for kind in &filter.kinds {
            binds.push(kind_token(*kind)?);
        }
    }
    sql.push_str(" ORDER BY enqueued_at, id");
    if let Some(limit) = filter.limit {
        sql.push_str(&format!(" LIMIT {limit}"));
    }
    Ok((sql, binds))
}

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

/// 结算批内的结果接纳：与 [`INSERT_THREAD_MQ_SQL`] 同列同条件，额外要求 child 仍 `active`。
///
/// 0 行同时表示「结果 ID 已落地」「child 已终态」或「结果 ID 属于别的 thread」，
/// 调用方按既有事实归类，不能把 0 行当成成功写入。
const INSERT_TERMINAL_RESULT_MQ_SQL: &str = "INSERT INTO thread_mq
    (thread_id, id, enqueued_at, kind, source, policy_json, payload_json, original_draft)
    SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8
    WHERE NOT EXISTS (SELECT 1 FROM messages WHERE message_id = ?2)
      AND NOT EXISTS (SELECT 1 FROM thread_mq WHERE id = ?2)
      AND EXISTS (SELECT 1 FROM threads WHERE id = ?1)
      AND EXISTS (SELECT 1 FROM threads WHERE id = ?9 AND agent_status = 'active')";

/// 结算批内的接收方提升：只认本次结果在目标 MQ 里的待发行，且 child 仍 `active`。
///
/// 结果投递回 child 自己时由调用方跳过，避免把刚写的终态重新提升为 `working`。
const PROMOTE_RECEIVER_ON_TERMINAL_SQL: &str = "UPDATE threads SET work_state = 'working'
    WHERE id = ?1 AND work_state = 'idle'
      AND EXISTS (SELECT 1 FROM thread_mq WHERE thread_id = ?1 AND id = ?2)
      AND EXISTS (SELECT 1 FROM threads WHERE id = ?3 AND agent_status = 'active')";

/// 终态 + 工作意图一次写入；`agent_status = 'active'` 保证同一 child 的终态只写一次。
const SETTLE_CHILD_TERMINAL_SQL: &str = "UPDATE threads
    SET agent_status = ?1, updated_at = ?2, work_state = ?3
    WHERE id = ?4 AND agent_status = 'active'";

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
        let ensures_processing = input.policy.ensures_processing();
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
            // 已移交历史的消息不再要求继续工作：重复接纳不能把它重新提升为 working。
            let pending = outcome.disposition != MqEnqueueDisposition::AlreadyHandedOff;
            self.maintain_work_intent_on_accept(&mut tx, thread_id, ensures_processing && pending)
                .await?;
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
        self.maintain_work_intent_on_accept(&mut tx, thread_id, ensures_processing)
            .await?;
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(thread_id.clone())))?;
        Ok(outcome)
    }

    /// 接纳维护目标工作意图：只有 `EnsureProcessing` 授予执行资格，且只把 `idle`
    /// 提升为 `working`——`stopped` 不被被动回调解除（设计 §5.2）。
    ///
    /// 终态 thread 不参与提升：它的 `working` 只是旧执行残留，迟到的接纳不能再把它
    /// 变成「待续跑」；显式 resume 走认领写入，不经过这里。
    async fn maintain_work_intent_on_accept(
        &self,
        connection: &mut SqliteConnection,
        thread_id: &ThreadId,
        ensures_processing: bool,
    ) -> SessionResourceResult<()> {
        if !ensures_processing {
            return Ok(());
        }
        sqlx::query(
            "UPDATE threads SET work_state = 'working'
             WHERE id = ?1 AND work_state = 'idle' AND agent_status = 'active'",
        )
        .bind(thread_id.as_str())
        .execute(&mut *connection)
        .await
        .map_err(|error| map_sqlx(&error))?;
        Ok(())
    }

    /// turn 收尾：没有待处理的 `EnsureProcessing` 条目才把 `working` 收敛为 `idle`。
    ///
    /// 判定与写入在同一次 `BEGIN IMMEDIATE` 内，避免覆盖刚到的消息；`stopped` 不在
    /// 条件里，收尾不能解除显式停止（设计 §5.1）。返回是否真的写入。
    pub(super) async fn settle_work_state_after_turn_mq(
        &self,
        thread_id: &ThreadId,
    ) -> SessionResourceResult<bool> {
        self.writable()?;
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        self.require_session(&mut tx, thread_id).await?;
        if pending_ensures_processing(&mut tx, thread_id).await? {
            tx.commit()
                .await
                .map_err(|_| commit_failure(Some(thread_id.clone())))?;
            return Ok(false);
        }
        let updated = sqlx::query(
            "UPDATE threads SET work_state = 'idle' WHERE id = ?1 AND work_state = 'working'",
        )
        .bind(thread_id.as_str())
        .execute(&mut *tx)
        .await
        .map_err(|error| map_sqlx(&error))?;
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(thread_id.clone())))?;
        Ok(updated.rows_affected() == 1)
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
        let (sql, binds) = filtered_mq_select(
            "id, enqueued_at, kind, source, policy_json, payload_json, original_draft",
            &filter,
        )?;
        let mut query = sqlx::query_as::<_, MqListRow>(sqlx::AssertSqlSafe(sql));
        query = query.bind(thread_id.as_str());
        for bind in binds {
            query = query.bind(bind);
        }
        let rows = query
            .fetch_all(&self.database.pool)
            .await
            .map_err(|error| map_sqlx(&error))?;
        rows.iter().map(listed_entry_of).collect()
    }

    /// 控制面投影：只读 id/时间/来源/策略列，不接触正文。
    pub(super) async fn query_mq_headers(
        &self,
        thread_id: &ThreadId,
        filter: MqFilter,
    ) -> SessionResourceResult<Vec<MqHeader>> {
        let (sql, binds) =
            filtered_mq_select("id, enqueued_at, kind, source, policy_json", &filter)?;
        let mut query = sqlx::query_as::<_, MqHeaderRow>(sqlx::AssertSqlSafe(sql));
        query = query.bind(thread_id.as_str());
        for bind in binds {
            query = query.bind(bind);
        }
        let rows = query
            .fetch_all(&self.database.pool)
            .await
            .map_err(|error| map_sqlx(&error))?;
        rows.iter()
            .map(|row| {
                decode_header(RawMqHeader {
                    id: &row.0,
                    enqueued_at: &row.1,
                    kind: &row.2,
                    source: &row.3,
                    policy_json: &row.4,
                })
            })
            .collect()
    }

    /// 行数：`COUNT(*)` 不读正文。
    pub(super) async fn count_mq(&self, thread_id: &ThreadId) -> SessionResourceResult<usize> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM thread_mq WHERE thread_id = ?1")
            .bind(thread_id.as_str())
            .fetch_one(&self.database.pool)
            .await
            .map_err(|error| map_sqlx(&error))?;
        usize::try_from(count).map_err(|_| corrupt("thread MQ count is not representable"))
    }

    /// 存在性：SQL `EXISTS`，不读正文也不计数。
    pub(super) async fn exists_mq(&self, thread_id: &ThreadId) -> SessionResourceResult<bool> {
        let exists: i64 =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM thread_mq WHERE thread_id = ?1)")
                .bind(thread_id.as_str())
                .fetch_one(&self.database.pool)
                .await
                .map_err(|error| map_sqlx(&error))?;
        Ok(exists != 0)
    }

    /// 最大水位：只读 `(enqueued_at, id)` 两列并取一条。
    pub(super) async fn watermark_mq(
        &self,
        thread_id: &ThreadId,
    ) -> SessionResourceResult<Option<MqWatermark>> {
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT enqueued_at, id FROM thread_mq WHERE thread_id = ?1
             ORDER BY enqueued_at DESC, id DESC LIMIT 1",
        )
        .bind(thread_id.as_str())
        .fetch_optional(&self.database.pool)
        .await
        .map_err(|error| map_sqlx(&error))?;
        row.map(|(enqueued_at, id)| {
            let id = id
                .parse::<uuid::Uuid>()
                .map(peri_acp_types::messages::MessageId::from)
                .map_err(|_| corrupt("MQ row has an invalid message id"))?;
            Ok(MqWatermark { enqueued_at, id })
        })
        .transpose()
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
    ///
    /// 批内顺序与远端一致：结果插入 → 接收方意图提升 → child 终态。插入与终态更新
    /// 都要求 child 仍 `active`，同一 child 的终态因此只写一次；批前读取只做快速
    /// no-op，批后按插入行数与既有事实归类。
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
            // 结算已落地，但这条 child 可能是经只写 `agent_status` 的旧路径收尾的：
            // 收敛残留的 `working`，幂等重放不会把它留成待续跑。
            converge_terminal_work_state(&mut tx, child, current).await?;
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
        let ensures_processing = input.policy.ensures_processing();
        let kind = kind_token(input.kind)?;
        let source = source_token(&input.source)?;
        let enqueued_at = enqueued_at_now();
        let incoming = RawMqRow {
            id: &message_id,
            enqueued_at: &enqueued_at,
            kind: &kind,
            source: &source,
            policy_json: &policy,
            payload_json: &payload,
            original_draft: input.original_draft.as_deref(),
        };
        // 结果插入在前：0 行同时表示「结果已落地」「child 已终态」或「ID 跨 thread
        // 冲突」。后续写入都以 `active` 与结果行存在为前提，后到的结算不能覆盖终态或
        // 重复接纳；两张 `NOT EXISTS` 同时挡住别的 thread 已占用的结果 ID。
        let inserted = sqlx::query(INSERT_TERMINAL_RESULT_MQ_SQL)
            .bind(target.as_str())
            .bind(incoming.id)
            .bind(incoming.enqueued_at)
            .bind(incoming.kind)
            .bind(incoming.source)
            .bind(incoming.policy_json)
            .bind(incoming.payload_json)
            .bind(incoming.original_draft)
            .bind(child.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        let outcome = if inserted.rows_affected() == 1 {
            Some(MqEnqueueOutcome {
                disposition: MqEnqueueDisposition::Inserted,
                entry: decode_entry(incoming)?,
            })
        } else {
            // 0 行不是成功：先按既有事实归类。跨 thread 冲突在这里返回；同 ID 已落地
            // 返回 `AlreadyHandedOff`/`Existing`；都没有且 child 已终态则本次是 no-op。
            let fact = self.mq_enqueue_fact(&mut tx, target, &incoming).await?;
            if fact.is_none() && !current.is_terminal() {
                // 批前快照为 active 且没有任何结果事实，条件插入仍 0 行：状态不可解释，
                // 不能把未写入报告成成功。
                return Err(conflict("child terminal settlement lost its active row"));
            }
            fact
        };
        // 只有本次仍能把 child 从 `active` 写成终态时才继续；已终态的结算只收敛残留
        // 工作意图。提升在终态之前，且只认目标 MQ 里仍存在的本次结果行（设计 §5.3）。
        if !current.is_terminal() {
            if ensures_processing && target != child {
                sqlx::query(PROMOTE_RECEIVER_ON_TERMINAL_SQL)
                    .bind(target.as_str())
                    .bind(&message_id)
                    .bind(child.as_str())
                    .execute(&mut *tx)
                    .await
                    .map_err(|error| map_sqlx(&error))?;
            }
            let updated = sqlx::query(SETTLE_CHILD_TERMINAL_SQL)
                .bind(status.as_str())
                .bind(peri_time::now_utc_rfc3339())
                .bind(work_state.as_str())
                .bind(child.as_str())
                .execute(&mut *tx)
                .await
                .map_err(|error| map_sqlx(&error))?;
            if updated.rows_affected() != 1 {
                // 同一 `BEGIN IMMEDIATE` 内读到的不是终态且行存在，条件更新仍 0 行说明
                // 状态不可解释；不能把未写入报告成成功。
                return Err(conflict("child terminal settlement lost its active row"));
            }
        } else {
            // 终态先提交、工作意图后到：收敛残留的 `working`，不覆盖终态也不新增结果行。
            converge_terminal_work_state(&mut tx, child, current).await?;
        }
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(child.clone())))?;
        Ok(outcome.unwrap_or_else(|| settled_without_write(input)))
    }

    /// 无结果终态：`BEGIN IMMEDIATE` 内一次条件更新 `agent_status` 与 `work_state`。
    ///
    /// 条件 `agent_status = 'active'` 保证同一 child 的终态只写一次；已终态时收敛残留的
    /// `working` 并返回既有终态，不覆盖它。两步独立写入会留下 `Done + working`
    /// 或半完成终态，因此这里与 [`Self::finalize_child_terminal_mq`] 共用事务形状。
    pub(super) async fn settle_terminal_without_result_rows(
        &self,
        child: &ThreadId,
        status: peri_acp_types::thread::AgentStatus,
        work_state: peri_acp_types::thread::WorkState,
    ) -> SessionResourceResult<TerminalSettlementOutcome> {
        self.writable()?;
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        self.require_session(&mut tx, child).await?;
        let current = stored_agent_status(&mut tx, child).await?;
        if current.is_terminal() {
            converge_terminal_work_state(&mut tx, child, current).await?;
            tx.commit()
                .await
                .map_err(|_| commit_failure(Some(child.clone())))?;
            return Ok(TerminalSettlementOutcome::AlreadyTerminal(current));
        }
        let updated = sqlx::query(
            "UPDATE threads SET agent_status = ?1, updated_at = ?2, work_state = ?3
             WHERE id = ?4 AND agent_status = 'active'",
        )
        .bind(status.as_str())
        .bind(peri_time::now_utc_rfc3339())
        .bind(work_state.as_str())
        .bind(child.as_str())
        .execute(&mut *tx)
        .await
        .map_err(|error| map_sqlx(&error))?;
        if updated.rows_affected() != 1 {
            // 同一 `BEGIN IMMEDIATE` 内读到的不是终态且行存在，条件更新仍 0 行说明
            // 状态不可解释；不能把未写入报告成成功。
            return Err(conflict("child terminal settlement lost its active row"));
        }
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(child.clone())))?;
        Ok(TerminalSettlementOutcome::Settled)
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

/// 该 thread 是否有仍要求继续处理的待发行。
///
/// 待发事实就是 `thread_mq` 里的行；判定只看 typed policy，不按 kind 或来源猜。
async fn pending_ensures_processing(
    connection: &mut SqliteConnection,
    thread_id: &ThreadId,
) -> SessionResourceResult<bool> {
    let rows: Vec<(String,)> = sqlx::query_as(SELECT_THREAD_MQ_POLICIES_SQL)
        .bind(thread_id.as_str())
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| map_sqlx(&error))?;
    for (policy_json,) in &rows {
        if policy_ensures_processing(policy_json)? {
            return Ok(true);
        }
    }
    Ok(false)
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

/// 终态已提交时收敛残留的 `working`：同一事务内只改工作意图，不改写终态、不写 MQ。
///
/// 只动 `working`：`stopped` 是显式停止意图，`idle` 已是目标值。
async fn converge_terminal_work_state(
    connection: &mut SqliteConnection,
    child: &ThreadId,
    status: peri_acp_types::thread::AgentStatus,
) -> SessionResourceResult<()> {
    sqlx::query("UPDATE threads SET work_state = ?1 WHERE id = ?2 AND work_state = 'working'")
        .bind(peri_acp_types::thread::WorkState::after_agent_terminal(status).as_str())
        .bind(child.as_str())
        .execute(&mut *connection)
        .await
        .map_err(|error| map_sqlx(&error))?;
    Ok(())
}
