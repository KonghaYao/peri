//! Compact v2 — 标记代替删除的上下文压缩
//!
//! 触发流程：
//! - budget < 0.75：跳过
//! - budget ≥ 0.75：Micro Compact 或 Smart Compact（根据配置选择）
//!   - Micro：按 round 分组，旧轮次的工具消息标 truncated
//!   - Smart：保留最近 N 条 Human/Ai + 最近 M 个 Tool 结果 + 所有错误消息，其余标 truncated
//!   - affected_count ≥ micro_min_affected → 有效，budget ≥ 0.95 时叠加 Full
//!   - affected_count < micro_min_affected → 无效，升级为 Full
//! - force=true：直接 Full（跳过 Micro/Smart）
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
    /// Full Compact 生成的摘要（Micro/Smart 时为 None）
    pub summary: Option<String>,
}

// ─── 顶层入口 ───────────────────────────────────────────────────────────────────

/// Compact 阶段动作
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactAction {
    /// 跳过 compact（预算充足）
    Skip,
    /// 执行 Micro Compact
    Micro,
    /// 执行 Smart Compact（规则驱动，保留关键消息）
    Smart,
}

/// 根据 budget 和配置决定 Compact 动作。
///
/// 返回 `Skip` 表示预算未到 75%，跳过 compact。
/// 返回 `Smart` 表示启用 Smart Compact 且预算 ≥ 75%。
/// 返回 `Micro` 表示未启用 Smart Compact 且预算 ≥ 75%。
///
/// Full Compact 的触发不在本函数内判定——由 run_compact 在执行后
/// 根据 affected_count 和 budget 动态决策。
pub fn determine_compact_action(budget: f64, config: &CompactConfig) -> CompactAction {
    if budget >= config.micro_compact_threshold {
        if config.smart_compact_enabled {
            CompactAction::Smart
        } else {
            CompactAction::Micro
        }
    } else {
        CompactAction::Skip
    }
}

/// 根据 ContextBudget 百分比选择策略并执行 Compact
///
/// 触发流程：
/// - budget < 0.75：跳过
/// - budget ≥ 0.75：
///   - smart_compact_enabled=true → Smart Compact（规则驱动保留关键消息）
///   - smart_compact_enabled=false → Micro Compact（按 round 分组截断）
///   - affected_count ≥ micro_min_affected → 有效，budget ≥ 0.95 时叠加 Full
///   - affected_count < micro_min_affected → 无效，升级为 Full
/// - force=true：直接 Full（跳过 Micro/Smart）
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

    // 手动触发 → 直接 Full
    if force {
        return run_full_or_degrade(
            transcript,
            llm,
            config,
            before_len,
            consecutive_failures,
            cwd,
        )
        .await;
    }

    // 检查 Compact 触发条件
    match determine_compact_action(budget, config) {
        CompactAction::Skip => CompactResult {
            strategy: CompactStrategy::Micro,
            affected_count: 0,
            before_len,
            after_visible_len: transcript.visible_messages().len(),
            summary: None,
        },
        CompactAction::Micro => {
            // 执行 Micro
            let affected = micro::micro_compact(transcript, config);
            *consecutive_failures = 0;

            if affected >= config.micro_min_affected {
                // Micro 有效
                if budget >= config.auto_compact_threshold {
                    debug!(affected, budget, "Micro 有效 + budget 高位 → 叠加 Full");
                    run_full_or_degrade(
                        transcript,
                        llm,
                        config,
                        before_len,
                        consecutive_failures,
                        cwd,
                    )
                    .await
                } else {
                    CompactResult {
                        strategy: CompactStrategy::Micro,
                        affected_count: affected,
                        before_len,
                        after_visible_len: transcript.visible_messages().len(),
                        summary: None,
                    }
                }
            } else {
                debug!(affected, budget, "Micro 无效 → 升级为 Full");
                run_full_or_degrade(
                    transcript,
                    llm,
                    config,
                    before_len,
                    consecutive_failures,
                    cwd,
                )
                .await
            }
        }
        CompactAction::Smart => {
            // 执行 Smart Compact（规则驱动，不调用 LLM）
            let affected = smart::smart_compact(transcript, config);
            *consecutive_failures = 0;

            if affected >= config.micro_min_affected {
                // Smart 有效
                if budget >= config.auto_compact_threshold {
                    debug!(affected, budget, "Smart 有效 + budget 高位 → 叠加 Full");
                    run_full_or_degrade(
                        transcript,
                        llm,
                        config,
                        before_len,
                        consecutive_failures,
                        cwd,
                    )
                    .await
                } else {
                    CompactResult {
                        strategy: CompactStrategy::Smart,
                        affected_count: affected,
                        before_len,
                        after_visible_len: transcript.visible_messages().len(),
                        summary: None,
                    }
                }
            } else {
                // Smart 无效 → 升级为 Full
                debug!(affected, budget, "Smart 无效 → 升级为 Full");
                run_full_or_degrade(
                    transcript,
                    llm,
                    config,
                    before_len,
                    consecutive_failures,
                    cwd,
                )
                .await
            }
        }
    }
}

/// 运行 Full Compact（含失败降级逻辑）
async fn run_full_or_degrade(
    transcript: &mut MessageTranscript,
    llm: Option<&dyn BaseModel>,
    config: &CompactConfig,
    before_len: usize,
    consecutive_failures: &mut u32,
    cwd: &str,
) -> CompactResult {
    // 重跑保护：清除上轮残留 excluded 标记
    if *consecutive_failures > 0 {
        let stale_ids: Vec<_> = transcript
            .entries()
            .iter()
            .filter(|e| transcript.flags(e.message.id()).excluded)
            .map(|e| e.message.id())
            .collect();
        for id in &stale_ids {
            transcript.set_excluded(*id, false);
        }
        if !stale_ids.is_empty() {
            debug!(count = stale_ids.len(), "清除上轮残留 excluded 标记");
        }
    }

    match full::full_compact_inner(transcript, llm, config, cwd).await {
        Ok(result) => {
            *consecutive_failures = 0;
            result
        }
        Err(e) => {
            warn!(error = %e, "Full Compact 失败");
            *consecutive_failures += 1;
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

#[cfg(test)]
mod _test;

#[cfg(test)]
#[path = "trigger_test.rs"]
mod trigger_test;
