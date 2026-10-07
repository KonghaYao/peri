use peri_acp_types::session_resources::{work::*, SessionResources};
use sha2::{Digest, Sha256};

pub(super) async fn prepare_delegation(
    resources: &dyn SessionResources,
    initiator: &str,
    invocation_id: &str,
) {
    let control = resources
        .load_session_control(&initiator.to_owned())
        .await
        .unwrap();
    let snapshot = resources
        .inspect_work(&WorkQuery::new(initiator, WorkSelector::Head))
        .await
        .unwrap();
    let arguments = "{}".to_owned();
    let arguments_ref = crate::agent::stages::prepare_work_evidence(
        resources,
        initiator,
        arguments.as_bytes().to_vec(),
    )
    .await
    .unwrap();
    let digest = format!("{:x}", Sha256::digest(arguments.as_bytes()));
    apply_fixture_mutation(
        resources,
        WorkCommand {
            session_id: initiator.into(),
            recipient_lifecycle: control.lifecycle,
            mutation_id: format!("fixture-delegation-prepare:{invocation_id}"),
            action: WorkAction::PrepareInvocation {
                expected_revision: snapshot.head.change_seq,
                intent: InvocationIntent {
                    invocation_id: invocation_id.into(),
                    tool_call_id: format!("model-call:{invocation_id}"),
                    tool_name: "fixture-delegation".into(),
                    arguments: arguments_ref.clone(),
                    arguments_digest: digest.clone(),
                    effective_tool_name: "fixture-delegation".into(),
                    effective_arguments: arguments_ref,
                    effective_arguments_digest: digest,
                    owner_identity: "fixture-agent-owner".into(),
                    scope_id: initiator.into(),
                    scope_epoch: None,
                    authorization_ref: "fixture-delegation-authorization".into(),
                    recovery_locator: format!("fixture-delegation:{invocation_id}"),
                },
            },
        },
    )
    .await;
    crate::session::test_resources::mock::work::dispatch_fixture_invocation(
        resources,
        initiator,
        invocation_id,
    )
    .await;
    let snapshot = resources
        .inspect_work(&WorkQuery::new(initiator, WorkSelector::Head))
        .await
        .unwrap();
    apply_fixture_mutation(
        resources,
        WorkCommand {
            session_id: initiator.into(),
            recipient_lifecycle: control.lifecycle,
            mutation_id: format!("fixture-delegation-resources:{invocation_id}"),
            action: WorkAction::BindResourceOwners {
                expected_revision: snapshot.head.change_seq,
                connections_json: "{}".into(),
                authorization_ref: "fixture-delegation-authorization".into(),
            },
        },
    )
    .await;
}

async fn apply_fixture_mutation(resources: &dyn SessionResources, mut command: WorkCommand) {
    let identity = command.mutation_id.clone();
    loop {
        let snapshot = resources
            .inspect_work(&WorkQuery::new(
                command.session_id.clone(),
                WorkSelector::Head,
            ))
            .await
            .unwrap();
        match &mut command.action {
            WorkAction::PrepareInvocation {
                expected_revision, ..
            }
            | WorkAction::BindResourceOwners {
                expected_revision, ..
            } => {
                *expected_revision = snapshot.head.change_seq;
            }
            _ => panic!("unexpected delegation fixture action"),
        }
        command.mutation_id = format!("{identity}:{}", command.digest().unwrap());
        let receipt = resources.apply_work_mutation(&command).await.unwrap();
        assert_eq!(receipt.session_id, command.session_id);
        assert_eq!(receipt.mutation_id, command.mutation_id);
        match receipt.decision {
            WorkDecision::Accepted => return,
            WorkDecision::Rejected {
                reason: WorkRejection::StaleRevision,
            } => {}
            decision => panic!("delegation fixture mutation rejected: {decision:?}"),
        }
    }
}
