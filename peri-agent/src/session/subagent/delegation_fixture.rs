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
    let receipt = resources
        .apply_work_mutation(&WorkCommand {
            session_id: initiator.into(),
            recipient_lifecycle: control.lifecycle,
            mutation_id: format!("fixture-delegation-prepare:{invocation_id}"),
            action: WorkAction::PrepareInvocation {
                expected_revision: snapshot.state.revision,
                intent: InvocationIntent {
                    invocation_id: invocation_id.into(),
                    tool_call_id: invocation_id.into(),
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
        })
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    let snapshot = resources
        .load_session_work(&WorkQuery {
            session_id: initiator.into(),
            limit: 1,
        })
        .await
        .unwrap();
    let receipt = resources
        .apply_work_mutation(&WorkCommand {
            session_id: initiator.into(),
            recipient_lifecycle: control.lifecycle,
            mutation_id: format!("fixture-delegation-resources:{invocation_id}"),
            action: WorkAction::BindResourceOwners {
                expected_revision: snapshot.state.revision,
                connections_json: "{}".into(),
                authorization_ref: "fixture-delegation-authorization".into(),
            },
        })
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
}
