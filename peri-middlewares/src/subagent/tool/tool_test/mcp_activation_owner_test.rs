//! MCP Agent 内容绑定审批门的归属回归（从 `tool_test.rs` 拆出，见 STD-SIZE-001）。
//!
//! 覆盖 v5 后台默认下的审批链路：
//! - 缺少可信归属 ⇒ 显式拒绝、零 broker 交互；
//! - 批准 ⇒ 先过审批门再登记后台任务；
//! - 拒绝 ⇒ 零后台任务、零 child thread；
//! - 定义内容或有效工具集变化 ⇒ 审批键变化，必须重新审批；
//! - 路径证明：同一 activation 在归属齐备时确实走到审批门。

use std::sync::atomic::{AtomicUsize, Ordering};

use peri_acp_types::identity::AttemptId;
use peri_acp_types::interaction::InteractionAttribution;
use peri_acp_types::session::{ExecutionBinding, TurnId};
use peri_agent::interaction::{
    ApprovalDecision, InteractionContext, InteractionResponse, TrustedInteractionOwner,
    UserInteractionBroker,
};
use peri_agent::tools::BaseTool;

use super::Arc;
use super::BaseMessage;
use super::AGENT_FACE_FIXTURES;

// ══════════════════════════════════════════════════════════════════════════════
// MCP Agent 内容绑定审批门：归属、批准、拒绝与重新审批
// ══════════════════════════════════════════════════════════════════════════════

#[derive(Clone, Copy)]
enum BrokerDecision {
    Approve,
    Reject,
}

/// 记录交互次数并按固定决策作答的 broker（零交互断言、拒绝零任务与路径证明共用）。
struct CountingBroker {
    calls: AtomicUsize,
    decision: BrokerDecision,
}

impl CountingBroker {
    fn approving() -> Arc<Self> {
        Self::new(BrokerDecision::Approve)
    }

    fn rejecting() -> Arc<Self> {
        Self::new(BrokerDecision::Reject)
    }

    fn new(decision: BrokerDecision) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            decision,
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl UserInteractionBroker for CountingBroker {
    async fn request(
        &self,
        _owner: TrustedInteractionOwner,
        ctx: InteractionContext,
    ) -> InteractionResponse {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let InteractionContext::Approval { items } = ctx else {
            return InteractionResponse::Rejected;
        };
        let decision = match self.decision {
            BrokerDecision::Approve => ApprovalDecision::Approve { source: None },
            BrokerDecision::Reject => ApprovalDecision::Reject {
                reason: "fixture rejected MCP agent activation".to_string(),
                source: None,
            },
        };
        InteractionResponse::Decisions(vec![decision; items.len()])
    }
}

/// 远端来源 Agent 资源面的 registry 与其池（池用于内容变化后失效资源缓存）。
struct RemoteAgentFace {
    registry: Arc<crate::mcp::McpAgentRegistry>,
    pool: Arc<crate::mcp::client::McpClientPool>,
}

/// **远端来源**（外部 MCP server）的 Agent 资源面：真实 peer + 真实
/// `resources/read`（`AgentFaceFixture` 的生产链路），但句柄不带 builtin
/// workspace 信任锚——目录因此投影为 `mcp__workspace__*` 远端条目，正文激活必须
/// 经过内容绑定审批门（`load_and_approve_mcp_agent`）。
async fn remote_agent_face(dir: &std::path::Path) -> RemoteAgentFace {
    use crate::mcp::client::{ClientStatus, McpClientHandle};
    let fixture = crate::mcp::agent_face_fixture::AgentFaceFixture::connect(dir).await;
    let pool = Arc::clone(&fixture.pool);
    {
        let builtin = pool
            .client("workspace")
            .expect("夹具必须注册 workspace 句柄");
        pool.clients.write().insert(
            "workspace".to_string(),
            Arc::new(McpClientHandle {
                call_gate: Default::default(),
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
    let registry = Arc::new(crate::mcp::McpAgentRegistry::new(Arc::clone(&pool)));
    AGENT_FACE_FIXTURES.with(|fixtures| fixtures.borrow_mut().push(fixture));
    RemoteAgentFace { registry, pool }
}

fn write_reviewface(dir: &std::path::Path, tools: Option<&str>, body: &str) {
    let agents = dir.join(".claude").join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    let tools = tools.map_or(String::new(), |tools| format!("tools: {tools}\n"));
    std::fs::write(
        agents.join("reviewface.md"),
        format!("---\nname: reviewface\ndescription: Remote review agent\n{tools}---\n\n{body}\n"),
    )
    .unwrap();
}

fn reviewface_uri() -> String {
    peri_acp_types::workspace_resources::agent_uri(
        peri_mcp_workspace::ResourceScope::Project,
        None,
        "reviewface",
    )
    .expect("reviewface uri")
}

/// 带审批归属的模型调用上下文（canonical dispatch 会绑定的真实 turn 事实）。
fn model_context<'a>(
    host: &'a super::HostFixture,
    messages: &'a [BaseMessage],
) -> peri_agent::tools::ToolContext<'a> {
    let mut ctx = host.context(messages);
    ctx.attribution = Some(InteractionAttribution::model_turn(
        "fixture-mcp-approval-session",
        ExecutionBinding {
            turn_id: TurnId::new(),
            attempt_id: AttemptId::new(),
        },
    ));
    ctx
}

/// 带后台通道的宿主与任务管理器。
async fn background_host(
    dir: &std::path::Path,
    label: &str,
) -> (
    super::HostFixture,
    Arc<peri_agent::agent::async_tasks::TaskManager>,
) {
    let (bg_tx, _bg_rx) =
        tokio::sync::mpsc::unbounded_channel::<peri_agent::agent::events::ExecutorEvent>();
    let manager = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    let host =
        super::HostFixture::open_in_with_background(dir, label, Arc::clone(&manager), bg_tx).await;
    (host, manager)
}

/// 夹具工具：只提供名字，供 `tools:` 声明过滤产生稳定的有效工具集。
struct NamedTool(&'static str);

#[async_trait::async_trait]
impl BaseTool for NamedTool {
    fn name(&self) -> &str {
        self.0
    }

    fn description(&self) -> &str {
        "approval fixture tool"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({})
    }

    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok(String::new())
    }
}

/// [回归] 缺可信归属 ⇒ 显式拒绝，零 broker 交互。
///
/// 路径证明：同一 activation 在归属齐备时**确实走到审批门**（同一 registry 的
/// 第二次调用产生一次交互并放行），因此第一次的拒绝来自归属缺失，而不是激活
/// 失败或目录未命中。
#[tokio::test]
async fn mcp_agent_activation_without_trusted_owner_is_rejected_without_asking() {
    let dir = tempfile::tempdir().unwrap();
    super::write_test_agent(&dir);
    write_reviewface(dir.path(), None, "Review.");
    let face = remote_agent_face(dir.path()).await;
    let broker = CountingBroker::approving();
    let tool = super::make_subagent_tool(Vec::new()).with_mcp_agents(
        Some(Arc::clone(&face.registry)),
        Some(Arc::clone(&broker) as Arc<dyn UserInteractionBroker>),
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
        ExecutionBinding {
            turn_id: TurnId::new(),
            attempt_id: AttemptId::new(),
        },
    );
    tool.load_and_approve_mcp_agent("mcp__workspace__reviewface", Some(&owner))
        .await
        .expect("归属齐备且获批时必须完成激活");
    assert_eq!(broker.calls(), 1, "有归属时恰好一次审批交互");
}

/// [回归] 批准后先过审批门，再登记后台任务并完成执行。
#[tokio::test]
async fn mcp_agent_approval_then_background_starts_task() {
    let dir = tempfile::tempdir().unwrap();
    write_reviewface(dir.path(), None, "Review.");
    let (host, manager) = background_host(dir.path(), "mcp-bg-approved").await;
    let face = remote_agent_face(dir.path()).await;
    let broker = CountingBroker::approving();
    let tool = host.bind(super::make_subagent_tool(Vec::new()).with_mcp_agents(
        Some(face.registry),
        Some(Arc::clone(&broker) as Arc<dyn UserInteractionBroker>),
    ));
    let messages = vec![BaseMessage::human("parent context")];
    let ctx = model_context(&host, &messages);

    let receipt = tool
        .invoke(
            serde_json::json!({
                "subagent_type": "mcp__workspace__reviewface",
                "prompt": "remote task",
                "cwd": dir.path().to_str().unwrap(),
            }),
            ctx,
        )
        .await
        .expect("获批 MCP Agent 必须登记后台任务");
    assert!(
        receipt.contains("Background task"),
        "批准后必须后台启动：{receipt}"
    );
    assert_eq!(broker.calls(), 1, "首次启动前恰好一次审批");
    assert_eq!(manager.active_count(), 1, "批准后必须登记一个后台任务");

    let completed = host.wait_background_completion().await;
    assert!(
        completed.success,
        "获批 MCP Agent 的后台执行必须完成：{:?}",
        completed.output
    );
    assert_eq!(broker.calls(), 1, "后台执行不得再次审批");
}

/// [回归] 拒绝后零后台任务、零 child thread。
#[tokio::test]
async fn mcp_agent_rejection_registers_no_background_task() {
    let dir = tempfile::tempdir().unwrap();
    write_reviewface(dir.path(), None, "Review.");
    let (host, manager) = background_host(dir.path(), "mcp-bg-rejected").await;
    let face = remote_agent_face(dir.path()).await;
    let broker = CountingBroker::rejecting();
    let tool = host.bind(super::make_subagent_tool(Vec::new()).with_mcp_agents(
        Some(face.registry),
        Some(Arc::clone(&broker) as Arc<dyn UserInteractionBroker>),
    ));
    let messages = vec![BaseMessage::human("parent context")];
    let ctx = model_context(&host, &messages);

    let error = tool
        .invoke(
            serde_json::json!({
                "subagent_type": "mcp__workspace__reviewface",
                "prompt": "remote task",
                "cwd": dir.path().to_str().unwrap(),
            }),
            ctx,
        )
        .await
        .expect_err("拒绝必须在后台登记前失败")
        .to_string();
    assert!(
        error.contains("fixture rejected MCP agent activation"),
        "拒绝原因必须原样来自审批决策：{error}"
    );
    assert!(
        error.contains("Tool rejected"),
        "审批拒绝必须投影为 ToolRejected：{error}"
    );
    assert_eq!(broker.calls(), 1, "拒绝也必须恰好一次审批");
    assert_eq!(manager.active_count(), 0, "拒绝后不得登记后台任务");
    assert!(
        host.fixture
            .facade()
            .list_children(&host.parent_id)
            .await
            .unwrap()
            .is_empty(),
        "拒绝后不得创建 child thread"
    );
}

/// [回归] 定义内容或有效工具集变化 ⇒ 审批键变化，必须重新审批；
/// 未变化的重复调用不得重复打扰用户。
#[tokio::test]
async fn mcp_agent_content_or_tool_change_requires_reapproval() {
    let dir = tempfile::tempdir().unwrap();
    write_reviewface(dir.path(), Some("[Read]"), "Review v1.");
    let (host, manager) = background_host(dir.path(), "mcp-bg-reapproval").await;
    let face = remote_agent_face(dir.path()).await;
    let broker = CountingBroker::approving();
    let parent_tools: Vec<Arc<dyn BaseTool>> = vec![Arc::new(NamedTool("Read"))];
    let tool = host.bind(super::make_subagent_tool(parent_tools).with_mcp_agents(
        Some(face.registry),
        Some(Arc::clone(&broker) as Arc<dyn UserInteractionBroker>),
    ));

    let invoke = |prompt: &'static str| {
        let host = &host;
        let tool = &tool;
        let dir = dir.path().to_path_buf();
        async move {
            let messages = vec![BaseMessage::human("parent context")];
            let ctx = model_context(host, &messages);
            tool.invoke(
                serde_json::json!({
                    "subagent_type": "mcp__workspace__reviewface",
                    "prompt": prompt,
                    "cwd": dir.to_str().unwrap(),
                }),
                ctx,
            )
            .await
        }
    };

    invoke("first activation").await.expect("首次激活必须成功");
    assert_eq!(broker.calls(), 1, "首次激活必须审批一次");
    assert_eq!(manager.active_count(), 1);
    host.wait_background_completion().await;

    invoke("unchanged definition")
        .await
        .expect("未变化定义必须复用审批");
    assert_eq!(broker.calls(), 1, "未变化定义不得重复审批");
    host.wait_background_completion().await;

    // 内容变化：资源缓存失效后重新读取正文，digest 改变 ⇒ 审批键改变。
    write_reviewface(dir.path(), Some("[Read]"), "Review v2.");
    face.pool
        .invalidate_resource_cache("workspace", Some(&reviewface_uri()))
        .await;
    invoke("content changed")
        .await
        .expect("内容变化后重新获批必须成功");
    assert_eq!(broker.calls(), 2, "内容变化必须重新审批");
    host.wait_background_completion().await;

    // 有效工具集变化：声明从 Read 变为 Edit，过滤后工具集改变 ⇒ 审批键改变。
    write_reviewface(dir.path(), Some("[Edit]"), "Review v2.");
    face.pool
        .invalidate_resource_cache("workspace", Some(&reviewface_uri()))
        .await;
    invoke("tools changed")
        .await
        .expect("工具集变化后重新获批必须成功");
    assert_eq!(broker.calls(), 3, "有效工具集变化必须重新审批");
    host.wait_background_completion().await;
}
