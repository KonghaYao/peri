use super::*;
use crate::config::Config;

fn directory() -> SessionDirectory {
    let config =
        Config::parse_from([], |key| (key == "HOME").then(|| "/fixture".to_owned())).unwrap();
    SessionDirectory::new(
        config.browser,
        "http://127.0.0.1:1/".to_owned(),
        config.secret,
    )
}

#[tokio::test]
async fn live_publication_binds_one_core_to_both_indexes_and_revokes_both_at_close() {
    let directory = directory();
    let id: SessionId = "transport-fixture".into();
    let entry = directory.initialize(id.clone());
    assert!(entry.admit().is_none());
    assert!(directory.list().is_empty());
    assert!(!directory.publish(&entry));
    assert!(entry.start_initialize());
    assert!(!entry.start_initialize());
    assert!(directory.publish(&entry));
    assert!(!directory.publish(&entry));
    let (token, core) = directory.list().pop().unwrap();
    assert!(token == derive_token(&directory.secret, core.session_id()));
    assert_ne!(core.session_id(), id.as_ref());
    assert!(directory.by_token("unknown").is_none());
    let via_token = directory.by_token(&token).unwrap();
    let via_id = directory.entry(&id).unwrap();
    assert!(Arc::ptr_eq(&via_id, &via_token));
    let (core_from_permit, permit) = via_token.admit().unwrap();
    assert!(Arc::ptr_eq(&core, &core_from_permit));
    assert_eq!(core.list_pages().await["count"], 0);

    entry.begin_close(CloseReason::Delete);
    entry.begin_close(CloseReason::Transport);
    assert!(entry.stop.is_cancelled());
    assert!(via_token.admit().is_none());
    assert!(directory.by_token(&token).is_none());
    assert!(directory.list().is_empty());
    assert!(directory.entry(&id).is_some(), "结算前保留 Closing 记录");
    let mut settle = std::pin::pin!(entry.settle_domain());
    assert!(futures::poll!(&mut settle).is_pending());
    drop(permit);
    let outcome = settle.await;
    assert!(outcome.is_ok());
    directory.finish(&id, &entry, outcome);
    let (first, duplicate) = tokio::join!(entry.wait(), entry.wait());
    assert!(first.is_ok() && duplicate.is_ok());
    assert!(directory.entry(&id).is_none());
}

#[tokio::test]
async fn shutdown_includes_inflight_creation_and_never_publishes_late_sessions() {
    let directory = directory();
    let creation = directory.create_permit().unwrap();
    let initial = directory.initialize("initial".into());
    assert!(initial.start_initialize());
    directory.begin_shutdown();
    assert!(directory.create_permit().is_none());
    assert!(initial.stop.is_cancelled());
    assert!(!directory.publish(&initial));
    let late_id: SessionId = "late".into();
    let late = directory.initialize(late_id.clone());
    assert!(late.stop.is_cancelled());
    assert!(!late.start_initialize());
    assert!(!directory.publish(&late));
    assert!(directory.list().is_empty());
    let mut wait = std::pin::pin!(directory.wait_shutdown());
    assert!(futures::poll!(&mut wait).is_pending());
    directory.finish(&late_id, &late, late.settle_domain().await);
    directory.finish(&"initial".into(), &initial, initial.settle_domain().await);
    drop(creation);
    wait.await.unwrap();
}

#[tokio::test]
async fn an_expired_initialization_cannot_become_live() {
    let directory = directory();
    let id: SessionId = "incomplete".into();
    let entry = directory.initialize(id.clone());
    assert!(entry.start_initialize());
    entry.expire_initialize();
    assert!(entry.stop.is_cancelled());
    assert!(!directory.publish(&entry));
    assert!(entry.admit().is_none());
    directory.finish(&id, &entry, entry.settle_domain().await);
    entry.wait().await.unwrap();
}
