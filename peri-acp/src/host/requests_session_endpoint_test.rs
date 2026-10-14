//! [审查中 3①] 端点注册是**会话级**的，不是轮级的。
//!
//! 缺陷形状：`register_session_endpoint` 只在 `spawn_prompt` /
//! `spawn_mcp_apps_request` 调用 ⇒ `session/new` 后从未 prompt 的连接不注册，
//! 调度触发（cron / loop）的审批从「弹窗」退化为「未获批准」。
//!
//! 本用例走**真实 server loop**（`ServerLoop::run`）：`initialize` → `session/load`
//! （运行期丢失后的恢复路径），随后用调度触发归属（Host，无请求作用域）发起审批，
//! 断言它**真实送达**该连接（审批帧）而不是 `Unanswered`。反证用一条不经 server
//! loop 建立的会话（等价于修复前状态）：同一归属得到 `NoOnlineEndpoint`。

use super::*;
use peri_acp_types::interaction::{
    ApprovalItem, InteractionContext, InteractionResponse, TrustedInteractionOwner,
    UserInteractionBroker as _,
};

/// 脚本化 ACP 客户端：按序投递请求、记录响应 / 审批帧、按键应答。
struct ScriptedTransport {
    incoming: std::sync::Mutex<std::collections::VecDeque<IncomingMessage>>,
    responses: std::sync::Mutex<Vec<(RequestId, Result<Value, String>)>>,
    frames: std::sync::Mutex<Vec<(String, Value)>>,
}

impl ScriptedTransport {
    fn new(requests: Vec<(RequestId, &str, Value)>) -> Arc<Self> {
        Arc::new(Self {
            incoming: std::sync::Mutex::new(
                requests
                    .into_iter()
                    .map(|(id, method, params)| IncomingMessage::Request {
                        id,
                        method: method.to_string(),
                        params,
                    })
                    .collect(),
            ),
            responses: std::sync::Mutex::new(Vec::new()),
            frames: std::sync::Mutex::new(Vec::new()),
        })
    }

    fn response(&self, id: i64) -> Result<Value, String> {
        self.responses
            .lock()
            .unwrap()
            .iter()
            .find(|(request_id, _)| matches!(request_id, RequestId::Number(value) if *value == id))
            .map(|(_, result)| result.clone())
            .unwrap_or_else(|| Err(format!("请求 {id} 没有响应")))
    }

    fn permission_frames(&self) -> Vec<Value> {
        self.frames
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, _)| method == "session/request_permission")
            .map(|(_, params)| params.clone())
            .collect()
    }
}

#[async_trait::async_trait]
impl crate::transport::AcpTransport for ScriptedTransport {
    async fn send_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        self.frames
            .lock()
            .unwrap()
            .push((method.to_string(), params));
        if method == "session/request_permission" {
            return Ok(json!({"outcome": {"outcome": "selected", "optionId": "allow_once"}}));
        }
        Ok(Value::Null)
    }

    async fn send_notification(&self, _: &str, _: Value) -> Result<(), AcpError> {
        Ok(())
    }

    async fn recv(&self) -> Option<IncomingMessage> {
        self.incoming.lock().unwrap().pop_front()
    }

    async fn send_response(
        &self,
        id: RequestId,
        result: Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        self.responses
            .lock()
            .unwrap()
            .push((id, result.map_err(|error| format!("{error:?}"))));
        Ok(())
    }
}

fn scheduled_approval() -> InteractionContext {
    InteractionContext::Approval {
        items: vec![ApprovalItem {
            tool_call_id: "cron-call-1".to_string(),
            tool_name: "Bash".to_string(),
            tool_input: json!({"command": "ls"}),
        }],
    }
}

/// 调度触发的归属：Host 入口，**无请求作用域连接**（cron / loop 的形态）。
fn scheduled_owner() -> TrustedInteractionOwner {
    TrustedInteractionOwner::host("peri/cron/scheduled-trigger".to_string(), 1)
}

/// `session/load` 之后，调度触发的审批必须可达本连接（端点是会话级的）。
#[tokio::test]
#[serial]
async fn session_load_registers_connection_endpoint_and_scheduled_approval_is_deliverable() {
    let fixture = Fixture::new().await;
    let id = fixture.id.clone();
    // 运行期丢失：运行时状态清零（持久存储保留）⇒ `session/load` 走**完整恢复**
    // 路径（重建会话环境），而不是 `AlreadyLive` 复用。
    let sessions: crate::host::SharedSessions = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let mut cfg = fixture.cfg;
    // 会话环境（含交互 router）由工作区装配产出：本用例要断言 `session/load` 之后
    // 重建设的 router 里本连接是**在线 endpoint**。
    cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd: fixture.cwd.to_string_lossy().into_owned(),
        bare: true,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
        capabilities: Default::default(),
    });
    let cfg = Arc::new(cfg);
    let transport = ScriptedTransport::new(vec![
        (
            RequestId::Number(1),
            "initialize",
            json!({"protocolVersion": 1, "clientCapabilities": {"_meta": {}}}),
        ),
        (
            RequestId::Number(2),
            "session/load",
            json!({"sessionId": id}),
        ),
    ]);
    let erased: Arc<dyn crate::transport::AcpTransport> =
        Arc::clone(&transport) as Arc<dyn crate::transport::AcpTransport>;

    let prompt_locks: crate::host::PromptLocks = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let (cont_tx, _cont_rx) = tokio::sync::mpsc::unbounded_channel();
    let cont_tx = Arc::new(cont_tx);
    let connection = Arc::new(tokio::sync::Mutex::new(
        crate::host::connection::ConnectionContext::new(true),
    ));
    let cancellation = tokio_util::sync::CancellationToken::new();
    crate::host::server_loop::ServerLoop {
        transport: &erased,
        cfg: &cfg,
        sessions: &sessions,
        prompt_locks: &prompt_locks,
        cont_tx: &cont_tx,
        connection: &connection,
        connection_cancellation: &cancellation,
    }
    .run()
    .await;

    transport
        .response(2)
        .unwrap_or_else(|error| panic!("session/load 必须返回结果（真实 server loop）：{error}"));

    let environment = {
        let guard = sessions.lock().await;
        let state = guard.get(&id).unwrap_or_else(|| {
            panic!(
                "session/load 必须登记会话；现有键: {:?}",
                guard.keys().collect::<Vec<_>>()
            )
        });
        state.environment.clone().unwrap_or_else(|| {
            panic!(
                "session/load 必须重建会话环境（closing={}, frozen={}, history={}, cwd={}）",
                state.closing,
                state.frozen.is_some(),
                state.history_payloads.len(),
                state.cwd
            )
        })
    };
    let response = environment
        .interaction_router()
        .request(scheduled_owner(), scheduled_approval())
        .await;
    assert!(
        !matches!(response, InteractionResponse::Unanswered { .. }),
        "session/load 后调度触发的审批必须可达（端点注册是会话级的），实际: {response:?}"
    );
    let frames = transport.permission_frames();
    assert_eq!(
        frames.len(),
        1,
        "审批帧必须送达该连接（弹窗而不是「未获批准」）"
    );
    assert_eq!(frames[0]["sessionId"], id, "审批帧必须携带本会话身份");
}

/// 反证（等价于修复前状态）：不经 server loop 建立的会话没有注册 endpoint，
/// 同一调度归属得到 `NoOnlineEndpoint`（这就是退化后的「未获批准」来源）。
#[tokio::test]
#[serial]
async fn session_without_loop_registration_is_not_deliverable() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cwd = tmp.path().join("workspace");
    std::fs::create_dir(&cwd).unwrap();
    std::fs::create_dir_all(cwd.join(".peri")).unwrap();
    let cwd = cwd.to_string_lossy().into_owned();
    let config = make_peri_config_with_provider(make_provider_config(
        "local",
        "openai",
        "fixture-key",
        "fixture-model",
    ));
    let provider = LlmProvider::from_config(&config).unwrap();
    let mut cfg = make_server_config(config, provider, &tmp).await;
    cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd: cwd.clone(),
        bare: true,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
        capabilities: Default::default(),
    });
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    let created = handle_request(
        "session/new",
        &json!({"cwd": cwd}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/new 必须成功");
    let id = created["sessionId"].as_str().unwrap().to_owned();
    let environment = sessions[&id]
        .environment
        .clone()
        .expect("session/new 必须装配环境（工作区装配已打开）");
    let response = environment
        .interaction_router()
        .request(scheduled_owner(), scheduled_approval())
        .await;
    match response {
        InteractionResponse::Unanswered { cause } => assert_eq!(
            cause,
            peri_acp_types::interaction::UnansweredCause::NoOnlineEndpoint,
            "没有任何在线 endpoint ⇒ 原因必须如实（不是「用户拒绝」）"
        ),
        other => panic!("未注册 endpoint 的连接不得收到审批，实际: {other:?}"),
    }
    // `session/load|resume|new` 三条路径在真实 server loop 里都会注册端点；
    // 本条反证说明「不注册」时的退化形态（fail-closed，不是自动批准）。
    assert!(
        environment
            .interaction_router()
            .lookup(&peri_acp_types::interaction::AttachmentId::new("none"))
            .is_none(),
        "反证夹具不需要任何附件"
    );
}

/// `session/fork` 同样建立**新**会话（`session_setup` 认它并派生新 id）：fork 之后该
/// 连接必须成为**新会话**的在线 endpoint，否则「fork 后从未 prompt」的连接在调度触发
/// 审批时不可达——与 3① 同一缺陷形状，只是注册白名单漏了 fork。
#[tokio::test]
#[serial]
async fn session_fork_registers_connection_endpoint_and_scheduled_approval_is_deliverable() {
    let fixture = Fixture::new().await;
    let source_id = fixture.id.clone();
    let sessions: crate::host::SharedSessions = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let mut cfg = fixture.cfg;
    // 运行期丢失：清空会话表后由真实 server loop 先 `session/load` 恢复源会话，
    // 再 `session/fork` 派生新会话（fork 要求源会话已在运行期加载）。
    cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd: fixture.cwd.to_string_lossy().into_owned(),
        bare: true,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
        capabilities: Default::default(),
    });
    let cfg = Arc::new(cfg);
    let transport = ScriptedTransport::new(vec![
        (
            RequestId::Number(1),
            "initialize",
            json!({"protocolVersion": 1, "clientCapabilities": {"_meta": {}}}),
        ),
        (
            RequestId::Number(2),
            "session/load",
            json!({"sessionId": source_id}),
        ),
        (
            RequestId::Number(3),
            "session/fork",
            json!({"sessionId": source_id}),
        ),
    ]);
    let erased: Arc<dyn crate::transport::AcpTransport> =
        Arc::clone(&transport) as Arc<dyn crate::transport::AcpTransport>;

    let prompt_locks: crate::host::PromptLocks = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let (cont_tx, _cont_rx) = tokio::sync::mpsc::unbounded_channel();
    let cont_tx = Arc::new(cont_tx);
    let connection = Arc::new(tokio::sync::Mutex::new(
        crate::host::connection::ConnectionContext::new(true),
    ));
    let cancellation = tokio_util::sync::CancellationToken::new();
    crate::host::server_loop::ServerLoop {
        transport: &erased,
        cfg: &cfg,
        sessions: &sessions,
        prompt_locks: &prompt_locks,
        cont_tx: &cont_tx,
        connection: &connection,
        connection_cancellation: &cancellation,
    }
    .run()
    .await;

    transport
        .response(2)
        .unwrap_or_else(|error| panic!("session/load 必须返回结果（真实 server loop）：{error}"));
    let forked = transport
        .response(3)
        .unwrap_or_else(|error| panic!("session/fork 必须返回结果（真实 server loop）：{error}"));
    let forked_id = forked["sessionId"]
        .as_str()
        .expect("session/fork 必须返回新会话 id")
        .to_owned();
    assert_ne!(forked_id, source_id, "fork 派生的是新会话");

    let environment = {
        let guard = sessions.lock().await;
        let state = guard.get(&forked_id).unwrap_or_else(|| {
            panic!(
                "session/fork 必须登记新会话；现有键: {:?}",
                guard.keys().collect::<Vec<_>>()
            )
        });
        state.environment.clone().unwrap_or_else(|| {
            panic!(
                "session/fork 必须装配新会话环境（closing={}）",
                state.closing
            )
        })
    };
    let response = environment
        .interaction_router()
        .request(scheduled_owner(), scheduled_approval())
        .await;
    assert!(
        !matches!(response, InteractionResponse::Unanswered { .. }),
        "fork 后调度触发的审批必须可达（端点注册是会话级的），实际: {response:?}"
    );
    let frames = transport.permission_frames();
    assert_eq!(
        frames.len(),
        1,
        "审批帧必须送达该连接（弹窗而不是「未获批准」）"
    );
    assert_eq!(
        frames[0]["sessionId"], forked_id,
        "审批帧必须携带 fork 出来的会话身份"
    );
}

// ─── R8（批 G）：旧别名 `peri/mcp/invoke` 的准入（真实 server loop 端到端）────────

/// 生产链路（真实 `ServerLoop` + 真实 `session/new` 装配 + 真实会话状态）：会话**有在飞
/// prompt**（`SessionState.cancel_token = Some`——prompt 任务的登记点写的正是这个字段）
/// 时，`peri/mcp/invoke` **仍被准入**并走到 relay 执行面：拒绝只能来自 relay 自己的错误面
/// （本用例给一个不存在的 server ⇒ `unknown_server`），不能是旧
/// `InvokeSessionGate.prompt_in_flight` 的 `policy_denied`（4b 批 G 已删除该判据）。
///
/// 为什么必须走 server loop：被删掉的判据在 `host/server_loop.rs` 的构造点，由
/// `state.cancel_token` 投影；`handle_invoke` 单测无法设置「prompt 在飞」这一事实。
///
/// 反向验证（mutation）：把该快照改回读 `cancel_token`（`open: !state.closing &&
/// state.cancel_token.is_none()`，同一 wire 形态）后复跑本用例必须失败——实测响应为
/// `data: {"kind": "policy_denied"}`，即被删判据的原判决。
#[tokio::test]
#[serial]
async fn invoke_during_prompt_is_admitted_through_the_real_server_loop() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("workspace");
    std::fs::create_dir(&target).unwrap();
    std::fs::create_dir_all(target.join(".peri")).unwrap();
    let cwd = target.to_string_lossy().into_owned();

    let config = make_peri_config_with_provider(make_provider_config(
        "local",
        "openai",
        "fixture-key",
        "fixture-model",
    ));
    let provider = LlmProvider::from_config(&config).unwrap();
    let mut cfg = make_server_config(config, provider, &tmp).await;
    // apps 能力打开：会话环境装配点据此构造 relay + facade（`apps/*` 与旧别名共用）。
    cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd: cwd.clone(),
        bare: true,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::deployment_profile(true),
        capabilities: Default::default(),
    });
    let cfg = Arc::new(cfg);

    // 先建会话（真实 `session/new` 装配），才能把「prompt 在飞」写进它的状态。
    let mut sessions = HashMap::new();
    let bootstrap: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let created = handle_request(
        "session/new",
        &json!({"cwd": cwd}),
        &cfg,
        &mut sessions,
        &bootstrap,
    )
    .await
    .expect("session/new 必须成功");
    let id = created["sessionId"].as_str().unwrap().to_owned();
    let environment = Arc::clone(
        sessions[&id]
            .environment
            .as_ref()
            .expect("会话环境（生产装配路径）"),
    );
    assert!(
        environment.cfg.mcp_apps_relay.is_some() && environment.apps_facade().is_some(),
        "apps 能力必须打开：否则请求会先被 capability_disabled 挡掉，本用例的断言落到空处"
    );

    sessions.get_mut(&id).unwrap().cancel_token = Some(tokio_util::sync::CancellationToken::new());
    let sessions: crate::host::SharedSessions = Arc::new(tokio::sync::Mutex::new(sessions));

    let transport = ScriptedTransport::new(vec![
        (
            RequestId::Number(1),
            "initialize",
            json!({"protocolVersion": 1, "clientCapabilities": {"_meta": {}}}),
        ),
        (
            RequestId::Number(2),
            "peri/mcp/invoke",
            json!({
                "envelopeVersion": peri_acp_types::mcp_apps::MCP_APPS_ENVELOPE_VERSION,
                "appsProtocolVersion": peri_acp_types::mcp_apps::MCP_APPS_PROTOCOL_VERSION,
                "serverId": "missing-server",
                "toolName": "missing-tool",
                "ownerSessionId": id,
                "arguments": {}
            }),
        ),
    ]);
    let erased: Arc<dyn crate::transport::AcpTransport> =
        Arc::clone(&transport) as Arc<dyn crate::transport::AcpTransport>;
    let prompt_locks: crate::host::PromptLocks = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let (cont_tx, _cont_rx) = tokio::sync::mpsc::unbounded_channel();
    let cont_tx = Arc::new(cont_tx);
    let connection = Arc::new(tokio::sync::Mutex::new(
        crate::host::connection::ConnectionContext::new(true),
    ));
    let cancellation = tokio_util::sync::CancellationToken::new();
    crate::host::server_loop::ServerLoop {
        transport: &erased,
        cfg: &cfg,
        sessions: &sessions,
        prompt_locks: &prompt_locks,
        cont_tx: &cont_tx,
        connection: &connection,
        connection_cancellation: &cancellation,
    }
    .run()
    .await;

    // apps 请求是 host-owned 任务（`spawn_mcp_apps_request`）：loop 退出后等它写出响应。
    let await_response = async {
        loop {
            let answered = transport.responses.lock().unwrap().iter().any(
                |(request_id, _)| matches!(request_id, RequestId::Number(value) if *value == 2),
            );
            if answered {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), await_response)
        .await
        .expect("invoke 必须在 10s 内给出响应（真实 server loop 的 host-owned 任务）");

    let error = transport
        .response(2)
        .expect_err("目标 server 不存在 ⇒ 必须是 relay 的错误面（不是成功响应）");
    assert!(
        error.contains("unknown_server"),
        "会话有在飞 prompt 时 invoke 必须走到 relay 执行面（relay 的既定错误面 = unknown_server），实际: {error}"
    );
    assert!(
        !error.contains("policy_denied"),
        "准入不得因「prompt 在飞」拒绝（R8 已删除该判据；那会破坏与 canonical 面的同一判决），实际: {error}"
    );
}
