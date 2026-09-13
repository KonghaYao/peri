//! legacy / modern 时代判定（WP-005）。
//!
//! MCP `2026-07-28` 移除了握手与会话（breaking change），但同一进程要同时服务两代客户端
//! （规范 `basic/versioning#backward-compatibility-with-initialization-based-versions`）。
//! 本模块把"这个请求属于哪一代"固化成**纯函数**：
//!
//! - era 只由**请求自身携带的协议版本**决定，不依赖连接状态或进程状态——因此
//!   "modern 无会话"不是纪律要求，而是结构事实（该判定没有可读的会话输入）；
//! - 版本字符串是 ISO 日期，可直接按字典序比较：`>= 2026-07-28` 即 modern；
//! - 判定为 legacy 不等于"一定会走到 `initialize`"：请求是否被 SDK 接受、版本是否受支持
//!   由 `supported_protocol_versions`（[`crate::mcp::server::SUPPORTED_VERSIONS`]）决定，
//!   不支持版本在进入 handler 前就得到 `-32022`。
//!
//! 该判定只影响**结果形态**（modern 结果带 `resultType`，列表可带 `ttlMs`/`cacheScope`），
//! 不影响授权、能力声明或工具面：不同年代的同一请求必须看到同样的七工具与同样的主体。

use crate::wire::MCP_MODERN_PROTOCOL_VERSION;

/// 请求所属的协议时代。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Era {
    /// `initialize` 握手 + 会话语义（`<= 2025-11-25`）。
    Legacy,
    /// 每请求自描述、无会话（`>= 2026-07-28`）。
    Modern,
}

impl Era {
    /// 由请求携带的协议版本判定时代。
    ///
    /// 输入必须是规范版本字符串（例如 `2026-07-28` / `2025-11-25`）。未知的更高日期会被
    /// 判为 modern：版本比较是单调的，未来的修订版仍然继承"每请求自描述"这一语义。
    pub fn of(version: &str) -> Self {
        if version >= MCP_MODERN_PROTOCOL_VERSION {
            Era::Modern
        } else {
            Era::Legacy
        }
    }

    /// wire 上可读的时代名（用于诊断与证据）。
    pub fn as_str(self) -> &'static str {
        match self {
            Era::Legacy => "legacy",
            Era::Modern => "modern",
        }
    }

    /// 是否 modern。
    pub fn is_modern(self) -> bool {
        matches!(self, Era::Modern)
    }
}

/// 判定请求是否属于 modern 时代；`None` 表示版本尚未确定（尚未协商），按 legacy 处理。
///
/// "版本尚未确定"只可能出现在 legacy 握手之前；此时**不得**给出 modern 专属字段，否则会
/// 让旧客户端收到它不认识的形状。
pub fn is_modern(version: Option<&str>) -> bool {
    version.map(Era::of).is_some_and(Era::is_modern)
}

/// 同 [`is_modern`]，但返回时代而不是布尔值。
pub fn era_of(version: Option<&str>) -> Era {
    version.map(Era::of).unwrap_or(Era::Legacy)
}

#[cfg(test)]
#[path = "era_test.rs"]
mod tests;
