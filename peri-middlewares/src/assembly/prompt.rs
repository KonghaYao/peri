//! 冻结 prompt 与 skills 的生产槽位投影。
use super::AssemblyContext;
use crate::{skills::SkillsMiddleware, subagent::SkillPreloadMiddleware, AgentsMdMiddleware};
use peri_agent::middleware::chain::MiddlewareChain;

pub(super) fn add_agents_md(ctx: &AssemblyContext, chain: &mut MiddlewareChain) {
    let AssemblyContext {
        claude_md_excludes,
        frozen_claude_md,
        frozen_claude_local_md,
        ..
    } = ctx;
    let mut mw = AgentsMdMiddleware::new().with_excludes(claude_md_excludes.clone());
    if let Some(main) = frozen_claude_md {
        mw = mw.with_frozen_content(main.clone(), frozen_claude_local_md.clone());
    }
    chain.add(Box::new(mw));
}

pub(super) fn add_skills(ctx: &AssemblyContext, chain: &mut MiddlewareChain) {
    let AssemblyContext {
        frozen_skill_summary,
        ..
    } = ctx;
    // W4b（F2/J5）：技能目录只由 MCP registry 投影——`plugin_skill_roots` /
    // `disable_bundled` 等本地扫描参数已从本中间件删除（它们现在只作为
    // workspace 实例的资源根/关闭位输入，见 `peri-acp/src/host/workspace.rs`）。
    let mut skills_mw = SkillsMiddleware::new().with_mcp_registry(ctx.mcp_skill_registry.clone());
    if let Some(summary) = frozen_skill_summary {
        skills_mw = skills_mw.with_frozen_summary(summary.clone());
    }
    chain.add(Box::new(skills_mw));
}

pub(super) fn add_skill_preload(ctx: &AssemblyContext, chain: &mut MiddlewareChain) {
    let AssemblyContext { preload_skills, .. } = ctx;
    chain.add(Box::new(
        SkillPreloadMiddleware::new(preload_skills.clone())
            .with_mcp_registry(ctx.mcp_skill_registry.clone()),
    ));
}
