//! 进入会话时的 context 占用展示（设计 §2.2）。
//!
//! 已保存的 `threads.context_usage_tokens` 只是最后提交的展示快照，不是本次请求的
//! 精确 usage；百分比按当前有效 context window 现算。启动载荷缺失或值为 NULL 时
//! 保持「未知」，不得落成 0%。

use serde_json::Value;

use super::atoms::{CONTEXT_USAGE, ContextUsage, RENDER_HEARTBEAT};

/// 从 `session/load` 响应读取服务端已保存的占用快照。
///
/// 旧服务端没有该字段，按未知处理（缺失即未知）。
pub(crate) fn from_load_response(response: &Value) -> ContextUsage {
    let tokens = response
        .pointer("/_meta/peri.contextUsage/tokens")
        .and_then(Value::as_u64);
    let window = response
        .pointer("/_meta/peri.contextUsage/window")
        .and_then(Value::as_u64);
    ContextUsage::from_tokens(tokens, window)
}

/// 进入会话时先显示已保存值；未知也覆盖，避免沿用上一会话的百分比。
pub(crate) fn establish_session_usage(usage: ContextUsage) {
    *CONTEXT_USAGE.state().write() = Some(usage);
    touch();
}

/// 运行期快照覆盖展示值。`has_usage` 为 false 表示 tracker 尚无有效占用（首轮或
/// 早退），此时保留现有展示（可能是进入时的已保存值），不写成 0%。
pub(crate) fn apply_state_snapshot(tokens: u64, has_usage: bool, window: Option<u64>) {
    let (true, Some(window)) = (has_usage, window) else {
        return;
    };
    *CONTEXT_USAGE.state().write() = Some(ContextUsage::Known { tokens, window });
    touch();
}

fn touch() {
    RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
}

#[cfg(test)]
#[path = "context_usage_test.rs"]
mod tests;
