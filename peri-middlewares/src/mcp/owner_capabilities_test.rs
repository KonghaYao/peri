use super::*;

fn capabilities() -> OwnerCapabilities {
    OwnerCapabilities {
        version: 1,
        owner_identity: "authenticated-owner".into(),
        scope_id: "child".into(),
        scope_epoch: 0,
        invocation_discovery: true,
        retained_tasks: true,
        scope_close_barrier: true,
        resource_settlement: true,
        invocation_idempotency: false,
    }
}

#[test]
fn capability_evidence_is_scoped_and_does_not_claim_owner_restart_persistence() {
    let capabilities = capabilities();
    capabilities.validate_scope("child").unwrap();
    assert!(capabilities.validate_scope("root").is_err());
    assert_eq!(
        capabilities.recovery_guarantee(),
        OwnerRecoveryGuarantee::DiscoverableWhileOwnerAlive
    );
    assert!(!capabilities.invocation_idempotency);
}

#[test]
fn missing_discovery_is_explicit_best_effort_not_reliable_close() {
    let mut capabilities = capabilities();
    capabilities.invocation_discovery = false;
    capabilities.scope_close_barrier = false;
    assert_eq!(
        capabilities.recovery_guarantee(),
        OwnerRecoveryGuarantee::BestEffortAtLeastOnce
    );
    assert!(capabilities.require_close_barrier().is_err());
}
