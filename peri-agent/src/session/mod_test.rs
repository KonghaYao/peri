use super::*;

#[test]
fn test_session_construction() {
    let cwd: Arc<str> = Arc::from("/tmp/project");
    let frozen = FrozenContext::builder()
        .system_prompt("You are Peri.")
        .claude_md("# Rules")
        .build();
    let session = Session::new(cwd.clone(), frozen, Some("thread-1".into()));

    // 五个实体均可访问
    assert_eq!(&*session.store().cwd, "/tmp/project");
    assert_eq!(&*session.store().frozen.system_prompt, "You are Peri.");
    assert!(session.transcript().read().is_empty());
    assert!(session.queue().is_empty());
    assert_eq!(session.config().permission_mode(), PermissionMode::Default);
}

#[test]
fn test_session_store_access() {
    let cwd: Arc<str> = Arc::from("/tmp");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);

    let store = session.store();
    assert_ne!(store.session_id.as_uuid(), uuid::Uuid::nil());
    assert!(store.thread_id.is_none());
    assert!(!store.is_git_repo());

    store.set_is_git_repo(true);
    assert!(store.is_git_repo());
}

#[test]
fn test_session_transcript_access() {
    let cwd: Arc<str> = Arc::from("/tmp");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);

    // transcript() 返回 Arc clone，可跨线程共享
    let t1 = session.transcript();
    let t2 = session.transcript();
    assert!(Arc::ptr_eq(&t1, &t2), "多次调用应返回同一 Arc");
}

#[test]
fn test_session_queue_access() {
    let cwd: Arc<str> = Arc::from("/tmp");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);

    let q = session.queue();
    assert!(q.is_empty());
    assert_eq!(q.len(), 0);
}

#[test]
fn test_session_config_access() {
    let cwd: Arc<str> = Arc::from("/tmp");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);

    assert_eq!(session.config().max_iterations(), 500);
    session.config().set_max_iterations(100);
    assert_eq!(session.config().max_iterations(), 100);
}

#[test]
fn test_start_turn_creates_fresh_context() {
    let cwd: Arc<str> = Arc::from("/tmp/project");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd.clone(), frozen, None);

    let ctx = session.start_turn();
    assert_eq!(ctx.current_step(), 0, "新 turn 的 step 应为 0");
    assert_eq!(&*ctx.cwd, "/tmp/project", "turn 应共享 session 的 cwd");
    assert!(!ctx.is_cancelled(), "新 turn 不应已取消");

    // Cancel session config 后 turn 应感知
    session.config().cancel();
    assert!(ctx.is_cancelled(), "turn 应感知 session 级 cancel");
}

#[test]
fn test_start_turn_independent_turn_ids() {
    let cwd: Arc<str> = Arc::from("/tmp");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);

    let ctx1 = session.start_turn();
    let ctx2 = session.start_turn();
    assert_ne!(
        ctx1.turn_id, ctx2.turn_id,
        "每次 start_turn 应生成独立 TurnId"
    );
}

#[test]
fn test_session_is_arc_shared() {
    let cwd: Arc<str> = Arc::from("/tmp");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);

    // Session::new 返回 Arc<Self>，clone 应指向同一实例
    let clone = Arc::clone(&session);
    assert!(Arc::ptr_eq(&session, &clone));
}

#[test]
fn test_new_with_cancel_and_queue_shares_underlying_queue() {
    // 验证：传入外部 MessageQueue 后，session.queue() 与外部共享底层。
    // 这是 v2 路径 "session 共享 MessageQueue" 修复的核心契约。
    use crate::messages::BaseMessage;
    use crate::messages::MessageContent;
    use crate::session::queue::{MessageKind, MessageSource, QueuedMessage};

    let cwd: Arc<str> = Arc::from("/tmp");
    let frozen = FrozenContext::builder().build();
    let cancel = Arc::new(tokio_util::sync::CancellationToken::new());
    let shared = MessageQueue::new();

    // 在创建 session 前 push 一条——验证 session 内部 queue 能看到
    shared.push(QueuedMessage::new(
        MessageKind::Info,
        MessageSource::SystemInjected,
        BaseMessage::human(MessageContent::text("pre-existing")),
    ));

    let session = Session::new_with_cancel_and_queue(cwd, frozen, None, cancel, shared.clone());

    // session.queue() 应看到外部 push 的消息
    assert_eq!(
        session.queue().len(),
        1,
        "session.queue() 应与外部 shared 共享同一底层 VecDeque"
    );

    // 从 session.queue() push，外部 shared 应看到
    session.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human(MessageContent::text("from session")),
    ));
    assert_eq!(shared.len(), 2, "外部 shared 应看到 session 侧 push 的消息");
}

#[test]
fn test_new_with_cancel_and_queue_cancel_propagates() {
    // 验证：cancel_token 仍为 linked（父 cancel 时 session 内 turn 能感知）
    let cwd: Arc<str> = Arc::from("/tmp");
    let frozen = FrozenContext::builder().build();
    let cancel = Arc::new(tokio_util::sync::CancellationToken::new());
    let session =
        Session::new_with_cancel_and_queue(cwd, frozen, None, cancel.clone(), MessageQueue::new());

    let turn = session.start_turn();
    assert!(!turn.is_cancelled());
    cancel.cancel();
    assert!(turn.is_cancelled(), "linked cancel token 应传播到 turn");
}
