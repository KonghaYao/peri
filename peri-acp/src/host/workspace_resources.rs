//! 会话资源输入装配（W5 自 `workspace.rs` 拆出；STD-SIZE-001）。
//!
//! 只做「根解析 + scope 映射 + 关闭位投影」，**不读技能/Agent/指令内容**
//! （扫描与正文读取在 provider 侧）。两个 project Agent 根的顺序即优先级
//! （`.claude/agents` 先、`{cwd}/agents` 后，F4）。

/// W4b 技能资源输入装配（F11/F12 适配器 → provider 输入位）。
///
/// 只做「根解析 + scope 映射 + 关闭位投影」：不读技能内容、不扫描目录
/// （扫描与正文读取在 provider 侧，`mcp-packages/workspace/src/resources/`）。
///
/// W4b 技能资源输入装配（F11/F12 适配器 → provider 输入位）+ W5 Agent 根接线。
///
/// 只做「根解析 + scope 映射 + 关闭位投影」：不读技能/Agent 内容、不扫描目录
/// （扫描与正文读取在 provider 侧，`mcp-packages/workspace/src/resources/`）。
///
/// - `resolve_skill_roots`：User（`~/.claude/skills`）→ Project
///   （`{cwd}/.claude/skills`）→ 插件根（带
///   `plugin_name` 标签，来自准备阶段的插件加载结果）→ Builtin 占位；
/// - Builtin 占位根**不映射**为资源根（其 path 是 `PathBuf::new()` 占位，静态
///   资产由 provider 内置提供），启用位只经 `disable_bundled` 传递；
/// - W5 Agent 根：两个 **project** 根，顺序即优先级——`{cwd}/.claude/agents` 先、
///   `{cwd}/agents` 后（F4：迁移前的候选序，恢复被 W5 早期版本误删的第二个根）；
///   插件根来自 `plugins.data.plugins[].agents_dirs`（带插件名，provider plugin
///   scope 要求）；builtin Agent 由 provider 静态表提供；
/// - 缺失目录不在这里过滤：由 provider 按既有「缺失 = 空批」语义处理。
pub(crate) fn workspace_resources_input(
    cwd: &str,
    plugins: &super::assemble::PreparedPlugins,
    instruction_excludes: &[String],
    disable_bundled: bool,
) -> peri_mcp_workspace::WorkspaceResourcesInput {
    use peri_acp_types::skills::SkillSource;
    use peri_mcp_workspace::{ResourceRoot, ResourceScope};

    let mut input = peri_mcp_workspace::WorkspaceResourcesInput::new()
        .with_disable_bundled(disable_bundled)
        .with_instruction_excludes(instruction_excludes.to_vec());
    for root in
        peri_middlewares::resolve_skill_roots(cwd, plugins.skill_roots.clone(), disable_bundled)
    {
        let scope = match root.source {
            SkillSource::User => ResourceScope::User,
            SkillSource::Project => ResourceScope::Project,
            SkillSource::Plugin => ResourceScope::Plugin,
            // Builtin 资产不是磁盘根：启用位已由 `disable_bundled` 表达。
            SkillSource::Builtin => continue,
            // 根解析不产出 Global / Mcp；出现即内部错误（不静默映射 scope）。
            SkillSource::Global | SkillSource::Mcp => {
                tracing::warn!(
                    path = %root.path.display(),
                    "skill 根解析产出不支持的 scope，跳过"
                );
                continue;
            }
        };
        input = match (scope, root.plugin_name.clone()) {
            (ResourceScope::Plugin, Some(plugin_name)) => {
                input.with_skill_root(ResourceRoot::plugin(root.path.clone(), plugin_name))
            }
            _ => input.with_skill_root(ResourceRoot::new(root.path.clone(), scope)),
        };
    }

    // W5 Agent 根（本地三来源中的 project / plugin；builtin 由 provider 静态表
    // 提供）。与迁移前 `scan_agents_detailed` 的目录集合逐字一致：
    // project = `{cwd}/.claude/agents`；plugin = 插件 manifest 的 `agents/` 目录。
    // 插件根的 `plugin_name` 标签来自准备阶段解析出的插件来源名——provider 的
    // plugin scope 要求它（URI authority 的 plugin 段与 `io.peri/plugin`）。
    // 顺序即同名前缀优先级（provider 的 (scope, plugin, id) 先到先得）：
    // `.claude/agents` 先、`{cwd}/agents` 后——与迁移前的候选序一致（F4）。
    input = input.with_agent_root(ResourceRoot::new(
        std::path::Path::new(cwd).join(".claude").join("agents"),
        ResourceScope::Project,
    ));
    input = input.with_agent_root(ResourceRoot::new(
        std::path::Path::new(cwd).join("agents"),
        ResourceScope::Project,
    ));
    for (dir, plugin_name) in plugin_agent_roots(plugins) {
        input = input.with_agent_root(ResourceRoot::plugin(dir, plugin_name));
    }
    input
}

/// 插件 Agent 根与来源名（`plugin_name@marketplace` 稳定标识，与技能根同源口径）。
///
/// 插件加载结果里 `agents_dirs` 与 `plugins` 是两段数据；本函数按插件逐项展开，
/// 每个目录都带上该插件名（provider 侧进 URI authority 的 plugin 段）。
pub(crate) fn plugin_agent_roots(
    plugins: &super::assemble::PreparedPlugins,
) -> Vec<(std::path::PathBuf, String)> {
    let Some(data) = plugins.data.as_ref() else {
        return Vec::new();
    };
    let mut roots = Vec::new();
    for plugin in &data.plugins {
        let name = plugin.name.clone();
        for dir in &plugin.agents_dirs {
            roots.push((dir.clone(), name.clone()));
        }
    }
    roots
}
