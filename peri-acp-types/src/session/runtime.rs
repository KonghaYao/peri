//! Agent runtime 注册表条目与 Cascade/Independent 取消判定。

use crate::thread::{CancelPolicy, ThreadId};
use std::collections::HashMap;

// ─── AgentRuntime（注册表条目 + cancel 判定） ────────────────────────────────

/// 运行时 agent 实例（子 agent 取消判定与终止执行的载体）。
///
/// cancel 最终执行权归 Agent 层（§2/§9）：本类型是跨层注册表条目契约
/// （ACP `AcpSession.active_agents` 持有），判定函数为纯函数、无层依赖。
pub struct AgentRuntime {
    pub thread_id: ThreadId,
    pub cancel_token: tokio_util::sync::CancellationToken,
    pub cancel_policy: CancelPolicy,
    pub status: crate::thread::AgentStatus,
}

impl AgentRuntime {
    pub fn new(thread_id: ThreadId, cancel_policy: CancelPolicy) -> Self {
        Self {
            thread_id,
            cancel_token: tokio_util::sync::CancellationToken::new(),
            cancel_policy,
            status: crate::thread::AgentStatus::Active,
        }
    }
}

/// cancel 判定（Cascade/Independent）与终止执行：取消所有 Cascade policy 的
/// 同步子 agent（跟随父 agent 取消）。Independent（bg）子 agent 不受影响，
/// 仅跟随 session 根取消。
pub fn cancel_cascade_agents<'a>(runtimes: impl IntoIterator<Item = &'a AgentRuntime>) {
    let _ = cancel_cascade_agents_collecting(runtimes);
}

/// 同 [`cancel_cascade_agents`]，并返回实际被取消的 thread 身份。
///
/// 调用方用它给这些对象落显式停止意图：取消范围与停止范围必须来自同一次判定，
/// 不能各自再筛一遍。
pub fn cancel_cascade_agents_collecting<'a>(
    runtimes: impl IntoIterator<Item = &'a AgentRuntime>,
) -> Vec<ThreadId> {
    let mut cancelled = Vec::new();
    for runtime in runtimes {
        if runtime.cancel_policy == CancelPolicy::Cascade {
            runtime.cancel_token.cancel();
            cancelled.push(runtime.thread_id.clone());
        }
    }
    cancelled
}

/// 取消所有 agent（session 结束 / close_session 时）。
pub fn cancel_all_agents<'a>(runtimes: impl IntoIterator<Item = &'a AgentRuntime>) {
    for runtime in runtimes {
        runtime.cancel_token.cancel();
    }
}

/// 便捷入口：按 `thread_id -> AgentRuntime` 注册表执行 cascade 判定。
pub fn cancel_cascade_in<'a>(
    runtimes: impl IntoIterator<Item = &'a HashMap<ThreadId, AgentRuntime>>,
) {
    for map in runtimes {
        cancel_cascade_agents(map.values());
    }
}

/// 便捷入口：按 `thread_id -> AgentRuntime` 注册表取消全部。
pub fn cancel_all_in<'a>(runtimes: impl IntoIterator<Item = &'a HashMap<ThreadId, AgentRuntime>>) {
    for map in runtimes {
        cancel_all_agents(map.values());
    }
}
