//! Compact Pipeline — execute() 内部各阶段实现。
//!
//! 按显式 Pipeline (Orchestration) 拆分：每阶段一个纯函数 + 显式输入输出类型，
//! 编排层（`compact.rs::execute`）只做组合。
//!
//! 阶段顺序：
//!   validate_inputs → resolve_auxiliary_model → (emit_started)
//!   → run_full_compact_with_cancel → re_inject_phase → assemble_compact_messages
//!   → (emit_completed)
//!
// [TRAP] 配置覆盖顺序：peri_config.config.compact.clone().unwrap_or_default() 之后
// 必须调 apply_env_overrides()。env 优先级 DISABLE_COMPACT / DISABLE_AUTO_COMPACT /
// COMPACT_THRESHOLD 每轮重新读取（非 frozen），此顺序不能调换。
// （详见 CLAUDE.md 环境变量章节、compact 章节）
//
// [TRAP] cancel_token.cancelled() 分支返回 PromptStopReason::Cancelled；错误/空历史/
// 无模型当前都返回 EndTurn。此语义在重构时不可变更，executor.rs 上游对 Cancelled 有
// 专门处理（spec/global/domains/agent.md#issue_2026-05-29-ctrl-c-interrupt-causes-agent-amnesia、
// #issue_2026-05-29-llm-stream-error-causes-amnesia）。
//
// [TRAP] full_compact 的第 4 参空字符串是 cwd 之前的占位（spec 全名/摘要 LLM 上下文锚点）。
// line 调用 `full_compact(&history, model, &config, "", &cwd)` 时空串位置不能改。
// full_compact 增加 cwd 参数是为修复 full-compact-loses-project-path-context
// （CLAUDE.md compact 章节 [TRAP]），不可回退。

use std::sync::Arc;

use peri_agent::{
    agent::{
        compact::{extract_file_info, extract_skill_names, full_compact, re_inject},
        events::CompactFileInfo,
        AgentCancellationToken,
    },
    llm::BaseModel,
    messages::BaseMessage,
};
use tracing::{info, warn};

use crate::session::{command::CommandContext, executor::PromptStopReason};

use super::events::{
    emit_compact_completed, emit_compact_error, emit_compact_started, FULL_COMPACT_MICRO_CLEARED,
};
use super::invariant::build_summary_human_message;

/// Compact 配置（从 peri_config 提取并应用 env overrides 后的快照）。
pub type CompactConfig = peri_agent::agent::compact::CompactConfig;

/// `full_compact` 的产物。
pub struct CompactOutput {
    pub summary: String,
}

/// `re_inject` 的产物（messages + 注入计数）。
pub struct ReInjectOutput {
    pub messages: Vec<BaseMessage>,
    pub files_injected: usize,
    pub skills_injected: usize,
}

/// Pipeline 终态。编排层据此决定返回值与是否中途 short-circuit。
pub enum PipelineOutcome {
    /// 正常完成：组装后的消息（首条 Human + System(文件)... + System(Skills)...）。
    Completed { messages: Vec<BaseMessage> },
    /// 取消（用户 Ctrl+C）：保留原 history，stop_reason = Cancelled。
    Cancelled { history: Vec<BaseMessage> },
    /// 边界情况（空历史 / 无模型 / full_compact 失败）：保留原 history，stop_reason = EndTurn。
    /// `error_event_message` 提示编排层已发出 CompactError 事件。
    EarlyReturn {
        history: Vec<BaseMessage>,
        stop_reason: PromptStopReason,
    },
}

/// 加载 compact 配置：`unwrap_or_default()` 后立即应用 env overrides。
///
// [TRAP] env 优先级 DISABLE_COMPACT / DISABLE_AUTO_COMPACT / COMPACT_THRESHOLD 每轮
// 重新读取（非 frozen），apply_env_overrides() 必须在 unwrap_or_default() 之后调用。
pub fn load_compact_config(peri_config: &crate::provider::PeriConfig) -> CompactConfig {
    let mut compact_config = peri_config.config.compact.clone().unwrap_or_default();
    compact_config.apply_env_overrides();
    compact_config
}

/// 运行 full_compact + re_inject + assemble_messages 的完整 Pipeline。
///
/// 调用方（`compact.rs::execute`）负责在调用前完成空 history 短路。
/// 此函数内部发出 CompactStarted / CompactError / CompactCompleted 事件。
///
/// 返回 `PipelineOutcome`，由调用方映射为 `CommandResult`。
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

    // 阶段 5: 执行 full_compact（支持 Ctrl+C 取消）
    let compact_result = match run_full_compact_with_cancel(
        &history,
        auxiliary_model.as_ref(),
        &compact_config,
        &cwd,
        &cancel_token,
        &event_sink,
        &session_id,
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
        summary_len = compact_result.summary.len(),
        "compact: full_compact 完成"
    );

    // 阶段 6: 执行 re_inject
    let re_inject_result = re_inject_phase(&history, &compact_config, &cwd).await;

    info!(
        files_injected = re_inject_result.files_injected,
        skills_injected = re_inject_result.skills_injected,
        "compact: re_inject 完成"
    );

    // 阶段 7: 组装最终消息（Human-first 不变量）
    let assembled = assemble_compact_messages(&compact_result.summary, &re_inject_result);

    // 阶段 8: 发出 CompactCompleted 事件（messages 字段与 result.messages 共享 clone）
    emit_compact_completed(
        &event_sink,
        &session_id,
        compact_result.summary.clone(),
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

/// full_compact + 取消语义的执行结果。
enum CancelOrError {
    Cancelled,
    Error,
}

/// 执行 full_compact 并封装取消/错误路径。
///
// [TRAP] full_compact 的第 4 参空字符串是 cwd 之前的占位（spec 全名/摘要 LLM 上下文锚点），
// 不可改。`full_compact(&history, model, &config, "", &cwd)` 时空串位置固定。
async fn run_full_compact_with_cancel(
    history: &[BaseMessage],
    model: &dyn BaseModel,
    config: &CompactConfig,
    cwd: &str,
    cancel_token: &AgentCancellationToken,
    event_sink: &Arc<dyn crate::session::event_sink::EventSink>,
    session_id: &str,
) -> Result<CompactOutput, CancelOrError> {
    let compact_result = tokio::select! {
        r = full_compact(history, model, config, "", cwd) => {
            match r {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, "compact: full_compact 失败");
                    emit_compact_error(event_sink, session_id, e.to_string()).await;
                    return Err(CancelOrError::Error);
                }
            }
        }
        _ = cancel_token.cancelled() => {
            tracing::info!(session_id = %session_id, "compact cancelled by user");
            emit_compact_error(event_sink, session_id, "compact cancelled").await;
            return Err(CancelOrError::Cancelled);
        }
    };

    Ok(CompactOutput {
        summary: compact_result.summary,
    })
}

/// re_inject 阶段：根据 history 重新注入文件与 Skills System 消息。
async fn re_inject_phase(
    history: &[BaseMessage],
    config: &CompactConfig,
    cwd: &str,
) -> ReInjectOutput {
    let re_inject_result = re_inject(history, config, cwd).await;
    ReInjectOutput {
        messages: re_inject_result.messages,
        files_injected: re_inject_result.files_injected,
        skills_injected: re_inject_result.skills_injected,
    }
}

/// 组装最终消息：首条 Human(摘要+续接指令) + System(文件)... + System(Skills)...。
///
// [TRAP] compact 后消息结构必须以 `BaseMessage::human(summary + continuation)` 开头。
// 禁止将摘要放在 `BaseMessage::system()` 中。完整结构：
//   [Human(摘要+续接指令), System(文件)..., System(Skills)...]。
// （详见 spec/global/domains/compact.md#issue_2026-05-20-auto-compact-empty-messages-400）
pub fn assemble_compact_messages(
    summary: &str,
    re_inject_result: &ReInjectOutput,
) -> AssembledMessages {
    let first = build_summary_human_message(summary);
    let mut new_messages = vec![first];
    new_messages.extend(re_inject_result.messages.clone());

    let files = extract_file_info(&re_inject_result.messages);
    let skills = extract_skill_names(&re_inject_result.messages);

    AssembledMessages {
        messages: new_messages,
        files,
        skills,
    }
}

/// assemble 阶段产物。
pub struct AssembledMessages {
    pub messages: Vec<BaseMessage>,
    pub files: Vec<CompactFileInfo>,
    pub skills: Vec<String>,
}

/// `/compact` 命令入口：执行完整 Pipeline 并映射终态到 `CommandResult`。
///
/// 从 `compact.rs` 的 `AgentCommand::execute` 提取此方法，使 `compact.rs` 纯化为
/// 仅含 `mod` + `pub struct` + trait impl 的真 shim（零业务逻辑）。
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
