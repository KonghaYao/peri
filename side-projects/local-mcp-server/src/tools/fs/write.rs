//! Write 工具：逐字复刻 `peri-middlewares/src/tools/filesystem/write.rs` + `transaction.rs`。
//!
//! 契约要点：
//! - 参数：`file_path`（必需）、`content`、`from_draft`、`append`（默认 false）；
//!   `content`/`from_draft` 的占位符规则与源实现一致（空串、纯空白、`__omit__` 视为未提供），
//!   `content` 优先；
//! - 事务：per-target 锁 → 读 pre → append 拼接 → 哨兵检查 → tmp + rename（复制权限位）；
//! - 失败草稿：只有写盘失败落草稿；`content` 优先路径的提示为 `draft_hint_en`；
//! - 成功文案：`Wrote {n} {line|lines} {rel}` /
//!   `Appended {n} {label} to {rel} (file total: {m} lines)`。

use crate::capability::Parents;

use super::draft::{draft_hint_en, DraftAccessError};
use super::transaction::{commit, read_pre, CommitError, SENTINEL_REJECTION, WRITE_IO_ERROR};
use super::{FsContext, FsFailure, FsOutcome};

/// `from_draft` 的 id 不存在。
const DRAFT_UNKNOWN: &str =
    "Draft is unknown or no longer available. Retry by providing content directly.";
/// `from_draft` 的 id 属于另一个 target。
const DRAFT_TARGET_MISMATCH: &str =
    "Draft belongs to a different file_path. Retry with the original file_path or content.";

/// 执行 Write。
pub(super) fn execute(ctx: &FsContext<'_>) -> Result<FsOutcome, FsFailure> {
    let _file_path = ctx.arguments["file_path"].as_str().ok_or_else(|| {
        FsFailure::text("The 'file_path' parameter is required for the Write tool.")
    })?;
    let content = ctx.arguments["content"]
        .as_str()
        .filter(|content| !content.trim().is_empty() && *content != "__omit__");
    let from_draft = ctx.arguments["from_draft"]
        .as_str()
        .filter(|id| !id.trim().is_empty() && *id != "__omit__");

    if let Some(content) = content {
        return direct_write(
            ctx,
            content,
            ctx.arguments["append"].as_bool().unwrap_or(false),
        );
    }
    if let Some(draft_id) = from_draft {
        return restore_write(ctx, draft_id);
    }
    Err(FsFailure::text(
        "Either 'content' or 'from_draft' must be provided for the Write tool.",
    ))
}

fn direct_write(ctx: &FsContext<'_>, content: &str, append: bool) -> Result<FsOutcome, FsFailure> {
    match commit_content(ctx, content, append) {
        Ok(total_lines) => Ok(success_outcome(ctx, content, append, total_lines)),
        Err(CommitError::Sentinel) => Err(FsFailure::text(SENTINEL_REJECTION)),
        Err(CommitError::Io) => {
            // 源实现在 Io 失败时落草稿；哨兵拒绝不落草稿。
            match ctx.runtime().save_draft(&ctx.target_key(), content, append) {
                Some(draft_id) => Err(FsFailure::text(format!(
                    "{WRITE_IO_ERROR}{}",
                    draft_hint_en(&draft_id, content)
                ))
                .with_extra("draft_id", serde_json::json!(draft_id))),
                None => Err(FsFailure::text(WRITE_IO_ERROR)),
            }
        }
    }
}

fn restore_write(ctx: &FsContext<'_>, draft_id: &str) -> Result<FsOutcome, FsFailure> {
    let target_key = ctx.target_key();
    let mut committed: Option<(String, bool)> = None;
    let result = ctx.runtime().with_draft_store(|store| {
        store.with_exact_entry(draft_id, &target_key, |content, append| {
            committed = Some((content.to_string(), append));
            commit_content(ctx, content, append)
        })
    });

    match result {
        // 草稿功能被 `PERI_WRITE_DRAFT=0|false` 关闭：与「id 不存在」同文案（源实现一致）。
        None => Err(FsFailure::text(DRAFT_UNKNOWN)),
        Some(Ok(total_lines)) => {
            let (content, append) = committed.unwrap_or_else(|| (String::new(), false));
            Ok(success_outcome(ctx, &content, append, total_lines))
        }
        Some(Err(DraftAccessError::Unknown)) => Err(FsFailure::text(DRAFT_UNKNOWN)),
        Some(Err(DraftAccessError::WrongTarget)) => Err(FsFailure::text(DRAFT_TARGET_MISMATCH)),
        Some(Err(DraftAccessError::Operation(CommitError::Sentinel))) => {
            Err(FsFailure::text(SENTINEL_REJECTION))
        }
        Some(Err(DraftAccessError::Operation(CommitError::Io))) => {
            Err(FsFailure::text(WRITE_IO_ERROR))
        }
    }
}

/// 提交（在目标锁内）：解析 + 建父目录 → 读 pre → 覆盖/append → 哨兵 + tmp + rename。
///
/// 目标锁的作用域**只覆盖**这一段：源实现同样在 `save_draft` 之前释放目标锁，
/// 避免与 `restore_write`（草稿锁内取目标锁）形成锁序反转。
fn commit_content(ctx: &FsContext<'_>, content: &str, append: bool) -> Result<usize, CommitError> {
    // 写授权根本身：源实现会在 rename 阶段失败并落草稿；这里保持同样的对外结果。
    if ctx.requested().is_root() {
        return Err(CommitError::Io);
    }
    let runtime = ctx.runtime();
    let key = ctx.target_key_path();
    runtime.locks().with_lock(&key, || {
        let target = runtime
            .root()
            .resolve_with(ctx.requested(), Parents::CreateMissing)
            .map_err(|_| CommitError::Io)?;
        let pre = read_pre(runtime.root(), ctx.requested()).map_err(|_| CommitError::Io)?;
        let pre = pre.unwrap_or_default();
        let post = if append {
            let mut post = Vec::with_capacity(pre.len() + content.len());
            post.extend_from_slice(&pre);
            post.extend_from_slice(content.as_bytes());
            post
        } else {
            content.as_bytes().to_vec()
        };
        let outcome = commit(&target, &pre, &post)?;
        Ok(outcome.total_lines)
    })
}

fn success_outcome(
    ctx: &FsContext<'_>,
    content: &str,
    append: bool,
    total_lines: usize,
) -> FsOutcome {
    let line_count = content.lines().count();
    let lines_label = if line_count == 1 { "line" } else { "lines" };
    let relative = ctx.relative_path();
    let text = if append {
        format!(
            "Appended {line_count} {lines_label} to {relative} (file total: {total_lines} lines)"
        )
    } else {
        format!("Wrote {line_count} {lines_label} {relative}")
    };
    FsOutcome::text(text)
        .with_extra(
            "path",
            serde_json::json!(ctx.display_path().to_string_lossy()),
        )
        .with_extra("lines", serde_json::json!(line_count))
        .with_extra("append", serde_json::json!(append))
        .with_extra("total_lines", serde_json::json!(total_lines))
}
