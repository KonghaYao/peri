use super::*;
use crate::kit::atoms::{self, CONFIRM_PAYLOAD};
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

fn context(cwd: &str) -> Value {
    json!({"version":1,"workspace":{
        "project_id":"00000000-0000-0000-0000-000000000001",
        "workspace_id":"00000000-0000-0000-0000-000000000002",
        "execution_registration_id":"00000000-0000-0000-0000-000000000002",
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
    server.send_response(id, response).await.unwrap();
    params
}

/// `session/load` 的只读准入响应：历史可读，但本次准入没有执行所有权。
fn read_only_response(admission: &ReadOnlyAdmission) -> Value {
    json!({"_meta": {"peri.sessionWorkspaceV1": {"read_only": admission}}})
}

#[tokio::test]
#[serial_test::serial]
async fn load_by_id_has_no_recovery_popup_or_reset_request() {
    let _guard = UiAtomsGuard::capture();
    let (client, server) = interactive_client();
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
async fn unavailable_environment_restores_history_without_confirmation() {
    let _guard = UiAtomsGuard::capture();
    let (client, server) = interactive_client();
    let admission = ReadOnlyAdmission::ExecutionLeaseRequired;
    let (load, _) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            client.load_session(TARGET, "/startup", None),
            reach_load(&server, Ok(read_only_response(&admission)))
        )
    })
    .await
    .expect("read-only session load must complete without confirmation");
    assert_eq!(load.unwrap(), TARGET);
    assert!(client.check_restore_error().is_ok());
    assert!(CONFIRM_PAYLOAD.state().read().is_none());
    assert_eq!(
        atoms::SESSION_READ_ONLY.state().read().clone(),
        Some(admission)
    );
    assert_no_request(&server, Duration::from_millis(50)).await;
}

#[tokio::test]
#[serial_test::serial]
async fn dirty_session_restores_history_without_recovery_popup_or_reset_request() {
    let _guard = UiAtomsGuard::capture();
    let (client, server) = interactive_client();
    let admission = ReadOnlyAdmission::RecoveryRequired(RecoveryRequiredDetails {
        thread_id: TARGET.into(),
        generation: 7,
    });
    let (load, params) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            client.load_session(TARGET, "/another-machine", None),
            reach_load(&server, Ok(read_only_response(&admission)))
        )
    })
    .await
    .expect("dirty session load must complete without a recovery decision");
    assert_eq!(params["sessionId"], TARGET);
    assert_eq!(params["cwd"], EFFECTIVE_CWD);
    assert_eq!(load.unwrap(), TARGET);
    assert!(client.check_restore_error().is_ok());
    assert_eq!(
        client.current_execution_cwd().as_deref(),
        Some(EFFECTIVE_CWD)
    );
    assert_eq!(atoms::ACTIVE_SESSION_ID.state().read().as_str(), TARGET);
    assert_eq!(
        atoms::SESSION_READ_ONLY.state().read().clone(),
        Some(admission)
    );
    assert!(CONFIRM_PAYLOAD.state().read().is_none());
    assert!(atoms::POPUP_KIND.state().read().is_none());
    assert_no_request(&server, Duration::from_millis(50)).await;
}
