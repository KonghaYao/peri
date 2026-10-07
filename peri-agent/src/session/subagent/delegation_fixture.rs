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
        .load_session_work(&WorkQuery {
            session_id: initiator.into(),
            limit: 1,
        })
        .await
        .unwrap();
    let arguments = "{}".to_owned();
    let digest = format!("{:x}", Sha256::digest(arguments.as_bytes()));
    apply_fixture_mutation(
        resources,
        WorkCommand {
            session_id: initiator.into(),
            recipient_lifecycle: control.lifecycle,
            mutation_id: format!("fixture-delegation-prepare:{invocation_id}"),
            action: WorkAction::PrepareInvocation {
                expected_revision: snapshot.state.revision,
                intent: InvocationIntent {
                    invocation_id: invocation_id.into(),
                    tool_call_id: format!("model-call:{invocation_id}"),
                    tool_name: "fixture-delegation".into(),
                    arguments_json: arguments.clone(),
                    arguments_digest: digest.clone(),
                    effective_tool_name: "fixture-delegation".into(),
                    effective_arguments_json: arguments,
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
    let snapshot = resources
        .load_session_work(&WorkQuery {
            session_id: initiator.into(),
            limit: 1,
        })
        .await
        .unwrap();
    apply_fixture_mutation(
        resources,
        WorkCommand {
            session_id: initiator.into(),
            recipient_lifecycle: control.lifecycle,
            mutation_id: format!("fixture-delegation-resources:{invocation_id}"),
            action: WorkAction::BindResourceOwners {
                expected_revision: snapshot.state.revision,
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
            .load_session_work(&WorkQuery {
                session_id: command.session_id.clone(),
                limit: 1,
            })
            .await
            .unwrap();
        match &mut command.action {
            WorkAction::PrepareInvocation {
                expected_revision, ..
            }
            | WorkAction::BindResourceOwners {
                expected_revision, ..
            } => {
                *expected_revision = snapshot.state.revision;
            }
            _ => panic!("unexpected delegation fixture action"),
        }
        command.mutation_id = format!("{identity}:{}", command.digest().unwrap());
        let receipt = resources
            .apply_work_mutation(
                &peri_acp_types::session_resources::work::PreparedWorkCommand::try_new(
                    command.clone(),
                )
                .unwrap(),
            )
            .await
            .unwrap();
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
