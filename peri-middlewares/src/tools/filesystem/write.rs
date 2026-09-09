use peri_agent::tools::BaseTool;
use serde_json::Value;
use std::sync::{Arc, Mutex};

use super::draft::{draft_hint_en, DraftAccessError, DraftStore};
use super::transaction::{target_key, with_target_lock, CommitError, SENTINEL_REJECTION};

const WRITE_FILE_DESCRIPTION: &str = include_str!("descriptions/write.md");
const WRITE_IO_ERROR: &str = "Write failed while committing the file.";
const DRAFT_UNKNOWN: &str =
    "Draft is unknown or no longer available. Retry by providing content directly.";
const DRAFT_TARGET_MISMATCH: &str =
    "Draft belongs to a different file_path. Retry with the original file_path or content.";

/// Write tool - 与 TypeScript write_tool 对齐
pub struct WriteFileTool {
    pub cwd: String,
    /// 失败草稿存储(进程级内存);None = PERI_WRITE_DRAFT=0 关闭
    drafts: Option<Arc<Mutex<DraftStore>>>,
}

impl WriteFileTool {
    pub fn new(cwd: impl Into<String>) -> Self {
        Self::with_draft(cwd, super::draft::draft_enabled())
    }

    /// 测试注入构造:enabled=false 时完全禁用草稿(不创建 store)
    pub(crate) fn with_draft(cwd: impl Into<String>, enabled: bool) -> Self {
        Self {
            cwd: cwd.into(),
            drafts: enabled.then(|| Arc::new(Mutex::new(DraftStore::new()))),
        }
    }

    fn save_draft(&self, target: &str, content: &str, append: bool) -> Option<String> {
        self.drafts.as_ref().map(|store| {
            store
                .lock()
                .unwrap()
                .save(target, content.to_string(), append)
        })
    }

    fn commit(
        &self,
        target: &std::path::Path,
        content: &str,
        append: bool,
    ) -> Result<usize, CommitError> {
        with_target_lock(target, |locked| {
            let pre = locked.read_pre().map_err(|_| CommitError::Io)?;
            let pre = pre.unwrap_or_default();
            let post = if append {
                let mut post = Vec::with_capacity(pre.len() + content.len());
                post.extend_from_slice(&pre);
                post.extend_from_slice(content.as_bytes());
                post
            } else {
                content.as_bytes().to_vec()
            };
            let total_lines = post.split(|byte| *byte == b'\n').count()
                - usize::from(post.last() == Some(&b'\n'));
            locked.guard_and_commit(&pre, &post)?;
            Ok(total_lines)
        })
    }

    fn success_message(
        &self,
        target: &std::path::Path,
        content: &str,
        append: bool,
        total_lines: usize,
    ) -> String {
        let line_count = content.lines().count();
        let lines_label = if line_count == 1 { "line" } else { "lines" };
        let rel = target.strip_prefix(&self.cwd).unwrap_or(target).display();
        if append {
            format!(
                "Appended {line_count} {lines_label} to {rel} (file total: {total_lines} lines)"
            )
        } else {
            format!("Wrote {line_count} {lines_label} {rel}")
        }
    }

    fn direct_write(
        &self,
        target: &std::path::Path,
        target_id: &str,
        content: &str,
        append: bool,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        match self.commit(target, content, append) {
            Ok(total_lines) => Ok(self.success_message(target, content, append, total_lines)),
            Err(CommitError::Sentinel) => Err(SENTINEL_REJECTION.into()),
            Err(CommitError::Io) => {
                let hint = self
                    .save_draft(target_id, content, append)
                    .map(|id| draft_hint_en(&id, content))
                    .unwrap_or_default();
                Err(format!("{WRITE_IO_ERROR}{hint}").into())
            }
        }
    }

    fn restore_write(
        &self,
        target: &std::path::Path,
        target_id: &str,
        draft_id: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let Some(store) = &self.drafts else {
            return Err(DRAFT_UNKNOWN.into());
        };
        let mut store = store.lock().unwrap();
        match store.with_exact_entry(draft_id, target_id, |content, append| {
            self.commit(target, content, append)
                .map(|total_lines| self.success_message(target, content, append, total_lines))
        }) {
            Ok(message) => Ok(message),
            Err(DraftAccessError::Unknown) => Err(DRAFT_UNKNOWN.into()),
            Err(DraftAccessError::WrongTarget) => Err(DRAFT_TARGET_MISMATCH.into()),
            Err(DraftAccessError::Operation(CommitError::Sentinel)) => {
                Err(SENTINEL_REJECTION.into())
            }
            Err(DraftAccessError::Operation(CommitError::Io)) => Err(WRITE_IO_ERROR.into()),
        }
    }
}

#[async_trait::async_trait]
impl BaseTool for WriteFileTool {
    fn name(&self) -> &str {
        "Write"
    }

    fn is_direct(&self) -> bool {
        true
    }

    fn namespace(&self) -> Option<&str> {
        Some("filesystem")
    }

    fn prompt_declaration(&self) -> Option<String> {
        Some(
            "Write a file → `{{name}}` (full contents). Use `{{name}}` for writing files, not `echo >`/`sed`/`awk`."
                .to_string(),
        )
    }

    fn description(&self) -> &str {
        WRITE_FILE_DESCRIPTION
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The absolute path to the file to write (must be absolute, not relative)"
                },
                "content": {
                    "type": "string",
                    "description": "The full content to write to the file. Either 'content' or 'from_draft' must be provided."
                },
                "from_draft": {
                    "type": "string",
                    "description": "A draft id returned in a previous Write error message. Recover the failed write without resending content. Mutually exclusive with 'content'; reuse the original file_path."
                },
                "append": {
                    "type": "boolean",
                    "description": "If true, append content to the end of the file instead of overwriting. Use this for writing large files in chunks: first call Write without append to create the file with the initial content, then call Write with append=true to add more content. This avoids sending the entire file content in a single tool call, saving context window space.",
                    "default": false
                }
            },
            "required": ["file_path"]
        })
    }

    fn timeout(&self) -> Option<std::time::Duration> {
        None
    }

    async fn invoke(
        &self,
        input: Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let file_path = input["file_path"]
            .as_str()
            .ok_or("The 'file_path' parameter is required for the Write tool.")?;
        let target = target_key(&self.cwd, file_path);
        let target_id = target.to_string_lossy().to_string();
        let content = input["content"]
            .as_str()
            .filter(|content| !content.trim().is_empty() && *content != "__omit__");
        let from_draft = input["from_draft"]
            .as_str()
            .filter(|id| !id.trim().is_empty() && *id != "__omit__");

        if let Some(content) = content {
            return self.direct_write(
                &target,
                &target_id,
                content,
                input["append"].as_bool().unwrap_or(false),
            );
        }
        if let Some(draft_id) = from_draft {
            return self.restore_write(&target, &target_id, draft_id);
        }
        Err("Either 'content' or 'from_draft' must be provided for the Write tool.".into())
    }
}

#[cfg(test)]
#[path = "write_test.rs"]
mod tests;
