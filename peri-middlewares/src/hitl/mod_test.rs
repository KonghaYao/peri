use peri_agent::agent::state::AgentState;

use super::*;

/// 自动批准 broker
struct AutoApproveBroker;

#[async_trait]
impl UserInteractionBroker for AutoApproveBroker {
    async fn request(&self, ctx: InteractionContext) -> InteractionResponse {
        match ctx {
            InteractionContext::Approval { items } => InteractionResponse::Decisions(
                items
                    .iter()
                    .map(|_| ApprovalDecision::Approve { source: None })
                    .collect(),
            ),
            _ => InteractionResponse::Decisions(vec![]),
        }
    }
}

/// 自动拒绝 broker
struct AutoRejectBroker;

#[async_trait]
impl UserInteractionBroker for AutoRejectBroker {
    async fn request(&self, ctx: InteractionContext) -> InteractionResponse {
        match ctx {
            InteractionContext::Approval { items } => InteractionResponse::Decisions(
                items
                    .iter()
                    .map(|_| ApprovalDecision::Reject {
                        reason: "用户拒绝".to_string(),
                        source: None,
                    })
                    .collect(),
            ),
            _ => InteractionResponse::Decisions(vec![]),
        }
    }
}

fn make_tool_call(name: &str) -> ToolCall {
    ToolCall {
        id: "test-id".to_string(),
        name: name.to_string(),
        input: serde_json::json!({"command": "ls"}),
    }
}

#[tokio::test]
async fn test_disabled_allows_all() {
    let mw = HumanInTheLoopMiddleware::disabled();
    let mut state = AgentState::new("/tmp");
    let tc = make_tool_call("Bash");
    let result = mw.before_tool(&mut state, &tc).await.unwrap();
    assert_eq!(result.name, "Bash");
}

#[tokio::test]
async fn test_approve_passes_through() {
    let mw = HumanInTheLoopMiddleware::new(Arc::new(AutoApproveBroker), default_requires_approval);
    let mut state = AgentState::new("/tmp");
    let tc = make_tool_call("Bash");
    let result = mw.before_tool(&mut state, &tc).await.unwrap();
    assert_eq!(result.name, "Bash");
}

#[tokio::test]
async fn test_reject_returns_error() {
    let mw = HumanInTheLoopMiddleware::new(Arc::new(AutoRejectBroker), default_requires_approval);
    let mut state = AgentState::new("/tmp");
    let tc = make_tool_call("Bash");
    let result = mw.before_tool(&mut state, &tc).await;
    assert!(matches!(result, Err(AgentError::ToolRejected { .. })));
}

#[tokio::test]
async fn test_read_file_not_intercepted() {
    let mw = HumanInTheLoopMiddleware::new(Arc::new(AutoRejectBroker), default_requires_approval);
    let mut state = AgentState::new("/tmp");
    let tc = make_tool_call("Read");
    let result = mw.before_tool(&mut state, &tc).await.unwrap();
    assert_eq!(result.name, "Read");
}

#[test]
fn test_default_requires_approval() {
    assert!(default_requires_approval("Bash"));
    assert!(default_requires_approval("Write"));
    assert!(default_requires_approval("Edit"));
    assert!(default_requires_approval("folder_operations"));
    assert!(default_requires_approval("delete_something"));
    assert!(default_requires_approval("rm_rf"));
    assert!(default_requires_approval("Agent"));
    // MCP 工具需审批
    assert!(default_requires_approval("mcp__filesystem__read_file"));
    assert!(default_requires_approval("mcp__filesystem__write_file"));
    assert!(default_requires_approval("mcp__github__create_issue"));
    assert!(default_requires_approval("mcp__database__query"));
    assert!(default_requires_approval("mcp__web__fetch"));

    // Web 工具需审批
    assert!(default_requires_approval("WebFetch"));
    assert!(default_requires_approval("WebSearch"));

    // cron_register 可定时触发任意 prompt，等价代理执行权，需审批
    assert!(default_requires_approval("cron_register"));
    // cron_list / cron_remove 仅查询/撤销，不拦截
    assert!(!default_requires_approval("cron_list"));
    assert!(!default_requires_approval("cron_remove"));

    assert!(!default_requires_approval("Read"));
    assert!(!default_requires_approval("Glob"));
    assert!(!default_requires_approval("Grep"));
    assert!(!default_requires_approval("TodoWrite"));
    assert!(!default_requires_approval("ask_user"));
    // mcp_read_resource 不以 mcp__（双下划线）开头，不拦截
    assert!(!default_requires_approval("mcp_read_resource"));
}

#[test]
fn test_mcp_prefix_edge_cases() {
    // 单下划线不匹配
    assert!(!default_requires_approval("mcp_"));
    assert!(!default_requires_approval("mcp_read_resource"));
    // 无下划线不匹配
    assert!(!default_requires_approval("mcp"));
    // 双下划线匹配
    assert!(default_requires_approval("mcp__a__b"));
    assert!(default_requires_approval("mcp__server__tool_name"));
    assert!(default_requires_approval("mcp__x__y__z"));
}

#[test]
fn test_is_edit_tool_excludes_mcp() {
    // MCP 工具不属于编辑工具，在 AcceptEdits 模式下仍需审批
    assert!(!is_edit_tool("mcp__filesystem__write_file"));
}

#[tokio::test]
async fn test_edit_modifies_input() {
    struct EditBroker;

    #[async_trait]
    impl UserInteractionBroker for EditBroker {
        async fn request(&self, ctx: InteractionContext) -> InteractionResponse {
            match ctx {
                InteractionContext::Approval { items } => InteractionResponse::Decisions(
                    items
                        .iter()
                        .map(|_| ApprovalDecision::Edit {
                            new_input: serde_json::json!({"command": "echo safe"}),
                        })
                        .collect(),
                ),
                _ => InteractionResponse::Decisions(vec![]),
            }
        }
    }

    let mw = HumanInTheLoopMiddleware::new(Arc::new(EditBroker), default_requires_approval);
    let mut state = AgentState::new("/tmp");
    let tc = make_tool_call("Bash");
    let result = mw.before_tool(&mut state, &tc).await.unwrap();
    assert_eq!(result.name, "Bash");
    assert_eq!(result.input, serde_json::json!({"command": "echo safe"}));
}

#[tokio::test]
async fn test_respond_returns_error_with_reason() {
    struct RespondBroker;

    #[async_trait]
    impl UserInteractionBroker for RespondBroker {
        async fn request(&self, ctx: InteractionContext) -> InteractionResponse {
            match ctx {
                InteractionContext::Approval { items } => InteractionResponse::Decisions(
                    items
                        .iter()
                        .map(|_| ApprovalDecision::Respond {
                            message: "请改用 echo 命令".to_string(),
                        })
                        .collect(),
                ),
                _ => InteractionResponse::Decisions(vec![]),
            }
        }
    }

    let mw = HumanInTheLoopMiddleware::new(Arc::new(RespondBroker), default_requires_approval);
    let mut state = AgentState::new("/tmp");
    let tc = make_tool_call("Bash");
    let result = mw.before_tool(&mut state, &tc).await;
    match result {
        Err(AgentError::ToolRejected { reason, .. }) => {
            assert_eq!(reason, "请改用 echo 命令");
        }
        other => unreachable!("期望 ToolRejected，实际: {:?}", other),
    }
}

// ─── 多模式测试 ─────────────────────────────────────────────────────────────

#[test]
fn test_is_edit_tool() {
    assert!(is_edit_tool("Write"));
    assert!(is_edit_tool("Edit"));
    assert!(is_edit_tool("folder_operations"));
    assert!(!is_edit_tool("Bash"));
    assert!(!is_edit_tool("Agent"));
    assert!(!is_edit_tool("delete_x"));
    assert!(!is_edit_tool("rm_x"));
    assert!(!is_edit_tool("Read"));
}

/// Mock 自动分类器
struct MockClassifier {
    result: Classification,
}
impl MockClassifier {
    fn new(result: Classification) -> Self {
        Self { result }
    }
}
#[async_trait]
impl AutoClassifier for MockClassifier {
    async fn classify(&self, _tool_name: &str, _tool_input: &serde_json::Value) -> Classification {
        self.result
    }
}

fn make_mw_with_mode(
    mode: PermissionMode,
    classifier: Option<Arc<dyn AutoClassifier>>,
) -> HumanInTheLoopMiddleware {
    let broker = Arc::new(AutoApproveBroker);
    let shared = SharedPermissionMode::new(mode);
    HumanInTheLoopMiddleware::with_shared_mode(
        broker,
        default_requires_approval,
        shared,
        classifier,
    )
}

#[tokio::test]
async fn test_bypass_permissions_allows_all() {
    let mw = make_mw_with_mode(PermissionMode::Bypass, None);
    let mut state = AgentState::new("/tmp");
    let tc = make_tool_call("Bash");
    let result = mw.before_tool(&mut state, &tc).await.unwrap();
    assert_eq!(result.name, "Bash");
}

#[tokio::test]
async fn test_dont_ask_rejects_all() {
    let mw = make_mw_with_mode(PermissionMode::DontAsk, None);
    let mut state = AgentState::new("/tmp");
    let tc = make_tool_call("Bash");
    let result = mw.before_tool(&mut state, &tc).await;
    assert!(matches!(result, Err(AgentError::ToolRejected { .. })));
}

#[tokio::test]
async fn test_accept_edits_allows_write_file() {
    let mw = make_mw_with_mode(PermissionMode::AcceptEdit, None);
    let mut state = AgentState::new("/tmp");
    let tc = make_tool_call("Write");
    let result = mw.before_tool(&mut state, &tc).await.unwrap();
    assert_eq!(result.name, "Write");
}

#[tokio::test]
async fn test_accept_edits_approves_bash_via_broker() {
    let mw = make_mw_with_mode(PermissionMode::AcceptEdit, None);
    let mut state = AgentState::new("/tmp");
    let tc = make_tool_call("Bash");
    let result = mw.before_tool(&mut state, &tc).await.unwrap();
    assert_eq!(result.name, "Bash");
}

#[tokio::test]
async fn test_default_mode_approves_bash_via_broker() {
    let mw = make_mw_with_mode(PermissionMode::Default, None);
    let mut state = AgentState::new("/tmp");
    let tc = make_tool_call("Bash");
    let result = mw.before_tool(&mut state, &tc).await.unwrap();
    assert_eq!(result.name, "Bash");
}

#[tokio::test]
async fn test_auto_mode_allow() {
    let mw = make_mw_with_mode(
        PermissionMode::AutoMode,
        Some(Arc::new(MockClassifier::new(Classification::Allow))),
    );
    let mut state = AgentState::new("/tmp");
    let tc = make_tool_call("Bash");
    let result = mw.before_tool(&mut state, &tc).await.unwrap();
    assert_eq!(result.name, "Bash");
}

#[tokio::test]
async fn test_auto_mode_deny() {
    let mw = make_mw_with_mode(
        PermissionMode::AutoMode,
        Some(Arc::new(MockClassifier::new(Classification::Deny))),
    );
    let mut state = AgentState::new("/tmp");
    let tc = make_tool_call("Bash");
    let result = mw.before_tool(&mut state, &tc).await;
    assert!(matches!(result, Err(AgentError::ToolRejected { .. })));
}

#[tokio::test]
async fn test_auto_mode_unsure_falls_back_to_broker() {
    let mw = make_mw_with_mode(
        PermissionMode::AutoMode,
        Some(Arc::new(MockClassifier::new(Classification::Unsure))),
    );
    let mut state = AgentState::new("/tmp");
    let tc = make_tool_call("Bash");
    let result = mw.before_tool(&mut state, &tc).await.unwrap();
    assert_eq!(result.name, "Bash");
}

#[tokio::test]
async fn test_auto_mode_no_classifier_falls_back_to_broker() {
    let mw = make_mw_with_mode(PermissionMode::AutoMode, None);
    let mut state = AgentState::new("/tmp");
    let tc = make_tool_call("Bash");
    let result = mw.before_tool(&mut state, &tc).await.unwrap();
    assert_eq!(result.name, "Bash");
}

#[tokio::test]
async fn test_process_batch_bypass_permissions() {
    let mw = make_mw_with_mode(PermissionMode::Bypass, None);
    let calls = vec![
        make_tool_call("Bash"),
        make_tool_call("Write"),
        make_tool_call("Read"),
    ];
    let results = mw.process_batch(&calls).await;
    assert_eq!(results.len(), 3);
    assert!(results.iter().all(|r| r.is_ok()));
}

#[tokio::test]
async fn test_process_batch_dont_ask_rejects_sensitive() {
    let mw = make_mw_with_mode(PermissionMode::DontAsk, None);
    let calls = vec![make_tool_call("Bash"), make_tool_call("Read")];
    let results = mw.process_batch(&calls).await;
    assert_eq!(results.len(), 2);
    assert!(results[0].is_err(), "bash 应被拒绝");
    assert!(results[1].is_ok(), "read_file 应放行");
}

#[tokio::test]
async fn test_process_batch_accept_edits_mixed() {
    let mw = make_mw_with_mode(PermissionMode::AcceptEdit, None);
    let calls = vec![
        make_tool_call("Write"),
        make_tool_call("Bash"),
        make_tool_call("Read"),
    ];
    let results = mw.process_batch(&calls).await;
    assert_eq!(results.len(), 3);
    assert!(results[0].is_ok(), "write_file 应放行");
    assert!(
        results[1].is_ok(),
        "bash 走 broker 审批（AutoApproveBroker）"
    );
    assert!(results[2].is_ok(), "read_file 应放行");
}

/// Broker 挂起时 before_tool 会无限等待，文档化当前的同步阻塞缺陷。
/// broker.request 会无限等待用户响应。
/// 真实场景中如果用户长时间不操作，before_tool 将永久阻塞。
#[tokio::test]
async fn test_broker_hang_rejects_with_timeout() {
    // 构造一个永不返回的 broker（模拟用户迟迟不点击审批按钮）
    struct HangingBroker;
    #[async_trait]
    impl UserInteractionBroker for HangingBroker {
        async fn request(&self, _ctx: InteractionContext) -> InteractionResponse {
            // 永不返回，模拟 broker 挂起
            std::future::pending::<()>().await;
            unreachable!()
        }
    }

    let mw = HumanInTheLoopMiddleware::new(Arc::new(HangingBroker), default_requires_approval)
        .with_broker_timeout(std::time::Duration::from_millis(500));
    let mut state = AgentState::new("/tmp");
    let tc = make_tool_call("Bash");

    let result = mw.before_tool(&mut state, &tc).await;

    // 修复后：broker_timeout 内置超时保护，应返回 ToolRejected 而非永久阻塞
    assert!(
        result.is_err(),
        "挂起 broker 应触发超时拒绝，实际: {:?}",
        result
    );
    let err = result.unwrap_err();
    assert!(
        matches!(&err, AgentError::ToolRejected { reason, .. } if reason.contains("超时")),
        "拒绝应为 ToolRejected 且原因包含超时，实际: {:?}",
        err
    );
}
