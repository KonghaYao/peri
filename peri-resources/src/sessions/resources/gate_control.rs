use super::*;
use peri_acp_types::session_resources::{
    ControlCommand, ControlReceipt, ControlResolution, ControlState,
};

impl MutationGate {
    async fn control_root(&self, id: &ThreadId) -> SessionResourceResult<ThreadId> {
        {
            let pending = self.pending.lock().expect("pending writes lock poisoned");
            for (root, writes) in pending.iter() {
                if writes
                    .control
                    .lock()
                    .expect("pending control lock poisoned")
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

    pub(in crate::sessions::resources) async fn load_control(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<ControlState> {
        self.ensure_recovery_permitted()?;
        let root = self.control_root(id).await?;
        let pending = self.pending_for(&root);
        let _barrier = pending.barrier.read().await;
        Self::check_pending(&pending, &root)?;
        self.data.load_session_control(id).await
    }

    pub(in crate::sessions::resources) async fn apply_control(
        &self,
        command: &ControlCommand,
    ) -> SessionResourceResult<ControlReceipt> {
        command.digest()?;
        self.ensure_session_write()?;
        let root = self.control_root(&command.session_id).await?;
        let scope = self.scope(&root, true).await?;
        *scope
            .pending
            .control
            .lock()
            .expect("pending control lock poisoned") = Some(command.clone());
        let result = self.data.apply_session_control(command).await;
        if result
            .as_ref()
            .err()
            .is_none_or(|error| error.effect() != MutationOutcome::Unknown)
        {
            scope
                .pending
                .control
                .lock()
                .expect("pending control lock poisoned")
                .take();
        }
        scope.settle(&result);
        result
    }

    pub(in crate::sessions::resources) async fn resolve_control(
        &self,
        command: &ControlCommand,
    ) -> SessionResourceResult<ControlResolution> {
        command.digest()?;
        self.ensure_recovery_write()?;
        let root = self.control_root(&command.session_id).await?;
        let pending = self.pending_for(&root);
        let _barrier = pending.barrier.write().await;
        let original = pending
            .control
            .lock()
            .expect("pending control lock poisoned")
            .clone();
        if original
            .as_ref()
            .is_some_and(|original| original != command)
        {
            return Err(SessionResourceError::conflict(
                "resolve the original pending control command",
            ));
        }
        if original.is_none() && pending.uncertain.load(Ordering::Acquire) {
            return Err(SessionResourceError::persistence_uncertain(Some(root)));
        }
        *pending
            .control
            .lock()
            .expect("pending control lock poisoned") = Some(command.clone());
        pending.uncertain.store(true, Ordering::Release);
        let resolution = match self.data.resolve_session_control(command).await {
            Ok(resolution) => resolution,
            Err(error) if matches!(error.kind(), SessionResourceErrorKind::Conflict { .. }) => {
                pending
                    .control
                    .lock()
                    .expect("pending control lock poisoned")
                    .take();
                pending.uncertain.store(false, Ordering::Release);
                return Err(error);
            }
            Err(_) => return Err(SessionResourceError::persistence_uncertain(Some(root))),
        };
        if !matches!(resolution, ControlResolution::Unknown) {
            pending
                .control
                .lock()
                .expect("pending control lock poisoned")
                .take();
            pending.uncertain.store(false, Ordering::Release);
        }
        Ok(resolution)
    }
}
