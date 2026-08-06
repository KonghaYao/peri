use std::path::{Path, PathBuf};

use async_trait::async_trait;
use peri_agent::{
    error::AgentResult,
    middleware::{r#trait::Middleware, state::MiddlewareState},
};

pub use peri_acp_types::agents::AgentOverrides;

use crate::parse_agent_file;

/// AgentDefineMiddleware - 根据 agent_id 注入 Claude Code Agent 定义文件
///
/// Agent 定义文件搜索路径（按优先级）：
/// 1. `{cwd}/.claude/agents/{agent_id}/agent.md`
/// 2. `{cwd}/.claude/agents/{agent_id}.md`
/// 3. `{cwd}/agents/{agent_id}/agent.md`
/// 4. `{cwd}/agents/{agent_id}.md`
///
/// Agent 定义文件格式（Claude Code YAML frontmatter）：
/// ```markdown
/// ---
/// name: code-reviewer
/// description: Reviews code for quality and best practices
/// tools: Read, Glob, Grep
/// tone: |
///   Be thorough and explain your reasoning in detail.
/// proactiveness: |
///   Proactively review related files and suggest improvements.
/// ---
///
/// You are a code reviewer. Focus on code quality and best practices.
/// ```
pub struct AgentDefineMiddleware;

impl AgentDefineMiddleware {
    pub fn new() -> Self {
        Self
    }

    /// 根据 cwd 和 agent_id 构建候选路径列表
    ///
    /// 如果 agent_id 包含路径分隔符或 `..`，返回空列表以防止路径遍历。
    pub fn candidate_paths(cwd: &str, agent_id: &str) -> Vec<PathBuf> {
        if agent_id.is_empty()
            || agent_id.contains('/')
            || agent_id.contains('\\')
            || agent_id.contains("..")
        {
            return Vec::new();
        }
        let cwd = Path::new(cwd);
        vec![
            cwd.join(".claude")
                .join("agents")
                .join(agent_id)
                .join("agent.md"),
            cwd.join(".claude")
                .join("agents")
                .join(format!("{}.md", agent_id)),
            cwd.join("agents").join(agent_id).join("agent.md"),
            cwd.join("agents").join(format!("{}.md", agent_id)),
        ]
    }

    /// 按优先级找到第一个存在的文件
    fn find_file(cwd: &str, agent_id: &str) -> Option<PathBuf> {
        Self::candidate_paths(cwd, agent_id)
            .into_iter()
            .find(|p| p.is_file())
    }

    /// 同步读取 agent.md，返回可覆盖 system prompt 的各个部分。
    ///
    /// 供 TUI 层在构建 LLM 前提前获取覆盖内容，传入 `build_system_prompt`。
    /// 返回 `None` 表示文件不存在或无有效内容。
    pub fn load_overrides(cwd: &str, agent_id: &str) -> Option<AgentOverrides> {
        let path = Self::find_file(cwd, agent_id)?;
        let content = std::fs::read_to_string(&path).ok()?;
        if content.trim().is_empty() {
            return None;
        }

        if let Some(agent) = parse_agent_file(&content) {
            let persona = if agent.system_prompt.is_empty() {
                None
            } else {
                Some(agent.system_prompt)
            };
            let overrides = AgentOverrides {
                persona,
                tone: agent.frontmatter.tone,
                proactiveness: agent.frontmatter.proactiveness,
                mode: agent.frontmatter.prompt_mode,
            };
            if overrides.is_empty() {
                return None;
            }
            return Some(overrides);
        }

        // 没有有效 frontmatter，把整个文件内容当作 persona
        let text = content.trim().to_string();
        if text.is_empty() {
            None
        } else {
            Some(AgentOverrides {
                persona: Some(text),
                ..Default::default()
            })
        }
    }
}

impl Default for AgentDefineMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for AgentDefineMiddleware {
    fn name(&self) -> &str {
        "AgentDefineMiddleware"
    }

    async fn before_agent(&self, _state: &mut dyn MiddlewareState) -> AgentResult<()> {
        // 覆盖注入已在构建 LLM 时通过 build_system_prompt(overrides, cwd) 完成，
        // 中间件层无需再操作消息列表。
        //
        // [设计说明 /agent 覆盖功能停用]（crate-audit 2026-07-06 P2-5 调查结论 A）
        //
        // `/agent` 覆盖功能在**主 Agent 链路**显式停用（executor.rs 内
        // `agent_overrides: None` 硬编码）。这是有意设计而非 bug：
        //
        // 1. **主 Agent 无 `/agent` 身份**：主 Agent 不携带 persona/tone/proactiveness，
        //    仅 SubAgent 才有（来自 `.claude/agents/{agent_id}.md` 的 frontmatter）。
        // 2. **SubAgent 覆盖走工具调用路径**：`SubAgentTool::invoke` →
        //    `overrides_from_agent_def`（`build_agent.rs:136`）从已解析的 agent 文件
        //    抽取 overrides，再通过 `system_builder`（`builder.rs:365`）传给
        //    `build_system_prompt(overrides, cwd)`。SubAgent **不需要**本中间件注入。
        // 3. **`load_overrides` 仅测试调用**：本中间件的 `load_overrides` 从磁盘读
        //    `.claude/agents/{id}.md`，但生产 SubAgent 路径已通过工具调用解析获取
        //    overrides，无需中途重读盘（与 CLAUDE.md [TRAP]「SubAgent 必须复用 main
        //    agent frozen 的 CLAUDE.md/Skills，禁止重新读盘」一致）。保留 `load_overrides`
        //    作为 helper / 测试入口。
        //
        // 详见 `docs/design/peri-agent-system-prompt-v2.md` §125-136。
        Ok(())
    }
}

#[cfg(test)]
#[path = "agent_define_test.rs"]
mod tests;
