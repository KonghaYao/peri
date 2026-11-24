//! P1-A 回归：父会话整树删除后，冷恢复创建的子实例必须离开宿主与 SessionManager。
//!
//! 恢复协调器为 `working` 的 child 冷装配独立会话，客户端只与父会话绑定；父树
//! `session/delete` 成功后持久行已整树删除，请求级 close 会因 `NotFound` 在清理
//! 本地状态前早退。观察面取宿主 `sessions` 与 `SessionManager` 登记。

use super::recovery_child::parent_notices;
use crate::host::requests::tests::recovery_tests::RecoveryHarness;
use crate::transport::types::AcpError;

use peri_acp_types::recovery::SessionRecovery;
use peri_acp_types::thread::{AgentStatus, WorkState};
use serde_json::{json, Value};
use std::sync::Arc;

/// 生产同款 `session/delete`：按客户端绑定的会话 id 删除整树。
async fn delete_session(harness: &RecoveryHarness, session_id: &str) -> Result<Value, AcpError> {
    let mut states = harness.sessions.lock().await;
    crate::host::requests::session_lifecycle::handle_delete(
        &json!({ "sessionId": session_id }),
        &harness.cfg,
        &mut states,
    )
    .await
}

/// 生产同款 `session/close`：按客户端绑定的会话 id 关闭。
async fn close_session(harness: &RecoveryHarness, session_id: &str) -> Result<Value, AcpError> {
    let mut states = harness.sessions.lock().await;
    crate::host::requests::session_lifecycle::handle_close(
        &json!({ "sessionId": session_id }),
        &harness.cfg,
        &mut states,
    )
    .await
}

/// 同一宿主上的另一棵根树：根会话经生产 `session/new` 建立。
async fn new_root_session(harness: &RecoveryHarness) -> String {
    let cwd = harness.cwd.clone();
    let mut states = harness.sessions.lock().await;
    let created = crate::host::requests::handle_request(
        "session/new",
        &json!({ "cwd": cwd }),
        &harness.cfg,
        &mut states,
        &harness.transport,
    )
    .await
    .expect("session/new 必须成功");
    created["sessionId"].as_str().unwrap().to_owned()
}

/// 冷恢复 + 子结束 + 父树删除成功：恢复创建的子实例不得留在宿主或 manager。
#[tokio::test]
async fn deleting_parent_tree_reclaims_recovered_child_after_turn() {
    let mut harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    harness.set_working("child-b").await;
    harness.spawn_continuation_scheduler();
    let recovery = harness.recovery();

    let report = recovery.enter(&root).await.expect("恢复准备必须成功");
    assert_eq!(report.recovered, vec!["child-b".to_owned()]);
    recovery.launch(&report).await;
    crate::host::requests::tests::recovery_tests::wait_for("child 的恢复 turn 结束", || async {
        harness
            .cfg
            .session_resources
            .load_session_meta(&"child-b".to_owned())
            .await
            .is_ok_and(|meta| meta.agent_status == AgentStatus::Done)
    })
    .await;
    crate::host::requests::tests::recovery_tests::wait_for("结果投递到父 MQ", || async {
        !parent_notices(&harness, &root).await.is_empty()
    })
    .await;
    assert!(
        harness.sessions.lock().await.contains_key("child-b"),
        "前提：恢复子实例仍驻留宿主"
    );
    assert!(
        harness.cfg.session_manager.get_session("child-b").is_some(),
        "前提：恢复子实例仍登记 manager"
    );

    delete_session(&harness, &root)
        .await
        .expect("父树删除必须成功");

    assert!(
        !harness.sessions.lock().await.contains_key("child-b"),
        "父树删除后恢复子实例必须离开宿主"
    );
    assert!(
        harness.cfg.session_manager.get_session("child-b").is_none(),
        "父树删除后恢复子登记必须移除"
    );
    assert!(
        harness
            .cfg
            .session_manager
            .recovered_children_of_tree(&root)
            .is_empty(),
        "该树的恢复登记必须排空"
    );
    assert!(
        harness
            .cfg
            .session_resources
            .load_session_meta(&"child-b".to_owned())
            .await
            .is_err(),
        "持久树已整树删除"
    );
}

/// 他树不受影响：删除一棵树只回收该树根下由恢复创建的 child。
#[tokio::test]
async fn deleting_one_tree_keeps_another_trees_recovered_child() {
    let harness = RecoveryHarness::new().await;
    let root_a = harness.thread_id.clone();
    harness.add_child(&root_a, "child-a", &root_a).await;
    harness.set_working("child-a").await;
    let root_b = new_root_session(&harness).await;
    harness.add_child(&root_b, "child-b", &root_b).await;
    harness.set_working("child-b").await;

    let recovery = harness.recovery();
    recovery
        .enter(&root_a)
        .await
        .expect("树 A 恢复准备必须成功");
    recovery
        .enter(&root_b)
        .await
        .expect("树 B 恢复准备必须成功");
    assert!(harness.sessions.lock().await.contains_key("child-a"));
    assert!(harness.sessions.lock().await.contains_key("child-b"));

    delete_session(&harness, &root_a)
        .await
        .expect("树 A 删除必须成功");

    assert!(
        !harness.sessions.lock().await.contains_key("child-a"),
        "树 A 的恢复子实例必须回收"
    );
    assert!(
        harness.sessions.lock().await.contains_key("child-b"),
        "树 B 的恢复子实例不得受树 A 删除影响"
    );
    assert!(
        harness.cfg.session_manager.get_session("child-b").is_some(),
        "树 B 的恢复子登记不得受树 A 删除影响"
    );
    assert_eq!(
        harness
            .cfg
            .session_manager
            .recovered_children_of_tree(&root_a)
            .len(),
        0,
        "树 A 的恢复登记必须排空"
    );
    assert_eq!(
        harness
            .cfg
            .session_manager
            .recovered_children_of_tree(&root_b)
            .len(),
        1,
        "树 B 的恢复登记必须保留"
    );
}

/// 代次推进后不再属于本次恢复记录：父树删除不得拆掉新代次实例。
#[tokio::test]
async fn advanced_generation_survives_parent_tree_delete() {
    let harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    harness.set_working("child-b").await;
    let recovery = harness.recovery();
    recovery.enter(&root).await.expect("恢复准备必须成功");
    {
        // 显式 load / rewind 等路径会推进 live 代次：记录不再代表当前实例。
        let mut states = harness.sessions.lock().await;
        states
            .get_mut("child-b")
            .expect("冷装配已发布 child")
            .refresh_session_generation();
    }

    delete_session(&harness, &root)
        .await
        .expect("父树删除必须成功");

    assert!(
        harness.sessions.lock().await.contains_key("child-b"),
        "新代次实例不属于本次恢复记录"
    );
    assert!(
        harness.cfg.session_manager.get_session("child-b").is_some(),
        "新代次登记不得被删除"
    );
}

/// 删除失败不误回收：父树删除在整树删除之前失败时，恢复子实例保持原样。
#[tokio::test]
async fn failed_parent_tree_delete_keeps_recovered_child() {
    let harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    harness.set_working("child-b").await;
    let recovery = harness.recovery();
    recovery.enter(&root).await.expect("恢复准备必须成功");
    {
        // 根上仍有在飞 turn：close 在整树删除之前以「prompt is still active」失败。
        let mut states = harness.sessions.lock().await;
        states.get_mut(&root).unwrap().cancel_token =
            Some(tokio_util::sync::CancellationToken::new());
    }

    let error = delete_session(&harness, &root)
        .await
        .expect_err("在飞 prompt 下删除必须失败");
    assert!(
        error.message.contains("prompt is still active"),
        "{error:?}"
    );

    assert!(
        harness.sessions.lock().await.contains_key("child-b"),
        "删除失败不得回收子树"
    );
    assert!(
        harness.cfg.session_manager.get_session("child-b").is_some(),
        "删除失败不得移除子登记"
    );
    assert_eq!(
        harness
            .cfg
            .session_manager
            .recovered_children_of_tree(&root)
            .len(),
        1,
        "删除失败必须保留恢复登记供重试"
    );
    assert_eq!(
        harness
            .cfg
            .session_resources
            .load_session_meta(&"child-b".to_owned())
            .await
            .unwrap()
            .work_state,
        WorkState::Working,
        "删除失败不得改写 child 的工作意图"
    );
}

/// 宿主 shutdown 兜底：删除失败留下的恢复子实例仍由整连接收口清理。
///
/// 父树删除失败（或回收未能完全结算）时子实例保留；连接正常 shutdown 必须仍能
/// 排空它——这是回收路径失败时的最后释放点。
#[tokio::test]
async fn host_shutdown_still_clears_retained_recovered_child() {
    use crate::host::connection::ConnectionContext;
    use crate::host::task_scope::HostTaskOwner;
    use peri_acp_types::ports::McpTaskOwnerPort;

    let harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    harness.set_working("child-b").await;
    let recovery = harness.recovery();
    recovery.enter(&root).await.expect("恢复准备必须成功");
    {
        let mut states = harness.sessions.lock().await;
        states.get_mut(&root).unwrap().cancel_token =
            Some(tokio_util::sync::CancellationToken::new());
    }
    delete_session(&harness, &root)
        .await
        .expect_err("在飞 prompt 下删除必须失败");
    assert!(harness.sessions.lock().await.contains_key("child-b"));

    let (mut task_owner, _task_spawner) = HostTaskOwner::new();
    let (mcp_owner, _mcp_spawner) = peri_middlewares::mcp::McpTaskOwner::new();
    let mut mcp_owner: Box<dyn McpTaskOwnerPort> = Box::new(mcp_owner);
    let mut cont_tx = Some(Arc::clone(&harness.sender));
    let connection = Arc::new(tokio::sync::Mutex::new(ConnectionContext::new(false)));
    let cancellation = tokio_util::sync::CancellationToken::new();
    let mut closing_sessions = std::collections::BTreeMap::new();
    let progress = crate::host::shutdown::ShutdownProgress::new();
    let report = crate::host::shutdown::shutdown_host(
        &mut task_owner,
        mcp_owner.as_mut(),
        &harness.cfg,
        &harness.sessions,
        &harness.locks,
        &mut cont_tx,
        &connection,
        &cancellation,
        &mut closing_sessions,
        &progress,
    )
    .await;

    assert!(
        matches!(
            report,
            crate::host::task_scope::HostTerminalShutdownReport::Complete { .. }
        ),
        "宿主 shutdown 必须收敛：{report:?}"
    );
    assert!(
        harness.sessions.lock().await.is_empty(),
        "宿主 shutdown 必须清空残留的恢复子实例"
    );
    assert!(
        harness.cfg.session_manager.session_ids().is_empty(),
        "宿主 shutdown 必须移除恢复子登记"
    );
}

/// 恢复登记晚于父树删除快照：删除看不到该登记，恢复方复查持久树后自行回收。
#[tokio::test]
async fn child_registered_after_parent_tree_delete_snapshot_is_reclaimed() {
    use crate::host::recovery::admission_gate::{self, PausePoint};
    use peri_acp_types::thread::ThreadId;

    let harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-race", &root).await;
    harness.set_working("child-race").await;
    let recovery = Arc::new(harness.recovery());
    let gate = admission_gate::install("child-race", PausePoint::BeforeRecordRecoveredChild);

    let entering = {
        let recovery = Arc::clone(&recovery);
        let root = ThreadId::from(root.clone());
        tokio::spawn(async move { recovery.enter(&root).await })
    };
    gate.wait_arrived(1).await;

    delete_session(&harness, &root)
        .await
        .expect("父树删除必须成功");
    assert!(
        harness.sessions.lock().await.contains_key("child-race"),
        "前提：删除快照早于恢复登记，child 暂未被回收"
    );

    gate.release(1);
    entering
        .await
        .expect("进入任务不得 panic")
        .expect("准备本身不是请求错误");

    assert!(
        !harness.sessions.lock().await.contains_key("child-race"),
        "晚到的恢复登记必须由恢复方自行回收"
    );
    assert!(
        harness
            .cfg
            .session_manager
            .get_session("child-race")
            .is_none(),
        "晚到的恢复登记必须从 manager 移除"
    );
    assert!(
        harness
            .cfg
            .session_manager
            .recovered_children_of_tree(&root)
            .is_empty(),
        "该树的恢复登记必须排空"
    );
    admission_gate::uninstall("child-race", PausePoint::BeforeRecordRecoveredChild);
}

/// 持久行已删除、宿主仍保留 closing 的收口对象：重试 close 必须完成全部结算。
#[tokio::test]
async fn close_after_metadata_gone_settles_retained_child() {
    let harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-retained", &root).await;
    harness.set_working("child-retained").await;
    let recovery = harness.recovery();
    recovery.enter(&root).await.expect("恢复准备必须成功");
    {
        let mut states = harness.sessions.lock().await;
        let state = states
            .get_mut("child-retained")
            .expect("冷装配已发布 child");
        state.closing = true;
    }
    harness
        .cfg
        .session_resources
        .delete_session_tree(&"child-retained".to_owned())
        .await
        .expect("持久行删除必须成功");

    close_session(&harness, "child-retained")
        .await
        .expect("持久行缺失时关闭必须收口");

    assert!(
        !harness.sessions.lock().await.contains_key("child-retained"),
        "关闭成功后宿主状态必须移除"
    );
    assert!(
        harness
            .cfg
            .session_manager
            .get_session("child-retained")
            .is_none(),
        "关闭成功后 manager 会话必须移除"
    );
    assert!(
        harness
            .cfg
            .session_manager
            .recovered_children_of_tree(&root)
            .is_empty(),
        "关闭成功后恢复登记必须排空"
    );
}
