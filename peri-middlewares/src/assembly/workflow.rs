//! Workflow agent 的独立装配端口；保留既有工具集与链序。
use crate::{
    hitl::HumanInTheLoopMiddleware,
    middleware::{FilesystemMiddleware, TerminalMiddleware, TodoMiddleware, WebMiddleware},
    permission::{default_requires_approval, PermissionMiddleware},
    skills::SkillsMiddleware,
    subagent::SkillPreloadMiddleware,
    workflow::WorkflowMiddleware,
    AgentDefineMiddleware, AgentsMdMiddleware, GitAttributionMiddleware,
};
use peri_acp_types::{
    ports::WorkflowMiddlewarePort,
    workflow::{AgentExecutor, ProgressEvent, WorkflowTaskResult},
};
use peri_agent::{
    agent::workflow::{WorkflowAgentContext, WorkflowAgentDefinition, WorkflowMiddlewareFactory},
    error_suggest::{ErrorSuggestRegistry, ToolRegistrySnapshot},
    middleware::r#trait::Middleware,
    tools::{BaseTool, ToolInvocationResolver},
};
use std::sync::Arc;

/// workflow agent 装配工厂（ZST：无状态装配器）。
pub struct WorkflowAgentMiddlewareFactory;

/// 构造 workflow agent 装配端口并 upcast（部署装配点调用；返回类型已锚定
/// 端口 trait，调用方无需引用 peri-agent 类型路径——TUI 等消费方只写
/// `peri_middlewares::assembly::default_workflow_middleware_factory()`）。
pub fn default_workflow_middleware_factory(
) -> Arc<dyn peri_agent::agent::workflow::WorkflowMiddlewareFactory> {
    Arc::new(WorkflowAgentMiddlewareFactory)
}

impl WorkflowMiddlewareFactory for WorkflowAgentMiddlewareFactory {
    fn resolve_agent_definition(
        &self,
        agent_type: &str,
        cwd: &str,
    ) -> Result<WorkflowAgentDefinition, String> {
        let project_path = AgentDefineMiddleware::candidate_paths(cwd, agent_type)
            .into_iter()
            .find(|path| path.is_file());
        let agent = if let Some(path) = project_path {
            let content = std::fs::read_to_string(&path).map_err(|error| {
                format!(
                    "failed to read agent definition '{}': {error}",
                    path.display()
                )
            })?;
            crate::parse_agent_file(&content)
                .ok_or_else(|| format!("failed to parse agent definition '{}'", path.display()))?
        } else {
            let built_in = crate::subagent::get_built_in_agent(agent_type)
                .ok_or_else(|| format!("cannot find agent definition '{agent_type}'"))?;
            crate::parse_agent_file(built_in.content).ok_or_else(|| {
                format!("failed to parse built-in agent definition '{agent_type}'")
            })?
        };
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
            crate::ToolsValue::Empty => None,
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

    fn build_tools(
        &self,
        cwd: &str,
        disabled: &std::collections::HashSet<String>,
    ) -> Vec<Box<dyn BaseTool>> {
        let mut tools: Vec<Box<dyn BaseTool>> = Vec::new();
        // MetaHarness（设计 §2.5）：关闭的 middleware 连坐，其工具不进列表。
        if !disabled.contains("FilesystemMiddleware") {
            tools.extend(FilesystemMiddleware::build_tools(cwd));
        }
        if !disabled.contains("TerminalMiddleware") {
            tools.extend(TerminalMiddleware::build_tools(cwd));
        }
        if !disabled.contains("WebMiddleware") {
            tools.extend(WebMiddleware::build_tools());
        }
        // Workflow agent 无 plugin_skill_roots，仅 project-level skill 可用。
        // 在注册工具前扫描 project skills，预填充缓存（SkillTool 无懒扫描回退）。
        // D3：统一模型可见协议为 SkillTool(skill_name) + DiscoverSkillsTool，
        // 与主 agent / subagent 链一致，不再注册旧 Skill(skill, args)。
        if !disabled.contains("SkillsMiddleware") {
            let project_skills_root = std::path::PathBuf::from(cwd).join(".claude").join("skills");
            let skills = crate::skills::loader::scan_skill_roots(&[crate::skills::SkillRoot {
                path: project_skills_root,
                source: crate::skills::SkillSource::Project,
                plugin_name: None,
            }]);
            let cached = std::sync::Arc::new(std::sync::RwLock::new(if skills.is_empty() {
                None
            } else {
                Some(skills)
            }));
            tools.push(Box::new(crate::skills::tools::SkillTool::new(Arc::clone(
                &cached,
            ))));
            tools.push(Box::new(crate::skills::tools::DiscoverSkillsTool::new(
                cached,
            )));
        }
        tools
    }

    fn build_sandbox_write_tool(
        &self,
        cwd: &str,
        allowed_dirs: &[String],
    ) -> Option<Box<dyn BaseTool>> {
        match crate::tools::filesystem::WriteSandboxTool::new(cwd, allowed_dirs.to_vec()) {
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
    ) -> Vec<Box<dyn Middleware>> {
        let mut middlewares: Vec<Box<dyn Middleware>> = Vec::new();

        // MetaHarness（设计 §2.5）：workflow agent 链独立装配，关闭面同样生效；
        // 未禁用项保持原相对顺序（行为契约，禁止重排）。
        let disabled = &ctx.meta_harness_disabled;

        if !disabled.contains("AgentsMdMiddleware") {
            let mut agents_md = AgentsMdMiddleware::new();
            if let Some(ref md) = ctx.frozen_claude_md {
                agents_md =
                    agents_md.with_frozen_content(md.clone(), ctx.frozen_claude_local_md.clone());
            }
            middlewares.push(Box::new(agents_md));
        }

        if !disabled.contains("SkillsMiddleware") {
            let mut skills_mw = SkillsMiddleware::new();
            if let Some(ref summary) = ctx.frozen_skill_summary {
                skills_mw = skills_mw.with_frozen_summary(summary.clone());
            }
            middlewares.push(Box::new(skills_mw));
        }

        // 与普通 subagent 一致：agent.md 声明的 skills 在启动时预加载。
        if !disabled.contains("SkillPreloadMiddleware") {
            middlewares.push(Box::new(SkillPreloadMiddleware::new(
                skill_names.to_vec(),
                &ctx.cwd,
            )));
        }

        if !disabled.contains("FilesystemMiddleware") {
            middlewares.push(Box::new(FilesystemMiddleware::new()));
        }

        // 3a. GitAttributionMiddleware（在 FilesystemMiddleware 之后）
        if !disabled.contains("GitAttributionMiddleware") {
            middlewares.push(Box::new(GitAttributionMiddleware::new(model_name)));
        }

        if !disabled.contains("TerminalMiddleware") {
            middlewares.push(Box::new(TerminalMiddleware::new()));
        }
        if !disabled.contains("WebMiddleware") {
            middlewares.push(Box::new(WebMiddleware::new()));
        }

        // 3b. TodoMiddleware（在 WebMiddleware 之后）
        if !disabled.contains("TodoMiddleware") {
            let (todo_tx, _todo_rx) = tokio::sync::mpsc::channel::<Vec<crate::tools::TodoItem>>(8);
            middlewares.push(Box::new(TodoMiddleware::new(todo_tx)));
        }

        // GAP-03: PermissionMiddleware（审批，原 HITL 审批职责）。
        // broker + permission_mode 均 Some 时启用审批（遵循 session 权限模式）；
        // 否则 Bypass（自主后台 agent 默认行为）。
        if !disabled.contains("PermissionMiddleware") {
            let permission = match (&ctx.broker, &ctx.permission_mode) {
                (Some(broker), Some(mode)) => PermissionMiddleware::with_shared_mode(
                    Arc::clone(broker),
                    default_requires_approval,
                    Arc::clone(mode),
                    None, // auto_classifier: workflow agent 不需要 LLM 分类器
                ),
                _ => PermissionMiddleware::disabled(),
            };
            middlewares.push(Box::new(permission));
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

    fn build_tool_resolver(&self) -> Arc<dyn ToolInvocationResolver> {
        Arc::new(crate::tool_search::ExecuteExtraToolResolver::default())
    }

    fn build_error_suggest(
        &self,
        cwd: &str,
        tool_names: &[String],
    ) -> (Arc<ErrorSuggestRegistry>, ToolRegistrySnapshot) {
        let agents_dir = std::path::Path::new(cwd).join(".claude").join("agents");
        let agents_dir_opt = if agents_dir.exists() {
            Some(agents_dir.as_path())
        } else {
            None
        };
        let snapshot = crate::error_suggest::build_tool_registry_snapshot(
            tool_names.iter().cloned(),
            agents_dir_opt,
        );
        (crate::error_suggest::build_default_registry(), snapshot)
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
