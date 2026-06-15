//! 构建 frozen session data，供 session/new、load、resume、fork 复用。
//!
//! 会话内不可变：system prompt + skills + CLAUDE.md 快照。
//! 实际构造委托给 `SessionManager::build_frozen_data`，避免重复实现。

use super::context::StdioContext;

/// 构建会话级别的冻结数据。
///
/// 委托给 `SessionManager::build_frozen_data`，保证 stdio 与 TUI 路径
/// 使用同一份 frozen 构造逻辑。
pub(super) fn build(
    ctx: &StdioContext,
    cwd: &str,
) -> peri_acp::session::executor::FrozenSessionData {
    ctx.session_manager
        .build_frozen_data(cwd, &ctx.plugin_skill_dirs, &ctx.plugin_agent_dirs)
}
