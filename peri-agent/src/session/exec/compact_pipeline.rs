//! Compact Pipeline — `/compact` 命令路径的 v2 实现（L5：自
//! peri-acp/src/host/exec/compact_pipeline.rs 迁入）。
//!
//! 本模块承载深绑 Agent 层执行类型（`MessageTranscript` /
//! `compact_v2::run_compact`）的 `/compact` 命令执行体；ACP 协议面
//! `session::command::compact::pipeline` 保留 re-export 薄壳。
//!
//! [v2] 从 v1 压缩路径迁移到 `compact_v2::run_compact(force=true)`：
//! - 把 history 加载进临时 `MessageTranscript`
//! - 调用 `run_compact` 触发 Full Compact
//! - 从 transcript 拿 compact 后的 visible_messages 组装事件载荷
//!
//! 编排层（`compact.rs::execute`）只做组合。
//!
//! 阶段顺序：
//!   validate_inputs → resolve_auxiliary_model → (emit_started)
//!   → run_v2_compact_with_cancel → assemble_compact_messages
//!   → (emit_completed)
//!
//! [TRAP] cancel_token.cancelled() 分支返回 PromptStopReason::Cancelled；错误/空历史/
//! 无模型当前都返回 EndTurn。executor.rs 上游对 Cancelled 有专门处理
//! （spec/global/domains/agent.md#issue_2026-05-29-ctrl-c-interrupt-causes-agent-amnesia）。
//!
//! 依赖反转（L5）：compact 配置不直接读取 ACP `PeriConfig`——`CommandContext`
//! 以 `compact_config` 投影（ACP 装配点按 `load_compact_config` 语义预填，
//! env overrides 每轮重新应用，语义保持）。

use std::collections::HashSet;
use std::sync::Arc;

use peri_acp_types::command::{
    CommandContext, CommandFeedback, CommandResult, FeedbackChannel, FeedbackLevel,
    PromptStopReason,
};
use peri_acp_types::compact::CompactConfig;
use peri_acp_types::event::CompactTrigger;
use peri_acp_types::messages::{BaseMessage, MessageId};
use peri_acp_types::session_resources::DataCapabilities;
use peri_acp_types::store::PersistedPayload;
use tokio_util::sync::CancellationToken as AgentCancellationToken;
use tracing::{info, warn};

use crate::agent::compact_v2;
use crate::session::transcript::{CompactionCommitState, MessageTranscript};

use super::events::{emit_compact_completed, emit_compact_started};

/// Pipeline 终态。编排层据此决定返回值与是否中途 short-circuit。
pub enum PipelineOutcome {
    /// 正常完成：组装后的消息（首条 Human 为摘要）。
    Completed {
        messages: Vec<BaseMessage>,
        /// v2 compact 操作计数（feedback 文案「已压缩 N 条消息」的 N）。
        affected_count: usize,
    },
    /// 取消返回候选 history；interceptor 按共享提交状态恢复已提交结果，
    /// 或在结果不确定时使热会话失效。未提交时 stop_reason = Cancelled。
    Cancelled { history: Vec<BaseMessage> },
    /// 边界情况（空历史 / 无模型 / compact 失败）：保留原 history，
    /// stop_reason = EndTurn，失败文案经 message 结构化返回（Phase 5 Step 4：
    /// 不再直接发射 CompactError 事件，由 execute_compact 最外层统一映射
    /// feedback(Error, UiOnly)）。
    EarlyReturn {
        history: Vec<BaseMessage>,
        stop_reason: PromptStopReason,
        message: String,
    },
}

/// 运行 v2 compact 的完整 Pipeline。
///
/// 调用方（`compact.rs::execute`）负责在调用前完成空 history 短路。
/// 此函数内部发出 CompactStarted / CompactCompleted 事件（Phase 5 Step 4：
/// CompactError 变体与发射已删除——错误不再经事件通道，改由
/// [`PipelineOutcome::EarlyReturn`] 结构化返回，由 execute_compact 最外层
/// 统一映射 feedback(Error, UiOnly)）。
pub async fn run_pipeline(ctx: CommandContext) -> PipelineOutcome {
    // The interceptor owns a clone beyond both cancellation select boundaries.
    let commit_state = ctx
        .dep::<CompactionCommitState>()
        .map(|state| (*state).clone())
        .unwrap_or_default();
    let CommandContext {
        session_id,
        history,
        cwd,
        compact_config,
        auxiliary_model,
        event_sink,
        cancel_token,
        session_resources,
        thread_id,
        ..
    } = ctx;

    tracing::debug!(history_len = history.len(), "compact: pipeline started");

    // 无普通消息不等于无上下文：绑定会话可能只有 canonical reminder。
    if history.is_empty() && (session_resources.is_none() || thread_id.is_none()) {
        warn!("compact: 无历史消息可压缩");
        return PipelineOutcome::EarlyReturn {
            history,
            stop_reason: PromptStopReason::EndTurn,
            message: "no history to compact".to_string(),
        };
    }

    // 阶段 3: 解析 auxiliary model
    let auxiliary_model: Arc<dyn peri_model::Model> = match auxiliary_model {
        Some(m) => m,
        None => {
            warn!("compact: 无可用模型");
            return PipelineOutcome::EarlyReturn {
                history,
                stop_reason: PromptStopReason::EndTurn,
                message: "no model available for compact".to_string(),
            };
        }
    };

    // 阶段 4: 手动 compact 必须绑定会话资源门面，保证 Full lifecycle 可原子提交。
    let (session_resources, thread_id) = match (session_resources, thread_id) {
        (Some(store), Some(thread_id)) => (store, thread_id),
        _ => {
            warn!("compact: persistence is unavailable");
            return PipelineOutcome::EarlyReturn {
                history,
                stop_reason: PromptStopReason::EndTurn,
                message: "compact persistence is unavailable".to_string(),
            };
        }
    };

    // 能力判定读领域能力面（`Complete` = 全部行为满足完整后置条件），不探测实现细节。
    match session_resources
        .inspect_availability(Some(&thread_id))
        .await
    {
        Ok(availability) if availability.capabilities == DataCapabilities::Complete => {}
        Ok(_) => {
            warn!("compact: persistence backend does not support complete behavior set");
            return PipelineOutcome::EarlyReturn {
                history,
                stop_reason: PromptStopReason::EndTurn,
                message: "compact lifecycle persistence is unavailable".to_string(),
            };
        }
        Err(error) => {
            warn!(%error, "compact: session availability check failed");
            return PipelineOutcome::EarlyReturn {
                history,
                stop_reason: PromptStopReason::EndTurn,
                message: format!("compact persistence failed: {error:#}"),
            };
        }
    }

    // 阶段 5: 已存在的 thread 从一次一致**有效**快照重建。先核对消息身份与顺序，
    // 再恢复 flags 供 Full 选择可见内容；不能拿 visible view 与 canonical 输入比较。
    //
    // 读取范围只含未排除的 own 条目与冻结继承区：已归档正文既不进入摘要输入，也不
    // 在本次重建中解码。归档规模因此不影响手动 compact 的正文读取量（REQ-F03）。
    let snapshot = match session_resources.load_active_snapshot(&thread_id).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            warn!(error = %format_args!("{error:#}"), "compact: failed to load persisted history");
            return PipelineOutcome::EarlyReturn {
                history,
                stop_reason: PromptStopReason::EndTurn,
                message: format!("compact persistence failed: {error:#}"),
            };
        }
    };
    let mut transcript = MessageTranscript::new().with_compaction_commit_state(commit_state);
    if snapshot.payloads.is_empty() && snapshot.inherited.payloads.is_empty() {
        // 有效集合为空有两种来源：全新 thread（历史尚未落库），或全部 own 条目都已
        // 被之前的 Full 归档。后者没有可压缩内容；用有界 flags 查询区分，不读取
        // 任何归档正文。
        let excluded_only = if history.is_empty() {
            false
        } else {
            let ids: Vec<MessageId> = history.iter().map(BaseMessage::id).collect();
            match session_resources.load_message_flags(&thread_id, &ids).await {
                Ok(flags) => {
                    !ids.is_empty()
                        && ids
                            .iter()
                            .all(|id| flags.get(id).is_some_and(|entry| entry.excluded))
                }
                Err(error) => {
                    warn!(error = %format_args!("{error:#}"), "compact: failed to load message flags");
                    return PipelineOutcome::EarlyReturn {
                        history,
                        stop_reason: PromptStopReason::EndTurn,
                        message: format!("compact persistence failed: {error:#}"),
                    };
                }
            }
        };
        if history.is_empty() || excluded_only {
            return PipelineOutcome::EarlyReturn {
                history,
                stop_reason: PromptStopReason::EndTurn,
                message: "no history to compact".to_string(),
            };
        }
        transcript = transcript.with_persistence(session_resources, thread_id);
        for message in &history {
            transcript.append(message.clone());
        }
        if let Err(error) = transcript.flush_persistence().await {
            warn!(error = %format_args!("{error:#}"), "compact: failed to persist initial history");
            return PipelineOutcome::EarlyReturn {
                history,
                stop_reason: PromptStopReason::EndTurn,
                message: format!("compact persistence failed: {error:#}"),
            };
        }
    } else {
        // 调用方身份只按**有效集合**校验（spec D3）：成功 Full 提交会释放热态里的
        // excluded 原文（D1 transcript shrink），宿主下一轮带回的历史天然只剩有效
        // 条目。两侧按 inherited 与 own 的有序身份序列比较。
        //
        // 兼容适配入口：旧调用方仍可能带回完整历史（含已归档 id）。多出的 id 必须
        // 证明确为 excluded（own 走有界 flags 查询，继承区用同一快照的 flags）；
        // 未知 id 与读取失败都保持拒绝，不读取这些 id 的正文。
        let inherited_ids: HashSet<MessageId> = snapshot
            .inherited
            .payloads
            .iter()
            .map(PersistedPayload::id)
            .collect();
        let own_ids: HashSet<MessageId> =
            snapshot.payloads.iter().map(PersistedPayload::id).collect();
        let overlap = inherited_ids.intersection(&own_ids).next().copied();
        if let Some(id) = overlap {
            warn!(?id, "compact: inherited and own history overlap");
            return PipelineOutcome::EarlyReturn {
                history,
                stop_reason: PromptStopReason::EndTurn,
                message: "compact persistence context mismatch".to_string(),
            };
        }
        // 继承区按冻结快照原样返回：它同时带着 excluded 原文与标记，而 own 区已由
        // `load_active_snapshot` 裁掉 excluded。普通 turn 交给调用方的是「继承区 +
        // own」同一份**有效投影**，因此持久侧身份也必须先按继承区的 excluded flags
        // 裁剪。否则子会话续聊后，合法的继承上下文会被误判为不一致。
        let inherited_excluded_ids: HashSet<MessageId> = snapshot
            .inherited
            .flags
            .iter()
            .filter(|(_, flags)| flags.excluded)
            .map(|(id, _)| *id)
            .collect();
        let excluded_ids: HashSet<MessageId> = inherited_excluded_ids
            .iter()
            .copied()
            .chain(
                snapshot
                    .flags
                    .iter()
                    .filter(|(_, flags)| flags.excluded)
                    .map(|(id, _)| *id),
            )
            .collect();
        // 命令 history 是普通消息投影；提醒类 payload 不携带 `BaseMessage`，不能参与
        // 身份比较，否则会把提醒数量误当成上下文差异。身份只来自消息投影。
        let persisted_messages: Vec<&BaseMessage> = snapshot
            .inherited
            .payloads
            .iter()
            .chain(&snapshot.payloads)
            .filter(|payload| !excluded_ids.contains(&payload.id()))
            .filter_map(PersistedPayload::as_message)
            .collect();
        let persisted_ids: Vec<MessageId> = persisted_messages
            .iter()
            .map(|message| message.id())
            .collect();
        let active_ids: HashSet<MessageId> = persisted_ids.iter().copied().collect();
        let mut command_history: Vec<&BaseMessage> = history.iter().collect();
        let command_matches = |history: &[&BaseMessage]| {
            history.len() == persisted_ids.len()
                && history
                    .iter()
                    .zip(&persisted_ids)
                    .all(|(incoming, persisted)| incoming.id() == *persisted)
        };
        if !command_matches(&command_history) {
            let extra_ids: Vec<MessageId> = history
                .iter()
                .map(BaseMessage::id)
                .filter(|id| !active_ids.contains(id))
                .collect();
            let known_excluded = if extra_ids.is_empty() {
                false
            } else {
                match session_resources
                    .load_message_flags(&thread_id, &extra_ids)
                    .await
                {
                    // 继承区的 excluded 事实已在同一份快照里，不再按 id 读一次；
                    // 未知 id 仍然拒绝，不放宽为「长度不同就放行」。
                    Ok(flags) => extra_ids.iter().all(|id| {
                        inherited_excluded_ids.contains(id)
                            || flags.get(id).is_some_and(|entry| entry.excluded)
                    }),
                    Err(error) => {
                        warn!(error = %format_args!("{error:#}"), "compact: failed to load message flags");
                        return PipelineOutcome::EarlyReturn {
                            history,
                            stop_reason: PromptStopReason::EndTurn,
                            message: format!("compact persistence failed: {error:#}"),
                        };
                    }
                }
            };
            if !known_excluded {
                warn!("compact: persisted active history does not match command history");
                return PipelineOutcome::EarlyReturn {
                    history,
                    stop_reason: PromptStopReason::EndTurn,
                    message: "compact persistence context mismatch".to_string(),
                };
            }
            command_history.retain(|message| active_ids.contains(&message.id()));
            if !command_matches(&command_history) {
                warn!("compact: persisted active history does not match command history");
                return PipelineOutcome::EarlyReturn {
                    history,
                    stop_reason: PromptStopReason::EndTurn,
                    message: "compact persistence context mismatch".to_string(),
                };
            }
        }
        // 校验通过后从**有效快照**重建。excluded 原文留在存储中，Full 只对当前
        // 有效条目落 flags；本 transcript 不再承载归档正文。
        transcript = transcript
            .with_ancestor_payloads(snapshot.inherited.payloads)
            .with_own_payloads(snapshot.payloads);
        transcript.set_flags_batch(snapshot.inherited.flags);
        transcript.set_flags_batch(snapshot.flags);
        transcript = transcript.with_persistence(session_resources, thread_id);
    }

    // 阶段 6: 发出 CompactStarted 事件
    emit_compact_started(&event_sink, &session_id).await;

    let mut consecutive_failures = 0u32;
    let compact_result = match run_v2_compact_with_cancel(
        &mut transcript,
        auxiliary_model.as_ref(),
        &compact_config,
        &cwd,
        &cancel_token,
        &session_id,
        &mut consecutive_failures,
    )
    .await
    {
        Ok(r) => r,
        Err(CancelOrError::Cancelled) => {
            return PipelineOutcome::Cancelled { history };
        }
        Err(CancelOrError::Error(message)) => {
            return PipelineOutcome::EarlyReturn {
                history,
                stop_reason: PromptStopReason::EndTurn,
                message,
            };
        }
    };

    info!(
        summary_len = compact_result.summary.as_deref().map(str::len).unwrap_or(0),
        strategy = ?compact_result.strategy,
        "compact: v2 run_compact 完成"
    );

    // 阶段 6: 组装最终消息（从 transcript visible_messages 取）
    let assembled = assemble_compact_messages(&transcript, &compact_result.summary.clone());

    // 阶段 7: 发出 CompactCompleted 事件（重建信号 + 展示/观测计数）。
    emit_compact_completed(
        &event_sink,
        &session_id,
        compact_result.summary.clone().unwrap_or_default(),
        assembled.messages.clone(),
        CompactTrigger::Manual,
        compact_result.strategy,
        compact_result.affected_count,
        compact_result.estimated_tokens_saved,
        assembled.files.clone(),
        assembled.skills.clone(),
    )
    .await;

    info!("compact: 完成，session 已更新");

    PipelineOutcome::Completed {
        messages: assembled.messages,
        affected_count: compact_result.affected_count,
    }
}

/// v2 run_compact + 取消语义的执行结果。
///
/// Phase 5 Step 4：失败路径不再发射 CompactError 事件（由 execute_compact
/// 最外层统一映射 feedback(Error, UiOnly)），仅以结构化错误返回。
enum CancelOrError {
    Cancelled,
    Error(String),
}

/// 执行 v2 run_compact 并封装取消/错误路径。
#[allow(clippy::too_many_arguments)]
async fn run_v2_compact_with_cancel(
    transcript: &mut MessageTranscript,
    model: &dyn peri_model::Model,
    config: &CompactConfig,
    cwd: &str,
    cancel_token: &AgentCancellationToken,
    session_id: &str,
    consecutive_failures: &mut u32,
) -> Result<compact_v2::CompactResult, CancelOrError> {
    let result = tokio::select! {
        biased;
        _ = cancel_token.cancelled() => {
            tracing::info!(session_id = %session_id, "compact cancelled by user");
            return Err(CancelOrError::Cancelled);
        }
        r = compact_v2::run_compact(
            transcript,
            Some(model),
            config,
            &compact_v2::ContextPressure {
                estimated_tokens: 0,
                context_window: u32::MAX,
                output_reserve: 0,
                predicted_tool_growth: 0,
                safety_buffer: 0,
                cache_hit_rate: 0.0,
            }, // force=true 时直接走 Full 路径，pressure 可填占位值
            true,
            consecutive_failures,
            cwd,
            None,
        ) => r,
    };

    // 模型完成与取消可在同一次 poll 内同时发生；返回反馈前仍以取消为先。
    if cancel_token.is_cancelled()
        || matches!(result.failure, Some(crate::error::AgentError::Interrupted))
    {
        return Err(CancelOrError::Cancelled);
    }
    if let Some(error) = &result.failure {
        let message = match error {
            // force 路径没有实际压力样本，不能把占位预算展示为用户用量。
            crate::error::AgentError::CompactRetriesExhausted { attempts, .. } => format!(
                "Full Compact failed after {attempts} attempts. Retry or change the compact model."
            ),
            error => error.user_facing_message(),
        };
        return Err(CancelOrError::Error(message));
    }

    // 无失败原因的 no-op 不伪装成成功压缩。
    if result.affected_count == 0 && result.summary.is_none() {
        warn!(strategy = ?result.strategy, "compact: v2 run_compact 无效果");
        return Err(CancelOrError::Error(
            "compact produced no effect".to_string(),
        ));
    }

    Ok(result)
}

/// 组装最终消息：从 transcript visible_messages 提取摘要及保留的可见消息。
///
/// [TRAP] compact 后消息结构必须以 `BaseMessage::human(summary + continuation)` 开头。
/// 但 v2 的 run_compact 已经在 transcript 内部追加了符合不变量的消息，
/// 此处直接读 visible_messages 即可，无需重新构造首条消息。
pub fn assemble_compact_messages(
    transcript: &MessageTranscript,
    _summary: &Option<String>,
) -> AssembledMessages {
    let mut messages: Vec<BaseMessage> = transcript
        .visible_messages()
        .into_iter()
        .filter(|message| !matches!(message, BaseMessage::System { .. }))
        .cloned()
        .collect();
    // Full Compact 保留 System 在 transcript 中，但命令重建 payload 不包含它们，
    // 且必须以 continuation summary 的 Human 消息开头。
    if let Some(summary_index) = messages.iter().position(|message| {
        matches!(message, BaseMessage::Human { .. })
            && message.content().contains(compact_v2::CONTINUATION_HINT)
    }) {
        let summary = messages.remove(summary_index);
        messages.insert(0, summary);
    }

    let files = compact_v2::extract_file_info(&messages);
    let skills = compact_v2::extract_skill_names(&messages);

    AssembledMessages {
        messages,
        files,
        skills,
    }
}

/// assemble 阶段产物。
pub struct AssembledMessages {
    pub messages: Vec<BaseMessage>,
    pub files: Vec<peri_acp_types::event::CompactFileInfo>,
    pub skills: Vec<String>,
}

/// `/compact` 命令入口：执行完整 Pipeline 并映射终态到 `CommandResult`。
///
/// Phase 5 Step 4：错误/成功反馈统一经 `feedback` 字段返回（编排层
/// emit_command_feedback 发射），本函数内零事件代码（CompactStarted /
/// CompactCompleted 阶段信号除外）。
pub async fn execute_compact(ctx: CommandContext) -> CommandResult {
    match run_pipeline(ctx).await {
        PipelineOutcome::Completed {
            messages,
            affected_count,
        } => CommandResult {
            messages,
            stop_reason: PromptStopReason::EndTurn,
            feedback: Some(CommandFeedback {
                level: FeedbackLevel::Info,
                message: format!("已压缩 {affected_count} 条消息"),
                channel: FeedbackChannel::UiOnly,
            }),
        },
        PipelineOutcome::Cancelled { history } => CommandResult {
            // [TRAP] cancel_token.cancelled() 分支返回 Cancelled；executor.rs 上游
            // 对 Cancelled 有专门处理（保留 agent 已写入 state 的消息，避免 amnesia）。
            // P2-4：cancel 是用户主动操作，非执行失败——Warning 级提示
            // （stop_reason: Cancelled 已由编排层消费）。
            messages: history,
            stop_reason: PromptStopReason::Cancelled,
            feedback: Some(CommandFeedback {
                level: FeedbackLevel::Warning,
                message: "compact cancelled".to_string(),
                channel: FeedbackChannel::UiOnly,
            }),
        },
        PipelineOutcome::EarlyReturn {
            history,
            stop_reason,
            message,
        } => CommandResult {
            messages: history,
            stop_reason,
            feedback: Some(CommandFeedback {
                level: FeedbackLevel::Error,
                message,
                channel: FeedbackChannel::UiOnly,
            }),
        },
    }
}

#[cfg(test)]
#[path = "compact_pipeline_test.rs"]
mod tests;
