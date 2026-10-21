//! session-owned canonical 入口（`SessionInvocationService`）的回归测试。
//!
//! 全部用例走**真实**路径：真实连接池 + 真实 MCP peer（进程内 duplex server，计数
//! 每次 `tools/call`）+ 真实 bridge/dispatcher/管线 + 真实 record 注册表。唯一被
//! 替代的是「用户决定」这一不可控外部输入（broker 桩）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use peri_acp_types::interaction::{AttachmentId, UnansweredCause};
use peri_acp_types::interaction::{InteractionContext, InteractionResponse, UserInteractionBroker};
use peri_acp_types::mcp_apps::{
    InvocationTarget, NotExecutedCause, SessionInvocationOutcome, SessionInvocationPort,
    SessionInvocationRequest,
};
use peri_acp_types::permission::{PermissionMode, SharedPermissionMode};
use peri_acp_types::tool_invocation::{InvocationId, InvocationOrigin};
use peri_agent::middleware::chain::MiddlewareChain;
use peri_agent::session::queue::MessageQueue;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ServerCapabilities,
    ServerConfig,
};
use tokio_util::sync::CancellationToken;

use super::super::apps::{AttachmentRoute, McpAppRecord, McpAppRecordSpec};
use super::super::client::output_store::tests::Wire;
use super::super::client::McpClientPool;
use super::*;
use peri_acp_types::mcp_apps_facade::AppQualification;

const SESSION: &str = "idle-session";
const SERVER: &str = "app-server";
const APP_TOOL: &str = "show_canvas";

/// App 可见工具（声明 `ui.resourceUri`）。
fn app_tool() -> rmcp::model::Tool {
    serde_json::from_value(serde_json::json!({
        "name": APP_TOOL,
        "description": "canvas",
        "inputSchema": {"type": "object"},
        "_meta": {"ui": {"resourceUri": "ui://app", "visibility": ["app", "model"]}}
    }))
    .expect("夹具工具声明必须可解析")
}

/// 把真实 peer 装进池（工具面是 App 工具，而不是通用夹具工具）。
fn install_app_server(pool: &McpClientPool, wire: &Wire) {
    let handle = Arc::new(super::super::client::McpClientHandle {
        call_gate: Default::default(),
        name: SERVER.to_string(),
        version: None,
        connected_at: None,
        protocol_version: None,
        cache_version: None,
        peer: Some(wire.client.peer().clone()),
        tools: vec![app_tool()],
        resources: vec![],
        status: super::super::client::ClientStatus::Connected,
        oauth_status: Default::default(),
        source: None,
        url: None,
        skills_capable: false,
    });
    pool.advance_handle_generation(&handle);
    pool.clients.write().insert(SERVER.to_string(), handle);
    // 生产同源：会话级 task manager 绑定（bridge 调用前会校验会话任务作用域）。
    let manager: Arc<dyn peri_acp_types::tasks::TaskManager> =
        Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    pool.bind_session_task_manager(SESSION, &manager);
}

/// 真实 MCP server 夹具：记录每次 `tools/call` 的调用与参数。
struct AppServer {
    calls: Arc<AtomicUsize>,
    inputs: Arc<Mutex<Vec<serde_json::Value>>>,
    gate: Option<Arc<tokio::sync::Notify>>,
}

impl rmcp::ServerHandler for AppServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            rmcp::model::Implementation::new("peri-app-fixture", "session_invocation_test"),
        )
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<CallToolResponse, rmcp::ErrorData> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inputs.lock().push(serde_json::Value::Object(
            request.arguments.unwrap_or_default(),
        ));
        // 只阻塞第一次调用：测试用它观察「已 dispatch 但尚未返回」的窗口。
        if let Some(gate) = self.gate.as_ref() {
            if self.calls.load(Ordering::SeqCst) == 1 {
                gate.notified().await;
            }
        }
        Ok(CallToolResponse::Complete(CallToolResult::success(vec![
            ContentBlock::text("canvas ready"),
        ])))
    }
}

/// 用户决定桩（唯一被替代的外部输入）。
struct DecisionStub {
    response: Mutex<InteractionResponse>,
    /// 决定作出后触发的观察钩子（模拟「用户思考期间 App 被关闭」）。
    before_response: Option<Arc<dyn Fn() + Send + Sync>>,
    seen_owners: Mutex<Vec<String>>,
}

#[async_trait]
impl UserInteractionBroker for DecisionStub {
    async fn request(
        &self,
        owner: peri_acp_types::interaction::TrustedInteractionOwner,
        _ctx: InteractionContext,
    ) -> InteractionResponse {
        self.seen_owners.lock().push(owner.to_string());
        if let Some(hook) = self.before_response.as_ref() {
            hook();
        }
        self.response.lock().clone()
    }
}

fn edit_response(arguments: serde_json::Value) -> InteractionResponse {
    InteractionResponse::Decisions(vec![peri_acp_types::interaction::ApprovalDecision::Edit {
        new_input: arguments,
    }])
}

struct Fixture {
    service: Arc<SessionInvocationService>,
    pool: Arc<McpClientPool>,
    wire: Wire,
    calls: Arc<AtomicUsize>,
    inputs: Arc<Mutex<Vec<serde_json::Value>>>,
    session_cancel: CancellationToken,
    decision: Arc<DecisionStub>,
    /// record 身份 → 附着时的附件事实（附件身份 + 纪元）。
    attachment_facts: Mutex<
        std::collections::HashMap<
            String,
            (AttachmentId, peri_acp_types::interaction::ConnectionEpoch),
        >,
    >,
}

impl Fixture {
    async fn new(
        response: InteractionResponse,
        before_response: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Self {
        Self::with_gate(response, before_response, None).await
    }

    async fn with_gate(
        response: InteractionResponse,
        before_response: Option<Arc<dyn Fn() + Send + Sync>>,
        gate: Option<Arc<tokio::sync::Notify>>,
    ) -> Self {
        let calls = Arc::new(AtomicUsize::new(0));
        let inputs = Arc::new(Mutex::new(Vec::new()));
        let wire = Wire::connect(AppServer {
            calls: calls.clone(),
            inputs: inputs.clone(),
            gate,
        })
        .await;
        let pool = Arc::new(McpClientPool::new_empty());
        install_app_server(&pool, &wire);

        let decision = Arc::new(DecisionStub {
            response: Mutex::new(response),
            before_response,
            seen_owners: Mutex::new(Vec::new()),
        });
        // 规则链：与生产同一构造实现（Permission 槽位），broker 用决定桩。
        let mut rules = MiddlewareChain::new();
        rules.add(Box::new(
            crate::permission::PermissionMiddleware::with_shared_mode(
                decision.clone(),
                crate::permission::default_requires_approval,
                SharedPermissionMode::new(PermissionMode::Default),
                None,
            ),
        ));
        let session_cancel = CancellationToken::new();
        let service = Arc::new(SessionInvocationService::new(SessionInvocationInputs {
            pool: Arc::clone(&pool),
            agent_id: peri_acp_types::identity::AgentId::new(),
            session_id: SESSION.to_string(),
            cwd: std::sync::Arc::from("/tmp/app-session"),
            queue: Arc::new(MessageQueue::new),
            rules: Arc::new(InvocationRuleChain::from_derived_blueprint(rules)),
            closed_instances: Default::default(),
            session_cancel: session_cancel.clone(),
        }));
        Self {
            service,
            pool,
            wire,
            calls,
            inputs,
            attachment_facts: Mutex::new(std::collections::HashMap::new()),
            session_cancel,
            decision,
        }
    }

    /// 签发并附着一条 App record（真实注册表路径）。
    ///
    /// 返回的 record 可能随后被撤销（此时它的附件集合为空）：夹具把**附着时**的
    /// 附件事实单独记下，供 `app_request` 构造「来自该 View 的请求」——撤销不改变
    /// 这个历史事实，只是它不再有效。
    fn attach_record(&self, connection_id: &str) -> Arc<McpAppRecord> {
        let handle = self
            .pool
            .get_client(SERVER)
            .expect("fixture server installed");
        let generation = self.pool.handle_generation(&handle);
        // 生产同序：会话环境登记运行时世代 → 实例化调用签发 record → 附着连接。
        self.pool
            .app_binding_leases
            .register_session_incarnation(SESSION, self.service.runtime_epoch());
        let record = Arc::new(McpAppRecord::new(McpAppRecordSpec {
            owner_session_id: SESSION.to_string(),
            server_id: SERVER.to_string(),
            server_generation: generation,
            resource_uri: "ui://app".to_string(),
            instantiating_tool: APP_TOOL.to_string(),
            invocation_id: InvocationId::generate(),
            invocation_token: "token-fixture".to_string(),
            tool_call_id: Some(peri_acp_types::tool_invocation::ToolCallId::new(
                "call-fixture",
            )),
            incarnation: self.service.runtime_epoch(),
            qualification: AppQualification::Interactive {
                allowed_tools: HashMap::from([(
                    APP_TOOL.to_string(),
                    super::super::tool_bridge::effective_mcp_tool_name(SERVER, APP_TOOL),
                )]),
            },
            effective_input: serde_json::Map::new(),
            initial_result: Arc::new(peri_acp_types::tool_outcome::RawToolResult::new(
                serde_json::json!({"content": [{"type": "text", "text": "fixture"}]}),
            )),
        }));
        self.pool
            .app_binding_leases
            .issue(Arc::clone(&record))
            .expect("夹具自检：世代一致的记录必须可登记");
        let claimed = self
            .pool
            .app_binding_leases
            .find(&record.record_id)
            .expect("夹具自检：已签发 record 必须可读（附着按 record 身份发生）");
        let attached = Arc::new(AttachmentRoute::new(
            connection_id,
            AttachmentId::new(record.record_id.clone()),
            peri_acp_types::mcp_apps_facade::RequestNamespaceId::new(),
            peri_acp_types::interaction::ConnectionEpoch::initial(),
            peri_acp_types::mcp_apps_facade::ClientViewId::new(connection_id),
        ));
        assert!(claimed.attach(attached), "夹具自检：领取到的记录必须可附着");
        self.attachment_facts.lock().insert(
            record.record_id.clone(),
            (
                AttachmentId::new(record.record_id.clone()),
                peri_acp_types::interaction::ConnectionEpoch::initial(),
            ),
        );
        record
    }

    /// 给**同一个 record** 再挂一个 View 附件（多附件事实：附件身份 + 纪元）。
    ///
    /// 用于验证「多附件下 detach 一个不影响其他附件与执行」（裁决 D6）。
    fn attach_second_view(
        &self,
        record: &Arc<McpAppRecord>,
        connection_id: &str,
        attachment_id: &str,
    ) -> (AttachmentId, peri_acp_types::interaction::ConnectionEpoch) {
        let route = Arc::new(AttachmentRoute::new(
            connection_id,
            AttachmentId::new(attachment_id),
            peri_acp_types::mcp_apps_facade::RequestNamespaceId::new(),
            peri_acp_types::interaction::ConnectionEpoch::initial(),
            peri_acp_types::mcp_apps_facade::ClientViewId::new(connection_id),
        ));
        let epoch = route.connection_epoch;
        assert!(
            record.attach(route),
            "夹具自检：同一 record 必须可挂第二个 View"
        );
        (AttachmentId::new(attachment_id), epoch)
    }

    /// 来自**指定 View**（附件身份 + 纪元）的 App 回调请求。
    fn app_request_from(
        &self,
        record: &Arc<McpAppRecord>,
        attachment_id: AttachmentId,
        connection_epoch: peri_acp_types::interaction::ConnectionEpoch,
        arguments: serde_json::Value,
    ) -> SessionInvocationRequest {
        SessionInvocationRequest {
            session_id: SESSION.to_string(),
            runtime_epoch: self.service.runtime_epoch(),
            origin: InvocationOrigin::App {
                app_id: record.record_id.clone(),
                attachment_id: Some(attachment_id),
                connection_epoch: Some(connection_epoch),
                server_generation: record.server_generation,
            },
            target: InvocationTarget::App {
                record_id: record.record_id.clone(),
                tool_name: APP_TOOL.to_string(),
                arguments: arguments.as_object().cloned().unwrap_or_default(),
            },
            cancel: CancellationToken::new(),
            invocation_id: None,
        }
    }

    fn app_request(
        &self,
        record: &Arc<McpAppRecord>,
        arguments: serde_json::Value,
    ) -> SessionInvocationRequest {
        let (attachment_id, connection_epoch) = self
            .attachment_facts
            .lock()
            .get(&record.record_id)
            .cloned()
            .expect("夹具自检：该 record 必须曾附着（附件事实在撤销后仍保留为历史）");
        self.app_request_from(record, attachment_id, connection_epoch, arguments)
    }

    async fn shutdown(self) {
        self.pool.app_binding_leases.revoke_session(SESSION);
        self.wire.close().await;
    }
}

fn calls(fixture: &Fixture) -> usize {
    fixture.calls.load(Ordering::SeqCst)
}

/// [A12] idle App 回调：经 canonical 管线真实执行；无 turn 事实（无 step、无模型
/// 可见事件），审批归属为该 App 的 attachment。
#[tokio::test]
async fn idle_callback_runs_without_turn_and_publishes_no_model_cards() {
    let fixture = Fixture::new(
        InteractionResponse::Decisions(vec![
            peri_acp_types::interaction::ApprovalDecision::Approve { source: None },
        ]),
        None,
    )
    .await;
    let record = fixture.attach_record("connection-1");

    let outcome = fixture
        .service
        .invoke(fixture.app_request(&record, serde_json::json!({"value": 1})))
        .await
        .expect("请求本身合法");

    match outcome {
        SessionInvocationOutcome::Completed(raw) => {
            assert!(!raw.is_error());
        }
        other => panic!("Approve 后必须真实执行并返回 raw 结果，实际: {other:?}"),
    }
    assert_eq!(calls(&fixture), 1, "真实 peer 恰好收到一次调用");
    assert_eq!(
        fixture.inputs.lock().as_slice(),
        &[serde_json::json!({"value": 1})],
        "执行参数必须来自批准后的最终调用"
    );
    // 审批带 App 归属（不是伪造的模型归属）。
    {
        let owners = fixture.decision.seen_owners.lock();
        assert_eq!(owners.len(), 1);
        assert!(
            owners[0].starts_with("app(app=") && owners[0].contains(&record.record_id),
            "idle 审批必须带 App 归属，实际: {}",
            owners[0]
        );
    }
    // outcome 记录：无 turn 的事后钩子被显式记录跳过（裁决 R2），而不是静默。
    let latest = fixture
        .service
        .outcomes()
        .latest()
        .expect("outcome 必须入账");
    assert_eq!(
        latest.skipped_post_hooks,
        vec![peri_acp_types::mcp_apps::SkippedPostHook::AfterToolNotAvailableWithoutTurn],
        "无 turn 时 after_tool 必须显式记录跳过"
    );
    assert!(!fixture.session_cancel.is_cancelled());
    fixture.shutdown().await;
}

/// [回归] 无在线 endpoint：审批不可用**不是**用户拒绝，且保持零 wire 调用。
#[tokio::test]
async fn no_online_endpoint_is_not_executed_and_never_reported_as_user_rejection() {
    let fixture = Fixture::new(
        InteractionResponse::Unanswered {
            cause: UnansweredCause::NoOnlineEndpoint,
        },
        None,
    )
    .await;
    let record = fixture.attach_record("connection-1");

    let outcome = fixture
        .service
        .invoke(fixture.app_request(&record, serde_json::json!({})))
        .await
        .expect("请求本身合法");

    assert_eq!(
        outcome,
        SessionInvocationOutcome::NotExecuted {
            cause: NotExecutedCause::ApprovalUnavailable(UnansweredCause::NoOnlineEndpoint)
        },
        "「无人可答」必须如实记录，不得折叠成用户拒绝"
    );
    assert_eq!(calls(&fixture), 0, "未获批准 ⇒ 零 wire 调用");
    fixture.shutdown().await;
}

/// 批准改参后重校验：最终参数生效；若审批期间目标被撤销 ⇒ `NotExecuted` + 零 wire。
#[tokio::test]
async fn approval_edit_revalidates_bound_target() {
    // (a) 仅改参：批准后的最终参数必须原样送达 wire。
    let fixture = Fixture::new(edit_response(serde_json::json!({"value": 2})), None).await;
    let record = fixture.attach_record("connection-1");
    let outcome = fixture
        .service
        .invoke(fixture.app_request(&record, serde_json::json!({"value": 1})))
        .await
        .expect("请求本身合法");
    assert!(
        matches!(outcome, SessionInvocationOutcome::Completed(_)),
        "改参后仍必须执行，实际: {outcome:?}"
    );
    assert_eq!(
        fixture.inputs.lock().as_slice(),
        &[serde_json::json!({"value": 2})],
        "wire 必须收到批准后的最终参数（不重新解析，也不回到原参数）"
    );
    fixture.shutdown().await;

    // (b) 审批期间目标被撤销（record 撤销 + 提交复核）：零 wire 调用。
    let revoked_during_approval = {
        let service_holder: Arc<Mutex<Option<Arc<SessionInvocationService>>>> = Arc::default();
        let record_id: Arc<Mutex<Option<String>>> = Arc::default();
        let service_for_hook = Arc::clone(&service_holder);
        let record_for_hook = Arc::clone(&record_id);
        let hook: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let (Some(service), Some(record_id)) = (
                service_for_hook.lock().as_ref().cloned(),
                record_for_hook.lock().clone(),
            ) {
                service.revoke_app(&record_id);
            }
        });
        let fixture =
            Fixture::new(edit_response(serde_json::json!({"value": 2})), Some(hook)).await;
        let record = fixture.attach_record("connection-1");
        *service_holder.lock() = Some(Arc::clone(&fixture.service));
        *record_id.lock() = Some(record.record_id.clone());
        let outcome = fixture
            .service
            .invoke(fixture.app_request(&record, serde_json::json!({"value": 1})))
            .await
            .expect("请求本身合法");
        assert_eq!(
            calls(&fixture),
            0,
            "审批期间被撤销 ⇒ 复核失败必须发生在 wire 调用之前"
        );
        fixture.shutdown().await;
        outcome
    };
    assert_eq!(
        revoked_during_approval,
        SessionInvocationOutcome::NotExecuted {
            cause: NotExecutedCause::TargetRevoked
        },
        "撤销胜出必须显式未执行"
    );
}

/// 提交输给撤销：提交前撤销 ⇒ 上游零调用（真实 `AdmittedPeerTool` + wire 计数）。
#[tokio::test]
async fn commit_loses_to_revocation_before_dispatch() {
    let fixture = Fixture::new(
        InteractionResponse::Decisions(vec![
            peri_acp_types::interaction::ApprovalDecision::Approve { source: None },
        ]),
        None,
    )
    .await;
    let record = fixture.attach_record("connection-1");
    // 提交前撤销（与 `apps/close` 同源路径）。
    fixture.service.revoke_app(&record.record_id);

    let outcome = fixture
        .service
        .invoke(fixture.app_request(&record, serde_json::json!({})))
        .await
        .expect("请求本身合法");

    assert_eq!(
        outcome,
        SessionInvocationOutcome::NotExecuted {
            cause: NotExecutedCause::TargetRevoked
        }
    );
    assert_eq!(calls(&fixture), 0, "撤销后不得发出任何 wire 调用");
    assert!(
        fixture.decision.seen_owners.lock().is_empty(),
        "撤销后不得再打扰用户（准入在审批之前失败）"
    );
    fixture.shutdown().await;
}

/// [A2] 关闭一个 App / 一条连接：① 同轮执行不被取消；② 其他连接的 App 回调仍成功；
/// ③ 已 dispatch 的调用完成且 outcome 入账。
#[tokio::test]
async fn closing_one_app_does_not_cancel_turn_or_other_app() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let fixture = Fixture::with_gate(
        InteractionResponse::Decisions(vec![
            peri_acp_types::interaction::ApprovalDecision::Approve { source: None },
        ]),
        None,
        Some(Arc::clone(&gate)),
    )
    .await;
    let record_a = fixture.attach_record("connection-a");
    let record_b = fixture.attach_record("connection-b");

    // App A 的调用已 dispatch（wire 上阻塞在 gate）。
    let in_flight = {
        let service = Arc::clone(&fixture.service);
        let request = fixture.app_request(&record_a, serde_json::json!({"app": "a"}));
        tokio::spawn(async move { service.invoke(request).await })
    };
    while calls(&fixture) == 0 {
        tokio::task::yield_now().await;
    }

    // 连接 A 关闭：撤销该连接的 Attachment（① 不取消执行；不得撤销 B）。
    fixture
        .pool
        .app_binding_leases
        .revoke_connection("connection-a");
    fixture
        .service
        .revoke_attachment(&AttachmentId::new(record_a.record_id.clone()));
    assert!(
        !fixture.session_cancel.is_cancelled(),
        "关闭 App / 连接不得取消同轮执行（会话令牌保持不变）"
    );
    assert!(
        fixture
            .pool
            .app_binding_leases
            .find(&record_b.record_id)
            .is_some(),
        "关闭连接 A 不得撤销连接 B 的 App record"
    );

    // 放行 in-flight 调用：必须正常完成并入账。
    gate.notify_one();
    let outcome_a = in_flight.await.expect("task joins").expect("请求合法");
    assert!(
        matches!(outcome_a, SessionInvocationOutcome::Completed(_)),
        "已 dispatch 的调用必须完成（关闭 App 不中断它），实际: {outcome_a:?}"
    );
    assert_eq!(calls(&fixture), 1);

    // ② 连接 B 的 App 回调仍成功（新审批）。
    let outcome_b = fixture
        .service
        .invoke(fixture.app_request(&record_b, serde_json::json!({"app": "b"})))
        .await
        .expect("请求合法");
    assert!(
        matches!(outcome_b, SessionInvocationOutcome::Completed(_)),
        "其他连接的 App 回调必须仍可执行，实际: {outcome_b:?}"
    );
    assert_eq!(calls(&fixture), 2);

    // ④ 新一轮「turn」不再撤销 App：注册表里 B 仍是 Attached（无 begin_session_turn）。
    assert!(record_b.is_attached(), "新 turn 不得撤销已有 App record");
    fixture.shutdown().await;
}

/// [A13] 执行结束不留回环：record 只被注册表持有，撤销 + 释放后可回收。
#[tokio::test]
async fn invocation_does_not_retain_records_after_it_finishes() {
    let fixture = Fixture::new(
        InteractionResponse::Decisions(vec![
            peri_acp_types::interaction::ApprovalDecision::Approve { source: None },
        ]),
        None,
    )
    .await;
    let record = fixture.attach_record("connection-1");
    let weak = Arc::downgrade(&record);
    let record_id = record.record_id.clone();
    let outcome = fixture
        .service
        .invoke(fixture.app_request(&record, serde_json::json!({})))
        .await
        .expect("请求合法");
    assert!(matches!(outcome, SessionInvocationOutcome::Completed(_)));

    // 撤销是终态：注册表移除条目；此后执行路径不得再持有 record。
    fixture.service.revoke_app(&record_id);
    drop(record);
    assert!(
        weak.upgrade().is_none(),
        "record 不得被 session 级执行路径长期持有（无 Arc 回环）"
    );
    fixture.shutdown().await;
}

/// [A2 · D6] 多附件：detach 一个 View 不影响**其他附件与执行**。
///
/// 同一 record 挂两个 View（两条连接）：① 一个 View 在途执行时 detach 另一个
/// View，不取消执行、不撤销 record；② 被 detach 的 View 不得再发起调用（零 wire）；
/// ③ 仍在的 View 照常执行。
#[tokio::test]
async fn detaching_one_view_leaves_other_attachments_and_execution_intact() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let fixture = Fixture::with_gate(
        InteractionResponse::Decisions(vec![
            peri_acp_types::interaction::ApprovalDecision::Approve { source: None },
        ]),
        None,
        Some(Arc::clone(&gate)),
    )
    .await;
    let record = fixture.attach_record("connection-a");
    let (view_a, epoch_a) = fixture
        .attachment_facts
        .lock()
        .get(&record.record_id)
        .cloned()
        .expect("夹具自检：首附的附件事实必须记录");
    let (view_b, epoch_b) = fixture.attach_second_view(&record, "connection-b", "attachment-b");
    assert_eq!(
        record.attachments().len(),
        2,
        "同一 record 的两个 View 必须都在附件集合内"
    );

    // View A 的调用已 dispatch（wire 上阻塞在 gate）。
    let in_flight = {
        let service = Arc::clone(&fixture.service);
        let request = fixture.app_request_from(
            &record,
            view_a.clone(),
            epoch_a,
            serde_json::json!({"app": "a"}),
        );
        tokio::spawn(async move { service.invoke(request).await })
    };
    while calls(&fixture) == 0 {
        tokio::task::yield_now().await;
    }

    // detach View B（另一条连接的页面卸载 / 连接退出）：只撤该附件的投递资格。
    fixture.service.revoke_attachment(&view_b);
    assert!(
        fixture
            .pool
            .app_binding_leases
            .find(&record.record_id)
            .is_some(),
        "detach 不得撤销 record（裁决 D6：终态撤销只在 close / session / 换代 / 到期）"
    );
    let remaining = record.attachments();
    assert_eq!(remaining.len(), 1, "只摘掉被 detach 的那一个附件");
    assert_eq!(
        remaining[0].attachment_id, view_a,
        "留下的必须是另一个 View"
    );
    assert_eq!(
        remaining[0].connection_epoch, epoch_a,
        "其他附件的纪元不受影响"
    );
    assert!(
        !fixture.session_cancel.is_cancelled(),
        "detach 不得取消会话执行"
    );

    // 放行在途调用：必须正常完成（detach 其他 View 不中断已 dispatch 的执行）。
    gate.notify_one();
    let outcome_a = in_flight.await.expect("task joins").expect("请求合法");
    assert!(
        matches!(outcome_a, SessionInvocationOutcome::Completed(_)),
        "已 dispatch 的执行必须完成，实际: {outcome_a:?}"
    );
    assert_eq!(calls(&fixture), 1);

    // 被 detach 的 View 不得再发起调用：附件不在集合中 ⇒ 显式未执行、零 wire。
    let outcome_b = fixture
        .service
        .invoke(fixture.app_request_from(
            &record,
            view_b.clone(),
            epoch_b,
            serde_json::json!({"app": "b"}),
        ))
        .await
        .expect("请求本身合法");
    assert_eq!(
        outcome_b,
        SessionInvocationOutcome::NotExecuted {
            cause: NotExecutedCause::TargetRevoked
        },
        "已 detach 的附件不再有准入资格"
    );
    assert_eq!(calls(&fixture), 1, "被 detach 的 View 必须零 wire 调用");

    // 仍在的 View 照常执行。
    let outcome_a2 = fixture
        .service
        .invoke(fixture.app_request_from(
            &record,
            view_a,
            epoch_a,
            serde_json::json!({"app": "a2"}),
        ))
        .await
        .expect("请求本身合法");
    assert!(
        matches!(outcome_a2, SessionInvocationOutcome::Completed(_)),
        "未被 detach 的 View 必须仍可执行，实际: {outcome_a2:?}"
    );
    assert_eq!(calls(&fixture), 2);
    fixture.shutdown().await;
}

/// [D4] rerun 目标权威：目标身份来自**服务端 outcome 记录**；跨会话 / 未保留 /
/// 未绑定目标的调用一律显式不可导出（绝不回落客户端字段）。
#[tokio::test]
async fn source_target_exports_target_from_retained_outcome_record() {
    let fixture = Fixture::new(
        InteractionResponse::Decisions(vec![
            peri_acp_types::interaction::ApprovalDecision::Approve { source: None },
        ]),
        None,
    )
    .await;
    let record = fixture.attach_record("connection-1");

    let outcome = fixture
        .service
        .invoke(fixture.app_request(&record, serde_json::json!({"value": 1})))
        .await
        .expect("请求本身合法");
    assert!(matches!(outcome, SessionInvocationOutcome::Completed(_)));

    let retained = fixture
        .service
        .outcomes()
        .latest()
        .expect("outcome 必须入账");
    let target = fixture
        .service
        .source_target(SESSION, &retained.invocation_id)
        .expect("已执行调用必须能从服务端记录导出目标");
    assert_eq!(target.server_id, SERVER);
    assert_eq!(target.wire_tool_name, APP_TOOL, "wire 名是服务端声明名");
    assert_eq!(
        target.effective_tool_name,
        super::super::tool_bridge::effective_mcp_tool_name(SERVER, APP_TOOL),
        "effective 名是目录准入后的名字（两者不可互相代替）"
    );

    // 跨会话：本服务只导出本会话记录（ID 只定位，不授权）。
    assert!(
        fixture
            .service
            .source_target("other-session", &retained.invocation_id)
            .is_none(),
        "跨会话不得导出目标"
    );
    // 未知 / 已淘汰的执行身份：显式无目标。
    assert!(
        fixture
            .service
            .source_target(SESSION, &InvocationId::generate())
            .is_none(),
        "未保留的执行身份不得导出目标"
    );

    // 绑定目标**之前**被拒的调用：记录里没有目标权威，同样不可导出。
    let (attachment_id, epoch) = fixture
        .attachment_facts
        .lock()
        .get(&record.record_id)
        .cloned()
        .expect("夹具自检：附件事实必须记录");
    let rejected = fixture
        .service
        .invoke(fixture.app_request_from(
            &record,
            attachment_id,
            epoch.next(),
            serde_json::json!({}),
        ))
        .await
        .expect("请求本身合法");
    assert_eq!(
        rejected,
        SessionInvocationOutcome::NotExecuted {
            cause: NotExecutedCause::UntrustedOwner
        },
        "纪元不符必须显式未执行"
    );
    let rejected_record = fixture
        .service
        .outcomes()
        .latest()
        .expect("拒绝同样要入账（不静默）");
    assert!(
        rejected_record.target.is_none(),
        "未绑定目标的调用不得伪造目标身份"
    );
    assert!(
        fixture
            .service
            .source_target(SESSION, &rejected_record.invocation_id)
            .is_none(),
        "无目标的记录必须显式不可导出"
    );
    fixture.shutdown().await;
}

/// `map_dispatch_error` 的 typed 事实**只**来自错误本身（裁决 R9）：不再有带外
/// 「无人可答」记录点，两种原因各自如实落账。
#[test]
fn map_dispatch_error_reads_typed_cause_without_side_channel() {
    let unavailable = peri_agent::tools::EffectiveToolError::new(
        EffectiveToolErrorCode::PermissionDenied,
        UnansweredCause::OwnerUnmatched.reason_text(),
    )
    .with_unanswered_cause(UnansweredCause::OwnerUnmatched);
    assert_eq!(
        map_dispatch_error(unavailable, false),
        SessionInvocationOutcome::NotExecuted {
            cause: NotExecutedCause::ApprovalUnavailable(UnansweredCause::OwnerUnmatched)
        },
        "「没有人可以决定」必须落成 ApprovalUnavailable，不得折叠成用户拒绝"
    );

    let rejected = peri_agent::tools::EffectiveToolError::new(
        EffectiveToolErrorCode::UserRejected,
        "用户拒绝",
    );
    assert_eq!(
        map_dispatch_error(rejected, false),
        SessionInvocationOutcome::NotExecuted {
            cause: NotExecutedCause::UserRejected
        },
        "已作出的拒绝决定必须落成 UserRejected"
    );

    // 本端策略拒绝（hook / 分类器 / 审批机制缺失或超时）：既不是用户拒绝，
    // 也不是无人可答，且不得记成「目标已撤销」。
    let policy = peri_agent::tools::EffectiveToolError::new(
        EffectiveToolErrorCode::PermissionDenied,
        "Hook blocked this call",
    );
    assert_eq!(
        map_dispatch_error(policy, false),
        SessionInvocationOutcome::NotExecuted {
            cause: NotExecutedCause::PolicyDenied
        }
    );
}
