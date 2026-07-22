pub mod builtin;
pub mod loader;

use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
pub use loader::{
    list_skills, load_skill_metadata, resolve_skill_roots, scan_skill_roots, SkillMetadata,
    SkillRoot, SkillSource, MAX_SCAN_DEPTH, MAX_SKILLS_DIRS_PER_ROOT,
};
use peri_agent::{
    error::AgentResult,
    middleware::{r#trait::Middleware, state::MiddlewareState},
};

/// 全局配置文件路径：~/.peri/settings.json
pub fn global_config_path() -> PathBuf {
    dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".peri")
        .join("settings.json")
}

/// 从全局配置中加载 skills_dir 路径
pub fn load_global_skills_dir() -> Option<PathBuf> {
    let path = global_config_path();
    if !path.exists() {
        return None;
    }

    let content = std::fs::read_to_string(&path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;

    // 支持嵌套 { "config": { "skillsDir": "..." } } 或扁平 { "skillsDir": "..." }
    let skills_dir = json
        .get("config")
        .and_then(|c| c.get("skillsDir"))
        .or_else(|| json.get("skillsDir"))
        .and_then(|v| v.as_str())
        .map(PathBuf::from);

    skills_dir.filter(|p| !p.as_os_str().is_empty())
}

/// 从 `~/.peri/settings.json` 读取 `disableBundledSkills` 配置（默认 false）
///
/// session/new 时一次性读取并冻结，会话内不再重新读取（保持系统提示词稳定性）。
pub fn load_disable_bundled_skills() -> bool {
    load_disable_bundled_skills_from_path(&global_config_path())
}

/// 测试注入入口：从指定 settings 文件读取 disableBundledSkills
pub fn load_disable_bundled_skills_from_path(path: &std::path::Path) -> bool {
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(json): Result<serde_json::Value, _> = serde_json::from_str(&content) else {
        return false;
    };
    // 支持嵌套 { "config": { "disableBundledSkills": ... } } 或扁平
    json.get("config")
        .and_then(|c| c.get("disableBundledSkills"))
        .or_else(|| json.get("disableBundledSkills"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// SkillsMiddleware - 渐进式 Skills 摘要注入
///
/// 在 `before_agent` 时扫描 skills 目录，将所有 skill 的 name + description
/// 生成摘要系统消息前插到消息历史中。
///
/// 搜索路径（按优先级）：
/// 1. `{cwd}/.claude/skills/`（项目级，优先）
/// 2. 全局配置的 `skills_dir`（可配置）
/// 3. `{home}/.claude/code/skills/`（用户级）
pub struct SkillsMiddleware {
    project_skills_dir: Option<PathBuf>,
    global_skills_dir: Option<PathBuf>,
    user_skills_dir: Option<PathBuf>,
    plugin_roots: Vec<SkillRoot>,
    /// Frozen skills summary (None = scan each turn from disk).
    frozen_summary: Option<String>,
    /// 是否禁用 builtin skill（session/new 时一次性读取冻结）
    disable_bundled: bool,
    /// Cached prompt contribution (populated in before_agent, returned by prompt_contribution).
    cached_contribution: Arc<RwLock<Option<String>>>,
}

impl SkillsMiddleware {
    pub fn new() -> Self {
        Self {
            project_skills_dir: None,
            global_skills_dir: None,
            user_skills_dir: None,
            plugin_roots: vec![],
            frozen_summary: None,
            disable_bundled: false,
            cached_contribution: Arc::new(RwLock::new(None)),
        }
    }

    /// 覆盖项目级 skills 目录（默认 `{cwd}/.claude/skills/`）
    pub fn with_project_dir(mut self, dir: PathBuf) -> Self {
        self.project_skills_dir = Some(dir);
        self
    }

    /// 设置全局 skills 目录（从配置读取）
    pub fn with_global_dir(mut self, dir: PathBuf) -> Self {
        self.global_skills_dir = Some(dir);
        self
    }

    /// 覆盖用户级 skills 目录（默认 `{home}/.claude/code/skills/`）
    pub fn with_user_dir(mut self, dir: PathBuf) -> Self {
        self.user_skills_dir = Some(dir);
        self
    }

    /// 从全局配置加载 skills 目录（默认从 `~/.peri/settings.json` 读取）
    pub fn with_global_config(mut self) -> Self {
        if let Some(dir) = load_global_skills_dir() {
            self.global_skills_dir = Some(dir);
        }
        self
    }

    /// 追加插件 skills 搜索根（每个 root 携带 source 与 plugin_name）
    /// 插件 skills 优先级低于项目级，同名先到先得
    pub fn with_plugin_roots(mut self, roots: Vec<SkillRoot>) -> Self {
        self.plugin_roots = roots;
        self
    }

    /// 注入冻结的 skills 摘要。设置后 `before_agent` 跳过目录扫描，
    /// 直接使用冻结内容。
    ///
    /// v2：构造时即填充 cached_contribution，使 prompt_contribution 立即可用，
    /// 无需 before_agent 触发（builder 在 before_agent 前收集 prompt_contribution）。
    pub fn with_frozen_summary(mut self, summary: String) -> Self {
        self.frozen_summary = Some(summary.clone());
        if !summary.trim().is_empty() {
            *self.cached_contribution.write().unwrap() = Some(summary);
        }
        self
    }

    /// 设置是否禁用 builtin skill（默认 false）
    pub fn with_disable_bundled(mut self, disable: bool) -> Self {
        self.disable_bundled = disable;
        self
    }

    /// 一次性扫描并构建冻结的 skills 摘要。
    ///
    /// 返回 `None` 表示无 skills 可用。
    /// 供 session 创建时调用。
    pub fn build_frozen_summary(
        cwd: &str,
        plugin_roots: Vec<SkillRoot>,
        disable_bundled: bool,
    ) -> Option<String> {
        let roots = Self::resolve_roots_static(cwd, plugin_roots, disable_bundled);
        let skills = scan_skill_roots(&roots);
        if skills.is_empty() {
            return None;
        }
        Some(Self::build_summary(&skills))
    }

    /// 在无 `&self` 时解析 skills 根列表（供静态 frozen 构造使用）。
    ///
    /// **注意**：`disable_bundled` 应在 session/new 时一次性读取并冻结，不要每轮传入不同值。
    pub fn resolve_roots_static(
        cwd: &str,
        plugin_roots: Vec<SkillRoot>,
        disable_bundled: bool,
    ) -> Vec<SkillRoot> {
        loader::resolve_skill_roots(cwd, plugin_roots, disable_bundled)
    }

    /// 根据 cwd 解析实际搜索根列表（含 source 标签）
    fn resolve_roots(&self, cwd: &str) -> Vec<SkillRoot> {
        // 有 override 字段时走测试隔离路径
        // 注意：测试隔离路径不含 Builtin root（override 模式用于测试，不需要内置 skill）
        if self.user_skills_dir.is_some()
            || self.global_skills_dir.is_some()
            || self.project_skills_dir.is_some()
        {
            let mut roots = Vec::new();
            // User override
            let user_dir = self.user_skills_dir.clone().unwrap_or_else(|| {
                dirs_next::home_dir()
                    .map(|h| h.join(".claude").join("skills"))
                    .unwrap_or_default()
            });
            roots.push(SkillRoot {
                path: user_dir,
                source: SkillSource::User,
                plugin_name: None,
            });
            // Global override
            if let Some(global) = &self.global_skills_dir {
                roots.push(SkillRoot {
                    path: global.clone(),
                    source: SkillSource::Global,
                    plugin_name: None,
                });
            }
            // Project override
            let project_dir = self
                .project_skills_dir
                .clone()
                .unwrap_or_else(|| PathBuf::from(cwd).join(".claude").join("skills"));
            roots.push(SkillRoot {
                path: project_dir,
                source: SkillSource::Project,
                plugin_name: None,
            });
            // Plugin roots
            for r in &self.plugin_roots {
                if r.path.is_dir() {
                    roots.push(r.clone());
                }
            }
            roots
        } else {
            loader::resolve_skill_roots(cwd, self.plugin_roots.clone(), self.disable_bundled)
        }
    }

    /// 生成 skills 摘要系统消息内容
    pub fn build_summary(skills: &[SkillMetadata]) -> String {
        let mut lines = vec![
            "你可以使用以下 Skills（专项能力），在需要时提及其名称：".to_string(),
            String::new(),
        ];

        for skill in skills {
            lines.push(format!(
                "- **{}**: {} {}",
                skill.name,
                skill.path.display(),
                skill.description
            ));
        }

        lines.push(String::new());
        lines.push("如需加载某 skill 的完整内容，在消息中提及其 name 即可。用户一般会使用 '/skill-name' 的形式。".to_string());

        lines.join("\n")
    }
}

impl Default for SkillsMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for SkillsMiddleware {
    fn name(&self) -> &str {
        "SkillsMiddleware"
    }

    fn prompt_contribution(&self) -> Option<String> {
        self.cached_contribution.read().unwrap().clone()
    }

    async fn before_agent(&self, state: &mut dyn MiddlewareState) -> AgentResult<()> {
        // 使用冻结摘要时跳过所有磁盘 I/O
        if let Some(ref summary) = self.frozen_summary {
            if !summary.trim().is_empty() {
                *self.cached_contribution.write().unwrap() = Some(summary.clone());
            } else {
                *self.cached_contribution.write().unwrap() = None;
            }
            return Ok(());
        }

        let roots = self.resolve_roots(state.cwd());
        let skills = tokio::task::spawn_blocking(move || scan_skill_roots(&roots))
            .await
            .map_err(|e| peri_agent::error::AgentError::MiddlewareError {
                middleware: "SkillsMiddleware".to_string(),
                reason: format!("spawn_blocking 失败: {e}"),
            })?;

        if skills.is_empty() {
            *self.cached_contribution.write().unwrap() = None;
            return Ok(());
        }

        let summary = Self::build_summary(&skills);
        *self.cached_contribution.write().unwrap() = Some(summary);
        Ok(())
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
