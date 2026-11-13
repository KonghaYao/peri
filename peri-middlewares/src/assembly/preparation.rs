//! 生产装配前的端口还原与父工具投影；所有句柄仍由 assemble 持有。
use super::{open_builtin_bridges, AssemblyContext};
use crate::{
    mcp::{McpClientPool, McpResourceTool},
    permission::{AutoClassifier, LlmAutoClassifier},
    tool_search::ToolSearchIndex,
};
use peri_acp_types::mcp_skills::McpSkillRegistry;
use peri_agent::session::factory::AssemblyError;
use peri_agent::tools::BaseTool;
use std::sync::Arc;

pub(super) struct ResolvedPorts {
    pub(super) mcp_pool_concrete: Option<Arc<McpClientPool>>,
    pub(super) mcp_agent_registry: Option<Arc<crate::mcp::McpAgentRegistry>>,
    pub(super) tool_search_index_concrete: Arc<ToolSearchIndex>,
    pub(super) auto_classifier: Option<Arc<dyn AutoClassifier>>,
}

pub(super) fn resolve_ports(ctx: &AssemblyContext) -> Result<ResolvedPorts, AssemblyError> {
    let AssemblyContext {
        mcp_pool,
        tool_search_index,
        auto_classifier_model,
        ..
    } = ctx;
    // L5：middlewares 具体类型经 peri-acp-types 端口接入，此处 downcast
    // 还原（端口实现方为本 crate）。端口存在但类型不符 ⇒ 显式错误，
    // 不构造空池/默认索引替代注入实例（否则装配产物与连接池/索引分离）。

    // MCP 连接池：端口 → Arc<McpClientPool>。None = 合法的无 MCP 装配
    //（print / 未声明 MCP）；Some 但类型不符 = 装配错误。
    let mcp_pool_concrete: Option<Arc<McpClientPool>> = match mcp_pool {
        None => None,
        Some(port) => Some(
            // 对象自身 vtable 的 TypeId 决定还原；包装器不会被误认成 McpClientPool。
            Arc::clone(port)
                .downcast_arc::<McpClientPool>()
                .map_err(|_| AssemblyError::PortMismatch {
                    port: "McpPoolPort",
                    expected: "McpClientPool",
                })?,
        ),
    };
    // W5：Agent registry（会话可见性过滤 + 链槽关闭位）经**唯一构造入口**
    // `McpAgentRegistry::for_session` 成型——关闭位在入口内由同一份
    // `meta_harness_disabled` 派生（`SubAgentMiddleware`，与 A24 关闭集同源），
    // 本文件不留第二事实源。
    let mcp_agent_registry = mcp_pool_concrete.as_ref().map(|pool| {
        Arc::new(crate::mcp::McpAgentRegistry::for_session(
            Arc::clone(pool),
            &ctx.session_id,
            &ctx.meta_harness_disabled,
        ))
    });
    // 候选目录端口绑定（W5）：ACP 只持 `Arc<dyn AgentCatalogPort>`，具体实现的
    // 绑定在装配点单次完成——registry 与 SubAgent 消费面是**同一份**（`Arc::ptr_eq`
    // 可观察），不再有第二个来源或第二个缓存。存在 registry 时端口必须可绑定，
    // 否则目录会静默为空。
    if let Some(registry) = mcp_agent_registry.as_ref() {
        let port = Arc::clone(&ctx.agent_catalog)
            .downcast_arc::<crate::host_ports::AgentCatalogProvider>()
            .map_err(|_| AssemblyError::PortMismatch {
                port: "AgentCatalogPort",
                expected: "AgentCatalogProvider",
            })?;
        port.bind(Arc::clone(registry));
    }

    // 工具搜索索引：端口 → Arc<ToolSearchIndex>（类型不符 ⇒ 显式错误，
    // 不再构造默认索引：注入的索引与装配产物必须是同一实例）。
    let tool_search_index_concrete: Arc<ToolSearchIndex> = Arc::clone(tool_search_index)
        .downcast_arc::<ToolSearchIndex>()
        .map_err(|_| AssemblyError::PortMismatch {
            port: "ToolSearchPort",
            expected: "ToolSearchIndex",
        })?;

    // HITL middleware — reuse auto_classifier model from cache when available
    let auto_classifier: Option<Arc<dyn AutoClassifier>> = Some(Arc::new(LlmAutoClassifier::new(
        auto_classifier_model.clone(),
    )));

    Ok(ResolvedPorts {
        mcp_pool_concrete,
        mcp_agent_registry,
        tool_search_index_concrete,
        auto_classifier,
    })
}

pub(super) fn build_parent_tools(
    ctx: &AssemblyContext,
    mcp_pool_concrete: &Option<Arc<McpClientPool>>,
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
    if !disabled.contains("McpMiddleware") {
        if let Some(ref pool) = mcp_pool_concrete {
            parent_tools.extend(open_builtin_bridges(pool, disabled));
            if pool.has_resources() {
                parent_tools.push(Box::new(
                    McpResourceTool::new(
                        Arc::clone(pool),
                        // 未装配 session 注册表（print 模式）→ 空注册表
                        //（无条目 = 不校验）
                        mcp_skill_registry
                            .clone()
                            .unwrap_or_else(|| Arc::new(McpSkillRegistry::new())),
                    )
                    .with_session_id(ctx.session_id.clone()),
                ));
            }
        }
    }

    parent_tools
}
