//! `folder_operations` 工具：逐字复刻 `peri-middlewares/src/tools/filesystem/folder.rs`。
//!
//! 契约要点：
//! - 参数：`operation`（create/list/exists/deep_scan）、`folder_path`、`recursive`（默认 true）、
//!   `max_depth`（deep_scan，默认 3，越界 clamp 到 1..=10）；
//! - `list`：不存在 → `Folder not found: {path}`；存在但非目录 → `Path exists but is not a folder: {path}`；
//! - `exists` 对非目录不报错（`✓ Folder exists …\n  Type: Directory|File`）；
//! - 两类 listing 都在超过 500 条时截断并落盘完整内容（公平分配：目录优先 250 条）；
//! - `deep_scan` 用 unicode tree 前缀，条目按路径排序保证确定性。

use chrono::{TimeZone, Utc};

use crate::capability::RequestedPath;

use super::args::parse_optional_u64;
use super::limits;
use super::{FsContext, FsFailure, FsOutcome};

/// 执行 `folder_operations`。
pub(super) fn execute(ctx: &FsContext<'_>) -> Result<FsOutcome, FsFailure> {
    let operation = ctx.arguments["operation"]
        .as_str()
        .ok_or_else(|| FsFailure::text("Missing operation parameter"))?;
    let _folder_path = ctx.arguments["folder_path"]
        .as_str()
        .ok_or_else(|| FsFailure::text("Missing folder_path parameter"))?;
    let recursive = ctx.arguments["recursive"].as_bool().unwrap_or(true);
    let display = ctx.display_path();

    match operation {
        "create" => {
            if recursive {
                ctx.root()
                    .create_dir_all(ctx.requested())
                    .map_err(|error| ctx.access_failure(error))?;
            } else {
                ctx.root()
                    .create_dir(ctx.requested())
                    .map_err(|error| ctx.access_failure(error))?;
            }
            Ok(FsOutcome::text(format!(
                "\u{2713} Folder created successfully at: {}",
                display.display()
            ))
            .with_extra("operation", serde_json::json!("create"))
            .with_extra("path", serde_json::json!(display.to_string_lossy())))
        }
        "exists" => {
            let exists = ctx
                .root()
                .exists(ctx.requested())
                .map_err(|error| ctx.access_failure(error))?;
            if !exists {
                return Ok(FsOutcome::text(format!(
                    "\u{2717} Folder does not exist at: {}",
                    display.display()
                ))
                .with_extra("operation", serde_json::json!("exists"))
                .with_extra("path", serde_json::json!(display.to_string_lossy()))
                .with_extra("exists", serde_json::json!(false)));
            }
            let is_dir = ctx
                .root()
                .entry_is_dir(ctx.requested())
                .map_err(|error| ctx.access_failure(error))?;
            let kind = if is_dir { "Directory" } else { "File" };
            Ok(FsOutcome::text(format!(
                "\u{2713} Folder exists at: {}\n  Type: {kind}",
                display.display()
            ))
            .with_extra("operation", serde_json::json!("exists"))
            .with_extra("path", serde_json::json!(display.to_string_lossy()))
            .with_extra("exists", serde_json::json!(true))
            .with_extra("kind", serde_json::json!(kind)))
        }
        "list" => {
            require_directory(ctx)?;
            let listing = list_directory(ctx, ctx.requested())?;
            let mut outcome = FsOutcome::text(listing.text)
                .with_extra("operation", serde_json::json!("list"))
                .with_extra("path", serde_json::json!(display.to_string_lossy()))
                .with_extra("entries", serde_json::json!(listing.entries));
            if listing.truncated {
                outcome = outcome.truncated();
            }
            Ok(outcome.with_persisted(listing.persisted_path))
        }
        "deep_scan" => {
            require_directory(ctx)?;
            let max_depth = match parse_optional_u64(&ctx.arguments["max_depth"], "max_depth")
                .map_err(FsFailure::text)?
            {
                Some(value) => (value as usize).clamp(1, 10),
                None => 3,
            };
            let scan = deep_scan(ctx, ctx.requested(), max_depth)?;
            Ok(FsOutcome::text(scan.text)
                .with_persisted(scan.persisted_path)
                .with_extra("operation", serde_json::json!("deep_scan"))
                .with_extra("path", serde_json::json!(display.to_string_lossy()))
                .with_extra("max_depth", serde_json::json!(max_depth))
                .with_extra("entries", serde_json::json!(scan.entries)))
        }
        other => Err(FsFailure::text(format!("Unknown operation: {other}"))),
    }
}

fn require_directory(ctx: &FsContext<'_>) -> Result<(), FsFailure> {
    let display = ctx.display_path();
    let exists = ctx
        .root()
        .exists(ctx.requested())
        .map_err(|error| ctx.access_failure(error))?;
    if !exists {
        return Err(FsFailure::text(format!(
            "Folder not found: {}",
            display.display()
        )));
    }
    if !ctx
        .root()
        .entry_is_dir(ctx.requested())
        .map_err(|error| ctx.access_failure(error))?
    {
        return Err(FsFailure::text(format!(
            "Path exists but is not a folder: {}",
            display.display()
        )));
    }
    Ok(())
}

/// 一次目录 listing 的结果。
pub(super) struct DirectoryListing {
    /// 文本（源实现格式）。
    pub text: String,
    /// 截断时的落盘路径。
    pub persisted_path: Option<String>,
    /// 条目总数（截断前）。
    pub entries: usize,
    /// 是否发生截断。
    pub truncated: bool,
}

/// `list` 操作（也被 Read 的目录分支复用）。
pub(super) fn list_directory(
    ctx: &FsContext<'_>,
    requested: &RequestedPath,
) -> Result<DirectoryListing, FsFailure> {
    let dir = ctx
        .root()
        .open_dir(requested)
        .map_err(|error| ctx.access_failure(error))?;
    let entries = dir.entries().map_err(|error| ctx.access_failure(error))?;

    let mut folders: Vec<String> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    for entry in &entries {
        let name = &entry.name;
        let size = entry.metadata.len();
        let modified = format_modified(&entry.metadata);
        if entry.metadata.is_dir() {
            folders.push(format!("  \u{1F4C1} {name}/ ({size} bytes, {modified})"));
        } else {
            files.push(format!("  \u{1F4C4} {name} ({size} bytes, {modified})"));
        }
    }

    let total_folders = folders.len();
    let total_files = files.len();
    let total = total_folders + total_files;
    let truncated = total > limits::FOLDER_MAX_LIST_ENTRIES;
    let mut persisted_path = None;
    let mut persist_hint = String::new();

    if truncated {
        // 截断前先落盘完整列表（源实现同序）。
        let full_list = folders
            .iter()
            .chain(files.iter())
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        let total_summary = format!("Total: {total_folders} directories, {total_files} files");
        let full_text = format!("{full_list}\n{total_summary}");
        let outcome = ctx.runtime().persister().persist(ctx.root(), &full_text);
        persisted_path = outcome.path;
        persist_hint = outcome.hint;

        let half = limits::FOLDER_MAX_LIST_ENTRIES / 2;
        folders.truncate(half.min(folders.len()));
        files.truncate((limits::FOLDER_MAX_LIST_ENTRIES - folders.len()).min(files.len()));
    }

    let mut result = format!("\u{1F4C1} {}\n\n", dir.path().display());
    if !folders.is_empty() {
        result.push_str("Directories:\n");
        for folder in &folders {
            result.push_str(folder);
            result.push('\n');
        }
        result.push('\n');
    }
    if !files.is_empty() {
        result.push_str("Files:\n");
        for file in &files {
            result.push_str(file);
            result.push('\n');
        }
    }
    if truncated {
        result.push_str(&format!(
            "\n[Output truncated: {total} total entries, showing first {}]{persist_hint}",
            limits::FOLDER_MAX_LIST_ENTRIES
        ));
    }
    result.push_str(&format!(
        "\nTotal: {total_folders} directories, {total_files} files"
    ));
    Ok(DirectoryListing {
        text: result,
        persisted_path,
        entries: total,
        truncated,
    })
}

struct DeepScan {
    text: String,
    persisted_path: Option<String>,
    entries: usize,
}

/// `deep_scan`：unicode tree + 路径排序 + 截断落盘。
fn deep_scan(
    ctx: &FsContext<'_>,
    requested: &RequestedPath,
    max_depth: usize,
) -> Result<DeepScan, FsFailure> {
    let root = ctx.root();
    let components = requested.components().to_vec();

    struct Entry {
        path: std::path::PathBuf,
        name: String,
        is_dir: bool,
        depth: usize,
        size: u64,
        modified: String,
        is_last: bool,
    }

    let mut entries: Vec<Entry> = Vec::new();
    // 深度换算（F-P3-02，round 7 修复）：源实现用
    // `walkdir::WalkDir::new(root).max_depth(max_depth)`——扫描根 depth=0、直接子项
    // depth=1，因此 `max_depth=N` 只产出 depth ≤ N 的条目。本层的 `RootDir::walk` 以
    // 「起始目录的直接子项」为 depth 0，且在 `depth < limit` 时继续下钻，故传入的上限
    // 必须减一：`limit = N - 1` ⇔ 产出的最大 walkdir depth = `limit + 1 = N`。
    // 修复前传 `Some(max_depth)`，`max_depth=2` 会多带出一层（曾孙条目）。
    let walk_limit = max_depth.saturating_sub(1);
    root.walk(&components, Some(walk_limit), &mut |entry| {
        if entry.is_dir() && super::should_skip_dir(entry.name) {
            return crate::capability::WalkControl::SkipDescend;
        }
        entries.push(Entry {
            path: entry.rel.to_path_buf(),
            name: entry.name.to_string(),
            is_dir: entry.metadata.is_dir(),
            // walkdir 语义：扫描根为 0，其直接子项为 1。
            depth: entry.depth + 1,
            size: entry.metadata.len(),
            modified: format_modified(entry.metadata),
            is_last: false,
        });
        crate::capability::WalkControl::Continue
    })
    .map_err(|error| ctx.access_failure(error))?;

    entries.sort_by(|a, b| a.path.cmp(&b.path));
    for index in 0..entries.len() {
        let current_parent = entries[index].path.parent();
        let next_parent = entries.get(index + 1).and_then(|entry| entry.path.parent());
        entries[index].is_last = current_parent != next_parent;
    }

    let mut output = format!("\u{1F4C1} {}\n\n", ctx.display_path().display());
    let mut ancestor_last: Vec<bool> = Vec::new();
    for entry in &entries {
        while ancestor_last.len() < entry.depth {
            ancestor_last.push(false);
        }
        ancestor_last.truncate(entry.depth);
        let mut prefix = String::new();
        for depth in 0..entry.depth.saturating_sub(1) {
            if depth < ancestor_last.len() && ancestor_last[depth] {
                prefix.push_str("    ");
            } else {
                prefix.push_str("\u{2502}   ");
            }
        }
        if entry.is_last {
            prefix.push_str("\u{2514}\u{2500}\u{2500} ");
        } else {
            prefix.push_str("\u{251C}\u{2500}\u{2500} ");
        }
        if entry.depth > 0 && ancestor_last.len() >= entry.depth {
            ancestor_last[entry.depth - 1] = entry.is_last;
        }
        let icon = if entry.is_dir {
            "\u{1F4C1}"
        } else {
            "\u{1F4C4}"
        };
        let trailing = if entry.is_dir { "/" } else { "" };
        output.push_str(&format!(
            "{prefix}{icon} {}{trailing} ({} bytes, {})\n",
            entry.name, entry.size, entry.modified
        ));
    }

    let total = entries.len();
    let truncated = total > limits::FOLDER_MAX_LIST_ENTRIES;
    let mut persisted_path = None;
    let mut persist_hint = String::new();
    if truncated {
        let outcome = ctx.runtime().persister().persist(ctx.root(), &output);
        persisted_path = outcome.path;
        persist_hint = outcome.hint;
        let lines: Vec<&str> = output.lines().collect();
        let header_lines = 3;
        let max_lines = header_lines + limits::FOLDER_MAX_LIST_ENTRIES;
        if lines.len() > max_lines {
            output = lines[..max_lines].join("\n");
        }
    }
    if truncated {
        output.push_str(&format!(
            "\n[Output truncated: {total} total entries, showing first {}]{persist_hint}",
            limits::FOLDER_MAX_LIST_ENTRIES
        ));
    }
    output.push_str(&format!("\nTotal: {total} entries"));

    Ok(DeepScan {
        text: output,
        persisted_path,
        entries: total,
    })
}

/// `YYYY/MM/DD`（chrono UTC）；时间戳不可用时为 `unknown`（源实现一致）。
fn format_modified(metadata: &crate::capability::EntryMeta) -> String {
    metadata
        .modified()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| {
            Utc.timestamp_opt(duration.as_secs() as i64, 0)
                .single()
                .map(|datetime| datetime.format("%Y/%m/%d").to_string())
                .unwrap_or_else(|| "unknown".to_string())
        })
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use crate::tools::fs::{FsCall, FsRuntime};

    /// F-P3-02 回归：`deep_scan` 的 `max_depth` 必须与源实现
    /// `walkdir::WalkDir::new(root).max_depth(N)`（扫描根 depth=0、直接子项 depth=1，
    /// 只产出 depth ≤ N）逐层一致——本实现曾多下钻一层。
    ///
    /// 树：`top.txt`（depth1）、`l1/`（1）、`l1/mid.txt`（2）、`l1/l2/`（2）、
    /// `l1/l2/deep.txt`（3）。
    #[test]
    fn test_deep_scan_max_depth_matches_walkdir_semantics() {
        let temp = tempfile::tempdir().expect("临时目录");
        let base = temp.path().canonicalize().expect("规范化临时目录");
        std::fs::create_dir_all(base.join("l1/l2")).expect("创建目录");
        std::fs::write(base.join("top.txt"), "t\n").expect("写文件");
        std::fs::write(base.join("l1/mid.txt"), "m\n").expect("写文件");
        std::fs::write(base.join("l1/l2/deep.txt"), "d\n").expect("写文件");
        let runtime = FsRuntime::new(base.clone()).expect("授权根可打开");
        let absolute = base.to_string_lossy().to_string();
        let scan = |max_depth: u64| {
            let call = FsCall {
                tool: "folder_operations".to_string(),
                arguments: serde_json::json!({
                    "operation": "deep_scan",
                    "folder_path": absolute.clone(),
                    "max_depth": max_depth,
                }),
                path: Some(absolute.clone()),
            };
            let response = runtime.call(&call).expect("deep_scan 必须返回工具结果");
            assert!(!response.is_error, "{}", response.text);
            response.text
        };

        let depth_one = scan(1);
        assert!(
            depth_one.contains("top.txt") && depth_one.contains("l1/"),
            "max_depth=1 含直接子项：{depth_one}"
        );
        assert!(
            !depth_one.contains("mid.txt"),
            "max_depth=1 不得含一级子目录内容（源 walkdir 语义）：{depth_one}"
        );

        let depth_two = scan(2);
        assert!(depth_two.contains("mid.txt"), "{depth_two}");
        assert!(
            !depth_two.contains("deep.txt"),
            "max_depth=2 不得含二级子目录内容（修复前的多一层行为）：{depth_two}"
        );

        let depth_three = scan(3);
        assert!(depth_three.contains("deep.txt"), "{depth_three}");
    }
}
