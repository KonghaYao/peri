//! Compact 阶段 hook 触发辅助函数。
//!
//! 为 PreCompact / PostCompact 提供统一入口，封装 [`super::fire_standalone_lifecycle_hooks`]
//! 的参数组装。调用方（ACP builder / compact pipeline）仅需传入已注册的 hook 列表与会话上下文。

use crate::hooks::types::{HookEvent, RegisteredHook};

/// 触发 PreCompact hook（compact 开始前）。
///
/// 所有匹配 `HookEvent::PreCompact` 的已注册 hook 将被同步/异步执行。
/// 此函数为 fire-and-forget——hook 结果不影响 compact 主流程。
pub async fn fire_pre_compact(
    registered_hooks: &[RegisteredHook],
    cwd: &str,
    session_id: &str,
    transcript_path: &str,
    current_model: &str,
    message_count: usize,
) {
    super::dispatcher::fire_standalone_lifecycle_hooks(
        registered_hooks,
        HookEvent::PreCompact,
        cwd,
        session_id,
        transcript_path,
        current_model,
        Some(message_count),
        None,
    )
    .await;
}

/// 触发 PostCompact hook（compact 完成后）。
///
/// 无论 compact 是否实际执行了消息压缩，均触发此 hook。
/// `compacted` 表示是否实际压缩了消息，`affected_count` 为受影响的条目数。
pub async fn fire_post_compact(
    registered_hooks: &[RegisteredHook],
    cwd: &str,
    session_id: &str,
    transcript_path: &str,
    current_model: &str,
    message_count: usize,
) {
    super::dispatcher::fire_standalone_lifecycle_hooks(
        registered_hooks,
        HookEvent::PostCompact,
        cwd,
        session_id,
        transcript_path,
        current_model,
        Some(message_count),
        None,
    )
    .await;
}
