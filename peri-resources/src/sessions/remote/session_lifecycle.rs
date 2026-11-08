//! 远程会话生命周期写入：未发布撤销、删除会话树、child resume 认领事实。
//!
//! ## 远端没有本机生命周期锚点
//!
//! 门面在删除路径上显式结束当前实例的运行所有权，不额外持久化执行行或墓碑；
//! 远端同样不新增执行状态副本——远端没有第二个副本，
//! 删除就是删除，不存在「删除被复制回来」的路径。所以这里的删除是**刻意删除数据事实本身**，
//! 不是把本机的墓碑语义搬过来。v10 撤销本机登记与未决锚点后，本机也不再持有
//! `session_lifecycle_commitments` 这类跨进程生命周期表——刻意删除由本机执行面显式结束
//! 所有权来表达。
//!
//! ## 一处有意的偏离（已记录，需在门面侧统一）
//!
//! 本机对「目标行不存在」的若干写入路径（`delete_tree` 之外的生命周期 UPDATE）只按
//! `UPDATE` 是否报错判断，0 行受影响也可能返回成功。远端按端口契约**不把未生效报告成
//! 成功**：批内守卫会把「会话/子会话不存在」变成明确的 `NotFound`。若要让两个 adapter
//! 在这一路径上完全一致，应改本机侧，而不是让远端退回「0 行也算成功」。

use std::str::FromStr;

use peri_acp_types::session_resources::{
    SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};
use peri_acp_types::thread::{AgentStatus, ThreadId};
use turso_serverless::Value;

use super::mutation::incomplete_reply;
use super::session_codec as codec;
use super::session_data::{invalid_input, not_found, RemoteSessionData};
use super::session_sql;
use super::sql::{int_at, text_at, StatementSpec};
use crate::sessions::data::ChildResumeRecord;

// ─── 批内守卫 ─────────────────────────────────────────────────────────────────

const GUARD_SESSION_ABSENT_SQL: &str = "INSERT INTO peri_store_meta(singleton)
    SELECT 0 WHERE NOT EXISTS (SELECT 1 FROM threads WHERE id = ?1)";

/// 撤销批内的子会话守卫：`COUNT` 的读与删除不在同一事务时，另一个实例可能在这两步之间
/// 提交 child；谓词成立就撞 `peri_store_meta` 的单行主键，整批回滚。
const GUARD_CHILDREN_PRESENT_SQL: &str = "INSERT INTO peri_store_meta(singleton)
    SELECT 0 WHERE EXISTS (SELECT 1 FROM threads WHERE parent_thread_id = ?1)";

// ─── 效果语句 ─────────────────────────────────────────────────────────────────

/// 子树范围前缀：每条删除各自带上递归定义，使「选范围」与「删范围」落进同一个托管事务批。
///
/// 远端批无法把一次读取的结果喂给后续语句，因此不能先读 id 列表再逐条删除——那会把删除
/// 范围钉在批外的快照上（子会话在两步之间提交就会成为孤儿）。删除范围由 SQL 在批内重新
/// 计算，读与写共享同一个事务快照。
macro_rules! tree_scoped {
    ($tail:literal) => {
        concat!(
            "WITH RECURSIVE tree(id) AS (",
            " SELECT id FROM threads WHERE id = ?1",
            " UNION ALL",
            " SELECT s.id FROM threads s JOIN tree t ON s.parent_thread_id = t.id",
            ") ",
            $tail
        )
    };
}

const DELETE_TREE_MESSAGES_SQL: &str =
    tree_scoped!("DELETE FROM messages WHERE thread_id IN (SELECT id FROM tree)");

const DELETE_TREE_BINDINGS_SQL: &str =
    tree_scoped!("DELETE FROM session_bindings WHERE thread_id IN (SELECT id FROM tree)");

const DELETE_TREE_CLOSE_INTENTS_SQL: &str =
    tree_scoped!("DELETE FROM session_close_intents WHERE thread_id IN (SELECT id FROM tree)");

const DELETE_TREE_MQ_SQL: &str =
    tree_scoped!("DELETE FROM thread_mq WHERE thread_id IN (SELECT id FROM tree)");

const DELETE_TREE_THREADS_SQL: &str =
    tree_scoped!("DELETE FROM threads WHERE id IN (SELECT id FROM tree)");

/// 直接子会话计数（撤销必须拒绝「已有子会话」的 identity，否则子会话会指向不存在的父）。
const COUNT_CHILDREN_SQL: &str = "SELECT COUNT(*) FROM threads WHERE parent_thread_id = ?1";

/// child resume 认领事实：状态 + 更新时间（`claimed` 由状态派生，不是独立列）。
const UPDATE_AGENT_STATUS_SQL: &str =
    "UPDATE threads SET agent_status = ?1, updated_at = ?2 WHERE id = ?3";

const SELECT_AGENT_STATUS_SQL: &str = "SELECT agent_status FROM threads WHERE id = ?1";

impl RemoteSessionData {
    pub(super) async fn write_close_intent(&self, id: &ThreadId) -> SessionResourceResult<()> {
        if !self.exists(id).await? {
            return Err(not_found());
        }
        self.commit_effects(
            "mark_session_closing",
            &[id.as_str().to_owned()],
            vec![StatementSpec::new(
                "INSERT OR IGNORE INTO session_close_intents(thread_id, requested_at) VALUES (?1, ?2)",
                vec![Value::Text(id.as_str().to_owned()), Value::Text(peri_time::now_utc_rfc3339())],
            )],
            id,
        ).await.map(|_| ())
    }

    pub(super) async fn read_close_intent(&self, id: &ThreadId) -> SessionResourceResult<bool> {
        let store = self.store().await?;
        let table = store.fetch_row(&StatementSpec::bare(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'session_close_intents'",
        )).await?;
        if table.is_none() {
            return Ok(false);
        }
        Ok(store
            .fetch_row(&StatementSpec::new(
                "SELECT 1 FROM session_close_intents WHERE thread_id = ?1",
                vec![Value::Text(id.as_str().to_owned())],
            ))
            .await?
            .is_some())
    }

    /// 撤销未发布的创建（write-once 完整创建的失败补偿：fork 等）。
    ///
    /// 语义由**入口**决定：这里的目标创建即带 frozen（可由 source 重生成），撤销就是把它
    /// 整条删掉——不叠加「未提交 frozen」判据。两阶段草稿的撤销走
    /// [`Self::revoke_unpublished_draft`]。
    ///
    /// 与本机同一判据（`parent_thread_id` 计数 > 0 → `InvalidInput`）；会话行本来就不在时
    /// 是幂等删除（本机同一语义：补偿路径把「已经不在了」当成目标已达成）。
    /// 远端不留 `creation_intent` 锚点：那本机事实用于判定「同一 identity 不被复活」，
    /// 远端没有第二个副本，也就没有需要锚定的复活路径。
    pub(super) async fn revoke_unpublished(&self, id: &ThreadId) -> SessionResourceResult<()> {
        // inherited 派生索引随 v15 退役：撤销批次只剩三条 canonical 删除。
        let effects = session_sql::revoke_session_statements(id.as_str());
        self.commit_revocation("revoke_unpublished_session", id, effects)
            .await
    }

    /// 撤销**两阶段草稿**（`SessionInitialization::abandon` 驱动的补偿）。
    ///
    /// 与 [`Self::revoke_unpublished`] 的唯一差别是判据：三条删除共用
    /// `frozen_context IS NULL`，已提交 frozen 的草稿一条都不删并返回 typed 冲突
    /// （那是「已定稿、未发布」的合法中间态，走 dirty 恢复）。
    pub(super) async fn revoke_unpublished_draft(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<()> {
        // inherited 派生索引随 v15 退役：草稿撤销批次只剩三条带判据的 canonical 删除。
        let effects = session_sql::revoke_draft_statements(id.as_str());
        let counts = self
            .commit_revocation_counts("revoke_unpublished_draft", id, effects)
            .await?;
        match counts.last() {
            // 重放：原操作已生效（行已经不在）。
            None => Ok(()),
            Some(0) => {
                // 判据不成立：要么已提交 frozen，要么行本来就不在（幂等删除）。
                match self.frozen_of(id).await? {
                    Some(_) => Err(SessionResourceError::conflict(
                        "session has a committed frozen snapshot and cannot be revoked",
                    )),
                    None => Ok(()),
                }
            }
            Some(_) => Ok(()),
        }
    }

    /// 撤销的公共编排：子会话守卫（计数必须可证明为 0）+ 一次托管删除批。
    async fn commit_revocation(
        &self,
        behavior: &str,
        id: &ThreadId,
        effects: Vec<StatementSpec>,
    ) -> SessionResourceResult<()> {
        self.commit_revocation_counts(behavior, id, effects)
            .await
            .map(|_| ())
    }

    async fn commit_revocation_counts(
        &self,
        behavior: &str,
        id: &ThreadId,
        effects: Vec<StatementSpec>,
    ) -> SessionResourceResult<Vec<u64>> {
        let store = self.store().await?;
        let children = store
            .fetch_row(&StatementSpec::new(
                COUNT_CHILDREN_SQL,
                vec![Value::Text(id.as_str().to_owned())],
            ))
            .await?;
        // 子会话数与后面的写入各取一次连接：借用不跨过去（见上）。
        drop(store);
        revocation_gate(children.as_ref().and_then(|values| int_at(values, 0)))?;
        // 预检只负责尽早给出可读的错误；真正的判据在批内重算，child 创建与撤销因此互斥。
        let mut guarded = Vec::with_capacity(effects.len() + 1);
        guarded.push(guard_children_statement(id));
        guarded.extend(effects);
        match self
            .commit_effects(behavior, &[format!("id:{}", id.as_str())], guarded, id)
            .await
        {
            Ok(counts) => Ok(counts),
            Err(error) => {
                // 守卫把并发出现的子会话折成批内约束拒绝；对外仍按撤销判据归类。
                if matches!(error.kind(), SessionResourceErrorKind::InvalidInput { .. })
                    && self.has_children(id).await.unwrap_or(false)
                {
                    return Err(invalid_input(
                        "session has published children and cannot be revoked",
                    ));
                }
                Err(error)
            }
        }
    }

    /// 直接子会话是否存在；只用于失败路径的归类：读不出计数时保留原错误。
    async fn has_children(&self, id: &ThreadId) -> SessionResourceResult<bool> {
        let store = self.store().await?;
        let children = store
            .fetch_row(&StatementSpec::new(
                COUNT_CHILDREN_SQL,
                vec![Value::Text(id.as_str().to_owned())],
            ))
            .await?;
        Ok(children
            .as_ref()
            .and_then(|values| int_at(values, 0))
            .is_some_and(|count| count > 0))
    }

    /// 删除会话树：子树（含根）的历史与会话行在同一批里消失。
    ///
    /// 删除是刻意行为：没有墓碑、没有执行行清理（远端都没有这些事实），但**不留半棵**——
    /// 子树范围由每条删除语句在**同一托管事务批内**重新计算，不再先读一份 id 快照。
    ///
    /// 根是否存在由批内守卫决定：根行缺失时守卫撞单行主键 → 整批回滚 → `NotFound`；空结果
    /// **不能**当成「没有东西要删」而报成功。子会话在批开始前提交时，批内递归已经包含它；
    /// 在批持锁期间提交则被串行化到删除之后，由 child 侧的父存在守卫拒绝。
    pub(super) async fn write_tree_deletion(&self, id: &ThreadId) -> SessionResourceResult<()> {
        // 删除不新增墓碑：树上的会话先清子行（与 `canonical::THREAD_CHILD_DELETES` 同一顺序：
        // messages → session_bindings → close intents → thread_mq），最后清会话行，全部在
        // 同一个批里。inherited 派生索引随 v15 退役，删除路径不再引用它们。
        let mut effects = Vec::with_capacity(6);
        effects.push(guard_session_statement(id));
        effects.push(StatementSpec::new(
            DELETE_TREE_MESSAGES_SQL,
            vec![Value::Text(id.as_str().to_owned())],
        ));
        effects.push(StatementSpec::new(
            DELETE_TREE_BINDINGS_SQL,
            vec![Value::Text(id.as_str().to_owned())],
        ));
        effects.push(StatementSpec::new(
            DELETE_TREE_CLOSE_INTENTS_SQL,
            vec![Value::Text(id.as_str().to_owned())],
        ));
        effects.push(StatementSpec::new(
            DELETE_TREE_MQ_SQL,
            vec![Value::Text(id.as_str().to_owned())],
        ));
        effects.push(StatementSpec::new(
            DELETE_TREE_THREADS_SQL,
            vec![Value::Text(id.as_str().to_owned())],
        ));
        match self
            .commit_effects(
                "delete_session_tree",
                &[format!("id:{}", id.as_str())],
                effects,
                id,
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(error) => {
                // 守卫把「根不存在」折成批内约束拒绝；对外仍是删除入口的同一结果。
                if matches!(error.kind(), SessionResourceErrorKind::InvalidInput { .. })
                    && !self.exists(id).await.unwrap_or(true)
                {
                    return Err(not_found());
                }
                Err(error)
            }
        }
    }

    /// 读取 child resume 认领事实：状态 + 是否仍在认领中。
    ///
    /// `claimed` 与本机同源：`agent_status` 处于 active 即「正在被认领」，不是独立列。
    pub(super) async fn read_child_resume(
        &self,
        child: &ThreadId,
    ) -> SessionResourceResult<ChildResumeRecord> {
        let store = self.store().await?;
        let row = store
            .fetch_row(&StatementSpec::new(
                SELECT_AGENT_STATUS_SQL,
                vec![Value::Text(child.as_str().to_owned())],
            ))
            .await?
            .ok_or_else(not_found)?;
        let status = text_at(&row, 0)
            .and_then(|text| AgentStatus::from_str(text).ok())
            .ok_or_else(|| codec::corrupt("agent_status is not a known value"))?;
        Ok(ChildResumeRecord {
            status,
            claimed: status.is_active(),
        })
    }

    /// 写入 child resume 认领事实（状态与终态由门面按领域结果给出）。
    ///
    /// 会话不存在时明确失败（见模块文档里记录的那处有意偏离：不把 0 行更新报告成成功）。
    pub(super) async fn write_child_resume(
        &self,
        child: &ThreadId,
        record: &ChildResumeRecord,
    ) -> SessionResourceResult<()> {
        let effects = vec![
            guard_session_statement(child),
            StatementSpec::new(
                UPDATE_AGENT_STATUS_SQL,
                vec![
                    Value::Text(record.status.as_str().to_owned()),
                    Value::Text(timestamp()),
                    Value::Text(child.as_str().to_owned()),
                ],
            ),
        ];
        self.commit_effects(
            "store_child_resume_record",
            &[
                format!("child:{}", child.as_str()),
                format!("status:{}", record.status.as_str()),
            ],
            effects,
            child,
        )
        .await
        .map(|_| ())
    }
}

pub(super) fn guard_session_statement(id: &ThreadId) -> StatementSpec {
    StatementSpec::new(
        GUARD_SESSION_ABSENT_SQL,
        vec![Value::Text(id.as_str().to_owned())],
    )
}

fn guard_children_statement(id: &ThreadId) -> StatementSpec {
    StatementSpec::new(
        GUARD_CHILDREN_PRESENT_SQL,
        vec![Value::Text(id.as_str().to_owned())],
    )
}

fn timestamp() -> String {
    peri_time::now_utc_rfc3339()
}

/// 撤销前的子会话判据：只有**明确读到 0** 才继续。
///
/// `COUNT(*)` 必定返回恰好一行，读不出来（没有行或不是整数）说明这次回复不完整。此时继续
/// 删除等于用不可证明的证据做破坏性决定，因此拒绝并报错，而不是当作「没有子会话」。
fn revocation_gate(children: Option<i64>) -> SessionResourceResult<()> {
    match children {
        Some(0) => Ok(()),
        Some(_) => Err(invalid_input(
            "session has published children and cannot be revoked",
        )),
        None => Err(incomplete_reply(
            "child session count row is missing or not an integer",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::super::session_sql::{
        DELETE_SESSION_BINDINGS_SQL, DELETE_SESSION_MESSAGES_SQL, DELETE_SESSION_SQL,
    };
    use super::*;

    fn placeholders(sql: &str) -> usize {
        sql.matches('?').count()
    }

    const TREE_DELETES: [(&str, &str); 5] = [
        (DELETE_TREE_MESSAGES_SQL, "DELETE FROM messages"),
        (DELETE_TREE_BINDINGS_SQL, "DELETE FROM session_bindings"),
        (
            DELETE_TREE_CLOSE_INTENTS_SQL,
            "DELETE FROM session_close_intents",
        ),
        (DELETE_TREE_MQ_SQL, "DELETE FROM thread_mq"),
        (DELETE_TREE_THREADS_SQL, "DELETE FROM threads"),
    ];

    #[test]
    fn every_statement_is_static_and_fully_bound() {
        let cases = [
            (GUARD_SESSION_ABSENT_SQL, 1),
            (GUARD_CHILDREN_PRESENT_SQL, 1),
            (COUNT_CHILDREN_SQL, 1),
            (DELETE_SESSION_MESSAGES_SQL, 1),
            (DELETE_SESSION_BINDINGS_SQL, 1),
            (DELETE_SESSION_SQL, 1),
            (UPDATE_AGENT_STATUS_SQL, 3),
        ];
        let tree_deletes = TREE_DELETES.iter().map(|(sql, _)| (*sql, 1));
        for (sql, expected) in cases.into_iter().chain(tree_deletes) {
            assert_eq!(placeholders(sql), expected, "绑定量与占位符不一致: {sql}");
            assert!(!sql.contains('\''), "语句里出现了字面量: {sql}");
        }
    }

    /// 删除范围是整棵子树（含根），范围在**同一个批内**由递归重新计算，不依赖批外快照。
    #[test]
    fn tree_scope_is_recomputed_inside_the_delete_batch() {
        for (sql, tail) in TREE_DELETES {
            assert!(
                sql.starts_with("WITH RECURSIVE tree(id) AS ("),
                "删除范围必须由批内递归给出: {sql}"
            );
            assert!(sql.contains("UNION ALL"), "递归必须沿 parent 链展开: {sql}");
            assert!(
                sql.contains("JOIN tree t ON s.parent_thread_id = t.id"),
                "递归必须沿 parent 链展开: {sql}"
            );
            assert!(sql.contains(tail), "删除目标不匹配: {sql}");
        }
    }

    /// 递归删除必须覆盖 canonical 的每一张子表：新增 `REFERENCES threads` 子表时，
    /// `THREAD_CHILD_DELETES` 会先被本机测试拦下，这里保证远端子树删除同步跟上。
    #[test]
    fn tree_deletes_cover_every_canonical_child_table() {
        for (table, _) in crate::sessions::canonical::THREAD_CHILD_DELETES {
            let expected = format!("DELETE FROM {table} ");
            assert!(
                TREE_DELETES.iter().any(|(sql, _)| sql.contains(&expected)),
                "tree deletion does not clear canonical child table {table}"
            );
        }
    }

    /// 撤销必须先问「有没有子会话」：有子会话的 identity 被补偿掉会让子会话指向空父节点。
    #[test]
    fn revocation_checks_published_children_first() {
        assert!(COUNT_CHILDREN_SQL.contains("parent_thread_id = ?1"));
        // 预检之外还有批内守卫：计数与删除之间的并发 child 由它挡住。
        assert!(GUARD_CHILDREN_PRESENT_SQL.contains("EXISTS"));
        assert!(GUARD_CHILDREN_PRESENT_SQL.contains("parent_thread_id = ?1"));
    }

    /// 撤销门槛只有三种落点，且「读不到计数」不是「没有子会话」。
    #[test]
    fn revocation_gate_requires_a_proven_zero() {
        assert!(revocation_gate(Some(0)).is_ok());
        let published = revocation_gate(Some(2)).unwrap_err();
        assert!(matches!(
            published.kind(),
            SessionResourceErrorKind::InvalidInput { .. }
        ));
        // 计数行缺失/不是整数：拒绝（Internal），而不是放行删除。
        let unreadable = revocation_gate(None).unwrap_err();
        assert!(matches!(
            unreadable.kind(),
            SessionResourceErrorKind::Internal { .. }
        ));
        assert!(!matches!(
            unreadable.kind(),
            SessionResourceErrorKind::NotFound | SessionResourceErrorKind::Corrupt { .. }
        ));
    }

    /// 根不存在由批内守卫报 `NotFound`，不是「成功但没删任何行」。
    #[test]
    fn absent_root_is_rejected_inside_the_batch() {
        assert!(GUARD_SESSION_ABSENT_SQL.contains("NOT EXISTS"));
        assert!(GUARD_SESSION_ABSENT_SQL.contains("FROM threads WHERE id = ?1"));
        assert!(!DELETE_TREE_THREADS_SQL.contains("SELECT 1 FROM threads WHERE id = ?1"));
    }

    /// 认领标记由状态派生，不是独立列：写入只动 `agent_status` 与更新时间。
    #[test]
    fn child_resume_claim_is_derived_from_status() {
        assert!(UPDATE_AGENT_STATUS_SQL.contains("agent_status = ?1"));
        assert!(!UPDATE_AGENT_STATUS_SQL.contains("claimed"));
    }
}
