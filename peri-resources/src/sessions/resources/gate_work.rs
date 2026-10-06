use super::*;
use peri_acp_types::session_resources::work::{
    WorkCommand, WorkQuery, WorkReceipt, WorkResolution, WorkSnapshot,
};

impl MutationGate {
    pub(in crate::sessions::resources) async fn load_work_availability(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<peri_acp_types::session_resources::work::WorkAvailability> {
        self.ensure_recovery_permitted()?;
        let root = self.work_root(id).await?;
        let pending = self.pending_for(&root);
        let _barrier = pending.barrier.read().await;
        if pending
            .work
            .lock()
            .expect("pending work lock poisoned")
            .is_none()
        {
            Self::check_pending(&pending, &root)?;
        }
        // 通知从未消费 blocked / candidates / pending_commands，不读取或克隆命令正文。
        self.data.load_work_availability(id).await
    }

    pub(in crate::sessions::resources) async fn load_work_delivery(
        &self,
        query: &peri_acp_types::session_resources::work::WorkDeliveryQuery,
    ) -> SessionResourceResult<Option<peri_acp_types::session_resources::work::DeliveryRecord>>
    {
        self.ensure_recovery_permitted()?;
        let root = self.work_root(&query.session_id).await?;
        let pending = self.pending_for(&root);
        let _barrier = pending.barrier.read().await;
        if pending
            .work
            .lock()
            .expect("pending work lock poisoned")
            .is_none()
        {
            Self::check_pending(&pending, &root)?;
        }
        self.data.load_work_delivery(query).await
    }

    async fn work_root(&self, id: &ThreadId) -> SessionResourceResult<ThreadId> {
        {
            let pending = self.pending.lock().expect("pending writes lock poisoned");
            for (root, writes) in pending.iter() {
                if writes
                    .work
                    .lock()
                    .expect("pending work lock poisoned")
                    .as_ref()
                    .is_some_and(|command| &command.session_id == id)
                {
                    return Ok(root.clone());
                }
            }
        }
        match self.data.session_root(id).await {
            Ok(root) => Ok(root),
            Err(error) if matches!(error.kind(), SessionResourceErrorKind::NotFound) => {
                Ok(id.clone())
            }
            Err(error) => Err(error),
        }
    }

    pub(in crate::sessions::resources) async fn load_work(
        &self,
        query: &WorkQuery,
    ) -> SessionResourceResult<WorkSnapshot> {
        self.ensure_recovery_permitted()?;
        let root = self.work_root(&query.session_id).await?;
        let pending = self.pending_for(&root);
        let _barrier = pending.barrier.read().await;
        let original = pending
            .work
            .lock()
            .expect("pending work lock poisoned")
            .clone();
        if original.is_none() {
            Self::check_pending(&pending, &root)?;
        }
        let mut snapshot = self.data.load_session_work(query).await?;
        if let Some(original) = original {
            if !snapshot.pending_commands.contains(&original) {
                snapshot.pending_commands.push(original);
            }
            snapshot.blocked = true;
            snapshot.candidates.clear();
        }
        Ok(snapshot)
    }

    pub(in crate::sessions::resources) async fn apply_work(
        &self,
        command: &WorkCommand,
    ) -> SessionResourceResult<WorkReceipt> {
        command.digest()?;
        self.ensure_session_write()?;
        let root = self.work_root(&command.session_id).await?;
        let scope = self.scope(&root, true).await?;
        *scope
            .pending
            .work
            .lock()
            .expect("pending work lock poisoned") = Some(command.clone());
        let result = self.data.apply_work_mutation(command).await;
        if result
            .as_ref()
            .err()
            .is_none_or(|error| error.effect() != MutationOutcome::Unknown)
        {
            scope
                .pending
                .work
                .lock()
                .expect("pending work lock poisoned")
                .take();
        }
        scope.settle(&result);
        result
    }

    pub(in crate::sessions::resources) async fn resolve_work(
        &self,
        command: &WorkCommand,
    ) -> SessionResourceResult<WorkResolution> {
        command.digest()?;
        self.ensure_recovery_write()?;
        let root = self.work_root(&command.session_id).await?;
        let pending = self.pending_for(&root);
        let _barrier = pending.barrier.write().await;
        let snapshot = self
            .data
            .load_session_work(&WorkQuery {
                session_id: root.clone(),
                limit: 64,
            })
            .await;
        if let Ok(snapshot) = snapshot {
            if !snapshot.pending_commands.is_empty()
                && !snapshot.pending_commands.contains(command)
                && self
                    .data
                    .load_work_command(&peri_acp_types::session_resources::work::WorkCommandQuery {
                        session_id: command.session_id.clone(),
                        mutation_id: command.mutation_id.clone(),
                    })
                    .await?
                    .is_none_or(|owned| owned.command != *command)
            {
                return Err(SessionResourceError::conflict(
                    "resolve the original owned work mutation",
                ));
            }
        }
        let original = pending
            .work
            .lock()
            .expect("pending work lock poisoned")
            .clone();
        if original
            .as_ref()
            .is_some_and(|original| original != command)
        {
            return Err(SessionResourceError::conflict(
                "resolve the original pending work mutation",
            ));
        }
        if original.is_none() && pending.uncertain.load(Ordering::Acquire) {
            return Err(SessionResourceError::persistence_uncertain(Some(root)));
        }
        *pending.work.lock().expect("pending work lock poisoned") = Some(command.clone());
        pending.uncertain.store(true, Ordering::Release);
        let resolution = match self.data.resolve_work_mutation(command).await {
            Ok(resolution) => resolution,
            Err(error) if matches!(error.kind(), SessionResourceErrorKind::Conflict { .. }) => {
                pending
                    .work
                    .lock()
                    .expect("pending work lock poisoned")
                    .take();
                pending.uncertain.store(false, Ordering::Release);
                return Err(error);
            }
            Err(_) => return Err(SessionResourceError::persistence_uncertain(Some(root))),
        };
        if !matches!(resolution, WorkResolution::Unknown) {
            pending
                .work
                .lock()
                .expect("pending work lock poisoned")
                .take();
            pending.uncertain.store(false, Ordering::Release);
        }
        Ok(resolution)
    }
}
