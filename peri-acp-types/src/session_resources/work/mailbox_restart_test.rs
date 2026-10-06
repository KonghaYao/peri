use super::*;
use crate::session::MessagePolicy;

fn command(action: WorkAction) -> WorkCommand {
    WorkCommand {
        session_id: "session".into(),
        recipient_lifecycle: 1,
        mutation_id: "publication".into(),
        action,
    }
}

fn stage(command_id: &str) -> WorkAction {
    WorkAction::StageUserInput {
        input_id: "input".into(),
        content: EvidenceWrite {
            session_id: "session".into(),
            storage_scope: "workspace".into(),
            payload_id: "input-content".into(),
            encoding: 1,
            bytes: b"draft".to_vec(),
        }
        .reference()
        .unwrap(),
        command_id: command_id.into(),
        fingerprint: 7,
    }
}

fn apply(facts: &mut WorkFacts, action: WorkAction) -> WorkTransition {
    let transition = transition_work(&command(action), facts).unwrap();
    assert_eq!(transition.receipt.decision, WorkDecision::Accepted);
    for write in &transition.writes {
        match write {
            WorkWrite::Head { record, .. } => facts.head = record.clone(),
            WorkWrite::Draft { record, .. } => {
                facts
                    .drafts
                    .retain(|draft| draft.input_id != record.input_id);
                facts.drafts.push(record.clone());
            }
            WorkWrite::Delivery { record, .. } => {
                facts
                    .deliveries
                    .retain(|delivery| delivery.delivery_id != record.delivery_id);
                facts.deliveries.push(record.clone());
            }
            _ => panic!("unexpected mailbox write"),
        }
    }
    transition
}

fn staged() -> WorkFacts {
    let mut facts = WorkFacts {
        session_id: "session".into(),
        control: Default::default(),
        head: Default::default(),
        processing: None,
        deliveries: Vec::new(),
        effects: Vec::new(),
        drafts: Vec::new(),
        admission: None,
        recovery_descriptor: None,
        terminal_obligation: None,
        legacy_evidence: None,
        parent_binding_receipt: None,
        parent_effect: None,
    };
    apply(&mut facts, stage("enqueue-old"));
    facts
}

fn publication(draft: &StagedUserInput, delivery_id: &str) -> PublishDelivery {
    let canonical_content = EvidenceWrite {
        session_id: "session".into(),
        storage_scope: "workspace".into(),
        payload_id: format!("canonical-{}", draft.input_id),
        encoding: 1,
        bytes: b"canonical message, not source JSON".to_vec(),
    }
    .reference()
    .unwrap();
    PublishDelivery {
        delivery_id: delivery_id.into(),
        event: WorkEvent {
            producer_namespace: "user-input".into(),
            event_id: format!("user-input:{}:publish", draft.input_id),
            event_kind: "input".into(),
            causation_id: Some(
                serde_json::to_string(&UserInputPublicationIdentity {
                    input_id: draft.input_id.clone(),
                    publication_generation: "publish".into(),
                    fingerprint: 19,
                    command_id: "publish".into(),
                    draft_binding: StagedUserInputPublicationBinding {
                        draft_revision: draft.revision,
                        draft_fingerprint: draft.fingerprint,
                        canonical_content: canonical_content.clone(),
                    },
                })
                .unwrap(),
            ),
            content: WorkPayload {
                message_id: crate::messages::MessageId::new(),
                role: "user".into(),
                content: canonical_content,
                tool_call_id: None,
            },
        },
        purpose: DeliveryPurpose::UserInput,
        policy: MessagePolicy::ensure_processing(),
    }
}

fn publish_action(facts: &WorkFacts, deliveries: Vec<PublishDelivery>) -> WorkAction {
    WorkAction::PublishStagedUserInputs {
        expected_revision: facts.head.change_seq,
        expected_control_generation: facts.control.control_generation,
        expected_attempt: facts.control.attempt.clone(),
        interrupt_current: false,
        deliveries,
    }
}

fn published() -> WorkFacts {
    let mut facts = staged();
    let delivery = publication(&facts.drafts[0], "publication");
    let action = publish_action(&facts, vec![delivery]);
    apply(&mut facts, action);
    facts
}

#[test]
fn selection_binds_each_draft_to_its_delivery_not_the_shared_mutation() {
    let mut facts = staged();
    let mut second = stage("enqueue-second");
    let WorkAction::StageUserInput { input_id, .. } = &mut second else {
        unreachable!()
    };
    *input_id = "second".into();
    apply(&mut facts, second);
    let deliveries = facts
        .drafts
        .iter()
        .map(|draft| publication(draft, &format!("delivery-{}", draft.input_id)))
        .collect();
    let action = publish_action(&facts, deliveries);
    apply(&mut facts, action);
    for draft in &facts.drafts {
        assert_eq!(
            draft.publication_id,
            Some(format!("delivery-{}", draft.input_id))
        );
        assert_eq!(draft.status, StagedUserInputStatus::Published);
    }
    assert_eq!(facts.head.required_count, 2);
}

#[test]
fn selection_rejects_missing_identity_or_mismatched_binding_without_partial_writes() {
    let facts = staged();
    for mismatch in 0..5 {
        let mut delivery = publication(&facts.drafts[0], "delivery");
        let mut identity: UserInputPublicationIdentity =
            serde_json::from_str(delivery.event.causation_id.as_deref().unwrap()).unwrap();
        match mismatch {
            0 => identity.input_id = "other".into(),
            1 => identity.draft_binding.draft_revision += 1,
            2 => identity.draft_binding.draft_fingerprint += 1,
            3 => identity.draft_binding.canonical_content.payload_id = "other".into(),
            4 => {}
            _ => unreachable!(),
        }
        delivery.event.causation_id = if mismatch == 4 {
            None
        } else {
            Some(serde_json::to_string(&identity).unwrap())
        };
        assert_rejected(
            &facts,
            publish_action(&facts, vec![delivery]),
            if mismatch == 4 {
                WorkRejection::InvalidTransition
            } else {
                WorkRejection::Conflict
            },
        );
    }
}

fn withdrawal(facts: &WorkFacts, stop: bool) -> WorkAction {
    let generation = stop.then_some(facts.control.control_generation);
    WorkAction::WithdrawDelivery {
        expected_revision: facts.head.change_seq,
        expected_control_generation: generation,
        delivery_id: "publication".into(),
        authorization_ref: serde_json::json!({
            "command_id": "withdraw",
            "fingerprint": 11,
            "expected_revision": facts.head.change_seq,
            "expected_control_generation": generation,
        })
        .to_string(),
    }
}

fn assert_rejected(facts: &WorkFacts, action: WorkAction, reason: WorkRejection) {
    let transition = transition_work(&command(action), facts).unwrap();
    assert_eq!(
        transition.receipt.decision,
        WorkDecision::Rejected { reason }
    );
    assert!(transition.writes.is_empty());
}

#[test]
fn withdrawal_atomically_updates_draft_and_preserves_authorization() {
    for stop in [false, true] {
        let mut facts = published();
        let revision = facts.drafts[0].revision;
        let action = withdrawal(&facts, stop);
        let WorkAction::WithdrawDelivery {
            authorization_ref, ..
        } = &action
        else {
            unreachable!()
        };
        let evidence = format!("withdrawn: {authorization_ref}");
        let transition = apply(&mut facts, action);
        assert!(transition.writes.iter().any(|write| matches!(
            write, WorkWrite::Draft { expected_revision: Some(expected), .. } if *expected == revision
        )));
        assert_eq!(facts.drafts[0].revision, revision + 1);
        assert_eq!(
            facts.drafts[0].status,
            if stop {
                StagedUserInputStatus::Queued
            } else {
                StagedUserInputStatus::Withdrawn
            }
        );
        assert_eq!(facts.drafts[0].publication_id, None);
        assert_eq!(facts.deliveries[0].obligation, ObligationStatus::Abandoned);
        assert_eq!(
            facts.deliveries[0].disposition.as_deref(),
            Some(evidence.as_str())
        );
        assert_eq!(facts.head.required_count, 0);
        assert_eq!(facts.head.required_bytes, 0);
    }
}

#[test]
fn new_enqueue_reenters_withdrawn_or_disposed_draft_without_reviving_old_delivery() {
    for legacy_disposed in [false, true] {
        let mut facts = published();
        let action = withdrawal(&facts, false);
        apply(&mut facts, action);
        if legacy_disposed {
            facts.drafts[0].status = StagedUserInputStatus::Published;
            facts.drafts[0].publication_id = Some("publication".into());
        }
        let prior = facts.drafts[0].clone();
        let old_delivery = facts.deliveries[0].clone();
        apply(&mut facts, stage("enqueue-new"));
        assert_eq!(facts.drafts[0].revision, prior.revision + 1);
        assert!(facts.drafts[0].publication_generation > prior.publication_generation);
        assert!(facts.drafts[0].sequence > prior.sequence);
        assert_eq!(facts.drafts[0].status, StagedUserInputStatus::Queued);
        assert_eq!(facts.drafts[0].publication_id, None);
        assert_eq!(facts.deliveries[0], old_delivery);
        apply(
            &mut facts,
            WorkAction::PublishDelivery {
                delivery: old_delivery.publication.clone(),
            },
        );
        assert_eq!(facts.deliveries[0], old_delivery);
        assert_eq!(facts.head.required_count, 0);
    }
}

#[test]
fn active_draft_and_changed_old_command_cannot_reenter() {
    let facts = published();
    assert_rejected(&facts, stage("enqueue-new"), WorkRejection::Conflict);
    let mut withdrawn = facts.clone();
    let action = withdrawal(&withdrawn, false);
    apply(&mut withdrawn, action);
    let mut changed = stage("enqueue-old");
    let WorkAction::StageUserInput { fingerprint, .. } = &mut changed else {
        unreachable!()
    };
    *fingerprint += 1;
    assert_rejected(&withdrawn, changed, WorkRejection::Conflict);
}

#[test]
fn withdrawal_rejects_claimed_projected_stale_or_ambiguous_facts() {
    let mut facts = published();
    facts.deliveries[0].processing_id = Some("claimed".into());
    assert_rejected(
        &facts,
        withdrawal(&facts, true),
        WorkRejection::InvalidTransition,
    );
    facts.deliveries[0].processing_id = None;
    facts.deliveries[0].projection = Some(crate::messages::MessageId::new());
    assert_rejected(
        &facts,
        withdrawal(&facts, false),
        WorkRejection::InvalidTransition,
    );
    facts.deliveries[0].projection = None;
    let stale = withdrawal(&facts, true);
    facts.control.control_generation += 1;
    assert_rejected(&facts, stale, WorkRejection::StaleControlGeneration);
    let mut duplicate = facts.drafts[0].clone();
    duplicate.input_id = "other-input".into();
    facts.drafts.push(duplicate);
    assert_rejected(&facts, withdrawal(&facts, false), WorkRejection::Conflict);
}
