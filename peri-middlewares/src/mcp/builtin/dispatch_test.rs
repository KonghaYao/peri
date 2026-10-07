//! `dispatch` 层（实例名 → handler）的 crate 内证据（owner H-01，接线扩展 H-05）。
//!
//! 覆盖口径（主 plan §6 H-01/H-05 行的闸门用例）：工厂覆盖的实例是**精确**集合——注册表里
//! 每个已实现实例各得自己的变体（保留名表与已实现实例表在 W3-B 后逐项一致，**未接线集合为空**），
//! 表外的名字一律 `None`：由 `runtime` 按 `BuiltinSpawnError::HandlerNotWired` 收口，**不得**
//! 静默回退到任一已实现实例。
//!
//! `workspace` 是唯一的例外形状（AW3-11）：它的工厂 arm **无条件**构造——缺 session 级输入
//! 时仍返回 `Workspace` 变体（可见但退化），因此 `None` 在这一支上不代表「未接线」。
//!
//! 工厂选择与输入装配测试位于 `dispatch_factory_tests`；这里保留跨 handler 的 wire 行为。
//! 两条用例的分工：`dispatch_factory_covers_implemented_instances_only` 从注册表派生断言
//! 「未接线集合为空 + 表外名字恒 `None`」；`dispatch_covers_cron_variant` 用带实例
//! 输入的上下文断言各实例拿到**自己的**变体（含「注入的同一份状态对象进了 handler」）以及
//! `workspace` 在**无**输入时的可见但退化。
//!
//! V 矩阵（v4-part-3 sub-plan V §8）第 20/22 行的具名用例在同文件末尾：
//! - [`all_registered_instances_have_handler`]：注册表 → 工厂的一一映射（遍历注册表派生）；
//! - [`builtin_source_propagates`]：cron 的 `ConfigSource::Builtin` 经 overlay →
//!   建传输（三分类）→ status 快照 → `DiscoverMCP` 只读投影保留，transport 分类 = builtin；
//! - [`call_tool_smoke_and_effective_bridge_round_trip`]（原第 22 行 `call_tool_uses_shared_result_mapping`，
//!   改名以匹配精炼后的观测面）：跨实例烟测 + effective 桥一致性。第 23 行
//!   `call_tool_error_text_is_fixed_and_redacted` 的规则细节（固定脱敏文本、逐类变体）
//!   已由各 package 的 `server_test.rs` 覆盖，宿主侧重复证据删除（宿主无映射实现）。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use peri_acp_types::builtin_mcp::{
    find, is_reserved_instance_name, BUILTIN_MCP_INSTANCES, BUILTIN_RESERVED_INSTANCE_NAMES,
};
use peri_acp_types::event::BackgroundTaskResult;
use peri_acp_types::plugin::{ConfigSource, McpServerConfig};
use peri_acp_types::tasks::{BgTaskKind, TaskManager};
use peri_agent::agent::async_tasks::TaskManager as ConcreteTaskManager;
use peri_agent::tools::{BaseTool, ToolContext};
use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ClientRequest, CustomRequest,
        ErrorCode, ServerResult,
    },
    service::{Peer, RoleClient, ServiceError},
    ServerHandler,
};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::{builtin_server_handler, BuiltinServerHandler};
use crate::mcp::apps::McpCapabilityProfile;
use crate::mcp::builtin::context::{BuiltinInstanceContext, CronInstanceInput};
use crate::mcp::builtin::runtime::{
    spawn_builtin_transport_with_handler, BuiltinInstanceSupervisor, BuiltinServerExit,
    TickCloseOutcome, BUILTIN_CONVERGE_TIMEOUT,
};
use crate::mcp::builtin::{apply_builtin_overlay, BuiltinInjectionPolicy};
use crate::mcp::client::{
    serve_client_auto, ClientStatus, McpClientHandle, McpClientPool, McpServiceWrapper, OAuthStatus,
};
use crate::mcp::discover_tool::DiscoverMCPTool;
use crate::mcp::tool_bridge::{build_deferred_tool_bridges, McpToolBridge};
use crate::mcp::transport::{require_known_builtin_instance, TransportConfig, TransportKind};
use crate::mcp::ToolCallError;
use peri_mcp_cron::{CronScheduler, CronTrigger};
use peri_mcp_workspace::{
    ResourceRoot, ResourceScope, WorkspaceInstanceInput, WorkspaceMcpServer,
    WorkspaceResourcesInput,
};

mod invocation_fixture {
    use std::sync::Arc;

    use peri_acp_types::identity::AttemptId;
    use peri_acp_types::messages::{BaseMessage, ToolCallRequest};
    use peri_acp_types::session::{MessagePolicy, TurnId};
    use peri_acp_types::session_resources::work::*;
    use peri_acp_types::session_resources::{
        ControlAttempt, FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResources,
    };
    use peri_acp_types::store::PersistedPayload;
    use peri_acp_types::tools::ToolContext;
    use peri_acp_types::workspace::SessionBinding;
    use peri_resources::sessions::SessionResourcesImpl;
    use serde_json::Value;
    use sha2::{Digest, Sha256};

    pub(super) const FIXTURE_SESSION_ID: &str = "builtin-dispatch-session";

    pub(super) struct InvocationFixture {
        _directory: tempfile::TempDir,
        resources: Arc<dyn SessionResources>,
        intents: Vec<Arc<InvocationIntent>>,
        target: WorkTarget,
    }

    impl InvocationFixture {
        pub(super) async fn new_calls(calls: &[(&str, Value)]) -> Self {
            let directory = tempfile::tempdir().unwrap();
            let resources: Arc<dyn SessionResources> = Arc::new(
                SessionResourcesImpl::open(directory.path().join("invocations.db"))
                    .await
                    .unwrap(),
            );
            let workspace = resources.resolve_workspace(directory.path()).await.unwrap();
            resources
                .create_session(&NewSession {
                    thread_id: FIXTURE_SESSION_ID.into(),
                    created_at: peri_time::now_utc_rfc3339(),
                    meta: NewSessionMeta {
                        title: None,
                        cwd: workspace.cwd.to_string_lossy().into_owned(),
                        parent_thread_id: None,
                        hidden: false,
                        cancel_policy: Default::default(),
                        snapshot_at_message_id: None,
                    },
                    binding: SessionBinding::from_workspace(&workspace),
                    frozen: FrozenSnapshotBytes::new("{\"version\":1}"),
                })
                .await
                .unwrap();
            let intents = calls
                .iter()
                .enumerate()
                .map(|(index, (tool, input))| {
                    let arguments_json = serde_json::to_string(input).unwrap();
                    let digest = format!("{:x}", Sha256::digest(arguments_json.as_bytes()));
                    Arc::new(InvocationIntent {
                        invocation_id: format!("invocation-{index}"),
                        tool_call_id: format!("call-{index}"),
                        tool_name: (*tool).into(),
                        arguments_digest: digest.clone(),
                        effective_tool_name: (*tool).into(),
                        effective_arguments_digest: digest,
                        effective_arguments_json: arguments_json.clone(),
                        arguments_json,
                        owner_identity: "fixture-owner".into(),
                        scope_id: FIXTURE_SESSION_ID.into(),
                        scope_epoch: None,
                        authorization_ref: "fixture-policy".into(),
                        recovery_locator: "fixture-link".into(),
                    })
                })
                .collect::<Vec<_>>();
            let mut fixture = Self {
                _directory: directory,
                resources,
                intents,
                target: WorkTarget {
                    work_id: "act-work".into(),
                    expected_work_revision: 0,
                },
            };
            fixture
                .apply(
                    "publish",
                    WorkAction::PublishDelivery {
                        delivery: PublishDelivery {
                            delivery_id: "delivery".into(),
                            event: WorkEvent {
                                producer_namespace: "fixture".into(),
                                event_id: "event".into(),
                                event_kind: "request".into(),
                                causation_id: None,
                                content: WorkPayload::from_payload(&PersistedPayload::Message(
                                    BaseMessage::human("run"),
                                ))
                                .unwrap(),
                            },
                            purpose: DeliveryPurpose::UserInput,
                            policy: MessagePolicy::ensure_processing(),
                        },
                    },
                )
                .await;
            let snapshot = fixture.snapshot().await;
            fixture
                .apply(
                    "admit",
                    WorkAction::RegisterAdmission {
                        admission: WorkAdmission {
                            session_id: FIXTURE_SESSION_ID.into(),
                            admission_id: "admission".into(),
                            instance_id: "fixture-sdk".into(),
                            generation_id: "fixture-generation".into(),
                            lifecycle: 1,
                            control_generation: snapshot.control.control_generation,
                            work_id: snapshot.candidates[0].work_id.clone(),
                            work_revision: snapshot.candidates[0].work_revision,
                            execution: ControlAttempt {
                                turn_id: TurnId::new(),
                                attempt_id: AttemptId::new(),
                            },
                        },
                    },
                )
                .await;
            let snapshot = fixture.snapshot().await;
            fixture
                .apply(
                    "claim",
                    WorkAction::ClaimBatch {
                        guard: Self::guard(&snapshot),
                        batch_id: snapshot.candidates[0].work_id.clone(),
                        delivery_ids: snapshot.candidates[0].delivery_ids.clone(),
                    },
                )
                .await;
            let snapshot = fixture.snapshot().await;
            fixture
                .apply(
                    "reason",
                    WorkAction::BeginReason {
                        guard: Self::guard(&snapshot),
                        target: Self::target(&snapshot, &snapshot.candidates[0].work_id),
                        request_id: "request".into(),
                        request: ReasonRequest {
                            serialized_request: "{}".into(),
                            request_digest: format!("{:x}", Sha256::digest(b"{}")),
                            model_ref: "fixture-model".into(),
                            authorization_ref: "fixture-policy".into(),
                        },
                    },
                )
                .await;
            let snapshot = fixture.snapshot().await;
            fixture
                .apply(
                    "response",
                    WorkAction::CommitReasonResponseAndDispatchIntent {
                        guard: Self::guard(&snapshot),
                        target: Self::target(&snapshot, &snapshot.candidates[0].work_id),
                        request_id: "request".into(),
                        response: WorkPayload::from_payload(&PersistedPayload::Message(
                            BaseMessage::ai_with_tool_calls(
                                "run",
                                fixture
                                    .intents
                                    .iter()
                                    .enumerate()
                                    .map(|(index, intent)| {
                                        ToolCallRequest::new(
                                            &intent.tool_call_id,
                                            &intent.tool_name,
                                            calls[index].1.clone(),
                                        )
                                    })
                                    .collect(),
                            ),
                        ))
                        .unwrap(),
                        dispatch_intents: fixture
                            .intents
                            .iter()
                            .map(|intent| (**intent).clone())
                            .collect(),
                        next_work_id: Some("act-work".into()),
                    },
                )
                .await;
            for (index, intent) in fixture.intents.iter().enumerate() {
                let snapshot = fixture.snapshot().await;
                fixture
                    .apply(
                        &format!("dispatch-{index}"),
                        WorkAction::BeginDispatch {
                            guard: Self::guard(&snapshot),
                            target: Self::target(&snapshot, "act-work"),
                            invocation_id: intent.invocation_id.clone(),
                        },
                    )
                    .await;
            }
            fixture.target = Self::target(&fixture.snapshot().await, "act-work");
            fixture
        }

        pub(super) fn context(&self, index: usize) -> ToolContext<'_> {
            let intent = &self.intents[index];
            let mut context = ToolContext::new(&[], "/tmp")
                .with_session_identity(FIXTURE_SESSION_ID, "fixture-turn")
                .with_work_invocation(
                    1,
                    intent.clone(),
                    self.target.clone(),
                    self.resources.clone(),
                );
            context.invocation_id = Some(intent.invocation_id.clone());
            context
        }

        async fn snapshot(&self) -> WorkSnapshot {
            self.resources
                .load_session_work(&WorkQuery {
                    session_id: FIXTURE_SESSION_ID.into(),
                    limit: 64,
                })
                .await
                .unwrap()
        }

        async fn apply(&self, mutation_id: &str, action: WorkAction) {
            let receipt = self
                .resources
                .apply_work_mutation(&WorkCommand {
                    session_id: FIXTURE_SESSION_ID.into(),
                    recipient_lifecycle: 1,
                    mutation_id: mutation_id.into(),
                    action,
                })
                .await
                .unwrap();
            assert_eq!(receipt.decision, WorkDecision::Accepted);
        }

        fn guard(snapshot: &WorkSnapshot) -> WorkGuard {
            WorkGuard {
                expected_revision: snapshot.state.revision,
                expected_control_generation: snapshot.control.control_generation,
                execution: snapshot.control.attempt.clone().unwrap(),
            }
        }

        fn target(snapshot: &WorkSnapshot, work_id: &str) -> WorkTarget {
            WorkTarget {
                work_id: work_id.into(),
                expected_work_revision: snapshot.state.works[work_id].revision,
            }
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// V 矩阵第 20 行：注册表 → 工厂映射；builtin 身份经 discover / status 传播
// ══════════════════════════════════════════════════════════════════════════════

/// wave 2 的**被验实例**（本文件三处断言的被验对象）。
///
/// 只固定实例身份；工具名、键、数量、effective name 一律读注册表（`find` / `tools`），
/// 不出现第二份清单——注册表增删工具时本文件的循环自动跟随。
const WAVE2_INSTANCES: [&str; 1] = ["cron"];

// ══════════════════════════════════════════════════════════════════════════════
// V 矩阵第 22/23 行：IF-D14 结果映射（真 handler + effective 桥 + 固定脱敏文本）
// ══════════════════════════════════════════════════════════════════════════════

/// 非法 cron 表达式（1 段，解析必然失败）：逼出 `CronError::InvalidExpression`。
const INVALID_EXPRESSION: &str = "not-a-cron-expression";
/// 合法 cron 表达式（填充任务上限用）。
const VALID_EXPRESSION: &str = "*/5 * * * *";
/// 用户 prompt 的可辨认形状串：任何失败文本里**不得**出现。
const LEAK_PROMPT: &str = "SENTINEL-PROMPT-must-not-leak";
/// 路径形状串（§9 规则 7：路径不入模型面文本）。
const LEAK_PATH_MARKER: &str = "secret-marker";
/// 无路由扩展名（不得回传给模型）。
const LEAK_EXTENSION: &str = "zzz";

/// `serve_client_auto` 侧握手上界（同进程链路）。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
/// client 侧关闭上界（收尾共用）。
const CLOSE_TIMEOUT: Duration = Duration::from_millis(500);

/// 交叉面夹具：一份齐备的实例上下文（cron 真实 scheduler）+ 观察端。
///
/// `web` / `artifact` 不需要额外输入（artifact 的解析根是 `ctx.cwd`），因此同一份上下文可
/// 喂给四个已实现实例的工厂。
struct CrossFixture {
    ctx: BuiltinInstanceContext,
    scheduler: Arc<Mutex<CronScheduler>>,
    /// cron 触发通道接收端（保持存活，使发送端始终有对端；本文件不驱动 tick）。
    _cron_triggers: mpsc::UnboundedReceiver<CronTrigger>,
    /// 夹具临时目录（仅靠 `Drop` 持有生命周期）。
    _dir: tempfile::TempDir,
}

impl CrossFixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let (trigger_tx, cron_triggers) = mpsc::unbounded_channel();
        let scheduler = Arc::new(Mutex::new(CronScheduler::new(trigger_tx)));
        let cwd = dir.path().to_string_lossy().to_string();
        let ctx = BuiltinInstanceContext::new(cwd).with_cron(CronInstanceInput {
            scheduler: Arc::clone(&scheduler),
            tick_enabled: false,
        });
        Self {
            ctx,
            scheduler,
            _cron_triggers: cron_triggers,
            _dir: dir,
        }
    }
}

/// 一条已握手的 builtin 链路：client service + 本代关闭所有权。
struct Pair {
    service: McpServiceWrapper,
    supervisor: BuiltinInstanceSupervisor,
}

impl Pair {
    fn peer(&self) -> Peer<RoleClient> {
        self.service.peer().clone()
    }

    /// 夹具收尾：释放 client（server 侧读到 EOF）→ 本代监督者按冻结顺序有界收敛。
    async fn shutdown(mut self) {
        let _ = self.service.close_with_timeout(CLOSE_TIMEOUT).await;
        let outcome = self.supervisor.close(BUILTIN_CONVERGE_TIMEOUT).await;
        assert!(
            matches!(outcome.tick, TickCloseOutcome::NotSpawned),
            "夹具装配恒不挂 tick（A32：tick 归 pool 的唯一 spawn 点）"
        );
        assert!(
            matches!(outcome.server, BuiltinServerExit::Quit(_)),
            "server task 必须在有界等待内正常收敛，实际：{:?}",
            outcome.server
        );
    }
}

/// 经 **dispatch 工厂**取 handler（`None` = 未接线 / 输入缺失 ⇒ 直接断言失败），装配真实
/// 同进程链路并由生产 `serve_client_auto` 握手——实例名与注册表一致。
async fn connect_via_dispatch(instance: &str, ctx: &BuiltinInstanceContext) -> Pair {
    let handler = builtin_server_handler(instance, ctx)
        .unwrap_or_else(|| panic!("{instance}: 工厂必须给出 handler（本夹具输入齐备）"));
    connect_handler(instance, handler).await
}

/// 以**调用方给出的** handler 装配真实同进程链路并由生产 `serve_client_auto` 握手。
///
/// W1 资源面证据用本入口绕过工厂（那时夹具直接把 handler 交给链路）；W4a 起生产工厂 arm
/// 已按 `ctx.workspace_resources` 装载资源面，装资源输入的用例走 [`connect_via_dispatch`]。
async fn connect_handler(instance: &str, handler: BuiltinServerHandler) -> Pair {
    let transport = spawn_builtin_transport_with_handler(instance, handler);
    let (io, supervisor) = transport.into_parts();
    let service = serve_client_auto(io, &McpCapabilityProfile::disabled(), HANDSHAKE_TIMEOUT)
        .await
        .expect("builtin 握手不得超时（同进程链路）")
        .expect("builtin 握手不得失败");
    Pair {
        service,
        supervisor,
    }
}

/// `tools/call` 请求（`arguments` 必须是 JSON object；缺省 = 空对象）。
fn call(name: &str, arguments: Value) -> CallToolRequestParams {
    CallToolRequestParams::new(name.to_string())
        .with_arguments(arguments.as_object().cloned().unwrap_or_default())
}

/// 结果里的首个文本块。
fn first_text(result: &CallToolResult) -> Option<String> {
    result.content.iter().find_map(|block| match block {
        rmcp::model::ContentBlock::Text(text) => Some(text.text.clone()),
        _ => None,
    })
}

/// 断言响应是 `Complete` 并取出结果（IF-D14 的失败形态**不是** `Err`）。
fn complete(response: CallToolResponse) -> CallToolResult {
    match response {
        CallToolResponse::Complete(result) => result,
        other => panic!("期望 Complete 结果，实际：{other:?}"),
    }
}

/// V 矩阵第 22 行（**宿主面**，原具名用例 `call_tool_uses_shared_result_mapping`）：cron
/// 实例经 dispatch 工厂 + 真实 wire 的**烟测**（成功 / 业务失败两形态的协议外观），
/// 以及 effective 名字（模型面）经生产 `McpToolBridge` 打回同一条真实链路的**桥接一致性**
/// （成功文本 == 线路文本；失败转 `ApplicationFailed`，其封装的 reason == 线路失败文本）。
///
/// 映射**规则本身**（固定脱敏文本、未知工具 → `invalid_params`、业务细节不入模型面）由各
/// package 的 `server_test.rs` 逐实例覆盖，本文件不重复断言规则字面量：宿主已无映射实现
/// （dispatch 的 `call_tool` 把请求原样转给 package handler），规则细节在这里没有第二个
/// 可失败面。
#[tokio::test]
async fn call_tool_smoke_and_effective_bridge_round_trip() {
    let fixture = CrossFixture::new();
    let cases = [Wave2CallCase {
        instance: "cron",
        success_tool: "cron_list",
        success_input: json!({}),
        fail_tool: "cron_register",
        fail_input: json!({ "expression": INVALID_EXPRESSION, "prompt": LEAK_PROMPT }),
    }];

    for case in cases {
        // 注册表前置：两个工具名必须是本实例声明的原始名（名字不凭空写）。
        let declared =
            find(case.instance).unwrap_or_else(|| panic!("{}: 实例必须在注册表内", case.instance));
        for name in [case.success_tool, case.fail_tool] {
            assert!(
                declared.tools.iter().any(|tool| tool.original_name == name),
                "{}/{name}: 必须是注册表声明的原始工具名",
                case.instance
            );
        }
        let effective_of = |name: &str| {
            declared
                .tools
                .iter()
                .find(|tool| tool.original_name == name)
                .unwrap_or_else(|| panic!("{}: 注册表必须声明 {name}", case.instance))
                .effective_name
        };

        // 真实链路：dispatch 工厂 → 真实 transport → 生产 client 握手。
        let pair = connect_via_dispatch(case.instance, &fixture.ctx).await;
        let peer = pair.peer();

        // ① 成功形态：`Complete(success 结果)`，文本非空。
        let success = complete(
            peer.call_tool_once(call(case.success_tool, case.success_input.clone()))
                .await
                .expect("成功路径必须是协议成功"),
        );
        assert_eq!(
            success.is_error,
            Some(false),
            "{}: 成功形态必须是 success 结果",
            case.instance
        );
        let success_text = first_text(&success).expect("成功结果必须有文本块");
        assert!(
            !success_text.is_empty(),
            "{}: 成功文本不得为空",
            case.instance
        );

        // ② 业务 Err 形态 smoke：`Complete(error 结果)` + 非空文本（规则细节见用例文档）。
        let failure = complete(
            peer.call_tool_once(call(case.fail_tool, case.fail_input.clone()))
                .await
                .expect("业务失败仍走协议成功（is_error 承载语义）"),
        );
        assert_eq!(
            failure.is_error,
            Some(true),
            "{}: 业务失败必须是 error 结果，不是 Err",
            case.instance
        );
        let failure_text = first_text(&failure).expect("error 结果必须有文本块");
        assert!(
            !failure_text.is_empty(),
            "{}: 失败文本不得为空",
            case.instance
        );

        // ③ effective 调用桥接：模型面名字（注册表冻结的 effective name）经生产
        //    `build_deferred_tool_bridges` 打回**同一条**真实链路的 handler。
        let pool = bridge_pool(case.instance, &peer).await;
        let bridges = build_deferred_tool_bridges(&pool);
        let invocation_fixture = invocation_fixture::InvocationFixture::new_calls(&[
            (effective_of(case.success_tool), case.success_input.clone()),
            (effective_of(case.fail_tool), case.fail_input.clone()),
        ])
        .await;
        let success_bridge = bridge_of(&bridges, effective_of(case.success_tool));
        let bridged_success = success_bridge
            .invoke(case.success_input.clone(), invocation_fixture.context(0))
            .await
            .unwrap_or_else(|error| {
                panic!("{}: effective 桥的成功路径不得失败：{error}", case.instance)
            });
        assert_eq!(
            bridged_success, success_text,
            "{}: effective 桥的成功文本必须与 handler 路径逐字一致",
            case.instance
        );

        let failure_bridge = bridge_of(&bridges, effective_of(case.fail_tool));
        let bridged_error = failure_bridge
            .invoke(case.fail_input.clone(), invocation_fixture.context(1))
            .await
            .expect_err("工具级失败经桥必须 Err（桥把 error 结果转成 ApplicationFailed）");
        match bridged_error.downcast_ref::<peri_acp_types::tools::EffectiveToolError>() {
            Some(error) => {
                assert_eq!(
                    error.code,
                    peri_acp_types::tools::EffectiveToolErrorCode::ApplicationFailed
                );
                assert_eq!(
                    error.message,
                    ToolCallError::CallFailed {
                        server: case.instance.into(),
                        tool: case.fail_tool.into(),
                        reason: failure_text.into(),
                    }
                    .to_string(),
                    "{}: 桥接失败原因必须与线路失败文本逐字一致",
                    case.instance
                );
            }
            other => panic!(
                "{}: 期望 ApplicationFailed（reason = 线路失败文本），实际 {other:?}",
                case.instance
            ),
        }

        println!(
            "[W2 dispatch/source] instance={} success=1 error=1 bridged=2",
            case.instance
        );
        pair.shutdown().await;
    }
}

/// 一个 wave 2 实例的烟测用例输入（成功 / 业务失败两形态，各配桥接对照；工具名必须能在注册表里查到）。
struct Wave2CallCase {
    instance: &'static str,
    success_tool: &'static str,
    success_input: Value,
    fail_tool: &'static str,
    fail_input: Value,
}

/// 把一条**已握手**的真实 builtin 链路登记成 pool 里的 Connected 句柄（形状同生产
/// `run_initialize` 的提交点：`peer` / `tools` 取自真实往返），再经生产
/// `build_deferred_tool_bridges` 取 effective name 的 `McpToolBridge`。
///
/// 句柄 `name` 必须是实例名：bridge 的 effective name 由 `(server_name, 原始工具名)` 计算
/// （`McpToolBridge::new` 用 `client.name`），因此这一步同时验证注册表冻结字面量与生产计算
/// 一致。
async fn bridge_pool(instance: &str, peer: &Peer<RoleClient>) -> Arc<McpClientPool> {
    let pool = Arc::new(McpClientPool::new_empty());
    let manager: Arc<dyn TaskManager> = Arc::new(ConcreteTaskManager::new());
    pool.bind_session_task_manager(invocation_fixture::FIXTURE_SESSION_ID, &manager);
    let tools = peer
        .list_all_tools()
        .await
        .expect("真实 tools/list 必须成功");
    pool.clients.write().insert(
        instance.to_string(),
        Arc::new(McpClientHandle {
            name: instance.to_string(),
            version: None,
            cache_version: None,
            peer: Some(peer.clone()),
            tools,
            resources: vec![],
            status: ClientStatus::Connected,
            oauth_status: OAuthStatus::default(),
            source: None,
            url: None,
            skills_capable: false,
        }),
    );
    pool
}

/// 按注册表冻结的 effective name 取真实桥（缺桥 ⇒ 断言失败）。
fn bridge_of<'a>(bridges: &'a [McpToolBridge], effective_name: &str) -> &'a McpToolBridge {
    bridges
        .iter()
        .find(|bridge| bridge.name() == effective_name)
        .unwrap_or_else(|| {
            panic!(
                "{effective_name}: 真实桥必须存在（tools/list 快照 + 注册表声明）；实际：{:?}",
                bridges
                    .iter()
                    .map(|bridge| bridge.name())
                    .collect::<Vec<_>>()
            )
        })
}

#[path = "dispatch_resource_wire_test.rs"]
mod dispatch_resource_wire_tests;

#[path = "dispatch_factory_test.rs"]
mod dispatch_factory_tests;
