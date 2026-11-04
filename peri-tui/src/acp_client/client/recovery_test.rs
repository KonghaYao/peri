use super::*;
use crate::acp_client::client::test_support::{install_history_v2, with_history_v2_initial};
use crate::kit::atoms::{self, CONFIRM_PAYLOAD};
use peri_acp::transport::{
    AcpTransport,
    mpsc::{MpscServerTransport, mpsc_transport_pair},
    types::{IncomingMessage, RequestId},
};
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
        save_atom!(atoms::SERVICE_SNAPSHOT);
        save_atom!(atoms::FILE_LIST);
        save_atom!(atoms::HOOK_LIST);
        save_atom!(atoms::PLUGIN_LIST);
        save_atom!(atoms::MCP_SERVERS);
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
    (client, server)
}

fn v2_interactive_client() -> (AcpTuiClient, MpscServerTransport) {
    let (client, server) = interactive_client();
    install_history_v2(&client);
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
            panic!("unexpected client request after session load: {message:?}")
        }
    }
}

/// 等客户端请求；若请求方任务先结束（成功或失败），把真实结果暴露出来——
/// 只等 wire 会把它报成 5 秒超时，看不出客户端为什么没发请求。
async fn next_request_or_task(
    server: &MpscServerTransport,
    task: &mut tokio::task::JoinHandle<Result<String, AcpError>>,
) -> (RequestId, String, Value) {
    loop {
        tokio::select! {
            result = &mut *task => panic!("客户端未发出请求就结束: {result:?}"),
            message = server.recv() => {
                let Some(IncomingMessage::Request { id, method, params }) = message else {
                    continue;
                };
                return (id, method, params);
            }
        }
    }
}

/// 协商到 `session/load` 请求按其结果回答为止，返回 load 的请求参数。
async fn reach_load(server: &MpscServerTransport, response: Result<Value, AcpError>) -> Value {
    let (id, method, params) = next_request(server).await;
    assert_eq!(method, "peri/session_context");
    assert_eq!(params["sessionId"], TARGET);
    server
        .send_response(id, Ok(context(EFFECTIVE_CWD)))
        .await
        .unwrap();
    let (id, method, params) = next_request(server).await;
    assert_eq!(method, "session/load");
    let response = response.map(|value| with_history_v2_initial(value, TARGET, &params));
    server.send_response(id, response).await.unwrap();
    params
}

#[tokio::test]
#[serial_test::serial]
async fn load_by_id_has_no_recovery_popup_or_reset_request() {
    let _guard = UiAtomsGuard::capture();
    let (client, server) = v2_interactive_client();
    let (load, params) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            client.load_session(TARGET, "/another-machine", None),
            reach_load(&server, Ok(json!({})))
        )
    })
    .await
    .expect("session load must complete without a recovery decision");
    assert_eq!(params["sessionId"], TARGET);
    assert_eq!(params["cwd"], EFFECTIVE_CWD);
    assert_eq!(load.unwrap(), TARGET);
    assert_eq!(
        client.current_execution_cwd().as_deref(),
        Some(EFFECTIVE_CWD)
    );
    assert!(CONFIRM_PAYLOAD.state().read().is_none());
    assert_no_request(&server, Duration::from_millis(50)).await;
}

#[tokio::test]
#[serial_test::serial]
async fn load_ignores_legacy_execution_admission_metadata() {
    let _guard = UiAtomsGuard::capture();
    let (client, server) = v2_interactive_client();
    let (load, params) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            client.load_session(TARGET, "/startup", None),
            reach_load(
                &server,
                Ok(json!({"_meta": {"peri.sessionWorkspaceV1": {
                    "read_only": {"reason": "execution_busy"},
                    "restore_warning": "former_owner_unverified"
                }}}))
            )
        )
    })
    .await
    .expect("legacy metadata must not affect session loading");
    assert_eq!(load.unwrap(), TARGET);
    assert_eq!(params["cwd"], EFFECTIVE_CWD);
    assert!(client.check_restore_error().is_ok());
    assert_eq!(atoms::ACTIVE_SESSION_ID.state().read().as_str(), TARGET);
    assert!(CONFIRM_PAYLOAD.state().read().is_none());
    assert_no_request(&server, Duration::from_millis(50)).await;
}

/// `/clear` 先关闭旧会话：`-32010` 是 ACP 明示的可重试错误，客户端必须在预算内
/// 重放同一 `session/close`，而不是把 incomplete 当成切换失败。
#[tokio::test]
#[serial_test::serial]
async fn close_incomplete_is_replayed_before_establishing_next_session() {
    let _guard = UiAtomsGuard::capture();
    let (client, server) = interactive_client();
    let opener = client.clone();
    let first = tokio::spawn(async move { opener.new_session("/w", None).await });
    let (id, method, params) = next_request(&server).await;
    assert_eq!(method, "session/new");
    assert_eq!(params["cwd"], "/w");
    server
        .send_response(id, Ok(json!({ "sessionId": "first" })))
        .await
        .unwrap();
    assert_eq!(first.await.unwrap().unwrap(), "first");

    let reopener = client.clone();
    let second = tokio::spawn(async move { reopener.new_session("/w", None).await });
    let (id, method, params) = next_request(&server).await;
    assert_eq!(method, "session/close");
    assert_eq!(params["sessionId"], "first");
    server
        .send_response(id, Err(AcpError::new(-32010, "Session close incomplete")))
        .await
        .unwrap();
    let (id, method, params) = next_request(&server).await;
    assert_eq!(method, "session/close");
    assert_eq!(
        params["sessionId"], "first",
        "incomplete 必须重放同一关闭请求"
    );
    server.send_response(id, Ok(json!({}))).await.unwrap();
    let (id, method, params) = next_request(&server).await;
    assert_eq!(method, "session/new");
    assert_eq!(params["cwd"], "/w");
    server
        .send_response(id, Ok(json!({ "sessionId": "second" })))
        .await
        .unwrap();
    assert_eq!(second.await.unwrap().unwrap(), "second");
    assert!(client.check_restore_error().is_ok());
    assert_eq!(client.current_session_id().as_deref(), Some("second"));
}

/// 关闭旧会话持续 incomplete：失败后必须保留旧身份，下一次建立会话先重放同一
/// 关闭——不得绕过关闭直接 `session/new`（服务端旧会话可能仍在 Closing），也不得
/// 写入粘滞 `restore_error`（那会让后续每次发送/建立都被同一条陈旧错误拦截）。
#[tokio::test]
#[serial_test::serial]
async fn incomplete_close_failure_keeps_old_session_pending() {
    let _guard = UiAtomsGuard::capture();
    let (client, server) = interactive_client();
    let opener = client.clone();
    let first = tokio::spawn(async move { opener.new_session("/w", None).await });
    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "session/new");
    server
        .send_response(id, Ok(json!({ "sessionId": "first" })))
        .await
        .unwrap();
    assert_eq!(first.await.unwrap().unwrap(), "first");

    let reopener = client.clone();
    let mut attempt = tokio::spawn(async move { reopener.new_session("/w", None).await });
    let mut replays = 0usize;
    let error = loop {
        tokio::select! {
            result = &mut attempt => break result.unwrap().unwrap_err(),
            message = server.recv() => {
                let Some(IncomingMessage::Request { id, method, params }) = message else {
                    panic!("关闭预算内客户端只应重放 session/close");
                };
                assert_eq!(method, "session/close");
                assert_eq!(params["sessionId"], "first");
                replays += 1;
                server
                    .send_response(id, Err(AcpError::new(-32010, "Session close incomplete")))
                    .await
                    .unwrap();
            }
        }
    };
    assert_eq!(error.code, -32010);
    assert!(
        replays > 1,
        "incomplete 必须在预算内重放：replays={replays}"
    );
    assert!(
        client.check_restore_error().is_ok(),
        "关闭失败不得写入粘滞错误"
    );
    assert!(!client.has_session(), "关闭失败后本会话身份不再保留");

    let retry_client = client.clone();
    let retry = tokio::spawn(async move { retry_client.new_session("/w", None).await });
    let (id, method, params) = next_request(&server).await;
    assert_eq!(
        method, "session/close",
        "重试必须先完成同一个关闭，而不是绕过它新建会话"
    );
    assert_eq!(params["sessionId"], "first");
    server.send_response(id, Ok(json!({}))).await.unwrap();
    let (id, method, params) = next_request(&server).await;
    assert_eq!(method, "session/new");
    assert_eq!(params["cwd"], "/w");
    server
        .send_response(id, Ok(json!({ "sessionId": "second" })))
        .await
        .unwrap();
    assert_eq!(retry.await.unwrap().unwrap(), "second");
    assert!(client.check_restore_error().is_ok());
}

/// 关闭 A 失败打断「加载 B」：下一次输入必须继续关闭 A 并完成加载 B，不得改去
/// 新建空会话——那会静默丢掉用户要的历史上下文。
#[tokio::test]
#[serial_test::serial]
async fn incomplete_close_keeps_pending_load_target() {
    let _guard = UiAtomsGuard::capture();
    let (client, server) = v2_interactive_client();
    let opener = client.clone();
    let first = tokio::spawn(async move { opener.new_session("/w", None).await });
    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "session/new");
    server
        .send_response(id, Ok(json!({ "sessionId": "first" })))
        .await
        .unwrap();
    assert_eq!(first.await.unwrap().unwrap(), "first");

    let switcher = client.clone();
    let mut attempt = tokio::spawn(async move { switcher.load_session(TARGET, "/w", None).await });
    let (id, method, params) = next_request_or_task(&server, &mut attempt).await;
    assert_eq!(method, "peri/session_context");
    assert_eq!(params["sessionId"], TARGET);
    server
        .send_response(id, Ok(context(EFFECTIVE_CWD)))
        .await
        .unwrap();

    let error = loop {
        tokio::select! {
            result = &mut attempt => break result.unwrap().unwrap_err(),
            message = server.recv() => {
                let Some(IncomingMessage::Request { id, method, params }) = message else {
                    panic!("关闭预算内客户端只应重放 session/close");
                };
                assert_eq!(method, "session/close");
                assert_eq!(params["sessionId"], "first");
                server
                    .send_response(id, Err(AcpError::new(-32010, "Session close incomplete")))
                    .await
                    .unwrap();
            }
        }
    };
    assert_eq!(error.code, -32010);
    assert!(!client.has_session(), "关闭失败后本会话身份不再保留");

    // 下一次输入：先关闭同一个「first」，然后完成被打断的加载。
    let sender = client.clone();
    let mut resumed = tokio::spawn(async move { sender.ensure_session("/w", None).await });
    let (id, method, params) = next_request_or_task(&server, &mut resumed).await;
    assert_eq!(method, "peri/session_context");
    assert_eq!(params["sessionId"], TARGET);
    server
        .send_response(id, Ok(context(EFFECTIVE_CWD)))
        .await
        .unwrap();
    let (id, method, params) = next_request_or_task(&server, &mut resumed).await;
    assert_eq!(method, "session/close", "重试必须先完成同一个关闭");
    assert_eq!(params["sessionId"], "first");
    server.send_response(id, Ok(json!({}))).await.unwrap();
    let (id, method, params) = next_request_or_task(&server, &mut resumed).await;
    assert_eq!(
        method, "session/load",
        "必须完成被打断的加载，而不是新建空会话"
    );
    assert_eq!(params["sessionId"], TARGET);
    server
        .send_response(id, Ok(with_history_v2_initial(json!({}), TARGET, &params)))
        .await
        .unwrap();
    assert_eq!(resumed.await.unwrap().unwrap(), TARGET);
    assert_eq!(client.current_session_id().as_deref(), Some(TARGET));
    assert!(client.check_restore_error().is_ok());
}
