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
        .await
        .unwrap();
    let result = crate::session::executor::run_session_loop(ctx, turn).await;
    let outcome = if result.failure.is_some() || result.persistence_inconsistent {
        UserInputAttemptOutcome::Failed
    } else if result.ok {
        UserInputAttemptOutcome::Completed
    } else {
        UserInputAttemptOutcome::Interrupted
    };
    mailbox.finish_attempt(&ticket, outcome).await;
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
    register_session_row(&resources, session_id, directory.path()).await;
    (resources, directory)
}

/// 生产装配先 `create_session` 再派发 prompt；MQ 入队要求 thread 行已存在，
/// 夹具必须复刻这一步，否则持久入队会被当作未知会话拒绝。
pub(in crate::host) async fn register_session_row(
    resources: &Arc<dyn SessionResources>,
    session_id: &str,
    cwd: &std::path::Path,
) {
    let workspace = resources.resolve_workspace(cwd).await.unwrap();
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
}

pub(in crate::host) fn initialize_runtime(
    ctx: &mut SessionContext,
    directory: Option<Arc<tempfile::TempDir>>,
) {
    if ctx.session_access.is_none() {
        // 与生产装配同形：有会话存储时 MQ 必须绑定同一份 resources/thread，
        // 否则 Receive 只改内存、Full Compact 在存储里找不到已显示的消息。
        let queue = match (&ctx.session_resources, &ctx.thread_id) {
            (Some(resources), Some(thread_id)) => {
                MessageQueue::bind(Arc::clone(resources), thread_id.clone())
            }
            _ => MessageQueue::new(),
        };
        ctx.session_access = Some(Arc::new(MockSessionAccess {
            session_id: ctx.session_id.clone(),
            inbox: Arc::new(SessionInbox::new(Arc::new(queue))),
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
