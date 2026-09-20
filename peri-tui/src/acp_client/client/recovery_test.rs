//! dirty 恢复确认的客户端契约：精确 target/generation、取消无写入、身份固定、fail closed。

use super::*;
use crate::kit::atoms::{self, CONFIRM_PAYLOAD, POPUP_KIND, PopupKind};
use peri_acp::transport::{
    AcpTransport,
    mpsc::{MpscServerTransport, mpsc_transport_pair},
    types::{IncomingMessage, RequestId},
};
use peri_acp_types::workspace::{ReadOnlyAdmission, RecoveryRequiredDetails};
use serde_json::Value;
use std::time::Duration;

const TARGET: &str = "target-thread";
const EFFECTIVE_CWD: &str = "/worktrees/feature/src";

/// 会话过渡（`project_session_boundary` / `project_execution_cwd`）会写多个全局
/// atom。测试必须整体保存并在结束（含 panic）时恢复，否则并行 lib 测试互相污染；
/// 只清 popup 两个 atom 不够。
struct UiAtomsGuard(Vec<Box<dyn FnOnce()>>);

impl UiAtomsGuard {
    fn capture() -> Self {
        use crate::kit::atoms;
        let mut restores: Vec<Box<dyn FnOnce()>> = Vec::new();
        macro_rules! save_atom {
            ($atom:expr) => {{
                let saved = $atom.state().read().clone();
                restores.push(Box::new(move || *$atom.state().write() = saved));
            }};
        }
        save_atom!(atoms::ACTIVE_SESSION_ID);
        save_atom!(atoms::ACTIVE_EXECUTION_CWD);
        save_atom!(atoms::SESSION_READ_ONLY);
        save_atom!(atoms::SERVICE_SNAPSHOT);
        save_atom!(atoms::BRIDGE_RESET_COUNTER);
        save_atom!(atoms::VIEW_MODELS);
        save_atom!(atoms::ACP_STATE);
        save_atom!(atoms::INPUT_BUFFER);
        save_atom!(atoms::HITL_PENDING);
        save_atom!(atoms::ASK_USER_PENDING);
        save_atom!(atoms::OAUTH_INFO);
        save_atom!(atoms::OAUTH_SESSION_ID);
        save_atom!(atoms::OPEN_PANELS);
        save_atom!(atoms::ACTIVE_PANEL);
        save_atom!(atoms::POPUP_KIND);
        save_atom!(atoms::CONFIRM_PAYLOAD);
        save_atom!(atoms::REWIND_PREVIEW);
        save_atom!(atoms::REWIND_TARGET_TEXT);
        save_atom!(atoms::REWIND_PREVIEW_FINGERPRINT);
        save_atom!(atoms::REWIND_BUDGET_STATE);
        save_atom!(atoms::REWIND_QUERY_ERROR);
        save_atom!(atoms::TODO_ITEMS);
        save_atom!(atoms::GOAL_SNAPSHOT);
        save_atom!(atoms::INPUT_HISTORY_INDEX);
        save_atom!(atoms::DRAFT);
        save_atom!(atoms::FOCUSED_ENTRY);
        save_atom!(atoms::FOLD_OVERRIDES);
        save_atom!(crate::kit::steer_state::STEERS);
        Self(restores)
    }
}

impl Drop for UiAtomsGuard {
    fn drop(&mut self) {
        for restore in self.0.drain(..) {
            restore();
        }
    }
}

fn interactive_client() -> (AcpTuiClient, MpscServerTransport) {
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new_interactive(transport);
    client
        .session_workspace
        .store(true, std::sync::atomic::Ordering::Release);
    client
        .session_recovery
        .store(true, std::sync::atomic::Ordering::Release);
    (client, server)
}

fn context(cwd: &str) -> Value {
    json!({"version":1,"workspace":{
        "project_id":"00000000-0000-0000-0000-000000000001",
        "workspace_id":"00000000-0000-0000-0000-000000000002",
        "cwd":cwd,"root":"/worktrees/feature","relative_cwd":"src"
    },"binding":{
        "schema_version":1,"revision":1,
        "project_id":"00000000-0000-0000-0000-000000000001",
        "workspace_id":"00000000-0000-0000-0000-000000000002",
        "cwd_relative_to_workspace":"src"
    }})
}

fn recovery_error(thread_id: &str, generation: i64) -> AcpError {
    AcpError::new(
        -32010,
        "previous session execution did not close cleanly; recovery is required",
    )
    .with_data(json!({
        "kind": "peri.recoveryRequiredV1",
        "details": {"thread_id": thread_id, "generation": generation},
    }))
}

async fn next_request(server: &MpscServerTransport) -> (RequestId, String, Value) {
    let msg = tokio::time::timeout(Duration::from_secs(5), server.recv())
        .await
        .expect("expected a client request")
        .unwrap();
    let IncomingMessage::Request { id, method, params } = msg else {
        panic!("expected request")
    };
    (id, method, params)
}

/// 断言在给定窗口内客户端没有发出任何新请求。
async fn assert_no_request(server: &MpscServerTransport, window: Duration) {
    match tokio::time::timeout(window, server.recv()).await {
        Err(_) => {}
        Ok(message) => {
            panic!("unexpected client write while awaiting the risk decision: {message:?}")
        }
    }
}

/// 协商到 `session/load` 首次失败（dirty）为止。
async fn reach_dirty_load(server: &MpscServerTransport, error: AcpError) -> Value {
    reach_load(server, Err(error)).await
}

/// 协商到 `session/load` 请求按其结果回答为止，返回 load 的请求参数。
async fn reach_load(server: &MpscServerTransport, response: Result<Value, AcpError>) -> Value {
    let (id, method, _) = next_request(server).await;
    assert_eq!(method, "peri/session_context");
    server
        .send_response(id, Ok(context(EFFECTIVE_CWD)))
        .await
        .unwrap();
    let (id, method, params) = next_request(server).await;
    assert_eq!(method, "session/load");
    server.send_response(id, response).await.unwrap();
    params
}

/// `session/load` 的只读准入响应：历史可读，但本次准入没有执行所有权。
fn read_only_response(admission: &ReadOnlyAdmission) -> Value {
    json!({"_meta": {"peri.sessionWorkspaceV1": {"read_only": admission}}})
}

async fn wait_for_recovery_owner()
-> std::sync::Arc<crate::kit::popups::confirm_popup::RecoveryConfirmation> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(atoms::ConfirmPayload {
                pending_action: atoms::ConfirmAction::RecoverDirty(owner),
                ..
            }) = CONFIRM_PAYLOAD.state().read().as_ref()
            {
                return owner.clone();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("dirty load must offer an explicit risk decision")
}

/// 接受风险后才写库：reset 携带精确 target 与显式字段，随后按原 ID/原目录恢复。
#[tokio::test]
#[serial_test::serial]
async fn test_dirty_load_accept_resets_exact_generation_then_loads_original_session() {
    let _guard = UiAtomsGuard::capture();
    let (client, server) = interactive_client();
    let loader = client.clone();
    let load =
        tokio::spawn(async move { loader.load_session(TARGET, "/startup", Some("m")).await });
    reach_dirty_load(&server, recovery_error(TARGET, 4)).await;
    let owner = wait_for_recovery_owner().await;
    assert_eq!(
        owner.target,
        RecoveryRequiredDetails {
            thread_id: TARGET.to_string(),
            generation: 4
        }
    );
    owner.mark_displayed();
    owner.answer(true);

    let (id, method, params) = next_request(&server).await;
    assert_eq!(method, "peri/session_reset_dirty");
    assert_eq!(
        params,
        json!({"target":{"thread_id":TARGET,"generation":4},"accept_risk":true})
    );
    server.send_response(id, Ok(json!({}))).await.unwrap();

    let (id, method, params) = next_request(&server).await;
    assert_eq!(method, "session/load");
    assert_eq!(params["sessionId"], TARGET);
    assert_eq!(params["cwd"], EFFECTIVE_CWD);
    assert_eq!(params["model"], "m");
    server.send_response(id, Ok(json!({}))).await.unwrap();

    assert_eq!(load.await.unwrap().unwrap(), TARGET);
    assert_eq!(client.current_session_id().as_deref(), Some(TARGET));
    assert_eq!(
        client.current_execution_cwd().as_deref(),
        Some(EFFECTIVE_CWD)
    );
    assert!(client.check_restore_error().is_ok());
    assert!(CONFIRM_PAYLOAD.state().read().is_none());
    assert!(*POPUP_KIND.state().read() != Some(PopupKind::Confirm));
}

/// 取消（默认路径）不得写库，失败保持可见且不提交会话。
#[tokio::test]
#[serial_test::serial]
async fn test_dirty_load_cancel_sends_no_reset_and_keeps_session_blocked() {
    let _guard = UiAtomsGuard::capture();
    let (client, server) = interactive_client();
    let loader = client.clone();
    let load =
        tokio::spawn(async move { loader.load_session(TARGET, "/startup", Some("m")).await });
    reach_dirty_load(&server, recovery_error(TARGET, 2)).await;
    wait_for_recovery_owner().await.answer(false);
    let error = load.await.unwrap().unwrap_err();
    assert!(error.message.contains("recovery is required"));
    assert_no_request(&server, Duration::from_millis(100)).await;
    assert_eq!(client.current_session_id(), None);
    assert!(client.current_execution_cwd().is_none());
    assert!(client.check_restore_error().is_err());
    assert!(CONFIRM_PAYLOAD.state().read().is_none());
    assert!(*POPUP_KIND.state().read() != Some(PopupKind::Confirm));
}

/// reset 被拒（例如旧子进程仍持有 OS 锁）不得静默放行，也不回落成 clean。
#[tokio::test]
#[serial_test::serial]
async fn test_dirty_load_reset_rejection_is_surfaced_without_retry() {
    let _guard = UiAtomsGuard::capture();
    let (client, server) = interactive_client();
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session(TARGET, "/startup", None).await });
    reach_dirty_load(&server, recovery_error(TARGET, 1)).await;
    let owner = wait_for_recovery_owner().await;
    owner.mark_displayed();
    owner.answer(true);
    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "peri/session_reset_dirty");
    server
        .send_response(
            id,
            Err(AcpError::new(
                -32010,
                "session is owned by another execution host",
            )),
        )
        .await
        .unwrap();
    let error = load.await.unwrap().unwrap_err();
    assert!(error.message.contains("another execution host"));
    assert_no_request(&server, Duration::from_millis(100)).await;
    assert_eq!(client.current_session_id(), None);
}

/// 错误不是本会话的 dirty 详情时不得提示，也不得发 reset。
#[tokio::test]
#[serial_test::serial]
async fn test_dirty_load_other_errors_and_foreign_targets_never_offer_confirmation() {
    let _guard = UiAtomsGuard::capture();
    let (client, server) = interactive_client();
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session(TARGET, "/startup", None).await });
    reach_dirty_load(&server, recovery_error("another-thread", 1)).await;
    let error = load.await.unwrap().unwrap_err();
    assert_eq!(error.code, -32010);
    assert!(CONFIRM_PAYLOAD.state().read().is_none());
    assert!(*POPUP_KIND.state().read() != Some(PopupKind::Confirm));
    assert_no_request(&server, Duration::from_millis(100)).await;

    drop(client);
    *POPUP_KIND.state().write() = None;
    *CONFIRM_PAYLOAD.state().write() = None;
    let (client, server) = interactive_client();
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session(TARGET, "/startup", None).await });
    reach_dirty_load(
        &server,
        AcpError::new(-32010, "session is owned by another execution host"),
    )
    .await;
    let error = load.await.unwrap().unwrap_err();
    assert!(error.message.contains("another execution host"));
    assert!(CONFIRM_PAYLOAD.state().read().is_none());
    assert_no_request(&server, Duration::from_millis(100)).await;
}

/// 未协商 `peri.sessionRecoveryV1`：不能展示确认时 fail closed。
#[tokio::test]
#[serial_test::serial]
async fn test_dirty_load_without_negotiated_capability_never_offers_confirmation() {
    let _guard = UiAtomsGuard::capture();
    let (client, server) = interactive_client();
    client
        .session_recovery
        .store(false, std::sync::atomic::Ordering::Release);
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session(TARGET, "/startup", None).await });
    reach_dirty_load(&server, recovery_error(TARGET, 1)).await;
    let error = load.await.unwrap().unwrap_err();
    assert!(error.message.contains("recovery is required"));
    assert!(CONFIRM_PAYLOAD.state().read().is_none());
    assert_no_request(&server, Duration::from_millis(100)).await;
}

/// 非交互宿主（无法展示确认）：不提示、不写库。
#[tokio::test]
#[serial_test::serial]
async fn test_headless_dirty_load_never_offers_confirmation() {
    let _guard = UiAtomsGuard::capture();
    let (transport, server) = mpsc_transport_pair();
    let (headless, _, _) = AcpTuiClient::new(transport);
    headless
        .session_workspace
        .store(true, std::sync::atomic::Ordering::Release);
    headless
        .session_recovery
        .store(true, std::sync::atomic::Ordering::Release);
    let loader = headless.clone();
    let load = tokio::spawn(async move { loader.load_session(TARGET, "/startup", None).await });
    reach_dirty_load(&server, recovery_error(TARGET, 1)).await;
    assert!(load.await.unwrap().is_err());
    assert!(CONFIRM_PAYLOAD.state().read().is_none());
    assert_no_request(&server, Duration::from_millis(100)).await;
}

/// 确认等待期间：会话切换/输入必须仍然被 reference-count reservation 挡住。
#[tokio::test]
#[serial_test::serial]
async fn test_dirty_load_confirmation_blocks_session_switch_and_input() {
    let _guard = UiAtomsGuard::capture();
    let (client, server) = interactive_client();
    let reservation = client.reserve_session_load();
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session(TARGET, "/startup", None).await });
    reach_dirty_load(&server, recovery_error(TARGET, 1)).await;
    let owner = wait_for_recovery_owner().await;

    let ensure = {
        let client = client.clone();
        tokio::spawn(async move { client.ensure_session("/startup", None).await })
    };
    let prompt = {
        let client = client.clone();
        tokio::spawn(async move {
            client
                .prompt(&peri_acp_types::messages::MessageContent::text("hi"), None)
                .await
        })
    };
    assert_no_request(&server, Duration::from_millis(100)).await;
    assert!(!ensure.is_finished());
    assert!(!prompt.is_finished());

    owner.answer(false);
    assert!(load.await.unwrap().is_err());
    drop(reservation);
    assert!(prompt.await.unwrap().is_err());
    ensure.abort();
    // abort 与自然返回（恢复错误）都可能发生，两者都不能产生额外请求。
    if let Ok(result) = ensure.await {
        assert!(result.is_err());
    }
    assert_no_request(&server, Duration::from_millis(100)).await;
}

/// 首帧之前弹窗被替换/撤销：旧 dirty 确认必须精确结清，load、gate、reservation 全部释放。
#[tokio::test]
#[serial_test::serial]
async fn test_dirty_load_confirmation_revoked_by_popup_replacement_releases_load() {
    let _guard = UiAtomsGuard::capture();
    let (client, server) = interactive_client();
    let reservation = client.reserve_session_load();
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session(TARGET, "/startup", None).await });
    reach_dirty_load(&server, recovery_error(TARGET, 5)).await;
    let owner = wait_for_recovery_owner().await;

    // 用户/其他链路在确认渲染前打开了另一个 popup：新 popup 保留，旧确认按取消收敛。
    crate::kit::popup_overlay::open_popup(PopupKind::OAuth);
    assert_eq!(*POPUP_KIND.state().read(), Some(PopupKind::OAuth));
    assert!(CONFIRM_PAYLOAD.state().read().is_none());
    let error = tokio::time::timeout(Duration::from_secs(5), load)
        .await
        .expect("revoked confirmation must not hold the load forever")
        .unwrap()
        .unwrap_err();
    assert!(error.message.contains("recovery is required"));
    assert_no_request(&server, Duration::from_millis(100)).await;

    // 撤销路径同样不写库，等待方按取消结束（不再持有响应通道）。
    owner.answer(true);
    assert_eq!(client.pending_session_load_count(), 1);
    drop(reservation);
    assert_eq!(client.pending_session_load_count(), 0);
    crate::kit::popup_overlay::close_popup();
    assert!(*POPUP_KIND.state().read() != Some(PopupKind::Confirm));
    assert!(CONFIRM_PAYLOAD.state().read().is_none());

    // gate 已释放：后续 load 能重新进入并发出自己的请求。
    let retry = client.clone();
    let retry =
        tokio::spawn(async move { retry.load_session("retry-thread", "/startup", None).await });
    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "peri/session_context");
    server
        .send_response(id, Ok(context(EFFECTIVE_CWD)))
        .await
        .unwrap();
    let (id, method, params) = next_request(&server).await;
    assert_eq!(method, "session/load");
    assert_eq!(params["sessionId"], "retry-thread");
    server.send_response(id, Ok(json!({}))).await.unwrap();
    assert_eq!(retry.await.unwrap().unwrap(), "retry-thread");
}

/// reset 成功但重新 load 失败：错误保持可见、阻止自动新建，重试可用同一 ID 恢复。
#[tokio::test]
#[serial_test::serial]
async fn test_dirty_load_reload_failure_blocks_new_session_and_retry_recovers() {
    let _guard = UiAtomsGuard::capture();
    let (client, server) = interactive_client();
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session(TARGET, "/startup", None).await });
    reach_dirty_load(&server, recovery_error(TARGET, 1)).await;
    let owner = wait_for_recovery_owner().await;
    owner.mark_displayed();
    owner.answer(true);
    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "peri/session_reset_dirty");
    server.send_response(id, Ok(json!({}))).await.unwrap();
    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "session/load");
    server
        .send_response(
            id,
            Err(AcpError::new(
                -32010,
                "session is owned by another execution host",
            )),
        )
        .await
        .unwrap();
    let error = load.await.unwrap().unwrap_err();
    assert!(error.message.contains("another execution host"));
    assert!(client.check_restore_error().is_err());
    assert_eq!(client.current_session_id(), None);

    // 恢复失败期间不得无声新建会话：ensure 直接返回恢复错误，且不发出 session/new。
    let ensure_client = client.clone();
    let ensure = tokio::spawn(async move { ensure_client.ensure_session("/startup", None).await });
    assert_no_request(&server, Duration::from_millis(100)).await;
    assert!(ensure.await.unwrap().is_err());

    // 重试：同一 ID 正常恢复，不再出现风险确认。
    let retry = client.clone();
    let retry = tokio::spawn(async move { retry.load_session(TARGET, "/startup", None).await });
    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "peri/session_context");
    server
        .send_response(id, Ok(context(EFFECTIVE_CWD)))
        .await
        .unwrap();
    let (id, method, params) = next_request(&server).await;
    assert_eq!(method, "session/load");
    assert_eq!(params["sessionId"], TARGET);
    server.send_response(id, Ok(json!({}))).await.unwrap();
    assert_eq!(retry.await.unwrap().unwrap(), TARGET);
    assert!(CONFIRM_PAYLOAD.state().read().is_none());
    assert!(client.check_restore_error().is_ok());
}

/// 确认期间被其他确认载荷取代：等待方按取消收敛，不得吞掉新确认。
#[tokio::test]
#[serial_test::serial]
async fn test_dirty_load_confirmation_superseded_by_other_confirm_cancels() {
    let _guard = UiAtomsGuard::capture();
    let (client, server) = interactive_client();
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session(TARGET, "/startup", None).await });
    reach_dirty_load(&server, recovery_error(TARGET, 1)).await;
    wait_for_recovery_owner().await;

    // thread_load_consumer 的既有路径：先写 payload，再置 Confirm。
    *CONFIRM_PAYLOAD.state().write() = Some(atoms::ConfirmPayload {
        title: "switch".into(),
        message: "bg tasks".into(),
        details: vec![],
        pending_action: atoms::ConfirmAction::ThreadSwitch("other".into()),
    });
    *POPUP_KIND.state().write() = Some(PopupKind::Confirm);
    let error = tokio::time::timeout(Duration::from_secs(5), load)
        .await
        .expect("superseded confirmation must not keep the load alive")
        .unwrap()
        .unwrap_err();
    assert!(error.message.contains("recovery is required"));
    assert_no_request(&server, Duration::from_millis(100)).await;
    // 新确认保留：撤销边界只结清 dirty 载荷。
    assert_eq!(
        CONFIRM_PAYLOAD.state().read().as_ref().unwrap().title,
        "switch"
    );
    crate::kit::popup_overlay::close_popup();
    assert!(CONFIRM_PAYLOAD.state().read().is_none());
}

/// 确认等待期间的第二个 load 必须等待 gate，回答后按自身目标继续。
#[tokio::test]
#[serial_test::serial]
async fn test_dirty_load_second_load_waits_for_decision_then_proceeds() {
    let _guard = UiAtomsGuard::capture();
    let (client, server) = interactive_client();
    let first = client.clone();
    let first = tokio::spawn(async move { first.load_session(TARGET, "/startup", None).await });
    reach_dirty_load(&server, recovery_error(TARGET, 1)).await;
    let owner = wait_for_recovery_owner().await;

    let second = client.clone();
    let second =
        tokio::spawn(async move { second.load_session("second-thread", "/startup", None).await });
    assert_no_request(&server, Duration::from_millis(100)).await;
    assert!(!second.is_finished());

    owner.answer(false);
    assert!(first.await.unwrap().is_err());
    let (id, method, params) = next_request(&server).await;
    assert_eq!(method, "peri/session_context");
    assert_eq!(params["sessionId"], "second-thread");
    server
        .send_response(id, Ok(context(EFFECTIVE_CWD)))
        .await
        .unwrap();
    let (id, method, params) = next_request(&server).await;
    assert_eq!(method, "session/load");
    assert_eq!(params["sessionId"], "second-thread");
    server.send_response(id, Ok(json!({}))).await.unwrap();
    assert_eq!(second.await.unwrap().unwrap(), "second-thread");
}

/// 确认期间取消（shutdown）：等待方被丢弃即按取消收敛，gate 释放且不写库。
#[tokio::test]
#[serial_test::serial]
async fn test_dirty_load_confirmation_cancelled_by_shutdown_releases_gate() {
    let _guard = UiAtomsGuard::capture();
    let (client, server) = interactive_client();
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session(TARGET, "/startup", None).await });
    reach_dirty_load(&server, recovery_error(TARGET, 1)).await;
    let owner = wait_for_recovery_owner().await;

    load.abort();
    assert!(load.await.unwrap_err().is_cancelled());
    assert!(CONFIRM_PAYLOAD.state().read().is_none());
    assert!(*POPUP_KIND.state().read() != Some(PopupKind::Confirm));
    assert_no_request(&server, Duration::from_millis(100)).await;
    owner.answer(true);

    let retry = client.clone();
    let retry = tokio::spawn(async move { retry.load_session(TARGET, "/startup", None).await });
    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "peri/session_context");
    server
        .send_response(id, Ok(context(EFFECTIVE_CWD)))
        .await
        .unwrap();
    let (id, method, params) = next_request(&server).await;
    assert_eq!(method, "session/load");
    assert_eq!(params["sessionId"], TARGET);
    server.send_response(id, Ok(json!({}))).await.unwrap();
    assert_eq!(retry.await.unwrap().unwrap(), TARGET);
}

/// 只读准入不是失败：历史照常进入、状态记下原因，且不发起任何写入尝试。
#[tokio::test]
#[serial_test::serial]
async fn test_read_only_load_enters_session_and_projects_reason() {
    let _guard = UiAtomsGuard::capture();
    let (client, server) = interactive_client();
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session(TARGET, "/startup", None).await });
    reach_load(
        &server,
        Ok(read_only_response(&ReadOnlyAdmission::ExecutionBusy)),
    )
    .await;

    assert_eq!(load.await.unwrap().unwrap(), TARGET);
    assert_eq!(client.current_session_id().as_deref(), Some(TARGET));
    assert_eq!(
        client.current_execution_cwd().as_deref(),
        Some(EFFECTIVE_CWD)
    );
    assert!(
        client.check_restore_error().is_ok(),
        "只读准入不得被记为恢复失败"
    );
    assert_eq!(
        atoms::SESSION_READ_ONLY.state().read().clone(),
        Some(ReadOnlyAdmission::ExecutionBusy)
    );
    assert!(
        CONFIRM_PAYLOAD.state().read().is_none(),
        "执行所有权他处持有不需要风险确认"
    );
    assert_no_request(&server, Duration::from_millis(100)).await;
}

/// 只读的 dirty 准入：接受风险后按精确代际解除，重新 load 取回执行所有权。
#[tokio::test]
#[serial_test::serial]
async fn test_read_only_dirty_load_accept_resets_exact_generation_then_commits_owned() {
    let _guard = UiAtomsGuard::capture();
    let (client, server) = interactive_client();
    let boundary_before = atoms::BRIDGE_RESET_COUNTER.get();
    let target = RecoveryRequiredDetails {
        thread_id: TARGET.to_string(),
        generation: 3,
    };
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session(TARGET, "/startup", None).await });
    reach_load(
        &server,
        Ok(read_only_response(&ReadOnlyAdmission::RecoveryRequired(
            target.clone(),
        ))),
    )
    .await;

    let owner = wait_for_recovery_owner().await;
    assert_eq!(owner.target, target);
    owner.mark_displayed();
    owner.answer(true);

    let (id, method, params) = next_request(&server).await;
    assert_eq!(method, "peri/session_reset_dirty");
    assert_eq!(
        params,
        json!({"target":{"thread_id":TARGET,"generation":3},"accept_risk":true})
    );
    server.send_response(id, Ok(json!({}))).await.unwrap();

    let (id, method, params) = next_request(&server).await;
    assert_eq!(method, "session/load");
    assert_eq!(params["sessionId"], TARGET);
    server.send_response(id, Ok(json!({}))).await.unwrap();

    assert_eq!(load.await.unwrap().unwrap(), TARGET);
    assert!(client.check_restore_error().is_ok());
    assert_eq!(
        *atoms::SESSION_READ_ONLY.state().read(),
        None,
        "取回执行所有权后不得残留只读标记"
    );
    assert_eq!(
        atoms::BRIDGE_RESET_COUNTER.get(),
        boundary_before + 2,
        "两次都会被宿主回放历史的 load 各需要一个回放边界，否则消息区整段重复"
    );
    assert_no_request(&server, Duration::from_millis(100)).await;
}

/// 只读的 dirty 准入被取消：不写库，但历史仍按只读进入——取消不再是拒绝进入。
#[tokio::test]
#[serial_test::serial]
async fn test_read_only_dirty_load_cancel_enters_session_without_reset() {
    let _guard = UiAtomsGuard::capture();
    let (client, server) = interactive_client();
    let boundary_before = atoms::BRIDGE_RESET_COUNTER.get();
    let target = RecoveryRequiredDetails {
        thread_id: TARGET.to_string(),
        generation: 2,
    };
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session(TARGET, "/startup", None).await });
    reach_load(
        &server,
        Ok(read_only_response(&ReadOnlyAdmission::RecoveryRequired(
            target.clone(),
        ))),
    )
    .await;
    wait_for_recovery_owner().await.answer(false);

    assert_eq!(load.await.unwrap().unwrap(), TARGET);
    assert_eq!(client.current_session_id().as_deref(), Some(TARGET));
    assert!(client.check_restore_error().is_ok());
    assert_eq!(
        atoms::SESSION_READ_ONLY.state().read().clone(),
        Some(ReadOnlyAdmission::RecoveryRequired(target))
    );
    assert!(CONFIRM_PAYLOAD.state().read().is_none());
    assert_eq!(
        atoms::BRIDGE_RESET_COUNTER.get(),
        boundary_before + 1,
        "取消不发生第二次回放，也就没有第二个回放边界"
    );
    assert_no_request(&server, Duration::from_millis(100)).await;
}

/// 取回所有权失败不得把已准入的只读会话整体丢掉：首次准入仍然有效。
///
/// reset 与第二次 load 都已失败时，会话必须保持可用（历史重新取一次、状态栏保留只读
/// 原因），而不是清空视图 + 置空会话 + 记一条恢复错误——那比不点「接受」更差。
#[tokio::test]
#[serial_test::serial]
async fn test_read_only_dirty_accept_reload_failure_keeps_read_only_admission() {
    let _guard = UiAtomsGuard::capture();
    let (client, server) = interactive_client();
    let boundary_before = atoms::BRIDGE_RESET_COUNTER.get();
    let target = RecoveryRequiredDetails {
        thread_id: TARGET.to_string(),
        generation: 5,
    };
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session(TARGET, "/startup", None).await });
    reach_load(
        &server,
        Ok(read_only_response(&ReadOnlyAdmission::RecoveryRequired(
            target.clone(),
        ))),
    )
    .await;
    let owner = wait_for_recovery_owner().await;
    owner.mark_displayed();
    owner.answer(true);

    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "peri/session_reset_dirty");
    server.send_response(id, Ok(json!({}))).await.unwrap();
    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "session/load");
    server
        .send_response(
            id,
            Err(AcpError::new(
                -32010,
                "session is owned by another execution host",
            )),
        )
        .await
        .unwrap();
    // 边界已经投影过、视图已清空：按首次的只读准入再取一次历史。
    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "session/load");
    server
        .send_response(
            id,
            Ok(read_only_response(&ReadOnlyAdmission::RecoveryRequired(
                target.clone(),
            ))),
        )
        .await
        .unwrap();

    assert_eq!(
        load.await.unwrap().unwrap(),
        TARGET,
        "取回所有权失败后仍按只读进入，而不是丢回「进不去」"
    );
    assert_eq!(client.current_session_id().as_deref(), Some(TARGET));
    assert_eq!(
        client.current_execution_cwd().as_deref(),
        Some(EFFECTIVE_CWD)
    );
    assert!(
        client.check_restore_error().is_ok(),
        "只读准入不是恢复失败，重取历史失败也不算"
    );
    assert_eq!(
        atoms::SESSION_READ_ONLY.state().read().clone(),
        Some(ReadOnlyAdmission::RecoveryRequired(target))
    );
    assert_eq!(
        atoms::BRIDGE_RESET_COUNTER.get(),
        boundary_before + 3,
        "三次会被宿主回放历史的 load（首次、reset 后、重取）各需要一个回放边界"
    );
    assert_no_request(&server, Duration::from_millis(100)).await;
}

/// 最坏情况：reset 与重新取历史都失败，会话仍按首次只读准入进入。
#[tokio::test]
#[serial_test::serial]
async fn test_read_only_dirty_accept_all_retries_failed_still_enters_read_only() {
    let _guard = UiAtomsGuard::capture();
    let (client, server) = interactive_client();
    let boundary_before = atoms::BRIDGE_RESET_COUNTER.get();
    let target = RecoveryRequiredDetails {
        thread_id: TARGET.to_string(),
        generation: 6,
    };
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session(TARGET, "/startup", None).await });
    reach_load(
        &server,
        Ok(read_only_response(&ReadOnlyAdmission::RecoveryRequired(
            target.clone(),
        ))),
    )
    .await;
    let owner = wait_for_recovery_owner().await;
    owner.mark_displayed();
    owner.answer(true);

    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "peri/session_reset_dirty");
    server
        .send_response(
            id,
            Err(AcpError::new(
                -32010,
                "session is owned by another execution host",
            )),
        )
        .await
        .unwrap();
    // 首次准入是只读：仍按它重取一次历史，失败也只留空视图。
    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "session/load");
    server
        .send_response(id, Err(AcpError::new(-32603, "reload unavailable")))
        .await
        .unwrap();

    assert_eq!(load.await.unwrap().unwrap(), TARGET);
    assert_eq!(client.current_session_id().as_deref(), Some(TARGET));
    assert!(
        client.check_restore_error().is_ok(),
        "取回所有权被拒不是本次准入的失败：会话已按只读进入"
    );
    assert_eq!(
        atoms::SESSION_READ_ONLY.state().read().clone(),
        Some(ReadOnlyAdmission::RecoveryRequired(target))
    );
    assert_eq!(
        atoms::BRIDGE_RESET_COUNTER.get(),
        boundary_before + 2,
        "首次与重取各需要一个回放边界；reset 失败没有发生回放"
    );
    assert_no_request(&server, Duration::from_millis(100)).await;
}

/// 只读标记是交互投影：非交互客户端没有状态栏，也不该写这个全局 atom——它唯一的
/// 清空点（`project_session_boundary`）只在交互路径上调用，写进去就再也不清。
#[tokio::test]
#[serial_test::serial]
async fn test_headless_read_only_load_leaves_no_ui_marker() {
    let _guard = UiAtomsGuard::capture();
    let (transport, server) = mpsc_transport_pair();
    let (headless, _, _) = AcpTuiClient::new(transport);
    headless
        .session_workspace
        .store(true, std::sync::atomic::Ordering::Release);
    let loader = headless.clone();
    let load = tokio::spawn(async move { loader.load_session(TARGET, "/startup", None).await });
    reach_load(
        &server,
        Ok(read_only_response(&ReadOnlyAdmission::ExecutionBusy)),
    )
    .await;

    assert_eq!(load.await.unwrap().unwrap(), TARGET);
    assert!(
        atoms::SESSION_READ_ONLY.state().read().is_none(),
        "非交互客户端不得写交互投影 atom（没有清空点，且会污染并行 UI 测试）"
    );
    assert_no_request(&server, Duration::from_millis(100)).await;
}
