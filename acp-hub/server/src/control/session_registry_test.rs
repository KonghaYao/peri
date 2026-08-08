//! SessionRegistry 单测（设计稿 §16 测试 28–30）。

use std::sync::Arc;

use crate::control::StoreSink;
use crate::persist::{PersistConfig, Store};
use crate::state::doc_manager::{BatchConfig, DocManager};

use super::*;

/// 真实 RegistryState（经 DocManager 全局 registry 写者，§5.2 单写）：
/// `set_session_status` 等写回依赖存活写者与 Registry Doc 条目，channel
/// receiver 被 drop 的假句柄会在写回时 `ChannelClosed`/`NotFound`。
async fn test_registry() -> (RegistryState, Arc<DocManager>) {
    let tmp = tempfile::tempdir().unwrap();
    let persist_cfg = PersistConfig {
        data_dir: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let store = Arc::new(Store::open(&persist_cfg).unwrap());
    store.recover().await;
    let sink = Arc::new(StoreSink::new(store.clone()).await.unwrap());
    let doc = Arc::new(DocManager::new(BatchConfig::default(), sink.clone()));
    let registry = doc.registry();
    (registry, doc)
}

#[tokio::test]
async fn register_and_bind_resolve() {
    let (reg, _doc) = test_registry().await;
    let reg = SessionRegistry::new(reg);
    reg.register("s1", "m1", Some("title")).await.unwrap();
    let e = reg.entry("s1").await.unwrap();
    assert_eq!(e.state, SessionState::Accepting);
    assert_eq!(e.machine_id, "m1");

    reg.bind("s1", "acp-1").await.unwrap();
    assert_eq!(reg.resolve("acp-1").await.as_deref(), Some("s1"));
    assert_eq!(reg.acp_session_id("s1").await.as_deref(), Some("acp-1"));
    // 幂等 bind。
    reg.bind("s1", "acp-1").await.unwrap();
    // binding 冲突。
    assert!(matches!(
        reg.bind("s2", "acp-1").await,
        Err(SessionError::BindingConflict(_))
    ));
}

#[tokio::test]
async fn bind_before_frames_dropped_semantics() {
    let (reg, _doc) = test_registry().await;
    let reg = SessionRegistry::new(reg);
    reg.register("s1", "m1", None).await.unwrap();
    // binding 前 resolve 未命中（§6.2：binding 前帧一律丢弃）。
    assert_eq!(reg.resolve("acp-x").await, None);
    reg.bind("s1", "acp-x").await.unwrap();
    assert_eq!(reg.resolve("acp-x").await.as_deref(), Some("s1"));
}

#[tokio::test]
async fn pending_close_offline() {
    let (reg, _doc) = test_registry().await;
    let reg = SessionRegistry::new(reg);
    reg.register("s1", "m1", None).await.unwrap();
    reg.request_close_offline("s1").await.unwrap();
    assert_eq!(reg.entry("s1").await.unwrap().state, SessionState::PendingClose);
    assert!(reg.pending_close_sessions().await.contains(&"s1".to_string()));
    // close 完成后清 pending_close（§7.6）。
    reg.transition("s1", SessionState::Closed).await.unwrap();
    assert!(reg.pending_close_sessions().await.is_empty());
}

#[tokio::test]
async fn reconcile_alive_report() {
    let (reg, _doc) = test_registry().await;
    let reg = SessionRegistry::new(reg);
    reg.register("s1", "m1", None).await.unwrap(); // 存活
    reg.register("s2", "m1", None).await.unwrap(); // machine 未报（missing）
    reg.register("s3", "m1", None).await.unwrap();
    reg.transition("s3", SessionState::Closed).await.unwrap(); // 终态（意外存活）

    let report = reg
        .reconcile_alive("m1", &["s1".to_string(), "s3".to_string()])
        .await
        .unwrap();
    assert_eq!(report.alive, vec!["s1".to_string()]);
    assert!(report.missing.contains(&"s2".to_string()));
    // s3 终态但 machine 声称存活 → 意外存活 + kill 裁决（§7.5）。
    assert!(report.unexpected_alive.contains(&"s3".to_string()));
    assert!(report.to_kill.contains(&"s3".to_string()));
}

#[tokio::test]
async fn reconcile_alive_pending_close_kill() {
    let (reg, _doc) = test_registry().await;
    let reg = SessionRegistry::new(reg);
    reg.register("s1", "m1", None).await.unwrap();
    reg.request_close_offline("s1").await.unwrap();
    // machine 重连后对账：pending_close 补发 kill（§7.6）。
    let report = reg.reconcile_alive("m1", &[]).await.unwrap();
    assert!(report.to_kill.contains(&"s1".to_string()));
}

#[tokio::test]
async fn terminal_transition_guard() {
    let (reg, _doc) = test_registry().await;
    let reg = SessionRegistry::new(reg);
    reg.register("s1", "m1", None).await.unwrap();
    reg.transition("s1", SessionState::Ended).await.unwrap();
    // 终态不可逆（防御：不覆盖）。
    reg.transition("s1", SessionState::Gap).await.unwrap();
    assert_eq!(reg.entry("s1").await.unwrap().state, SessionState::Ended);
}

#[tokio::test]
async fn active_turn_tracking() {
    let (reg, _doc) = test_registry().await;
    let reg = SessionRegistry::new(reg);
    reg.register("s1", "m1", None).await.unwrap();
    reg.set_active_turn("s1", "t1").await;
    assert_eq!(reg.active_turn("s1").await.as_deref(), Some("t1"));
    reg.clear_active_turn("s1").await;
    assert_eq!(reg.active_turn("s1").await, None);
}

#[tokio::test]
async fn sessions_for_machine() {
    let (reg, _doc) = test_registry().await;
    let reg = SessionRegistry::new(reg);
    reg.register("s1", "m1", None).await.unwrap();
    reg.register("s2", "m2", None).await.unwrap();
    let list = reg.sessions_for_machine("m1").await;
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].0, "s1");
}

/// 状态机全路径（§7.3）：crashed（进程崩溃）、gap（分区）→ 恢复可用
/// （补推追平后清除，迁移回非 gap 状态）。
#[tokio::test]
async fn transition_crashed_and_gap_recover() {
    let (reg, _doc) = test_registry().await;
    let reg = SessionRegistry::new(reg);
    // crashed：进程崩溃（终态，视图保留）。
    reg.register("s1", "m1", None).await.unwrap();
    reg.transition("s1", SessionState::Crashed).await.unwrap();
    assert_eq!(reg.entry("s1").await.unwrap().state, SessionState::Crashed);
    assert!(reg.entry("s1").await.unwrap().state.is_terminal());
    // ended：ACP 进程退出。
    reg.register("s2", "m1", None).await.unwrap();
    reg.transition("s2", SessionState::Ended).await.unwrap();
    assert_eq!(reg.entry("s2").await.unwrap().state, SessionState::Ended);
    // gap：machine 分区 → 补推追平 → 恢复可用（§7.3「Gap 清除 → 恢复
    // 可用、可开新 turn」）。
    reg.register("s3", "m1", None).await.unwrap();
    reg.transition("s3", SessionState::Gap).await.unwrap();
    assert_eq!(reg.entry("s3").await.unwrap().state, SessionState::Gap);
    reg.transition("s3", SessionState::Accepting).await.unwrap();
    assert_eq!(reg.entry("s3").await.unwrap().state, SessionState::Accepting);
}

/// 错误路径：未知 session 的迁移/离线 close → NotFound（§7.3）。
#[tokio::test]
async fn transition_unknown_session_not_found() {
    let (reg, _doc) = test_registry().await;
    let reg = SessionRegistry::new(reg);
    assert!(matches!(
        reg.transition("ghost", SessionState::Closed).await,
        Err(SessionError::NotFound(_))
    ));
    assert!(matches!(
        reg.request_close_offline("ghost").await,
        Err(SessionError::NotFound(_))
    ));
}
