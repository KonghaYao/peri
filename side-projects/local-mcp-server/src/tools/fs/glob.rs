//! Glob 工具：逐字复刻 `peri-middlewares/src/tools/filesystem/glob.rs`。
//!
//! 契约要点：
//! - 参数：`pattern`（必需）、`path`（可选，缺省 = 工作区根，等价源实现的 cwd）；
//! - 匹配字符串是**相对搜索根**的路径（`\` 归一为 `/`），`glob::Pattern` 默认
//!   `*`/`?` 跨 `/`；遍历本身用逐目录 `openat` 下钻、不跟随符号链接；
//! - 结果按 mtime **降序**稳定排序（相同 mtime 保持遍历序）；
//! - 1000 条早停；20000 字节超限时落盘全量并内联前 100 条；
//! - 空结果返回 `No files found.`（非错误）；非法 pattern / 目录不存在是错误；
//! - 15 秒扫描超时：本实现是同步执行域，这里用协作式 deadline 检查（每 256 条一次），
//!   语义与源实现的 `spawn_blocking` + 协作 `cancelled` 一致（超时即停止并返回同一文案）。
//!
//! 与源实现的两处实现差异（记录在 handoff）：遍历走 [`crate::capability::RootDir::walk`]
//! 的 fd 锚定版本（源实现用 `walkdir`）；收窄逻辑（`WalkPlan`）保留，但深度按
//! 「相对搜索根的 walkdir 深度」换算，保证过滤条件与源实现逐条等价。

use std::path::Path;
use std::time::{Instant, SystemTime};

use crate::capability::WalkControl;

use super::{limits, should_skip_dir, FsContext, FsFailure, FsOutcome};

/// 执行 Glob。
pub(super) fn execute(ctx: &FsContext<'_>) -> Result<FsOutcome, FsFailure> {
    let pattern = ctx.arguments["pattern"]
        .as_str()
        .ok_or_else(|| FsFailure::text("The 'pattern' parameter is required for the Glob tool."))?;
    let compiled = glob::Pattern::new(pattern).map_err(|error| {
        FsFailure::text(format!(
            "Error: Pattern syntax error in {pattern:?}: {error}"
        ))
    })?;
    let pattern_warn = soft_warn_pattern(pattern);

    let requested = ctx.requested();
    let display_root = ctx.display_path();
    let base_rel = crate::capability::join_components(requested.components());

    if !ctx
        .root()
        .exists(requested)
        .map_err(|error| ctx.access_failure(error))?
    {
        return Err(FsFailure::text(format!(
            "Error: Directory not found: {}",
            display_root.display()
        )));
    }

    let plan = plan_walk(pattern);
    let base_name = requested.components().last().cloned();
    let (start, consumed) =
        narrow_root(base_name.as_deref(), &plan).unwrap_or((requested.components().to_vec(), 0));

    let body = run_scan(
        ctx,
        &compiled,
        &plan,
        &start,
        consumed,
        &base_rel,
        &display_root,
    )?;

    let mut outcome = if let Some(warn) = pattern_warn {
        FsOutcome::text(format!("Note: {warn}\n\n{}", body.text))
    } else {
        FsOutcome::text(body.text)
    };
    outcome = outcome
        .with_persisted(body.persisted_path)
        .with_extra("pattern", serde_json::json!(pattern))
        .with_extra(
            "search_root",
            serde_json::json!(display_root.to_string_lossy()),
        )
        .with_extra("count", serde_json::json!(body.count))
        .with_extra("early_stopped", serde_json::json!(body.early_stopped));
    Ok(outcome)
}

struct ScanBody {
    text: String,
    count: usize,
    early_stopped: bool,
    persisted_path: Option<String>,
}

#[allow(clippy::too_many_arguments)]
fn run_scan(
    ctx: &FsContext<'_>,
    pattern: &glob::Pattern,
    plan: &WalkPlan,
    start: &[String],
    consumed: usize,
    base_rel: &Path,
    display_root: &Path,
) -> Result<ScanBody, FsFailure> {
    // 遍历根本身若是被跳目录，源实现的 depth-0 黑名单检查会剪掉整棵树
    // （含「搜索根就是工作区根」的情形，此时 walk root 名 = 根目录名）。
    let walk_root_name = start
        .last()
        .cloned()
        .or_else(|| {
            base_rel
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
        })
        .or_else(|| {
            ctx.root()
                .base()
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
        });
    if walk_root_name.as_deref().is_some_and(should_skip_dir) {
        return Ok(ScanBody {
            text: "No files found.".to_string(),
            count: 0,
            early_stopped: false,
            persisted_path: None,
        });
    }

    let deadline = Instant::now() + limits::GLOB_SCAN_TIMEOUT;
    let mut results: Vec<(Option<SystemTime>, String)> = Vec::new();
    let mut early_stopped = false;
    let mut timed_out = false;
    let mut seen = 0usize;

    let walk_result = ctx.root().walk(start, None, &mut |entry| {
        seen += 1;
        if seen.is_multiple_of(256) && Instant::now() >= deadline {
            // 协作式超时：spawn_blocking 无法强杀线程，源实现同样靠协作停止。
            timed_out = true;
            return WalkControl::Stop;
        }
        if entry.is_dir() {
            if should_skip_dir(entry.name) {
                return WalkControl::SkipDescend;
            }
            // walkdir 深度换算：条目深度 + 1（walkdir 的遍历根为 0）+ 收窄消费的前缀层数。
            let depth = entry.depth + 1 + consumed;
            if depth == 0 {
                return WalkControl::Continue;
            }
            if depth <= plan.prefix_dirs.len() {
                let expected = plan.prefix_dirs[consumed + entry.depth].as_str();
                return if expected == entry.name {
                    WalkControl::Continue
                } else {
                    WalkControl::SkipDescend
                };
            }
            return if plan.max_depth.is_none_or(|limit| depth <= limit) {
                WalkControl::Continue
            } else {
                WalkControl::SkipDescend
            };
        }
        if entry.is_file() {
            let relative = entry
                .rel
                .strip_prefix(base_rel)
                .unwrap_or(entry.rel)
                .to_path_buf();
            let rel_str = relative.to_string_lossy().replace('\\', "/");
            if pattern.matches(&rel_str) {
                // `Path::join("")` 会多出一个结尾分隔符；与源实现的 `e.path()` 保持一致。
                let display = if relative.as_os_str().is_empty() {
                    display_root.to_path_buf()
                } else {
                    display_root.join(&relative)
                }
                .to_string_lossy()
                .to_string();
                results.push((entry.metadata.modified(), display));
                if results.len() > limits::GLOB_MAX_RESULTS {
                    early_stopped = true;
                    return WalkControl::Stop;
                }
            }
        }
        WalkControl::Continue
    });

    if let Err(error) = walk_result {
        return Err(ctx.access_failure(error));
    }
    if timed_out {
        return Err(FsFailure::text(
            "Error: Search timed out after 15 seconds. Please use a more specific pattern.",
        ));
    }

    results.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    let count = results.len();
    if results.is_empty() {
        return Ok(ScanBody {
            text: "No files found.".to_string(),
            count,
            early_stopped,
            persisted_path: None,
        });
    }

    if results.len() > limits::GLOB_MAX_RESULTS {
        let full = join_paths(&results);
        let truncated = &results[..limits::GLOB_MAX_RESULTS];
        let persisted = ctx.runtime().persister().persist(ctx.root(), &full);
        let stop_note = if early_stopped {
            " (collection stopped at the result limit)"
        } else {
            ""
        };
        let text = format!(
            "{}\n\n[Output truncated: {} files total{}, showing first {}]{}",
            join_paths(truncated),
            count,
            stop_note,
            limits::GLOB_MAX_RESULTS,
            persisted.hint
        );
        return Ok(ScanBody {
            text,
            count,
            early_stopped,
            persisted_path: persisted.path,
        });
    }

    let joined = join_paths(&results);
    if joined.len() > limits::GLOB_MAX_OUTPUT_BYTES {
        let persisted = ctx.runtime().persister().persist(ctx.root(), &joined);
        let head_count = limits::GLOB_HEAD_RESULTS_ON_BYTES_OVERFLOW.min(results.len());
        let head = &results[..head_count];
        let text = super::glob_byte_overflow_text(
            &join_paths(head),
            count,
            joined.len(),
            head_count,
            &persisted.hint,
        );
        return Ok(ScanBody {
            text,
            count,
            early_stopped,
            persisted_path: persisted.path,
        });
    }

    Ok(ScanBody {
        text: joined,
        count,
        early_stopped,
        persisted_path: None,
    })
}

fn join_paths(results: &[(Option<SystemTime>, String)]) -> String {
    results
        .iter()
        .map(|(_, path)| path.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// 软告警 pattern（仍执行，仅前置提示）。
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
fn first_meta_index(pattern: &str) -> Option<usize> {
    pattern
        .char_indices()
        .find_map(|(index, ch)| matches!(ch, '*' | '?' | '[' | '{').then_some(index))
}

/// 遍历边界规划（源实现 `WalkPlan`）。
struct WalkPlan {
    prefix_dirs: Vec<String>,
    max_depth: Option<usize>,
}

fn plan_walk(pattern: &str) -> WalkPlan {
    let leading_slash = pattern.starts_with('/');
    let meta_index = first_meta_index(pattern);

    let prefix_dirs: Vec<String> = if leading_slash {
        // 前导 `/` 的 pattern 匹配绝对路径，相对路径永不命中；保守回退全遍历。
        Vec::new()
    } else {
        let literal = meta_index.map_or(pattern, |index| &pattern[..index]);
        let usable = !literal.contains('\\') && (meta_index.is_none() || literal.ends_with('/'));
        if usable {
            let mut segments: Vec<String> = literal
                .split('/')
                .filter(|segment| !segment.is_empty())
                .map(str::to_string)
                .collect();
            if meta_index.is_none() {
                segments.pop();
            }
            segments
        } else {
            Vec::new()
        }
    };

    let max_depth = if leading_slash || meta_index.is_some() {
        None
    } else {
        Some(pattern.matches('/').count())
    };

    WalkPlan {
        prefix_dirs,
        max_depth,
    }
}

/// 前缀目录链收窄为 walk root；不可收窄返回 `None`（回退全遍历）。
///
/// 回退条件与源实现逐条一致：前缀为空、搜索根本身是被跳目录、前缀链中段（末段除外）是被跳目录、
/// 任一段为 `.`/`..`。返回 `(起点组件, 消费的前缀段数)`。
fn narrow_root(base_name: Option<&str>, plan: &WalkPlan) -> Option<(Vec<String>, usize)> {
    let prefix_dirs = &plan.prefix_dirs;
    if prefix_dirs.is_empty() {
        return None;
    }
    if base_name.is_some_and(should_skip_dir) {
        return None;
    }
    for (index, segment) in prefix_dirs.iter().enumerate() {
        if segment == "." || segment == ".." {
            return None;
        }
        if index + 1 < prefix_dirs.len() && should_skip_dir(segment) {
            return None;
        }
    }
    Some((prefix_dirs.clone(), prefix_dirs.len()))
}
