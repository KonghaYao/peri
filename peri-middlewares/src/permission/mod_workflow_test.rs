//! Workflow 启动入口审批与提示词可见性测试。
//!
//! 从 `mod_test.rs` 拆出以控制单文件行数；挂载在 `tests` 下，测试过滤路径为
//! `permission::tests::workflow::...`。

use super::super::*;
use super::{approval_state, make_tool_call, registry_effective_name};

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

/// Workflow 启动调用的模型面名字取自真实 builtin 声明表。
fn workflow_start_call() -> ToolCall {
    ToolCall {
        id: "workflow-start".to_string(),
        name: registry_effective_name("workflow", "Workflow").to_string(),
        input: serde_json::json!({"script": "..."}),
    }
}

fn workflow_tool_call(original: &str, input: serde_json::Value) -> ToolCall {
    ToolCall {
        id: format!("workflow-{original}"),
        name: registry_effective_name("workflow", original).to_string(),
        input,
    }
}

#[tokio::test]
async fn workflow_start_requires_approval_in_default_mode() {
    let broker = RecordingBroker::new(true);
    let mw = mw_with_mode_and_broker(PermissionMode::Default, broker.clone(), None);
    let mut state = approval_state();
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
    let mut state = approval_state();
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
    let mut state = approval_state();

    // 对照：AcceptEdits 下 Write 仍免审批（编辑工具自动放行）。
    let write = make_tool_call(registry_effective_name("workspace", "Write"));
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
    let mut state = approval_state();
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
    let mut state = approval_state();
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
    let mut state = approval_state();
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
    let mut state = approval_state();

    let declared = peri_acp_types::builtin_mcp::find("workflow").expect("workflow 实例应有声明");
    let mut checked = 0;
    for tool in declared
        .tools
        .iter()
        .filter(|tool| tool.original_name != "Workflow")
    {
        let tc = workflow_tool_call(tool.original_name, serde_json::json!({}));
        let result = mw.before_tool(&mut state, &tc).await;
        assert!(
            result.is_ok(),
            "`{}` 不应触发审批，实际: {result:?}",
            tool.effective_name
        );
        assert_eq!(result.unwrap().name, tc.name);
        checked += 1;
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

/// deferred 工具经 `ExecuteExtraTool` 的兼容路径同样按目标名判定。
#[tokio::test]
async fn workflow_start_via_execute_extra_tool_requires_approval() {
    let broker = RecordingBroker::new(false);
    let mw = mw_with_mode_and_broker(PermissionMode::Default, broker.clone(), None);
    let mut state = approval_state();
    let target = registry_effective_name("workflow", "Workflow");
    let tc = ToolCall {
        id: "workflow-extra".to_string(),
        name: "ExecuteExtraTool".to_string(),
        input: serde_json::json!({
            "tool_name": target,
            "params": {"script": "..."},
        }),
    };

    let result = mw.before_tool(&mut state, &tc).await;

    assert!(matches!(
        result,
        Err(AgentError::ToolRejected {
            cause: ToolRejectionCause::UserRejected,
            ..
        })
    ));
    assert_eq!(
        broker.requested_tools(),
        vec!["ExecuteExtraTool".to_string()],
        "包装调用应进入审批（目标名 {target} 驱动判定）"
    );
}

/// workflow 子链的 `disabled()` 语义保持不变：本轮只补主会话启动入口。
#[tokio::test]
async fn disabled_permission_middleware_keeps_workflow_chain_semantics() {
    let mw = PermissionMiddleware::disabled();
    let mut state = approval_state();
    let tc = workflow_start_call();

    let result = mw.before_tool(&mut state, &tc).await.unwrap();

    assert_eq!(result.name, tc.name);
}

/// 批量路径：启动项被拒绝不影响同批非敏感工具，且不会留下半启动子链。
#[tokio::test]
async fn workflow_start_rejection_in_batch_keeps_control_tools_result() {
    let broker = RecordingBroker::new(false);
    let mw = mw_with_mode_and_broker(PermissionMode::Default, broker.clone(), None);
    let mut state = approval_state();
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

/// §8 第 17 行（F9）：模型可见 Markdown 的显示名从声明表解析 effective name。
///
/// 断言的是**渲染输出**：`cron_register` 迁移为 builtin 实例后已不存在裸名工具，
/// 渲染文本里出现裸名条目即点名一个不存在的工具。期望值从注册表
/// （`peri_acp_types::builtin_mcp`）派生而非复用生产查表助手——复用助手会让
/// 「助手退回裸名」这种回归与断言同向变化而无法失败。
#[test]
fn cron_sensitive_markdown_uses_registry_effective_name() {
    let rendered = format_sensitive_tools();
    let lines: Vec<&str> = rendered.lines().collect();
    assert_eq!(lines.len(), 13, "敏感工具完整列表应逐项渲染");

    let cron = peri_acp_types::builtin_mcp::find("cron").expect("cron 实例应有声明");
    let register = cron
        .tools
        .iter()
        .find(|tool| tool.original_name == "cron_register")
        .expect("cron 实例应声明 cron_register");
    let cron_line = format!(
        "- `{}` — scheduled task registration (can trigger arbitrary prompts later, equivalent to delegated execution rights)",
        register.effective_name
    );
    assert!(
        lines.contains(&cron_line.as_str()),
        "cron 条目显示名必须是注册表解析出的 effective name，实际渲染：\n{rendered}"
    );

    let monitor = peri_acp_types::builtin_mcp::find("monitor").expect("monitor 实例应有声明");
    let start = monitor
        .tools
        .iter()
        .find(|tool| tool.original_name == "monitor_start")
        .expect("monitor 实例应声明 monitor_start");
    assert!(
        lines.contains(
            &format!(
                "- `{}` — monitor command execution (starts a shell command and watches its output)",
                start.effective_name
            )
            .as_str()
        ),
        "monitor 条目显示名必须是注册表解析出的 effective name，实际渲染：\n{rendered}"
    );

    let workflow = peri_acp_types::builtin_mcp::find("workflow").expect("workflow 实例应有声明");
    let workflow_start = workflow
        .tools
        .iter()
        .find(|tool| tool.original_name == "Workflow")
        .expect("workflow 实例应声明 Workflow");
    assert_eq!(
        *lines.last().expect("敏感工具列表非空"),
        format!(
            "- `{}` — workflow delegation (launches a multi-agent workflow that bypasses HITL, equivalent to delegated execution rights)",
            workflow_start.effective_name
        ),
        "末项（Workflow 启动）显示名必须是注册表解析出的 effective name，实际渲染：\n{rendered}"
    );

    // 已失效裸名不得作为条目名出现；effective name 内部含 `cron_register` 子串，
    // 故按「行首条目名」判定而非整串包含。
    assert!(
        !lines
            .iter()
            .any(|line| line.starts_with("- `cron_register")),
        "渲染输出不得残留已失效的裸名条目：\n{rendered}"
    );

    // 未进敏感清单的 builtin 实例（artifact）不得因改名被误加进来
    for instance in ["artifact"] {
        let declared = peri_acp_types::builtin_mcp::find(instance).expect("实例应有声明");
        for tool in declared.tools {
            assert!(
                !lines
                    .iter()
                    .any(|line| line.starts_with(&format!("- `{}`", tool.effective_name))),
                "`{}` 不应在敏感清单（判定同源，A19）",
                tool.effective_name
            );
        }
    }
}

#[test]
fn deployment_closed_builtin_is_omitted_from_prompt_without_weakening_approval() {
    let disabled = std::collections::HashSet::from([
        "CronMiddleware".to_owned(),
        "MonitorMiddleware".to_owned(),
        "WorkflowMiddleware".to_owned(),
        "WorkspaceMiddleware".to_owned(),
    ]);
    let visible = format_sensitive_tools_for_disabled(&disabled);
    assert!(!visible.contains("mcp__cron__cron_register"));
    assert!(!visible.contains("mcp__monitor__monitor_start"));
    assert!(!visible
        .lines()
        .any(|line| line.starts_with("- `mcp__workflow__Workflow`")));
    assert!(!visible.lines().any(|line| line.starts_with("- `Bash`")));
    assert!(visible.lines().any(|line| line.starts_with("- `WebFetch`")));
    assert!(default_requires_approval("cron_register"));
    assert!(default_requires_approval("monitor_start"));
    // 关闭实例只影响提示词清单，不削弱运行时判定（仍保守审批）。
    assert!(default_requires_approval("mcp__workflow__Workflow"));
    assert_eq!(format_sensitive_tools().lines().count(), 13);
}
