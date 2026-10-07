//! `SessionResources` 门面替身：实现契约语义，未覆盖的行为按 `Unsupported` 拒绝而不是静默 no-op。

use super::*;

/// 替身的工作区投影：按保存路径造一个自洽的 `ResolvedWorkspace`。
///
/// 替身不建模发现与登记，只保证「路径即目录、目录即根」这一条自洽关系；真实门面
/// 才做发现、登记与复核。
fn doubled_workspace(cwd: &str) -> ResolvedWorkspace {
    let cwd = std::path::PathBuf::from(cwd);
    let workspace_id = peri_acp_types::workspace::WorkspaceId::new();
    ResolvedWorkspace {
        project_id: peri_acp_types::workspace::ProjectId::new(),
        workspace_id,
        execution_registration_id: workspace_id,
        cwd: cwd.clone(),
        root: cwd,
        relative_cwd: std::path::PathBuf::new(),
        discovery_snapshot: None,
    }
}

fn unsupported(behavior: &str) -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::Internal {
        detail: format!("{behavior} is not implemented by the in-memory test double"),
    })
}

#[async_trait]
impl SessionResources for MockSessionResources {
    async fn inspect_work(
        &self,
        query: &peri_acp_types::session_resources::work::WorkQuery,
    ) -> SessionResourceResult<peri_acp_types::session_resources::work::WorkInspection> {
        self.durable_backend(&query.session_id)
            .await
            .inspect_work(query)
            .await
    }
    async fn read_evidence(
        &self,
        query: &peri_acp_types::session_resources::work::EvidenceQuery,
    ) -> SessionResourceResult<peri_acp_types::session_resources::work::EvidenceRecord> {
        self.durable_backend(&query.session_id)
            .await
            .read_evidence(query)
            .await
    }
    async fn prepare_evidence(
        &self,
        evidence: &peri_acp_types::session_resources::work::EvidenceWrite,
    ) -> SessionResourceResult<peri_acp_types::session_resources::work::PayloadRef> {
        self.ensure_writable()?;
        self.durable_backend(&evidence.session_id)
            .await
            .prepare_evidence(evidence)
            .await
    }
    async fn apply_work_mutation(
        &self,
        command: &peri_acp_types::session_resources::work::WorkCommand,
    ) -> SessionResourceResult<peri_acp_types::session_resources::work::WorkReceipt> {
        self.ensure_writable()?;
        let backend = self.durable_backend(&command.session_id).await;
        let receipt = backend.apply_work_mutation(command).await?;
        let snapshot = backend.load_session_snapshot(&command.session_id).await?;
        self.with_region(&command.session_id, |region| {
            for payload in snapshot.payloads {
                if !region
                    .payloads
                    .iter()
                    .any(|existing| existing.id() == payload.id())
                {
                    region.payloads.push(payload);
                }
            }
        });
        Ok(receipt)
    }
    async fn resolve_work_mutation(
        &self,
        command: &peri_acp_types::session_resources::work::WorkCommand,
    ) -> SessionResourceResult<peri_acp_types::session_resources::work::WorkResolution> {
        self.durable_backend(&command.session_id)
            .await
            .resolve_work_mutation(command)
            .await
    }
    async fn load_session_control(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<peri_acp_types::session_resources::ControlState> {
        self.durable_backend(id)
            .await
            .load_session_control(id)
            .await
    }
    async fn apply_session_control(
        &self,
        command: &peri_acp_types::session_resources::ControlCommand,
    ) -> SessionResourceResult<peri_acp_types::session_resources::ControlReceipt> {
        self.ensure_writable()?;
        self.durable_backend(&command.session_id)
            .await
            .apply_session_control(command)
            .await
    }
    async fn resolve_session_control(
        &self,
        command: &peri_acp_types::session_resources::ControlCommand,
    ) -> SessionResourceResult<peri_acp_types::session_resources::ControlResolution> {
        self.durable_backend(&command.session_id)
            .await
            .resolve_session_control(command)
            .await
    }
    async fn inspect_availability(
        &self,
        _session: Option<&ThreadId>,
    ) -> SessionResourceResult<SessionAvailability> {
        Ok(SessionAvailability {
            access: AccessMode::ReadWrite,
            execution: Some(peri_acp_types::session_resources::ExecutionAvailability::Available),
            capabilities: if self.history_read_only.load(Ordering::SeqCst) {
                DataCapabilities::HistoryReadOnly
            } else {
                DataCapabilities::Complete
            },
        })
    }

    async fn resolve_workspace(
        &self,
        _cwd: &std::path::Path,
    ) -> SessionResourceResult<ResolvedWorkspace> {
        Err(unsupported("resolve_workspace"))
    }

    async fn validate_session(
        &self,
        _id: &ThreadId,
        _workspace: &ResolvedWorkspace,
    ) -> SessionResourceResult<()> {
        Ok(())
    }

    async fn finish_close(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.ensure_writable()?;
        self.closing.lock().unwrap().remove(id);
        Ok(())
    }

    async fn close_settlement(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<peri_acp_types::session_resources::CloseSettlement> {
        Ok(if self.closing.lock().unwrap().contains(id) {
            peri_acp_types::session_resources::CloseSettlement::Pending
        } else {
            peri_acp_types::session_resources::CloseSettlement::Finished
        })
    }

    async fn create_session(&self, _input: &NewSession) -> SessionResourceResult<()> {
        Err(unsupported("create_session"))
    }

    async fn abandon_initialization(&self, _id: &ThreadId) -> SessionResourceResult<()> {
        Err(unsupported("abandon_initialization"))
    }

    async fn begin_initialization(
        &self,
        _draft: &peri_acp_types::session_resources::NewSessionDraft,
    ) -> SessionResourceResult<Arc<dyn peri_acp_types::session_resources::SessionInitialization>>
    {
        Err(unsupported("begin_initialization"))
    }

    async fn discard_incomplete_initialization(&self, _id: &ThreadId) -> SessionResourceResult<()> {
        Err(unsupported("discard_incomplete_initialization"))
    }

    async fn adopt_legacy_session(
        &self,
        _id: &ThreadId,
        _saved_cwd: &str,
        _workspace: &ResolvedWorkspace,
        _frozen: &peri_acp_types::session_resources::FrozenSnapshotBytes,
    ) -> SessionResourceResult<()> {
        Err(unsupported("adopt_legacy_session"))
    }

    async fn load_session_snapshot(&self, id: &ThreadId) -> SessionResourceResult<SessionSnapshot> {
        // 阶段门按「一次快照读取内部的读序」排布：先行读、继承区、flags。
        if self.claimed.load(Ordering::SeqCst) {
            Self::wait_gate(&self.load_gate).await;
            Self::wait_gate(&self.inherited_load_gate).await;
            Self::wait_gate(&self.flags_load_gate).await;
        }
        let read = self.snapshot_reads.fetch_add(1, Ordering::SeqCst) + 1;
        let fail_at = self.fail_snapshot_at.load(Ordering::SeqCst);
        if fail_at != 0 && read == fail_at {
            return Err(SessionResourceError::new(
                SessionResourceErrorKind::Internal {
                    detail: "failed to load messages".to_owned(),
                },
            ));
        }
        let Some(region) = self.region(id) else {
            return Err(SessionResourceError::new(
                SessionResourceErrorKind::NotFound,
            ));
        };
        Ok(SessionSnapshot {
            meta: region.meta.unwrap_or_else(|| default_meta_for(id)),
            binding: match region.binding {
                Some(binding) => BindingState::Bound(binding),
                None => BindingState::Missing,
            },
            frozen: match region.frozen {
                Some(bytes) => FrozenState::Present(FrozenSnapshotBytes::new(bytes)),
                None => FrozenState::LegacyAbsent,
            },
            payloads: region.payloads,
            flags: region.flags,
            inherited: region.inherited,
        })
    }

    async fn load_session_binding(&self, id: &ThreadId) -> SessionResourceResult<BindingState> {
        // 替身不建模本机登记：已登记的会话按「有绑定」回答，未登记按真实门面语义报
        // `NotFound`（不冒充 legacy 或「没有绑定」）。
        match self.region(id).and_then(|region| region.meta) {
            Some(meta) => Ok(BindingState::Bound(fixture_binding(&meta.cwd))),
            None => Err(SessionResourceError::new(
                SessionResourceErrorKind::NotFound,
            )),
        }
    }

    async fn validate_bound_workspace(
        &self,
        id: &ThreadId,
        _check: BindingRecheck,
    ) -> SessionResourceResult<ResolvedWorkspace> {
        match self.region(id).and_then(|region| region.meta) {
            Some(meta) => Ok(doubled_workspace(&meta.cwd)),
            None => Err(SessionResourceError::new(
                SessionResourceErrorKind::Workspace(WorkspaceError::BindingMissing),
            )),
        }
    }

    async fn load_session_history(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<Vec<PersistedPayload>> {
        // 夹具历史不入库：已登记会话如实回答空历史，未登记报 `NotFound`。
        match self.region(id).and_then(|region| region.meta) {
            Some(_) => Ok(Vec::new()),
            None => Err(SessionResourceError::new(
                SessionResourceErrorKind::NotFound,
            )),
        }
    }

    async fn load_session_meta(&self, id: &ThreadId) -> SessionResourceResult<ThreadMeta> {
        match self.region(id).and_then(|region| region.meta) {
            Some(meta) => Ok(meta),
            None => Err(SessionResourceError::new(
                SessionResourceErrorKind::NotFound,
            )),
        }
    }

    async fn list_sessions(
        &self,
        _query: &ScopedThreadQuery,
    ) -> SessionResourceResult<ScopedThreadPage> {
        Err(unsupported("list_sessions"))
    }

    async fn list_children(&self, parent: &ThreadId) -> SessionResourceResult<Vec<ThreadMeta>> {
        Ok(self
            .threads()
            .into_iter()
            .filter(|meta| meta.parent_thread_id.as_deref() == Some(parent.as_str()))
            .collect())
    }

    async fn list_session_tree(&self, _root: &ThreadId) -> SessionResourceResult<Vec<ThreadMeta>> {
        Ok(Vec::new())
    }

    async fn append_history(
        &self,
        id: &ThreadId,
        payloads: &[PersistedPayload],
    ) -> SessionResourceResult<()> {
        self.ensure_writable()?;
        if !self.writable {
            return Err(SessionResourceError::new(
                SessionResourceErrorKind::ReadOnlyStore,
            ));
        }
        self.injection.lock().unwrap().append_calls += 1;
        for payload in payloads {
            self.with_region(id, |region| region.payloads.push(payload.clone()));
        }
        Ok(())
    }

    async fn append_reminder_if_absent(
        &self,
        id: &ThreadId,
        message_id: peri_acp_types::messages::MessageId,
        reminder: &peri_acp_types::system_reminder::TrustedSystemReminder,
    ) -> SessionResourceResult<bool> {
        self.ensure_writable()?;
        self.with_region(id, |region| {
            if let Some(existing) = region
                .payloads
                .iter()
                .find(|payload| payload.id() == message_id)
            {
                if let PersistedPayload::SystemReminder {
                    reminder: stored, ..
                } = existing
                {
                    if stored.as_reminder() == reminder.as_reminder() {
                        return Ok(false);
                    }
                }
                return Err(unsupported("append_reminder_if_absent conflict"));
            }
            region.payloads.push(PersistedPayload::SystemReminder {
                id: message_id,
                reminder: reminder.clone(),
            });
            Ok(true)
        })
    }

    async fn mark_session_closing(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.ensure_writable()?;
        self.closing.lock().unwrap().insert(id.clone());
        Ok(())
    }

    async fn is_session_closing(&self, id: &ThreadId) -> SessionResourceResult<bool> {
        Ok(self.closing.lock().unwrap().contains(id))
    }

    async fn save_fork(&self, fork: &ForkSnapshot) -> SessionResourceResult<()> {
        self.ensure_writable()?;
        let target = &fork.target;
        self.with_region(&target.thread_id, |region| {
            region.meta = Some(child_meta(&target.thread_id, &target.meta, false));
            region.binding = Some(target.binding.clone());
            region.frozen = Some(target.frozen.as_str().to_owned());
            region.payloads = fork.payloads.clone();
            region.flags = fork.flags.clone();
        });
        Ok(())
    }

    async fn save_child(&self, child: &ChildSnapshot) -> SessionResourceResult<()> {
        self.ensure_writable()?;
        let target = &child.target;
        // 与真实门面同构：child 的 frozen 逐字节取自 root 已保存快照、绑定继承父会话、
        // 继承区来自调用方；这里不做 SQL 层校验，但保留「一次写入成立」的形状。
        self.with_region(&target.thread_id, |region| {
            region.meta = Some(child_meta(&target.thread_id, &target.meta, true));
            region.binding = Some(target.binding.clone());
            region.frozen = Some(target.frozen.as_str().to_owned());
            region.inherited = child.inherited.clone();
        });
        Ok(())
    }

    async fn claim_child_resume(
        &self,
        child: &ThreadId,
        _root: &ThreadId,
    ) -> SessionResourceResult<Box<dyn ChildResumeClaim>> {
        self.ensure_writable()?;
        // 与资源实现同构：门禁内「读状态 + 写 active」，已有 active 时拒绝并发认领。
        let previous = self
            .region(child)
            .and_then(|region| region.meta)
            .ok_or_else(|| SessionResourceError::new(SessionResourceErrorKind::NotFound))?
            .agent_status;
        if previous.is_active() {
            return Err(SessionResourceError::new(
                SessionResourceErrorKind::InvalidInput {
                    detail: "child session is still active".to_owned(),
                },
            ));
        }
        self.write_status(child, AgentStatus::Active);
        // 阶段门在写入之后：用例据此断言「已提交的写入不会被调用方 drop 撤销」。
        Self::wait_gate(&self.active_write_gate).await;
        self.claimed.store(true, Ordering::SeqCst);
        Ok(Box::new(MockResumeClaim {
            claimed: Arc::clone(&self.claimed),
            regions: Arc::clone(&self.regions),
            statuses: Arc::clone(&self.statuses),
            status_changed: Arc::clone(&self.status_changed),
            child: child.clone(),
            previous,
        }))
    }

    async fn apply_compaction(
        &self,
        session_id: &ThreadId,
        change: &CompactionChange,
    ) -> SessionResourceResult<()> {
        self.ensure_writable()?;
        let mut injection = self.injection.lock().unwrap();
        injection.compaction_calls += 1;
        if injection.fail_compaction {
            drop(injection);
            return Err(SessionResourceError::new(
                SessionResourceErrorKind::Internal {
                    detail: "injected compaction failure".to_owned(),
                },
            ));
        }
        drop(injection);
        self.with_region(session_id, |region| {
            for (id, value) in &change.flag_updates {
                region.flags.insert(*id, value.clone());
            }
            for message in &change.appended_messages {
                region
                    .payloads
                    .push(PersistedPayload::Message(message.clone()));
            }
        });
        Ok(())
    }

    async fn apply_message_projections(
        &self,
        id: &ThreadId,
        updates: &[(MessageId, MessageFlags)],
    ) -> SessionResourceResult<()> {
        self.ensure_writable()?;
        // 全有或全无：单次数据区写入，不产生中途可见的中间态（门面契约的原子性）。
        self.with_region(id, |region| {
            for (target, value) in updates {
                region.flags.insert(*target, value.clone());
            }
        });
        Ok(())
    }

    async fn rewind_history(
        &self,
        id: &ThreadId,
        boundary: RewindBoundary,
    ) -> SessionResourceResult<()> {
        self.ensure_writable()?;
        self.injection.lock().unwrap().rewind_calls += 1;
        let target = boundary.message_id();
        let Some(region) = self.region(id) else {
            return Err(SessionResourceError::new(
                SessionResourceErrorKind::NotFound,
            ));
        };
        let keep = match boundary {
            RewindBoundary::KeepThrough(_) => region
                .payloads
                .iter()
                .position(|payload| payload.id() == target)
                .map(|index| index + 1),
            RewindBoundary::RemoveFrom(_) => region
                .payloads
                .iter()
                .position(|payload| payload.id() == target),
        };
        let Some(len) = keep else {
            return Err(SessionResourceError::new(
                SessionResourceErrorKind::NotFound,
            ));
        };
        let removed: Vec<MessageId> = region.payloads[len..]
            .iter()
            .map(PersistedPayload::id)
            .collect();
        self.with_region(id, |region| {
            region.payloads.truncate(len);
            for removed_id in &removed {
                region.flags.remove(removed_id);
            }
        });
        Ok(())
    }

    async fn remove_history_entries(
        &self,
        id: &ThreadId,
        ids: &[MessageId],
    ) -> SessionResourceResult<()> {
        self.ensure_writable()?;
        self.with_region(id, |region| {
            region
                .payloads
                .retain(|payload| !ids.contains(&payload.id()));
            for removed in ids {
                region.flags.remove(removed);
            }
        });
        Ok(())
    }

    async fn update_session_meta(
        &self,
        id: &ThreadId,
        patch: &SessionMetaPatch,
    ) -> SessionResourceResult<()> {
        self.ensure_writable()?;
        if let Some(status) = patch.status {
            self.write_status(id, status);
        }
        Ok(())
    }

    async fn delete_session_tree(&self, _id: &ThreadId) -> SessionResourceResult<()> {
        Err(unsupported("delete_session_tree"))
    }

    async fn recover_session_persistence(
        &self,
        _id: &ThreadId,
    ) -> SessionResourceResult<PersistenceRecovery> {
        Ok(PersistenceRecovery::Recovered)
    }

    async fn drain_persistence(&self, _id: &ThreadId) -> SessionResourceResult<()> {
        Ok(())
    }
}
