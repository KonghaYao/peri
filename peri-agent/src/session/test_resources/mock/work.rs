use std::sync::Arc;

use peri_acp_types::session_resources::{work::*, SessionResources};
use sha2::{Digest, Sha256};

pub(crate) async fn bind_fixture_task(
    resources: Arc<dyn SessionResources>,
    session_id: &str,
    lifecycle: u64,
    task_id: &str,
) -> InvocationIntent {
    let mut snapshot = resources
        .inspect_work(&WorkQuery::new(session_id, WorkSelector::Head))
        .await
        .unwrap();
    let arguments_json = "{}".to_owned();
    let arguments_ref = crate::agent::stages::prepare_work_evidence(resources.as_ref(), session_id, arguments_json.as_bytes().to_vec()).await.unwrap();
    let digest = format!("{:x}", Sha256::digest(arguments_json.as_bytes()));
    let intent = InvocationIntent {
        invocation_id: uuid::Uuid::now_v7().to_string(),
        tool_call_id: uuid::Uuid::now_v7().to_string(),
        tool_name: "explicit-fixture-tool".into(),
        arguments: arguments_ref.clone(),
        arguments_digest: digest.clone(),
        effective_tool_name: "explicit-fixture-tool".into(),
        effective_arguments: arguments_ref,
        effective_arguments_digest: digest,
        owner_identity: "explicit-fixture-owner".into(),
        scope_id: session_id.into(),
        scope_epoch: None,
        authorization_ref: "explicit-fixture-authorization".into(),
        recovery_locator: format!("fixture-task:{task_id}"),
    };
    let receipt = resources
        .apply_work_mutation(&WorkCommand {
            session_id: session_id.into(),
            recipient_lifecycle: lifecycle,
            mutation_id: format!("fixture-prepare:{}", intent.invocation_id),
            action: WorkAction::PrepareInvocation {
                expected_revision: snapshot.head.change_seq,
                intent: intent.clone(),
            },
        })
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    snapshot = resources
        .inspect_work(&WorkQuery::new(session_id, WorkSelector::Head))
        .await
        .unwrap();
    let receipt = resources
        .apply_work_mutation(&WorkCommand {
            session_id: session_id.into(),
            recipient_lifecycle: lifecycle,
            mutation_id: format!("fixture-bind:{}", intent.invocation_id),
            action: WorkAction::ReconcileTaskBinding {
                expected_revision: snapshot.head.change_seq,
                binding: TaskBinding {
                    invocation_id: intent.invocation_id.clone(),
                    owner_identity: intent.owner_identity.clone(),
                    owner_task_id: task_id.into(),
                    initiator_session_id: session_id.into(),
                    recipient_lifecycle: lifecycle,
                    recovery_locator: intent.recovery_locator.clone(),
                    authorization_ref: intent.authorization_ref.clone(),
                },
            },
        })
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    intent
}
