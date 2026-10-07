use std::sync::Arc;

use peri_acp_types::session_resources::{
    FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResources,
};
use peri_acp_types::workspace::{SessionBinding, SESSION_BINDING_VERSION};
use peri_agent::agent::stages::StageContext;
use peri_resources::sessions::SessionResourcesImpl;

#[path = "../../../peri-agent/src/session/test_resources/mock/admission.rs"]
mod admission;

pub(crate) async fn bind(context: &mut StageContext) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    let resources: Arc<dyn SessionResources> = Arc::new(
        SessionResourcesImpl::open(directory.path().join("fixture.db"))
            .await
            .unwrap(),
    );
    let repository = directory.path().join("workspace");
    std::fs::create_dir(&repository).unwrap();
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&repository)
        .status()
        .unwrap()
        .success());
    let workspace = resources.resolve_workspace(&repository).await.unwrap();
    let session_id = uuid::Uuid::now_v7().to_string();
    resources
        .create_session(&NewSession {
            thread_id: session_id.clone(),
            created_at: peri_time::now_utc_rfc3339(),
            meta: NewSessionMeta {
                title: Some("durable middleware fixture".into()),
                cwd: workspace.cwd.to_string_lossy().into_owned(),
                parent_thread_id: None,
                hidden: false,
                cancel_policy: Default::default(),
                snapshot_at_message_id: None,
            },
            binding: SessionBinding {
                schema_version: SESSION_BINDING_VERSION,
                revision: 1,
                project_id: workspace.project_id,
                workspace_id: workspace.execution_registration_id,
                cwd_relative_to_workspace: workspace.relative_cwd,
            },
            frozen: FrozenSnapshotBytes::new("{\"version\":1,\"test\":true}"),
        })
        .await
        .unwrap();
    let history = context.session.transcript.read().persisted_payloads();
    if !history.is_empty() {
        resources
            .append_history(&session_id, &history)
            .await
            .unwrap();
    }
    {
        let mut transcript = context.session.transcript.write();
        *transcript =
            std::mem::take(&mut *transcript).with_persistence(Arc::clone(&resources), session_id);
    }
    context.recipient_lifecycle = Some(1);
    context.execution_admission_port = Some(Arc::new(admission::FixtureAdmission(resources)));
    directory
}

pub(crate) async fn finish(context: &StageContext, evidence_id: &str) {
    use peri_acp_types::execution_admission::{
        AttemptStoppedProof, SettlementOutcome, SettlementRequest,
    };
    let writer = context
        .session
        .transcript
        .read()
        .persist_tx_handle()
        .unwrap();
    peri_agent::session::MessageTranscript::flush_via_tx(&writer)
        .await
        .unwrap();
    let admission = context.session.turn.work_admission().unwrap().clone();
    let proof = AttemptStoppedProof::AttemptStopped {
        instance_id: admission.instance_id.clone(),
        generation_id: admission.generation_id.clone(),
        execution: admission.execution.clone(),
        evidence_id: evidence_id.into(),
    };
    let outcome = context
        .execution_admission_port()
        .unwrap()
        .settle(SettlementRequest {
            admission: admission.clone(),
            proof,
        })
        .await
        .unwrap();
    assert!(matches!(outcome, SettlementOutcome::Applied { receipt }
        if receipt.admission == admission && receipt.evidence_id == evidence_id));
}
