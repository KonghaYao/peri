//! 本机执行面：发现、登记、运行句柄、创建准入与关闭。
//!
//! 本模块是「本机事实」的唯一持有者：项目/工作区证据来自发现（[`super::discovery`]），
//! 运行句柄和未知写入效果仅驻留内存，创建事务只保存 canonical 数据。
//! 数据面（[`super::session_data`]）只回答数据事实，不判断
//! 「这条会话在本机能不能执行」。
//!
//! 门面（[`crate::sessions::SessionResourcesImpl`]）是唯一调用方；本类型不导出给
//! 业务侧，也不提供无 guard 的写入入口。
//!
//! 本类型只服务本地 SQLite locator。Turso locator 使用独立的进程内执行端口。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use peri_acp_types::session_resources::{
    MutationOutcome, NewSession, SessionResourceError, SessionResourceResult,
};
use peri_acp_types::thread::ThreadId;
use peri_acp_types::workspace::{ResolvedWorkspace, SessionBinding, SessionExecutionLease};

use super::connection::ReadOnlyThreadStoreError;
use super::database::SqliteSessionDatabase;
use super::execution::{ExclusiveExecutionGuard, ExecutionLease, ExecutionWriteGuard};
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
        map.values().cloned().collect()
    }

    /// 放弃一次未发布创建的所有权并执行补偿。
    ///
    /// 传入的 lease 必须是**本进程这条 identity 的活 owner**，
    /// 不能让另一个 owner（或另一条会话的 lease）替它承担补偿。补偿动作由调用方给出
    /// （数据面的撤销行为），本函数只负责准入顺序：关闭准入 → 等待在途写入 → 补偿 →
    /// 结束本次运行句柄。
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
        if !crate::sessions::execution::same_lease(&owned, lease) {
            return Err(lease_required());
        }
        owned
            .abandon_ownership(move || async move {
                let current = self
                    .database
                    .registered_lease(id)
                    .map_err(execution_failure)?
                    .ok_or_else(lease_required)?;
                if !crate::sessions::execution::same_lease(&current, lease) {
                    return Err(lease_required());
                }
                revoke().await
            })
            .await
    }

    /// legacy 来源证据：无绑定、无父会话、无 frozen，且保存的绝对 cwd 落在本机已登记
    /// 工作区内。
    ///
    /// 这是「这条历史来自本机某个已登记目录」的证据，不是「可以执行」的许可；接纳本身
    /// 仍由数据面在写事务内复核（保存路径一致、登记关系一致、既有绑定只校验不覆盖）。
    pub(in crate::sessions) async fn legacy_confirmed(&self, id: &ThreadId) -> Result<bool> {
        let row: Option<(String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT cwd, parent_thread_id, frozen_context FROM threads WHERE id = ?1",
        )
        .bind(id.as_str())
        .fetch_optional(&self.database.pool)
        .await?;
        let Some((cwd, parent, frozen)) = row else {
            return Ok(false);
        };
        if parent.is_some() {
            return Ok(false);
        }
        if self.database.load_session_binding_impl(id).await?.is_some() {
            return Ok(false);
        }
        if super::session_data::validate_unbound_legacy_frozen(frozen.as_deref()).is_err() {
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
        let roots: Vec<(String,)> =
            sqlx::query_as("SELECT root FROM legacy_execution_registrations")
                .fetch_all(&self.database.pool)
                .await?;
        Ok(roots.iter().any(|(root,)| {
            Path::new(root)
                .canonicalize()
                .map(|root| cwd.starts_with(root))
                .unwrap_or(false)
        }))
    }

    // ── runtime owner ─────────────────────────────────────────────────────────

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

    // ── 删除后的收尾 ─────────────────────────────────────────────────────────

    /// 会话数据已被删除：结束这条 identity 的本机执行事实与所有权。
    ///
    /// 排空并关闭当前实例的确切运行句柄；没有登记句柄不是错误。
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

    /// 事务保存完整 canonical 数据，提交后登记当前实例的运行句柄。
    pub(in crate::sessions) async fn create_with_lease(
        &self,
        input: &NewSession,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>> {
        if self.database.is_read_only() {
            return Err(super::failure::read_only_store());
        }
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
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(input.thread_id.clone())))?;
        self.register_lease(&input.thread_id)
    }

    /// 事务保存 bound 且 frozen 为空的 canonical 草稿，提交后登记运行句柄。
    pub(in crate::sessions) async fn begin_initialization(
        &self,
        draft: &peri_acp_types::session_resources::NewSessionDraft,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>> {
        if self.database.is_read_only() {
            return Err(super::failure::read_only_store());
        }
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
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(draft.thread_id.clone())))?;
        self.register_lease(&draft.thread_id)
    }

    /// 当前确切、活跃且非未决的 owner 持有排他门禁，事务内仅定稿有绑定的草稿。
    pub(in crate::sessions) async fn commit_frozen(
        &self,
        id: &ThreadId,
        lease: &Arc<dyn SessionExecutionLease>,
        frozen: &peri_acp_types::session_resources::FrozenSnapshotBytes,
    ) -> SessionResourceResult<()> {
        if self.database.is_read_only() {
            return Err(super::failure::read_only_store());
        }
        let owned = self.initialization_owner(id, lease)?;
        let guard = self
            .database
            .exclusive_guard_for(owned)
            .await
            .map_err(execution_failure)?
            .ok_or_else(lease_required)?;
        let outcome = async {
            self.initialization_owner(id, lease)?;
            let mut tx = self
                .database
                .pool
                .begin_with("BEGIN IMMEDIATE")
                .await
                .map_err(|error| map_sqlx(&error))?;
            let token = lease.owner_token().ok_or_else(lease_required)?;
            if token.root_id != *id {
                return Err(lease_required());
            }
            let owner: Option<(i64,)> = sqlx::query_as(
                "SELECT 1 FROM session_execution_owners WHERE root_id = ?1 AND epoch = ?2
                 AND nonce = ?3 AND released = 0
                 AND expires_at_unix > CAST(strftime('%s','now') AS INTEGER)",
            )
            .bind(id.as_str())
            .bind(token.epoch)
            .bind(&token.nonce)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
            if owner.is_none() {
                return Err(super::failure::conflict("session execution owner is stale"));
            }
            let updated = sqlx::query(
                "UPDATE threads SET frozen_context = ?1 WHERE id = ?2 AND frozen_context IS NULL
                 AND EXISTS (SELECT 1 FROM session_bindings WHERE thread_id = ?2)",
            )
            .bind(frozen.as_str())
            .bind(id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?
            .rows_affected();
            if updated != 1 {
                return Err(super::failure::conflict(
                    "session is not a bound draft with an uncommitted frozen snapshot",
                ));
            }
            tx.commit()
                .await
                .map_err(|_| commit_failure(Some(id.clone())))?;
            Ok(())
        }
        .await;
        if !outcome
            .as_ref()
            .is_err_and(|error| error.effect() == MutationOutcome::Unknown)
        {
            guard.finish();
        }
        outcome
    }

    /// 提交前后的本机资格复核：确切当前 Arc 必须活跃且没有未知写入效果。
    pub(in crate::sessions) async fn verify_initialization_owner(
        &self,
        id: &ThreadId,
        lease: &Arc<dyn SessionExecutionLease>,
    ) -> SessionResourceResult<()> {
        self.initialization_owner(id, lease).map(|_| ())
    }

    fn initialization_owner(
        &self,
        id: &ThreadId,
        lease: &Arc<dyn SessionExecutionLease>,
    ) -> SessionResourceResult<Arc<ExecutionLease>> {
        if self.database.is_read_only() {
            return Err(super::failure::read_only_store());
        }
        if self.database.pool.is_closed() {
            return Err(lease_required());
        }
        let owned = self
            .database
            .registered_lease(id)
            .map_err(execution_failure)?
            .filter(|owned| owned.is_active())
            .ok_or_else(lease_required)?;
        if !crate::sessions::execution::same_lease(&owned, lease) {
            return Err(lease_required());
        }
        if owned.is_uncertain() {
            return Err(super::failure::conflict(
                "session persistence outcome is uncertain",
            ));
        }
        Ok(owned)
    }

    /// 半写草稿的清理：判据成立、无本进程活跃句柄后，删除草稿的全部行。
    ///
    /// 判据与无绑定、无 frozen 的 legacy 会话互斥：
    ///
    /// - 有绑定行（`session_bindings` 命中）**且** `threads.frozen_context IS NULL`；
    /// - 已提交 frozen 的会话**不是**半写草稿：返回 typed 冲突；
    /// - 本进程有活跃初始化句柄 ⇒ 拒绝（那不是崩溃残留）。
    ///
    /// 删除在一条 `BEGIN IMMEDIATE` 里完成（子表 + threads 行），与
    /// [`super::session_data`] 的撤销同一顺序、同一判据。
    pub(in crate::sessions) async fn discard_incomplete_initialization(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<()> {
        if self.database.is_read_only() {
            return Err(super::failure::read_only_store());
        }
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        let draft: Option<(Option<String>, bool)> = sqlx::query_as(
            "SELECT frozen_context, EXISTS (SELECT 1 FROM session_bindings WHERE thread_id = ?1)
             FROM threads WHERE id = ?1",
        )
        .bind(id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| map_sqlx(&error))?;
        let Some((frozen, bound)) = draft else {
            // 行已不在：目标已达成（清理幂等），不把「已经不存在」当成失败。
            return Ok(());
        };
        if frozen.is_some() {
            return Err(super::failure::conflict(
                "session already has a committed frozen snapshot",
            ));
        }
        if !bound {
            return Err(super::failure::conflict(
                "session has no binding and is not an incomplete draft",
            ));
        }
        if let Some(lease) = self
            .database
            .registered_lease(id)
            .map_err(execution_failure)?
        {
            if lease.is_active() || lease.is_uncertain() {
                return Err(super::failure::conflict(
                    "session is still owned by a live initialization",
                ));
            }
        }
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

    /// 为已保存完整数据的会话登记运行句柄（收敛，不是重建）。
    ///
    /// 只用于 [`Self::create_with_lease`] 之外留下的 `data_saved` 状态：数据面已确认
    /// 保存完整（远程保存、或进程在准入前结束），此时不能重造 binding/frozen，也不能
    /// 报「确定未创建」。
    ///
    /// 数据保存及机器环境可用性由门面确认，本机仅记录本次运行与生命周期句柄。
    /// 不按历史绑定的目录对象认领会话。
    pub(in crate::sessions) async fn admit_existing(
        &self,
        id: &ThreadId,
        _binding: &SessionBinding,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>> {
        if self.database.is_read_only() {
            return Err(super::failure::read_only_store());
        }
        self.register_lease(id)
    }

    /// 按单一登记规则复用活跃句柄、拒绝未决句柄，或替换已关闭的确切 Arc。
    fn register_lease(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>> {
        let lease = self
            .database
            .register_execution_lease(id)
            .map_err(|_| lease_registration_failure(id))?;
        Ok(lease)
    }
}

/// 登记失败时 canonical 数据已提交，必须按已保存但未准入上报。
fn lease_registration_failure(id: &ThreadId) -> SessionResourceError {
    SessionResourceError::saved_but_not_admitted(id.clone())
}

/// 本机执行面对门面的行为：全部委托到本文件的方法。
///
/// 这层转发不改变任何语义（同一份 SQL、同一套运行门禁），它的存在只是让门面按端口调用，
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
