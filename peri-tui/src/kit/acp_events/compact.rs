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
    trigger: &str,
    outcome: &str,
) {
    tracing::info!(
        summary_len = summary.len(),
        %strategy,
        %trigger,
        %outcome,
        "bridge: CompactCompleted"
    );
    // S4.1 方案 A：trigger 由服务端透传。仅手动 /compact 置
    // compact_just_completed（TurnDone 需在完整重建后到达）；auto compact
    // 不置位——auto 后 ReAct 循环继续运行，zero-output 后重放旧消息的
    // 边缘洞即被根治（流事件清除逻辑保留为防御）。
    if trigger == "manual" {
        state.compact_just_completed = true;
    }
    // 不重置 phase——auto compact 后 ReAct 循环继续运行，
    // loading 由流式事件（TextChunk/ToolStarted）和 TurnDone 管理。
    // 手动 /compact 路径由 push_done → TurnDone 兜底清除。

    // 根据 outcome 做精确展示
    let compact_type = match outcome {
        "micro_applied" => i18n::tr("app-note-compact-type-micro"),
        "smart_applied" => i18n::tr("app-note-compact-type-smart"),
        "full_applied" => i18n::tr("app-note-compact-type-full"),
        "full_failed" => return, // FullFailed: SystemNote 不显示，静默跳过
        "shadowed" => return,    // Shadowed: 不显示 SystemNote
        "micro_applied_then_full_failed" => return, // 微压缩已应用但全量失败，静默跳过
        "smart_applied_then_full_failed" => return, // Smart 已应用但 Full 失败，静默跳过
        "interrupted_after_commit" => return, // commit 已发生但流程被中断，由流式事件恢复展示，不注入 SystemNote
        _ => {
            // fallback: 按 strategy 判断
            match strategy {
                "micro" => i18n::tr("app-note-compact-type-micro"),
                _ => i18n::tr("app-note-compact-type-full"),
            }
        }
    };

    let mut parts = vec![];
    let file_count = files.len();
    let skill_count = skills.len();
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
    state.inject_system_note(text.clone(), TuiNoteLevel::Warning);

    // manual /compact 的完成提示需跨 TurnDone → session/load replay 存活：
    // replay 的 BRIDGE_RESET_COUNTER 重置会清空 committed（含本 SystemNote），
    // bridge reset 分支从 PENDING_COMPACT_NOTE 重建（issue
    // 2026-08-08-e2e-compact-command-screenshot-too-early）。auto compact 不
    // 写入——无 replay 触发，避免残留串到后续 thread 切换的 reset。
    if trigger == "manual" {
        crate::kit::atoms::PENDING_COMPACT_NOTE.set(Some(text));
    }
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
