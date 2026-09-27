//! Stable sidecar OS ownership and durable dirty generations.
//!
//! v10 之后只有**一个执行域**：执行代际与 sidecar 锁都按 `thread_id` 原文归属——同一个
//! 本机库服务多个 store 时，那个维度已经不存在（用户裁决不做交叉能力）。调用方给 thread
//! id，本层按原文写行、按摘要取锁。 Drop never implies clean.

use super::database::SqliteSessionDatabase;
use crate::sessions::local_port::SessionFacts;
use anyhow::{Context, Result};
use async_trait::async_trait;
use peri_acp_types::{
    session_resources::SessionResourceResult,
    thread::ThreadId,
    workspace::{RecoveryRequiredDetails, SessionExecutionLease, WorkspaceError},
};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use std::{
    fs::File,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

/// 本机域的 sidecar 锁名：`<sha256(thread id 原文)>.lock`。
///
/// v10 之后只有这一个执行域，锁目录仍是既有的 `<db>.execution-locks/`：既有行、既有锁名
/// 逐字节相同，升级不改本机身份。
pub(in crate::sessions) fn local_lock_name(id: &ThreadId) -> String {
    format!("{:x}.lock", Sha256::digest(id.as_str().as_bytes()))
}

pub(in crate::sessions) struct ExecutionLease {
    thread_id: ThreadId,
    generation: i64,
    pool: SqlitePool,
    file: tokio::sync::Mutex<Option<File>>,
    active: AtomicBool,
    mutation_gate: Arc<tokio::sync::RwLock<()>>,
    mutation_uncertain: AtomicBool,
}

impl ExecutionLease {
    /// 新建租约；`file` 是已取得的 sidecar OS 锁（`None` 表示只读/无锁场景）。
    pub(super) fn new(
        thread_id: ThreadId,
        generation: i64,
        pool: SqlitePool,
        file: Option<File>,
    ) -> Self {
        Self {
            thread_id,
            generation,
            pool,
            file: tokio::sync::Mutex::new(file),
            active: AtomicBool::new(true),
            mutation_gate: Arc::new(tokio::sync::RwLock::new(())),
            mutation_uncertain: AtomicBool::new(false),
        }
    }

    /// 本次所有权是否仍接受新写入（关闭不可逆）。
    pub(in crate::sessions) fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    /// 是否存在无法证明终态的写入。
    pub(in crate::sessions) fn is_uncertain(&self) -> bool {
        self.mutation_uncertain.load(Ordering::Acquire)
    }

    /// 有界排空屏障：取写侧门禁再释放，证明此刻没有已准入写入在途。
    ///
    /// 调用方负责超时：等待外部 future 时不持普通全局互斥量。
    pub(in crate::sessions) async fn wait_for_in_flight(&self) {
        let _gate = self.mutation_gate.clone().write_owned().await;
    }

    /// 结束本次所有权：会话**数据已被删除**时的收尾，不做 clean CAS。
    ///
    /// 与 [`SessionExecutionLease::mark_clean`] 的唯一区别是「不发声明」：代际行已随数据
    /// 删除，clean 这句话没有对象，硬写下 `clean = 1` 只会制造一条描述不存在会话的行。
    /// 顺序相同——先关准入（等待已准入写入结束，不可逆），再释放 OS 锁。
    ///
    /// 释放后本租约仍是幂等终态：`mark_clean` 见到锁已不在会直接成功返回，因此调用方
    /// 无需知道数据删除与所有权结束的先后。
    ///
    /// 只有调用方能给出「数据确实已删除」这个事实（门面在删除返回成功后调用），本方法
    /// 自己不做任何推测：它不查数据行，也不把「行缺失」当成删除的证据。
    pub(in crate::sessions) async fn dispose_ownership(&self) {
        let _writes = self.mutation_gate.clone().write_owned().await;
        self.active.store(false, Ordering::Release);
        self.file.lock().await.take();
    }
}

/// The guard retains both the lease and admission lock until the complete SQL operation finishes.
/// A cancelled mutation leaves its run dirty even if SQLx still has a queued database command.
pub(in crate::sessions) struct ExecutionWriteGuard {
    lease: Arc<ExecutionLease>,
    _gate: tokio::sync::OwnedRwLockReadGuard<()>,
    completed: bool,
}

impl ExecutionWriteGuard {
    pub(in crate::sessions) fn finish(mut self) {
        self.completed = true;
    }
}

impl Drop for ExecutionWriteGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.lease.mutation_uncertain.store(true, Ordering::Release);
        }
    }
}

/// 排他写入范围：持有同一 owner 的写侧门禁。
///
/// 与 [`ExecutionWriteGuard`] 的区别是并发语义：读侧门禁允许同 root 的多个 mutation
/// 并发（SQLite 自己保证事务串行），写侧门禁用来把「检查 + 写入」做成一段不可插入的
/// 区间（child resume 认领的状态检查与写入、未发布创建的撤销）。
pub(in crate::sessions) struct ExclusiveExecutionGuard {
    lease: Arc<ExecutionLease>,
    _gate: tokio::sync::OwnedRwLockWriteGuard<()>,
    completed: bool,
}

impl ExclusiveExecutionGuard {
    pub(in crate::sessions) fn finish(mut self) {
        self.completed = true;
    }
}

impl Drop for ExclusiveExecutionGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.lease.mutation_uncertain.store(true, Ordering::Release);
        }
    }
}

/// 事务性写入的效果边界：进入 `commit` 前可证明「未生效」，提交成功后是「已生效」，
/// 只有提交自身的失败落在证明之外。
///
/// `sqlx` 的 SQLite 事务在 `commit()` 失败后由连接回滚，但「回滚是否真的完成」不由
/// 调用方观察得到；这里把不可证明的那一小段标出来，让写入准入保持未决，而不是用
/// `Err` 冒充「没生效」。
#[derive(Default)]
pub(in crate::sessions) struct TransactionEffect {
    committing: bool,
    committed: bool,
}

impl TransactionEffect {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// 即将调用 `commit()`：此后失败不再能证明未生效。
    pub(super) fn enter_commit(&mut self) {
        self.committing = true;
    }

    /// `commit()` 已成功返回。
    pub(super) fn commit_succeeded(&mut self) {
        self.committing = false;
        self.committed = true;
    }

    /// 按效果结清准入：可证明「未生效」或「已生效」时才 `finish`；其余情况释放 guard
    /// 由 `Drop` 留下未决证据。
    pub(super) fn settle(&self, guard: Option<ExecutionWriteGuard>) {
        if !self.committing {
            if let Some(guard) = guard {
                guard.finish();
            }
        }
    }
}

#[async_trait]
impl SessionExecutionLease for ExecutionLease {
    fn thread_id(&self) -> &ThreadId {
        &self.thread_id
    }

    async fn mark_clean(&self) -> Result<()> {
        let _writes = self.mutation_gate.write().await;
        if self.mutation_uncertain.load(Ordering::Acquire) {
            return Err(WorkspaceError::RecoveryRequired(RecoveryRequiredDetails {
                thread_id: self.thread_id.clone(),
                generation: self.generation,
            })
            .into());
        }
        let mut file = self.file.lock().await;
        if file.is_none() {
            return Ok(());
        }
        // Closing admission is irreversible, even if the SQL await is cancelled.
        // Otherwise a late old-owner mutation could run after a committed clean row.
        self.active.store(false, Ordering::Release);
        let updated = self.mark_clean_row().await?;
        if updated != 1 {
            // A cancelled mark_clean can have committed its SQL already. Only this
            // exact generation's clean record makes a retry safe to release the lock.
            //
            // 记录缺失在这里只有一个诚实结论：不知道它为什么不见了（被外部改写、
            // 库损坏、行没写成功），因此如实上报 `RecoveryRequired`。**刻意删除不走这条
            // 路**——会话删除由 [`Self::dispose_ownership`] 显式结束所有权（数据与代际
            // 行同时消失，之后本方法见到锁已释放即幂等成功），所以这里不需要、也不允许
            // 用「行不在」猜出「被删了」。
            if self.load_state_row().await? != Some((self.generation, true)) {
                return Err(WorkspaceError::RecoveryRequired(RecoveryRequiredDetails {
                    thread_id: self.thread_id.clone(),
                    generation: self.generation,
                })
                .into());
            }
        }
        self.active.store(false, Ordering::Release);
        file.take();
        Ok(())
    }
}

impl ExecutionLease {
    /// 宣告这一代 clean，返回受影响行数（1 = 本次生效）。
    async fn mark_clean_row(&self) -> Result<u64> {
        Ok(sqlx::query(
            "UPDATE execution_runs SET clean = 1 WHERE thread_id = ? AND generation = ? AND clean = 0",
        )
        .bind(self.thread_id.as_str())
        .bind(self.generation)
        .execute(&self.pool)
        .await?
        .rows_affected())
    }

    /// 这条 identity 的代际行（复核用；缺行返回 `None`）。
    async fn load_state_row(&self) -> Result<Option<(i64, bool)>> {
        Ok(
            sqlx::query_as("SELECT generation, clean FROM execution_runs WHERE thread_id = ?")
                .bind(self.thread_id.as_str())
                .fetch_optional(&self.pool)
                .await?,
        )
    }
}

impl ExecutionLease {
    /// 放弃本次所有权：等待已准入写入 → 执行补偿 → 关闭准入并释放 OS 锁。
    ///
    /// 只用于「本次创建被撤销」：数据行会被删除，因此不能走 `mark_clean` 的 clean CAS
    /// （那要求记录仍然存在）。
    ///
    /// 补偿失败时**不动**所有权状态：既不关闭准入也不放锁，调用方仍持有这条会话并可
    /// 重试撤销或继续使用。反过来若先关闭准入再补偿，一次失败的补偿会把会话变成
    /// 「既没撤销、又不能再用」，那才是真正的半状态。
    pub(in crate::sessions) async fn abandon_ownership<F, Fut, T>(
        &self,
        compensate: F,
    ) -> SessionResourceResult<T>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = SessionResourceResult<T>>,
    {
        // 补偿期间取写侧门禁：已准入的写入先结束，新写入等在这里。
        let gate = self.mutation_gate.clone().write_owned().await;
        let outcome = compensate().await;
        if outcome.is_ok() {
            // 补偿成功：这条 identity 已撤销，关闭准入不可逆，然后释放 OS 锁。
            self.active.store(false, Ordering::Release);
            drop(gate);
            let mut file = self.file.lock().await;
            file.take();
        }
        outcome
    }
}

/// 执行锁的重试预算与间隔。
///
/// `flock` 的锁挂在 open file description 上，而 `fork` 出的子进程共享父进程的描述符
/// （`CLOEXEC` 只在子进程 `exec` 时才关闭）。会话生命周期里必然有子进程（Git 发现、
/// `sw_vers`、LSP 等），在子进程 `fork` 到 `exec` 的窗口内，本进程自己重开同一 inode
/// 会被内核拒绝——锁仍被继承者持有。实测窗口在毫秒级，但子进程何时被调度取决于机器
/// 负载。没有重试时这些瞬时持有会被误报成 `ExecutionBusy`（「会话已被其他执行宿主占用」），
/// 把一次正常的取得所有权变成偶发失败；真正的外部持有者会持续持有，预算耗尽后仍按原语义上报。
const EXECUTION_LOCK_RETRY_BUDGET: Duration = Duration::from_millis(500);
const EXECUTION_LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(10);

impl SqliteSessionDatabase {
    /// `lock_name` 是 [`local_lock_name`] 给出的锁相对名，落在 `<db>.execution-locks/`。
    pub(super) async fn lock_execution(&self, lock_name: &str) -> Result<File> {
        let name = lock_name.to_owned();
        let mut directory = self.db_path.as_os_str().to_os_string();
        directory.push(".execution-locks");
        let path = std::path::PathBuf::from(directory).join(name);
        tokio::task::spawn_blocking(move || -> Result<File> {
            std::fs::create_dir_all(path.parent().context("lock directory missing")?)?;
            // 所有进程复用同一 inode，绝不删除锁文件。
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(path)?;
            let deadline = std::time::Instant::now() + EXECUTION_LOCK_RETRY_BUDGET;
            loop {
                match file.try_lock() {
                    Ok(()) => break,
                    Err(std::fs::TryLockError::WouldBlock) => {
                        if std::time::Instant::now() >= deadline {
                            return Err(WorkspaceError::ExecutionBusy.into());
                        }
                        std::thread::sleep(EXECUTION_LOCK_RETRY_INTERVAL);
                    }
                    Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
                }
            }
            Ok(file)
        })
        .await?
    }

    pub(super) async fn reset_dirty_execution_impl(
        &self,
        target: &RecoveryRequiredDetails,
    ) -> Result<()> {
        if self.read_only {
            return Err(WorkspaceError::ExecutionLeaseRequired.into());
        }
        let _file = self
            .lock_execution(&local_lock_name(&target.thread_id))
            .await?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let updated = sqlx::query(
            "UPDATE execution_runs SET clean = 1 WHERE thread_id = ? AND generation = ? AND clean = 0",
        )
        .bind(target.thread_id.as_str())
        .bind(target.generation)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if updated != 1 {
            return Err(WorkspaceError::RecoveryGenerationMismatch.into());
        }
        tx.commit().await?;
        Ok(())
    }

    pub(super) async fn acquire_execution_lease_impl(
        &self,
        id: &ThreadId,
        facts: &SessionFacts,
    ) -> Result<Arc<dyn SessionExecutionLease>> {
        if self.read_only {
            return Err(WorkspaceError::ExecutionLeaseRequired.into());
        }
        // 取得执行所有权是准入的最后一步：绑定已在同一次准入里复核过（解析或
        // `validate_session_binding_value`），这里只复核调用方给出的已记录字节，不重复完整发现。
        let binding = facts
            .binding
            .as_ref()
            .ok_or(WorkspaceError::BindingMissing)?;
        // Owned children have one owner: the root lease and its close transaction.
        // 树根由数据面回答：本机没有这条会话的行时（远端组合）也不该去本机表里找父链。
        if facts.root != *id {
            return Err(WorkspaceError::ExecutionLeaseRequired.into());
        }
        let mut connection = self.pool.acquire().await?;
        Self::validate_binding_relation_on(&mut connection, binding).await?;
        drop(connection);
        let file = self.lock_execution(&local_lock_name(id)).await?;
        let key = id.clone();
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let prior: Option<(i64, bool)> =
            sqlx::query_as("SELECT generation, clean FROM execution_runs WHERE thread_id = ?")
                .bind(id.as_str())
                .fetch_optional(&mut *tx)
                .await?;
        if let Some((generation, false)) = prior {
            return Err(WorkspaceError::RecoveryRequired(RecoveryRequiredDetails {
                thread_id: id.clone(),
                generation,
            })
            .into());
        }
        Self::validate_binding_relation_on(&mut tx, binding).await?;
        let generation = prior
            .map_or(Some(1), |(generation, _)| generation.checked_add(1))
            .context("execution generation exhausted")?;
        sqlx::query(
            "INSERT INTO execution_runs (thread_id, generation, clean) VALUES (?, ?, 0)
            ON CONFLICT(thread_id) DO UPDATE SET generation = excluded.generation, clean = 0",
        )
        .bind(id.as_str())
        .bind(generation)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        let lease = Arc::new(ExecutionLease::new(
            id.clone(),
            generation,
            self.pool.clone(),
            Some(file),
        ));
        self.execution_leases
            .lock()
            .map_err(|_| WorkspaceError::ExecutionLeaseRequired)?
            .insert(key, Arc::downgrade(&lease));
        Ok(lease)
    }

    /// 本进程登记的这条 identity 的活 owner（精确 id，不沿父链上溯）。
    ///
    /// 上溯需要父链，而父链是数据面事实（远端组合里本机没有这条会话的行），因此本函数只回答
    /// 「本进程是否持有**这一条** identity 的租约」。子树归属由调用方按 [`SessionFacts::root`]
    /// 显式问 root（见 [`Self::owner_lease`]）。
    pub(super) fn registered_lease(&self, id: &ThreadId) -> Result<Option<Arc<ExecutionLease>>> {
        Ok(self
            .execution_leases
            .lock()
            .map_err(|_| WorkspaceError::ExecutionLeaseRequired)?
            .get(id)
            .and_then(std::sync::Weak::upgrade))
    }

    /// 找到持有这棵树的活 owner。
    ///
    /// `Ok(None)` 只表示这棵树既没有绑定也没有活 owner——那是 legacy/测试路径；有绑定
    /// 而没有活 owner 是 `ExecutionLeaseRequired`，不能当成「无 owner」放行。
    ///
    /// 活 owner 只可能挂在 root 上：取得所有权要求这条会话没有父会话（子会话由 root 的租约
    /// 与它的关闭事务统一持有，自己不写 `execution_runs`），所以只查这条会话与 [`SessionFacts::root`]
    /// 两处即可。树形事实由调用方从数据面给出，本机不再走自己的 `threads` 父链。
    ///
    /// 本函数不取门禁：调用方必须自己决定要读侧还是写侧门禁（见
    /// [`Self::require_execution_lease`] 与 [`Self::exclusive_execution_guard`]），
    /// 避免在同一个 async 任务里嵌套两次加锁。
    pub(super) async fn owner_lease(
        &self,
        id: &ThreadId,
        facts: &SessionFacts,
    ) -> Result<Option<Arc<ExecutionLease>>> {
        if let Some(lease) = self.registered_lease(id)? {
            return Ok(Some(lease));
        }
        if facts.root != *id {
            if let Some(lease) = self.registered_lease(&facts.root)? {
                return Ok(Some(lease));
            }
        }
        if facts.bound {
            return Err(WorkspaceError::ExecutionLeaseRequired.into());
        }
        Ok(None)
    }

    /// 诊断读取：只回答「本进程是否持有这棵树的 owner」，不把「有绑定但无 owner」
    /// 当成错误——那正是可执行（尚未取得所有权）的常态。
    pub(super) async fn live_owner_lease(
        &self,
        id: &ThreadId,
        facts: &SessionFacts,
    ) -> Result<Option<Arc<ExecutionLease>>> {
        if let Some(lease) = self.registered_lease(id)? {
            return Ok(Some(lease));
        }
        if facts.root != *id {
            return self.registered_lease(&facts.root);
        }
        Ok(None)
    }

    /// 本机执行代际事实（generation, clean）；不创建锁文件、不改变状态。
    pub(super) async fn load_execution_state(&self, id: &ThreadId) -> Result<Option<(i64, bool)>> {
        Ok(
            sqlx::query_as("SELECT generation, clean FROM execution_runs WHERE thread_id = ?")
                .bind(id.as_str())
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    /// 删除这条 identity 的执行代际行（会话数据已删除时的收尾；删不到是正常情况）。
    ///
    /// 本机组合在删数据的同一事务里已经删过（`session_data::delete_tree`），这里是幂等的
    /// 空操作；数据在**远端**的组合靠这一步收敛——远端行消失后本机还留着一条代际行，
    /// 那是一条没有对象的行：它会让同名 identity 的重新创建看起来「已有代际」。
    pub(super) async fn delete_execution_state(&self, id: &ThreadId) -> Result<()> {
        sqlx::query("DELETE FROM execution_runs WHERE thread_id = ?")
            .bind(id.as_str())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub(super) async fn require_execution_lease(
        &self,
        id: &ThreadId,
        facts: &SessionFacts,
    ) -> Result<Option<ExecutionWriteGuard>> {
        if self.read_only {
            return Err(WorkspaceError::ExecutionLeaseRequired.into());
        }
        let Some(lease) = self.owner_lease(id, facts).await? else {
            return Ok(None);
        };
        self.write_guard_for(lease).await
    }

    /// 读侧写入准入（按已解析的 root 租约）：[`Self::owner_lease`] 之后的取门禁一步，
    /// 门禁挂在 root 的租约上（子会话与 root 共享同一条门禁）。
    pub(super) async fn write_guard_for(
        &self,
        lease: Arc<ExecutionLease>,
    ) -> Result<Option<ExecutionWriteGuard>> {
        let gate = lease.mutation_gate.clone().read_owned().await;
        // Close may have won while admission waited behind its write lock.
        if !lease.active.load(Ordering::Acquire) {
            return Err(WorkspaceError::ExecutionLeaseRequired.into());
        }
        if lease.mutation_uncertain.load(Ordering::Acquire) {
            return Err(WorkspaceError::RecoveryRequired(RecoveryRequiredDetails {
                thread_id: lease.thread_id.clone(),
                generation: lease.generation,
            })
            .into());
        }
        Ok(Some(ExecutionWriteGuard {
            lease,
            _gate: gate,
            completed: false,
        }))
    }

    /// 排他范围：与 [`Self::require_execution_lease`] 相同的准入判定（同一套数据面事实），
    /// 但取写侧门禁。
    pub(super) async fn exclusive_execution_guard(
        &self,
        id: &ThreadId,
        facts: &SessionFacts,
    ) -> Result<Option<ExclusiveExecutionGuard>> {
        if self.read_only {
            return Err(WorkspaceError::ExecutionLeaseRequired.into());
        }
        let Some(lease) = self.owner_lease(id, facts).await? else {
            return Ok(None);
        };
        self.exclusive_guard_for(lease).await
    }

    /// 写侧写入准入（按已解析的 root 租约）：同 [`Self::write_guard_for`]，取写锁。
    pub(super) async fn exclusive_guard_for(
        &self,
        lease: Arc<ExecutionLease>,
    ) -> Result<Option<ExclusiveExecutionGuard>> {
        let gate = lease.mutation_gate.clone().write_owned().await;
        if !lease.active.load(Ordering::Acquire) {
            return Err(WorkspaceError::ExecutionLeaseRequired.into());
        }
        if lease.mutation_uncertain.load(Ordering::Acquire) {
            return Err(WorkspaceError::RecoveryRequired(RecoveryRequiredDetails {
                thread_id: lease.thread_id.clone(),
                generation: lease.generation,
            })
            .into());
        }
        Ok(Some(ExclusiveExecutionGuard {
            lease,
            _gate: gate,
            completed: false,
        }))
    }
}
