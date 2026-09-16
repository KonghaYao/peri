//! 请求方法清单，以及"未被 SDK 路由的请求"的归因（WP-005）。
//!
//! SDK 先按方法名把请求解析成强类型结构；解析失败的请求会落到 `on_custom_request`。
//! 此时**方法名仍然可信**，而形状已经不可信，于是有两种完全不同的失败必须分开：
//!
//! - 方法不在本服务清单里 ⇒ `-32601 Method not found`（调用方写错了方法）；
//! - 方法在清单里但 `params` 不满足 schema ⇒ `-32602 Invalid params`（调用方写错了参数）。
//!
//! 若不区分，"缺字段的 `tools/call`"会被报成"不存在的方法"，与冻结的错误边界表
//! （`artifacts/designs/WP-001/interfaces.md` §4.1）以及规范 `server/tools` 的示例不符。
//!
//! 清单的每个成员都必须有真实的处理路径：`tools/list`/`tools/call` 来自 `tools` 能力，
//! `resources/*` 与 `subscriptions/listen` 来自 `resources` 能力（未接入任务来源时它们
//! 由能力声明与 handler 共同拒绝），`initialize`/`ping`/`server/discover` 是生命周期方法。

/// 本服务实现的请求方法（顺序即文档顺序，与 wire 行为无关）。
pub const IMPLEMENTED_METHODS: [&str; 10] = [
    "initialize",
    "ping",
    "server/discover",
    "tools/list",
    "tools/call",
    "resources/list",
    "resources/read",
    "resources/subscribe",
    "resources/unsubscribe",
    "subscriptions/listen",
];

/// 未被 SDK 路由的请求的归因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnroutedRequest {
    /// 方法本身不在 [`IMPLEMENTED_METHODS`] 里。
    NotImplemented,
    /// 方法存在，但请求参数不符合该方法的 schema。
    MalformedParams,
}

/// 归因一个未被 SDK 路由的请求。
pub fn classify_unrouted(method: &str) -> UnroutedRequest {
    if is_implemented(method) {
        UnroutedRequest::MalformedParams
    } else {
        UnroutedRequest::NotImplemented
    }
}

/// 方法是否属于本服务的实现清单。
pub fn is_implemented(method: &str) -> bool {
    IMPLEMENTED_METHODS.contains(&method)
}

/// 清单中 legacy 专用的方法（`2026-07-28` 起由 `subscriptions/listen` 取代）。
///
/// 保留它们是为了 legacy 客户端有可用的订阅路径；modern 请求若调用这两个方法，
/// 仍按"形状可解析"处理（SDK 的 deprecated 标记不影响 wire 行为）。
pub const LEGACY_ONLY_METHODS: [&str; 2] = ["resources/subscribe", "resources/unsubscribe"];

/// 清单中 modern 专用的方法。
pub const MODERN_ONLY_METHODS: [&str; 1] = ["subscriptions/listen"];

#[cfg(test)]
#[path = "methods_test.rs"]
mod tests;
