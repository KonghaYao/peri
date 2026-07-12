//! TurnContext — 一次 "用户输入 → Agent 处理 → 回答" 的上下文
//!
//! 一次 turn 是从用户提交 prompt 开始，到 Agent 给出最终回答结束的完整过程。
//! 一个 turn 内可能包含多个 ReAct step（多轮工具调用）。所有事件携带同一 `turn_id`，
//! 作为 LLM 调用 → 工具执行全程可追踪的纽带。
//!
//! Turn 开始时创建，turn 结束即销毁。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

// ─── TurnId ──────────────────────────────────────────────────────────────────

/// Turn 唯一标识符 — UUID v7（时间有序）
///
/// 作为一次 turn 内所有事件的统一纽带。从 LlmCallStart 到 TurnCompleted 全程一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TurnId(uuid::Uuid);

impl TurnId {
    pub fn new() -> Self {
        Self(uuid::Uuid::now_v7())
    }

    pub fn as_uuid(&self) -> uuid::Uuid {
        self.0
    }
}

impl Default for TurnId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TurnId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ─── TurnContext ─────────────────────────────────────────────────────────────

/// 一次 turn 的上下文
///
/// 在 turn 开始（用户提交 prompt 触发 ReAct 循环）时创建，turn 结束（emit TurnCompleted）时销毁。
/// 一个 turn 内的多个 ReAct step 共享同一 TurnContext。
///
/// 字段说明：
/// - `turn_id`：事件流纽带，所有事件携带
/// - `step`：ReAct step 计数（turn 内的循环迭代次数）
/// - `cwd`：工作目录（只读引用）
/// - `cancel_token`：与 SessionConfig 共享的 cancel token，支持用户中断
/// - `started_at`：turn 开始时间，用于耗时统计
#[derive(Debug)]
pub struct TurnContext {
    /// Turn 唯一 ID（事件流纽带）
    pub turn_id: TurnId,
    /// 当前 ReAct step（turn 内的循环迭代次数，AtomicUsize 支持 &self 自增）
    step: AtomicUsize,
    /// 工作目录（只读）
    pub cwd: Arc<str>,
    /// Cancel token（与 SessionConfig 共享）
    pub cancel_token: Arc<CancellationToken>,
    /// Turn 开始时间（用于耗时统计）
    pub started_at: Instant,
}

impl TurnContext {
    /// 创建新 TurnContext — turn 开始时调用
    pub fn new(cwd: Arc<str>, cancel_token: Arc<CancellationToken>) -> Self {
        Self {
            turn_id: TurnId::new(),
            step: AtomicUsize::new(0),
            cwd,
            cancel_token,
            started_at: Instant::now(),
        }
    }

    /// 当前 step（从 0 开始）
    pub fn current_step(&self) -> usize {
        self.step.load(Ordering::Relaxed)
    }

    /// 推进 step，返回推进后的值
    pub fn advance_step(&self) -> usize {
        self.step.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// 重置 step（一般不需要，仅测试或 rewind 用）
    pub fn reset_step(&self) {
        self.step.store(0, Ordering::Relaxed);
    }

    /// 是否已取消
    pub fn is_cancelled(&self) -> bool {
        self.cancel_token.is_cancelled()
    }

    /// Turn 已耗时（秒）
    pub fn elapsed_secs(&self) -> f64 {
        self.started_at.elapsed().as_secs_f64()
    }

    /// 创建子 token（用于 turn 内的子任务取消，如单次工具调用）
    pub fn child_token(&self) -> CancellationToken {
        self.cancel_token.child_token()
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "turn_test.rs"]
mod tests;
