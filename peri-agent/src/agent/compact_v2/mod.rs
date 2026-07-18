//! Compact v2 — 标记代替删除的上下文压缩
//!
//! 三级渐进策略：
//! - **Micro**：零 LLM 调用，标 `truncated`（不改内容）
//! - **Full**：LLM 摘要 + 旧消息标 `excluded` + 追加 Human 摘要 + Re-inject
//! - **Smart**（未实现）：LLM 决策保留 id + 未选中标 `excluded` + 追加 system-reminder
//!
//! 与 v1 的区别：v2 基于 `MessageTranscript` 标记 API，不修改消息本体，
//! 旧消息标 `excluded` 后 `visible_messages()` 自动过滤。
//! Full Compact 通过 `BaseModel::invoke` 标准链路请求摘要。
//! 所有注入消息使用 `BaseMessage::human()` —— 禁止 System，防止 hoist 污染 FrozenContext。

use tracing::{debug, warn};

use crate::agent::events::CompactStrategy;
use crate::llm::BaseModel;
use crate::session::transcript::MessageTranscript;

pub mod config;
pub mod full;
pub mod micro;
pub mod smart;

// ─── 公共重导出：保持外部调用路径不变 ─────────────────────────────────────────────

pub use config::{CompactConfig, CONTINUATION_HINT};
pub use full::{extract_file_info, extract_skill_names, re_inject_v2, ReInjectResult};
pub use micro::micro_compact;

// ─── CompactResult ───────────────────────────────────────────────────────────────

/// Compact 执行结果
#[derive(Debug, Clone)]
pub struct CompactResult {
    /// 使用的策略
    pub strategy: CompactStrategy,
    /// 操作的消息数量（标 truncated / excluded 的数量）
    pub affected_count: usize,
    /// 操作前消息总数
    pub before_len: usize,
    /// 操作后可见消息数量
    pub after_visible_len: usize,
    /// Full Compact 生成的摘要（Micro 时为 None）
    pub summary: Option<String>,
}

// ─── 顶层入口 ───────────────────────────────────────────────────────────────────

/// 根据 ContextBudget 百分比和配置确定 Compact 策略。
///
/// 返回 `None` 表示"不需要 compact"（预算充足，跳过）。
/// 返回 `Some(strategy)` 表示应执行的策略（Micro 或 Full）。
///
/// P1-5: stages/compact 和 compact_v2::run_compact 统一使用此函数，
/// 消除两处策略判断的重复逻辑。
pub fn determine_compact_strategy(
    budget: f64,
    config: &CompactConfig,
    force: bool,
) -> Option<CompactStrategy> {
    if force || budget >= config.auto_compact_threshold {
        Some(CompactStrategy::Full)
    } else if budget >= config.micro_compact_threshold {
        Some(CompactStrategy::Micro)
    } else {
        None
    }
}

/// 根据 ContextBudget 百分比选择策略并执行 Compact
///
/// - budget < 0.70：跳过
/// - 0.70 <= budget < 0.85：Micro
/// - budget >= 0.85 或 force=true：Full
/// - 连续失败超过 max_consecutive_failures 次时降级跳过
pub async fn run_compact(
    transcript: &mut MessageTranscript,
    llm: Option<&dyn BaseModel>,
    config: &CompactConfig,
    budget: f64,
    force: bool,
    consecutive_failures: &mut u32,
    cwd: &str,
) -> CompactResult {
    let before_len = transcript.len();

    // 防死循环：连续失败超限则跳过
    if *consecutive_failures >= config.max_consecutive_failures {
        debug!(consecutive_failures, "Compact 降级：连续失败超限，跳过本轮");
        return CompactResult {
            strategy: CompactStrategy::Micro,
            affected_count: 0,
            before_len,
            after_visible_len: transcript.visible_messages().len(),
            summary: None,
        };
    }

    let strategy = match determine_compact_strategy(budget, config, force) {
        Some(s) => s,
        None => {
            // 预算充足，跳过
            return CompactResult {
                strategy: CompactStrategy::Micro,
                affected_count: 0,
                before_len,
                after_visible_len: transcript.visible_messages().len(),
                summary: None,
            };
        }
    };

    match strategy {
        CompactStrategy::Micro => {
            let affected = micro::micro_compact(transcript, config);
            *consecutive_failures = 0;
            CompactResult {
                strategy: CompactStrategy::Micro,
                affected_count: affected,
                before_len,
                after_visible_len: transcript.visible_messages().len(),
                summary: None,
            }
        }
        CompactStrategy::Full => {
            // 重跑保护：上轮 Full Compact 失败时设置的 excluded 标记若残留，
            // 会污染 visible_messages() 导致本轮 compact 错误地认为已压缩。
            // 仅清 excluded（保留 truncated——属 Micro Compact 状态，不可误清）。
            if *consecutive_failures > 0 {
                let stale_ids: Vec<_> = transcript
                    .entries()
                    .iter()
                    .filter(|e| transcript.flags(e.message.id()).excluded)
                    .map(|e| e.message.id())
                    .collect();
                let cleared = stale_ids.len();
                for id in stale_ids {
                    transcript.set_excluded(id, false);
                }
                if cleared > 0 {
                    debug!(cleared, "Full Compact 重跑：清除上轮残留 excluded 标记");
                }
            }
            match full::full_compact_inner(transcript, llm, config, cwd).await {
                Ok(result) => {
                    *consecutive_failures = 0;
                    result
                }
                Err(e) => {
                    *consecutive_failures = consecutive_failures.saturating_add(1);
                    warn!(
                        error = %e,
                        consecutive_failures,
                        "Full Compact 失败，降级跳过"
                    );
                    CompactResult {
                        strategy: CompactStrategy::Full,
                        affected_count: 0,
                        before_len,
                        after_visible_len: transcript.visible_messages().len(),
                        summary: None,
                    }
                }
            }
        }
        CompactStrategy::Smart => {
            // 未实现，降级为 Micro
            let affected = micro::micro_compact(transcript, config);
            CompactResult {
                strategy: CompactStrategy::Micro,
                affected_count: affected,
                before_len,
                after_visible_len: transcript.visible_messages().len(),
                summary: None,
            }
        }
    }
}

#[cfg(test)]
mod _test;
