//! 生产装配前的端口还原与父工具投影；所有句柄仍由 assemble 持有。
use super::{AssemblyContext, ChainSlot};
use crate::mcp::McpClientPool;
use crate::mcp::McpResourceTool;
use crate::permission::AutoClassifier;
use crate::permission::LlmAutoClassifier;
use crate::tool_search::ToolSearchIndex;
use peri_acp_types::mcp_skills::McpSkillRegistry;
use peri_agent::session::factory::AssemblyError;
use peri_agent::tools::BaseTool;
use std::sync::Arc;

pub(super) struct ResolvedPorts {
    pub(super) mcp_pool_concrete: Option<Arc<McpClientPool>>,
    pub(super) mcp_agent_registry: Option<Arc<crate::mcp::McpAgentRegistry>>,
    pub(super) tool_search_index_concrete: Option<Arc<ToolSearchIndex>>,
    pub(super) auto_classifier: Option<Arc<dyn AutoClassifier>>,
}

pub(super) fn resolve_ports(
    ctx: &AssemblyContext,
    selected: &[ChainSlot],
) -> Result<ResolvedPorts, AssemblyError> {
    let uses_pool = selected.iter().any(|slot| {
        matches!(
            slot,
            ChainSlot::Mcp
                | ChainSlot::SubAgent
                | ChainSlot::AtMention
                | ChainSlot::Image
                | ChainSlot::GitAttribution
        )
    });
    let mcp_pool_concrete = ctx
        .mcp_pool
        .as_ref()
        .filter(|_| uses_pool)
        .map(|port| {
            Arc::clone(port)
                .downcast_arc::<McpClientPool>()
                .map_err(|_| AssemblyError::PortMismatch {
                    port: "McpPoolPort",
                    expected: "McpClientPool",
                })
        })
        .transpose()?;
    let tool_search_index_concrete = selected
        .contains(&ChainSlot::ToolSearch)
        .then(|| {
            Arc::clone(&ctx.tool_search_index)
                .downcast_arc::<ToolSearchIndex>()
                .map_err(|_| AssemblyError::PortMismatch {
                    port: "ToolSearchPort",
                    expected: "ToolSearchIndex",
                })
        })
        .transpose()?;

    // Validate every consumed port before mutating the shared catalog.
    let mcp_agent_registry = mcp_pool_concrete
        .as_ref()
        .filter(|_| selected.contains(&ChainSlot::SubAgent))
        .map(|pool| {
            let port = Arc::clone(&ctx.agent_catalog)
                .downcast_arc::<crate::host_ports::AgentCatalogProvider>()
                .map_err(|_| AssemblyError::PortMismatch {
                    port: "AgentCatalogPort",
                    expected: "AgentCatalogProvider",
                })?;
            let registry = Arc::new(crate::mcp::McpAgentRegistry::for_session(
                Arc::clone(pool),
                &ctx.session_id,
                &ctx.meta_harness_disabled,
            ));
            port.bind(Arc::clone(&registry));
            Ok::<_, AssemblyError>(registry)
        })
        .transpose()?;

    let auto_classifier = selected.contains(&ChainSlot::Permission).then(|| {
        Arc::new(LlmAutoClassifier::new(ctx.auto_classifier_model.clone()))
            as Arc<dyn AutoClassifier>
    });

    Ok(ResolvedPorts {
        mcp_pool_concrete,
        mcp_agent_registry,
        tool_search_index_concrete,
        auto_classifier,
    })
}

pub(super) fn build_parent_tools(
    ctx: &AssemblyContext,
    mcp_pool_concrete: Option<&Arc<McpClientPool>>,
) -> Vec<Box<dyn BaseTool>> {
    let AssemblyContext {
        mcp_skill_registry,
        meta_harness_disabled: disabled,
        ..
    } = ctx;
    // 父工具集（供子 agent 继承）。MetaHarness：父工具按持有 middleware
    // 分支构造——关闭的 middleware 连坐，其工具不进入 parent_tools
    // （设计 §2.5"关闭面 = 全部装配入口"）。
    //
    // v4-part-4 W3-C1：7 个文件/终端裸名（`Read` / `Write` / `Edit` / `Glob` / `Grep` /
    // `folder_operations` / `Bash`）的**裸名来源已从本面摘除**（原 `FilesystemMiddleware`
    // / `TerminalMiddleware` 两段 `build_tools` 连坐块已删）。它们的唯一来源是下一段的
    // builtin direct bridge——`open_builtin_bridges` 按注册表遍历**全部**实例，`workspace`
    // 实例的 7 项（声明 `direct: true`）因此自动进入 parent_tools，关闭键为
    // `WorkspaceMiddleware`（注册表 `policy_key`，与 Web/Artifact 同一条路径）。
    let mut parent_tools: Vec<Box<dyn BaseTool>> = Vec::new();
    if let Some(pool) = mcp_pool_concrete {
        parent_tools.extend(super::open_builtin_bridges(pool, disabled));
        if pool.has_resources() {
            parent_tools.push(Box::new(
                McpResourceTool::new(
                    Arc::clone(pool),
                    mcp_skill_registry
                        .clone()
                        .unwrap_or_else(|| Arc::new(McpSkillRegistry::new())),
                )
                .with_session_id(ctx.session_id.clone()),
            ));
        }
    }

    parent_tools
}
