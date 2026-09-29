//! 本机执行面：发现、登记、owner、dirty、创建准入与关闭。
//!
//! 本模块是「本机事实」的唯一持有者：项目/工作区证据来自发现（[`super::discovery`]），
//! 执行所有权来自 `execution_runs` 与 sidecar OS 锁，创建准入把两者与 durable 数据
//! 按本地事实组合起来。数据面（[`super::session_data`]）只回答数据事实，不判断
//! 「这条会话在本机能不能执行」。
//!
//! 门面（[`crate::sessions::SessionResourcesImpl`]）是唯一调用方；本类型不导出给
//! 业务侧，也不提供无 guard 的写入入口。
//!
//! v10 之后它是 [`LocalExecutionPort`] 的**唯一**实现：远端组合的 canonical 数据在远端，
//! 但执行事实（workspace 证据、执行代际、OS 锁、owner）仍只写在本机库，绑定字节与父链由
//! 数据端口提供。执行域因此只有一个——按 `thread_id` 原文，没有 store 维度。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use peri_acp_types::session_resources::{NewSession, SessionResourceError, SessionResourceResult};
use peri_acp_types::thread::ThreadId;
use peri_acp_types::workspace::{
    RecoveryRequiredDetails, ResolvedWorkspace, SessionBinding, SessionExecutionLease,
};

use super::connection::ReadOnlyThreadStoreError;
use super::database::SqliteSessionDatabase;
use super::execution::{
    local_lock_name, ExclusiveExecutionGuard, ExecutionLease, ExecutionWriteGuard,
};
use super::failure::{
    binding_relation_failure, commit_failure, execution_failure, lease_required, map_sqlx,
};
use super::session_data::SqliteSessionData;
use super::session_rows::{delete_thread_child_rows, insert_binding_row, insert_thread_row};
use crate::sessions::local_port::{LocalExecutionPort, RevokeEffect, SessionFacts};

/// 本机执行面的句柄：与数据面共用同一个库（同一条连接真相）。
#[derive(Clone)]
pub(in crate::sessions) struct LocalExecution {
    database: Arc<SqliteSessionDatabase>,
}

impl LocalExecution {
    pub(in crate::sessions) async fn open(db_path: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self {
            database: Arc::new(SqliteSessionDatabase::open(db_path).await?),
        })
    }

    /// 复用既有库句柄：迁移期桥与门面必须指向同一份连接与 owner 登记。
    pub(in crate::sessions) fn from_shared_database(database: Arc<SqliteSessionDatabase>) -> Self {
        Self { database }
    }

    pub(in crate::sessions) async fn open_existing_read_only(
        db_path: impl AsRef<Path>,
    ) -> std::result::Result<Self, ReadOnlyThreadStoreError> {
        Ok(Self {
            database: Arc::new(SqliteSessionDatabase::open_existing_read_only(db_path).await?),
        })
    }

    /// 默认数据库位置 `~/.peri/threads/threads.db`；不创建目录、数据库或连接。
    pub(in crate::sessions) fn default_database_path() -> Result<PathBuf> {
        SqliteSessionDatabase::default_database_path()
    }

    /// 数据面句柄：只有门面持有它，业务侧拿不到。
    pub(in crate::sessions) fn data_port(&self) -> SqliteSessionData {
        SqliteSessionData::new(Arc::clone(&self.database))
    }

    pub(in crate::sessions) fn is_read_only(&self) -> bool {
        self.database.is_read_only()
    }

    /// 测试用：直接读库内事实（生产侧由各行为自己的后置条件覆盖）。
    #[cfg(test)]
    pub(in crate::sessions) fn pool(&self) -> &sqlx::SqlitePool {
        &self.database.pool
    }

    // ── 发现与登记 ────────────────────────────────────────────────────────────

    /// 解析并登记本机执行目录（只读打开时按 `WorkspaceError::ReadOnlyStore` 失败）。
    pub(in crate::sessions) async fn resolve_workspace(
        &self,
        cwd: &Path,
    ) -> Result<ResolvedWorkspace> {
        self.database.resolve_workspace_impl(cwd).await
    }

    /// 用调用方给出的绑定字节做本机复核（本机组合来自 `session_bindings`，远端组合来自
    /// 远端会话行）。
    ///
    /// `full` 为真时叠一次完整发现快照比对（一次准入的权威复核），否则只查关系与关键
    /// 文件对象（准入内的复核）。两种模式走的是同一套判定，因此「能不能执行」与
    /// 「绑定向哪里」不会出现两套结论。
    pub(in crate::sessions) async fn validate_binding_value(
        &self,
        binding: &SessionBinding,
        full: bool,
    ) -> Result<ResolvedWorkspace> {
        self.database
            .validate_binding_value_impl(binding, full)
            .await
    }

    /// 本进程当前持有的全部活 owner（关闭协调用；不跨进程探测）。
    pub(in crate::sessions) fn live_leases(&self) -> Vec<Arc<ExecutionLease>> {
        let Ok(map) = self.database.execution_leases.lock() else {
            return Vec::new();
        };
        map.values().filter_map(std::sync::Weak::upgrade).collect()
    }

    /// 放弃一次未发布创建的所有权并执行补偿。
    ///
    /// 传入的 lease 必须是**本进程这条 identity 的活 owner**：撤销会删除执行行，
    /// 不能让另一个 owner（或另一条会话的 lease）替它承担补偿。补偿动作由调用方给出
    /// （数据面的撤销行为），本函数只负责准入顺序：关闭准入 → 等待在途写入 → 补偿 →
    /// 释放 OS 锁。
    ///
    /// 所有权按**精确 identity** 认：撤销的对象是这次创建的那条会话，它的租约就登记在这个
    /// id 上（子会话的写入另走 root 的门禁，不参与撤销）。
    pub(in crate::sessions) async fn abandon_initialization<F, Fut>(
        &self,
        id: &ThreadId,
        lease: &Arc<dyn SessionExecutionLease>,
        revoke: F,
    ) -> SessionResourceResult<()>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = SessionResourceResult<()>>,
    {
        let owned = self
            .database
            .registered_lease(id)
            .map_err(execution_failure)?
            .ok_or_else(lease_required)?;
        if !same_lease(&owned, lease) {
            return Err(lease_required());
        }
        owned.abandon_ownership(revoke).await
    }

    /// legacy 来源证据：无绑定、无父会话、无执行代际，且保存的绝对 cwd 落在本机已登记
    /// 工作区内。
    ///
    /// 这是「这条历史来自本机某个已登记目录」的证据，不是「可以执行」的许可；接纳本身
    /// 仍由数据面在写事务内复核（保存路径一致、登记关系一致、既有绑定只校验不覆盖）。
    pub(in crate::sessions) async fn legacy_confirmed(&self, id: &ThreadId) -> Result<bool> {
        let row: Option<(String, Option<String>)> =
            sqlx::query_as("SELECT cwd, parent_thread_id FROM threads WHERE id = ?1")
                .bind(id.as_str())
                .fetch_optional(&self.database.pool)
                .await?;
        let Some((cwd, parent)) = row else {
            return Ok(false);
        };
        if parent.is_some() {
            return Ok(false);
        }
        if self.database.load_session_binding_impl(id).await?.is_some() {
            return Ok(false);
        }
        if self.database.load_execution_state(id).await?.is_some() {
            return Ok(false);
        }
        let cwd = PathBuf::from(cwd);
        if !cwd.is_absolute() {
            return Ok(false);
        }
        // 两侧是不同时刻写入的字符串：同一目录可能一侧已解析、另一侧仍是符号链接
        // 路径（macOS 的 /var 与 /private/var）。按字面比较会把本机 legacy 误判成
        // 外来会话，因此按文件系统事实比较。
        let Ok(cwd) = cwd.canonicalize() else {
            return Ok(false);
        };
        let roots: Vec<(String,)> = sqlx::query_as("SELECT root FROM workspaces")
            .fetch_all(&self.database.pool)
            .await?;
        Ok(roots.iter().any(|(root,)| {
            Path::new(root)
                .canonicalize()
                .map(|root| cwd.starts_with(root))
                .unwrap_or(false)
        }))
    }

    // ── owner / dirty ─────────────────────────────────────────────────────────

    /// 沿 root 关系找到活 owner；`None` 表示这棵树既无绑定也无 owner。
    pub(in crate::sessions) async fn owner_lease(
        &self,
        id: &ThreadId,
        facts: &SessionFacts,
    ) -> Result<Option<Arc<ExecutionLease>>> {
        self.database.owner_lease(id, facts).await
    }

    /// 诊断读取：本进程是否持有这棵树的 owner（没有不构成错误）。
    pub(in crate::sessions) async fn live_owner(
        &self,
        id: &ThreadId,
        facts: &SessionFacts,
    ) -> Result<Option<Arc<ExecutionLease>>> {
        self.database.live_owner_lease(id, facts).await
    }

    /// 本机执行代际事实；不创建锁文件。
    pub(in crate::sessions) async fn execution_state(
        &self,
        id: &ThreadId,
    ) -> Result<Option<(i64, bool)>> {
        self.database.load_execution_state(id).await
    }

    /// 读侧写入准入（允许同 root 并发 mutation）。
    pub(in crate::sessions) async fn write_guard(
        &self,
        id: &ThreadId,
        facts: &SessionFacts,
    ) -> Result<Option<ExecutionWriteGuard>> {
        self.database.require_execution_lease(id, facts).await
    }

    /// 写侧写入准入（把「检查 + 写入」做成不可插入的区间）。
    pub(in crate::sessions) async fn exclusive_guard(
        &self,
        id: &ThreadId,
        facts: &SessionFacts,
    ) -> Result<Option<ExclusiveExecutionGuard>> {
        self.database.exclusive_execution_guard(id, facts).await
    }

    /// 取得已有会话的执行所有权。
    pub(in crate::sessions) async fn acquire_lease(
        &self,
        id: &ThreadId,
        facts: &SessionFacts,
    ) -> Result<Arc<dyn SessionExecutionLease>> {
        self.database.acquire_execution_lease_impl(id, facts).await
    }

    /// 解除精确代际的 dirty（CAS 在实现内部，不跨接口传递）。
    pub(in crate::sessions) async fn reset_dirty(
        &self,
        target: &RecoveryRequiredDetails,
    ) -> Result<()> {
        self.database.reset_dirty_execution_impl(target).await
    }

    // ── 删除后的收尾 ─────────────────────────────────────────────────────────

    /// 会话数据已被删除：结束这条 identity 的本机执行事实与所有权。
    ///
    /// 顺序是「先抹掉代际行，再放所有权」：抹掉之后即使本进程崩溃，剩下的也只是锁文件
    /// （进程退出即释放），不会留下一条描述不存在会话的代际行。没有活 owner 不是错误
    /// ——删除可能是由另一个仍持有 owner 的调用方完成的，本机这时没有可结束的所有权。
    ///
    /// 所有权按**精确 identity** 认（[`SqliteSessionDatabase::registered_lease`]）：数据已经被删，
    /// 这条会话的父链此刻无从解析，能证明的只有「本进程持有这条 identity 的租约」。因此删除
    /// 一棵**子树**只结束这棵子树里本进程持有的所有权，不会连带结束它 root 的所有权。
    pub(in crate::sessions) async fn dispose_execution(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<()> {
        if self.database.is_read_only() {
            // 只读打开不会取得 owner，也没有删除路径能走到这里（门面在写入准入就拒绝）。
            // 这里不写库也不改状态：本机执行事实不归只读的一次打开处置。
            return Ok(());
        }
        self.database
            .delete_execution_state(id)
            .await
            .map_err(execution_failure)?;
        if let Some(lease) = self
            .database
            .registered_lease(id)
            .map_err(execution_failure)?
        {
            lease.dispose_ownership().await;
        }
        Ok(())
    }

    // ── 创建准入（本地塌缩） ───────────────────────────────────────────────────

    /// 新建会话：OS 预留 → 完整数据与执行代际同一事务 → 返回 owner。
    ///
    /// 本地同库让 `threads` → `session_bindings` → frozen → `execution_runs` 落在同一个
    /// `BEGIN IMMEDIATE` 里，因此创建意图、数据保存与执行准入在本地是**一次提交**：
    /// 事务失败则什么都没保存（锁文件句柄随返回值释放），提交则数据完整且已有一个
    /// 未结清的执行代际（`clean = 0`），进程在提交后崩溃也只是普通 dirty，恢复依据是
    /// 完整数据加代际本身，不需要重造 binding/frozen。
    pub(in crate::sessions) async fn create_with_lease(
        &self,
        input: &NewSession,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>> {
        if self.database.is_read_only() {
            return Err(super::failure::read_only_store());
        }
        // 先占稳定 OS 锁：同一 identity 的创建与他处执行互斥，且在写库之前就排除。
        let file = self
            .database
            .lock_execution(&local_lock_name(&input.thread_id))
            .await
            .map_err(execution_failure)?;
        let snapshot_at = input
            .meta
            .snapshot_at_message_id
            .map(|id| id.as_uuid().to_string());
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        // 绑定关系与关键文件对象在写事务内复核：未登记的 project/workspace 给出
        // workspace 语义的失败，而不是留到最后变成外键错误。
        let resolved = SqliteSessionDatabase::validate_binding_relation_on(&mut tx, &input.binding)
            .await
            .map_err(binding_relation_failure)?;
        // `threads.cwd` 以已复核的绑定为准：同一份事实只有一个来源，调用方给的 cwd
        // 不再构成第二个真相。
        let binding_cwd =
            super::discovery::path_text(&resolved.cwd).map_err(super::failure::write_failure)?;
        let mut row = super::session_data::new_session_row(
            input,
            snapshot_at.as_deref(),
            Some(input.frozen.as_str()),
            0,
        );
        row.cwd = binding_cwd;
        insert_thread_row(&mut tx, &row)
            .await
            .map_err(super::failure::write_failure)?;
        insert_binding_row(&mut tx, &input.thread_id, &input.binding)
            .await
            .map_err(super::failure::write_failure)?;
        sqlx::query("INSERT INTO execution_runs (thread_id, generation, clean) VALUES (?1, 1, 0)")
            .bind(input.thread_id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(input.thread_id.clone())))?;
        self.register_lease(input.thread_id.clone(), 1, Some(file))
    }

    /// 未发布创建的第一阶段：OS 预留 → 草稿数据与执行代际同一事务 → 返回 owner。
    ///
    /// 与 [`Self::create_with_lease`] 逐字同形状，唯一差别是 `frozen_context` 写 `NULL`：
    /// 内容准入（资源读取后定稿）需要所有权先成立，因此身份/绑定/代际先在此成立，
    /// frozen 由 [`Self::commit_frozen`] 一次性补上。草稿不可执行：它不在任何会话表里，
    /// 也没有 frozen 可被冷恢复解释（半写草稿按 typed 错误 fail-closed）。
    pub(in crate::sessions) async fn begin_initialization(
        &self,
        draft: &peri_acp_types::session_resources::NewSessionDraft,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>> {
        if self.database.is_read_only() {
            return Err(super::failure::read_only_store());
        }
        let file = self
            .database
            .lock_execution(&local_lock_name(&draft.thread_id))
            .await
            .map_err(execution_failure)?;
        let snapshot_at = draft
            .meta
            .snapshot_at_message_id
            .map(|id| id.as_uuid().to_string());
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        let resolved = SqliteSessionDatabase::validate_binding_relation_on(&mut tx, &draft.binding)
            .await
            .map_err(binding_relation_failure)?;
        let binding_cwd =
            super::discovery::path_text(&resolved.cwd).map_err(super::failure::write_failure)?;
        let mut row = super::session_data::new_session_draft_row(draft, snapshot_at.as_deref(), 0);
        row.cwd = binding_cwd;
        insert_thread_row(&mut tx, &row)
            .await
            .map_err(super::failure::write_failure)?;
        insert_binding_row(&mut tx, &draft.thread_id, &draft.binding)
            .await
            .map_err(super::failure::write_failure)?;
        sqlx::query("INSERT INTO execution_runs (thread_id, generation, clean) VALUES (?1, 1, 0)")
            .bind(draft.thread_id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(draft.thread_id.clone())))?;
        self.register_lease(draft.thread_id.clone(), 1, Some(file))
    }

    /// 一次性提交 frozen：owner/代际校验与 write-once CAS 在同一 `BEGIN IMMEDIATE` 内。
    ///
    /// 三条判据都必须成立，否则 typed 冲突且**不写入**：
    ///
    /// 1. `lease` 是本进程这条 identity 的**活 owner**（撤销/提交只能由 owner 发起）；
    /// 2. 这条 identity 的执行代际仍是 `clean = 0`（已结清的草稿不允许再定稿内容）；
    /// 3. `UPDATE ... WHERE id = ? AND frozen_context IS NULL` 恰好命中一行（write-once）。
    pub(in crate::sessions) async fn commit_frozen(
        &self,
        id: &ThreadId,
        lease: &Arc<dyn SessionExecutionLease>,
        frozen: &peri_acp_types::session_resources::FrozenSnapshotBytes,
    ) -> SessionResourceResult<()> {
        if self.database.is_read_only() {
            return Err(super::failure::read_only_store());
        }
        // 活 owner 判定取活跃弱引用表（崩溃即失效），与撤销同一条判据。
        let owned = self
            .database
            .registered_lease(id)
            .map_err(execution_failure)?
            .filter(|owned| owned.is_active())
            .ok_or_else(lease_required)?;
        if !same_lease(&owned, lease) {
            return Err(lease_required());
        }
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        let execution: Option<(i64, bool)> =
            sqlx::query_as("SELECT generation, clean FROM execution_runs WHERE thread_id = ?1")
                .bind(id.as_str())
                .fetch_optional(&mut *tx)
                .await
                .map_err(|error| map_sqlx(&error))?;
        match execution {
            Some((_, false)) => {}
            Some((_, true)) => {
                return Err(super::failure::conflict(
                    "execution generation is already settled",
                ))
            }
            None => {
                return Err(super::failure::conflict(
                    "draft has no execution generation",
                ))
            }
        }
        let updated = sqlx::query(
            "UPDATE threads SET frozen_context = ?1 WHERE id = ?2 AND frozen_context IS NULL",
        )
        .bind(frozen.as_str())
        .bind(id.as_str())
        .execute(&mut *tx)
        .await
        .map_err(|error| map_sqlx(&error))?
        .rows_affected();
        if updated != 1 {
            return Err(super::failure::conflict(
                "session frozen snapshot was already committed",
            ));
        }
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(id.clone())))?;
        Ok(())
    }

    /// 提交前后的本机资格复核：活 owner 是这一条 identity 的本次租约，且代际未结清。
    pub(in crate::sessions) async fn verify_initialization_owner(
        &self,
        id: &ThreadId,
        lease: &Arc<dyn SessionExecutionLease>,
    ) -> SessionResourceResult<()> {
        let owned = self
            .database
            .registered_lease(id)
            .map_err(execution_failure)?
            .filter(|owned| owned.is_active())
            .ok_or_else(lease_required)?;
        if !same_lease(&owned, lease) {
            return Err(lease_required());
        }
        match self
            .database
            .load_execution_state(id)
            .await
            .map_err(execution_failure)?
        {
            Some((_, false)) => Ok(()),
            _ => Err(super::failure::conflict(
                "draft execution generation is not open",
            )),
        }
    }

    /// 半写草稿的清理：判据成立、无活 owner、拿到 OS 锁之后，删除草稿的全部行。
    ///
    /// 判据（§5.3，与 legacy「无绑定且无代际行」互斥）：
    ///
    /// - 有绑定行（`session_bindings` 命中）**且** `threads.frozen_context IS NULL`；
    /// - 已提交 frozen 的会话**不是**半写草稿：返回 typed 冲突，交给既有 dirty 恢复；
    /// - 本进程有活 owner、或 OS 锁被其他进程持有 ⇒ 拒绝（那不是崩溃残留）。
    ///
    /// 删除在一条 `BEGIN IMMEDIATE` 里完成（执行代际 + 子表 + threads 行），与
    /// [`super::session_data`] 的撤销同一顺序、同一判据。
    pub(in crate::sessions) async fn discard_incomplete_initialization(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<()> {
        if self.database.is_read_only() {
            return Err(super::failure::read_only_store());
        }
        let frozen: Option<(Option<String>,)> =
            sqlx::query_as("SELECT frozen_context FROM threads WHERE id = ?1")
                .bind(id.as_str())
                .fetch_optional(&self.database.pool)
                .await
                .map_err(|error| map_sqlx(&error))?;
        let Some((frozen,)) = frozen else {
            // 行已不在：目标已达成（清理幂等），不把「已经不存在」当成失败。
            return Ok(());
        };
        if frozen.is_some() {
            return Err(super::failure::conflict(
                "session already has a committed frozen snapshot",
            ));
        }
        if self
            .database
            .load_session_binding_impl(id)
            .await
            .map_err(execution_failure)?
            .is_none()
        {
            return Err(super::failure::conflict(
                "session has no binding and is not an incomplete draft",
            ));
        }
        if let Some(lease) = self
            .database
            .registered_lease(id)
            .map_err(execution_failure)?
        {
            if lease.is_active() {
                return Err(super::failure::conflict(
                    "session is still owned by a live initialization",
                ));
            }
        }
        let _file = self
            .database
            .lock_execution(&local_lock_name(id))
            .await
            .map_err(execution_failure)?;
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        sqlx::query("DELETE FROM execution_runs WHERE thread_id = ?1")
            .bind(id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        delete_thread_child_rows(&mut tx, id.as_str())
            .await
            .map_err(|error| map_sqlx(&error))?;
        sqlx::query(super::session_rows::DELETE_THREAD_SQL)
            .bind(id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(id.clone())))?;
        Ok(())
    }

    /// 为一个已保存完整数据、但还没有执行代际的会话建立准入（收敛，不是重建）。
    ///
    /// 只用于 [`Self::create_with_lease`] 之外留下的 `data_saved` 状态：数据面已确认
    /// 保存完整（远程保存、或进程在准入前结束），此时不能重造 binding/frozen，也不能
    /// 报「确定未创建」。
    ///
    /// **绑定字节由调用方给出**（来自数据端口：本机组合是本机 `session_bindings` 行，
    /// 远程组合是远端会话行）。本机只做本机的事：执行代际的唯一性、sidecar 锁、以及
    /// 「这组绑定字节指向本机已登记的 workspace」这一条复核——canonical 会话是否存在
    /// 由数据面回答，本机不查 `threads`/`session_bindings`（远程组合里它们本来就没有这
    /// 条会话的行）。
    pub(in crate::sessions) async fn admit_existing(
        &self,
        id: &ThreadId,
        binding: &SessionBinding,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>> {
        if self.database.is_read_only() {
            return Err(super::failure::read_only_store());
        }
        let existing = self
            .database
            .load_execution_state(id)
            .await
            .map_err(execution_failure)?;
        if existing.is_some() {
            // 已有代际的会话走正常取得所有权路径（含 dirty 判定），不在这里插队。
            return Err(super::failure::invalid_input(
                "session already has an execution generation",
            ));
        }
        let file = self
            .database
            .lock_execution(&local_lock_name(id))
            .await
            .map_err(execution_failure)?;
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        // 业务前提必须仍然成立：绑定关系与关键对象在准入这一刻复核。
        SqliteSessionDatabase::validate_binding_relation_on(&mut tx, binding)
            .await
            .map_err(binding_relation_failure)?;
        sqlx::query("INSERT INTO execution_runs (thread_id, generation, clean) VALUES (?1, 1, 0)")
            .bind(id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(id.clone())))?;
        self.register_lease(id.clone(), 1, Some(file))
    }

    /// 登记租约并返回：`ExecutionLease` 的持有者是调用方，库里只留弱引用用于复核准入。
    fn register_lease(
        &self,
        id: ThreadId,
        generation: i64,
        file: Option<std::fs::File>,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>> {
        let key = id.clone();
        let lease = Arc::new(ExecutionLease::new(
            id.clone(),
            generation,
            self.database.pool.clone(),
            file,
        ));
        self.database
            .execution_leases
            .lock()
            .map_err(|_| lease_registration_failure(&id))?
            .insert(key, Arc::downgrade(&lease));
        Ok(lease)
    }
}

/// 登记失败时数据已经提交：这条会话存在且有一个未结清的执行代际，只是本次没能拿到
/// owner。这个结果必须按「已保存、未准入」上报，不能谎称「没有生效」。
fn lease_registration_failure(id: &ThreadId) -> SessionResourceError {
    SessionResourceError::saved_but_not_admitted(id.clone())
}

/// 传入的 `Arc<dyn SessionExecutionLease>` 是否就是本进程登记的那条租约。
///
/// 比较的是同一个分配对象的地址：`Arc<dyn Trait>` 与 `Arc<Concrete>` 互转只能靠地址，
/// 而这里要回答的正是「是不是同一次所有权」，不是「是不是同一个会话」。
pub(in crate::sessions) fn same_lease(
    owned: &Arc<ExecutionLease>,
    lease: &Arc<dyn SessionExecutionLease>,
) -> bool {
    std::ptr::eq(
        Arc::as_ptr(owned) as *const (),
        Arc::as_ptr(lease) as *const (),
    )
}

/// 本机执行面对门面的行为：全部委托到本文件的方法。
///
/// 这层转发不改变任何语义（同一份 SQL、同一套锁与代际），它的存在只是让门面按端口调用，
/// 从而与远程组合共用一套公开行为。
#[async_trait::async_trait]
impl LocalExecutionPort for LocalExecution {
    fn is_read_only(&self) -> bool {
        self.is_read_only()
    }

    async fn resolve_workspace(&self, cwd: &Path) -> Result<ResolvedWorkspace> {
        self.resolve_workspace(cwd).await
    }

    async fn validate_binding_value(
        &self,
        binding: &SessionBinding,
        full: bool,
    ) -> Result<ResolvedWorkspace> {
        self.validate_binding_value(binding, full).await
    }

    async fn legacy_confirmed(&self, id: &ThreadId) -> Result<bool> {
        self.legacy_confirmed(id).await
    }

    async fn execution_state(&self, id: &ThreadId) -> Result<Option<(i64, bool)>> {
        self.execution_state(id).await
    }

    async fn owner_lease(
        &self,
        id: &ThreadId,
        facts: &SessionFacts,
    ) -> Result<Option<Arc<ExecutionLease>>> {
        self.owner_lease(id, facts).await
    }

    async fn dispose_execution(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.dispose_execution(id).await
    }

    async fn live_owner(
        &self,
        id: &ThreadId,
        facts: &SessionFacts,
    ) -> Result<Option<Arc<ExecutionLease>>> {
        self.live_owner(id, facts).await
    }

    fn live_leases(&self) -> Vec<Arc<ExecutionLease>> {
        self.live_leases()
    }

    async fn write_guard(
        &self,
        id: &ThreadId,
        facts: &SessionFacts,
    ) -> Result<Option<ExecutionWriteGuard>> {
        self.write_guard(id, facts).await
    }

    async fn exclusive_guard(
        &self,
        id: &ThreadId,
        facts: &SessionFacts,
    ) -> Result<Option<ExclusiveExecutionGuard>> {
        self.exclusive_guard(id, facts).await
    }

    async fn acquire_lease(
        &self,
        id: &ThreadId,
        facts: &SessionFacts,
    ) -> Result<Arc<dyn SessionExecutionLease>> {
        self.acquire_lease(id, facts).await
    }

    async fn reset_dirty(&self, target: &RecoveryRequiredDetails) -> Result<()> {
        self.reset_dirty(target).await
    }

    async fn create_session(
        &self,
        input: &NewSession,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>> {
        self.create_with_lease(input).await
    }

    async fn begin_initialization(
        &self,
        draft: &peri_acp_types::session_resources::NewSessionDraft,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>> {
        self.begin_initialization(draft).await
    }

    async fn commit_frozen(
        &self,
        id: &ThreadId,
        lease: &Arc<dyn SessionExecutionLease>,
        frozen: &peri_acp_types::session_resources::FrozenSnapshotBytes,
    ) -> SessionResourceResult<()> {
        self.commit_frozen(id, lease, frozen).await
    }

    async fn discard_incomplete_initialization(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.discard_incomplete_initialization(id).await
    }

    async fn verify_initialization_owner(
        &self,
        id: &ThreadId,
        lease: &Arc<dyn SessionExecutionLease>,
    ) -> SessionResourceResult<()> {
        self.verify_initialization_owner(id, lease).await
    }

    async fn admit_existing(
        &self,
        id: &ThreadId,
        binding: &SessionBinding,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>> {
        self.admit_existing(id, binding).await
    }

    async fn abandon_initialization(
        &self,
        id: &ThreadId,
        lease: &Arc<dyn SessionExecutionLease>,
        revoke: RevokeEffect<'_>,
    ) -> SessionResourceResult<()> {
        self.abandon_initialization(id, lease, move || revoke).await
    }

    #[cfg(test)]
    fn sqlite_pool(&self) -> Option<&sqlx::SqlitePool> {
        Some(self.pool())
    }
}
