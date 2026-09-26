use peri_acp_types::builtin_mcp::{
    is_reserved_instance_name, original_tool_name_of_effective, BuiltinMcpTool,
    BUILTIN_MCP_INSTANCES,
};
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
    let mw = PermissionMiddleware::disabled();
    let mut state = AgentState::new("/tmp");
    let tc = make_tool_call("Bash");
    let result = mw.before_tool(&mut state, &tc).await.unwrap();
    assert_eq!(result.name, "Bash");
}

#[tokio::test]
async fn test_approve_passes_through() {
    let mw = PermissionMiddleware::new(Arc::new(AutoApproveBroker), default_requires_approval);
    let mut state = AgentState::new("/tmp");
    let tc = make_tool_call("Bash");
    let result = mw.before_tool(&mut state, &tc).await.unwrap();
    assert_eq!(result.name, "Bash");
}

#[tokio::test]
async fn test_reject_returns_error() {
    let mw = PermissionMiddleware::new(Arc::new(AutoRejectBroker), default_requires_approval);
    let mut state = AgentState::new("/tmp");
    let tc = make_tool_call("Bash");
    let result = mw.before_tool(&mut state, &tc).await;
    assert!(matches!(result, Err(AgentError::ToolRejected { .. })));
}

#[tokio::test]
async fn test_read_file_not_intercepted() {
    let mw = PermissionMiddleware::new(Arc::new(AutoRejectBroker), default_requires_approval);
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

    let mw = PermissionMiddleware::new(Arc::new(EditBroker), default_requires_approval);
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

    let mw = PermissionMiddleware::new(Arc::new(RespondBroker), default_requires_approval);
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
) -> PermissionMiddleware {
    let broker = Arc::new(AutoApproveBroker);
    let shared = SharedPermissionMode::new(mode);
    PermissionMiddleware::with_shared_mode(broker, default_requires_approval, shared, classifier)
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

/// [回归测试] cron_register 在四模式下的行为与 10_hitl.md 机制说明一致。
///
/// 历史背景（审计 prompt-sections-audit.md P1-3）：10_hitl.md 固定清单漏列
/// `cron_register`，但运行时 `default_requires_approval` 已含该项（可定时
/// 触发任意 prompt，等价代理执行权）。重写后的机制说明补齐该项；本测试
/// 锁定实际决策：Default 走审批（broker），Bypass 直接放行，AcceptEdit 下
/// 不属于编辑工具仍需审批。
#[tokio::test]
async fn test_cron_register_requires_approval_in_default_mode() {
    let mw = make_mw_with_mode(PermissionMode::Default, None);
    let mut state = AgentState::new("/tmp");
    let tc = make_tool_call("cron_register");
    // AutoApproveBroker → 审批通过（说明确实进入了审批路径；Read 等非敏感
    // 工具根本不经过 broker）
    let result = mw.before_tool(&mut state, &tc).await.unwrap();
    assert_eq!(result.name, "cron_register");
}

#[tokio::test]
async fn test_cron_register_allowed_in_bypass_mode() {
    let mw = make_mw_with_mode(PermissionMode::Bypass, None);
    let mut state = AgentState::new("/tmp");
    let tc = make_tool_call("cron_register");
    let result = mw.before_tool(&mut state, &tc).await.unwrap();
    assert_eq!(result.name, "cron_register");
}

#[tokio::test]
async fn test_cron_register_not_edit_tool_in_accept_edit_mode() {
    // AcceptEdit 只自动放行 Write/Edit/folder_operations；
    // cron_register 仍走审批（AutoApproveBroker 通过）
    assert!(!is_edit_tool("cron_register"));
    let mw = make_mw_with_mode(PermissionMode::AcceptEdit, None);
    let mut state = AgentState::new("/tmp");
    let tc = make_tool_call("cron_register");
    let result = mw.before_tool(&mut state, &tc).await.unwrap();
    assert_eq!(result.name, "cron_register");
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

    let mw = PermissionMiddleware::new(Arc::new(HangingBroker), default_requires_approval)
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

// ─── 10_hitl 段落持有（波 4 演进 C3）────────────────────────────────────

/// 契约（设计 §3.1.2）：sensitive 条目与 `default_requires_approval` 判定
/// 一一对应——精确条目按名、前缀条目按前缀探测名必须判定为敏感；代表性
/// 非敏感工具必须判定为不敏感。列表与判定函数失同步即测试失败。
#[test]
fn sensitive_entries_match_default_requires_approval() {
    for entry in sensitive_tool_entries() {
        let probe = if entry.prefix_match {
            format!("{}some_tool", entry.name)
        } else {
            entry.name.to_string()
        };
        assert!(
            default_requires_approval(&probe),
            "条目 `{}`（prefix={}）应在 default_requires_approval 判定为敏感",
            entry.name,
            entry.prefix_match
        );
    }
    // 反向抽查：常规工具不敏感（与 test_default_requires_approval 的负例一致）
    for tool in [
        "Read",
        "Glob",
        "Grep",
        "TodoWrite",
        "AskUserQuestion",
        "cron_list",
        "cron_remove",
        "mcp_read_resource",
    ] {
        assert!(
            !default_requires_approval(tool),
            "{tool} 不应判定为敏感（条目列表与判定函数失同步）"
        );
    }
}

/// 条目集合与判定分支数一致（精确 11 项 + 前缀 3 项 = 14 项；
/// 变更 `default_requires_approval` 分支时必须同步条目清单）。
#[test]
fn sensitive_entries_cover_all_requires_approval_branches() {
    let entries = sensitive_tool_entries();
    assert_eq!(
        entries.len(),
        14,
        "条目清单应覆盖 default_requires_approval 全部分支"
    );
    // 前缀条目恰好 3 项（delete_ / rm_ / mcp__）
    let prefix_count = entries.iter().filter(|e| e.prefix_match).count();
    assert_eq!(prefix_count, 3, "前缀匹配条目应恰好 3 项");
}

/// 10_hitl 段落声明：位置属性（Uncached order=3）与内容结构（机制说明 +
/// 动态列表 + 模式决策尾句）。
#[test]
fn hitl_section_declaration_shape() {
    let sections = PermissionMiddleware::sections();
    assert_eq!(sections.len(), 1, "10_hitl 段应唯一");
    let section = &sections[0];
    assert_eq!(section.id, "10_hitl");
    assert_eq!(section.zone, PromptSectionZone::Uncached);
    assert_eq!(section.order, 3);
    let content = section.content.as_str();
    assert!(
        content.contains("# Human-in-the-Loop (HITL) Approval Mode"),
        "机制说明标题应保留（include_str 零拷贝）"
    );
    assert!(
        content.contains("## Which tools are sensitive"),
        "sensitive 小节引导保留"
    );
    assert!(
        content.contains("- `Bash` — shell command execution"),
        "动态列表按代码事实生成"
    );
    assert!(
        content.contains("Whether a sensitive tool actually requires approval is decided by the current `PermissionMode`"),
        "模式决策尾句保留"
    );
    // 段落文件不再硬编码列表（失同步防线）
    let file_content = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../peri-acp/prompts/sections/10_hitl.md"
    ));
    assert!(
        !file_content.contains("- `Bash`"),
        "10_hitl.md 不应再硬编码 sensitive 列表（列表由代码事实生成）"
    );
}

// ─── A4 生效名归一（IF-D6 判定型 ①② / IF-D15）─────────────────────────────

/// 声明表里的全部工具（`(实例, 工具)` 的单一事实源）。
fn declared_builtin_tools() -> Vec<&'static BuiltinMcpTool> {
    BUILTIN_MCP_INSTANCES
        .iter()
        .flat_map(|instance| instance.tools.iter())
        .collect()
}

/// IF-D6 判定型归一的等价性：对声明表逐项，effective name 的判定**等于**原始名。
#[test]
fn builtin_effective_names_match_original_name_policy() {
    let tools = declared_builtin_tools();
    assert_eq!(
        tools.len(),
        3,
        "wave 1 声明表应恰好三行（web×2 + artifact×1）"
    );

    for tool in tools {
        let (effective, original) = (tool.effective_name, tool.original_name);
        assert_eq!(
            original_tool_name_of_effective(effective),
            Some(original),
            "`{effective}` 应命中归一表并返回原始名 `{original}`"
        );
        assert_eq!(
            default_requires_approval(effective),
            default_requires_approval(original),
            "`{effective}` 的审批判定必须等于原始名 `{original}`（IF-D6）"
        );
        assert_eq!(
            is_edit_tool(effective),
            is_edit_tool(original),
            "`{effective}` 的编辑工具判定必须等于原始名 `{original}`（IF-D6）"
        );
    }
}

/// wave 1 冻结判定结果表（IF-G2）：逐项锁定两名称形态的判定值。
///
/// 显式记录的决策（不得靠读者推断）：`mcp__artifact__artifact` 由迁移前的
/// 「`mcp__` 前缀 ⇒ 需审批」变为「按原始名 `artifact` ⇒ **不需审批**」——这是
/// 「与迁移前 `artifact` 工具行为等价」的结果，不是新放行；收紧为需审批属独立
/// 策略变更（登记为可选分支 O1，缺省不启用）。
#[test]
fn wave1_frozen_effective_name_policy() {
    // 原始名侧（迁移前行为）
    assert!(default_requires_approval("WebSearch"));
    assert!(default_requires_approval("WebFetch"));
    assert!(!default_requires_approval("artifact"));

    // effective name 侧（迁移后，必须与原始名相等）
    assert!(default_requires_approval("mcp__web__WebSearch"));
    assert!(default_requires_approval("mcp__web__WebFetch"));
    assert!(
        !default_requires_approval("mcp__artifact__artifact"),
        "artifact 按原始名判定 ⇒ 不审批（IF-G2 决策 1，已显式记录）"
    );

    for name in [
        "WebSearch",
        "WebFetch",
        "artifact",
        "mcp__web__WebSearch",
        "mcp__web__WebFetch",
        "mcp__artifact__artifact",
    ] {
        assert!(!is_edit_tool(name), "`{name}` 不是编辑工具（IF-G2）");
    }
}

/// 反证（IF-D6 冻结约束 3）：未知 / 外部 `mcp__*` 的保守语义分毫不变。
#[test]
fn unknown_mcp_prefix_remains_conservative() {
    for unknown in [
        "mcp__filesystem__read_file",
        "mcp__filesystem__write_file",
        "mcp__github__create_issue",
        "mcp__database__query",
        "mcp__unknown__anything",
        "mcp__a__b",
        "mcp__x__y__z",
    ] {
        assert_eq!(
            original_tool_name_of_effective(unknown),
            None,
            "`{unknown}` 不在归一表内（未知 / 外部 MCP 工具）"
        );
        assert!(
            default_requires_approval(unknown),
            "未知 `mcp__*` 必须保守敏感：{unknown}"
        );
    }
    for not_mcp in ["mcp_", "mcp", "mcp_read_resource"] {
        assert!(!default_requires_approval(not_mcp), "{not_mcp} 不应敏感");
    }

    // 两条**理由不同**的 true 必须各自成立（IF-G2 反证）：`mcp__web__fetch` 与
    // `mcp__web__WebFetch` 不是同一个名字——前者未命中归一表，靠 `mcp__` 前缀
    // （未知 MCP 工具）判定；后者命中归一表，靠原始名 `WebFetch` 判定。
    assert_eq!(original_tool_name_of_effective("mcp__web__fetch"), None);
    assert!(default_requires_approval("mcp__web__fetch"));
    assert_eq!(
        original_tool_name_of_effective("mcp__web__WebFetch"),
        Some("WebFetch")
    );
    assert!(default_requires_approval("mcp__web__WebFetch"));
}

/// 保留名反例（A3 / IF-D6 冻结约束 4）：外部 server 若用保留名接管，绝不能靠
/// 「按名字反查」静默继承 builtin 一等工具的判定（即移除 `mcp__*` 审批门）。
///
/// 该风险的落点是**加载期 typed error**（`McpConfigError::ReservedBuiltinInstanceName`，
/// 断言在配置侧 `mcp::builtin_apply`，owner I-02）。本测试锁定 permission 侧可证的
/// 两项前提：① 归一表命中的实例名全部是保留名（加载器有据可拦）；
/// ② 预留但未实现的实例名不进归一表 ⇒ 其 effective name 仍是未知 MCP 工具，保守敏感。
#[test]
fn builtin_reserved_name_is_not_parity_hijackable() {
    for instance in BUILTIN_MCP_INSTANCES {
        assert!(
            is_reserved_instance_name(instance.name),
            "归一表只允许以保留实例名为键（保留名由加载期 typed error 保护，A3）：{}",
            instance.name
        );
    }
    for reserved in ["web", "artifact", "cron", "lsp", "workspace"] {
        assert!(
            is_reserved_instance_name(reserved),
            "{reserved} 应为保留实例名"
        );
    }

    // 预留但未实现（无工具声明）⇒ 不参与归一 ⇒ 名字仍是未知 MCP 工具，敏感
    assert_eq!(
        original_tool_name_of_effective("mcp__workspace__Read"),
        None
    );
    assert!(default_requires_approval("mcp__workspace__Read"));

    // 「外部 server 名 `artifact` + 工具 `artifact`」的判定跟原始名走（另一结果
    // = 加载期 typed error，见配置侧断言）；两者都必须成立其一，不得是第三种。
    assert_eq!(
        default_requires_approval("mcp__artifact__artifact"),
        default_requires_approval("artifact")
    );
}

/// A19 / IF-G3：已迁移条目的 `name` 为 effective name；`mcp__` 前缀条目的
/// description 不再宣称「所有 `mcp__*` 一律敏感」（对 builtin 一等工具已不充分）。
#[test]
fn sensitive_entries_use_effective_names_and_parity_description() {
    let entries = sensitive_tool_entries();
    let names: Vec<&str> = entries.iter().map(|entry| entry.name).collect();

    // 迁移前的裸名不再作为条目名出现（模型面看到的名字就是 effective name）
    assert!(
        !names.contains(&TOOL_WEBFETCH),
        "条目名不应再是裸名 `{TOOL_WEBFETCH}`（该名字迁移后不存在）"
    );
    assert!(
        !names.contains(&TOOL_WEBSEARCH),
        "条目名不应再是裸名 `{TOOL_WEBSEARCH}`（该名字迁移后不存在）"
    );

    // 两个 Web 工具以 effective name 精确条目出现
    let web = peri_acp_types::builtin_mcp::find("web").expect("web 实例应有声明");
    for tool in web.tools {
        assert!(
            entries
                .iter()
                .any(|entry| entry.name == tool.effective_name && !entry.prefix_match),
            "`{}` 应以 effective name 的精确条目出现",
            tool.effective_name
        );
    }

    // artifact 不在敏感清单（IF-G2 决策 1）——条目表与判定同源
    let artifact = peri_acp_types::builtin_mcp::find("artifact").expect("artifact 实例应有声明");
    for tool in artifact.tools {
        assert!(
            !names.contains(&tool.effective_name),
            "`{}` 不应在敏感清单（artifact 不审批）",
            tool.effective_name
        );
    }

    // 前缀条目条文与归一后的判定一致
    let prefix_entry = entries
        .iter()
        .find(|entry| entry.prefix_match && entry.name == "mcp__")
        .expect("`mcp__` 前缀条目应存在");
    assert!(
        prefix_entry
            .description
            .contains("follow their original tool's rule"),
        "`mcp__` 条目条文必须说明 builtin 一等工具按原始名规则，实际：{}",
        prefix_entry.description
    );
}
