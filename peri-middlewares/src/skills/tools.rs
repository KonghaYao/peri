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
        let content = find_and_load_skill(&skills, skill_name)?;
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
                    s.name.to_lowercase().contains(q) || s.description.to_lowercase().contains(q)
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

/// 在已扫描的 skills 列表中按名称（大小写无关）查找并加载 SKILL.md 全文。
///
/// 支持命名空间前缀：`ecc:plan` → 去前缀后匹配 `plan`。
/// 返回 `Err` 仅当找不到匹配 skill，不 panic。
fn find_and_load_skill(
    skills: &[SkillMetadata],
    skill_name: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let input_lower = skill_name.to_lowercase();
    // 去掉可能的命名空间前缀 `ns:name` → `name`
    let bare_name = input_lower
        .rsplit_once(':')
        .map(|(_, n)| n)
        .unwrap_or(&input_lower);

    // 大小写无关精确匹配
    let matched = skills.iter().find(|s| {
        let skill_name_lower = s.name.to_lowercase();
        skill_name_lower == bare_name
    });

    let Some(skill) = matched else {
        return Err(format!(
            "Skill '{skill_name}' not found. Use DiscoverSkillsTool to see available skills."
        )
        .into());
    };

    // Builtin source 从编译期常量加载，其他 source 从磁盘读取
    let content = load_skill_content(skill)?;
    Ok(content)
}

/// 根据 skill 的 source 类型读取完整 SKILL.md 内容
///
/// 错误语义（frozen catalog 与磁盘中途变化的边界，见 13_skills.md）：
/// - 磁盘读取失败说明该 skill 在**当前扫描缓存**中存在但文件不可读——通常是
///   会话期间被删除/移动，属可恢复错误，可重新 `DiscoverSkillsTool` 确认。
fn load_skill_content(
    skill: &SkillMetadata,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    if matches!(skill.source, super::SkillSource::Builtin) {
        crate::skills::builtin::BUILTIN_SKILLS
            .iter()
            .find(|bs| bs.name == skill.name)
            .map(|bs| bs.content.to_string())
            .ok_or_else(|| {
                format!("Builtin skill '{}' not found in BUILTIN_SKILLS", skill.name).into()
            })
    } else {
        std::fs::read_to_string(&skill.path).map_err(|_e| {
            format!(
                "Skill '{}' is in the session catalog but its file cannot be read ({}). It may have been moved or deleted mid-session — run DiscoverSkillsTool to see the current set.",
                skill.name,
                skill.path.display()
            )
            .into()
        })
    }
}

/// 将 SkillMetadata 转为 DiscoverSkillsTool 的 JSON 输出格式
fn skill_to_json(skill: &SkillMetadata) -> serde_json::Value {
    let source_str = match skill.source {
        super::SkillSource::User => "user",
        super::SkillSource::Global => "global",
        super::SkillSource::Project => "project",
        super::SkillSource::Plugin => "plugin",
        super::SkillSource::Builtin => "builtin",
    };
    json!({
        "name": skill.name,
        "description": skill.description,
        "source": source_str,
    })
}

#[cfg(test)]
#[path = "tools_test.rs"]
mod tests;
