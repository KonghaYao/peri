use peri_acp_types::session_resources::{
    FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResources,
};
use peri_acp_types::tools::ToolContext;
use peri_acp_types::workspace::SessionBinding;
use peri_resources::sessions::SessionResourcesImpl;
use serde_json::Value;
use std::sync::Arc;
pub(crate) struct CurrentInvocationFixture {
    _directory: tempfile::TempDir,
    pub(crate) resources: Arc<dyn SessionResources>,
    session_id: String,
    count: usize,
}
impl CurrentInvocationFixture {
    pub(crate) async fn new(session_id: &str, _tool: &str, inputs: &[Value]) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let resources: Arc<dyn SessionResources> = Arc::new(
            SessionResourcesImpl::open(directory.path().join("invocations.db"))
                .await
                .unwrap(),
        );
        let workspace = resources.resolve_workspace(directory.path()).await.unwrap();
        resources
            .create_session(&NewSession {
                thread_id: session_id.into(),
                created_at: peri_time::now_utc_rfc3339(),
                meta: NewSessionMeta {
                    title: None,
                    cwd: workspace.cwd.to_string_lossy().into_owned(),
                    parent_thread_id: None,
                    hidden: false,
                    cancel_policy: Default::default(),
                    snapshot_at_message_id: None,
                },
                binding: SessionBinding::from_workspace(&workspace),
                frozen: FrozenSnapshotBytes::new("{\"version\":1}"),
            })
            .await
            .unwrap();
        Self {
            _directory: directory,
            resources,
            session_id: session_id.into(),
            count: inputs.len(),
        }
    }
    pub(crate) fn context<'context>(
        &self,
        index: usize,
        cwd: &'context str,
    ) -> ToolContext<'context> {
        assert!(index < self.count);
        let mut context =
            ToolContext::new(&[], cwd).with_session_identity(&self.session_id, "fixture-turn");
        context.invocation_id = Some(format!("invocation-{index}"));
        context.tool_call_id = Some(format!("call-{index}"));
        context.session_resources = Some(self.resources.clone());
        context
    }
}
