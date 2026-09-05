//! Compact event handlers — CompactStarted, CompactCompleted.

use super::*;
use crate::i18n;
use crate::kit::tui_render_unit::TuiNoteLevel;
use fluent_bundle::FluentValue;

pub(super) fn handle_compact_started(state: &mut BridgeState) {
    tracing::info!("bridge: CompactStarted");
    state.phase = SessionPhase::PromptRunning;
    super::render::push_acp_state(state);
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_compact_completed(
    state: &mut BridgeState,
    summary: &str,
    trigger: &str,
    strategy: &str,
    affected_count: usize,
    estimated_tokens_saved: u64,
    file_count: usize,
    skill_count: usize,
) {
    tracing::info!(
        summary_len = summary.len(),
        %trigger,
        %strategy,
        affected_count,
        estimated_tokens_saved,
        file_count,
        skill_count,
        "bridge: CompactCompleted"
    );
    if trigger == "manual" {
        state.compact_just_completed = true;
    }

    let compact_type = match strategy {
        "micro" => i18n::tr("app-note-compact-type-micro"),
        "smart" => i18n::tr("app-note-compact-type-smart"),
        _ => i18n::tr("app-note-compact-type-full"),
    };
    let detail = i18n::tr_args(
        "app-note-compact-detail",
        &[
            ("messages".into(), FluentValue::from(affected_count as u64)),
            ("tokens".into(), FluentValue::from(estimated_tokens_saved)),
            ("files".into(), FluentValue::from(file_count as u64)),
            ("skills".into(), FluentValue::from(skill_count as u64)),
        ],
    );
    let summary_display: String = summary.chars().take(60).collect();
    let text = if summary_display.is_empty() {
        i18n::tr_args(
            "app-note-compact-completed",
            &[
                ("detail".into(), FluentValue::from(detail.as_str())),
                ("type".into(), FluentValue::from(compact_type.as_str())),
            ],
        )
    } else {
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
    if trigger == "manual" {
        crate::kit::atoms::PENDING_COMPACT_NOTE.set(Some(text.clone()));
    }
    state.inject_system_note(text, TuiNoteLevel::Info);
}
