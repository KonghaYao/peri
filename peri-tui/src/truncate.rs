//! 共享的工具调用摘要 / 截断 helper。
//!
//! `router.rs`（streaming 通道：`tool-started` / `tool-ended` JSON 事件）与
//! `view_mapper.rs`（view-commit 通道：`ToolCard` ViewModel 的 input_summary /
//! output_summary）原本各自维护一份相似但**格式分歧**的实现（同一工具调用在两个
//! 通道显示不同格式，最典型的是 `pattern: "TODO"` vs `pattern: TODO`）。
//!
//! 本模块提供共享 helper，统一两通道的显示格式：
//!
//! - `pattern` / `query` / `cmd` / `path` 等键名前缀统一为**带引号** repr（JSON value
//!   的标准 repr，与 streaming 通道历史行为一致），便于人眼快速定位 value 边界
//! - 其余字段（`file_path`、`command`、`operation` 等）按既有约定：无引号裸值
//!
//! 统一后，同一工具调用在 streaming 与 view-commit 通道显示**相同**格式，避免 TUI
//! 渲染层对两通道做差异化处理。
//!
//! 路径精简：工具卡片头行展示的路径类参数（`file_path` / `folder_path` 等）若以
//! TUI 启动时的工作目录为前缀，则去掉前缀显示为相对路径（如
//! `Read (peri-model/src/protocol/mod.rs)`），减少超长绝对路径对头行的占用。
//! 非 cwd 前缀的路径保持原样。cwd 由 `set_display_cwd` 在启动时设置一次。

use std::sync::OnceLock;

/// TUI 启动时的工作目录（进程生命周期内不变），用于路径显示精简。
static DISPLAY_CWD: OnceLock<String> = OnceLock::new();

/// 设置显示用 cwd。仅在启动时调用一次；重复调用忽略（保持首次值，避免测试污染）。
pub fn set_display_cwd(cwd: impl Into<String>) {
    let _ = DISPLAY_CWD.set(cwd.into());
}

/// 去掉 `cwd` 前缀的路径精简（纯函数）。
///
/// 规则：
/// - `cwd` 为空、`cwd` 为根目录、`path == cwd` → 原样返回（避免空串/退化显示）
/// - `path` 以 `cwd + 分隔符` 开头 → 去掉前缀，返回相对路径
/// - 其余 → 原样返回
pub fn shorten_path_for_display(path: &str, cwd: &str) -> String {
    if cwd.is_empty() || cwd == "/" || cwd == "\\" || path == cwd {
        return path.to_string();
    }
    let cwd_trimmed = cwd.trim_end_matches(['/', '\\']);
    if let Some(rest) = path.strip_prefix(cwd_trimmed)
        && let Some(rel) = rest.strip_prefix('/').or_else(|| rest.strip_prefix('\\'))
    {
        return rel.to_string();
    }
    path.to_string()
}

/// 读取全局显示 cwd 后精简路径；未设置 cwd 时原样返回。
fn shorten_path(path: &str) -> String {
    match DISPLAY_CWD.get() {
        Some(cwd) => shorten_path_for_display(path, cwd),
        None => path.to_string(),
    }
}

/// Truncate text to `max_chars` Unicode code points (CJK-safe).
///
/// 等价于历史上的 `truncate_text` / `truncate_chars`（两份实现完全一致，仅命名不同）。
pub fn truncate_text(s: &str, max_chars: usize) -> String {
    let len = s.chars().count();
    if len <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{}...", truncated)
    }
}

/// Produce a one-line summary of a tool's JSON input.
///
/// 合并 router.rs 与 view_mapper.rs 的实现：
/// - 兼具 view_mapper 的 Read `path` 兜底、folder_operations 完整字段
/// - 兼具 router 的 `_` 分支多级兜底（path/file_path → query/pattern → command → 首个 KV）
/// - `pattern` / `query` 统一为**带引号**格式
pub fn summarize_input(name: &str, input: &serde_json::Value) -> String {
    // 优先按 Object 提取，非 Object 走 truncate 兜底（与 view_mapper 一致）
    let obj = match input {
        serde_json::Value::Object(map) => map,
        other => return truncate_text(&other.to_string(), 120),
    };

    // 字符串字段读取 helper（view_mapper 风格，空值返回 ""）
    let str_val = |key: &str| -> String {
        obj.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    // 旧 router 风格的 Option<&str> 读取（用于 "有值才格式化带引号" 分支）
    let field = |key: &str| -> Option<&str> { obj.get(key).and_then(|v| v.as_str()) };

    match name {
        // ── 无前缀，文件路径不截断但精简 cwd 前缀（view_mapper：file_path 为空时回退 path；
        //    若仍为空返回 "(empty input)" 占位，避免渲染层显示空白卡片）──
        "Read" | "Write" | "Edit" => {
            let p = shorten_path(&str_val("file_path"));
            if p.is_empty() {
                let fallback = shorten_path(&str_val("path"));
                if fallback.is_empty() {
                    "(empty input)".to_string()
                } else {
                    fallback
                }
            } else {
                p
            }
        }
        // ── 无前缀，命令截断 400 ──
        "Bash" => truncate_text(&str_val("command"), 400),
        // ── 有前缀 pattern:，pattern 截断 200，带引号（统一格式）──
        "Glob" | "Grep" => {
            let p = str_val("pattern");
            let p = if p.is_empty() { str_val("query") } else { p };
            format!(r#"pattern: "{}""#, truncate_text(&p, 200))
        }
        // ── "operation folder_path"，不截断但精简 cwd 前缀 ──
        "folder_operations" => {
            let op = str_val("operation");
            let fp = shorten_path(&str_val("folder_path"));
            format!("{} {}", op, fp)
        }
        // ── query: 截断 60，带引号（统一格式）──
        "WebSearch" => field("query")
            .map(|s| format!(r#"query: "{}""#, truncate_text(s, 60)))
            .unwrap_or_else(|| "(empty input)".to_string()),
        // ── url: 不截断 ──
        "WebFetch" => field("url")
            .map(|s| format!("url: {}", s))
            .unwrap_or_else(|| "(empty input)".to_string()),
        // ── 空字符串（文档无参数）──
        "TodoWrite" => String::new(),
        // ── task_id 截断 12 ──
        "AgentResult" => truncate_text(&str_val("task_id"), 12),
        // ── file_path 不截断但精简 cwd 前缀 ──
        "artifact" => shorten_path(&str_val("file_path")),
        // ── operation 截断 40 ──
        "LSP" => truncate_text(&str_val("operation"), 40),
        // ── tool_name 截断 40 ──
        "ExecuteExtraTool" => truncate_text(&str_val("tool_name"), 40),
        // ── query 截断 40 ──
        "SearchExtraTools" => truncate_text(&str_val("query"), 40),
        // ── 兜底：router 风格的多级探测（path/file_path → query/pattern → command → 首个 KV）──
        _ => {
            if let Some(path) = obj.get("path").or_else(|| obj.get("file_path")) {
                return format!(
                    "path: {}",
                    truncate_text(&shorten_path(&path.to_string()), 120)
                );
            }
            if let Some(query) = obj.get("query").or_else(|| obj.get("pattern")) {
                return format!("query: {}", truncate_text(&query.to_string(), 120));
            }
            if let Some(cmd) = obj.get("command") {
                return format!("cmd: {}", truncate_text(&cmd.to_string(), 120));
            }
            if let Some((k, v)) = obj.iter().next() {
                let raw = v.as_str().unwrap_or("");
                return format!("{}: {}", k, truncate_text(raw, 100));
            }
            "(empty input)".to_string()
        }
    }
}

/// Produce a one-line summary of a tool's output.
///
/// 合并 router.rs 与 view_mapper.rs 的实现，保留 view_mapper 更丰富的特殊分支
/// （WebFetch / TodoWrite / Read / Glob / Grep 折叠态行数），streaming 与 view-commit
/// 共享同一展示语义。
pub fn summarize_output(name: &str, output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    match name {
        "Edit" | "Write" => {
            let lines = trimmed.lines().count();
            if lines <= 3 {
                return truncate_text(trimmed, 200);
            }
            format!("{} lines changed", lines)
        }
        "WebFetch" => {
            let lines = trimmed.lines().count();
            let bytes = output.len();
            format!(
                "{} lines · {} bytes\n{}",
                lines,
                bytes,
                truncate_text(trimmed, 400)
            )
        }
        // TodoWrite 返回全量内容（显示完整 todo 列表）
        "TodoWrite" => trimmed.to_string(),
        // Read / Glob / Grep — 折叠态显示行数
        "Read" | "Glob" | "Grep" => {
            let lines = trimmed.lines().count();
            format!("{} lines", lines)
        }
        _ => truncate_text(trimmed, 200),
    }
}

#[cfg(test)]
#[path = "truncate_test.rs"]
mod tests;
