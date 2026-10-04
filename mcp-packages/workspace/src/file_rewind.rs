//! Workspace-owned, session-scoped reversal of recorded Write/Edit calls.

use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path, PathBuf},
};

use rmcp::{
    model::{CustomRequest, CustomResult},
    ErrorData as McpError,
};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Change {
    Write {
        path: String,
        content: Option<String>,
    },
    Edit {
        path: String,
        old_string: String,
        new_string: String,
    },
}

pub(crate) async fn rewind_files(
    cwd: &str,
    request: CustomRequest,
) -> Result<CustomResult, McpError> {
    let changes: Vec<Change> = serde_json::from_value(
        request
            .params
            .as_ref()
            .and_then(|p| p.get("changes"))
            .cloned()
            .ok_or_else(|| invalid("missing changes"))?,
    )
    .map_err(|_| invalid("invalid changes"))?;
    if changes.is_empty() || changes.len() > 100 {
        return Err(invalid("rewind requires 1..100 changes"));
    }
    let root = tokio::fs::canonicalize(cwd)
        .await
        .map_err(|_| invalid("Workspace root unavailable"))?;
    let mut planned: HashMap<PathBuf, Option<String>> = HashMap::new();
    let mut order = Vec::new();
    let mut completed_writes = HashSet::new();
    // Validate the complete batch before the first mutation. If any history record
    // no longer matches the current Workspace content, leave every file untouched.
    for change in changes.into_iter().rev() {
        let raw = match &change {
            Change::Write { path, .. } | Change::Edit { path, .. } => path,
        };
        let (path, relative) = checked_path(&root, raw).await?;
        // A later Write replaced all earlier contents of the same file. Once
        // reversed to HEAD (or absence), older edits/writes need no replay.
        if completed_writes.contains(&path) {
            continue;
        }
        let current = match planned.get(&path) {
            Some(content) => content.clone(),
            None => tokio::fs::read_to_string(&path).await.ok(),
        };
        let replacement = match change {
            Change::Write { content, .. } => {
                let content = content
                    .ok_or_else(|| invalid("Write history lacks content; rewind refused"))?;
                if current.as_deref() != Some(content.as_str()) {
                    return Err(invalid("Write output changed externally; rewind refused"));
                }
                completed_writes.insert(path.clone());
                head_content(&root, &relative).await?
            }
            Change::Edit {
                old_string,
                new_string,
                ..
            } => {
                if new_string.is_empty()
                    || current
                        .as_deref()
                        .is_none_or(|text| text.matches(&new_string).count() != 1)
                {
                    return Err(invalid(
                        "Edit output changed or is ambiguous; rewind refused",
                    ));
                }
                Some(current.unwrap().replacen(&new_string, &old_string, 1))
            }
        };
        if !planned.contains_key(&path) {
            order.push(path.clone());
        }
        planned.insert(path, replacement);
    }
    for path in order {
        match planned.remove(&path).expect("planned path") {
            Some(content) => tokio::fs::write(&path, content).await.map_err(|_| {
                McpError::internal_error(
                    "Workspace rewind write failed; some files may already be changed",
                    None,
                )
            })?,
            None => tokio::fs::remove_file(&path).await.map_err(|_| {
                McpError::internal_error(
                    "Workspace rewind remove failed; some files may already be changed",
                    None,
                )
            })?,
        }
    }
    Ok(CustomResult::new(serde_json::json!({"ok": true})))
}

async fn checked_path(root: &Path, raw: &str) -> Result<(PathBuf, PathBuf), McpError> {
    let input = Path::new(raw);
    let relative = if input.is_absolute() {
        input
            .strip_prefix(root)
            .map_err(|_| invalid("path outside Workspace"))?
    } else {
        input
    };
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(invalid("unsafe rewind path"));
    }
    let path = root.join(relative);
    let actual = tokio::fs::canonicalize(&path)
        .await
        .map_err(|_| invalid("rewind file missing"))?;
    if !actual.starts_with(root) || !actual.is_file() {
        return Err(invalid("rewind file escapes Workspace or is not a file"));
    }
    Ok((actual, relative.to_path_buf()))
}

async fn head_content(root: &Path, relative: &Path) -> Result<Option<String>, McpError> {
    let repo = tokio::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(root)
        .output()
        .await
        .map_err(|_| invalid("git unavailable for Write rewind"))?;
    if !repo.status.success() {
        return Ok(None);
    }
    let repo_root = PathBuf::from(String::from_utf8_lossy(&repo.stdout).trim());
    let repo_path = root
        .join(relative)
        .strip_prefix(&repo_root)
        .map_err(|_| invalid("Workspace outside Git repository"))?
        .to_path_buf();
    let tracked = tokio::process::Command::new("git")
        .args(["ls-files", "--error-unmatch", "--"])
        .arg(&repo_path)
        .current_dir(&repo_root)
        .output()
        .await
        .map_err(|_| invalid("git index lookup failed"))?;
    if !tracked.status.success() {
        return Ok(None);
    }
    let spec = format!("HEAD:{}", repo_path.to_string_lossy());
    let output = tokio::process::Command::new("git")
        .args(["show", &spec])
        .current_dir(repo_root)
        .output()
        .await
        .map_err(|_| invalid("git HEAD read failed"))?;
    if !output.status.success() {
        return Err(invalid("tracked file missing at HEAD"));
    }
    String::from_utf8(output.stdout)
        .map(Some)
        .map_err(|_| invalid("tracked HEAD file is not UTF-8"))
}

fn invalid(message: &'static str) -> McpError {
    McpError::invalid_params(message, None)
}

#[cfg(test)]
#[path = "file_rewind_test.rs"]
mod tests;
