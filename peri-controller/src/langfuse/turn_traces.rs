//! sid → 当前活跃 turn trace 的注册表。
//!
//! 指标事件（`peri-agent::metrics`）没有自己的 turn 上下文，但带 `sid`（会话 =
//! thread id）。tracer 在 turn 开始登记、turn 结束清理；`LangfuseMetricsSink`
//! 按 sid 取回活跃 trace 与挂载点，把指标挂到该 turn 的 trace 下。
//!
//! 子 agent 的 thread id 也在 `on_subagent_start` 登记，否则子 agent 的指标只会
//! fallback 成各自独立的 root trace。前台子 agent 随 turn 结束清理；**后台子
//! agent 可以活过父 turn**，其登记保留到 `on_subagent_stop`，避免 turn 结束后
//! 仍在产生的工具失败指标成为独立 root trace。
//!
//! [不变量] 表项只在登记者生命周期内存在：`clear` 与 `clear_trace` 都是
//! compare-and-remove，只移除仍指向本 tracer trace 的登记，不会误删后一 turn
//! 的登记；`clear_trace` 不清理声明跨 turn 存活的后台子 agent。没有活跃 trace
//! 时（未登记 / 已清理 / 指标无 sid）由出口回退独立 root trace，不丢事件。

use std::collections::HashMap;

use parking_lot::Mutex;

/// 活跃 turn 的归属信息：指标事件挂到哪个 trace、哪个父观测下。
#[derive(Clone, Debug)]
pub struct ActiveTurnTrace {
    pub trace_id: String,
    /// 指标事件应挂载的父观测；未采样的 turn 没有根 span，为 None。
    pub parent_observation_id: Option<String>,
    /// 后台子 agent 在父 turn 结束后仍会运行：turn 结束时不清该登记，
    /// 由该 sid 自己的 stop 清理。
    pub outlives_turn: bool,
}

/// 进程级注册表（随 `LangfuseSession` 共享，所有 turn 与指标出口共用一份）。
#[derive(Default)]
pub struct TurnTraceRegistry {
    active: Mutex<HashMap<String, ActiveTurnTrace>>,
}

impl TurnTraceRegistry {
    /// turn 开始时登记 sid 的活跃 trace；同 sid 重复登记以最后一次为准。
    ///
    /// `parent_observation_id` 是该 turn 根 span 的 id：采样的 turn 有根 span，
    /// 未采样的 turn 没有（指标只共享 trace，不挂父观测）。
    pub fn register(&self, sid: &str, trace_id: &str, parent_observation_id: Option<&str>) {
        self.insert(sid, trace_id, parent_observation_id, false);
    }

    /// 登记跨 turn 存活的后台子 agent：turn 结束时保留，直到该 sid 自己的 stop。
    pub fn register_outliving_turn(
        &self,
        sid: &str,
        trace_id: &str,
        parent_observation_id: Option<&str>,
    ) {
        self.insert(sid, trace_id, parent_observation_id, true);
    }

    fn insert(
        &self,
        sid: &str,
        trace_id: &str,
        parent_observation_id: Option<&str>,
        outlives_turn: bool,
    ) {
        self.active.lock().insert(
            sid.to_string(),
            ActiveTurnTrace {
                trace_id: trace_id.to_string(),
                parent_observation_id: parent_observation_id.map(str::to_owned),
                outlives_turn,
            },
        );
    }

    /// turn 结束时清理；仅当 sid 仍指向该 trace 时才移除。
    pub fn clear(&self, sid: &str, trace_id: &str) {
        let mut active = self.active.lock();
        if active
            .get(sid)
            .is_some_and(|current| current.trace_id == trace_id)
        {
            active.remove(sid);
        }
    }

    /// turn 结束时清理该 trace 的登记（主 sid 与前台子 agent sid）。
    ///
    /// 仍是 compare-and-remove：只移除仍指向该 trace 的登记，后一 turn 的
    /// 登记不受影响；跨 turn 存活的后台子 agent 保留到其自身 stop。
    pub fn clear_trace(&self, trace_id: &str) {
        self.active
            .lock()
            .retain(|_, current| current.trace_id != trace_id || current.outlives_turn);
    }

    /// 取 sid 当前的活跃 turn 归属。
    pub fn resolve(&self, sid: &str) -> Option<ActiveTurnTrace> {
        self.active.lock().get(sid).cloned()
    }
}

#[cfg(test)]
#[path = "turn_traces_test.rs"]
mod tests;
