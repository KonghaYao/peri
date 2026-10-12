//! 退出后的会话恢复提示。
//!
//! TUI 退出（`/exit`、双击 Ctrl+C）后由 `entry::run_kit_fullscreen` 调用：
//! 此时 ratatui-kit 的 `RestoreGuard` 已执行 `ratatui::restore()`，终端回到
//! 普通模式，向 stdout 写一行纯文本不会与渲染循环并发写终端。
//! 未建立会话（首次配置取消、离线模式）时不打印。

use crate::i18n;
use crate::kit::atoms;

/// 组合恢复提示文本；会话 ID 为空（未建立会话）时返回 `None`。
pub fn resume_hint(session_id: &str) -> Option<String> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return None;
    }
    Some(i18n::tr_args(
        "exit-resume-hint",
        &[("session".into(), session_id.into())],
    ))
}

/// 读取当前会话 ID 组合提示；无会话时不产生文本。
pub fn current_resume_hint() -> Option<String> {
    let session_id = atoms::ACTIVE_SESSION_ID.state().read().clone();
    resume_hint(&session_id)
}

/// 退出路径调用：把恢复提示写到 stdout。
pub fn print_resume_hint() {
    if let Some(hint) = current_resume_hint() {
        println!("{hint}");
    }
}

#[cfg(test)]
#[path = "exit_hint_test.rs"]
mod tests;
