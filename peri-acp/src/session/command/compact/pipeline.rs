//! Compact Pipeline — `/compact` 命令路径的 v2 实现。
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

use std::sync::Arc;

use peri_agent::{
    agent::{compact::CompactConfig, compact_v2, AgentCancellationToken},
    llm::BaseModel,
    messages::BaseMessage,
    session::transcript::MessageTranscript,
};
use tracing::{info, warn};

use crate::session::{command::CommandContext, executor::PromptStopReason};

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

/// 加载 compact 配置：`unwrap_or_default()` 后立即应用 env overrides。
///
/// [TRAP] env 优先级 DISABLE_COMPACT / DISABLE_AUTO_COMPACT / COMPACT_THRESHOLD 每轮
/// 重新读取（非 frozen），apply_env_overrides() 必须在 unwrap_or_default() 之后调用。
pub fn load_compact_config(peri_config: &crate::provider::PeriConfig) -> CompactConfig {
    let mut compact_config = peri_config.config.compact.clone().unwrap_or_default();
    compact_config.apply_env_overrides();
    compact_config
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
        peri_config,
        auxiliary_model,
        event_sink,
        cancel_token,
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

    // 阶段 2: 加载 compact 配置
    let compact_config = load_compact_config(&peri_config);

    // 阶段 3: 解析 auxiliary model
    let auxiliary_model: Arc<dyn BaseModel> = match auxiliary_model {
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

    // 阶段 4: 发出 CompactStarted 事件
    emit_compact_started(&event_sink, &session_id).await;

    // 阶段 5: 加载 history 进临时 transcript 并运行 v2 compact（force=true 触发 Full）
    let mut transcript = MessageTranscript::new();
    for msg in &history {
        transcript.append(msg.clone());
    }

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
    model: &dyn BaseModel,
    config: &CompactConfig,
    cwd: &str,
    cancel_token: &AgentCancellationToken,
    event_sink: &Arc<dyn crate::session::event_sink::EventSink>,
    session_id: &str,
    consecutive_failures: &mut u32,
) -> Result<compact_v2::CompactResult, CancelOrError> {
    let result = tokio::select! {
        r = compact_v2::run_compact(
            transcript,
            Some(model),
            config,
            1.0, // budget=1.0 + force=true → 强制 Full Compact
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
    let messages: Vec<BaseMessage> = transcript.visible_messages().into_iter().cloned().collect();

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
    pub files: Vec<peri_agent::agent::events::CompactFileInfo>,
    pub skills: Vec<String>,
}

/// `/compact` 命令入口：执行完整 Pipeline 并映射终态到 `CommandResult`。
pub async fn execute_compact(ctx: super::CommandContext) -> super::CommandResult {
    match run_pipeline(ctx).await {
        PipelineOutcome::Completed { messages } => super::CommandResult {
            messages,
            stop_reason: PromptStopReason::EndTurn,
        },
        PipelineOutcome::Cancelled { history } => super::CommandResult {
            // [TRAP] cancel_token.cancelled() 分支返回 Cancelled；executor.rs 上游
            // 对 Cancelled 有专门处理（保留 agent 已写入 state 的消息，避免 amnesia）。
            messages: history,
            stop_reason: PromptStopReason::Cancelled,
        },
        PipelineOutcome::EarlyReturn {
            history,
            stop_reason,
        } => super::CommandResult {
            messages: history,
            stop_reason,
        },
    }
}
