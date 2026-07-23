//! Compact event handlers — CompactStarted, CompactCompleted, CompactError.

use super::*;
use crate::i18n;
use crate::kit::tui_render_unit::TuiNoteLevel;
use fluent_bundle::FluentValue;

pub(super) fn handle_compact_started(state: &mut BridgeState) {
    tracing::info!("bridge: CompactStarted");
    state.phase = SessionPhase::PromptRunning;
    super::render::push_acp_state(state);
}

pub(super) fn handle_compact_completed(
    state: &mut BridgeState,
    summary: &str,
    files: &[serde_json::Value],
    skills: &[String],
    strategy: &str,
) {
    tracing::info!(
        summary_len = summary.len(),
        %strategy,
        "bridge: CompactCompleted"
    );
    state.compact_just_completed = true;
    // 不重置 phase——auto compact 后 ReAct 循环继续运行，
    // loading 由流式事件（TextChunk/ToolStarted）和 TurnDone 管理。
    // 手动 /compact 路径由 push_done → TurnDone 兜底清除。
    // 全量压缩和有效微压缩都注入消息流通知
    // strategy 字段由 compact 引擎提供，准确区分 Micro/Full/Smart
    let mut parts = vec![];
    let file_count = files.len();
    let skill_count = skills.len();
    let compact_type = match strategy {
        "micro" => i18n::tr("app-note-compact-type-micro"),
        _ => i18n::tr("app-note-compact-type-full"), // "full" | "smart" → Full label
    };
    if file_count > 0 {
        parts.push(format!("{file_count} 文件"));
    }
    if skill_count > 0 {
        parts.push(format!("{skill_count} skills"));
    }
    let detail = if parts.is_empty() {
        String::new()
    } else {
        format!("（{}）", parts.join("，"))
    };
    let text = if summary.is_empty() {
        i18n::tr_args(
            "app-note-compact-completed",
            &[
                ("detail".into(), FluentValue::from(detail.as_str())),
                ("type".into(), FluentValue::from(compact_type.as_str())),
            ],
        )
    } else {
        let brief: String = summary.chars().take(60).collect();
        let suffix = if summary.chars().count() > 60 {
            "…"
        } else {
            ""
        };
        let summary_display = format!("{brief}{suffix}");
        i18n::tr_args(
            "app-note-compact-completed-summary",
            &[
                ("detail".into(), FluentValue::from(detail.as_str())),
                (
                    "summary".into(),
                    FluentValue::from(summary_display.as_str()),
                ),
                ("type".into(), FluentValue::from(compact_type.as_str())),
            ],
        )
    };
    state.inject_system_note(text, TuiNoteLevel::Warning);
}

pub(super) fn handle_compact_error(state: &mut BridgeState, message: &str) {
    tracing::warn!(message, "bridge: CompactError");
    let text = i18n::tr_args(
        "app-note-compact-error",
        &[("message".into(), FluentValue::from(message))],
    );
    state.inject_system_note(text, TuiNoteLevel::Warning);
    state.phase = SessionPhase::Idle;
    super::render::push_acp_state(state);
}
