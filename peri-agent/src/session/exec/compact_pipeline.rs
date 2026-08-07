//! Compact Pipeline — `/compact` 命令路径的 v2 实现（L5：自
//! peri-acp/src/host/exec/compact_pipeline.rs 迁入）。
//!
//! 本模块承载深绑 Agent 层执行类型（`MessageTranscript` /
//! `compact_v2::run_compact`）的 `/compact` 命令执行体；ACP 协议面
//! `session::command::compact::pipeline` 保留 re-export 薄壳。
//!
//! [v2] 从 v1 `full_compact + re_inject` 迁移到 `compact_v2::run_compact(force=true)`：
//! - 把 history 加载进临时 `MessageTranscript`
//! - 调用 `run_compact` 触发 Full Compact + re-inject
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

use std::sync::Arc;

use peri_acp_types::command::{CommandContext, CommandResult, PromptStopReason};
use peri_acp_types::compact::CompactConfig;
use peri_acp_types::event::EventSink;
use peri_acp_types::messages::BaseMessage;
use tokio_util::sync::CancellationToken as AgentCancellationToken;
use tracing::{info, warn};

use crate::agent::compact_v2;
use crate::session::transcript::MessageTranscript;

use super::events::{
    emit_compact_completed, emit_compact_error, emit_compact_started, FULL_COMPACT_MICRO_CLEARED,
};

/// Pipeline 终态。编排层据此决定返回值与是否中途 short-circuit。
pub enum PipelineOutcome {
    /// 正常完成：组装后的消息（首条 Human + re-inject 消息...）。
    Completed { messages: Vec<BaseMessage> },
    /// 取消（用户 Ctrl+C）：保留原 history，stop_reason = Cancelled。
    Cancelled { history: Vec<BaseMessage> },
    /// 边界情况（空历史 / 无模型 / compact 失败）：保留原 history，stop_reason = EndTurn。
    EarlyReturn {
        history: Vec<BaseMessage>,
        stop_reason: PromptStopReason,
    },
}

/// 运行 v2 compact 的完整 Pipeline。
///
/// 调用方（`compact.rs::execute`）负责在调用前完成空 history 短路。
/// 此函数内部发出 CompactStarted / CompactError / CompactCompleted 事件。
pub async fn run_pipeline(ctx: CommandContext) -> PipelineOutcome {
    let CommandContext {
        session_id,
        history,
        cwd,
        compact_config,
        auxiliary_model,
        event_sink,
        cancel_token,
        thread_store,
        thread_id,
        ..
    } = ctx;

    tracing::debug!(history_len = history.len(), "compact: pipeline started");

    // 阶段 1: 验证 history 非空（边界短路）
    if history.is_empty() {
        warn!("compact: 无历史消息可压缩");
        emit_compact_error(&event_sink, &session_id, "no history to compact").await;
        return PipelineOutcome::EarlyReturn {
            history,
            stop_reason: PromptStopReason::EndTurn,
        };
    }

    // 阶段 3: 解析 auxiliary model
    let auxiliary_model: Arc<dyn peri_model::Model> = match auxiliary_model {
        Some(m) => m,
        None => {
            warn!("compact: 无可用模型");
            emit_compact_error(&event_sink, &session_id, "no model available for compact").await;
            return PipelineOutcome::EarlyReturn {
                history,
                stop_reason: PromptStopReason::EndTurn,
            };
        }
    };

    // 阶段 4: 手动 compact 必须绑定持久化 transcript，保证 Full lifecycle 可原子提交。
    let (thread_store, thread_id) = match (thread_store, thread_id) {
        (Some(store), Some(thread_id)) => (store, thread_id),
        _ => {
            warn!("compact: persistence is unavailable");
            emit_compact_error(
                &event_sink,
                &session_id,
                "compact persistence is unavailable",
            )
            .await;
            return PipelineOutcome::EarlyReturn {
                history,
                stop_reason: PromptStopReason::EndTurn,
            };
        }
    };

    if !thread_store.supports_compaction_lifecycle() {
        warn!("compact: persistence backend does not support lifecycle commits");
        emit_compact_error(
            &event_sink,
            &session_id,
            "compact lifecycle persistence is unavailable",
        )
        .await;
        return PipelineOutcome::EarlyReturn {
            history,
            stop_reason: PromptStopReason::EndTurn,
        };
    }

    // 阶段 5: 已存在的 thread 从完整消息和 flags 重建。命令输入是可见视图，
    // 因此不能将物理存储中的 excluded 原文直接与其比较。
    let persisted_history = match thread_store.load_messages(&thread_id).await {
        Ok(messages) => messages,
        Err(_) => {
            warn!("compact: failed to load persisted history");
            emit_compact_error(&event_sink, &session_id, "compact persistence failed").await;
            return PipelineOutcome::EarlyReturn {
                history,
                stop_reason: PromptStopReason::EndTurn,
            };
        }
    };
    let mut transcript = MessageTranscript::new();
    if persisted_history.is_empty() {
        transcript = transcript.with_persistence(thread_store, thread_id);
        for message in &history {
            transcript.append(message.clone());
        }
        if transcript.flush_persistence().await.is_err() {
            warn!("compact: failed to persist initial history");
            emit_compact_error(&event_sink, &session_id, "compact persistence failed").await;
            return PipelineOutcome::EarlyReturn {
                history,
                stop_reason: PromptStopReason::EndTurn,
            };
        }
    } else {
        let persisted_flags = match thread_store.load_message_flags(&thread_id).await {
            Ok(flags) => flags,
            Err(_) => {
                warn!("compact: failed to load persisted flags");
                emit_compact_error(&event_sink, &session_id, "compact persistence failed").await;
                return PipelineOutcome::EarlyReturn {
                    history,
                    stop_reason: PromptStopReason::EndTurn,
                };
            }
        };
        for message in &persisted_history {
            transcript.append(message.clone());
        }
        transcript.set_flags_batch(persisted_flags);
        let expected_history = if transcript
            .entries()
            .iter()
            .any(|entry| transcript.flags(entry.message.id()).excluded)
        {
            assemble_compact_messages(&transcript, &None).messages
        } else {
            transcript.visible_messages().into_iter().cloned().collect()
        };
        let visible_matches = expected_history.len() == history.len()
            && expected_history
                .iter()
                .zip(&history)
                .all(|(persisted, incoming)| persisted.id() == incoming.id());
        if !visible_matches {
            warn!("compact: persisted visible history does not match command history");
            emit_compact_error(
                &event_sink,
                &session_id,
                "compact persistence context mismatch",
            )
            .await;
            return PipelineOutcome::EarlyReturn {
                history,
                stop_reason: PromptStopReason::EndTurn,
            };
        }
        transcript = transcript.with_persistence(thread_store, thread_id);
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
        &event_sink,
        &session_id,
        &mut consecutive_failures,
    )
    .await
    {
        Ok(r) => r,
        Err(CancelOrError::Cancelled) => {
            return PipelineOutcome::Cancelled { history };
        }
        Err(CancelOrError::Error) => {
            return PipelineOutcome::EarlyReturn {
                history,
                stop_reason: PromptStopReason::EndTurn,
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

    // 阶段 7: 发出 CompactCompleted 事件
    emit_compact_completed(
        &event_sink,
        &session_id,
        compact_result.summary.clone().unwrap_or_default(),
        assembled.files.clone(),
        assembled.skills.clone(),
        FULL_COMPACT_MICRO_CLEARED,
        assembled.messages.clone(),
        compact_result.strategy,
        compact_result.outcome,
        compact_result.estimated_tokens_saved,
        compact_result.affected_count,
    )
    .await;

    info!("compact: 完成，session 已更新");

    PipelineOutcome::Completed {
        messages: assembled.messages,
    }
}

/// v2 run_compact + 取消语义的执行结果。
enum CancelOrError {
    Cancelled,
    Error,
}

/// 执行 v2 run_compact 并封装取消/错误路径。
#[allow(clippy::too_many_arguments)]
async fn run_v2_compact_with_cancel(
    transcript: &mut MessageTranscript,
    model: &dyn peri_model::Model,
    config: &CompactConfig,
    cwd: &str,
    cancel_token: &AgentCancellationToken,
    event_sink: &Arc<dyn EventSink>,
    session_id: &str,
    consecutive_failures: &mut u32,
) -> Result<compact_v2::CompactResult, CancelOrError> {
    let result = tokio::select! {
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
        ) => r,
        _ = cancel_token.cancelled() => {
            tracing::info!(session_id = %session_id, "compact cancelled by user");
            emit_compact_error(event_sink, session_id, "compact cancelled").await;
            return Err(CancelOrError::Cancelled);
        }
    };

    // 检测失败：affected_count == 0 + summary 为 None 表示 compact 未成功
    if result.affected_count == 0 && result.summary.is_none() {
        warn!(strategy = ?result.strategy, "compact: v2 run_compact 无效果");
        emit_compact_error(event_sink, session_id, "compact produced no effect").await;
        return Err(CancelOrError::Error);
    }

    Ok(result)
}

/// 组装最终消息：从 transcript visible_messages 提取首条 Human + re-inject 消息。
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
pub async fn execute_compact(ctx: CommandContext) -> CommandResult {
    match run_pipeline(ctx).await {
        PipelineOutcome::Completed { messages } => CommandResult {
            messages,
            stop_reason: PromptStopReason::EndTurn,
        },
        PipelineOutcome::Cancelled { history } => CommandResult {
            // [TRAP] cancel_token.cancelled() 分支返回 Cancelled；executor.rs 上游
            // 对 Cancelled 有专门处理（保留 agent 已写入 state 的消息，避免 amnesia）。
            messages: history,
            stop_reason: PromptStopReason::Cancelled,
        },
        PipelineOutcome::EarlyReturn {
            history,
            stop_reason,
        } => CommandResult {
            messages: history,
            stop_reason,
        },
    }
}
