//! Workflow 启动入口审批与提示词可见性测试。
//!
//! 从 `mod_test.rs` 拆出以控制单文件行数；挂载在 `tests` 下，测试过滤路径为
//! `permission::tests::workflow::...`。

use super::super::async_trait;
use super::super::default_requires_approval;
use super::super::AgentError;
use super::super::ApprovalDecision;
use super::super::Arc;
use super::super::AutoClassifier;
use super::super::Classification;
use super::super::InteractionContext;
use super::super::InteractionResponse;
use super::super::PermissionMiddleware;
use super::super::PermissionMode;
use super::super::SharedPermissionMode;
use super::super::ToolCall;
use super::super::ToolRejectionCause;
use super::super::TrustedInteractionOwner;
use super::super::UserInteractionBroker;
use peri_agent::middleware::Middleware as _;

// ─── Workflow 启动入口审批（真实 `before_tool` 路径）──────────────────────
//
// 判定函数为真只是必要条件：以下用例用 fake broker + 声明表 effective name 走
// 真实中间件入口，锁定「执行前产生审批、拒绝即不进入执行」的行为。
// `MiddlewareChain::run_before_tool` 在 `before_tool` 返回 Err 时以 `?` 中止，
// 不会到达工具分派（`peri-agent/src/middleware/chain.rs`）。

/// 记录审批请求的 broker（可批准或拒绝）；`requested_tools` 是审批通道被触发的证据。
struct RecordingBroker {
    approve: bool,
    requested: std::sync::Mutex<Vec<String>>,
}

impl RecordingBroker {
    fn new(approve: bool) -> Arc<Self> {
        Arc::new(Self {
            approve,
            requested: std::sync::Mutex::new(Vec::new()),
        })
    }

    fn requested_tools(&self) -> Vec<String> {
        self.requested.lock().expect("审批记录锁未中毒").clone()
    }
}

#[async_trait]
impl UserInteractionBroker for RecordingBroker {
    async fn request(
        &self,
        _owner: TrustedInteractionOwner,
        ctx: InteractionContext,
    ) -> InteractionResponse {
        match ctx {
            InteractionContext::Approval { items } => {
                self.requested
                    .lock()
                    .expect("审批记录锁未中毒")
                    .extend(items.iter().map(|item| item.tool_name.clone()));
                let decision = if self.approve {
                    ApprovalDecision::Approve { source: None }
                } else {
                    ApprovalDecision::Reject {
                        reason: "用户拒绝".to_string(),
                        source: None,
                    }
                };
                InteractionResponse::Decisions(items.iter().map(|_| decision.clone()).collect())
            }
            _ => InteractionResponse::Decisions(vec![]),
        }
    }
}

/// 记录分类请求的分类器；`calls` 用于断言 Auto 模式不重复判定。
struct RecordingClassifier {
    result: Classification,
    calls: std::sync::Mutex<Vec<String>>,
}

impl RecordingClassifier {
    fn new(result: Classification) -> Arc<Self> {
        Arc::new(Self {
            result,
            calls: std::sync::Mutex::new(Vec::new()),
        })
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("分类记录锁未中毒").clone()
    }
}

#[async_trait]
impl AutoClassifier for RecordingClassifier {
    async fn classify(&self, tool_name: &str, _tool_input: &serde_json::Value) -> Classification {
        self.calls
            .lock()
            .expect("分类记录锁未中毒")
            .push(tool_name.to_string());
        self.result
    }
}

fn mw_with_mode_and_broker(
    mode: PermissionMode,
    broker: Arc<RecordingBroker>,
    classifier: Option<Arc<dyn AutoClassifier>>,
) -> PermissionMiddleware {
    PermissionMiddleware::with_shared_mode(
        broker,
        default_requires_approval,
        SharedPermissionMode::new(mode),
        classifier,
    )
}

fn workflow_model_name(original: &str) -> &'static str {
    peri_acp_types::builtin_mcp::model_tool_name_of("workflow", original)
        .expect("workflow fixture tool must be declared")
}

/// Workflow 启动调用的模型面名字取自真实 builtin 声明表。
fn workflow_start_call() -> ToolCall {
    ToolCall {
        id: "workflow-start".to_string(),
        name: workflow_model_name("Workflow").to_string(),
        input: serde_json::json!({"script": "..."}),
    }
}

fn workflow_tool_call(original: &str, input: serde_json::Value) -> ToolCall {
    ToolCall {
        id: format!("workflow-{original}"),
        name: workflow_model_name(original).to_string(),
        input,
    }
}

#[tokio::test]
async fn workflow_start_requires_approval_in_default_mode() {
    let broker = RecordingBroker::new(true);
    let mw = mw_with_mode_and_broker(PermissionMode::Default, broker.clone(), None);
    let mut state = super::approval_state();
    let tc = workflow_start_call();

    let result = mw.before_tool(&mut state, &tc).await.unwrap();

    assert_eq!(result.name, tc.name);
    assert_eq!(
        broker.requested_tools(),
        vec![tc.name.clone()],
        "Workflow 启动必须在执行前进入审批通道（恰好一次）"
    );
}

#[tokio::test]
async fn workflow_start_rejection_blocks_execution_without_partial_start() {
    let broker = RecordingBroker::new(false);
    let mw = mw_with_mode_and_broker(PermissionMode::Default, broker.clone(), None);
    let mut state = super::approval_state();
    let tc = workflow_start_call();

    let result = mw.before_tool(&mut state, &tc).await;

    // 只有 Ok 才会进入工具分派；Err 即调度前终止，不存在已启动的子链。
    assert!(
        result.is_err(),
        "未获批准的 Workflow 启动不得进入执行，实际: {result:?}"
    );
    assert!(
        matches!(
            result,
            Err(AgentError::ToolRejected {
                cause: ToolRejectionCause::UserRejected,
                ..
            })
        ),
        "拒绝应保留 typed cause = UserRejected"
    );
    assert_eq!(
        broker.requested_tools(),
        vec![tc.name.clone()],
        "拒绝只弹一次审批"
    );
}

#[tokio::test]
async fn workflow_start_requires_approval_in_accept_edits_mode() {
    let broker = RecordingBroker::new(true);
    let mw = mw_with_mode_and_broker(PermissionMode::AcceptEdit, broker.clone(), None);
    let mut state = super::approval_state();

    // 对照：AcceptEdits 下 Write 仍免审批（编辑工具自动放行）。
    let write = super::make_tool_call("Write");
    mw.before_tool(&mut state, &write).await.unwrap();
    assert!(
        broker.requested_tools().is_empty(),
        "AcceptEdits 下 Write 不得进入审批"
    );

    // Workflow 不是编辑工具 → 仍必须审批。
    let tc = workflow_start_call();
    let result = mw.before_tool(&mut state, &tc).await.unwrap();
    assert_eq!(result.name, tc.name);
    assert_eq!(broker.requested_tools(), vec![tc.name.clone()]);
}

#[tokio::test]
async fn workflow_start_auto_mode_deny_does_not_prompt_or_execute() {
    let broker = RecordingBroker::new(true);
    let classifier = RecordingClassifier::new(Classification::Deny);
    let mw = mw_with_mode_and_broker(
        PermissionMode::AutoMode,
        broker.clone(),
        Some(classifier.clone()),
    );
    let mut state = super::approval_state();
    let tc = workflow_start_call();

    let result = mw.before_tool(&mut state, &tc).await;

    assert!(matches!(
        result,
        Err(AgentError::ToolRejected {
            cause: ToolRejectionCause::PolicyDenied,
            ..
        })
    ));
    assert_eq!(
        classifier.calls(),
        vec![tc.name.clone()],
        "分类器对 Workflow 只判定一次"
    );
    assert!(
        broker.requested_tools().is_empty(),
        "分类器拒绝后不得重复弹审批"
    );
}

#[tokio::test]
async fn workflow_start_auto_mode_unsure_falls_back_to_single_approval() {
    let broker = RecordingBroker::new(true);
    let classifier = RecordingClassifier::new(Classification::Unsure);
    let mw = mw_with_mode_and_broker(
        PermissionMode::AutoMode,
        broker.clone(),
        Some(classifier.clone()),
    );
    let mut state = super::approval_state();
    let tc = workflow_start_call();

    let result = mw.before_tool(&mut state, &tc).await.unwrap();

    assert_eq!(result.name, tc.name);
    assert_eq!(classifier.calls(), vec![tc.name.clone()]);
    assert_eq!(
        broker.requested_tools(),
        vec![tc.name.clone()],
        "Unsure 恰好回退一次人工审批"
    );
}

#[tokio::test]
async fn workflow_start_auto_mode_allow_does_not_prompt() {
    let broker = RecordingBroker::new(false);
    let classifier = RecordingClassifier::new(Classification::Allow);
    let mw = mw_with_mode_and_broker(
        PermissionMode::AutoMode,
        broker.clone(),
        Some(classifier.clone()),
    );
    let mut state = super::approval_state();
    let tc = workflow_start_call();

    let result = mw.before_tool(&mut state, &tc).await.unwrap();

    assert_eq!(result.name, tc.name);
    assert_eq!(classifier.calls(), vec![tc.name.clone()]);
    assert!(
        broker.requested_tools().is_empty(),
        "Allow 后不得再次弹审批"
    );
}

/// 非启动类 workflow 工具（Status/Kill/Claim 及宿主回填通道）不得被误判敏感：
/// 在 Default 模式下经过真实入口也不会触发 broker。
#[tokio::test]
async fn workflow_non_start_tools_do_not_enter_approval() {
    let broker = RecordingBroker::new(false);
    let mw = mw_with_mode_and_broker(PermissionMode::Default, broker.clone(), None);
    let mut state = super::approval_state();

    let declared = peri_acp_types::builtin_mcp::find("workflow").expect("workflow 实例应有声明");
    let mut checked = 0;
    {
        let matching_tools = declared
            .tools
            .iter()
            .filter(|tool| tool.original_name != "Workflow");
        for tool in matching_tools {
            let tc = workflow_tool_call(tool.original_name, serde_json::json!({}));
            let result = mw.before_tool(&mut state, &tc).await;
            assert!(
                result.is_ok(),
                "`{}` 不应触发审批，实际: {result:?}",
                tool.legacy_name
            );
            assert_eq!(result.unwrap().name, tc.name);
            checked += 1;
        }
    }
    assert_eq!(
        checked,
        declared.tools.len() - 1,
        "应逐个覆盖除 Workflow 外的全部声明工具"
    );
    assert!(
        broker.requested_tools().is_empty(),
        "非启动类 workflow 工具不得触发审批"
    );
}

/// deferred 工具经 ToolInvoke（及旧名 alias）的兼容路径同样按目标名判定。
#[tokio::test]
async fn workflow_start_via_tool_invoke_requires_approval() {
    let broker = RecordingBroker::new(false);
    let mw = mw_with_mode_and_broker(PermissionMode::Default, broker.clone(), None);
    let target = workflow_model_name("Workflow");

    for (wrapper_name, input) in [
        (
            crate::tool_search::TOOL_INVOKE_NAME,
            serde_json::json!({"name": target, "arguments": {"script": "..."}}),
        ),
        (
            crate::tool_search::EXECUTE_EXTRA_TOOL_NAME,
            serde_json::json!({"tool_name": target, "params": {"script": "..."}}),
        ),
    ] {
        let mut state = super::approval_state();
        let tc = ToolCall {
            id: "workflow-extra".to_string(),
            name: wrapper_name.to_string(),
            input,
        };

        let result = mw.before_tool(&mut state, &tc).await;

        assert!(matches!(
            result,
            Err(AgentError::ToolRejected {
                cause: ToolRejectionCause::UserRejected,
                ..
            })
        ));
    }

    assert_eq!(
        broker.requested_tools(),
        vec![
            crate::tool_search::TOOL_INVOKE_NAME.to_string(),
            crate::tool_search::EXECUTE_EXTRA_TOOL_NAME.to_string(),
        ],
        "包装调用应进入审批（目标名 {target} 驱动判定）"
    );
}

/// 畸形代理调用（字段冲突/缺参）必须 fail-closed 进入审批，不能按目标名静默放行。
#[tokio::test]
async fn malformed_tool_invoke_requires_approval_fail_closed() {
    let broker = RecordingBroker::new(false);
    let mw = mw_with_mode_and_broker(PermissionMode::Default, broker.clone(), None);

    for input in [
        serde_json::json!({"name": "Read", "tool_name": "Write", "arguments": {}}),
        serde_json::json!({"name": "Read"}),
        serde_json::json!({"name": "", "arguments": {}}),
    ] {
        let mut state = super::approval_state();
        let tc = ToolCall {
            id: "malformed-proxy".to_string(),
            name: crate::tool_search::TOOL_INVOKE_NAME.to_string(),
            input,
        };
        let result = mw.before_tool(&mut state, &tc).await;
        assert!(matches!(
            result,
            Err(AgentError::ToolRejected {
                cause: ToolRejectionCause::UserRejected,
                ..
            })
        ));
    }

    assert_eq!(
        broker.requested_tools().len(),
        3,
        "畸形代理调用必须逐条进入审批"
    );
}

/// workflow 子链的 `disabled()` 语义保持不变：本轮只补主会话启动入口。
#[tokio::test]
async fn disabled_permission_middleware_keeps_workflow_chain_semantics() {
    let mw = PermissionMiddleware::for_workflow(None, None);
    assert!(mw.prompt_sections().is_empty());
    let mut state = super::approval_state();
    let tc = workflow_start_call();

    let result = mw.before_tool(&mut state, &tc).await.unwrap();

    assert_eq!(result.name, tc.name);
}

/// 批量路径：启动项被拒绝不影响同批非敏感工具，且不会留下半启动子链。
#[tokio::test]
async fn workflow_start_rejection_in_batch_keeps_control_tools_result() {
    let broker = RecordingBroker::new(false);
    let mw = mw_with_mode_and_broker(PermissionMode::Default, broker.clone(), None);
    let mut state = super::approval_state();
    let start = workflow_start_call();
    let status = workflow_tool_call("WorkflowStatus", serde_json::json!({}));

    let results = mw
        .before_tools_batch(&mut state, &[start.clone(), status.clone()])
        .await;

    assert!(matches!(
        &results[0],
        Err(AgentError::ToolRejected {
            cause: ToolRejectionCause::UserRejected,
            ..
        })
    ));
    assert!(
        results[1].is_ok(),
        "同批 WorkflowStatus 不受 Workflow 拒绝影响"
    );
    assert_eq!(
        broker.requested_tools(),
        vec![start.name.clone()],
        "批量只对启动项弹一次审批"
    );
}
