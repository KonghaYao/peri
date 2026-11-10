//! Thread MQ 的远端行为：条件批处理 + 一致读取，不回落成两次独立写入。
//!
//! 远端的执行器只有一种原子单位——托管事务批（`BEGIN IMMEDIATE` … `COMMIT`），且批次内
//! 只回受影响行数、不回结果集。因此每条领域操作都写成「静态 SQL + 参数 + 条件语句」：
//!
//! - **入队**用 `INSERT ... SELECT ... WHERE NOT EXISTS` 做条件插入，0 行即可判定「已有
//!   同 ID 事实」，再走一次一致读取把事实读成 `Existing`/`AlreadyHandedOff`/冲突；
//!   插入命中时工作意图提升与插入同批（`Existing` 的提升另起一批，失败上报可重试错误）；
//! - **移交**把「写历史」与「删 MQ 行」放进同一批，插入语句的正文取自 `thread_mq`
//!   （不读回本机），删除条件带 `enqueued_at`，两边的行数共同决定 `Committed`/`Missing`；
//! - **终态结算**把 child 终态、`work_state`、结果接纳与接收方意图提升放进同一批；
//!   结果插入与终态更新都要求 child 仍为 `active`，终态因此只写一次，后到的结算按批后
//!   读取归类成 no-op 或冲突；
//! - **取消**先一致读取整行（要返回原稿与来源），再按 `(thread_id, id, enqueued_at)` 删除。
//!
//! 读与写之间的空档由**条件语句**吸收：删除/插入都带自己的前提，读到旧事实不会导致误删或
//! 重复移交。批次提交本身仍由 [`super::session_write::RemoteSessionData::commit_effects`]
//! 的资格写 + 确定性三分类负责。

use std::str::FromStr;

use peri_acp_types::messages::MessageId;
use peri_acp_types::session::{
    MessageSource, MqCancelOutcome, MqEnqueueDisposition, MqEnqueueOutcome, MqEntry, MqFilter,
    MqInput, MqReceiveDisposition, MqReceiveOutcome, MqReceiveSelection, MqReceiveStatus,
};
use peri_acp_types::session_resources::{
    SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};
use peri_acp_types::thread::{AgentStatus, ThreadId, WorkState};
use turso_serverless::Value;

use crate::sessions::mq_rows::{
    decode_entry, enqueued_at_now, filter_entries, item_outcome, kind_token, payload_json,
    policy_json, same_payload, source_token, title_of, RawMqRow, DELETE_THREAD_MQ_ROW_SQL,
    DERIVE_THREAD_TITLE_SQL, INSERT_MESSAGE_FROM_MQ_SQL, INSERT_THREAD_MQ_SQL,
    REFRESH_THREAD_COUNT_SQL, SELECT_MESSAGE_ROW_SQL, SELECT_THREAD_MQ_BY_ID_SQL,
    SELECT_THREAD_MQ_SQL,
};

use crate::sessions::failure::conflict;

use super::session_codec::{corrupt, optional_text};
use super::session_data::{invalid_input, not_found, RemoteSessionData};
use super::sql::{int_at, text_at, StatementSpec};

/// 接纳确认后的工作意图提升：只把 `idle` 提升为 `working`，且仅当该消息仍以
/// 本次接纳的稳定行存在（并发移交/替换后不再提升），同时 thread 仍 `active`
/// （终态 thread 的迟到消息不能再授予续跑意图）。
///
/// 是否属于「同一条消息」由调用方用 `same_payload` 判定；SQL 只做并发守卫，
/// 不用 payload 文本比较（重序列化后的文本可能不同）。
const PROMOTE_WORK_INTENT_SQL: &str = "UPDATE threads SET work_state = 'working'
    WHERE id = ?1 AND work_state = 'idle' AND agent_status = 'active'
      AND EXISTS (
        SELECT 1 FROM thread_mq
        WHERE thread_id = ?1 AND id = ?2 AND enqueued_at = ?3
      )";

/// turn 收尾：没有待处理的 `EnsureProcessing` 条目才把 `working` 收敛为 `idle`。
///
/// 判定与写入同一条语句；`policy_json` 的 `EnsureProcessing` 形态由
/// `MessagePolicy::ensures_processing()` 的序列化固定（required + model_visible +
/// activation.kind = ensure_processing）。`stopped` 不被收尾解除（设计 §5.1）。
const SETTLE_WORK_STATE_SQL: &str = "UPDATE threads SET work_state = 'idle'
    WHERE id = ?1 AND work_state = 'working'
      AND NOT EXISTS (
        SELECT 1 FROM thread_mq
        WHERE thread_id = ?1
          AND json_extract(policy_json, '$.requirement') = 'required'
          AND json_extract(policy_json, '$.model_visible') = 1
          AND json_extract(policy_json, '$.activation.kind') = 'ensure_processing'
      )";

/// 批内会话存在性守卫：`peri_store_meta` 只有一行，会话缺失时这次插入撞主键，
/// 整批回滚。与 `session_lifecycle` / `session_history` 的同一技巧（远端批没有条件分支）。
const GUARD_SESSION_ABSENT_SQL: &str = "INSERT INTO peri_store_meta(singleton)
    SELECT 0 WHERE NOT EXISTS (SELECT 1 FROM threads WHERE id = ?1)";

/// 结果 ID 已属于别的 thread：批内中止，避免终态越过冲突提交。
const GUARD_RESULT_ID_FOREIGN_SQL: &str = "INSERT INTO peri_store_meta(singleton)
    SELECT 0 WHERE
      EXISTS (SELECT 1 FROM messages WHERE message_id = ?1 AND thread_id <> ?2)
      OR EXISTS (SELECT 1 FROM thread_mq WHERE id = ?1 AND thread_id <> ?2)";

/// 结算终态 + 工作意图：只在 child 仍 `active` 时生效，后到的结算不能覆盖已提交的终态。
pub(super) const SETTLE_CHILD_STATE_SQL: &str = "UPDATE threads
    SET agent_status = ?1, updated_at = ?2, work_state = ?3
    WHERE id = ?4 AND agent_status = 'active'";

const SELECT_AGENT_STATUS_SQL: &str = "SELECT agent_status FROM threads WHERE id = ?1";

const SELECT_THREAD_EXISTS_SQL: &str = "SELECT EXISTS (SELECT 1 FROM threads WHERE id = ?1)";

/// 结算批内的结果接纳：与 [`INSERT_THREAD_MQ_SQL`] 同列同条件，额外要求 child 仍 `active`。
///
/// 静态 SQL 不能由常量拼接，因此列清单在这里重复一次；两条语句必须同步修改。
const INSERT_TERMINAL_RESULT_SQL: &str = "INSERT INTO thread_mq
    (thread_id, id, enqueued_at, kind, source, policy_json, payload_json, original_draft)
    SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8
    WHERE NOT EXISTS (SELECT 1 FROM messages WHERE message_id = ?2)
      AND NOT EXISTS (SELECT 1 FROM thread_mq WHERE id = ?2)
      AND EXISTS (SELECT 1 FROM threads WHERE id = ?1)
      AND EXISTS (SELECT 1 FROM threads WHERE id = ?9 AND agent_status = 'active')";

/// 结算批内的接收方提升：`Inserted` 与 `Existing` 都以「目标 MQ 里仍有这条待发结果」
/// 为事实；已移交历史的结果没有 MQ 行，不能把刚写的 child 终态重新提升为 `working`。
/// 同时要求 child 仍 `active`：后到的结算既不覆盖终态，也不重复改变接收方意图。
const PROMOTE_RECEIVER_WORK_INTENT_SQL: &str = "UPDATE threads SET work_state = 'working'
    WHERE id = ?1 AND work_state = 'idle'
      AND EXISTS (SELECT 1 FROM thread_mq WHERE thread_id = ?1 AND id = ?2)
      AND EXISTS (SELECT 1 FROM threads WHERE id = ?3 AND agent_status = 'active')";

impl RemoteSessionData {
    pub(super) async fn enqueue_mq(
        &self,
        thread_id: &ThreadId,
        input: MqInput,
    ) -> SessionResourceResult<MqEnqueueOutcome> {
        if !self.exists(thread_id).await? {
            return Err(not_found());
        }
        // 与本机同一条边界校验：MQ 行与最终历史必须共用一个稳定 ID。
        if input.payload.id() != input.id {
            return Err(invalid_input(
                "MQ payload id must match the queued message id",
            ));
        }
        let message_id = label(input.id);
        let payload = payload_json(&input.payload)?;
        let policy = policy_json(&input.policy);
        let ensures_processing = input.policy.ensures_processing();
        let kind = kind_token(input.kind)?;
        let source = source_token(&input.source)?;
        let enqueued_at = enqueued_at_now();
        let inputs = vec![
            thread_id.as_str().to_owned(),
            message_id.clone(),
            kind.clone(),
            source.clone(),
            policy.clone(),
            payload.clone(),
        ];
        let incoming = RawMqRow {
            id: &message_id,
            enqueued_at: &enqueued_at,
            kind: &kind,
            source: &source,
            policy_json: &policy,
            payload_json: &payload,
            original_draft: input.original_draft.as_deref(),
        };
        // 插入与提升同一个批：批失败不会留下「已接纳、意图未提升」的中间态。插入 0 行时
        // 提升也自然 0 行（`(thread_id, id, enqueued_at)` 守卫不成立），回到读取判定。
        let mut effects = vec![insert_mq_statement(thread_id, &incoming)];
        if ensures_processing {
            effects.push(promote_work_intent_statement(
                thread_id,
                &message_id,
                &enqueued_at,
            ));
        }
        let counts = self
            .commit_effects("mq_enqueue", &inputs, effects, thread_id)
            .await?;
        if counts.first() == Some(&1) {
            let entry = decode_entry(incoming)?;
            return Ok(MqEnqueueOutcome {
                disposition: MqEnqueueDisposition::Inserted,
                entry,
            });
        }
        if let Some(outcome) = self.enqueue_mq_fact(thread_id, &incoming).await? {
            // 只有 same_payload 确认是同一条消息才提升；同 ID 冲突与已移交都不提升。
            // 提升失败必须上报，不能把「已接纳但意图未落地」当成成功。
            if outcome.disposition == MqEnqueueDisposition::Existing && ensures_processing {
                self.promote_work_intent_after_accept(
                    thread_id,
                    &message_id,
                    &outcome.entry.enqueued_at,
                )
                .await?;
            }
            return Ok(outcome);
        }
        Err(conflict(
            "MQ enqueue raced the same message id; read the id back before retrying",
        ))
    }

    /// `Existing` 路径在 `same_payload` 确认后的工作意图提升。
    ///
    /// `Inserted` 路径已在接纳批内完成；这里只处理同 ID 重试。失败不吞：把
    /// `PersistenceUncertain`/`Unavailable`/`Timeout` 这类可重试错误原样上抛，调用方用
    /// **同一稳定 ID** 重试；重放读到 `Existing` 后会再次执行同一条条件 UPDATE（幂等）。
    async fn promote_work_intent_after_accept(
        &self,
        thread_id: &ThreadId,
        message_id: &str,
        enqueued_at: &str,
    ) -> SessionResourceResult<()> {
        let inputs = vec![
            thread_id.as_str().to_owned(),
            message_id.to_owned(),
            enqueued_at.to_owned(),
        ];
        self.commit_effects(
            "work_state_promote",
            &inputs,
            vec![promote_work_intent_statement(
                thread_id,
                message_id,
                enqueued_at,
            )],
            thread_id,
        )
        .await
        .map(|_| ())
    }

    /// 远端 child 终态结算：终态、工作意图、结果接纳与接收方意图提升共用一次托管批。
    ///
    /// **终态只写一次**：结果插入与终态/工作意图更新都要求 child 仍为 `active`，后到的
    /// 结算既不能覆盖已提交的终态，也不会产生第二条结果。批前读取只做快速幂等与冲突分类；
    /// 提交后以结果插入行数为准，0 行时按提交后的读取归类 `Existing`/`AlreadyHandedOff`/
    /// 冲突。批内守卫拒绝 child/target 不存在与「结果 ID 已属于别的 thread」，失败整批回滚。
    ///
    /// 并发边界：托管批只回影响行数，无法在批内比较 payload。若同 ID 结果在批前读取与提交
    /// 之间以不同内容落入目标 MQ，本批的结果插入为 0 行，终态是否写入由 `active` 守卫决定，
    /// 批后读取报告冲突。重试会因终态与结果 ID 已落地而成为 no-op。
    pub(super) async fn finalize_child_terminal_mq(
        &self,
        child: &ThreadId,
        status: AgentStatus,
        work_state: WorkState,
        target: &ThreadId,
        input: MqInput,
    ) -> SessionResourceResult<MqEnqueueOutcome> {
        // 与本机覆写同一顺序：先拒绝只读访问，再做输入校验与幂等读取。结算成功即使
        // 落在 no-op 也属于写意图，不能在只读 store 上宣称完成。
        self.ensure_writable().await?;
        if input.payload.id() != input.id {
            return Err(invalid_input(
                "MQ payload id must match the queued message id",
            ));
        }
        let message_id = label(input.id);
        let payload = payload_json(&input.payload)?;
        let policy = policy_json(&input.policy);
        let kind = kind_token(input.kind)?;
        let source = source_token(&input.source)?;
        let ensures_processing = input.policy.ensures_processing();
        let (current, target_exists, message, mq) = self
            .read_settlement_facts(child, target, &message_id)
            .await?;
        if !target_exists {
            return Err(not_found());
        }
        if current.is_terminal() && settlement_landed(target, message.as_ref(), mq.as_ref()) {
            return Ok(settled_without_write(input));
        }
        if let Some((owner, _)) = &message {
            if owner != target.as_str() {
                return Err(conflict(
                    "message id already belongs to another thread's history",
                ));
            }
        }
        if let Some(row) = &mq {
            if row.thread_id != target.as_str() {
                return Err(conflict("message id already belongs to another thread"));
            }
            if !same_payload(&row.payload_json, &payload) {
                return Err(conflict(
                    "a different message already uses this id in the thread MQ",
                ));
            }
        }
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
        let mut effects = Vec::with_capacity(8);
        effects.push(guard_session_statement(child));
        if target != child {
            effects.push(guard_session_statement(target));
        }
        effects.push(StatementSpec::new(
            GUARD_RESULT_ID_FOREIGN_SQL,
            vec![
                Value::Text(message_id.clone()),
                Value::Text(target.as_str().to_owned()),
            ],
        ));
        // 插入放在最前：它的 0 行同时表示「结果已落地」或「child 已不再 active」。后续
        // 写入都带 `active` 守卫，因此后到的结算不会覆盖先提交的终态。
        let insert_index = effects.len();
        effects.push(StatementSpec::new(
            INSERT_TERMINAL_RESULT_SQL,
            vec![
                Value::Text(target.as_str().to_owned()),
                Value::Text(message_id.clone()),
                Value::Text(enqueued_at.clone()),
                Value::Text(kind.clone()),
                Value::Text(source.clone()),
                Value::Text(policy.clone()),
                Value::Text(payload.clone()),
                optional_text(input.original_draft.as_deref()),
                Value::Text(child.as_str().to_owned()),
            ],
        ));
        // 提升在终态更新之前：两条语句都以 child 仍 `active` 为前提，批内先提升接收方，
        // 再把 child 置为终态；结果行不存在时提升自然不生效。
        if ensures_processing && target != child {
            effects.push(StatementSpec::new(
                PROMOTE_RECEIVER_WORK_INTENT_SQL,
                vec![
                    Value::Text(target.as_str().to_owned()),
                    Value::Text(message_id.clone()),
                    Value::Text(child.as_str().to_owned()),
                ],
            ));
        }
        effects.push(StatementSpec::new(
            SETTLE_CHILD_STATE_SQL,
            vec![
                Value::Text(status.as_str().to_owned()),
                Value::Text(timestamp()),
                Value::Text(work_state.as_str().to_owned()),
                Value::Text(child.as_str().to_owned()),
            ],
        ));
        let inputs = vec![
            child.as_str().to_owned(),
            target.as_str().to_owned(),
            message_id.clone(),
            status.as_str().to_owned(),
            work_state.as_str().to_owned(),
            kind.clone(),
            source.clone(),
            policy.clone(),
            payload.clone(),
        ];
        match self
            .commit_effects("finalize_child_terminal", &inputs, effects, child)
            .await
        {
            Ok(counts) if counts.get(insert_index) == Some(&1) => Ok(MqEnqueueOutcome {
                disposition: MqEnqueueDisposition::Inserted,
                entry: decode_entry(incoming)?,
            }),
            Ok(_) => match self.enqueue_mq_fact(target, &incoming).await? {
                Some(outcome) => Ok(outcome),
                None => Err(conflict(
                    "child terminal settlement raced the result message; read the id back before retrying",
                )),
            },
            Err(error) => Err(self
                .classify_settlement_failure(child, target, &message_id, error)
                .await),
        }
    }

    /// 批内守卫把竞态折成约束拒绝；补读一次给出与批前读取一致的归类。
    ///
    /// 只对「确定未生效」的输入类失败补读；未决/传输失败保持原错误，不能因补读失败
    /// 把未决降级成冲突或不存在。
    async fn classify_settlement_failure(
        &self,
        child: &ThreadId,
        target: &ThreadId,
        message_id: &str,
        error: SessionResourceError,
    ) -> SessionResourceError {
        if !matches!(error.kind(), SessionResourceErrorKind::InvalidInput { .. }) {
            return error;
        }
        if !self.exists(child).await.unwrap_or(true) || !self.exists(target).await.unwrap_or(true) {
            return not_found();
        }
        match self.read_mq_facts(message_id).await {
            Ok((message, mq)) => {
                let foreign = message
                    .as_ref()
                    .is_some_and(|(owner, _)| owner != target.as_str())
                    || mq
                        .as_ref()
                        .is_some_and(|row| row.thread_id != target.as_str());
                if foreign {
                    conflict("message id already belongs to another thread")
                } else {
                    error
                }
            }
            Err(_) => error,
        }
    }

    /// 结算前的一致读取：child 状态、目标存在性与结果 ID 的历史/MQ 事实来自同一快照。
    async fn read_settlement_facts(
        &self,
        child: &ThreadId,
        target: &ThreadId,
        message_id: &str,
    ) -> SessionResourceResult<(
        AgentStatus,
        bool,
        Option<(String, String)>,
        Option<RemoteMqRow>,
    )> {
        let store = self.store().await?;
        let batches = store
            .read_batch(vec![
                StatementSpec::new(
                    SELECT_AGENT_STATUS_SQL,
                    vec![Value::Text(child.as_str().to_owned())],
                ),
                StatementSpec::new(
                    SELECT_THREAD_EXISTS_SQL,
                    vec![Value::Text(target.as_str().to_owned())],
                ),
                StatementSpec::new(
                    SELECT_MESSAGE_ROW_SQL,
                    vec![Value::Text(message_id.to_owned())],
                ),
                StatementSpec::new(
                    SELECT_THREAD_MQ_BY_ID_SQL,
                    vec![Value::Text(message_id.to_owned())],
                ),
            ])
            .await?;
        let mut batches = batches.into_iter();
        let status_rows = batches.next().ok_or_else(|| {
            super::mutation::incomplete_reply("expected the child status result set")
        })?;
        let target_rows = batches.next().ok_or_else(|| {
            super::mutation::incomplete_reply("expected the target existence result set")
        })?;
        let message_rows = batches
            .next()
            .ok_or_else(|| super::mutation::incomplete_reply("expected the message result set"))?;
        let mq_rows = batches
            .next()
            .ok_or_else(|| super::mutation::incomplete_reply("expected the MQ result set"))?;
        let status = status_rows
            .first()
            .and_then(|row| text_at(row, 0))
            .ok_or_else(not_found)?;
        let status = AgentStatus::from_str(status).map_err(|_| corrupt("invalid agent status"))?;
        let target_exists = target_rows.first().and_then(|row| int_at(row, 0)) == Some(1);
        let message = match message_rows.first() {
            Some(row) => Some((
                text_at(row, 0)
                    .ok_or_else(|| corrupt("invalid history row"))?
                    .to_owned(),
                text_at(row, 1)
                    .ok_or_else(|| corrupt("invalid history row"))?
                    .to_owned(),
            )),
            None => None,
        };
        let mq = match mq_rows.first() {
            Some(row) => Some(RemoteMqRow::decode(row)?),
            None => None,
        };
        Ok((status, target_exists, message, mq))
    }

    /// turn 收尾的条件收敛：返回是否真的写入 `idle`。
    pub(super) async fn settle_work_state_after_turn_mq(
        &self,
        thread_id: &ThreadId,
    ) -> SessionResourceResult<bool> {
        let inputs = vec![thread_id.as_str().to_owned()];
        let counts = self
            .commit_effects(
                "work_state_settle",
                &inputs,
                vec![StatementSpec::new(
                    SETTLE_WORK_STATE_SQL,
                    vec![Value::Text(thread_id.as_str().to_owned())],
                )],
                thread_id,
            )
            .await?;
        Ok(counts.first() == Some(&1))
    }

    /// 条件插入没命中时的事实判定：历史优先，其次 MQ。
    async fn enqueue_mq_fact(
        &self,
        thread_id: &ThreadId,
        incoming: &RawMqRow<'_>,
    ) -> SessionResourceResult<Option<MqEnqueueOutcome>> {
        let (message, mq) = self.read_mq_facts(incoming.id).await?;
        if let Some((owner, _content)) = message {
            if owner != thread_id.as_str() {
                return Err(conflict(
                    "message id already belongs to another thread's history",
                ));
            }
            return Ok(Some(MqEnqueueOutcome {
                disposition: MqEnqueueDisposition::AlreadyHandedOff,
                entry: decode_entry(RawMqRow {
                    enqueued_at: &enqueued_at_now(),
                    ..*incoming
                })?,
            }));
        }
        let Some(row) = mq else {
            return Ok(None);
        };
        if row.thread_id != thread_id.as_str() {
            return Err(conflict("message id already belongs to another thread"));
        }
        let same = row.kind == incoming.kind
            && row.source == incoming.source
            && row.policy_json == incoming.policy_json
            && same_payload(&row.payload_json, incoming.payload_json)
            && row.original_draft.as_deref() == incoming.original_draft;
        if !same {
            return Err(conflict(
                "a different message already uses this id in the thread MQ",
            ));
        }
        Ok(Some(MqEnqueueOutcome {
            disposition: MqEnqueueDisposition::Existing,
            entry: row.into_entry()?,
        }))
    }

    pub(super) async fn query_mq(
        &self,
        thread_id: &ThreadId,
        filter: MqFilter,
    ) -> SessionResourceResult<Vec<MqEntry>> {
        let store = self.store().await?;
        // 一致读取：排序与投影取自同一快照，不会出现「按这页排序、读那页内容」。
        let mut batches = store
            .read_batch(vec![StatementSpec::new(
                SELECT_THREAD_MQ_SQL,
                vec![Value::Text(thread_id.as_str().to_owned())],
            )])
            .await?;
        let rows = batches
            .pop()
            .ok_or_else(|| super::mutation::incomplete_reply("expected the MQ result set"))?;
        let entries = rows
            .iter()
            .map(|row| decode_remote_entry(row))
            .collect::<SessionResourceResult<Vec<_>>>()?;
        Ok(filter_entries(entries, &filter))
    }

    pub(super) async fn cancel_mq(
        &self,
        thread_id: &ThreadId,
        id: MessageId,
    ) -> SessionResourceResult<MqCancelOutcome> {
        if !self.exists(thread_id).await? {
            return Err(not_found());
        }
        let message_id = label(id);
        let (message, mq) = self.read_mq_facts(&message_id).await?;
        let Some(row) = mq.filter(|row| row.thread_id == thread_id.as_str()) else {
            return Ok(match message {
                Some((owner, _)) if owner == thread_id.as_str() => {
                    MqCancelOutcome::AlreadyHandedOff
                }
                _ => MqCancelOutcome::NotFound,
            });
        };
        let entry = row.clone().into_entry()?;
        if entry.source != MessageSource::UserInput {
            return Err(invalid_input(
                "only pending user input can be cancelled from the thread MQ",
            ));
        }
        let inputs = vec![
            thread_id.as_str().to_owned(),
            message_id.clone(),
            entry.enqueued_at.clone(),
        ];
        let counts = self
            .commit_effects(
                "mq_cancel",
                &inputs,
                vec![StatementSpec::new(
                    DELETE_THREAD_MQ_ROW_SQL,
                    vec![
                        Value::Text(thread_id.as_str().to_owned()),
                        Value::Text(message_id),
                        Value::Text(entry.enqueued_at.clone()),
                    ],
                )],
                thread_id,
            )
            .await?;
        if counts.first() == Some(&1) {
            return Ok(MqCancelOutcome::Cancelled(entry));
        }
        // 条件删除没命中：行已被并发 Receive 移交，或取消/重新入队后水位已变。
        let (message, mq) = self.read_mq_facts(&label(id)).await?;
        Ok(match (message, mq) {
            (Some((owner, _)), _) if owner == thread_id.as_str() => {
                MqCancelOutcome::AlreadyHandedOff
            }
            _ => MqCancelOutcome::NotFound,
        })
    }

    pub(super) async fn receive_mq(
        &self,
        thread_id: &ThreadId,
        selection: MqReceiveSelection,
    ) -> SessionResourceResult<MqReceiveOutcome> {
        if !self.exists(thread_id).await? {
            return Err(not_found());
        }
        let mut effects = Vec::with_capacity(selection.items.len() * 2 + 2);
        // 每条选择项在批次里的语句位置：`(插入历史, 删除 MQ)`，`None` 表示这类处置不写该语句。
        let mut placements = Vec::with_capacity(selection.items.len());
        let mut inputs = vec![thread_id.as_str().to_owned()];
        let mut committed = Vec::new();
        for item in &selection.items {
            let message_id = label(item.entry.id);
            inputs.push(message_id.clone());
            inputs.push(item.entry.enqueued_at.clone());
            let insert = match item.disposition {
                MqReceiveDisposition::Commit => {
                    committed.push(item.entry.clone());
                    let at = effects.len();
                    effects.push(StatementSpec::new(
                        INSERT_MESSAGE_FROM_MQ_SQL,
                        vec![
                            Value::Text(thread_id.as_str().to_owned()),
                            Value::Text(message_id.clone()),
                            Value::Text(item.entry.enqueued_at.clone()),
                            Value::Text(
                                crate::sessions::canonical::payload_role(&item.entry.payload)
                                    .to_owned(),
                            ),
                        ],
                    ));
                    Some(at)
                }
                MqReceiveDisposition::Suppress => None,
            };
            let delete = effects.len();
            effects.push(StatementSpec::new(
                DELETE_THREAD_MQ_ROW_SQL,
                vec![
                    Value::Text(thread_id.as_str().to_owned()),
                    Value::Text(message_id),
                    Value::Text(item.entry.enqueued_at.clone()),
                ],
            ));
            placements.push((insert, delete));
        }
        if !committed.is_empty() {
            // 派生计数只在历史真的增长时推进；标题只在为空时补齐（与本机同一份派生规则）。
            effects.push(StatementSpec::new(
                REFRESH_THREAD_COUNT_SQL,
                vec![
                    Value::Text(thread_id.as_str().to_owned()),
                    Value::Text(timestamp()),
                ],
            ));
            effects.push(StatementSpec::new(
                DERIVE_THREAD_TITLE_SQL,
                vec![
                    Value::Text(thread_id.as_str().to_owned()),
                    super::session_codec::optional_text(title_of(&committed).as_deref()),
                ],
            ));
        }
        let counts = self
            .commit_effects("mq_receive", &inputs, effects, thread_id)
            .await?;
        let mut items = Vec::with_capacity(selection.items.len());
        for (item, (insert, delete)) in selection.items.iter().zip(&placements) {
            let deleted = counts.get(*delete) == Some(&1);
            if !deleted {
                items.push(item_outcome(item.entry.id, MqReceiveStatus::Missing, None));
                continue;
            }
            let status = match (item.disposition, *insert) {
                (MqReceiveDisposition::Suppress, _) => MqReceiveStatus::Suppressed,
                (MqReceiveDisposition::Commit, Some(index)) => {
                    if counts.get(index) == Some(&1) {
                        MqReceiveStatus::Committed
                    } else {
                        // 插入被 `NOT EXISTS` 挡住：同 ID 已在历史。内容相同是幂等命中，
                        // 不同（或属于别的 thread）是身份冲突——两种都只删 MQ 行、不写历史。
                        let (message, _) = self.read_mq_facts(&label(item.entry.id)).await?;
                        match message {
                            Some((owner, content))
                                if owner == thread_id.as_str()
                                    && same_payload(
                                        &content,
                                        &payload_json(&item.entry.payload)?,
                                    ) =>
                            {
                                MqReceiveStatus::AlreadyPresent
                            }
                            _ => MqReceiveStatus::Conflict,
                        }
                    }
                }
                (MqReceiveDisposition::Commit, None) => MqReceiveStatus::Missing,
            };
            items.push(item_outcome(
                item.entry.id,
                status,
                Some(item.entry.clone()),
            ));
        }
        Ok(MqReceiveOutcome { items })
    }

    pub(super) async fn clear_mq(&self, thread_id: &ThreadId) -> SessionResourceResult<()> {
        if !self.exists(thread_id).await? {
            return Err(not_found());
        }
        let inputs = vec![thread_id.as_str().to_owned()];
        self.commit_effects(
            "mq_clear",
            &inputs,
            vec![StatementSpec::new(
                crate::sessions::canonical::DELETE_THREAD_MQ_BY_THREAD_SQL,
                vec![Value::Text(thread_id.as_str().to_owned())],
            )],
            thread_id,
        )
        .await
        .map(|_| ())
    }

    /// MQ 行是否仍在该 thread 待发（只读事实）。
    pub(super) async fn history_contains(
        &self,
        thread_id: &ThreadId,
        id: MessageId,
    ) -> SessionResourceResult<bool> {
        if !self.exists(thread_id).await? {
            return Err(not_found());
        }
        let (message, _mq) = self.read_mq_facts(&label(id)).await?;
        Ok(matches!(message, Some((owner, _)) if owner == thread_id.as_str()))
    }

    /// 同一次一致读取里取「历史同 ID 行」与「MQ 行」：两者的先后不由两次请求给出。
    async fn read_mq_facts(
        &self,
        message_id: &str,
    ) -> SessionResourceResult<(Option<(String, String)>, Option<RemoteMqRow>)> {
        let store = self.store().await?;
        let batches = store
            .read_batch(vec![
                StatementSpec::new(
                    SELECT_MESSAGE_ROW_SQL,
                    vec![Value::Text(message_id.to_owned())],
                ),
                StatementSpec::new(
                    SELECT_THREAD_MQ_BY_ID_SQL,
                    vec![Value::Text(message_id.to_owned())],
                ),
            ])
            .await?;
        let mut batches = batches.into_iter();
        let message_rows = batches
            .next()
            .ok_or_else(|| super::mutation::incomplete_reply("expected the message result set"))?;
        let mq_rows = batches
            .next()
            .ok_or_else(|| super::mutation::incomplete_reply("expected the MQ result set"))?;
        let message = match message_rows.first() {
            Some(row) => Some((
                text_at(row, 0)
                    .ok_or_else(|| corrupt("invalid history row"))?
                    .to_owned(),
                text_at(row, 1)
                    .ok_or_else(|| corrupt("invalid history row"))?
                    .to_owned(),
            )),
            None => None,
        };
        let mq = match mq_rows.first() {
            Some(row) => Some(RemoteMqRow::decode(row)?),
            None => None,
        };
        Ok((message, mq))
    }
}

pub(super) fn guard_session_statement(id: &ThreadId) -> StatementSpec {
    StatementSpec::new(
        GUARD_SESSION_ABSENT_SQL,
        vec![Value::Text(id.as_str().to_owned())],
    )
}

/// 接纳提升：只把 `idle` 提升为 `working`，且仅当该消息仍以本次接纳的稳定行存在
/// （并发移交/替换后不再提升）。是否属于「同一条消息」由调用方用 `same_payload` 判定。
fn promote_work_intent_statement(
    thread_id: &ThreadId,
    message_id: &str,
    enqueued_at: &str,
) -> StatementSpec {
    StatementSpec::new(
        PROMOTE_WORK_INTENT_SQL,
        vec![
            Value::Text(thread_id.as_str().to_owned()),
            Value::Text(message_id.to_owned()),
            Value::Text(enqueued_at.to_owned()),
        ],
    )
}

fn insert_mq_statement(thread_id: &ThreadId, row: &RawMqRow<'_>) -> StatementSpec {
    StatementSpec::new(
        INSERT_THREAD_MQ_SQL,
        vec![
            Value::Text(thread_id.as_str().to_owned()),
            Value::Text(row.id.to_owned()),
            Value::Text(row.enqueued_at.to_owned()),
            Value::Text(row.kind.to_owned()),
            Value::Text(row.source.to_owned()),
            Value::Text(row.policy_json.to_owned()),
            Value::Text(row.payload_json.to_owned()),
            optional_text(row.original_draft),
        ],
    )
}

/// 这次结算是否已落地：child 终态之外，结果 ID 必须已在目标 thread 的 MQ 或历史里。
fn settlement_landed(
    target: &ThreadId,
    message: Option<&(String, String)>,
    mq: Option<&RemoteMqRow>,
) -> bool {
    message.is_some_and(|(owner, _)| owner == target.as_str())
        || mq.is_some_and(|row| row.thread_id == target.as_str())
}

/// 已落地结算的成功 no-op：本次没有写入任何行，entry 只是请求投影。
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

/// 远端读回的一行 MQ 事实（列序与 [`SELECT_THREAD_MQ_BY_ID_SQL`] 一致）。
#[derive(Clone)]
struct RemoteMqRow {
    thread_id: String,
    id: String,
    enqueued_at: String,
    kind: String,
    source: String,
    policy_json: String,
    payload_json: String,
    original_draft: Option<String>,
}

impl RemoteMqRow {
    fn decode(values: &[Value]) -> SessionResourceResult<Self> {
        Ok(Self {
            thread_id: text_at(values, 0)
                .ok_or_else(|| corrupt("invalid MQ row"))?
                .to_owned(),
            id: text_at(values, 1)
                .ok_or_else(|| corrupt("invalid MQ row"))?
                .to_owned(),
            enqueued_at: text_at(values, 2)
                .ok_or_else(|| corrupt("invalid MQ row"))?
                .to_owned(),
            kind: text_at(values, 3)
                .ok_or_else(|| corrupt("invalid MQ row"))?
                .to_owned(),
            source: text_at(values, 4)
                .ok_or_else(|| corrupt("invalid MQ row"))?
                .to_owned(),
            policy_json: text_at(values, 5)
                .ok_or_else(|| corrupt("invalid MQ row"))?
                .to_owned(),
            payload_json: text_at(values, 6)
                .ok_or_else(|| corrupt("invalid MQ row"))?
                .to_owned(),
            original_draft: optional_text_at(values, 7),
        })
    }

    fn into_entry(self) -> SessionResourceResult<MqEntry> {
        decode_entry(RawMqRow {
            id: &self.id,
            enqueued_at: &self.enqueued_at,
            kind: &self.kind,
            source: &self.source,
            policy_json: &self.policy_json,
            payload_json: &self.payload_json,
            original_draft: self.original_draft.as_deref(),
        })
    }
}

/// 列表行的列序与常量 SQL 一致（不带 `thread_id`）。
fn decode_remote_entry(values: &[Value]) -> SessionResourceResult<MqEntry> {
    decode_entry(RawMqRow {
        id: text_at(values, 0).ok_or_else(|| corrupt("invalid MQ row"))?,
        enqueued_at: text_at(values, 1).ok_or_else(|| corrupt("invalid MQ row"))?,
        kind: text_at(values, 2).ok_or_else(|| corrupt("invalid MQ row"))?,
        source: text_at(values, 3).ok_or_else(|| corrupt("invalid MQ row"))?,
        policy_json: text_at(values, 4).ok_or_else(|| corrupt("invalid MQ row"))?,
        payload_json: text_at(values, 5).ok_or_else(|| corrupt("invalid MQ row"))?,
        original_draft: optional_text_at(values, 6).as_deref(),
    })
}

fn optional_text_at(values: &[Value], index: usize) -> Option<String> {
    match values.get(index) {
        Some(Value::Text(text)) => Some(text.clone()),
        _ => None,
    }
}

fn label(id: MessageId) -> String {
    id.as_uuid().to_string()
}

fn timestamp() -> String {
    peri_time::now_utc_rfc3339()
}
