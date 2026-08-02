//! 推测深挖哨兵（SpeculationGuard）
//!
//! 检测 ReAct 循环中"无用户输入 + 连续工具调用 + 推理文本持续推测"的深挖模式：
//! agent 把运行时症状（剪贴板/权限/外部进程/并发用户操作）当成静态代码缺陷，
//! 长时间静态追查而不向用户提问（issue: 2026-08-02-agent-asks-user-too-late-in-ambiguous-env）。
//!
//! 触发条件（全部满足才提醒）：
//! - A. 自本 turn 首个用户 Prompt 以来连续无输入工具轮数 ≥ N1（默认 6）
//! - B. 当前轮无用户输入（consumed_count==0 且 has_tool_calls——由调用点保证）
//! - C. 最近 K=2 轮 thought 命中推测词，或最近 M=2 轮工具结果含错误
//! - D. 本 turn 尚未调用过 AskUserQuestion
//!
//! 推测词不改变触发阈值（Goodhart 风险：词表会被规避），只影响提醒措辞：
//! 命中推测词时措辞点明"推测"；未命中（纯工具错误）时措辞点明"连续出错"。
//!
//! 分级提醒（L1 温和 / L2 强制），注入方式与 tool_dispatch::handle_consecutive_failures
//! 完全同款：`QueuedMessage::info` push 到 v2 queue，下一轮 Receive 消费
//! （带 `<system-reminder>` 包裹写入 transcript）。
//!
//! SubAgent 排除：SubAgent 复用同一 loop 且内部深挖是正常模式。SubAgent 构建点
//! （`peri-middlewares/src/subagent/v2_bridge.rs`）与主 agent 构建点
//! （`peri-acp/src/agent/builder.rs`）都不在本 issue 允许修改范围，无法从构建端
//! 传 `with_ask_discipline(false)`；二者也无法用 agent_id 区分（都是随机 Uuid）。
//! 因此用运行时可检测信号：主 agent 的 session_context 由 builder 注入 `session_id`
//! 键，SubAgent（v2_bridge）构造空 HashMap——以该键存在作为主 agent 判定。

use std::collections::VecDeque;

use crate::messages::{BaseMessage, MessageContent};
use crate::session::queue::{MessageSource, QueuedMessage};

use super::{LoopState, StageContext};

/// 推测词列表（中英文，thought 命中任一即视为推测轮）
const SPECULATION_WORDS: [&str; 8] = [
    "可能", "也许", "大概", "或许", "probably", "maybe", "推测", "猜测",
];

/// 最近 K 轮 thought 推测词命中窗口
const SPECULATION_WINDOW: usize = 2;
/// 最近 M 轮工具错误窗口
const ERROR_WINDOW: usize = 2;
/// 连续无输入工具轮阈值 N1（推测词只影响措辞，不影响此阈值）
const DEFAULT_THRESHOLD: u32 = 6;
/// L2 强制提醒相对 N1 的偏移
const L2_OFFSET: u32 = 4;

/// 主 agent 判定键（builder.rs 注入 session_id；SubAgent v2_bridge 为空 HashMap）
const MAIN_AGENT_SESSION_KEY: &str = "session_id";

/// 哨兵是否启用：ask_discipline 开关 + 主 agent 信号
pub fn enabled(ctx: &StageContext) -> bool {
    if !ctx.ask_discipline {
        return false;
    }
    ctx.session
        .session_context
        .read()
        .contains_key(MAIN_AGENT_SESSION_KEY)
}

/// 用户新输入（Prompt）到达时重置计数。
///
/// run_react_loop 在 Receive 之后调用（仅当本轮队列中存在 Prompt——
/// Info/Defer 系统注入不重置，否则 SpeculationGuard 自己的提醒消息
/// 会把计数清零，L2 永远无法升级）。
///
/// 私有：仅 mod.rs 的 run_react_loop 调用（LoopState 为 mod.rs 私有类型）。
pub(super) fn reset(state: &mut LoopState) {
    state.speculation_rounds = 0;
    state.recent_speculation.clear();
    state.recent_errors.clear();
    state.warned_level = 0;
}

/// 观察一轮"无用户输入 + 工具调用"轮，必要时注入分级提醒。
///
/// 在 run_react_loop 的 Act 之后调用（此时可读 thought 与 has_tool_calls）。
/// `had_tool_error` 由调用点对比 Act 前后 `consecutive_failures` 得到。
///
/// 私有：仅 mod.rs 的 run_react_loop 调用（LoopState 为 mod.rs 私有类型）。
pub(super) fn observe_tool_round(
    ctx: &StageContext,
    state: &mut LoopState,
    thought: &str,
    had_tool_error: bool,
) {
    if !enabled(ctx) {
        return;
    }

    state.speculation_rounds += 1;
    push_window(
        &mut state.recent_speculation,
        SPECULATION_WINDOW,
        contains_speculation(thought),
    );
    push_window(&mut state.recent_errors, ERROR_WINDOW, had_tool_error);

    // C 条件：最近窗口内 thought 连续推测 或 工具结果连续错误
    let speculation_hit = window_all_hit(&state.recent_speculation);
    let error_hit = window_all_hit(&state.recent_errors);
    if !(speculation_hit || error_hit) {
        return;
    }

    // A 条件：连续工具轮数 ≥ N1（推测词不降阈值，仅影响措辞）
    let n1 = DEFAULT_THRESHOLD;

    // D 条件：本 turn 尚未 AskUserQuestion（由调用点维护 state.asked_user）
    if state.asked_user {
        return;
    }

    if state.warned_level < 1 && state.speculation_rounds >= n1 {
        let text = if speculation_hit {
            format!(
                "已连续 {} 轮推测深挖无新证据；若症状来自运行时环境（剪贴板/权限/外部进程），应立即 AskUserQuestion",
                state.speculation_rounds
            )
        } else {
            format!(
                "已连续 {} 轮工具调用无进展（结果出错）；应切换路径：AskUserQuestion 或换实证方法",
                state.speculation_rounds
            )
        };
        inject_reminder(ctx, text);
        state.warned_level = 1;
    } else if state.warned_level < 2 && state.speculation_rounds >= n1 + L2_OFFSET {
        let text = "应停止静态追查：AskUserQuestion 获取运行时信息，或向用户汇报现状";
        inject_reminder(ctx, text.to_string());
        state.warned_level = 2;
    }
}

/// thought 是否命中推测词
fn contains_speculation(thought: &str) -> bool {
    let lower = thought.to_lowercase();
    SPECULATION_WORDS.iter().any(|w| lower.contains(w))
}

/// 向窗口 push 一轮结果，超出容量时丢弃最旧
fn push_window(window: &mut VecDeque<bool>, capacity: usize, hit: bool) {
    window.push_back(hit);
    while window.len() > capacity {
        window.pop_front();
    }
}

/// 窗口是否已满且全部命中
fn window_all_hit(window: &VecDeque<bool>) -> bool {
    window.len() == SPECULATION_WINDOW && window.iter().all(|&b| b)
}

/// 注入分级提醒（与 handle_consecutive_failures 同款 queue push Info 模式）
fn inject_reminder(ctx: &StageContext, text: String) {
    tracing::warn!(
        rounds = ctx.session.turn.current_step(),
        "SpeculationGuard 注入提醒"
    );
    let content = format!("<system-reminder>\n{}\n</system-reminder>", text);
    ctx.session.queue.push(QueuedMessage::info(
        MessageSource::SpeculationGuard,
        BaseMessage::human(MessageContent::text(content)),
    ));
}

#[cfg(test)]
#[path = "speculation_guard_test.rs"]
mod tests;
