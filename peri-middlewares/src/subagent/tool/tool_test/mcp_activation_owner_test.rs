//! MCP Agent 内容绑定审批门的归属回归（从 `tool_test.rs` 拆出，见 STD-SIZE-001）。
//!
//! 覆盖 `load_and_approve_mcp_agent` 的 fail-closed 分支：缺少可信归属 ⇒ 显式拒绝、
//! 零 broker 交互；并用真实线路夹具做路径证明。

use super::*;

// ══════════════════════════════════════════════════════════════════════════════
// MCP Agent 内容绑定审批门：缺少可信归属 ⇒ fail-closed
// ══════════════════════════════════════════════════════════════════════════════

/// 记录交互次数、恒定批准的 broker（零交互断言与路径证明共用）。
struct CountingBroker {
    calls: std::sync::atomic::AtomicUsize,
}

impl CountingBroker {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl peri_agent::interaction::UserInteractionBroker for CountingBroker {
    async fn request(
        &self,
        _owner: peri_agent::interaction::TrustedInteractionOwner,
        ctx: peri_agent::interaction::InteractionContext,
    ) -> peri_agent::interaction::InteractionResponse {
        use peri_agent::interaction::{ApprovalDecision, InteractionContext, InteractionResponse};
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let InteractionContext::Approval { items } = ctx else {
            return InteractionResponse::Rejected;
        };
        InteractionResponse::Decisions(vec![
            ApprovalDecision::Approve { source: None };
            items.len()
        ])
    }
}

/// **远端来源**（外部 MCP server）的 Agent 资源面：真实 peer + 真实
/// `resources/read`（`AgentFaceFixture` 的生产链路），但句柄不带 builtin
/// workspace 信任锚——目录因此投影为 `mcp__workspace__*` 远端条目，正文激活必须
/// 经过内容绑定审批门（`load_and_approve_mcp_agent`）。
async fn remote_agent_face(dir: &std::path::Path) -> Arc<crate::mcp::McpAgentRegistry> {
    use crate::mcp::client::{ClientStatus, McpClientHandle};
    let fixture = crate::mcp::agent_face_fixture::AgentFaceFixture::connect(dir).await;
    let pool = Arc::clone(&fixture.pool);
    {
        let builtin = pool
            .get_client("workspace")
            .expect("夹具必须注册 workspace 句柄");
        pool.clients.write().insert(
            "workspace".to_string(),
            Arc::new(McpClientHandle {
                name: builtin.name.clone(),
                version: builtin.version.clone(),
                connected_at: builtin.connected_at.clone(),
                protocol_version: builtin.protocol_version.clone(),
                cache_version: builtin.cache_version.clone(),
                peer: builtin.peer.clone(),
                tools: Vec::new(),
                resources: builtin.resources.clone(),
                status: ClientStatus::Connected,
                oauth_status: Default::default(),
                // 非 builtin 信任锚 ⇒ 远端来源（正文激活须审批）。
                source: None,
                url: None,
                skills_capable: false,
            }),
        );
    }
    let registry = Arc::new(crate::mcp::McpAgentRegistry::new(pool));
    AGENT_FACE_FIXTURES.with(|fixtures| fixtures.borrow_mut().push(fixture));
    registry
}

/// [回归] MCP Agent 正文激活审批门缺可信归属 ⇒ 显式拒绝，零 broker 交互。
///
/// 路径证明：同一 activation 在归属齐备时**确实走到审批门**（同一 registry 的
/// 第二次调用产生一次交互并放行），因此第一次的拒绝来自归属缺失，而不是激活
/// 失败或目录未命中。
#[tokio::test]
async fn mcp_agent_activation_without_trusted_owner_is_rejected_without_asking() {
    use peri_agent::interaction::TrustedInteractionOwner;

    let dir = tempdir().unwrap();
    write_test_agent(&dir);
    std::fs::write(
        dir.path()
            .join(".claude")
            .join("agents")
            .join("reviewface.md"),
        "---\nname: reviewface\ndescription: Remote review agent\n---\n\nReview.\n",
    )
    .unwrap();
    let registry = remote_agent_face(dir.path()).await;
    let broker = CountingBroker::new();
    let tool = make_subagent_tool(Vec::new()).with_mcp_agents(
        Some(Arc::clone(&registry)),
        Some(Arc::clone(&broker) as Arc<dyn peri_agent::interaction::UserInteractionBroker>),
    );

    let rejected = tool
        .load_and_approve_mcp_agent("mcp__workspace__reviewface", None)
        .await
        .expect_err("缺归属必须拒绝激活，不得自动批准")
        .to_string();
    assert!(
        rejected.contains(crate::interaction_owner::UNTRUSTED_OWNER_REASON),
        "拒绝原因必须是无归属说明，实际: {rejected}"
    );
    assert_eq!(broker.calls(), 0, "缺归属不得发出任何审批交互");

    let owner = TrustedInteractionOwner::model(
        "session-test",
        peri_acp_types::session::ExecutionBinding {
            turn_id: peri_acp_types::session::TurnId::new(),
            attempt_id: peri_acp_types::identity::AttemptId::new(),
        },
    );
    tool.load_and_approve_mcp_agent("mcp__workspace__reviewface", Some(&owner))
        .await
        .expect("归属齐备且获批时必须完成激活");
    assert_eq!(broker.calls(), 1, "有归属时恰好一次审批交互");
}
