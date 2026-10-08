//! Workflow agent 的独立装配端口；保留既有工具集与链序。
//!
//! v4-part-4 W3-C1：**无池侧不再有文件/终端工具**（原 `FilesystemMiddleware` /
//! `TerminalMiddleware` 两段已删）。7 个 workspace 工具的唯一提供面是
//! [`BuiltinWorkflowAgentFactory::builtin_tools`]（`open_builtin_bridges`，走注册表
//! `direct` 声明与 `policy_key` 关闭集）——因此本模块在无池/无 McpMiddleware 时
//! 不提供 `Read` / `Write` / `Edit` / `Glob` / `Grep` / `folder_operations` / `Bash`
//! （与 Web/Artifact 的 A6 面③语义同构，见 [`WorkflowAgentMiddlewareFactory`]）。
use crate::{
    hitl::HumanInTheLoopMiddleware, mcp::McpClientPool, middleware::TodoMiddleware,
    permission::PermissionMiddleware, skills::SkillsMiddleware, subagent::SkillPreloadMiddleware,
    workflow::WorkflowMiddleware, AgentsMdMiddleware, GitAttributionMiddleware,
};
use peri_acp_types::{
    ports::WorkflowMiddlewarePort,
    workflow::{AgentExecutor, ProgressEvent, WorkflowTaskResult},
};
use peri_agent::{
    agent::workflow::{WorkflowAgentContext, WorkflowAgentDefinition, WorkflowMiddlewareFactory},
    middleware::r#trait::Middleware,
    tools::{BaseTool, ToolInvocationResolver},
};
use std::sync::Arc;

/// workflow agent 装配工厂（无池 ZST；既有调用点语义逐位不变）。
///
/// 生产装配要走 builtin 提供面（A6 面③）时用
/// [`default_workflow_middleware_factory_with_pool`]；本 ZST 保留为「无池」构造，
/// 保持既有调用点（宿主注入点 / 装配测试的单元值构造）签名与行为不变。
pub struct WorkflowAgentMiddlewareFactory;

/// 持有 builtin pool 的装配器（A6 面③）。
///
/// 覆写 [`WorkflowMiddlewareFactory::build_tools`]：在既有工具集之后追加 builtin
/// 实例声明的 direct bridge（WebSearch / WebFetch / artifact），并按同一份
/// frozen policy 过滤关闭实例；独立中间件链复用同一池的 Workspace 内容读取端口。
/// 其余方法与无池工厂逐位一致（delegate）。
///
/// **非 public**：宿主只经 [`default_workflow_middleware_factory_with_pool`] 拿到
/// upcast 后的端口对象（不新增 public 类型）。
struct BuiltinWorkflowAgentFactory {
    builtin: Option<Arc<McpClientPool>>,
    /// 会话级 MCP Agent registry（W5）：workflow agent 定义**只服务本地受信来源**
    /// （project / plugin / builtin）。远端 `mcp__*` 需要内容绑定批准 seam，本面
    /// 没有 broker ⇒ 显式拒绝（见 `resolve_agent_definition_via_registry`）。
    /// 关闭面由 `WorkflowMiddleware` 承担（Agent 工具面关闭位不影响本路径）。
    agent_registry: Option<Arc<crate::mcp::McpAgentRegistry>>,
}

fn build_workflow_middlewares(
    ctx: &WorkflowAgentContext,
    model_name: &str,
    skill_names: &[String],
    pool: Option<Arc<McpClientPool>>,
) -> Vec<Box<dyn Middleware>> {
    let reader: Arc<dyn crate::workspace_io::WorkspaceFileReader> =
        Arc::new(crate::workspace_io::McpWorkspaceFileReader::new(
            pool,
            ctx.session_id.clone(),
            &ctx.meta_harness_disabled,
        ));
    let mut middlewares: Vec<Box<dyn Middleware>> = Vec::new();

    // MetaHarness（设计 §2.5）：workflow agent 链独立装配，关闭面同样生效；
    // 未禁用项保持原相对顺序（行为契约，禁止重排）。
    let disabled = &ctx.meta_harness_disabled;

    if !disabled.contains("AgentsMdMiddleware") {
        // M4：main / local 独立贡献（local-only 有效）。
        let agents_md = AgentsMdMiddleware::new().with_frozen_parts(
            ctx.frozen_claude_md.clone(),
            ctx.frozen_claude_local_md.clone(),
        );
        middlewares.push(Box::new(agents_md));
    }

    if !disabled.contains("SkillsMiddleware") {
        let mut skills_mw =
            SkillsMiddleware::new().with_mcp_registry(ctx.mcp_skill_registry.clone());
        if let Some(ref summary) = ctx.frozen_skill_summary {
            skills_mw = skills_mw.with_frozen_summary(summary.clone());
        }
        middlewares.push(Box::new(skills_mw));
    }

    // 与普通 subagent 一致：agent.md 声明的 skills 在启动时预加载。
    // W4b（F5/J5）：预载只查会话级 MCP registry（与主链同一份）。
    if !disabled.contains("SkillPreloadMiddleware") {
        middlewares.push(Box::new(
            SkillPreloadMiddleware::new(skill_names.to_vec())
                .with_mcp_registry(ctx.mcp_skill_registry.clone()),
        ));
    }

    // 3a. GitAttributionMiddleware
    // v4-part-4 W3-C1：原 FilesystemMiddleware 段（此处之前）已删除，本槽位不再有
    // 「在 Filesystem 之后」的位置语义；workspace 工具面改由 `build_tools` 的
    // builtin direct bridge 提供，链上不再有文件工具提供器。
    if !disabled.contains("GitAttributionMiddleware") {
        middlewares.push(Box::new(GitAttributionMiddleware::new(
            model_name,
            reader.clone(),
        )));
    }

    // 3b. TodoMiddleware
    // v4-part-4 W3-C1：原 TerminalMiddleware 段（此处之前）已删除，本槽位不再有
    // 「在 Terminal 之后」的位置语义；`_execution_manager` 形参因此在本函数内没有
    // 消费点，保留以维持端口签名不变。
    // A6 面③：Web 槽位已随 `WebMiddleware` 提供面删除，且此处**不**补 MCP
    // middleware（workflow 链不引入 deferred / ToolSearch 语义）；Web / Artifact
    // 与 workspace 7 工具的能力改由 `build_tools` 的 builtin direct bridge 提供。
    if !disabled.contains("TodoMiddleware") {
        let (todo_tx, _todo_rx) = tokio::sync::mpsc::channel::<Vec<crate::tools::TodoItem>>(8);
        middlewares.push(Box::new(TodoMiddleware::new(todo_tx)));
    }

    // GAP-03: PermissionMiddleware（审批，原 HITL 审批职责）。
    // broker + permission_mode 均 Some 时启用审批（遵循 session 权限模式）；
    // 否则 Bypass（自主后台 agent 默认行为）。
    if !disabled.contains("PermissionMiddleware") {
        // 有效模式规则归 `PermissionMiddleware`（H2/D3）：broker + 共享 mode 齐备
        // ⇒ 共享模式审批；否则 disabled。段落投影消费同一规则。
        middlewares.push(Box::new(PermissionMiddleware::for_workflow(
            ctx.broker.clone(),
            ctx.permission_mode.clone(),
        )));
    }
    // 提问通道（新 HumanInTheLoopMiddleware，含 AskUserQuestion）：
    // workflow agent 的 broker 恒 None（advisor 裁决 B：workflow 链不
    // 装配 HITL，`workflow_agent.rs` / `agent.rs` 构造点），此处不装配
    // ——AskUserQuestion 随 2026-08-15 拆分从 workflow agent 消失
    // （旧行为经宿主级 shared_tools 泄漏 TUI broker 到后台 agent，
    // 非有意设计，见 spec/issues/2026-08-15-permission-hitl-split.md）。
    if !disabled.contains("HumanInTheLoopMiddleware") {
        if let Some(broker) = &ctx.broker {
            middlewares.push(Box::new(HumanInTheLoopMiddleware::new(Arc::clone(broker))));
        }
    }

    // [v2] CompactMiddleware 已移除——Workflow agent 的自动 compact 由 v2
    // stages/compact.rs 统一接管（run_react_loop 在每轮开头调 compact_v2::run_compact）。

    middlewares
}

/// 构造 workflow agent 装配端口并 upcast（部署装配点调用；返回类型已锚定
/// 端口 trait，调用方无需引用 peri-agent 类型路径——TUI 等消费方只写
/// `peri_middlewares::assembly::default_workflow_middleware_factory()`）。
pub fn default_workflow_middleware_factory(
) -> Arc<dyn peri_agent::agent::workflow::WorkflowMiddlewareFactory> {
    Arc::new(WorkflowAgentMiddlewareFactory)
}

/// 生产装配入口（A6 面③）：把 deployment MCP pool 交给 workflow agent 的装配器，
/// 使它的工具列表仍含 Web / Artifact 能力（原名 direct bridge）。
///
/// Bare 传入仅含 workspace 的池；`None`（无 pool）时的行为与
/// [`default_workflow_middleware_factory`] 一致。
pub fn default_workflow_middleware_factory_with_pool(
    pool: Option<Arc<McpClientPool>>,
) -> Arc<dyn peri_agent::agent::workflow::WorkflowMiddlewareFactory> {
    let agent_registry = pool
        .as_ref()
        .map(|pool| Arc::new(crate::mcp::McpAgentRegistry::new(Arc::clone(pool))));
    Arc::new(BuiltinWorkflowAgentFactory {
        builtin: pool,
        agent_registry,
    })
}

impl BuiltinWorkflowAgentFactory {
    /// builtin 提供面：`build_tools` 的追加段（关闭集过滤见
    /// [`crate::assembly::open_builtin_bridges`]）。
    ///
    /// `McpMiddleware` 关闭时整个 MCP 提供面连坐（与主链「槽位不构造」等价），
    /// 其余 builtin 实例关闭走注册表 `policy_key`。
    fn builtin_tools(
        &self,
        disabled: &std::collections::HashSet<String>,
    ) -> Vec<Box<dyn BaseTool>> {
        if disabled.contains("McpMiddleware") {
            return Vec::new();
        }
        match self.builtin.as_ref() {
            Some(pool) => crate::assembly::open_builtin_bridges(pool, disabled),
            None => Vec::new(),
        }
    }
}

#[async_trait::async_trait]
impl WorkflowMiddlewareFactory for BuiltinWorkflowAgentFactory {
    fn mcp_pool(&self) -> Option<Arc<dyn peri_acp_types::ports::McpPoolPort>> {
        self.builtin
            .as_ref()
            .map(|pool| Arc::clone(pool) as Arc<dyn peri_acp_types::ports::McpPoolPort>)
    }

    async fn resolve_agent_definition(
        &self,
        agent_type: &str,
        cwd: &str,
    ) -> Result<WorkflowAgentDefinition, String> {
        resolve_agent_definition_via_registry(self.agent_registry.as_ref(), agent_type, cwd).await
    }

    fn build_tools(
        &self,
        cwd: &str,
        disabled: &std::collections::HashSet<String>,
        execution_manager: Option<Arc<dyn peri_acp_types::tasks::TaskManager>>,
        mcp_skill_registry: Option<Arc<peri_acp_types::mcp_skills::McpSkillRegistry>>,
    ) -> Vec<Box<dyn BaseTool>> {
        let mut tools = WorkflowAgentMiddlewareFactory::workflow_tools(
            cwd,
            disabled,
            execution_manager,
            mcp_skill_registry,
        );
        tools.extend(self.builtin_tools(disabled));
        tools
    }

    fn build_sandbox_write_tool(
        &self,
        cwd: &str,
        allowed_dirs: &[String],
    ) -> Option<Box<dyn BaseTool>> {
        WorkflowAgentMiddlewareFactory.build_sandbox_write_tool(cwd, allowed_dirs)
    }

    fn build_middlewares(
        &self,
        ctx: &WorkflowAgentContext,
        model_name: &str,
        skill_names: &[String],
        _execution_manager: Option<Arc<dyn peri_acp_types::tasks::TaskManager>>,
    ) -> Vec<Box<dyn Middleware>> {
        build_workflow_middlewares(ctx, model_name, skill_names, self.builtin.clone())
    }

    fn build_tool_resolver(&self) -> Arc<dyn ToolInvocationResolver> {
        WorkflowAgentMiddlewareFactory.build_tool_resolver()
    }

    fn build_workflow_middleware(
        &self,
        executor: Arc<dyn AgentExecutor>,
        cwd: &str,
        notification_tx: tokio::sync::broadcast::Sender<WorkflowTaskResult>,
        progress_rx: Option<tokio::sync::mpsc::UnboundedReceiver<ProgressEvent>>,
    ) -> Arc<dyn WorkflowMiddlewarePort> {
        WorkflowAgentMiddlewareFactory.build_workflow_middleware(
            executor,
            cwd,
            notification_tx,
            progress_rx,
        )
    }
}

impl WorkflowAgentMiddlewareFactory {
    /// workflow agent 的基础工具集（无 builtin 提供面）。
    ///
    /// 迁移后**不含**裸名 Web 工具：Web / Artifact 能力由 builtin 实例以
    /// 原名 direct bridge 的形式提供（A6 面③，见
    /// [`BuiltinWorkflowAgentFactory`] 与 [`crate::assembly::open_builtin_bridges`]）。
    ///
    /// v4-part-4 W3-C1：**同样不含** 7 个 workspace 裸名（`Read` / `Write` / `Edit` /
    /// `Glob` / `Grep` / `folder_operations` / `Bash`）——它们的提供面已迁 builtin
    /// `workspace` 实例，经 `BuiltinWorkflowAgentFactory::builtin_tools`（即
    /// `open_builtin_bridges`）以原名提供，关闭键为该实例的
    /// `policy_key`（`WorkspaceMiddleware`）。因此 `execution_manager` 在本函数内
    /// 已无消费点（原 `TerminalMiddleware::build_tools_with_registry` 是唯一消费者）；
    /// 形参保留以维持端口签名（`WorkflowMiddlewareFactory::build_tools`）不变。
    /// workflow agent 的基础工具集（无 builtin 提供面）。
    ///
    /// 迁移后**不含**裸名 Web 工具：Web / Artifact 能力由 builtin 实例以
    /// 原名 direct bridge 的形式提供（A6 面③，见
    /// [`BuiltinWorkflowAgentFactory`] 与 [`crate::assembly::open_builtin_bridges`]）。
    ///
    /// v4-part-4 W3-C1：**同样不含** 7 个 workspace 裸名（`Read` / `Write` / `Edit` /
    /// `Glob` / `Grep` / `folder_operations` / `Bash`）——它们的提供面已迁 builtin
    /// `workspace` 实例，经 `BuiltinWorkflowAgentFactory::builtin_tools`（即
    /// `open_builtin_bridges`）以原名提供，关闭键为该实例的
    /// `policy_key`（`WorkspaceMiddleware`）。因此 `execution_manager` 在本函数内
    /// 已无消费点（原 `TerminalMiddleware::build_tools_with_registry` 是唯一消费者）；
    /// 形参保留以维持端口签名（`WorkflowMiddlewareFactory::build_tools`）不变。
    ///
    /// W4b（F4/J5）：**本地扫描已删除**——两个技能工具在调用时直接读会话级
    /// MCP skill registry 的当前投影（`registry` 形参；workflow agent 与主链
    /// 消费同一份目录，不再只看 project-level，也不再经过 before_agent 快照）。
    fn workflow_tools(
        _cwd: &str,
        disabled: &std::collections::HashSet<String>,
        _execution_manager: Option<Arc<dyn peri_acp_types::tasks::TaskManager>>,
        mcp_skill_registry: Option<Arc<peri_acp_types::mcp_skills::McpSkillRegistry>>,
    ) -> Vec<Box<dyn BaseTool>> {
        let mut tools: Vec<Box<dyn BaseTool>> = Vec::new();
        // MetaHarness（设计 §2.5）：关闭的 middleware 连坐，其工具不进列表。
        // D3：统一模型可见协议为 SkillTool(skill_name) + DiscoverSkillsTool，
        // 与主 agent / subagent 链一致，不再注册旧 Skill(skill, args)。
        if !disabled.contains("SkillsMiddleware") {
            tools.push(Box::new(crate::skills::tools::SkillTool::new(
                mcp_skill_registry.clone(),
            )));
            tools.push(Box::new(crate::skills::tools::DiscoverSkillsTool::new(
                mcp_skill_registry,
            )));
        }
        tools
    }
}

/// Workflow agent 定义解析（W5）：与 SubAgent 同一来源与同一优先级。
///
/// 本地三来源经会话级 [`crate::mcp::McpAgentRegistry`]（builtin `workspace`
/// 实例的 `agent://{scope}/{id}/agent.md`）读取，优先级 project → builtin →
/// plugin（E13）；远端 `mcp__{server}__{id}` 走同一 registry 的远端条目。
/// registry 缺席（无池装配 / 测试 ZST）⇒ 明确错误，不回落磁盘。
async fn resolve_agent_definition_via_registry(
    registry: Option<&Arc<crate::mcp::McpAgentRegistry>>,
    agent_type: &str,
    _cwd: &str,
) -> Result<WorkflowAgentDefinition, String> {
    // F2（安全）：本面**没有**批准 seam（唯一的远端激活批准门在
    // `subagent/tool/mcp_activation.rs` 与 `execute_resume.rs`，都需要 broker）。
    // workflow 路径在 W5 之前只支持本地盘 + builtin，因此这里显式拒绝远端 id，
    // 不把「无批准即可读远端正文」的能力开出去。
    if agent_type.starts_with("mcp__") {
        return Err(format!(
            "remote agent definitions are not available on the workflow path \
             (no approval seam); use a local agent definition instead of '{agent_type}'"
        ));
    }
    let Some(registry) = registry else {
        return Err(format!(
            "agent definitions are unavailable (MCP workspace face is not assembled); cannot resolve '{agent_type}'"
        ));
    };
    let agent = registry
        .activate(agent_type, true)
        .await
        .map_err(|error| format!("cannot find agent definition '{agent_type}': {error}"))?
        .definition;
    let frontmatter = agent.frontmatter;
    let prompt_overrides = {
        let overrides = crate::AgentOverrides {
            persona: (!agent.system_prompt.is_empty()).then_some(agent.system_prompt),
            tone: frontmatter.tone.clone(),
            proactiveness: frontmatter.proactiveness.clone(),
            mode: frontmatter.prompt_mode.clone(),
        };
        (!overrides.is_empty()).then_some(overrides)
    };
    let model = frontmatter
        .model
        .filter(|model| !model.is_empty() && model != "inherit");
    let allowed_tools = match frontmatter.tools {
        peri_mcp_core::agent_definition::ToolsValue::Empty => None,
        tools => Some(tools.to_vec()),
    };
    Ok(WorkflowAgentDefinition {
        model,
        allowed_tools,
        disallowed_tools: frontmatter.disallowed_tools.to_vec(),
        skill_names: frontmatter.skills,
        allowed_write_dirs: frontmatter.allowed_write_dirs,
        max_iterations: frontmatter.max_turns.unwrap_or(200) as usize,
        prompt_overrides,
    })
}

#[async_trait::async_trait]
impl WorkflowMiddlewareFactory for WorkflowAgentMiddlewareFactory {
    async fn resolve_agent_definition(
        &self,
        agent_type: &str,
        cwd: &str,
    ) -> Result<WorkflowAgentDefinition, String> {
        resolve_agent_definition_via_registry(None, agent_type, cwd).await
    }

    fn build_tools(
        &self,
        cwd: &str,
        disabled: &std::collections::HashSet<String>,
        execution_manager: Option<Arc<dyn peri_acp_types::tasks::TaskManager>>,
        mcp_skill_registry: Option<Arc<peri_acp_types::mcp_skills::McpSkillRegistry>>,
    ) -> Vec<Box<dyn BaseTool>> {
        Self::workflow_tools(cwd, disabled, execution_manager, mcp_skill_registry)
    }

    fn build_sandbox_write_tool(
        &self,
        cwd: &str,
        allowed_dirs: &[String],
    ) -> Option<Box<dyn BaseTool>> {
        match peri_mcp_workspace::filesystem::WriteSandboxTool::new(cwd, allowed_dirs.to_vec()) {
            Ok(tool) => Some(Box::new(tool)),
            Err(error) => {
                tracing::warn!(
                    %error,
                    sandbox_dirs = ?allowed_dirs,
                    "workflow agent: failed to construct SandboxWrite"
                );
                None
            }
        }
    }

    fn build_middlewares(
        &self,
        ctx: &WorkflowAgentContext,
        model_name: &str,
        skill_names: &[String],
        _execution_manager: Option<Arc<dyn peri_acp_types::tasks::TaskManager>>,
    ) -> Vec<Box<dyn Middleware>> {
        build_workflow_middlewares(ctx, model_name, skill_names, None)
    }

    fn build_tool_resolver(&self) -> Arc<dyn ToolInvocationResolver> {
        Arc::new(crate::tool_search::ExecuteExtraToolResolver::default())
    }

    fn build_workflow_middleware(
        &self,
        executor: Arc<dyn AgentExecutor>,
        cwd: &str,
        notification_tx: tokio::sync::broadcast::Sender<WorkflowTaskResult>,
        progress_rx: Option<tokio::sync::mpsc::UnboundedReceiver<ProgressEvent>>,
    ) -> Arc<dyn WorkflowMiddlewarePort> {
        Arc::new(WorkflowMiddleware::new(
            executor,
            cwd,
            notification_tx,
            progress_rx,
        ))
    }
}
