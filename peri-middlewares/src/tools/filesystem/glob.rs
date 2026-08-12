use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::SystemTime;

use peri_agent::tools::BaseTool;
use serde_json::Value;
use tokio::time::{timeout, Duration};

use super::resolve_path;
use super::should_skip_dir;
use crate::tools::output_persist::persist_truncated_output;

/// Glob tool — aligned with the TypeScript glob_tool.
pub struct GlobFilesTool {
    pub cwd: String,
}

impl GlobFilesTool {
    pub fn new(cwd: impl Into<String>) -> Self {
        Self { cwd: cwd.into() }
    }
}

/// Maximum number of files returned; protects the LLM context window from exploding.
const MAX_RESULTS: usize = 1_000;
/// Maximum output bytes; beyond this, the full payload is persisted to a temp file and only the first N paths are returned inline with a hint.
const MAX_OUTPUT_BYTES: usize = 20_000;
/// When the byte limit is hit, how many paths to keep inline so the LLM can see them directly.
const HEAD_RESULTS_ON_BYTES_OVERFLOW: usize = 100;

const GLOB_FILES_DESCRIPTION: &str = include_str!("descriptions/glob.md");

/// 扫描超时：与 Grep 工具对齐，防止大目录树上长时间占用 blocking pool。
const SCAN_TIMEOUT: Duration = Duration::from_secs(15);

/// Soft-warn pattern — still executes, but prepends a warning. A hit strongly suggests the caller actually wanted to list a directory.
fn soft_warn_pattern(pattern: &str) -> Option<&'static str> {
    match pattern.trim() {
        "*" => Some(
            "Bare `*` matches files at any depth (wildcards cross `/`); use folder_operations or Bash ls to list a directory instead.",
        ),
        "**" | "**/*" => Some(
            "`**/*` recursively expands the entire subtree (including every worktree/plugin copy); prefer folder_operations or a more specific pattern.",
        ),
        _ => None,
    }
}

/// 返回 pattern 中第一个元字符（`* ? [ {`）的字节下标。
///
/// 注意：glob 0.3.3 无反斜杠转义——`\` 是字面字符，`\*` 中 `*` 仍是元字符
/// （`Pattern::escape` 用 `[x]` 包裹而非 `\`）。`{` 在 glob 0.3.3 中也没有
/// 交替语义（按字面匹配），这里按保守集合识别：把 `{` 当作元字符只会让剪枝
/// 更保守（回退全遍历），不影响正确性。若未来升级 glob 版本，需重审此集合。
fn first_meta_index(pattern: &str) -> Option<usize> {
    pattern
        .char_indices()
        .find_map(|(idx, ch)| matches!(ch, '*' | '?' | '[' | '{').then_some(idx))
}

/// Walk 边界规划：在不改变匹配结果的前提下，把遍历限制到模式约束的子树。
struct WalkPlan {
    /// 元字符之前的完整字面目录链（如 `src/**/*.rs` → ["src"]）：下钻时只有名字
    /// 匹配的目录会被进入。任何命中路径都必须以这些目录为前缀，剪枝不会漏报。
    prefix_dirs: Vec<String>,
    /// 目录下钻的最大深度。仅对无元字符的纯字面 pattern 生效：
    /// `glob::Pattern::matches` 默认 `require_literal_separator = false`，`*`/`?`
    /// 可跨 `/`，含元字符的 pattern 可在任意深度命中，深度剪枝不安全。
    max_depth: Option<usize>,
}

fn plan_walk(pattern: &str) -> WalkPlan {
    let leading_slash = pattern.starts_with('/');
    let meta_idx = first_meta_index(pattern);

    let prefix_dirs: Vec<String> = if leading_slash {
        // 前导 `/` 的 pattern 匹配绝对路径，相对路径永不命中；保守回退全遍历。
        Vec::new()
    } else {
        let literal = meta_idx.map_or(pattern, |i| &pattern[..i]);
        // 前缀必须是完整目录链（以 '/' 结尾）且不含转义；纯字面 pattern 整串作为前缀候选。
        let usable = !literal.contains('\\') && (meta_idx.is_none() || literal.ends_with('/'));
        if usable {
            let mut segments: Vec<String> = literal
                .split('/')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            if meta_idx.is_none() {
                // 无元字符：最后一段是文件名而非目录。
                segments.pop();
            }
            segments
        } else {
            Vec::new()
        }
    };

    let max_depth = if leading_slash || meta_idx.is_some() {
        None
    } else {
        Some(pattern.matches('/').count())
    };

    WalkPlan {
        prefix_dirs,
        max_depth,
    }
}

/// 前缀目录链安全收窄为 walk root；不可收窄时返回 `None`（回退全遍历）。
///
/// 返回 `(root, consumed)`：`WalkDir` 直接从 `root` 起步，跳过 base 整层列举与
/// stat。任何命中路径必以字面前缀开头，root 收窄不漏报；匹配与显示路径仍相对
/// base（`strip_prefix(base)` 语义不变），收窄只影响遍历范围。`consumed` 只有
/// 两种取值：收窄时恒等于 `prefix_dirs.len()`（消费全部前缀段），回退时恒为 0；
/// 不存在中间状态，filter 索引公式见 `collect_files`。
///
/// 回退条件（任一命中即 `None`，保证与全遍历逐字节一致）：
/// - 前缀为空（`**/*.rs`、`Cargo.toml` 等无静态目录链的 pattern）。
/// - base 名本身是被跳目录：现状 filter 的 depth 0 黑名单检查会拒绝整个扫描，
///   收窄后 depth 0 只查 root 名，不补查会漂移（cwd=node_modules + `src/**/*.rs`
///   从 No files found 变为返回文件）。
/// - 前缀链中段（末段除外）是被跳目录：收窄会让被跳目录的子孙变为可遍历，
///   行为反转（如 `node_modules/pkg/*.rs` 现状返回 No files found）。
///   末段不查：收窄后由 filter 的 depth 0 黑名单检查自动拒绝，行为不变。
/// - 任一段为 `.`/`..`：walk 出的 rel 路径不含这两段，拼接只会改变遍历范围，
///   保守回退。
fn narrow_root(base: &Path, plan: &WalkPlan) -> Option<(PathBuf, usize)> {
    let prefix_dirs = &plan.prefix_dirs;
    if prefix_dirs.is_empty() {
        return None;
    }
    if base
        .file_name()
        .is_some_and(|n| should_skip_dir(&n.to_string_lossy()))
    {
        return None;
    }
    let mut root = base.to_path_buf();
    for (i, seg) in prefix_dirs.iter().enumerate() {
        if seg == "." || seg == ".." {
            return None;
        }
        if i + 1 < prefix_dirs.len() && should_skip_dir(seg) {
            return None;
        }
        root.push(seg);
    }
    Some((root, prefix_dirs.len()))
}

/// 收集匹配文件（同步，在 spawn_blocking 中运行）。
///
/// 返回是否因超过 MAX_RESULTS 提前停止。`cancelled` 置位后最多再处理 256 个条目
/// 即退出——spawn_blocking 无法强制中止，协作停止是超时后的兜底保护。
fn collect_files(
    base: &Path,
    walk_root: &Path,
    consumed: usize,
    pattern: &glob::Pattern,
    plan: &WalkPlan,
    cancelled: &AtomicBool,
    results: &mut Vec<(Option<SystemTime>, String)>,
) -> bool {
    let walker = walkdir::WalkDir::new(walk_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            if e.file_type().is_dir() {
                let name = e.file_name().to_string_lossy();
                if !should_skip_dir(&name) {
                    let depth = e.depth();
                    if depth == 0 {
                        // 根目录总是下钻（黑名单已检查，与既有行为一致）
                        true
                    } else if depth <= plan.prefix_dirs.len().saturating_sub(consumed) {
                        // 前缀层：只进入名字匹配的目录。仅回退态（consumed=0）可达——
                        // 收窄态 consumed == len 使 depth <= 0 恒 false（walkdir 目录
                        // 深度 >= 1），直接走下方深度上限分支；索引 `consumed + depth - 1`
                        // 在回退态退化为 `depth - 1`，与收窄前原逻辑一致。
                        name.as_ref() == plan.prefix_dirs[consumed + depth - 1].as_str()
                    } else {
                        // 深度上限（仅纯字面 pattern 生效）；收窄使相对深度减少 consumed 层
                        plan.max_depth
                            .is_none_or(|m| depth <= m.saturating_sub(consumed))
                    }
                } else {
                    false
                }
            } else {
                true
            }
        });

    let mut early_stopped = false;
    for (entries_seen, entry) in walker.enumerate() {
        if entries_seen.is_multiple_of(256) && cancelled.load(Ordering::Relaxed) {
            break;
        }
        match entry {
            Ok(e) => {
                if e.file_type().is_file() {
                    let abs_path = e.path().to_string_lossy().to_string();
                    if let Ok(rel) = e.path().strip_prefix(base) {
                        let rel_str = rel.to_string_lossy().replace('\\', "/");
                        if pattern.matches(&rel_str) {
                            // mtime 在收集时读取一次并缓存，排序阶段不再做任何 syscall。
                            let mtime = e.metadata().ok().and_then(|m| m.modified().ok());
                            results.push((mtime, abs_path));
                            if results.len() > MAX_RESULTS {
                                early_stopped = true;
                                break;
                            }
                        }
                    }
                }
            }
            Err(e) => {
                tracing::debug!(error = %e, "glob walk error (skipped)");
            }
        }
    }
    early_stopped
}

/// 排序 + 组装输出（同步，与 collect_files 一起在 spawn_blocking 中运行）。
fn run_glob(
    base: &Path,
    pattern: &glob::Pattern,
    plan: &WalkPlan,
    cancelled: &AtomicBool,
) -> String {
    let mut results: Vec<(Option<SystemTime>, String)> = Vec::new();
    // root 收窄：静态字面前缀目录链存在时直接从 base+前缀起步（如 `src/**/*.rs`
    // → root=base/src），跳过 base 整层列举与 stat；不可收窄时退回全遍历。
    let (walk_root, consumed) = narrow_root(base, plan).unwrap_or_else(|| (base.to_path_buf(), 0));
    let early_stopped = collect_files(
        base,
        &walk_root,
        consumed,
        pattern,
        plan,
        cancelled,
        &mut results,
    );

    // 稳定降序：mtime 相同的文件保持遍历顺序（与既有行为一致）。
    // 早停语义：超过 MAX_RESULTS 即停止收集，排序窗口 = 遍历中先收集到的
    // MAX_RESULTS+1 条，而非全树全局最新 N 条——两者一致时无差异（glob.md 已说明）。
    results.sort_by_key(|b| std::cmp::Reverse(b.0));

    if results.is_empty() {
        return "No files found.".to_string();
    }

    if results.len() > MAX_RESULTS {
        let full = results
            .iter()
            .map(|(_, p)| p.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let truncated = &results[..MAX_RESULTS];
        let persist_hint = persist_truncated_output(&full);
        let stop_note = if early_stopped {
            " (collection stopped at the result limit)"
        } else {
            ""
        };
        format!(
            "{}\n\n[Output truncated: {} files total{}, showing first {}]{}",
            truncated
                .iter()
                .map(|(_, p)| p.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            results.len(),
            stop_note,
            MAX_RESULTS,
            persist_hint
        )
    } else {
        let joined = results
            .iter()
            .map(|(_, p)| p.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        // Byte guard: many short paths can still overflow by total byte size.
        if joined.len() > MAX_OUTPUT_BYTES {
            let persist_hint = persist_truncated_output(&joined);
            let head_count = HEAD_RESULTS_ON_BYTES_OVERFLOW.min(results.len());
            let head = &results[..head_count];
            format!(
                "{}\n\n[Output truncated: {} files total, {} bytes; showing first {} — exceeds {} byte limit]{}",
                head.iter()
                    .map(|(_, p)| p.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
                results.len(),
                joined.len(),
                head_count,
                MAX_OUTPUT_BYTES,
                persist_hint
            )
        } else {
            joined
        }
    }
}

#[async_trait::async_trait]
impl BaseTool for GlobFilesTool {
    fn name(&self) -> &str {
        "Glob"
    }

    fn is_direct(&self) -> bool {
        true
    }

    /// 同类工具分组（design v2 §2.5.1）：filesystem 工具统一归组。
    fn namespace(&self) -> Option<&str> {
        Some("filesystem")
    }

    /// 提示词层声明模板（design v2 §2.5.3）：对应 05 段落 "File name search"
    /// 条目语义（选择指引 + 纪律约束），不逐字重复（守护测试断言）。
    /// title 不覆盖——走 `tool_description` 默认推导路径。
    fn prompt_declaration(&self) -> Option<String> {
        Some(
            "Find files by name → `{{name}}` (e.g. `**/*.rs`, `*.config.json`). Use `{{name}}` for name search, not `Bash` with `find`; never `{{name}}(\"*\")`/`{{name}}(\"**/*\")` — that dumps the whole tree — list directories via `folder_operations` or `Bash ls`."
                .to_string(),
        )
    }

    fn description(&self) -> &str {
        GLOB_FILES_DESCRIPTION
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "The glob pattern to match file paths relative to the search root (e.g. \"src/**/*.rs\", \"*.config.json\"). Wildcards follow the glob crate defaults: `*` and `?` match across `/`, so `*.rs` matches .rs files at ANY depth, not just the current directory. To scope the walk, use a literal directory prefix (e.g. \"src/*.rs\") or the path parameter"
                },
                "path": {
                    "type": "string",
                    "description": "The directory to search in. Absolute path or relative to cwd. If not specified, the current working directory is used"
                }
            },
            "required": ["pattern"]
        })
    }

    async fn invoke(
        &self,
        input: Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let pattern = input["pattern"]
            .as_str()
            .ok_or("The 'pattern' parameter is required for the Glob tool.")?;

        // Pattern 语法预校验 + 只编译一次：collect_files 复用编译结果，
        // 不再在逐文件热循环里重复 glob::Pattern::new。
        let compiled = glob::Pattern::new(pattern)
            .map_err(|e| format!("Error: Pattern syntax error in {pattern:?}: {e}"))?;

        // Pattern soft-warn — record the hint; we still execute so the LLM can see the output size and self-correct.
        let pattern_warn = soft_warn_pattern(pattern);

        let search_root = if let Some(p) = input["path"].as_str() {
            resolve_path(&self.cwd, p)
        } else {
            Path::new(&self.cwd).to_path_buf()
        };

        if !search_root.exists() {
            return Err(format!("Error: Directory not found: {}", search_root.display()).into());
        }

        let plan = plan_walk(pattern);
        let cancelled = Arc::new(AtomicBool::new(false));

        // 扫描是同步阻塞链（遍历 + 排序 + 落盘），必须移入 blocking pool，不能占用
        // async runtime worker。超时后置位 cancelled，线程在下一个检查点协作退出
        // （spawn_blocking 无法强制 kill，协作停止是真实保护）。
        let scan = tokio::task::spawn_blocking({
            let cancelled = Arc::clone(&cancelled);
            move || run_glob(&search_root, &compiled, &plan, &cancelled)
        });

        let body = match timeout(SCAN_TIMEOUT, scan).await {
            Err(_) => {
                // 注意：超时与协作取消路径无法在确定性测试中覆盖（SCAN_TIMEOUT 为 15s，
                // 测试中不可触发），该路径仅由代码审查保障，未经自动化验证。
                cancelled.store(true, Ordering::Relaxed);
                return Err(
                    "Error: Search timed out after 15 seconds. Please use a more specific pattern."
                        .into(),
                );
            }
            Ok(Err(e)) => return Err(format!("Error: {e}").into()),
            Ok(Ok(body)) => body,
        };

        if let Some(warn) = pattern_warn {
            Ok(format!("Note: {warn}\n\n{body}"))
        } else {
            Ok(body)
        }
    }
}

#[cfg(test)]
#[path = "glob_test.rs"]
mod tests;
