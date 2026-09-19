//! Stable sidecar OS ownership and durable dirty generations. Drop never implies clean.

use super::SqliteThreadStore;
use anyhow::{Context, Result};
use async_trait::async_trait;
use peri_acp_types::{
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
};

pub(super) struct ExecutionLease {
    thread_id: ThreadId,
    generation: i64,
    pool: SqlitePool,
    file: tokio::sync::Mutex<Option<File>>,
    active: AtomicBool,
    mutation_gate: Arc<tokio::sync::RwLock<()>>,
    mutation_uncertain: AtomicBool,
}

/// The guard retains both the lease and admission lock until the complete SQL operation finishes.
/// A cancelled mutation leaves its run dirty even if SQLx still has a queued database command.
pub(super) struct ExecutionWriteGuard {
    lease: Arc<ExecutionLease>,
    _gate: tokio::sync::OwnedRwLockReadGuard<()>,
    completed: bool,
}

impl ExecutionWriteGuard {
    pub(super) fn finish(mut self) {
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
        let updated = sqlx::query("UPDATE execution_runs SET clean = 1 WHERE thread_id = ? AND generation = ? AND clean = 0")
            .bind(&self.thread_id).bind(self.generation).execute(&self.pool).await?;
        if updated.rows_affected() != 1 {
            let run: Option<(i64, bool)> =
                sqlx::query_as("SELECT generation, clean FROM execution_runs WHERE thread_id = ?")
                    .bind(&self.thread_id)
                    .fetch_optional(&self.pool)
                    .await?;
            // A cancelled mark_clean can have committed its SQL already. Only this
            // exact generation's clean record makes a retry safe to release the lock.
            if run != Some((self.generation, true)) {
                let exists: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM threads WHERE id = ?")
                    .bind(&self.thread_id)
                    .fetch_one(&self.pool)
                    .await?;
                // New-session compensation can delete its row while the lease owns it.
                if exists.0 != 0 {
                    return Err(WorkspaceError::RecoveryRequired(RecoveryRequiredDetails {
                        thread_id: self.thread_id.clone(),
                        generation: self.generation,
                    })
                    .into());
                }
            }
        }
        self.active.store(false, Ordering::Release);
        file.take();
        Ok(())
    }
}

impl SqliteThreadStore {
    async fn lock_execution(&self, id: &ThreadId) -> Result<File> {
        let safe_id = format!("{:x}", Sha256::digest(id.as_bytes()));
        let mut directory = self.db_path.as_os_str().to_os_string();
        directory.push(".execution-locks");
        let path = std::path::PathBuf::from(directory).join(format!("{safe_id}.lock"));
        tokio::task::spawn_blocking(move || -> Result<File> {
            std::fs::create_dir_all(path.parent().context("lock directory missing")?)?;
            // 所有进程复用同一 inode，绝不删除锁文件。
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(path)?;
            file.try_lock().map_err(|error| match error {
                std::fs::TryLockError::WouldBlock => {
                    anyhow::Error::from(WorkspaceError::ExecutionBusy)
                }
                std::fs::TryLockError::Error(error) => error.into(),
            })?;
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
        let _file = self.lock_execution(&target.thread_id).await?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let updated = sqlx::query(
            "UPDATE execution_runs SET clean = 1 WHERE thread_id = ? AND generation = ? AND clean = 0",
        )
        .bind(&target.thread_id)
        .bind(target.generation)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(WorkspaceError::RecoveryGenerationMismatch.into());
        }
        tx.commit().await?;
        Ok(())
    }

    pub(super) async fn acquire_execution_lease_impl(
        &self,
        id: &ThreadId,
    ) -> Result<Arc<dyn SessionExecutionLease>> {
        if self.read_only {
            return Err(WorkspaceError::ExecutionLeaseRequired.into());
        }
        // 取得执行所有权是准入的最后一步：绑定已在同一次准入里复核过（解析或
        // `validate_session_binding`），这里只复核已记录证据，不再重复完整发现。
        self.reassert_session_binding_impl(id).await?;
        let parent: (Option<String>,) =
            sqlx::query_as("SELECT parent_thread_id FROM threads WHERE id = ?")
                .bind(id)
                .fetch_one(&self.pool)
                .await?;
        // Owned children have one owner: the root lease and its close transaction.
        if parent.0.is_some() {
            return Err(WorkspaceError::ExecutionLeaseRequired.into());
        }
        let file = self.lock_execution(id).await?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let prior: Option<(i64, bool)> =
            sqlx::query_as("SELECT generation, clean FROM execution_runs WHERE thread_id = ?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        if let Some((generation, false)) = prior {
            return Err(WorkspaceError::RecoveryRequired(RecoveryRequiredDetails {
                thread_id: id.clone(),
                generation,
            })
            .into());
        }
        Self::validate_session_binding_on(&mut tx, id).await?;
        let generation = prior
            .map_or(Some(1), |(generation, _)| generation.checked_add(1))
            .context("execution generation exhausted")?;
        sqlx::query(
            "INSERT INTO execution_runs (thread_id, generation, clean) VALUES (?, ?, 0)
            ON CONFLICT(thread_id) DO UPDATE SET generation = excluded.generation, clean = 0",
        )
        .bind(id)
        .bind(generation)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        let lease = Arc::new(ExecutionLease {
            thread_id: id.clone(),
            generation,
            pool: self.pool.clone(),
            file: tokio::sync::Mutex::new(Some(file)),
            active: AtomicBool::new(true),
            mutation_gate: Arc::new(tokio::sync::RwLock::new(())),
            mutation_uncertain: AtomicBool::new(false),
        });
        self.execution_leases
            .lock()
            .map_err(|_| WorkspaceError::ExecutionLeaseRequired)?
            .insert(id.clone(), Arc::downgrade(&lease));
        Ok(lease)
    }

    pub(super) async fn require_execution_lease(
        &self,
        id: &ThreadId,
    ) -> Result<Option<ExecutionWriteGuard>> {
        if self.read_only {
            return Err(WorkspaceError::ExecutionLeaseRequired.into());
        }
        let mut current = id.clone();
        let mut visited = std::collections::HashSet::new();
        let mut bound = false;
        loop {
            if !visited.insert(current.clone()) {
                return Err(WorkspaceError::InvalidBinding.into());
            }
            // An adopted legacy root can still have unbound children. Their mutations
            // belong to the same root owner even though their own binding is absent.
            bound |= self.load_session_binding_impl(&current).await?.is_some();
            let owned = self
                .execution_leases
                .lock()
                .map_err(|_| WorkspaceError::ExecutionLeaseRequired)?
                .get(&current)
                .and_then(std::sync::Weak::upgrade);
            if let Some(lease) = owned {
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
                return Ok(Some(ExecutionWriteGuard {
                    lease,
                    _gate: gate,
                    completed: false,
                }));
            }
            let parent: Option<(Option<String>,)> =
                sqlx::query_as("SELECT parent_thread_id FROM threads WHERE id = ?")
                    .bind(&current)
                    .fetch_optional(&self.pool)
                    .await?;
            match parent {
                Some((Some(parent),)) => current = parent,
                _ if bound => return Err(WorkspaceError::ExecutionLeaseRequired.into()),
                _ => return Ok(None),
            }
        }
    }
}
