//! ChatRegistry 单测（设计稿 §16 测试 28–30）。

use std::sync::Arc;
use std::time::Duration;

use crate::control::StoreSink;
use crate::persist::{PersistConfig, Store};
use crate::state::doc_manager::{BatchConfig, DocManager};

use super::*;

/// 真实 RegistryState（经 DocManager 全局 registry 写者，§5.2 单写）：
/// `set_chat_status` 等写回依赖存活写者与 Registry Doc 条目，channel
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
    let reg = ChatRegistry::new(reg);
    reg.register("s1", "m1", Some("title"), "/", None)
        .await
        .unwrap();
    let e = reg.entry("s1").await.unwrap();
    assert_eq!(e.state, ChatState::Accepting);
    assert_eq!(e.instance_id, "m1");

    reg.bind("s1", "acp-1").await.unwrap();
    assert_eq!(reg.resolve("acp-1").await.as_deref(), Some("s1"));
    assert_eq!(reg.session_id("s1").await.as_deref(), Some("acp-1"));
    // 幂等 bind。
    reg.bind("s1", "acp-1").await.unwrap();
    // binding 冲突。
    assert!(matches!(
        reg.bind("s2", "acp-1").await,
        Err(ChatError::BindingConflict(_))
    ));
}

#[tokio::test]
async fn live_workspace_scan_uses_runtime_state_not_session_hints() {
    let (reg, _doc) = test_registry().await;
    let reg = ChatRegistry::new(reg);
    reg.register("active", "m1", None, "/", Some("project-a"))
        .await
        .unwrap();
    reg.register("other", "m1", None, "/", Some("project-b"))
        .await
        .unwrap();
    assert!(reg.has_live_workspace("project-a").await);
    assert!(!reg.has_live_workspace("missing").await);
    reg.transition("active", ChatState::Closed).await.unwrap();
    assert!(!reg.has_live_workspace("project-a").await);
    assert!(reg.has_live_workspace("project-b").await);
}

#[tokio::test]
async fn bind_before_frames_dropped_semantics() {
    let (reg, _doc) = test_registry().await;
    let reg = ChatRegistry::new(reg);
    reg.register("s1", "m1", None, "/", None).await.unwrap();
    // binding 前 resolve 未命中（§6.2：binding 前帧一律丢弃）。
    assert_eq!(reg.resolve("acp-x").await, None);
    reg.bind("s1", "acp-x").await.unwrap();
    assert_eq!(reg.resolve("acp-x").await.as_deref(), Some("s1"));
}

/// §8.5 会话切换：进程内 load 后 switch_session 更新 chat 当前会话。
#[tokio::test]
async fn switch_session_updates_current_and_bindings() {
    let (reg, _doc) = test_registry().await;
    let reg = ChatRegistry::new(reg);
    reg.register("s1", "m1", None, "/", None).await.unwrap();
    reg.bind("s1", "acp-1").await.unwrap();
    assert_eq!(reg.session_id("s1").await.as_deref(), Some("acp-1"));

    // load 切换：同 chat 内换到 acp-2（会话是进程内实体，进程不重建）。
    reg.switch_session("s1", "acp-2").await.unwrap();
    assert_eq!(reg.session_id("s1").await.as_deref(), Some("acp-2"));
    // 新旧会话 binding 均指向该 chat（relay 逐帧校验：任一 sessionId 的
    // 帧都属于本进程，§8.5）。
    assert_eq!(reg.resolve("acp-1").await.as_deref(), Some("s1"));
    assert_eq!(reg.resolve("acp-2").await.as_deref(), Some("s1"));

    // 幂等重切同会话：仍成功且无副作用。
    reg.switch_session("s1", "acp-2").await.unwrap();
    assert_eq!(reg.session_id("s1").await.as_deref(), Some("acp-2"));

    // 冲突：目标会话已被**另一 chat** 绑定 → BindingConflict（并发 load
    // 同会话的防御，§8.5）。
    reg.register("s2", "m1", None, "/", None).await.unwrap();
    reg.bind("s2", "acp-9").await.unwrap();
    assert!(matches!(
        reg.switch_session("s2", "acp-1").await,
        Err(ChatError::BindingConflict(_))
    ));
    assert_eq!(reg.session_id("s2").await.as_deref(), Some("acp-9"));
}

#[tokio::test]
async fn terminal_transition_releases_binding() {
    let (reg, _doc) = test_registry().await;
    let reg = ChatRegistry::new(reg);
    reg.register("s1", "m1", None, "/", None).await.unwrap();
    reg.bind("s1", "acp-1").await.unwrap();
    assert_eq!(reg.resolve("acp-1").await.as_deref(), Some("s1"));
    // 终态（关闭/崩溃/结束）→ 释放 binding（§8.5 激活语义：会话可再次
    // 被激活/加载，不因 chat 关闭而永久占用）。
    reg.transition("s1", ChatState::Closed).await.unwrap();
    assert_eq!(reg.resolve("acp-1").await, None, "终态后绑定必须释放");
    // 非终态迁移不释放（binding 在活跃生命周期内保持）。
    reg.register("s2", "m1", None, "/", None).await.unwrap();
    reg.bind("s2", "acp-2").await.unwrap();
    reg.transition("s2", ChatState::Gap).await.unwrap();
    assert_eq!(reg.resolve("acp-2").await.as_deref(), Some("s2"));
}

#[tokio::test]
async fn pending_close_offline() {
    let (reg, _doc) = test_registry().await;
    let reg = ChatRegistry::new(reg);
    reg.register("s1", "m1", None, "/", None).await.unwrap();
    reg.request_close_offline("s1").await.unwrap();
    assert_eq!(
        reg.entry("s1").await.unwrap().state,
        ChatState::PendingClose
    );
    assert!(reg.pending_close_chats().await.contains(&"s1".to_string()));
    // close 完成后清 pending_close（§7.6）。
    reg.transition("s1", ChatState::Closed).await.unwrap();
    assert!(reg.pending_close_chats().await.is_empty());
}

#[tokio::test]
async fn reconcile_alive_report() {
    let (reg, _doc) = test_registry().await;
    let reg = ChatRegistry::new(reg);
    reg.register("s1", "m1", None, "/", None).await.unwrap(); // 存活
    reg.register("s2", "m1", None, "/", None).await.unwrap(); // instance 未报（missing）
    reg.register("s3", "m1", None, "/", None).await.unwrap();
    reg.transition("s3", ChatState::Closed).await.unwrap(); // 终态（意外存活）

    let report = reg
        .reconcile_alive("m1", &["s1".to_string(), "s3".to_string()])
        .await
        .unwrap();
    assert_eq!(report.alive, vec!["s1".to_string()]);
    assert!(report.missing.contains(&"s2".to_string()));
    // s3 终态但 instance 声称存活 → 意外存活 + kill 裁决（§7.5）。
    assert!(report.unexpected_alive.contains(&"s3".to_string()));
    assert!(report.to_kill.contains(&"s3".to_string()));
}

#[tokio::test]
async fn reconcile_alive_pending_close_kill() {
    let (reg, _doc) = test_registry().await;
    let reg = ChatRegistry::new(reg);
    reg.register("s1", "m1", None, "/", None).await.unwrap();
    reg.request_close_offline("s1").await.unwrap();
    // instance 重连后对账：pending_close 补发 kill（§7.6）。
    let report = reg.reconcile_alive("m1", &[]).await.unwrap();
    assert!(report.to_kill.contains(&"s1".to_string()));
}

#[tokio::test]
async fn terminal_transition_guard() {
    let (reg, _doc) = test_registry().await;
    let reg = ChatRegistry::new(reg);
    reg.register("s1", "m1", None, "/", None).await.unwrap();
    reg.transition("s1", ChatState::Ended).await.unwrap();
    // 终态不可逆（防御：不覆盖）。
    reg.transition("s1", ChatState::Gap).await.unwrap();
    assert_eq!(reg.entry("s1").await.unwrap().state, ChatState::Ended);
}

#[tokio::test]
async fn active_turn_tracking() {
    let (reg, _doc) = test_registry().await;
    let reg = ChatRegistry::new(reg);
    reg.register("s1", "m1", None, "/", None).await.unwrap();
    reg.set_active_turn("s1", "t1").await;
    assert_eq!(reg.active_turn("s1").await.as_deref(), Some("t1"));
    reg.clear_active_turn("s1").await;
    assert_eq!(reg.active_turn("s1").await, None);
}

/// #3 增量窗口计时（issue #3）：touch_active_turn 续命 / active_turn_idle
/// 空闲时长语义；无登记表项 → None（调用方按「无活动窗口」处理）。
#[tokio::test]
async fn active_turn_touch_and_idle() {
    let (reg, _doc) = test_registry().await;
    let reg = ChatRegistry::new(reg);
    // 无登记 → idle None；touch 无登记表项 → 幂等无害。
    assert_eq!(reg.active_turn_idle("s1").await, None);
    reg.touch_active_turn("s1").await;
    assert_eq!(reg.active_turn_idle("s1").await, None);
    // 登记 → idle 为微小正时长（远小于 1s）。
    reg.set_active_turn("s1", "t1").await;
    let idle = reg.active_turn_idle("s1").await.expect("登记后有 idle");
    assert!(
        idle < Duration::from_secs(1),
        "刚登记 idle 应微小（got {idle:?}）"
    );
    // touch 续命：sleep 30ms 后 touch → idle 重置回微小值（增量窗口续期）。
    tokio::time::sleep(Duration::from_millis(30)).await;
    reg.touch_active_turn("s1").await;
    let idle2 = reg.active_turn_idle("s1").await.expect("touch 后仍有 idle");
    assert!(
        idle2 < Duration::from_millis(30),
        "touch 重置 idle 时钟（got {idle2:?}）"
    );
    // 语义保留：turn_id 查询/清除不受影响。
    assert_eq!(reg.active_turn("s1").await.as_deref(), Some("t1"));
    reg.clear_active_turn("s1").await;
    assert_eq!(reg.active_turn("s1").await, None);
    assert_eq!(reg.active_turn_idle("s1").await, None);
}

#[tokio::test]
async fn chats_for_instance() {
    let (reg, _doc) = test_registry().await;
    let reg = ChatRegistry::new(reg);
    reg.register("s1", "m1", None, "/", None).await.unwrap();
    reg.register("s2", "m2", None, "/", None).await.unwrap();
    let list = reg.chats_for_instance("m1").await;
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].0, "s1");
}

/// 状态机全路径（§7.3）：crashed（进程崩溃）、gap（分区）→ 恢复可用
/// （补推追平后清除，迁移回非 gap 状态）。
#[tokio::test]
async fn transition_crashed_and_gap_recover() {
    let (reg, _doc) = test_registry().await;
    let reg = ChatRegistry::new(reg);
    // crashed：进程崩溃（终态，视图保留）。
    reg.register("s1", "m1", None, "/", None).await.unwrap();
    reg.transition("s1", ChatState::Crashed).await.unwrap();
    assert_eq!(reg.entry("s1").await.unwrap().state, ChatState::Crashed);
    assert!(reg.entry("s1").await.unwrap().state.is_terminal());
    // ended：ACP 进程退出。
    reg.register("s2", "m1", None, "/", None).await.unwrap();
    reg.transition("s2", ChatState::Ended).await.unwrap();
    assert_eq!(reg.entry("s2").await.unwrap().state, ChatState::Ended);
    // gap：instance 分区 → 补推追平 → 恢复可用（§7.3「Gap 清除 → 恢复
    // 可用、可开新 turn」）。
    reg.register("s3", "m1", None, "/", None).await.unwrap();
    reg.transition("s3", ChatState::Gap).await.unwrap();
    assert_eq!(reg.entry("s3").await.unwrap().state, ChatState::Gap);
    reg.transition("s3", ChatState::Accepting).await.unwrap();
    assert_eq!(reg.entry("s3").await.unwrap().state, ChatState::Accepting);
}

/// 错误路径：未知 chat 的迁移/离线 close → NotFound（§7.3）。
#[tokio::test]
async fn transition_unknown_chat_not_found() {
    let (reg, _doc) = test_registry().await;
    let reg = ChatRegistry::new(reg);
    assert!(matches!(
        reg.transition("ghost", ChatState::Closed).await,
        Err(ChatError::NotFound(_))
    ));
    assert!(matches!(
        reg.request_close_offline("ghost").await,
        Err(ChatError::NotFound(_))
    ));
}
