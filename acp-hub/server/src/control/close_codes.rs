//! 关闭码与客户端重连策略（架构 §4.7 表，设计稿 `f5-channel-control.md` §13）。
//!
//! 关闭码决策：4500/4501/4502 为 acp-hub 专用码（1000–4999 为应用可用区），
//! 1011/1013 为标准码。

/// 机器离线（`MACHINE_OFFLINE`）：停止自动重连，展示手动重试（§4.7）。
///
/// 【决策】M1 单 machine 语义：client 连接上 action 分派遇 `MACHINE_OFFLINE`
/// 且连接不再可服务时由 server 关闭；多 machine 时代改为仅 `action_error`
/// （设计稿保留策略表）。
pub const CLOSE_MACHINE_OFFLINE: u16 = 4500;

/// keep_alive 超时：不在后台自动重连（§4.7）。
pub const CLOSE_KEEPALIVE_TIMEOUT: u16 = 4501;

/// 配置性永久失败（spawn 配置错误 / 认证失败）：停止自动重连（§4.7/§9.2）。
pub const CLOSE_CONFIG_FATAL: u16 = 4502;

/// 关闭码 → 客户端重连策略（§4.7 表；gateway 关闭前选择）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconnectPolicy {
    /// 停止自动重连（4500：展示手动重试）。
    Stop,
    /// 不在后台自动重连（4501；用户手动重连）。
    ManualOnly,
    /// 永久失败（4502：停止自动重连，提示配置修复）。
    StopPermanent,
    /// 退避重连（1011/1013 等标准码）。
    Backoff,
}

/// 关闭码 → 重连策略映射（§4.7 表；未列出的应用码默认 [`ReconnectPolicy::Backoff`]）。
pub fn reconnect_policy(code: u16) -> ReconnectPolicy {
    match code {
        CLOSE_MACHINE_OFFLINE => ReconnectPolicy::Stop,
        CLOSE_KEEPALIVE_TIMEOUT => ReconnectPolicy::ManualOnly,
        CLOSE_CONFIG_FATAL => ReconnectPolicy::StopPermanent,
        _ => ReconnectPolicy::Backoff,
    }
}

#[cfg(test)]
mod close_codes_test {
    use super::*;

    #[test]
    fn policy_table() {
        assert_eq!(reconnect_policy(4500), ReconnectPolicy::Stop);
        assert_eq!(reconnect_policy(4501), ReconnectPolicy::ManualOnly);
        assert_eq!(reconnect_policy(4502), ReconnectPolicy::StopPermanent);
        assert_eq!(reconnect_policy(1011), ReconnectPolicy::Backoff);
        assert_eq!(reconnect_policy(1013), ReconnectPolicy::Backoff);
    }
}
