use std::sync::Arc;

use crate::session::executor::{PromptResult, SessionContext, TurnInput};
use peri_acp_types::{
    session::{MessageQueue, SessionAccessPort, SessionInbox},
    session_resources::{FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResources},
    workspace::SessionBinding,
};
use peri_agent::session::user_input_mailbox::{UserInputAttemptOutcome, UserInputMailbox};

pub(in crate::host) async fn run_session_loop(
    ctx: SessionContext,
    turn: TurnInput,
) -> PromptResult {
    let mailbox = ctx.user_input_mailbox.as_ref().unwrap().clone();
    let ticket = mailbox
        .attach_external_attempt(
            ctx.cancel.clone(),
            !turn.continuation && turn.content.is_empty(),
        )
        .unwrap();
    let result = crate::session::executor::run_session_loop(ctx, turn).await;
    let outcome = if result.failure.is_some() || result.persistence_inconsistent {
        UserInputAttemptOutcome::Failed
    } else if result.ok {
        UserInputAttemptOutcome::Completed
    } else {
        UserInputAttemptOutcome::Interrupted
    };
    mailbox.finish_attempt(&ticket, outcome);
    result
}

pub(in crate::host) async fn new_resources(
    session_id: &str,
) -> (Arc<dyn SessionResources>, Arc<tempfile::TempDir>) {
    let directory = Arc::new(tempfile::tempdir().unwrap());
    let resources: Arc<dyn SessionResources> = Arc::new(
        peri_resources::sessions::SessionResourcesImpl::open(directory.path().join("history.db"))
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
            frozen: FrozenSnapshotBytes::new(r#"{"v":1}"#),
        })
        .await
        .unwrap();
    (resources, directory)
}

pub(in crate::host) fn initialize_runtime(
    ctx: &mut SessionContext,
    directory: Option<Arc<tempfile::TempDir>>,
) {
    if ctx.session_access.is_none() {
        ctx.session_access = Some(Arc::new(MockSessionAccess {
            session_id: ctx.session_id.clone(),
            inbox: Arc::new(SessionInbox::new(Arc::new(MessageQueue::new()))),
            tasks: Arc::new(peri_agent::agent::async_tasks::TaskManager::new()),
            idle: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            _directory: directory,
        }));
    }
    let inbox = ctx
        .session_access
        .as_ref()
        .unwrap()
        .session_inbox(&ctx.session_id)
        .unwrap();
    ctx.user_input_mailbox = Some(UserInputMailbox::new(
        ctx.session_id.clone(),
        inbox,
        Arc::new(|_| {}),
    ));
}

struct MockSessionAccess {
    session_id: String,
    inbox: Arc<SessionInbox>,
    tasks: Arc<dyn peri_acp_types::tasks::TaskManager>,
    idle: Arc<std::sync::atomic::AtomicBool>,
    _directory: Option<Arc<tempfile::TempDir>>,
}

impl SessionAccessPort for MockSessionAccess {
    fn v2_message_queue(&self, session_id: &str) -> Option<MessageQueue> {
        (session_id == self.session_id).then(|| self.inbox.queue().clone())
    }
    fn session_inbox(&self, session_id: &str) -> Option<Arc<SessionInbox>> {
        (session_id == self.session_id).then(|| self.inbox.clone())
    }
    fn idle_suspended_flag(&self, session_id: &str) -> Option<Arc<std::sync::atomic::AtomicBool>> {
        (session_id == self.session_id).then(|| self.idle.clone())
    }
    fn task_manager(
        &self,
        session_id: &str,
    ) -> Option<Arc<dyn peri_acp_types::tasks::TaskManager>> {
        (session_id == self.session_id).then(|| self.tasks.clone())
    }
    fn goal_controller(
        &self,
        _session_id: &str,
    ) -> Option<Arc<dyn peri_acp_types::goal::GoalController>> {
        None
    }
    fn register_runtime(
        &self,
        _session_id: &str,
    ) -> Option<peri_acp_types::frozen::RegisterRuntimeFn> {
        None
    }
    fn deregister_runtime(
        &self,
        _session_id: &str,
    ) -> Option<peri_acp_types::frozen::DeregisterRuntimeFn> {
        None
    }
    fn cancel_cascade_children(&self, _session_id: &str) {}
    fn cron_bridge_for(&self, _session_id: &str) -> bool {
        false
    }
}
