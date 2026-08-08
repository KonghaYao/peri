//! 审计结构化日志最小集（`docs/architecture.md` §9.4）。
//!
//! §1.3 排除完整审计，但保留结构化操作日志（动作类型/commandId/token_id/
//! 结果/耗时，天然由 §9.3 日志规范承载）作为未来审计基础。
//!
//! 脱敏纪律（§9.3）：只记关联 ID/状态/耗时/大小，token 本体/正文/参数/
//! 密钥永不落日志。字段集合恒 ⊆ `{action, command_id, token_id, result,
//! duration_ms, auth_failed_total}`（§6.2 T8 断言）。
//!
//! 触发点：`auth.machine` / `auth.client`（本模块）、`token.generate`（main.rs
//! token 子命令）；`conn.open` / `conn.close`（F5 gateway 复用同一 helper）。

use std::time::Duration;

/// 审计最小集：动作类型 / commandId / token_id / 结果 / 耗时 /
/// （可选）认证失败总数快照（§4.8：失败路径携带，结构化日志即聚合事实源）。
///
/// `action` 取 `"auth.machine"` / `"auth.client"` / `"token.generate"` /
/// `"conn.open"` / `"conn.close"` 等稳定枚举值；`result` 取 `"ok"` /
/// `"unknown_token"` / `"replay"` 等短稳定串（不做自由文本，保持可聚合）。
pub fn audit(
    action: &str,
    command_id: Option<&str>,
    token_id: Option<&str>,
    result: &str,
    took: Duration,
    auth_failed_total: Option<u64>,
) {
    tracing::info!(
        target: "acp_hub.audit",
        action,
        command_id = command_id.map(ToOwned::to_owned),
        token_id = token_id.map(ToOwned::to_owned),
        result,
        duration_ms = took.as_millis() as u64,
        auth_failed_total = auth_failed_total,
    );
}
