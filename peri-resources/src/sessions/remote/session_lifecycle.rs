//! 远程会话生命周期写入：legacy 接纳、未发布撤销、删除会话树、child resume 认领事实。
//!
//! ## 远端没有本机生命周期锚点
//!
//! 本机侧这些行为会同时写执行行（`execution_runs`）并在删除路径上显式结束所有权：那些是
//! **本机事实**（同步、回收、执行代际的判据），远端没有也**不得**新增——远端没有第二个副本，
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
    FrozenSnapshotBytes, SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};
use peri_acp_types::thread::{AgentStatus, ThreadId};
use peri_acp_types::workspace::{ResolvedWorkspace, SessionBinding, WorkspaceError};
use turso_serverless::Value;

use super::mutation::incomplete_reply;
use super::session_codec as codec;
use super::session_data::{invalid_input, not_found, RemoteSessionData};
use super::session_sql::{self, binding_relative_text, DELETE_SESSION_SQL};
use super::sql::{int_at, text_at, StatementSpec};
use crate::sessions::data::ChildResumeRecord;

// ─── 批内守卫 ─────────────────────────────────────────────────────────────────

const GUARD_SESSION_ABSENT_SQL: &str = "INSERT INTO peri_store_meta(singleton)
    SELECT 0 WHERE NOT EXISTS (SELECT 1 FROM threads WHERE id = ?1)";

// ─── 效果语句 ─────────────────────────────────────────────────────────────────

/// 子树 id（含根自身）；只读，用于确认删除范围。
const SELECT_TREE_IDS_SQL: &str = "WITH RECURSIVE tree(id) AS (
    SELECT id FROM threads WHERE id = ?1
    UNION ALL
    SELECT s.id FROM threads s JOIN tree t ON s.parent_thread_id = t.id
) SELECT id FROM tree";

/// 直接子会话计数（撤销必须拒绝「已有子会话」的 identity，否则子会话会指向不存在的父）。
const COUNT_CHILDREN_SQL: &str = "SELECT COUNT(*) FROM threads WHERE parent_thread_id = ?1";

/// 接纳依据：保存的绝对 cwd 与父关系。
const SELECT_ADOPT_FACTS_SQL: &str = "SELECT cwd, parent_thread_id FROM threads WHERE id = ?1";

/// 补 frozen：已有值不变（`IS NULL` 谓词即本机「已有值不变」的同一语义）。
const ADOPT_FROZEN_SQL: &str = "UPDATE threads SET frozen_context = ?2
    WHERE id = ?1 AND frozen_context IS NULL";

/// 补不可变绑定：只在 `session_bindings` 里还没有这一行时写入，之后任何行为都不改写它
/// （本机同一判定：先读已有绑定，没有才 INSERT）。
const ADOPT_BINDING_SQL: &str = "INSERT INTO session_bindings
    (thread_id, schema_version, project_id, workspace_id, relative_cwd)
    SELECT ?1, ?2, ?3, ?4, ?5
    WHERE NOT EXISTS (SELECT 1 FROM session_bindings WHERE thread_id = ?1)";

/// child resume 认领事实：状态 + 更新时间（`claimed` 由状态派生，不是独立列）。
const UPDATE_AGENT_STATUS_SQL: &str =
    "UPDATE threads SET agent_status = ?1, updated_at = ?2 WHERE id = ?3";

const SELECT_AGENT_STATUS_SQL: &str = "SELECT agent_status FROM threads WHERE id = ?1";

impl RemoteSessionData {
    /// 接纳 legacy 会话：binding 与缺失的 frozen 一次成立，已有值不变。
    ///
    /// 本机在这一步还要核对本机 workspace 登记（`workspaces` 表）——那是**本机证据**，
    /// 远端没有也不得伪造：绑定由调用方在本机解析后给出，远端只负责把事实写下去。
    pub(super) async fn adopt_legacy(
        &self,
        id: &ThreadId,
        saved_cwd: &str,
        workspace: &ResolvedWorkspace,
        frozen: &FrozenSnapshotBytes,
    ) -> SessionResourceResult<()> {
        if !std::path::Path::new(saved_cwd).is_absolute() {
            return Err(SessionResourceError::new(
                SessionResourceErrorKind::Workspace(WorkspaceError::Unavailable),
            ));
        }
        let store = self.store().await?;
        let facts = store
            .fetch_row(&StatementSpec::new(
                SELECT_ADOPT_FACTS_SQL,
                vec![Value::Text(id.as_str().to_owned())],
            ))
            .await?
            .ok_or_else(not_found)?;
        let cwd = text_at(&facts, 0).ok_or_else(|| codec::corrupt("session cwd is unreadable"))?;
        // 保存的绝对 cwd 是接纳依据；调用方不能借接纳顺手改绑，也不能接纳 child。
        if cwd != saved_cwd || text_at(&facts, 1).is_some() {
            return Err(SessionResourceError::new(
                SessionResourceErrorKind::Workspace(WorkspaceError::ExecutionBindingMismatch),
            ));
        }
        // 下面两次读取自己取连接：借用不能跨过去（重连要拿写锁，同任务里握着读锁会自锁）。
        drop(store);
        let binding_present = self.binding_of(id).await?.is_some();
        let frozen_present = self.frozen_of(id).await?.is_some();
        if binding_present && frozen_present {
            // 已经接纳过：不写、也不假装写入什么。
            return Ok(());
        }
        let mut effects = vec![guard_session_statement(id)];
        if !frozen_present {
            effects.push(StatementSpec::new(
                ADOPT_FROZEN_SQL,
                vec![
                    Value::Text(id.as_str().to_owned()),
                    Value::Text(frozen.as_str().to_owned()),
                ],
            ));
        }
        let binding = SessionBinding::from_workspace(workspace);
        if !binding_present {
            let relative = binding_relative_text(&binding)?;
            effects.push(StatementSpec::new(
                ADOPT_BINDING_SQL,
                vec![
                    Value::Text(id.as_str().to_owned()),
                    codec::int_value(i64::from(binding.schema_version)),
                    Value::Text(binding.project_id.to_string()),
                    Value::Text(binding.workspace_id.to_string()),
                    Value::Text(relative),
                ],
            ));
        }
        let inputs = vec![
            format!("id:{}", id.as_str()),
            format!("cwd:{saved_cwd}"),
            format!("frozen:{}", frozen.as_str()),
            format!("workspace:{}", workspace.workspace_id),
        ];
        self.commit_effects("adopt_legacy_session", &inputs, effects, id)
            .await
            .map(|_| ())
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
        self.commit_effects(behavior, &[format!("id:{}", id.as_str())], effects, id)
            .await
    }

    /// 删除会话树：子树（含根）的历史与会话行在同一批里消失。
    ///
    /// 删除是刻意行为：没有墓碑、没有执行行清理（远端都没有这些事实），但**不留半棵**——
    /// 子树 id 先只读确认，删除在同一托管批内完成。
    ///
    /// 根是否存在**只由这次子树读取决定**（不再先问一次 `exists`，两次读取之间的空档会让
    /// 「刚被删掉的根」既非存在也非不存在）：`SELECT_TREE_IDS_SQL` 的递归从根行出发，所以
    /// 空子树等价于「根不存在」→ `NotFound`，与本机 `delete_tree` 同一结果。反过来，空结果
    /// **不能**当成「没有东西要删」而报成功——那会在没删任何行的情况下返回 `Ok`。
    pub(super) async fn write_tree_deletion(&self, id: &ThreadId) -> SessionResourceResult<()> {
        let store = self.store().await?;
        let rows = store
            .fetch_rows(&StatementSpec::new(
                SELECT_TREE_IDS_SQL,
                vec![Value::Text(id.as_str().to_owned())],
            ))
            .await?;
        // 树上的 id 读完再写：借用不跨到后面的写入路径（见上）。
        drop(store);
        let tree = tree_ids(&rows)?;
        // 删除不新增墓碑：树上的每个会话先清子行（messages → session_bindings，
        // 与 `canonical::THREAD_CHILD_DELETES` 同一份语句与顺序）再清会话行，
        // 全部在同一个批里。
        let mut effects = Vec::with_capacity(tree.len() * 4);
        for (_, statement) in crate::sessions::canonical::THREAD_CHILD_DELETES {
            for thread in &tree {
                effects.push(StatementSpec::new(
                    statement,
                    vec![Value::Text(thread.clone())],
                ));
            }
        }
        for thread in &tree {
            effects.push(StatementSpec::new(
                DELETE_SESSION_SQL,
                vec![Value::Text(thread.clone())],
            ));
        }
        // 删除的摘要输入 = 目标本身 + 这次实际命中的子树（同一根下删掉了哪些会话构成这次操作）。
        let mut inputs = vec![format!("id:{}", id.as_str())];
        inputs.extend(tree.iter().cloned());
        self.commit_effects("delete_session_tree", &inputs, effects, id)
            .await
            .map(|_| ())
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

fn guard_session_statement(id: &ThreadId) -> StatementSpec {
    StatementSpec::new(
        GUARD_SESSION_ABSENT_SQL,
        vec![Value::Text(id.as_str().to_owned())],
    )
}

fn timestamp() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// 子树读取 → 会话 id 列表。
///
/// 空结果**不是**「没有东西要删」：递归从根行开始，读不到根就是根不存在 → `NotFound`
/// （与本机 `delete_tree` 对不存在会话的同一结果），而不是一次「成功但什么都没删」。
fn tree_ids(rows: &[Vec<Value>]) -> SessionResourceResult<Vec<String>> {
    if rows.is_empty() {
        return Err(not_found());
    }
    rows.iter()
        .map(|row| {
            text_at(row, 0)
                .map(str::to_owned)
                .ok_or_else(|| codec::corrupt("session tree row is not a session id"))
        })
        .collect()
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
    use super::super::session_sql::{DELETE_SESSION_BINDINGS_SQL, DELETE_SESSION_MESSAGES_SQL};
    use super::*;

    fn placeholders(sql: &str) -> usize {
        sql.matches('?').count()
    }

    #[test]
    fn every_statement_is_static_and_fully_bound() {
        let cases: [(&str, usize); 8] = [
            (GUARD_SESSION_ABSENT_SQL, 1),
            (SELECT_TREE_IDS_SQL, 1),
            (COUNT_CHILDREN_SQL, 1),
            (SELECT_ADOPT_FACTS_SQL, 1),
            (DELETE_SESSION_MESSAGES_SQL, 1),
            (DELETE_SESSION_BINDINGS_SQL, 1),
            (DELETE_SESSION_SQL, 1),
            (UPDATE_AGENT_STATUS_SQL, 3),
        ];
        for (sql, expected) in cases {
            assert_eq!(placeholders(sql), expected, "绑定量与占位符不一致: {sql}");
            assert!(!sql.contains('\''), "语句里出现了字面量: {sql}");
        }
    }

    /// 接纳只补缺失的事实：`IS NULL` 谓词就是本机「已有值不变」的同一语义。
    #[test]
    fn adopt_only_fills_missing_facts() {
        assert!(ADOPT_FROZEN_SQL.contains("AND frozen_context IS NULL"));
        assert!(ADOPT_BINDING_SQL.contains("WHERE NOT EXISTS (SELECT 1 FROM session_bindings"));
        // 不可变绑定没有「改绑」路径：接纳只在缺行时插入一次，任何路径都不 UPDATE 它。
        assert!(!ADOPT_BINDING_SQL.contains("UPDATE"));
    }

    /// 删除范围是整棵子树（含根），且删除语句不含任何计算——范围完全由绑定参数给出。
    #[test]
    fn tree_scope_is_the_whole_subtree() {
        assert!(SELECT_TREE_IDS_SQL.starts_with("WITH RECURSIVE tree(id) AS ("));
        assert!(SELECT_TREE_IDS_SQL.contains("UNION ALL"));
        assert!(SELECT_TREE_IDS_SQL
            .trim_end()
            .ends_with("SELECT id FROM tree"));
        assert_eq!(DELETE_SESSION_SQL, "DELETE FROM threads WHERE id = ?1");
        assert_eq!(
            DELETE_SESSION_MESSAGES_SQL,
            "DELETE FROM messages WHERE thread_id = ?1"
        );
        assert_eq!(
            DELETE_SESSION_BINDINGS_SQL,
            "DELETE FROM session_bindings WHERE thread_id = ?1"
        );
    }

    /// 撤销必须先问「有没有子会话」：有子会话的 identity 被补偿掉会让子会话指向空父节点。
    #[test]
    fn revocation_checks_published_children_first() {
        assert!(COUNT_CHILDREN_SQL.contains("parent_thread_id = ?1"));
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

    /// 空子树是「根不存在」（`NotFound`），不是「成功但没删任何行」。
    #[test]
    fn empty_subtree_is_not_found_and_rows_must_be_ids() {
        let absent = tree_ids(&[]).unwrap_err();
        assert!(matches!(absent.kind(), SessionResourceErrorKind::NotFound));

        let malformed = tree_ids(&[vec![Value::Integer(1)]]).unwrap_err();
        assert!(matches!(
            malformed.kind(),
            SessionResourceErrorKind::Corrupt { .. }
        ));

        let ids = tree_ids(&[
            vec![Value::Text("root".to_owned())],
            vec![Value::Text("child".to_owned())],
        ])
        .unwrap();
        assert_eq!(ids, vec!["root".to_owned(), "child".to_owned()]);
    }

    /// 认领标记由状态派生，不是独立列：写入只动 `agent_status` 与更新时间。
    #[test]
    fn child_resume_claim_is_derived_from_status() {
        assert!(UPDATE_AGENT_STATUS_SQL.contains("agent_status = ?1"));
        assert!(!UPDATE_AGENT_STATUS_SQL.contains("claimed"));
    }
}
