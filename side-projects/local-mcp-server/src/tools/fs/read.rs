//! Read 工具：逐字复刻 `peri-middlewares/src/tools/filesystem/read.rs` 的分支与文案。
//!
//! 分支顺序（与源实现一致，顺序本身是契约）：
//! 1. `file_path` 缺失 → 必需参数错误；
//! 2. `.pdf` + `pages` → `[PDF READING NOT YET SUPPORTED]` 占位（**不是**错误）；
//! 3. 二进制扩展名（34 项）→ `[BINARY FILE DETECTED]`（**先于**存在性检查）；
//! 4. 元数据：> 32 MiB → 报错；不存在 → `Error: File not found at {file_path}`（原文参数）；
//!    目录 → 目录 listing；0 字节 → `[EMPTY FILE]`；
//! 5. 读为 UTF-8（失败即源实现的 io 错误文案）；
//! 6. `offset`/`limit` 切片 → `{:>6}\t` 行号 → 长行截断 → 总输出 5000 字节按行截断。
//!
//! `prefers_persist=true` 是宿主投影事实（源 `read.rs:301`），Read **从不落盘**——落盘会被
//! 二次 Read 再编号。该事实登记在 `tests/fixtures/schemas/read.json`，不在 wire 输出里。

use crate::output::truncate_bytes;

use super::args::parse_line_number;
use super::folder::list_directory;
use super::limits;
use super::{FsContext, FsFailure, FsOutcome};

/// 执行 Read。
pub(super) fn execute(ctx: &FsContext<'_>) -> Result<FsOutcome, FsFailure> {
    let file_path = ctx.arguments["file_path"].as_str().ok_or_else(|| {
        FsFailure::text(
            "The 'file_path' parameter is required for the Read tool. Provide the absolute path to the file.",
        )
    })?;
    let offset =
        parse_line_number(&ctx.arguments["offset"], "offset", 1).map_err(FsFailure::text)?;
    let limit = parse_line_number(&ctx.arguments["limit"], "limit", limits::READ_MAX_LINES)
        .map_err(FsFailure::text)?;
    let pages = ctx.arguments["pages"].as_str();
    let requested = ctx.requested();
    let display = ctx.display_path();
    let extension = extension_of(requested.components());

    if let Some(extension) = extension.as_deref() {
        if extension.eq_ignore_ascii_case("pdf") && pages.is_some() {
            return Ok(FsOutcome::text(format!(
                "[PDF READING NOT YET SUPPORTED]\n\nFile path: {}\nPDF reading with page selection is not yet implemented. Use the Bash tool with a PDF reader command as a workaround.",
                display.display()
            ))
            .with_extra("path", display_json(&display)));
        }
    }
    if let Some(extension) = extension.as_deref() {
        if is_binary_extension(&extension.to_lowercase()) {
            return Ok(FsOutcome::text(format!(
                "[BINARY FILE DETECTED]\n\nFile type: .{extension}\nFile path: {}\n\nThis is a binary file and cannot be displayed as text.",
                display.display()
            ))
            .with_extra("path", display_json(&display)));
        }
    }

    // 路径指向授权根本身：源实现把目录转成 listing，这里保持同一语义。
    if requested.is_root() {
        let listing = list_directory(ctx, requested)?;
        let mut outcome = FsOutcome::text(format!(
            "[DIRECTORY DETECTED]\n\nRead received a directory path and converted it to a directory listing. Use folder_operations with operation=\"list\" for explicit directory operations.\n\n{}",
            listing.text
        ))
        .with_extra("path", display_json(&display));
        if listing.truncated {
            outcome = outcome.truncated();
        }
        return Ok(outcome.with_persisted(listing.persisted_path));
    }

    let mut entry = match ctx.root().open_entry(requested) {
        Ok(entry) => entry,
        Err(error) if error.is_not_found() => {
            return Err(FsFailure::text(format!(
                "Error: File not found at {file_path}"
            )))
        }
        Err(error) => return Err(ctx.access_failure(error)),
    };
    let metadata = entry
        .metadata()
        .map_err(|error| FsFailure::text(error.to_string()))?;
    if metadata.len() > limits::READ_MAX_FILE_SIZE {
        return Err(FsFailure::text(format!(
            "Error: File too large ({} bytes, max {} bytes). offset/limit cannot bypass the file-size limit; use Grep to locate content or another suitable file-processing tool.",
            metadata.len(),
            limits::READ_MAX_FILE_SIZE
        )));
    }
    if metadata.is_dir() {
        let listing = list_directory(ctx, requested)?;
        let mut outcome = FsOutcome::text(format!(
            "[DIRECTORY DETECTED]\n\nRead received a directory path and converted it to a directory listing. Use folder_operations with operation=\"list\" for explicit directory operations.\n\n{}",
            listing.text
        ))
        .with_extra("path", display_json(&display));
        if listing.truncated {
            outcome = outcome.truncated();
        }
        return Ok(outcome.with_persisted(listing.persisted_path));
    }
    if metadata.len() == 0 {
        return Ok(FsOutcome::text(format!(
            "[EMPTY FILE]\n\nFile path: {}\nThe file is empty (0 bytes).",
            display.display()
        ))
        .with_extra("path", display_json(&display)));
    }

    let content = match entry.read_to_string() {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(FsFailure::text(format!(
                "Error: File not found at {file_path}"
            )))
        }
        Err(error) => return Err(FsFailure::text(error.to_string())),
    };

    let lines: Vec<&str> = content.split('\n').collect();
    let start = offset - 1;
    if start >= lines.len() {
        return Err(FsFailure::text(format!(
            "Error: offset {offset} exceeds file length ({} lines). Valid offsets are 1..={}; omit offset to read from the beginning. Do not guess another offset or use offset to probe the file end.",
            lines.len(),
            lines.len()
        )));
    }
    let end = (start + limit).min(lines.len());
    let selected = &lines[start..end];

    let mut line_truncated = false;
    let mut numbered: Vec<String> = Vec::new();
    for (index, line) in selected.iter().enumerate() {
        let line_number = start + index + 1;
        let line_char_count = line.chars().count();
        let content = if line_char_count > limits::READ_MAX_CHARS_PER_LINE {
            line_truncated = true;
            format!(
                "[LINE TRUNCATED: {line_char_count} characters total; retained first {} characters before output-level truncation] {}",
                limits::READ_MAX_CHARS_PER_LINE,
                line.chars()
                    .take(limits::READ_MAX_CHARS_PER_LINE)
                    .collect::<String>()
            )
        } else {
            (*line).to_string()
        };
        numbered.push(format!("{line_number:>6}\t{content}"));
    }

    let mut output = numbered.join("\n");
    let mut output_truncated = false;
    if output.len() > limits::READ_MAX_OUTPUT_BYTES {
        output_truncated = true;
        let original_output_bytes = output.len();
        let total_lines = lines.len();
        let mut budget = limits::READ_MAX_OUTPUT_BYTES;
        let mut kept_lines = 0usize;
        for line in &numbered {
            if line.len() + 1 > budget {
                break;
            }
            budget -= line.len() + 1;
            kept_lines += 1;
        }
        if kept_lines == 0 {
            // 单行就超过上限：退回字节截断，引导跳过该行继续读。
            let truncated_line = truncate_bytes(&numbered[0], limits::READ_MAX_OUTPUT_BYTES);
            let next_offset = start + 2;
            output = format!(
                "{truncated_line}\n[Output truncated: {original_output_bytes} bytes total; line {next_offset} exceeds the output limit; use offset={next_offset} to read the rest of the file]"
            );
        } else {
            let shown_start = start + 1;
            let shown_end = start + kept_lines;
            let next_offset = shown_end + 1;
            output = format!(
                "{}\n[Output truncated: {original_output_bytes} bytes total; showing lines {shown_start}..={shown_end} of {total_lines}; continue reading with offset={next_offset}]",
                numbered[..kept_lines].join("\n")
            );
        }
    }

    Ok(FsOutcome {
        text: output,
        truncated: output_truncated || line_truncated,
        persisted_path: None,
        extra: vec![
            ("path", display_json(&display)),
            ("start_line", serde_json::json!(start + 1)),
            ("end_line", serde_json::json!(end)),
            ("total_lines", serde_json::json!(lines.len())),
        ],
    })
}

/// 末段文件名的扩展名（源实现 `resolved.extension()`：取最后一个 `.` 之后的部分）。
fn extension_of(components: &[String]) -> Option<String> {
    let name = components.last()?;
    let path = std::path::Path::new(name);
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_string)
}

/// 二进制扩展名清单（源实现 `is_binary_extension` 的 34 项，逐字保留）。
pub(super) fn is_binary_extension(extension: &str) -> bool {
    matches!(
        extension,
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "bmp"
            | "ico"
            | "webp"
            | "tiff"
            | "pdf"
            | "doc"
            | "docx"
            | "xls"
            | "xlsx"
            | "ppt"
            | "pptx"
            | "zip"
            | "rar"
            | "7z"
            | "tar"
            | "gz"
            | "mp3"
            | "wav"
            | "ogg"
            | "flac"
            | "mp4"
            | "avi"
            | "mkv"
            | "mov"
            | "exe"
            | "dll"
            | "so"
            | "dylib"
            | "bin"
            | "class"
    )
}

fn display_json(display: &std::path::Path) -> serde_json::Value {
    serde_json::json!(display.to_string_lossy())
}
