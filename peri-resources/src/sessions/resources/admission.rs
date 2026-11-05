//! 执行可用性判定与「已保存创建」的重试准入。

use super::*;

impl SessionResourcesImpl {
    pub(super) async fn execution_availability(
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
        if let Some(machine_id) = self.gate.data().machine_id_of(id).await? {
            if machine_id
                != local.machine_id().map_err(|_| {
                    crate::sessions::failure::unavailable("machine identity is not initialized")
                })?
            {
                return Ok(ExecutionAvailability::WorkspaceUnavailable);
            }
        }
        let meta = self.gate.data().load_meta(id).await?;
        if matches!(self.home, SessionDataHome::RemoteStore)
            && self
                .gate
                .data()
                .binding_discovery_snapshot(id)
                .await?
                .is_none()
        {
            return Ok(ExecutionAvailability::WorkspaceUnavailable);
        }
        if !local.directory_available(Path::new(&meta.cwd)).await {
            return Ok(ExecutionAvailability::WorkspaceUnavailable);
        }
        if matches!(self.home, SessionDataHome::RemoteStore) {
            match self.recheck_binding_of(id, true).await {
                Ok(_) => {}
                Err(error) if matches!(error.kind(), SessionResourceErrorKind::Workspace(_)) => {
                    return Ok(ExecutionAvailability::WorkspaceUnavailable);
                }
                Err(error) => return Err(error),
            }
        }
        Ok(ExecutionAvailability::Available)
    }

    /// 完整创建的重试：不可变事实一致时只重新建立执行准入。
    ///
    /// 前提（又是同一次创建、工作区证据仍然一致）成立时补上准入；否则保留 identity 并
    /// 如实报告「已保存、未准入」——既不重造 binding/frozen，也不谎称「确定未创建」。
    pub(super) async fn admit_saved_creation(
        &self,
        input: &NewSession,
    ) -> SessionResourceResult<()> {
        let id = &input.thread_id;
        let saved = self.gate.data().load_snapshot(id).await?;
        let created_at = chrono::DateTime::parse_from_rfc3339(&input.created_at)
            .map_err(|_| invalid_input("invalid session creation timestamp"))?;
        let snapshot_at = input
            .meta
            .snapshot_at_message_id
            .map(|message| message.as_uuid().to_string());
        if saved.meta.id != *id
            || saved.meta.created_at != created_at
            || saved.meta.cwd != input.meta.cwd
            || saved.meta.parent_thread_id != input.meta.parent_thread_id
            || saved.meta.snapshot_at_message_id != snapshot_at
            || saved.binding != BindingState::Bound(input.binding.clone())
            || saved.frozen != FrozenState::Present(input.frozen.clone())
        {
            return Err(SessionResourceError::conflict(
                "session identity has different immutable creation facts",
            ));
        }
        if !matches!(
            self.execution_availability(id).await?,
            ExecutionAvailability::Available
        ) {
            return Err(SessionResourceError::saved_but_not_admitted(id.clone()));
        }
        self.recheck_binding(Some(&input.binding), false).await?;
        Ok(())
    }

    /// 测试用：本机组合背后的 SQLite 连接池（逐条构造事实的夹具使用）。
    #[cfg(all(test, not(target_os = "emscripten")))]
    pub(in crate::sessions) fn local_pool(&self) -> &sqlx::SqlitePool {
        self.gate
            .local()
            .sqlite_pool()
            .expect("local composition always has a SQLite pool")
    }
}
