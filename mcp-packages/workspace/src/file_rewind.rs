//! Workspace-owned, session-scoped reversal of recorded Write/Edit calls.
//!
//! Write replaces the whole file, so Git `HEAD` is not what the call replaced.
//! The caller sends each call's tool-call id; this module restores the pre-image
//! the Workspace recorded for that call and refuses when it is missing or the
//! current content no longer equals the recorded output.

use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
};

use rmcp::{
    model::{CustomRequest, CustomResult},
    ErrorData as McpError,
};
use serde::Deserialize;

use crate::rewind_snapshot::RewindSnapshots;

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Change {
    Write {
        path: String,
        /// Written payload. Reversal uses the recorded pre/post images, but the
        /// payload still proves the history entry and snapshot belong together.
        content: Option<String>,
        tool_call_id: Option<String>,
    },
    Edit {
        path: String,
        old_string: String,
        new_string: String,
        tool_call_id: Option<String>,
    },
}

pub(crate) async fn rewind_files(
    cwd: &str,
    snapshots: &RewindSnapshots,
    owner: Option<&str>,
    request: CustomRequest,
) -> Result<CustomResult, McpError> {
    let params = request
        .params
        .as_ref()
        .ok_or_else(|| invalid("missing changes"))?;
    let changes: Vec<Change> = serde_json::from_value(
        params
            .get("changes")
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
    let mut planned: HashMap<PathBuf, Option<Vec<u8>>> = HashMap::new();
    let mut expected_on_disk: HashMap<PathBuf, Option<Vec<u8>>> = HashMap::new();
    let mut order = Vec::new();
    // Validate the complete batch before the first mutation. If any history record
    // no longer matches the current Workspace content, leave every file untouched.
    for change in changes.into_iter().rev() {
        let raw = match &change {
            Change::Write { path, .. } | Change::Edit { path, .. } => path,
        };
        let (path, _) = checked_path(&root, raw).await?;
        let current = match planned.get(&path) {
            Some(content) => content.clone(),
            None => read_optional(&path).await?,
        };
        let replacement = match change {
            Change::Write {
                content,
                tool_call_id,
                ..
            } => {
                let tool_call_id = tool_call_id
                    .ok_or_else(|| invalid("Write rewind lacks call identity; rewind refused"))?;
                let snapshot = snapshots
                    .get(owner, &tool_call_id)
                    .ok_or_else(|| invalid("Write pre-image unavailable; rewind refused"))?;
                if snapshot.path != path {
                    return Err(invalid("Write pre-image target mismatch; rewind refused"));
                }
                if !write_payload_matches(&snapshot, content.as_deref()) {
                    return Err(invalid(
                        "Write history does not match recorded pre-image; rewind refused",
                    ));
                }
                if current.as_deref() != Some(snapshot.post.as_slice()) {
                    return Err(invalid("Write output changed externally; rewind refused"));
                }
                snapshot.pre
            }
            Change::Edit {
                old_string,
                new_string,
                tool_call_id,
                ..
            } => {
                let recorded = tool_call_id.as_deref().and_then(|id| {
                    let snapshot = snapshots.get(owner, id)?;
                    (snapshot.path == path).then_some(snapshot)
                });
                match recorded {
                    Some(snapshot) => {
                        if current.as_deref() != Some(snapshot.post.as_slice()) {
                            return Err(invalid("Edit output changed externally; rewind refused"));
                        }
                        snapshot.pre
                    }
                    // Histories recorded before pre-image capture keep the
                    // self-contained string reversal: the call itself carries the
                    // exact old/new pair.
                    None => reverse_edit(current.as_deref(), &old_string, &new_string)?,
                }
            }
        };
        if !planned.contains_key(&path) {
            order.push(path.clone());
            expected_on_disk.insert(path.clone(), current);
        }
        planned.insert(path, replacement);
    }
    for path in order {
        let replacement = planned.remove(&path).expect("planned path");
        let expected = expected_on_disk.remove(&path).expect("expected path");
        let apply_path = path.clone();
        tokio::task::spawn_blocking(move || {
            crate::filesystem::transaction::replace_if_unchanged(
                &apply_path,
                expected.as_deref(),
                replacement.as_deref(),
            )
        })
        .await
        .map_err(|_| {
            McpError::internal_error(
                "Workspace rewind task failed; some files may already be changed",
                None,
            )
        })?
        .map_err(|error| {
            let message = match error {
                crate::filesystem::transaction::RewindCommitError::ChangedSinceValidation => {
                    "Workspace rewind target changed during apply; some files may already be changed"
                }
                crate::filesystem::transaction::RewindCommitError::Commit(error) => {
                    tracing::warn!(?error, "workspace rewind commit failed");
                    "Workspace rewind write failed; some files may already be changed"
                }
            };
            McpError::internal_error(message, None)
        })?;
    }
    Ok(CustomResult::new(serde_json::json!({"ok": true})))
}

/// The recorded payload must be what the snapshot says the call produced:
/// full replacement equals `post`, appended payload equals `post` after `pre`.
fn write_payload_matches(
    snapshot: &crate::rewind_snapshot::Snapshot,
    content: Option<&str>,
) -> bool {
    let Some(content) = content else {
        return true;
    };
    if snapshot.post == content.as_bytes() {
        return true;
    }
    snapshot.pre.as_deref().is_some_and(|pre| {
        let mut combined = Vec::with_capacity(pre.len() + content.len());
        combined.extend_from_slice(pre);
        combined.extend_from_slice(content.as_bytes());
        combined == snapshot.post
    })
}

/// Rebuild the pre-edit bytes from the recorded old/new pair.
fn reverse_edit(
    current: Option<&[u8]>,
    old_string: &str,
    new_string: &str,
) -> Result<Option<Vec<u8>>, McpError> {
    let text = current
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .filter(|_| !new_string.is_empty())
        .ok_or_else(|| invalid("Edit output changed or is ambiguous; rewind refused"))?;
    if text.matches(new_string).count() != 1 {
        return Err(invalid(
            "Edit output changed or is ambiguous; rewind refused",
        ));
    }
    Ok(Some(text.replacen(new_string, old_string, 1).into_bytes()))
}

async fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, McpError> {
    match tokio::fs::read(path).await {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(invalid("rewind file is not readable")),
    }
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

fn invalid(message: &'static str) -> McpError {
    McpError::invalid_params(message, None)
}

#[cfg(test)]
#[path = "file_rewind_test.rs"]
mod tests;
