//! Store owner claim and its in-process lease admission.

use super::*;
use peri_acp_types::workspace::ExecutionOwnerClaim;

pub(super) async fn abandon_with_canonical_guard<'a>(
    local: &'a dyn LocalExecutionPort,
    data: &'a dyn SessionDataPort,
    id: &'a ThreadId,
    lease: &'a Arc<dyn SessionExecutionLease>,
    revoke: RevokeEffect<'a>,
) -> SessionResourceResult<()> {
    let facts = SessionFacts { root: id.clone() };
    let owned = local
        .owner_lease(id, &facts)
        .await
        .map_err(execution_failure)?
        .ok_or_else(lease_required)?;
    local
        .abandon_initialization(
            id,
            lease,
            Box::pin(async move {
                if !owned.is_active() {
                    return match data.load_meta(id).await {
                        Ok(_) => Err(lease_required()),
                        Err(error)
                            if matches!(error.kind(), SessionResourceErrorKind::NotFound) =>
                        {
                            Ok(())
                        }
                        Err(error) => Err(error),
                    };
                }
                revoke.await
            }),
        )
        .await
}

impl SessionResourcesImpl {
    pub(super) async fn install_execution_claim(
        &self,
        id: &ThreadId,
        lease: Arc<dyn SessionExecutionLease>,
        require_closing: bool,
        expected_previous_epoch: Option<i64>,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>> {
        let facts = self.gate.session_facts(id).await?;
        let data = self.gate.data();
        let local = self
            .gate
            .local()
            .owner_lease(id, &facts)
            .await
            .map_err(execution_failure)?
            .ok_or_else(lease_required)?;
        let claim = if let Some(existing) = data.execution_owner_token(&facts.root) {
            if require_closing && expected_previous_epoch != Some(existing.epoch) {
                return Err(SessionResourceError::conflict(
                    "closing owner epoch changed",
                ));
            }
            match data.renew_execution_owner(&existing).await {
                Ok(()) => ExecutionOwnerClaim {
                    token: existing,
                    prior_unreleased: local.prior_unreleased_generation(),
                },
                Err(error) if matches!(error.kind(), SessionResourceErrorKind::Conflict { .. }) => {
                    // An expired generation may still have local writes holding this lease.
                    // Drain them before installing a new token that they could otherwise use.
                    tokio::time::timeout(SETTLE_WAIT, local.wait_for_in_flight())
                        .await
                        .map_err(|_| {
                            SessionResourceError::conflict("execution writes did not drain")
                        })?;
                    data.claim_execution_owner(
                        &facts.root,
                        require_closing,
                        expected_previous_epoch,
                    )
                    .await?
                }
                Err(error) => return Err(error),
            }
        } else {
            data.claim_execution_owner(&facts.root, require_closing, expected_previous_epoch)
                .await?
        };
        local.install_owner_token(claim.token.clone());
        local.install_prior_unreleased(claim.prior_unreleased);
        data.install_execution_owner_token(claim.token);
        Ok(lease)
    }

    pub(super) async fn acquire_execution_owner(
        &self,
        id: &ThreadId,
        workspace: &ResolvedWorkspace,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>> {
        self.gate.ensure_session_write()?;
        let facts = self.gate.session_facts(id).await?;
        self.validate_session(id, workspace).await?;
        if let Some(machine_id) = self.gate.data().machine_id_of(id).await? {
            if machine_id
                != crate::sessions::machine::current().map_err(|_| {
                    crate::sessions::sqlite_store::unavailable(
                        "machine identity is not initialized",
                    )
                })?
            {
                return Err(SessionResourceError::new(
                    SessionResourceErrorKind::Workspace(WorkspaceError::Unavailable),
                ));
            }
        }
        let lease = self
            .gate
            .local()
            .acquire_lease(id, &facts)
            .await
            .map_err(execution_failure)?;
        self.install_execution_claim(id, lease, false, None).await
    }

    pub(super) async fn claim_closing_execution_owner(
        &self,
        root: &ThreadId,
        expected_current_epoch: i64,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>> {
        self.gate.ensure_session_write()?;
        let facts = self.gate.session_facts(root).await?;
        if facts.root != *root {
            return Err(invalid_input("closing takeover requires root session"));
        }
        let lease = self
            .gate
            .local()
            .acquire_lease(root, &facts)
            .await
            .map_err(execution_failure)?;
        self.install_execution_claim(root, lease, true, Some(expected_current_epoch))
            .await
    }
}
