//! Full Compact 实现
//!
//! 完整流程：
//! 1. 从包含 canonical reminder 的可见模型上下文派生摘要请求
//! 2. LLM 生成结构化摘要
//! 3. 后处理摘要
//! 4. 快照内自有的普通历史和 reminder 标 excluded（保留 System / ancestor）
//! 5. 追加 Human 摘要消息（带 CONTINUATION_HINT，wrap 在 system-reminder 标签中）
//!
//! 历史工具调用及结果留存在 transcript 中；不会从计算实例本机重新读取 Workspace 文件。

use peri_model::{ModelMessage, ModelRequest};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::agent::{
    compact_v2::{config::CompactConfig, CompactOutcome},
    events::CompactStrategy,
    model_bridge::{map_model_error, AgentModelBridge},
};
use crate::error::AgentResult;
use crate::messages::BaseMessage;
use crate::session::transcript::MessageTranscript;
use crate::session::MessageFlags;
use crate::thread::CompactionChange;

// ─── 公共常量 ──────────────────────────────────────────────────────────────────

/// Full Compact 摘要 system prompt
const SUMMARY_SYSTEM_PROMPT: &str = include_str!("descriptions/summary_system_prompt.md");

/// Full Compact user prompt 模板
const SUMMARY_USER_PROMPT: &str = include_str!("descriptions/summary_user_prompt.md");

#[path = "summary.rs"]
mod summary;

use summary::{assemble_summary, SummaryAssembly};

const SUMMARY_CONTINUATION_PROMPT: &str = "Your summary was cut off by the output token limit. \
Continue exactly from the end of your previous text, including any unfinished word or tag. \
Output only the missing remainder; do not restart, repeat earlier sections, or add analysis. \
This is your final response: finish the remaining essential facts concisely within the output budget \
and close </summary>. There will be no further continuation. Do not call tools.";

// ─── Full Compact ───────────────────────────────────────────────────────────────

/// Full Compact 内部实现
///
/// 步骤：
/// 1. 从包含 canonical reminder 的可见模型上下文派生摘要请求
/// 2. LLM 生成结构化摘要
/// 3. 后处理摘要
/// 4. 快照内自有的普通历史和 reminder 标 excluded（保留 System / ancestor）
/// 5. 追加 Human 摘要消息（带 CONTINUATION_HINT，wrap 在 system-reminder 标签中）
pub(super) async fn full_compact_inner(
    transcript: &mut MessageTranscript,
    llm: Option<&dyn peri_model::Model>,
    config: &CompactConfig,
    _cwd: &str,
) -> AgentResult<super::CompactResult> {
    let llm = llm.ok_or(crate::error::AgentError::CompactNoLlm)?;
    // Full 和 Reason 读取同一已提交视图。按摘要 provider 的协议保护 reasoning；恢复器只接受
    // 与工具输入/思考独立的 ToolResult 投影，不为摘要另做有损预览。
    let visible = super::projection::render_persisted_llm_view(
        transcript,
        &AgentModelBridge::projection_capabilities(llm),
    )?;
    let before_visible_len = visible.len();
    // 先固定本次快照的 own IDs；await 期间到达 inbox 的新结果不属于本次摘要。
    // ancestor 与 System 仍由各自 owner 管理；canonical reminder 不是豁免历史。
    let flag_updates: Vec<_> = transcript
        .entries()
        .iter()
        .skip(transcript.ancestor_len())
        .filter(|entry| !transcript.flags(entry.id()).excluded)
        .filter(|entry| !matches!(entry.as_message(), Some(BaseMessage::System { .. })))
        .map(|entry| {
            (
                entry.id(),
                MessageFlags {
                    excluded: true,
                    ..Default::default()
                },
            )
        })
        .collect();
    let affected_count = flag_updates.len();
    // 只有继承上下文时没有可替换的 own 历史，沿用空历史 fallback，避免无效摘要调用。
    let has_history = !flag_updates.is_empty()
        && visible
            .iter()
            .any(|message| !matches!(message, BaseMessage::System { .. }));
    let summary = if has_history {
        // 保留历史的角色、工具配对和完整正文；摘要指令只追加到派生请求，
        // 不回写原 transcript，也不提供可执行工具。
        let mut messages = AgentModelBridge::convert_messages(&visible)?;
        messages.insert(0, ModelMessage::system_text(SUMMARY_SYSTEM_PROMPT));
        messages.push(ModelMessage::user_text(SUMMARY_USER_PROMPT.replace(
            "{summary_target_tokens}",
            &(config.summary_max_tokens / 2).max(1).to_string(),
        )));
        let request = ModelRequest::new(messages).with_max_tokens(config.summary_max_tokens);
        complete_summary(llm, request).await?
    } else {
        // 全 System / 空历史仍保持命令输出 Human-first 的既有契约。
        "No conversation history to compact.".to_owned()
    };

    transcript
        .commit_compaction_lifecycle(CompactionChange {
            flag_updates,
            appended_messages: vec![build_summary_message(&summary)],
        })
        .await?;
    transcript.mark_full_compaction_committed();

    let after_visible = transcript
        .entries()
        .iter()
        .filter(|entry| !transcript.flags(entry.id()).excluded)
        .count();

    debug!(
        before_visible_len,
        after_visible, "Full Compact: excluded 旧消息 + 追加摘要"
    );

    Ok(super::CompactResult {
        strategy: CompactStrategy::Full,
        affected_count,
        estimated_tokens_saved: 0,
        before_visible_len,
        after_visible_len: after_visible,
        summary: Some(summary),
        full_escalation_reason: None,
        outcome: CompactOutcome::FullApplied,
        failure: None,
        changed_messages: 0,
        changed_fields: 0,
        no_op_candidates: 0,
    })
}

async fn complete_summary(
    llm: &dyn peri_model::Model,
    mut request: ModelRequest,
) -> AgentResult<String> {
    let first = llm
        .complete(request.clone(), CancellationToken::new())
        .await
        .map_err(map_model_error)?;
    match assemble_summary(&first, None).inspect_err(|error| {
        warn!(%error, "Full Compact first response has no usable summary");
    })? {
        SummaryAssembly::Complete { text, .. } => return Ok(text),
        SummaryAssembly::Continue => {}
    }
    warn!(
        output_tokens = first.usage().map(|usage| usage.output_tokens),
        "Full Compact output truncated; continuing the existing summary once"
    );
    request.messages.push(ModelMessage::assistant_text(
        first.assistant_text().unwrap_or_default(),
    ));
    request
        .messages
        .push(ModelMessage::user_text(SUMMARY_CONTINUATION_PROMPT));
    tokio::task::yield_now().await;
    let second = llm
        .complete(request, CancellationToken::new())
        .await
        .map_err(map_model_error)?;
    match assemble_summary(&first, Some(&second)).inspect_err(|error| {
        warn!(%error, "Full Compact continuation has no usable summary");
    })? {
        SummaryAssembly::Complete { text, truncated } => {
            if truncated {
                warn!(
                    output_tokens = second.usage().map(|usage| usage.output_tokens),
                    "Full Compact reached output limit twice; preserving both chunks and omitting the remaining tail"
                );
            }
            Ok(text)
        }
        SummaryAssembly::Continue => {
            unreachable!("a second summary response cannot request continuation")
        }
    }
}

/// 构造 Full Compact 的 Human 摘要消息。
fn build_summary_message(summary: &str) -> BaseMessage {
    BaseMessage::human(format!(
        "{}\n\n{}",
        crate::agent::compact_v2::CONTINUATION_HINT,
        summary
    ))
}

/// 提取旧格式压缩文件/Skill 元信息（事实源 peri-acp-types::compact）
pub use peri_acp_types::compact::{extract_file_info, extract_skill_names};

#[cfg(test)]
#[path = "full_test.rs"]
mod tests;

#[cfg(test)]
#[path = "full_report_test.rs"]
mod report_tests;

#[cfg(test)]
#[path = "full_continuation_test.rs"]
mod continuation_tests;
