use rmcp::{
    model::{CustomRequest, CustomResult},
    ErrorData as McpError,
};
use std::path::{Component, Path};

const MAX_MENTION_LINES: usize = 2000;
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
    let raw = std::fs::read_to_string(resolved)
        .map_err(|_| McpError::internal_error("workspace mention read failed", None))?;
    let lines: Vec<&str> = raw.lines().collect();
    let start = line_start.unwrap_or(1).saturating_sub(1);
    let end = line_end.unwrap_or(lines.len()).min(lines.len());
    let selected = if start >= lines.len() || start >= end {
        &[][..]
    } else {
        &lines[start..end]
    };
    let truncated = selected.len() > MAX_MENTION_LINES;
    let content = selected
        .iter()
        .take(MAX_MENTION_LINES)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    let content = if truncated {
        format!("{content}\n... (truncated)")
    } else {
        content
    };
    Ok(CustomResult::new(serde_json::json!({
        "path": path, "content": content, "lineStart": line_start, "lineEnd": line_end,
        "truncated": truncated, "isDir": false,
    })))
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
