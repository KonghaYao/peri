//! ACP Stdio 传输的共享上下文和 session 状态。
//!
//! stdio 部署单元的过渡期装配上下文：`cfg` 为统一装配产物
//! （[`crate::host::assemble::assemble_server_config`]，与 TUI/notify 路径
//! 同一份 `AcpServerConfig`），`sessions` 为 stdio 侧会话状态映射，统一后
//! 改用宿主 [`crate::host::SessionState`]（会话创建方即 writer，见
//! `SessionState::lease`）。handler 的字段引用统一经 `ctx.cfg.xxx`。

use std::time::Duration;

/// Stdio 传输环境的共享上下文
pub(super) struct StdioContext {
    pub(super) cfg: crate::host::AcpServerConfig,
    pub(super) sessions:
        parking_lot::RwLock<std::collections::HashMap<String, crate::host::SessionState>>,
}

/// 解析 `PERI_ASK_USER_TIMEOUT_SECS` 环境变量值（纯逻辑，便于单测）：
/// 缺失/非法回落默认 300 秒；`0` 表示不超时（返回 None）。
fn parse_ask_user_timeout(value: Option<&str>) -> Option<Duration> {
    match value.and_then(|v| v.parse::<u64>().ok()).unwrap_or(300) {
        0 => None,
        seconds => Some(Duration::from_secs(seconds)),
    }
}

pub(super) fn ask_user_timeout() -> Option<Duration> {
    parse_ask_user_timeout(std::env::var("PERI_ASK_USER_TIMEOUT_SECS").ok().as_deref())
}

#[cfg(test)]
#[path = "context_test.rs"]
mod tests;
