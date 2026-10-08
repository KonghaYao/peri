use std::sync::Arc;

use peri_acp_types::session_resources::{
    FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResources,
};
use peri_acp_types::workspace::{SessionBinding, SESSION_BINDING_VERSION};
use peri_agent::agent::stages::StageContext;
use peri_resources::sessions::SessionResourcesImpl;

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
                title: Some("middleware history fixture".into()),
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
                workspace_id: workspace.workspace_id,
                cwd_relative_to_workspace: workspace.relative_cwd,
            },
            frozen: FrozenSnapshotBytes::new("{\"version\":1,\"test\":true}"),
        })
        .await
        .unwrap();
    {
        let mut transcript = context.session.transcript.write();
        *transcript =
            std::mem::take(&mut *transcript).with_persistence(Arc::clone(&resources), session_id);
    }
    directory
}
