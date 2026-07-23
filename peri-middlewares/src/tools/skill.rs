//! Skill Core Tool —— LLM 可主动调用以加载 Skill 的完整 SKILL.md 内容。
//!
//! 仿照 Claude Code SkillTool，默认 inline 模式：工具按 skill 名称查找，
//! 返回 SKILL.md 全文，LLM 按 skill 指导自行执行。

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use peri_agent::{
    middleware::r#trait::Middleware,
    tools::{BaseTool, ToolContext},
};
use serde_json::Value;

use crate::skills::{
    loader,
    loader::{resolve_skill_roots, scan_skill_roots},
    SkillMetadata, SkillRoot, SkillSource,
};

/// 工具描述（中文，仿 Claude Code SkillTool prompt）
const SKILL_TOOL_DESCRIPTION: &str = include_str!("descriptions/skill.md");

/// 输出截断长度（SKILL.md 可能很长，如 ultracode）
const OUTPUT_CHAR_LIMIT: usize = 5000;

/// SkillTool —— LLM 可主动调用以加载 Skill 的完整 SKILL.md 内容
pub struct SkillTool {
    cwd: String,
    plugin_roots: Vec<SkillRoot>,
    disable_bundled: bool,
    /// SkillsMiddleware 在 before_agent 时预扫描的 skills 列表缓存。
    /// 由 SkillToolMiddleware 在 collect_tools 时注入。
    cached_skills: Arc<RwLock<Option<Vec<SkillMetadata>>>>,
}

impl SkillTool {
    pub fn new(
        cwd: impl Into<String>,
        plugin_roots: Vec<SkillRoot>,
        disable_bundled: bool,
        cached_skills: Arc<RwLock<Option<Vec<SkillMetadata>>>>,
    ) -> Self {
        Self {
            cwd: cwd.into(),
            plugin_roots,
            disable_bundled,
            cached_skills,
        }
    }

    /// 按名称查找 skill 并返回 SKILL.md 内容（含路径前缀）。
    /// 优先使用缓存的 skills 列表，避免重复磁盘扫描。
    /// 缓存未命中时惰性扫描并写入（只在对话的第一次工具调用时发生）。
    fn lookup_skill(&self, skill_name: &str) -> Option<(SkillMetadata, String)> {
        // 优先使用缓存
        if let Some(ref cached) = *self.cached_skills.read().unwrap() {
            return loader::find_skill_in_list(cached, skill_name);
        }
        // fallback: 扫描磁盘，写入缓存，再查找
        let roots = resolve_skill_roots(&self.cwd, self.plugin_roots.clone(), self.disable_bundled);
        let skills = scan_skill_roots(&roots);
        let result = loader::find_skill_in_list(&skills, skill_name);
        *self.cached_skills.write().unwrap() = Some(skills);
        result
    }

    /// 模糊匹配建议（复用 skim matcher 基础设施）
    fn fuzzy_suggest(&self, skill_name: &str) -> Vec<String> {
        let candidate_names: Vec<String> =
            if let Some(ref cached) = *self.cached_skills.read().unwrap() {
                cached.iter().map(|s| s.name.clone()).collect()
            } else {
                let roots =
                    resolve_skill_roots(&self.cwd, self.plugin_roots.clone(), self.disable_bundled);
                let skills = scan_skill_roots(&roots);
                let names: Vec<String> = skills.iter().map(|s| s.name.clone()).collect();
                // 写入缓存供后续调用复用
                *self.cached_skills.write().unwrap() = Some(skills);
                names
            };
        peri_agent::error_suggest::matcher::fuzzy_filter(&candidate_names, skill_name)
    }
}

#[async_trait]
impl BaseTool for SkillTool {
    fn name(&self) -> &str {
        "Skill"
    }

    fn description(&self) -> &str {
        SKILL_TOOL_DESCRIPTION
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "skill": {
                    "type": "string",
                    "description": "要加载的 skill 名称。与系统提示词中 skills 摘要所列的名称一致"
                },
                "args": {
                    "type": "string",
                    "description": "传递给 skill 的额外参数（可选，附加到返回内容末尾）"
                }
            },
            "required": ["skill"]
        })
    }

    async fn invoke(
        &self,
        input: Value,
        _ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        // 1. 提取参数
        let skill_name = input["skill"].as_str().ok_or("参数 'skill' 为必填项")?;
        let args = input["args"].as_str();

        // 2. 按名称查找 skill 内容（含 metadata）
        match self.lookup_skill(skill_name) {
            Some((meta, content)) => {
                // 路径前缀：帮助 agent 解析 skill 引用的相对路径文件
                let mut result = match meta.source {
                    SkillSource::Builtin => String::new(),
                    _ => {
                        let parent = meta
                            .path
                            .parent()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default();
                        format!("[Skill 路径: {}]\n\n", parent)
                    }
                };
                result.push_str(&content);
                if let Some(a) = args {
                    result.push_str(&format!("\n\n[调用参数: {}]\n", a));
                }
                Ok(result)
            }
            None => {
                // 3. 未找到 → 模糊匹配建议
                let suggestions = self.fuzzy_suggest(skill_name);
                if suggestions.is_empty() {
                    Err(format!("Unknown skill: '{}'。没有找到可用的 skill。", skill_name).into())
                } else {
                    let hint = suggestions
                        .iter()
                        .take(3)
                        .map(|s| format!("'{}'", s))
                        .collect::<Vec<_>>()
                        .join(", ");
                    Err(format!("Unknown skill: '{}'。Did you mean: {}?", skill_name, hint).into())
                }
            }
        }
    }

    fn output_char_limit(&self) -> Option<usize> {
        Some(OUTPUT_CHAR_LIMIT)
    }

    fn prefers_persist(&self) -> bool {
        true
    }

    fn timeout(&self) -> Option<std::time::Duration> {
        Some(std::time::Duration::from_secs(30))
    }
}

// ── SkillToolMiddleware ───────────────────────────────────────────────────

/// SkillToolMiddleware —— 向 agent 注册 Skill 工具
///
/// 职责单一：通过 `collect_tools` 提供 [`SkillTool`]。
/// 与 [`SkillsMiddleware`]（摘要注入）和 [`SkillPreloadMiddleware`]（用户 /skill-name 触发）
/// 职责分离，互不影响。
///
/// v2：接收共享的 `cached_skills` Arc，避免工具调用时重复磁盘扫描。
pub struct SkillToolMiddleware {
    plugin_roots: Vec<SkillRoot>,
    disable_bundled: bool,
    /// SkillsMiddleware 在 before_agent 时预扫描的 skills 列表缓存。
    /// 由 builder 层从 SkillsMiddleware 获取后传入。
    cached_skills: Arc<RwLock<Option<Vec<SkillMetadata>>>>,
}

impl SkillToolMiddleware {
    pub fn new() -> Self {
        Self {
            plugin_roots: Vec::new(),
            disable_bundled: false,
            cached_skills: Arc::new(RwLock::new(None)),
        }
    }

    pub fn with_plugin_roots(mut self, roots: Vec<SkillRoot>) -> Self {
        self.plugin_roots = roots;
        self
    }

    pub fn with_disable_bundled(mut self, disable: bool) -> Self {
        self.disable_bundled = disable;
        self
    }

    /// 注入共享的 skills 缓存（由 SkillsMiddleware 预填充）。
    /// 若未设置，工具首次调用时会自行扫描磁盘（fallback）。
    pub fn with_cached_skills(mut self, cached: Arc<RwLock<Option<Vec<SkillMetadata>>>>) -> Self {
        self.cached_skills = cached;
        self
    }
}

impl Default for SkillToolMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for SkillToolMiddleware {
    fn name(&self) -> &str {
        "SkillToolMiddleware"
    }

    fn collect_tools(&self, cwd: &str) -> Vec<Box<dyn BaseTool>> {
        vec![Box::new(SkillTool::new(
            cwd.to_string(),
            self.plugin_roots.clone(),
            self.disable_bundled,
            Arc::clone(&self.cached_skills),
        ))]
    }
}

#[cfg(test)]
#[path = "skill_test.rs"]
mod tests;
