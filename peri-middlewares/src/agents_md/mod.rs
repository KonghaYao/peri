use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
use peri_agent::{
    error::AgentResult,
    middleware::{r#trait::Middleware, state::MiddlewareState},
};

/// AgentsMdMiddleware - 注入项目指引文件（AGENTS.md / CLAUDE.md）
///
/// 在 `before_agent` 时，按优先级搜索指引文件并将内容前插为系统消息。
///
/// 搜索优先级：
/// 1. `{cwd}/AGENTS.md`
/// 2. `{cwd}/CLAUDE.md`
/// 3. `{cwd}/.claude/AGENTS.md`
/// 4. `{home}/.claude/AGENTS.md`（用户全局）
pub struct AgentsMdMiddleware {
    extra_search_paths: Vec<PathBuf>,
    excludes: Vec<String>,
    /// Frozen CLAUDE.md main content (resolved @import). When set, skip disk read.
    frozen_main: Option<String>,
    /// Frozen CLAUDE.local.md content.
    frozen_local: Option<String>,
    /// Cached prompt contribution (populated in before_agent, returned by prompt_contribution).
    cached_contribution: Arc<RwLock<Option<String>>>,
}

impl AgentsMdMiddleware {
    pub fn new() -> Self {
        Self {
            extra_search_paths: Vec::new(),
            excludes: Vec::new(),
            frozen_main: None,
            frozen_local: None,
            cached_contribution: Arc::new(RwLock::new(None)),
        }
    }

    /// 添加额外搜索路径（应用层可注入）
    pub fn with_extra_paths(mut self, paths: Vec<PathBuf>) -> Self {
        self.extra_search_paths = paths;
        self
    }

    /// 设置 CLAUDE.md 排除 glob 模式
    pub fn with_excludes(mut self, patterns: Vec<String>) -> Self {
        self.excludes = patterns;
        self
    }

    /// Inject frozen CLAUDE.md content (main with resolved @import) and
    /// optional CLAUDE.local.md content.
    ///
    /// When set, `before_agent` skips disk I/O entirely and uses the frozen
    /// content directly.
    pub fn with_frozen_content(mut self, main: String, local: Option<String>) -> Self {
        // v2：构造时即填充 cached_contribution，使 prompt_contribution 立即可用，
        // 无需 before_agent 触发（SubAgent 链构造时 prompt_contribution 在 before_agent 前被收集）。
        // 保留 frozen_main / frozen_local 字段，让 before_agent 仍可走 build_contribution 兼容路径。
        let mut combined = main.clone();
        if let Some(ref local) = local {
            if !local.trim().is_empty() {
                combined.push_str("\n\n");
                combined.push_str(local);
            }
        }
        if !combined.trim().is_empty() {
            *self.cached_contribution.write().unwrap() = Some(combined);
        }
        self.frozen_main = Some(main);
        self.frozen_local = local;
        self
    }

    /// Read and freeze CLAUDE.md content once (with @import resolution).
    ///
    /// Returns `(main_content, local_content)`, either may be `None`.
    /// Called at session creation so the content never drifts mid-session.
    pub fn read_frozen_content(cwd: &str) -> (Option<String>, Option<String>) {
        let candidates = vec![
            Path::new(cwd).join("AGENTS.md"),
            Path::new(cwd).join("CLAUDE.md"),
            Path::new(cwd).join(".claude").join("AGENTS.md"),
        ];
        let main_content = candidates
            .into_iter()
            .find(|p| p.is_file())
            .and_then(|path| {
                let content = std::fs::read_to_string(&path).ok()?;
                if content.trim().is_empty() {
                    return None;
                }
                let is_claude_md = path
                    .file_name()
                    .map(|n| n.to_string_lossy().starts_with("CLAUDE"))
                    .unwrap_or(false);
                if is_claude_md {
                    let dir = path.parent().unwrap_or(Path::new("."));
                    let mut visited = HashSet::new();
                    if let Ok(canonical) = path.canonicalize() {
                        visited.insert(canonical);
                    }
                    Some(resolve_imports(&content, dir, 3, &mut visited))
                } else {
                    Some(content)
                }
            });
        let local_content = {
            let local_path = Path::new(cwd).join("CLAUDE.local.md");
            if local_path.is_file() {
                let c = std::fs::read_to_string(&local_path).unwrap_or_default();
                if c.trim().is_empty() {
                    None
                } else {
                    Some(c)
                }
            } else {
                None
            }
        };
        (main_content, local_content)
    }

    /// 根据 cwd 构建候选路径列表（含默认路径 + 额外路径）
    fn candidate_paths(&self, cwd: &str) -> Vec<PathBuf> {
        let cwd = Path::new(cwd);
        let mut candidates = vec![
            cwd.join("AGENTS.md"),
            cwd.join("CLAUDE.md"),
            cwd.join(".claude").join("AGENTS.md"),
        ];

        if let Some(home) = dirs_next::home_dir() {
            candidates.push(home.join(".claude").join("AGENTS.md"));
        }

        candidates.extend(self.extra_search_paths.iter().cloned());

        candidates
    }

    /// 按优先级找到第一个存在的文件（排除匹配 excludes 模式的路径）
    fn find_file(&self, cwd: &str) -> Option<PathBuf> {
        self.candidate_paths(cwd).into_iter().find(|p| {
            if !p.is_file() {
                return false;
            }
            if self.excludes.is_empty() {
                return true;
            }
            let path_str = p.to_string_lossy();
            !self.excludes.iter().any(|pat| {
                glob::Pattern::new(pat)
                    .map(|g| g.matches(&path_str))
                    .unwrap_or(false)
            })
        })
    }

    /// Build the CLAUDE.md / AGENTS.md contribution string.
    /// When frozen content is set, uses it directly (no disk I/O).
    async fn build_contribution(&self, cwd: &str) -> AgentResult<Option<String>> {
        // Use frozen content when available — skip all disk I/O.
        if let Some(ref main) = self.frozen_main {
            let mut content = main.clone();
            if let Some(ref local) = self.frozen_local {
                if !local.trim().is_empty() {
                    content = format!("{content}\n\n{local}");
                }
            }
            if !content.trim().is_empty() {
                return Ok(Some(content));
            }
            return Ok(None);
        }

        let Some(path) = self.find_file(cwd) else {
            // 即使没有主文件，也尝试读取 CLAUDE.local.md
            let local_path = Path::new(cwd).join("CLAUDE.local.md");
            if local_path.is_file() {
                let lp = local_path.clone();
                let local_content =
                    tokio::task::spawn_blocking(move || std::fs::read_to_string(&lp))
                        .await
                        .map_err(|e| peri_agent::error::AgentError::MiddlewareError {
                            middleware: "AgentsMdMiddleware".to_string(),
                            reason: format!("spawn_blocking 失败: {e}"),
                        })?
                        .map_err(|e| peri_agent::error::AgentError::MiddlewareError {
                            middleware: "AgentsMdMiddleware".to_string(),
                            reason: format!("读取 CLAUDE.local.md 失败: {e}"),
                        })?;
                if !local_content.trim().is_empty() {
                    return Ok(Some(local_content));
                }
            }
            return Ok(None);
        };

        let path_display = path.display().to_string();
        let is_claude_md = path
            .file_name()
            .map(|n| n.to_string_lossy().starts_with("CLAUDE"))
            .unwrap_or(false);
        let import_dir = path.parent().map(|p| p.to_path_buf());
        let main_file_canonical = path.canonicalize().ok();
        let content = tokio::task::spawn_blocking(move || std::fs::read_to_string(&path))
            .await
            .map_err(|e| peri_agent::error::AgentError::MiddlewareError {
                middleware: "AgentsMdMiddleware".to_string(),
                reason: format!("spawn_blocking 失败: {e}"),
            })?
            .map_err(|e| peri_agent::error::AgentError::MiddlewareError {
                middleware: "AgentsMdMiddleware".to_string(),
                reason: format!("读取 {} 失败: {e}", path_display),
            })?;

        let content = if content.trim().is_empty() {
            return Ok(None);
        } else {
            content
        };

        // 追加 CLAUDE.local.md（个人项目级，不入库）
        let local_path = Path::new(cwd).join("CLAUDE.local.md");
        let content = if local_path.is_file() {
            let lp = local_path.clone();
            let local_content = tokio::task::spawn_blocking(move || std::fs::read_to_string(&lp))
                .await
                .map_err(|e| peri_agent::error::AgentError::MiddlewareError {
                    middleware: "AgentsMdMiddleware".to_string(),
                    reason: format!("spawn_blocking 失败: {e}"),
                })?
                .map_err(|e| peri_agent::error::AgentError::MiddlewareError {
                    middleware: "AgentsMdMiddleware".to_string(),
                    reason: format!("读取 CLAUDE.local.md 失败: {e}"),
                })?;
            if local_content.trim().is_empty() {
                content
            } else {
                format!("{content}\n\n{local_content}")
            }
        } else {
            content
        };

        // 仅对 CLAUDE.md 系列文件解析 @import（AGENTS.md 不处理）
        let content = if is_claude_md {
            let dir = import_dir.as_deref().unwrap_or(Path::new(cwd));
            let mut visited = HashSet::new();
            if let Some(canonical) = main_file_canonical {
                visited.insert(canonical);
            }
            resolve_imports(&content, dir, 3, &mut visited)
        } else {
            content
        };

        Ok(Some(content))
    }
}

/// 递归解析 `<!-- @import path -->` 引用，替换为引用文件内容。
/// `base_dir` 为包含 @import 的文件所在目录。
/// `depth` 递归深度上限 3，`visited` 防循环。
pub(crate) fn resolve_imports(
    content: &str,
    base_dir: &Path,
    depth: u32,
    visited: &mut HashSet<PathBuf>,
) -> String {
    if depth == 0 {
        return content.to_string();
    }
    let mut result = String::with_capacity(content.len());
    let mut pos = 0;
    while pos < content.len() {
        if let Some(offset) = content[pos..].find("<!-- @import ") {
            let abs_pos = pos + offset;
            result.push_str(&content[pos..abs_pos]);
            // 提取 path：从 "<!-- @import " 之后到 " -->"
            let after = &content[abs_pos + 13..]; // 13 = "<!-- @import ".len()
            if let Some(end) = after.find(" -->") {
                let import_path = after[..end].trim();
                let resolved = base_dir
                    .join(import_path)
                    .canonicalize()
                    .unwrap_or_else(|_| base_dir.join(import_path));
                if visited.contains(&resolved) || !resolved.is_file() {
                    // 循环引用或文件不存在，保留原始占位符
                    result.push_str(&content[abs_pos..abs_pos + 13 + end + 4]);
                } else {
                    visited.insert(resolved.clone());
                    let imported_content = std::fs::read_to_string(&resolved).unwrap_or_default();
                    let import_dir = resolved.parent().unwrap_or(base_dir);
                    let resolved_content =
                        resolve_imports(&imported_content, import_dir, depth - 1, visited);
                    result.push_str(&resolved_content);
                }
                pos = abs_pos + 13 + end + 4; // 4 = " -->".len()
            } else {
                // 没找到 " -->"，不是有效的 @import，原样保留
                result.push_str("<!-- @import ");
                pos = abs_pos + 13;
            }
        } else {
            result.push_str(&content[pos..]);
            break;
        }
    }
    result
}

impl Default for AgentsMdMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for AgentsMdMiddleware {
    fn name(&self) -> &str {
        "AgentsMdMiddleware"
    }

    fn prompt_contribution(&self) -> Option<String> {
        self.cached_contribution.read().unwrap().clone()
    }

    async fn before_agent(&self, state: &mut dyn MiddlewareState) -> AgentResult<()> {
        let contribution = self.build_contribution(state.cwd()).await?;
        *self.cached_contribution.write().unwrap() = contribution;
        Ok(())
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
