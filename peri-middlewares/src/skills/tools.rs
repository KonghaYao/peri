//! SkillTool + DiscoverSkillsTool — 让 LLM 在推理过程中动态发现和加载 skill
//!
//! 参考 Claude Code 的同名工具实现。SkillTool 按名称加载 skill 全文，
//! DiscoverSkillsTool 搜索可用 skills 列表。两者均通过 SkillsMiddleware
//! 注入的 plugin_roots / disable_bundled 访问完整的 skill 搜索路径。
//!
//! 版本 2：工具不再自行扫描磁盘，改用 SkillsMiddleware 在 before_agent 时
//! 预先扫描并缓存的 skills 列表（`cached_skills`）。

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use peri_acp_types::mcp_skills::mcp_skill_name;
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
    /// SkillsMiddleware 在 before_agent 时预扫描的 skills 列表缓存。
    cached_skills: Arc<RwLock<Option<Vec<SkillMetadata>>>>,
}

impl SkillTool {
    pub fn new(cached_skills: Arc<RwLock<Option<Vec<SkillMetadata>>>>) -> Self {
        Self { cached_skills }
    }
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

        // 只复制命中的 metadata；锁在进入 blocking 线程之前释放。
        let skill = {
            let cached = self.cached_skills.read().unwrap();
            let skills = cached.as_ref().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "Skills cache is empty — before_agent may not have run",
                )
            })?;
            find_skill(skills, skill_name)?.clone()
        };
        let content = tokio::task::spawn_blocking(move || super::content::load(&skill)).await??;
        Ok(content)
    }
}

// ─── DiscoverSkillsTool ───────────────────────────────────────────────────────

/// 搜索可用 skills 列表。
///
/// LLM 通过此工具发现当前环境中可用的所有 skill，按名称或描述筛选。
/// 结果以 JSON 数组返回，包含 name、description、source 字段。
pub struct DiscoverSkillsTool {
    /// SkillsMiddleware 在 before_agent 时预扫描的 skills 列表缓存。
    cached_skills: Arc<RwLock<Option<Vec<SkillMetadata>>>>,
}

impl DiscoverSkillsTool {
    pub fn new(cached_skills: Arc<RwLock<Option<Vec<SkillMetadata>>>>) -> Self {
        Self { cached_skills }
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
        // 缓存由 SkillsMiddleware::before_agent 保证填充，不再做懒扫描回退
        let cached = self.cached_skills.read().unwrap();
        let skills = match cached.as_ref() {
            Some(s) => s.clone(),
            None => {
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "Skills cache is empty — before_agent may not have run",
                )));
            }
        };

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
/// 支持命名空间前缀：`ecc:plan` → 去前缀后匹配 `plan`。
/// 返回 `Err` 仅当找不到匹配 skill，不 panic。
fn find_skill<'a>(
    skills: &'a [SkillMetadata],
    skill_name: &str,
) -> Result<&'a SkillMetadata, Box<dyn std::error::Error + Send + Sync>> {
    let input_lower = skill_name.to_lowercase();

    // 名称已在扫描时把 `:` 规范为 `-`；完整名称优先匹配，避免把包含
    // 命名空间前缀的输入过早降级为最后一段。
    if let Some(skill) = skills.iter().find(|s| {
        s.name.eq_ignore_ascii_case(&input_lower)
            || s.aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(&input_lower))
    }) {
        return Ok(skill);
    }

    // MCP 别名分支（DD-3）：`<server>:<skill>` → `mcp__<server>__<skill>`。
    // 在既有 rsplit_once 剥前缀**之前**同构查找缓存（大小写无关）；命中即
    // 加载返回。未命中继续走下方磁盘路径——本地 plugin 命名空间语义不变。
    // 兜底（决策 1 + A3）：plugin 多冒号 server key（`plugin:{plugin}:{server}`）
    // 下别名按原名拼名必 miss——按「server 名末段小写 / 完整名」匹配
    // SkillOrigin::Mcp 的 server（与命令面 fullname 首段派生、SkillPreload
    // 的 registry find_by_command 同构）。
    if let Some((prefix, suffix)) = skill_name.rsplit_once(':') {
        if !suffix.is_empty() {
            let prefix = prefix.to_lowercase();
            let mcp_full = mcp_skill_name(&prefix, suffix).to_lowercase();
            if let Some(skill) = skills.iter().find(|s| s.name.to_lowercase() == mcp_full) {
                return Ok(skill);
            }
            let want_skill = suffix.to_lowercase();
            if let Some(skill) = skills.iter().find(|s| match &s.origin {
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
            }) {
                return Ok(skill);
            }
        }
    }

    // 去掉可能的命名空间前缀 `ns:name` → `name`
    let bare_name = input_lower
        .rsplit_once(':')
        .map(|(_, n)| n)
        .unwrap_or(&input_lower);

    // 大小写无关精确匹配
    let matched = skills.iter().find(|s| {
        s.name.eq_ignore_ascii_case(bare_name)
            || s.aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(bare_name))
    });

    let Some(skill) = matched else {
        return Err(format!(
            "Skill '{skill_name}' not found. Use DiscoverSkillsTool to see available skills."
        )
        .into());
    };

    Ok(skill)
}

/// 将 SkillMetadata 转为 DiscoverSkillsTool 的 JSON 输出格式
fn skill_to_json(skill: &SkillMetadata) -> serde_json::Value {
    let source_str = match skill.source {
        super::SkillSource::User => "user",
        super::SkillSource::Global => "global",
        super::SkillSource::Project => "project",
        super::SkillSource::Plugin => "plugin",
        super::SkillSource::Builtin => "builtin",
        super::SkillSource::Mcp => "mcp",
    };
    json!({
        "name": skill.name,
        "aliases": skill.aliases,
        "description": skill.description,
        "source": source_str,
    })
}

#[cfg(test)]
#[path = "tools_test.rs"]
mod tests;
