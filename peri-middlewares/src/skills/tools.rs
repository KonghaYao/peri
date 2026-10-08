//! SkillTool + DiscoverSkillsTool — 让 LLM 在推理过程中动态发现和加载 skill
//!
//! 参考 Claude Code 的同名工具实现。SkillTool 按名称加载 skill 全文，
//! DiscoverSkillsTool 搜索可用 skills 列表。两者都**在调用时**读取会话级 MCP
//! skill registry 的当前投影（M8：目录不依赖 `before_agent` 时刻的旧副本），
//! 正文经统一 activation（`resources/read` + digest 校验）读取——**两个工具都
//! 不触碰文件系统**（J5：技能来源只有 MCP 侧；本地三根 / 插件根 / builtin 资产
//! 由 builtin `workspace` 实例的资源面承担）。
//!
//! 目录状态（L2）显式区分：
//! - **未装配**（registry 缺失）：稳定可操作的「目录不可用」错误，
//!   不泄露内部实现串，也不返回假空成功；
//! - **初始化中**（仍有 server 的发现任务未收口）：显式告知目录尚未就绪，
//!   不把空投影当空目录；
//! - **Ready(empty)**：合法空目录——`DiscoverSkillsTool` 返回 `[]`；
//! - **Ready(nonempty)**：正常投影。

use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::mcp_skills::{mcp_skill_name, McpSkillRegistry};
use peri_acp_types::skills::SkillOrigin;
use peri_agent::tools::{BaseTool, ToolContext};
use serde_json::{json, Value};

use super::SkillMetadata;

const SKILL_TOOL_NAME: &str = "SkillTool";
const DISCOVER_SKILLS_TOOL_NAME: &str = "DiscoverSkillsTool";

// ─── SkillTool ────────────────────────────────────────────────────────────────

/// 加载指定 skill 的完整 SKILL.md 内容。
///
/// LLM 在推理过程中通过此工具按需加载 skill，获取其完整 frontmatter + body，
/// 无需用户手动输入 `/skill-name`。
pub struct SkillTool {
    /// 会话级 MCP skill 注册表（M8：调用时读取当前投影；None = 未装配技能面 →
    /// 目录不可用的稳定错误，不回落磁盘、不返回假空成功）。
    mcp_registry: Option<Arc<McpSkillRegistry>>,
}

impl SkillTool {
    pub fn new(mcp_registry: Option<Arc<McpSkillRegistry>>) -> Self {
        Self { mcp_registry }
    }
}

/// 目录当前投影；「初始化中」显式失败，只有发现已收口才返回投影
/// （空投影 = 合法空目录）。
fn catalog_projection(
    registry: &McpSkillRegistry,
) -> Result<Vec<SkillMetadata>, Box<dyn std::error::Error + Send + Sync>> {
    let skills = registry.all_skills();
    if skills.is_empty() && registry.discovery_in_progress() {
        return Err(super::skill_catalog_initializing_message().into());
    }
    Ok(skills)
}

#[async_trait]
impl BaseTool for SkillTool {
    fn name(&self) -> &str {
        SKILL_TOOL_NAME
    }

    fn is_direct(&self) -> bool {
        true
    }

    /// 提示词层声明分组（design v2 §2.5.1）：skills 工具归入 `skills`。
    fn namespace(&self) -> Option<&str> {
        Some("skills")
    }

    /// 提示词层声明模板（design v2 §2.5.3）：按名加载 skill 全文。
    ///
    /// title 不覆盖——走 `BaseTool::tool_description` 默认路径由 name 推导。
    /// 05_using_tools.md 手写条目在渐进迁移完成前保留（守护测试防逐字重复）。
    fn prompt_declaration(&self) -> Option<String> {
        Some(
            "Load the full SKILL.md of a skill → `{{name}}` ({{title}}), by name — e.g. when a skill appears in your instructions and you need its full body. Matching is case-insensitive and supports namespace prefixes (e.g. 'ecc:plan')."
                .to_string(),
        )
    }

    fn description(&self) -> &str {
        "Load the full content of a skill by name. Use this tool when you need to know the detailed instructions of a skill mentioned in the system prompt. The skill name is case-insensitive and supports namespace prefix (e.g. 'ecc:plan' matches skill 'plan'). Returns the full SKILL.md content including frontmatter headers."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "skill_name": {
                    "type": "string",
                    "description": "The name of the skill to load (e.g. 'brainstorming', 'code-review'). Case-insensitive. Supports namespace prefix (e.g. 'ecc:plan')."
                }
            },
            "required": ["skill_name"]
        })
    }

    async fn invoke(
        &self,
        input: Value,
        _ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let skill_name = input["skill_name"]
            .as_str()
            .ok_or("SkillTool: missing required parameter 'skill_name'")?;

        // M8：调用时读取会话 registry 的当前投影（不依赖 before_agent 旧副本）。
        // 未装配 ⇒ 稳定可操作的「目录不可用」错误（不泄露内部实现串）。
        let Some(registry) = self.mcp_registry.as_ref() else {
            return Err(super::skill_catalog_unavailable_message().into());
        };
        let skills = catalog_projection(registry)?;
        let skill = find_skill(&skills, skill_name)?.clone();
        // W4b（F1/F8）：正文**只有**一条读取路径——统一 activation 经
        // `resources/read`（+ digest/frontmatter 校验，stale 经 `skills/get`
        // 刷新一次）。本地磁盘分支与 builtin 嵌入分支已删除（J5）：所有条目的
        // origin 都是 MCP 实例（builtin `workspace` 承担本地三根 / 插件 / 内置
        // 资产），未装配 registry 时上面的投影读取已显式报目录不可用，
        // 不回落任何本地来源。
        match crate::mcp::skill_activation::activate(registry, &skill, None).await {
            // 内容带来源标注（与 preload / 命令面同源）。
            Ok(content) => Ok(super::annotate_mcp_content(&skill, &content)),
            Err(error) => {
                Err(super::skill_activation_failed_message(&skill.name, error.reason()).into())
            }
        }
    }
}

// ─── DiscoverSkillsTool ───────────────────────────────────────────────────────

/// 搜索可用 skills 列表。
///
/// LLM 通过此工具发现当前环境中可用的所有 skill，按名称或描述筛选。
/// 结果以 JSON 数组返回，包含 name、description、source 字段；合法空目录返回
/// `[]`（不是错误），目录未就绪 / 未装配则显式失败（不返回假空成功）。
pub struct DiscoverSkillsTool {
    /// 会话级 MCP skill 注册表（M8：调用时读取当前投影）。
    mcp_registry: Option<Arc<McpSkillRegistry>>,
}

impl DiscoverSkillsTool {
    pub fn new(mcp_registry: Option<Arc<McpSkillRegistry>>) -> Self {
        Self { mcp_registry }
    }
}

#[async_trait]
impl BaseTool for DiscoverSkillsTool {
    fn name(&self) -> &str {
        DISCOVER_SKILLS_TOOL_NAME
    }

    fn is_direct(&self) -> bool {
        true
    }

    /// 提示词层声明分组（design v2 §2.5.1）：skills 工具归入 `skills`。
    fn namespace(&self) -> Option<&str> {
        Some("skills")
    }

    /// 提示词层声明模板（design v2 §2.5.3）：按名称或描述搜索可用 skills。
    ///
    /// title 不覆盖——走 `BaseTool::tool_description` 默认路径由 name 推导。
    /// 05_using_tools.md 手写条目在渐进迁移完成前保留（守护测试防逐字重复）。
    fn prompt_declaration(&self) -> Option<String> {
        Some(
            "Find available skills → `{{name}}` ({{title}}) by name/description; use it to see which skills exist in this workspace. Without a query it returns all skills."
                .to_string(),
        )
    }

    fn description(&self) -> &str {
        "Search for available skills by name or description. Use this tool to discover what skills are available in the current workspace. Returns a JSON array of matching skills with their name, description, and source. If no query is provided, returns all available skills."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Optional search query to filter skills by name or description (case-insensitive substring match). If empty or absent, returns all available skills."
                }
            },
            "required": []
        })
    }

    async fn invoke(
        &self,
        input: Value,
        _ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        // M8/L2：调用时读取会话 registry 的当前投影；未装配 ⇒ 目录不可用
        // （稳定可操作错误，不是空目录）；发现未收口 ⇒ 目录未就绪，
        // 不把空投影当空目录返回。
        let Some(registry) = self.mcp_registry.as_ref() else {
            return Err(super::skill_catalog_unavailable_message().into());
        };
        let skills = catalog_projection(registry)?;

        let query = input
            .get("query")
            .and_then(|v| v.as_str())
            .filter(|q| !q.trim().is_empty())
            .map(|q| q.to_lowercase());

        let matched: Vec<serde_json::Value> = skills
            .iter()
            .filter(|s| {
                if let Some(ref q) = query {
                    s.name.to_lowercase().contains(q)
                        || s.description.to_lowercase().contains(q)
                        || s.aliases
                            .iter()
                            .any(|alias| alias.to_lowercase().contains(q))
                } else {
                    true
                }
            })
            .map(skill_to_json)
            .collect();

        Ok(serde_json::to_string(&matched).unwrap_or_else(|_| "[]".into()))
    }
}

// ─── 内部辅助函数 ────────────────────────────────────────────────────────────

/// 在已扫描的 skills 列表中按名称（大小写无关）选择 metadata。
///
/// 解析顺序（每步命中即返回；每步内多命中 → **显式歧义错误**，不静默取首个）：
/// 1. 完整名称 / 别名；
/// 2. MCP 别名 `<server>:<skill>`（先按原拼名 `mcp__<server>__<skill>`，再按
///    server 名末段/完整名匹配 origin——plugin 多冒号 server key 的兜底）；
/// 3. 去掉命名空间前缀后的裸名。
///
/// 同名跨 origin（workspace 与外部 server 同名 skill）必须由调用方以完整名
/// 消歧：歧义错误里给出候选清单（`{server}:{skill}` 排序）。
/// 返回 `Err` 仅当找不到匹配或命中歧义，不 panic。
fn find_skill<'a>(
    skills: &'a [SkillMetadata],
    skill_name: &str,
) -> Result<&'a SkillMetadata, Box<dyn std::error::Error + Send + Sync>> {
    let input_lower = skill_name.to_lowercase();

    /// 候选裁决：0 → 未命中（继续下一步）；1 → 命中；多 → 歧义错误。
    fn resolve<'a>(
        skill_name: &str,
        mut hits: Vec<&'a SkillMetadata>,
    ) -> Result<Option<&'a SkillMetadata>, Box<dyn std::error::Error + Send + Sync>> {
        match hits.len() {
            0 => Ok(None),
            1 => Ok(Some(hits.remove(0))),
            _ => {
                let owned: Vec<SkillMetadata> = hits.into_iter().cloned().collect();
                Err(super::skill_ambiguous_message(skill_name, &owned).into())
            }
        }
    }

    // 名称已在扫描时把 `:` 规范为 `-`；完整名称优先匹配，避免把包含
    // 命名空间前缀的输入过早降级为最后一段。
    let exact: Vec<&SkillMetadata> = skills
        .iter()
        .filter(|s| {
            s.name.eq_ignore_ascii_case(&input_lower)
                || s.aliases
                    .iter()
                    .any(|alias| alias.eq_ignore_ascii_case(&input_lower))
        })
        .collect();
    if let Some(skill) = resolve(skill_name, exact)? {
        return Ok(skill);
    }

    // MCP 别名分支（DD-3）：`<server>:<skill>` → `mcp__<server>__<skill>`。
    // 在既有 rsplit_once 剥前缀**之前**同构查找缓存（大小写无关）；命中即
    // 加载返回。未命中继续走下方磁盘路径——本地 plugin 命名空间语义不变。
    // 兜底（决策 1 + A3）：plugin 多冒号 server key（`plugin:{plugin}:{server}`）
    // 下别名按原名拼名必 miss——按「server 名末段小写 / 完整名」匹配
    // SkillOrigin::Mcp 的 server（与命令面 fullname 首段派生、SkillPreload
    // 的 registry lookup_by_command 同构）。
    if let Some((prefix, suffix)) = skill_name.rsplit_once(':') {
        if !suffix.is_empty() {
            let prefix = prefix.to_lowercase();
            let mcp_full = mcp_skill_name(&prefix, suffix).to_lowercase();
            let by_full_name: Vec<&SkillMetadata> = skills
                .iter()
                .filter(|s| s.name.to_lowercase() == mcp_full)
                .collect();
            if let Some(skill) = resolve(skill_name, by_full_name)? {
                return Ok(skill);
            }
            let want_skill = suffix.to_lowercase();
            let by_server_trail: Vec<&SkillMetadata> = skills
                .iter()
                .filter(|s| match &s.origin {
                    Some(SkillOrigin::Mcp { server, .. }) => {
                        let trail = server
                            .rsplit(':')
                            .next()
                            .unwrap_or(server.as_str())
                            .to_lowercase();
                        (trail == prefix || server.to_lowercase() == prefix)
                            && s.name.to_lowercase()
                                == mcp_skill_name(server, &want_skill).to_lowercase()
                    }
                    _ => false,
                })
                .collect();
            if let Some(skill) = resolve(skill_name, by_server_trail)? {
                return Ok(skill);
            }
        }
    }

    // 去掉可能的命名空间前缀 `ns:name` → `name`
    let bare_name = input_lower
        .rsplit_once(':')
        .map(|(_, n)| n)
        .unwrap_or(&input_lower);

    // W4b（风险 d：裸名规范化层打通）：registry 条目名是宿主注册名
    // `mcp__{server}__{skill}`（`mcp_skill_name`），而用户/模型输入裸名
    // （`/skill-name`、`SkillTool("skill-name")`）只带 `<skill>` 段——因此裸名
    // 匹配必须同时看**末段**，与 preload 的 `McpSkillRegistry::lookup`、
    // 命令面 `core:{skill}` 投影同一口径（跨 origin 同名仍走歧义拒绝）。
    let bare: Vec<&SkillMetadata> = skills
        .iter()
        .filter(|s| {
            s.name.eq_ignore_ascii_case(bare_name)
                || peri_acp_types::mcp_skills::bare_skill_segment(&s.name)
                    .is_some_and(|segment| segment.eq_ignore_ascii_case(bare_name))
                || s.aliases
                    .iter()
                    .any(|alias| alias.eq_ignore_ascii_case(bare_name))
        })
        .collect();
    match resolve(skill_name, bare)? {
        Some(skill) => Ok(skill),
        None => Err(super::skill_not_found_message(skill_name).into()),
    }
}

/// 将 SkillMetadata 转为 DiscoverSkillsTool 的 JSON 输出格式
fn skill_to_json(skill: &SkillMetadata) -> serde_json::Value {
    json!({
        "name": skill.name,
        "aliases": skill.aliases,
        "description": skill.description,
        "source": super::SkillsMiddleware::source_label(skill),
    })
}

#[cfg(test)]
#[path = "tools_test.rs"]
mod tests;
