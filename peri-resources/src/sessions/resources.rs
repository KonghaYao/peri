//! 会话资源门面实现：把本机执行面与数据面组合成消费侧唯一入口。
//!
//! 职责分工（B §2）：门面持有**两类事实**——数据面（`SessionDataPort` 的 SQLite 实现）
//! 与本机执行面（[`LocalExecution`]：发现、登记、owner、dirty、准入）；业务侧只看到本
//! 门面。数据端口是 `crate::sessions` 内的可见类型，其他 crate 与资源层其他模块都拿
//! 不到裸写句柄，本门面也不导出任何无 guard 的写入路径。
//!
//! 四条贯穿全部 mutation 的规则：
//!
//! 1. **统一准入**：能力/权限 → 未决持久化 → 本 root 有效 owner，检查在门面内部完成
//!    （[`MutationGate`]），不靠调用方先查。
//! 2. **效果结清**：只有 `Applied | NotApplied` 才释放写入准入；`Unknown`（取消、
//!    提交未确认）把范围交给 `Drop`，在租约上留下未决证据。
//! 3. **只读不退化**：已有会话上的写入在只读打开时返回 `ReadOnlyStore`（历史可读、
//!    执行权不可得，与既有只读降级路径一致）；需要登记新身份/新绑定的写入返回
//!    `Workspace(ReadOnlyStore)`（连会话都还没有，没有可降级的对象）。
//! 4. **诚实失败**：数据已完整保存但准入未成立时返回 `saved_but_not_admitted`，
//!    不谎称「确定未创建」，也不让调用方据此删数据。
//!
//! SQLite 的本地塌缩（新建时数据与执行代际同一事务）见 [`LocalExecution::create_with_lease`]；
//! 远程是「durable 数据 + 本机准入」两步，不是分布式事务。

mod claim;
mod gate;
mod lifecycle;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::{
    AccessMode, BindingRecheck, BindingState, ChildResumeClaim, ChildSnapshot, DataCapabilities,
    ExecutionAvailability, ForkSnapshot, FrozenSnapshotBytes, NewSession, PersistenceRecovery,
    RewindBoundary, SessionAvailability, SessionMetaPatch, SessionResourceError,
    SessionResourceErrorKind, SessionResourceResult, SessionResources, SessionSnapshot,
};
use peri_acp_types::store::{CompactionChange, MessageFlags, PersistedPayload};
use peri_acp_types::thread::{AgentStatus, ThreadId, ThreadMeta};
use peri_acp_types::workspace::{
    RecoveryRequiredDetails, ResolvedWorkspace, ScopedThreadPage, ScopedThreadQuery,
    SessionBinding, SessionExecutionLease, WorkspaceError,
};

use super::data::{ensure_child_relation, ChildResumeRecord, SessionDataPort};
use super::local_port::LocalExecutionPort;
use super::sqlite_store::{
    execution_failure, invalid_input, lease_required, not_found, same_lease, LocalExecution,
    ReadOnlyThreadStoreError,
};

use claim::ChildResumeClaimHandle;
use gate::MutationGate;
use lifecycle::{Lifecycle, LifecycleState};

/// 排空与关闭的有界等待。
///
/// 本机写入是短事务（单条 SQL 或一个 `BEGIN IMMEDIATE`），超过这个预算说明有写入卡住；
/// 此时报告未结清，而不是无限期等待一个外部 future。
const SETTLE_WAIT: Duration = Duration::from_secs(10);

/// 会话数据的存放位置 — 由组合层在装配时确定，门面不做后端推断。
///
/// 它只决定 `create_session` 的提交次数，不改变任何公开行为：
///
/// - 数据与执行代际在**同一个本机库**时是一次提交（本地塌缩，数据与代际同生共死）；
/// - 数据在**远端**时是两步（先由数据端口保存 canonical 数据，再取本机执行准入）。
///   两步之间没有分布式事务，因此保存成功而准入失败时只能如实报告
///   `saved_but_not_admitted`（历史可读，执行资格不可得）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::sessions) enum SessionDataHome {
    LocalLibrary,
    RemoteStore,
}

/// 会话资源门面（生产实现）。
///
/// 构造即确定访问模式与后端：[`Self::open`] 写打开（必要时原地升级 schema），
/// [`Self::open_existing_read_only`] 只读打开（不建库、不建表、不建锁文件）。
pub struct SessionResourcesImpl {
    gate: MutationGate,
    /// 关闭生命周期与 [`MutationGate`] 共享：`Closing` 起停止新写入，只有真实检查全部
    /// 结清并关闭数据面之后才确认 `Closed`（见 [`Self::close`]）。
    lifecycle: Lifecycle,
    /// 串行化关闭确认：并发关闭必须依次看到真实结论，不能两个都「从头开始」而重复关闭
    /// 同一个连接，也不能把另一个调用的未确认状态当成成功。
    close_confirm: tokio::sync::Mutex<()>,
    /// 会话数据的存放位置：决定 `create_session` 是几次提交（见 [`SessionDataHome`]）。
    home: SessionDataHome,
}

impl SessionResourcesImpl {
    /// 打开或创建会话库，原地升级已知旧 schema 并保留历史数据。
    pub async fn open(db_path: impl Into<PathBuf>) -> anyhow::Result<Self> {
        Ok(Self::from_local(LocalExecution::open(db_path).await?))
    }

    /// 以只读方式打开已存在的会话库；不创建目录、库、schema 或锁文件。
    pub async fn open_existing_read_only(
        db_path: impl AsRef<Path>,
    ) -> Result<Self, ReadOnlyThreadStoreError> {
        Ok(Self::from_local(
            LocalExecution::open_existing_read_only(db_path).await?,
        ))
    }

    /// 默认数据库位置 `~/.peri/threads/threads.db`；不创建目录、数据库或连接。
    pub fn default_database_path() -> anyhow::Result<PathBuf> {
        LocalExecution::default_database_path()
    }

    /// 与迁移桥共享同一个库句柄（迁移期唯一装配点 `SqliteThreadStore::open_shared*` 使用）。
    ///
    /// 本机组合：数据面与执行面由同一个库句柄回答（同一条连接真相），两个端口因此只是
    /// 同一实现的两张面孔。
    pub(in crate::sessions) fn from_local(local: LocalExecution) -> Self {
        let data = Arc::new(local.data_port());
        Self::from_ports(data, Arc::new(local), SessionDataHome::LocalLibrary)
    }

    /// 组合层装配点：数据面 + 本机执行面，两者由调用方决定指向哪个后端。
    ///
    /// 门面不持有任何后端判断，只按 `home`（组合层给出的**事实**）决定 `create_session`
    /// 是一步还是两步；公开行为仍只有 [`SessionResources`] 这一套。
    pub(in crate::sessions) fn from_ports(
        data: Arc<dyn SessionDataPort>,
        local: Arc<dyn LocalExecutionPort>,
        home: SessionDataHome,
    ) -> Self {
        let lifecycle = Lifecycle::new();
        let gate = MutationGate::new(data, local, lifecycle.clone());
        Self {
            gate,
            lifecycle,
            close_confirm: tokio::sync::Mutex::new(()),
            home,
        }
    }

    /// 会话级执行资格：只读事实、未决持久化与代际事实分开表达。
    ///
    /// 本方法不取得所有权、不创建锁文件：跨进程的持有者只能由
    /// [`SessionResources::acquire_execution`] 的稳定锁判定。
    async fn execution_availability(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<ExecutionAvailability> {
        let local = self.gate.local();
        if local.is_read_only() {
            return Ok(ExecutionAvailability::ReadOnlyStore);
        }
        if !self.gate.data().session_exists(id).await? {
            return Err(not_found());
        }
        // 数据面事实一次取齐：活 owner 判定要树根，绑定复核要绑定字节。两者都不是本机事实，
        // 远端组合里本机没有这条会话的行。
        let facts = self.gate.session_facts(id).await?;
        // 活 owner 优先于代际事实：正在运行的会话必然是 `clean = 0`，那是「有主」而不是
        // 「需要恢复」。`OwnedElsewhere` 因此表达「执行权已在某处且活跃，本次不能再次取得」。
        if local
            .live_owner(id, &facts)
            .await
            .map_err(execution_failure)?
            .is_some_and(|lease| lease.is_active())
        {
            return Ok(ExecutionAvailability::OwnedElsewhere);
        }
        if let Some((generation, false)) =
            local.execution_state(id).await.map_err(execution_failure)?
        {
            return Ok(ExecutionAvailability::Dirty(RecoveryRequiredDetails {
                thread_id: id.clone(),
                generation,
            }));
        }
        match self.recheck_binding(facts.binding.as_ref(), false).await {
            Ok(_) => Ok(ExecutionAvailability::Available),
            Err(error)
                if matches!(
                    error.workspace_error(),
                    Some(WorkspaceError::BindingMissing)
                ) =>
            {
                Ok(ExecutionAvailability::BindingMissing)
            }
            Err(_) => Ok(ExecutionAvailability::WorkspaceUnavailable),
        }
    }

    /// 用 canonical 绑定字节做本机复核（`full` 为真时叠一次完整发现快照比对）。
    ///
    /// 绑定字节来自数据面：本机组合是本机 `session_bindings`，远程组合是远端会话行。
    /// 没有绑定行时按 `BindingMissing` 如实失败——执行资格需要一个可复核的绑定。
    async fn recheck_binding(
        &self,
        binding: Option<&SessionBinding>,
        full: bool,
    ) -> SessionResourceResult<ResolvedWorkspace> {
        let binding = binding.ok_or_else(|| {
            SessionResourceError::new(SessionResourceErrorKind::Workspace(
                WorkspaceError::BindingMissing,
            ))
        })?;
        self.gate
            .local()
            .validate_binding_value(binding, full)
            .await
            .map_err(execution_failure)
    }

    /// 按 id 取数据面绑定字节后复核（只回答「绑定向哪里」的调用方用这个）。
    async fn recheck_binding_of(
        &self,
        id: &ThreadId,
        full: bool,
    ) -> SessionResourceResult<ResolvedWorkspace> {
        let binding = self.gate.data().binding_of(id).await?;
        self.recheck_binding(binding.as_ref(), full).await
    }

    /// `Missing` 与 `LegacyConfirmed` 的差别是本机来源证据：无绑定、无父会话、无执行
    /// 代际，且保存的绝对 cwd 落在本机已登记工作区内，才表达为 legacy 历史；其余的无
    /// 绑定状态（外来会话、登记缺失）不冒充 legacy。
    ///
    /// 这份证据**只属于本机组合**：`legacy_confirmed` 读的是本机 `threads` / `session_bindings`
    /// / `workspaces`，远端组合里会话行与它的 cwd 都在远端，本机根本没有可读的来源证据。
    /// 因此远端组合由门面**固定为 false**（端口文档同此），而不是去本机表里碰运气：
    /// 本机恰有一条同 id 的行就会把远端会话判成 legacy。
    async fn classify_binding(
        &self,
        id: &ThreadId,
        state: BindingState,
    ) -> SessionResourceResult<BindingState> {
        if !matches!(state, BindingState::Missing) {
            return Ok(state);
        }
        if self.home == SessionDataHome::RemoteStore {
            return Ok(state);
        }
        if self
            .gate
            .local()
            .legacy_confirmed(id)
            .await
            .map_err(execution_failure)?
        {
            return Ok(BindingState::LegacyConfirmed);
        }
        Ok(state)
    }

    /// 「数据已完整保存、执行代际未写」的收敛。
    ///
    /// 前提（又是同一次创建、工作区证据仍然一致）成立时补上准入；否则保留 identity 并
    /// 如实报告「已保存、未准入」——既不重造 binding/frozen，也不谎称「确定未创建」。
    async fn admit_saved_creation(
        &self,
        id: &ThreadId,
        binding: &SessionBinding,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>> {
        let local = self.gate.local();
        if local
            .execution_state(id)
            .await
            .map_err(execution_failure)?
            .is_some()
        {
            return Err(invalid_input("session identity already exists"));
        }
        let saved = self.gate.data().binding_of(id).await?;
        let premise_holds = saved.as_ref() == Some(binding)
            && self.recheck_binding(saved.as_ref(), false).await.is_ok();
        if !premise_holds {
            return Err(SessionResourceError::saved_but_not_admitted(id.clone()));
        }
        match local.admit_existing(id, binding).await {
            Ok(lease) => Ok(lease),
            Err(error) if error.is_persistence_uncertain() => Err(error),
            Err(_) => Err(SessionResourceError::saved_but_not_admitted(id.clone())),
        }
    }

    fn workspace_mismatch() -> SessionResourceError {
        SessionResourceError::new(SessionResourceErrorKind::Workspace(
            WorkspaceError::ExecutionBindingMismatch,
        ))
    }

    /// 测试用：本机组合背后的 SQLite 连接池（逐条构造事实的夹具使用）。
    #[cfg(test)]
    pub(super) fn local_pool(&self) -> &sqlx::SqlitePool {
        self.gate
            .local()
            .sqlite_pool()
            .expect("local composition always has a SQLite pool")
    }
}

// ─── 门面实现 ─────────────────────────────────────────────────────────────────

#[async_trait]
impl SessionResources for SessionResourcesImpl {
    // ── 能力与准入 ──

    async fn inspect_availability(
        &self,
        session: Option<&ThreadId>,
    ) -> SessionResourceResult<SessionAvailability> {
        let read_only = self.gate.local().is_read_only();
        let access = if read_only {
            AccessMode::ReadOnly
        } else {
            AccessMode::ReadWrite
        };
        let capabilities = if read_only {
            DataCapabilities::HistoryReadOnly
        } else {
            DataCapabilities::Complete
        };
        let execution = match session {
            None => None,
            Some(id) => Some(self.execution_availability(id).await?),
        };
        Ok(SessionAvailability {
            access,
            capabilities,
            execution,
        })
    }

    async fn resolve_workspace(&self, cwd: &Path) -> SessionResourceResult<ResolvedWorkspace> {
        self.gate.ensure_registration_write()?;
        self.gate
            .local()
            .resolve_workspace(cwd)
            .await
            .map_err(execution_failure)
    }

    async fn validate_session(
        &self,
        id: &ThreadId,
        workspace: &ResolvedWorkspace,
    ) -> SessionResourceResult<()> {
        // 一次准入的权威复核：关系、关键文件对象加一次完整发现快照比对。
        let resolved = self.recheck_binding_of(id, true).await?;
        if &resolved != workspace {
            return Err(Self::workspace_mismatch());
        }
        Ok(())
    }

    async fn acquire_execution(
        &self,
        id: &ThreadId,
        workspace: &ResolvedWorkspace,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>> {
        self.gate.ensure_session_write()?;
        // 先复核再取锁：binding 不可变，先复核不与取得所有权竞争，且失败时不会留下
        // 一个新的 dirty 代际。
        let facts = self.gate.session_facts(id).await?;
        let resolved = self.recheck_binding(facts.binding.as_ref(), true).await?;
        if &resolved != workspace {
            return Err(Self::workspace_mismatch());
        }
        self.gate
            .local()
            .acquire_lease(id, &facts)
            .await
            .map_err(execution_failure)
    }

    async fn reset_dirty_execution(
        &self,
        request: &peri_acp_types::workspace::ResetDirtyRequest,
    ) -> SessionResourceResult<()> {
        self.gate.ensure_session_write()?;
        // 显式风险接受是这条行为的领域前提，不能只由协议层把关。
        if !request.accept_risk {
            return Err(invalid_input("explicit risk acceptance required"));
        }
        self.gate
            .local()
            .reset_dirty(&request.target)
            .await
            .map_err(execution_failure)
    }

    // ── 创建与接纳 ──

    async fn create_session(
        &self,
        input: &NewSession,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>> {
        self.gate.ensure_registration_write()?;
        let local = self.gate.local();
        if self.gate.data().session_exists(&input.thread_id).await? {
            return self
                .admit_saved_creation(&input.thread_id, &input.binding)
                .await;
        }
        // 提交次数由数据位置决定（见 [`SessionDataHome`]）：同一个本机库时数据与执行代际
        // 一次提交；数据在别处时先由数据端口保存，再取本机执行准入。门面不做后端推断。
        match self.home {
            SessionDataHome::LocalLibrary => local.create_session(input).await,
            SessionDataHome::RemoteStore => {
                self.gate.data().save_new_session(input).await?;
                match local.admit_existing(&input.thread_id, &input.binding).await {
                    Ok(lease) => Ok(lease),
                    Err(error) if error.is_persistence_uncertain() => Err(error),
                    Err(_) => Err(SessionResourceError::saved_but_not_admitted(
                        input.thread_id.clone(),
                    )),
                }
            }
        }
    }

    async fn abandon_initialization(
        &self,
        id: &ThreadId,
        lease: &Arc<dyn SessionExecutionLease>,
    ) -> SessionResourceResult<()> {
        self.gate.ensure_session_write()?;
        let data = self.gate.data().clone();
        self.gate
            .local()
            .abandon_initialization(
                id,
                lease,
                Box::pin(async move { data.revoke_unpublished_session(id).await }),
            )
            .await
    }

    async fn adopt_legacy_session(
        &self,
        id: &ThreadId,
        saved_cwd: &str,
        workspace: &ResolvedWorkspace,
        frozen: &FrozenSnapshotBytes,
    ) -> SessionResourceResult<()> {
        self.gate.ensure_session_write()?;
        // 接纳在数据面的一次写事务内完成：保存路径一致、登记关系一致、既有绑定只校验
        // 不覆盖、有执行行时拒绝（不借接纳绕过 dirty）。
        self.gate
            .data()
            .adopt_legacy_session(id, saved_cwd, workspace, frozen)
            .await
    }

    // ── 读取 ──

    async fn load_session_snapshot(&self, id: &ThreadId) -> SessionResourceResult<SessionSnapshot> {
        let mut snapshot = self.gate.data().load_snapshot(id).await?;
        snapshot.binding = self.classify_binding(id, snapshot.binding).await?;
        Ok(snapshot)
    }

    async fn load_session_binding(&self, id: &ThreadId) -> SessionResourceResult<BindingState> {
        let state = self.gate.data().load_binding(id).await?;
        self.classify_binding(id, state).await
    }

    async fn validate_bound_workspace(
        &self,
        id: &ThreadId,
        check: BindingRecheck,
    ) -> SessionResourceResult<ResolvedWorkspace> {
        // 复核不改绑、不取执行权：与 `execution_availability` 走同一对本机原语，
        // 因此「能不能执行」与「绑定向哪里」不会出现两套结论。
        self.recheck_binding_of(id, matches!(check, BindingRecheck::Full))
            .await
    }

    async fn load_session_history(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<Vec<PersistedPayload>> {
        self.gate.data().load_session_history(id).await
    }

    async fn load_session_meta(&self, id: &ThreadId) -> SessionResourceResult<ThreadMeta> {
        self.gate.data().load_meta(id).await
    }

    async fn list_sessions(
        &self,
        query: &ScopedThreadQuery,
    ) -> SessionResourceResult<ScopedThreadPage> {
        self.gate.data().list_sessions(query).await
    }

    async fn list_children(&self, parent: &ThreadId) -> SessionResourceResult<Vec<ThreadMeta>> {
        self.gate.data().list_children(parent).await
    }

    async fn list_session_tree(&self, root: &ThreadId) -> SessionResourceResult<Vec<ThreadMeta>> {
        self.gate.data().list_session_tree(root).await
    }

    // ── 写入 ──

    async fn append_history(
        &self,
        id: &ThreadId,
        payloads: &[PersistedPayload],
    ) -> SessionResourceResult<()> {
        self.gate
            .with_mutation(id, || self.gate.data().append_history(id, payloads))
            .await
    }

    async fn save_fork(
        &self,
        fork: &ForkSnapshot,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>> {
        self.gate.ensure_registration_write()?;
        let local = self.gate.local();
        let data = self.gate.data();
        if !data.session_exists(&fork.source_id).await? {
            return Err(not_found());
        }
        // 目标已存在：与 create 同一条判定——已有执行代际是 identity 冲突，数据已保存
        // 而未准入则收敛（同一次尝试的幂等重试不会重复写历史）。
        if data.session_exists(&fork.target.thread_id).await? {
            return self
                .admit_saved_creation(&fork.target.thread_id, &fork.target.binding)
                .await;
        }
        // 目标快照先完整落库（数据面一次事务），再建立执行准入；准入失败时数据已保存，
        // 如实报告「已保存、未准入」，不重造目标快照。
        //
        // 这里不取租约门禁：目标是**新 identity**，此刻既没有 root 也没有 owner 可挂；
        // 落库结果自描述（`threads` 行在、`execution_runs` 无），重试按「已保存、未准入」
        // 收敛，因此不需要在别处留下未决证据。与 child 的差别在于 child 的写入归属
        // root 执行域，没有自己的 identity 可解释残留。
        self.gate.data().save_fork(fork).await?;
        match local
            .admit_existing(&fork.target.thread_id, &fork.target.binding)
            .await
        {
            Ok(lease) => Ok(lease),
            Err(error) if error.is_persistence_uncertain() => Err(error),
            Err(_) => Err(SessionResourceError::saved_but_not_admitted(
                fork.target.thread_id.clone(),
            )),
        }
    }

    async fn save_child(
        &self,
        child: &ChildSnapshot,
        lease: &Arc<dyn SessionExecutionLease>,
    ) -> SessionResourceResult<()> {
        // 输入自洽先于一切副作用：快照里的父子关系出现两次（`parent_id` 与 target meta），
        // 落库用的是后者。不一致的快照必须先被拒绝——此时还什么都没写、也没有在 root 的
        // 租约上留下未决证据；只靠调用点自觉会让直接调用门面的一方写出「独立 root」。
        ensure_child_relation(child)?;
        self.gate.ensure_registration_write()?;
        let local = self.gate.local();
        // root 的事实由数据面回答（子会话在本机可能一行都没有）；root 自己也可能是别处的
        // 子会话，因此按「root 的 root + root 有没有绑定」问一次。
        let facts = self.gate.session_facts(&child.root_id).await?;
        let owned = local
            .owner_lease(&child.root_id, &facts)
            .await
            .map_err(execution_failure)?
            .ok_or_else(lease_required)?;
        // child 沿用 root owner：传入的必须是这条 root 的活 owner，不能借别人的所有权写。
        if !owned.is_active() || !same_lease(&owned, lease) {
            return Err(lease_required());
        }
        // child 沿用 root owner，写入门禁也必须挂在 root 上：新 child 自己既没有 identity 也没有
        // 执行代际，用 target id 解析只会得到「链上无 owner」而不设门禁，取消/超时就无法在
        // root 的租约上留下未决证据（B §4.1.3「guard 覆盖真正的 adapter 工作完成」）。
        self.gate
            .with_mutation(&child.root_id, || self.gate.data().save_child(child))
            .await
    }

    async fn claim_child_resume(
        &self,
        child: &ThreadId,
        root: &ThreadId,
    ) -> SessionResourceResult<Box<dyn ChildResumeClaim>> {
        self.gate.ensure_session_write()?;
        if self.gate.data().session_root(child).await? != *root {
            return Err(invalid_input(
                "child session does not belong to the claimed root",
            ));
        }
        // 认领在写侧门禁内完成「读状态 + 写 active」：并发认领里只有一个能看到非 active。
        let previous = self
            .gate
            .with_exclusive(root, || async {
                let previous = self.gate.data().load_child_resume_record(child).await?;
                if previous.status.is_active() {
                    return Err(invalid_input("child session is still active"));
                }
                self.gate
                    .data()
                    .store_child_resume_record(
                        child,
                        &ChildResumeRecord {
                            status: AgentStatus::Active,
                            claimed: true,
                        },
                    )
                    .await?;
                Ok(previous)
            })
            .await?;
        Ok(Box::new(ChildResumeClaimHandle::new(
            self.gate.clone(),
            child.clone(),
            previous,
        )))
    }

    async fn apply_compaction(
        &self,
        id: &ThreadId,
        change: &CompactionChange,
    ) -> SessionResourceResult<()> {
        self.gate
            .with_mutation(id, || self.gate.data().apply_compaction(id, change))
            .await
    }

    async fn apply_message_projections(
        &self,
        id: &ThreadId,
        updates: &[(MessageId, MessageFlags)],
    ) -> SessionResourceResult<()> {
        self.gate
            .with_mutation(id, || {
                self.gate.data().apply_message_projections(id, updates)
            })
            .await
    }

    async fn rewind_history(
        &self,
        id: &ThreadId,
        boundary: RewindBoundary,
    ) -> SessionResourceResult<()> {
        self.gate
            .with_mutation(id, || self.gate.data().rewind_history(id, boundary))
            .await
    }

    async fn remove_history_entries(
        &self,
        id: &ThreadId,
        ids: &[MessageId],
    ) -> SessionResourceResult<()> {
        self.gate
            .with_mutation(id, || self.gate.data().remove_history_entries(id, ids))
            .await
    }

    async fn update_session_meta(
        &self,
        id: &ThreadId,
        patch: &SessionMetaPatch,
    ) -> SessionResourceResult<()> {
        self.gate
            .with_mutation(id, || self.gate.data().update_meta(id, patch))
            .await
    }

    async fn delete_session_tree(&self, id: &ThreadId) -> SessionResourceResult<()> {
        // 删除是显式生命周期行为：门面确认有效 owner 后才动数据；删除即删除——v10 之后
        // 本机不再留删除锚点（用户裁决不做跨安装的终态判定），远端删除也不再需要先把
        // 「正在删除」写回本机。
        self.gate
            .with_mutation(id, || self.gate.data().delete_tree(id))
            .await?;
        // 数据消失之后，本机执行事实也随之结束：这条 identity 的代际行与本进程持有的
        // owner 在同一步收尾（见 `LocalExecutionPort::dispose_execution`）。放在删除**之后**
        // 是必须的——所有权要在数据被删的整个过程中保持；删除失败时提前返回，所有权原样
        // 保留，调用方仍可重试或显式放弃。
        self.gate.local().dispose_execution(id).await
    }

    /// 未决持久化的收敛：不需要调用方提供任何令牌或操作 id。
    ///
    /// 收谁的口供由数据面决定（本机写入同事务完成，因此本机组合只需确认锚点已清；远程组合
    /// 按本机日志里的**原操作 id** 逐条向远端账本求证终态）。门面在这里只做两件事：
    ///
    /// 1. **不与自己在途的请求抢判定权**：本进程还持有这条 root 的活 owner 时，先等已准入
    ///    的写入结束（有界），再让数据面判定。取消/超时之后的真实写仍会留在日志里，因此
    ///    等待只是把先后顺序摆正，不是判定依据；
    /// 2. 把结论原样转达（`Recovered` 才代表本机已无可证明未结态的写入），失败不降级。
    ///
    /// 只读打开时本方法只读不写：回答的是「能否重载」，不是「已经收敛」。
    ///
    /// 关闭流程（`Closing`）下仍可调用：第一次关闭因未结清失败之后，收敛必须还有入口。
    async fn recover_session_persistence(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<PersistenceRecovery> {
        self.gate.ensure_recovery_permitted()?;
        // 这是先后顺序而不是判定依据：等待失败（例如本机父链损坏导致诊断读取失败）不构成
        // 「不能判定」，收敛结论仍只由数据面给出，不在这里用兜底把未知当已知。
        let facts = self.gate.session_facts(id).await?;
        let owner = self
            .gate
            .local()
            .live_owner(id, &facts)
            .await
            .ok()
            .flatten();
        if let Some(lease) = owner {
            if lease.is_active() {
                tokio::time::timeout(SETTLE_WAIT, lease.wait_for_in_flight())
                    .await
                    .map_err(|_| SessionResourceError::new(SessionResourceErrorKind::Timeout))?;
            }
        }
        self.gate.data().recover_persistence(id).await
    }

    async fn drain_persistence(&self, id: &ThreadId) -> SessionResourceResult<()> {
        // 与恢复同样在 `Closing` 下放行：关闭前的排空必须能重做。
        self.gate.ensure_recovery_permitted()?;
        let local = self.gate.local();
        let facts = self.gate.session_facts(id).await?;
        // 排空等待的是**本进程**已准入的写入：他处持有的所有权不归本次等待，
        // 因此这里用诊断查询，不把「本进程没有 owner」当成错误。
        if let Some(lease) = local
            .live_owner(id, &facts)
            .await
            .map_err(execution_failure)?
        {
            if lease.is_active() {
                // 有界等待已准入写入结清；超时说明有写入卡住，报告未结清而不是无限等。
                tokio::time::timeout(SETTLE_WAIT, lease.wait_for_in_flight())
                    .await
                    .map_err(|_| SessionResourceError::new(SessionResourceErrorKind::Timeout))?;
            }
            if lease.is_uncertain() {
                return Err(SessionResourceError::persistence_uncertain(Some(
                    id.clone(),
                )));
            }
        }
        self.gate.data().drain(id).await
    }
}

// ─── 部署关闭 ─────────────────────────────────────────────────────────────────

impl SessionResourcesImpl {
    /// 关闭整个存储（部署生命周期行为，不属于业务行为面）。
    ///
    /// 完成条件有两条，缺一不算确认：**本机传输面的关闭走完**（数据面 `close` 成功返回），
    /// 以及**这个 store 的持久化未决已结清**——未结清判定按整 store 作用域提问（见下），
    /// 不缩到活跃租约或活跃 root：租约已经 drop、durable 锚点仍在的写入同样挡住关闭。
    /// 未结清时保持 `Closing`（恢复入口仍可用），重复关闭重新做一遍真实检查。
    ///
    /// 权限不由本方法决定而由**谁能拿到实例**决定：业务侧只持有
    /// `Arc<dyn SessionResources>`（[`SessionResources`] 已不含关闭），本方法只对
    /// 具体实例可见，取用点只有部署 owner [`SessionStoreShutdownOwner`]。
    ///
    /// [`SessionStoreShutdownOwner`]: crate::context::SessionStoreShutdownOwner
    pub(crate) async fn close(&self) -> SessionResourceResult<()> {
        // 关闭确认串行化：后来者要么看到确认关闭（幂等成功），要么重新做一遍真实检查，
        // 不会因为「上一次调用过」或「正好并发」而绕过未结清事实。
        let _confirm = self.close_confirm.lock().await;
        if self.lifecycle.state() == LifecycleState::Closed {
            return Ok(());
        }
        // 停止新写入不可逆；`Closing` 不是 `Closed`：未结清事实仍可收敛（恢复/排空在
        // `Closing` 下继续可用，见 `gate::ensure_recovery_permitted`）。
        self.lifecycle.begin_closing();

        // 每次调用都重新做真实检查，不复用上一次的失败结论。
        //
        // 等待与判定分开：等待建立先后顺序（在途写入在关闭前结束），判定只认 `is_uncertain`
        // 与未决锚点；超时说明写入卡住，报告未结清而不是无限期等待外部 future。
        for lease in self.gate.local().live_leases() {
            if !lease.is_active() {
                continue;
            }
            if tokio::time::timeout(SETTLE_WAIT, lease.wait_for_in_flight())
                .await
                .is_err()
            {
                return Err(SessionResourceError::new(SessionResourceErrorKind::Timeout));
            }
            if lease.is_uncertain() {
                return Err(SessionResourceError::persistence_uncertain(Some(
                    lease.thread_id().clone(),
                )));
            }
        }
        // 未结清事实只在活租约上（本机没有再留 durable 锚点）：租约 drop 之后那次写入的
        // 终态由 `execution_runs.clean = 0` 表达，不挡关闭——那是「需要恢复」而不是
        // 「本次关闭不能确认」。
        //
        // 恢复所需的证据已确认结清之后才关闭数据面：提前取走连接（远程 adapter 的唯一
        // 连接句柄）会让「未确认」的未决事实失去收敛路径，而重复关闭恰恰要能重做检查。
        self.gate.data().close().await?;
        // 只有到这里才是确认关闭（并发调用由串行化保证只有一个走到这里；即便迁移已由
        // 别处完成，结论也相同）。本机数据面的 `close` 只停止它自己的写入入口（连接池由
        // 共享库句柄所有）；clean 由各 owner 自己写（`SessionExecutionLease::mark_clean`），
        // 门面不代写、也不替它们宣告会话已结清。
        self.lifecycle.confirm_closed();
        Ok(())
    }
}

#[cfg(test)]
#[path = "resources_test.rs"]
mod tests;
