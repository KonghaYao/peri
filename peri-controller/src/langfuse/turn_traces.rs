//! sid → 当前活跃 turn trace 的注册表。
//!
//! 指标事件（`peri-agent::metrics`）没有自己的 turn 上下文，但带 `sid`（会话 =
//! thread id）。tracer 在 turn 开始登记、turn 结束清理；`LangfuseMetricsSink`
//! 按 sid 取回活跃 trace，把指标挂到该 turn 的 trace 下。
//!
//! [不变量] 表项只在 turn 生命周期内存在：`clear` 是 compare-and-remove，
//! 只移除仍指向本 tracer trace 的登记，不会误删后一 turn 的登记。
//! 没有活跃 trace 时（未登记 / 已清理 / 指标无 sid）由出口回退独立 root trace，
//! 不丢事件。

use std::collections::HashMap;

use parking_lot::Mutex;

/// 进程级注册表（随 `LangfuseSession` 共享，所有 turn 与指标出口共用一份）。
#[derive(Default)]
pub struct TurnTraceRegistry {
    active: Mutex<HashMap<String, String>>,
}

impl TurnTraceRegistry {
    /// turn 开始时登记 sid 的活跃 trace；同 sid 重复登记以最后一次为准。
    pub fn register(&self, sid: &str, trace_id: &str) {
        self.active
            .lock()
            .insert(sid.to_string(), trace_id.to_string());
    }

    /// turn 结束时清理；仅当 sid 仍指向该 trace 时才移除。
    pub fn clear(&self, sid: &str, trace_id: &str) {
        let mut active = self.active.lock();
        if active.get(sid).is_some_and(|current| current == trace_id) {
            active.remove(sid);
        }
    }

    /// 取 sid 当前的活跃 trace id。
    pub fn resolve(&self, sid: &str) -> Option<String> {
        self.active.lock().get(sid).cloned()
    }
}

#[cfg(test)]
#[path = "turn_traces_test.rs"]
mod tests;
