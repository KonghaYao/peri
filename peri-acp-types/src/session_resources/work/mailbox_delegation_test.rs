use super::*;

fn delegated_candidate() -> WorkFacts {
    let mut facts = facts();
    apply(
        &mut facts,
        command(WorkAction::PublishDelivery {
            delivery: publication("delivery"),
        }),
    );
    facts.deliveries[0].delegation = Some(DelegationRef {
        parent_session_id: "parent-session".into(),
        parent_lifecycle: 1,
        delegation_id: "parent-invocation".into(),
    });
    facts
}

#[test]
fn claim_atomically_transfers_candidate_delegation_to_its_processing() {
    let mut facts = delegated_candidate();
    admit(&mut facts);
    let delegation = facts.deliveries[0].delegation.clone();
    let action = WorkAction::ClaimBatch {
        guard: guard(&facts),
        batch_id: work_id(&facts),
        delivery_ids: vec!["delivery".into()],
    };
    let transition = apply(&mut facts, command(action));
    assert_eq!(facts.processing.as_ref().unwrap().delegation, delegation);
    assert_eq!(
        facts.processing.as_ref().unwrap().stage,
        WorkStage::ReasonReady
    );
    assert_eq!(facts.deliveries[0].delegation, None);
    assert_eq!(
        facts.deliveries[0].processing_id.as_deref(),
        Some(facts.admission.as_ref().unwrap().admission.work_id.as_str())
    );
    assert!(transition.writes.iter().any(|write| matches!(write,
        WorkWrite::Processing { record, .. } if record.delegation == delegation
    )));
    assert!(transition.writes.iter().any(|write| matches!(write,
        WorkWrite::Delivery { record, .. } if record.delegation.is_none()
    )));
}

#[test]
fn claim_cannot_merge_distinct_or_missing_delegation_authorizations() {
    for other_delegation in [
        None,
        Some(DelegationRef {
            parent_session_id: "parent-session".into(),
            parent_lifecycle: 1,
            delegation_id: "another-parent-invocation".into(),
        }),
    ] {
        let mut facts = delegated_candidate();
        apply(
            &mut facts,
            command(WorkAction::PublishDelivery {
                delivery: publication("second-delivery"),
            }),
        );
        facts.deliveries[1].delegation = other_delegation;
        admit(&mut facts);
        assert_rejected(
            &facts,
            WorkAction::ClaimBatch {
                guard: guard(&facts),
                batch_id: work_id(&facts),
                delivery_ids: vec!["delivery".into(), "second-delivery".into()],
            },
            WorkRejection::Conflict,
        );
        assert!(facts.processing.is_none());
        assert!(facts.deliveries[0].delegation.is_some());
        assert_eq!(facts.head.required_count, 2);
    }
}
