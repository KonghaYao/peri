use rmcp::{
    model::{CustomRequest, CustomResult},
    ErrorData as McpError,
};
use std::io::BufRead;
use std::path::{Component, Path};

const MAX_MENTION_LINES: usize = 2000;
/// 模型可见正文的 UTF-8 字节预算（H7）：单行超大文件不得整段进入上下文。
/// 与行数上限**先到先截**；截断处给出可继续读取的行号。
const MAX_MENTION_CONTENT_BYTES: usize = 32 * 1024;
const MAX_MENTION_DIRECTORY_ENTRIES: usize = 100;
const MAX_MENTION_FILE_BYTES: u64 = 32 * 1024 * 1024;

/// Resolve an @path against this Workspace instance, including symlink targets.
fn mention_path(cwd: &str, path: &str) -> Result<std::path::PathBuf, McpError> {
    let relative = Path::new(path);
    if path.is_empty()
        || relative.is_absolute()
        || path.starts_with('\\')
        || relative
            .components()
            .any(|component| matches!(component, Component::Prefix(_)))
    {
        return Err(McpError::invalid_params(
            "invalid workspace mention path",
            None,
        ));
    }
    let mut depth = 0usize;
    for component in relative.components() {
        match component {
            Component::ParentDir => {
                depth = depth.checked_sub(1).ok_or_else(|| {
                    McpError::invalid_params("invalid workspace mention path", None)
                })?;
            }
            Component::Normal(_) => depth += 1,
            _ => {}
        }
    }
    let root = Path::new(cwd)
        .canonicalize()
        .map_err(|_| McpError::internal_error("workspace mention read failed", None))?;
    let resolved = root
        .join(relative)
        .canonicalize()
        .map_err(|_| McpError::internal_error("workspace mention read failed", None))?;
    if !resolved.starts_with(&root) {
        return Err(McpError::invalid_params(
            "invalid workspace mention path",
            None,
        ));
    }
    Ok(resolved)
}

fn read_mention_sync(cwd: &str, request: CustomRequest) -> Result<CustomResult, McpError> {
    let params = request.params.as_ref();
    let path = params
        .and_then(|params| params.get("path"))
        .and_then(|path| path.as_str())
        .ok_or_else(|| McpError::invalid_params("workspace/readMention requires path", None))?;
    let line_start = mention_line_number(params, "lineStart")?;
    let line_end = mention_line_number(params, "lineEnd")?;
    if matches!((line_start, line_end), (Some(start), Some(end)) if end < start) {
        return Err(McpError::invalid_params(
            "invalid workspace mention line range",
            None,
        ));
    }
    let resolved = mention_path(cwd, path)?;
    let metadata = std::fs::metadata(&resolved)
        .map_err(|_| McpError::internal_error("workspace mention read failed", None))?;
    if metadata.is_dir() {
        // Keep only the lexicographically first N+1 names so directory size cannot
        // grow this request's memory use. N+1 also gives an exact truncation flag.
        let mut entries: Vec<String> = Vec::with_capacity(MAX_MENTION_DIRECTORY_ENTRIES + 1);
        for entry in std::fs::read_dir(&resolved)
            .map_err(|_| McpError::internal_error("workspace mention read failed", None))?
        {
            let entry = entry
                .map_err(|_| McpError::internal_error("workspace mention read failed", None))?;
            let mut name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                name.push('/');
            }
            if entries.len() == MAX_MENTION_DIRECTORY_ENTRIES + 1
                && name >= *entries.last().unwrap()
            {
                continue;
            }
            let index = entries.partition_point(|entry| entry <= &name);
            entries.insert(index, name);
            entries.truncate(MAX_MENTION_DIRECTORY_ENTRIES + 1);
        }
        let truncated = entries.len() > MAX_MENTION_DIRECTORY_ENTRIES;
        entries.truncate(MAX_MENTION_DIRECTORY_ENTRIES);
        return Ok(CustomResult::new(serde_json::json!({
            "path": path, "content": entries.join("\n"), "lineStart": null, "lineEnd": null,
            "truncated": truncated, "isDir": true,
        })));
    }
    if !metadata.is_file() || metadata.len() > MAX_MENTION_FILE_BYTES {
        return Err(McpError::internal_error(
            "workspace mention read failed",
            None,
        ));
    }
    let body = read_mention_body(&resolved, line_start, line_end)?;
    let content = if body.truncated {
        // 截断说明必须**可行动**：能按行续读就给行号；单行本身超预算时不编造
        // 行号续读位置（那会得到同样的空结果），改为明确原因。
        let note = match (body.cut_within_line, body.resume_line) {
            (true, _) => {
                format!("; this line exceeds the {MAX_MENTION_CONTENT_BYTES}-byte budget, use a narrower read")
            }
            (false, Some(line)) => format!("; continue with lineStart={line}"),
            (false, None) => String::new(),
        };
        format!("{}\n... (truncated{note})", body.content)
    } else {
        body.content
    };
    Ok(CustomResult::new(serde_json::json!({
        "path": path, "content": content, "lineStart": line_start, "lineEnd": line_end,
        "truncated": body.truncated, "isDir": false,
    })))
}

/// 按行范围与预算读取正文的结果。
struct MentionBody {
    content: String,
    truncated: bool,
    /// 截断后可继续读取的 1-based 行号（截断处所在行）；行内截断时为 None。
    resume_line: Option<usize>,
    /// 截断发生在**行内**（该行本身超出剩余预算）：行号续读不可表达。
    cut_within_line: bool,
}

/// 按行范围与预算读取正文。
///
/// 三重约束（先到先截）：调用方已判的文件大小门限、行数上限
/// [`MAX_MENTION_LINES`]、模型可见正文的 UTF-8 字节预算
/// [`MAX_MENTION_CONTENT_BYTES`]。范围为 `[line_start, line_end]`（1-based、
/// 含端点）；范围外的行只跳过不保留，因此读取量受行范围与预算共同约束，
/// 不为「只取几行」的请求把整份文件读进内存。
///
/// 截断一律有标记：按行收束时给出可继续读取的行号；单行自身超预算时给该行的
/// 有界 UTF-8 前缀并标记行内截断（不编造无效的行号续读位置）。
fn read_mention_body(
    path: &Path,
    line_start: Option<usize>,
    line_end: Option<usize>,
) -> Result<MentionBody, McpError> {
    let start = line_start.unwrap_or(1).max(1);
    let end = line_end.unwrap_or(usize::MAX);
    if start >= end {
        return Ok(MentionBody {
            content: String::new(),
            truncated: false,
            resume_line: None,
            cut_within_line: false,
        });
    }
    let file = std::fs::File::open(path)
        .map_err(|_| McpError::internal_error("workspace mention read failed", None))?;
    let mut reader = std::io::BufReader::new(file);
    let mut buffer: Vec<u8> = Vec::new();
    let mut content = String::new();
    let mut line_number = 0usize;
    let mut kept_lines = 0usize;
    loop {
        buffer.clear();
        let read = reader
            .read_until(b'\n', &mut buffer)
            .map_err(|_| McpError::internal_error("workspace mention read failed", None))?;
        if read == 0 {
            break;
        }
        line_number += 1;
        if line_number < start {
            continue;
        }
        if line_number > end {
            break;
        }
        let line = buffer.strip_suffix(b"\n").unwrap_or(&buffer);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let line = std::str::from_utf8(line)
            .map_err(|_| McpError::internal_error("workspace mention read failed", None))?;
        if kept_lines >= MAX_MENTION_LINES {
            return Ok(MentionBody {
                content,
                truncated: true,
                resume_line: Some(line_number),
                cut_within_line: false,
            });
        }
        let separator = usize::from(kept_lines > 0);
        let used = content.len() + separator;
        if used + line.len() > MAX_MENTION_CONTENT_BYTES {
            let remaining = MAX_MENTION_CONTENT_BYTES.saturating_sub(used);
            if remaining == 0 {
                return Ok(MentionBody {
                    content,
                    truncated: true,
                    resume_line: Some(line_number),
                    cut_within_line: false,
                });
            }
            // 单行超预算：保留该行的有界前缀（UTF-8 边界），标记行内截断。
            let cut = floor_char_boundary(line, remaining);
            if separator == 1 {
                content.push('\n');
            }
            content.push_str(&line[..cut]);
            return Ok(MentionBody {
                content,
                truncated: true,
                resume_line: None,
                cut_within_line: true,
            });
        }
        if separator == 1 {
            content.push('\n');
        }
        content.push_str(line);
        kept_lines += 1;
    }
    Ok(MentionBody {
        content,
        truncated: false,
        resume_line: None,
        cut_within_line: false,
    })
}

/// 不大于 `max` 的最大 UTF-8 字符边界（不切断多字节字符）。
fn floor_char_boundary(text: &str, max: usize) -> usize {
    let mut end = max.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

fn mention_line_number(
    params: Option<&serde_json::Value>,
    field: &str,
) -> Result<Option<usize>, McpError> {
    match params.and_then(|params| params.get(field)) {
        None => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| McpError::invalid_params("invalid workspace mention line range", None)),
    }
}

pub(crate) async fn read_mention(
    cwd: &str,
    request: CustomRequest,
) -> Result<CustomResult, McpError> {
    let cwd = cwd.to_string();
    tokio::task::spawn_blocking(move || read_mention_sync(&cwd, request))
        .await
        .map_err(|_| McpError::internal_error("workspace mention read failed", None))?
}

pub(crate) async fn read_text(cwd: &str, request: CustomRequest) -> Result<CustomResult, McpError> {
    let path = request
        .params
        .as_ref()
        .and_then(|params| params.get("path"))
        .and_then(|path| path.as_str())
        .filter(|path| !path.is_empty())
        .ok_or_else(|| McpError::invalid_params("workspace/readText requires path", None))?;
    let path = std::path::Path::new(cwd).join(path);
    let text = tokio::fs::read_to_string(path)
        .await
        .map_err(|_| McpError::internal_error("workspace text read failed", None))?;
    Ok(CustomResult::new(serde_json::json!({ "text": text })))
}

#[cfg(test)]
#[path = "file_observation_test.rs"]
mod tests;
