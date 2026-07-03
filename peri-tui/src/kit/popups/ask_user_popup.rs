//! ratatui-kit AskUserPopup component.
//!
//! 用户问答弹窗：从 `ASK_USER_PENDING` atom 读取真实问题列表（id/header/
//! question/options/multi_select）。支持 Up/Down 在问题间切换、Tab 切换焦点
//! 问题、1-9 选择选项、Enter 提交（关闭 popup）、Esc 取消。
//!
//! I21-B：替换原 mock_questions() 写死 3 个静态问题——现在 popup 展示 agent
//! 实际提出的问题，用户能据此回答。
//!
//! ## 用户路径
//!
//! - **Up/Down**：在问题列表中移动选中
//! - **1-9 / a-z**：在当前 multi_select=false 问题的 options 中选中（最后
//!   选择覆盖之前的；单选语义）
//! - **Enter**：提交（关闭 popup——ASK_USER_PENDING 由 close_popup 清空）
//! - **Esc**：取消（由全局 Esc 链处理）

use peri_acp_types::event_data::{AskUser, Question};
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers},
    prelude::*,
    ratatui::{
        style::{Style, Stylize},
        text::Line,
    },
};

use crate::kit::atoms::ASK_USER_PENDING;
use crate::kit::list_nav::{
    ListNavAction, classify_list_nav, cycle_next, cycle_previous, next_selection,
    previous_selection,
};
use crate::kit::popup_overlay::close_popup;
use crate::kit::theme;

#[component]
pub fn AskUserPopup(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let pending_store = hooks.use_atom(&ASK_USER_PENDING);
    let pending: Option<AskUser> = pending_store.read().clone();
    let _ = pending_store;

    // 当前选中的问题索引
    let focused = hooks.use_state(|| 0usize);
    // 每个问题的当前选中 option index（单选语义；multi_select 暂按单选处理）
    let answers = hooks.use_state(Vec::<Option<usize>>::new);
    // I22-D：session 指纹——记录上次渲染时的 question id 列表。
    // 当 payload 变化（不同 AskUser 事件）时复位 focused/answers，
    // 避免上次会话的旧焦点/旧选项残留到新 popup。
    let session_fingerprint = hooks.use_state(Vec::<String>::new);

    let question_count = pending.as_ref().map(|q| q.questions.len()).unwrap_or(0);

    // I22-D：检测 payload 变化——question id 列表不同则视为新 session
    let current_fingerprint: Vec<String> = pending
        .as_ref()
        .map(|p| p.questions.iter().map(|q| q.id.clone()).collect())
        .unwrap_or_default();
    if *session_fingerprint.read() != current_fingerprint {
        // 新 popup 会话——复位本地状态
        *focused.write() = 0;
        *answers.write() = vec![None; question_count];
        *session_fingerprint.write() = current_fingerprint;
    }

    // 闭包另持一份 pending 副本（避免与渲染端争用 move）
    let pending_for_closure = pending.clone();

    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, move |event| {
        let Event::Key(key) = event else {
            return EventResult::Ignored;
        };
        if key.kind != KeyEventKind::Press {
            return EventResult::Ignored;
        }

        match classify_list_nav(&key) {
            Some(ListNavAction::MoveUp) => {
                let mut f = focused.write();
                *f = previous_selection(*f);
                EventResult::Consumed
            }
            Some(ListNavAction::MoveDown) if question_count > 0 => {
                let mut f = focused.write();
                *f = next_selection(*f, question_count);
                EventResult::Consumed
            }
            Some(ListNavAction::CycleForward) if question_count > 0 => {
                let mut f = focused.write();
                *f = cycle_next(*f, question_count);
                EventResult::Consumed
            }
            Some(ListNavAction::CycleBackward) if question_count > 0 => {
                let mut f = focused.write();
                *f = cycle_previous(*f, question_count);
                EventResult::Consumed
            }
            Some(ListNavAction::Confirm) => {
                close_popup();
                EventResult::Consumed
            }
            _ => match (key.modifiers, key.code) {
                (KeyModifiers::NONE, KeyCode::Char(c)) if c.is_ascii_digit() => {
                    let idx = *focused.read();
                    let digit = (c as u8 - b'1') as usize;
                    let mut a = answers.write();
                    if let Some(questions) = pending_for_closure.as_ref().map(|p| &p.questions)
                        && let Some(q) = questions.get(idx)
                        && digit < q.options.len()
                    {
                        if idx >= a.len() {
                            a.resize(idx + 1, None);
                        }
                        a[idx] = Some(digit);
                    }
                    EventResult::Consumed
                }
                _ => EventResult::Ignored,
            },
        }
    });

    let popup_tokens = &theme::component().popup;
    let semantic = theme::semantic();
    let mut lines: Vec<Line<'_>> = Vec::new();

    match &pending {
        None => {
            // 理论上不会渲染此分支
            lines.push(Line::from(""));
            lines.push(
                Line::from("  No pending questions.")
                    .fg(semantic.text.muted)
                    .italic(),
            );
            lines.push(Line::from(""));
            lines.push(Line::from("  Esc: close").fg(semantic.text.dim));
        }
        Some(au) if au.questions.is_empty() => {
            lines.push(Line::from(""));
            lines.push(
                Line::from("  Agent asked 0 questions (malformed request).")
                    .fg(semantic.status.warning),
            );
            lines.push(Line::from(""));
            lines.push(Line::from("  Esc: close").fg(semantic.text.dim));
        }
        Some(au) => {
            let focused_idx = (*focused.read()).min(au.questions.len() - 1);
            let answers_read = answers.read();

            lines.push(Line::from(""));

            for (i, q) in au.questions.iter().enumerate() {
                let is_focused = i == focused_idx;
                render_question(
                    &mut lines,
                    i,
                    q,
                    is_focused,
                    answers_read.get(i).copied().flatten(),
                    popup_tokens,
                    semantic,
                );
                lines.push(Line::from(""));
            }

            lines.push(
                Line::from(
                    "  ↑↓: navigate  |  1-9: select option  |  Enter: submit  |  Esc: cancel",
                )
                .fg(semantic.text.dim),
            );
        }
    }

    popup_text_shell!(" Question ", popup_tokens.action_primary, lines)
}

/// 渲染单个问题——header + question + 选项列表 + 当前选中标记
fn render_question(
    lines: &mut Vec<Line<'_>>,
    idx: usize,
    q: &Question,
    is_focused: bool,
    selected: Option<usize>,
    popup_tokens: &theme::PopupTokens,
    semantic: &theme::SemanticTokens,
) {
    // 问题 header 行（如 "Module Name"）
    let header_marker = if is_focused { "▶" } else { " " };
    lines.push(if is_focused {
        Line::from(format!("{} Q{}: {}", header_marker, idx + 1, q.header))
            .fg(popup_tokens.selected_fg)
            .bold()
    } else {
        Line::from(format!("{} Q{}: {}", header_marker, idx + 1, q.header))
            .fg(semantic.text.primary)
    });

    // 问题正文
    lines.push(Line::from(format!("    {}", q.question)).fg(semantic.text.muted));

    // 选项列表
    for (opt_i, opt) in q.options.iter().enumerate() {
        let is_selected = selected == Some(opt_i);
        let check = if is_selected { "✔" } else { " " };
        let key_hint = if opt_i < 9 {
            format!("{}", opt_i + 1)
        } else {
            " ".to_string()
        };

        let style = if is_selected {
            Style::new().fg(popup_tokens.action_primary).bold()
        } else {
            Style::new().fg(semantic.text.primary)
        };

        let line = format!("    {}) {} {}", key_hint, check, opt.label);
        lines.push(Line::from(line).style(style));

        // 选项描述（如有，次行展示）
        if !opt.description.is_empty() {
            lines.push(Line::from(format!("        {}", opt.description)).fg(semantic.text.dim));
        }
    }

    // multi_select 提示（当前实现按单选处理，提示用户）
    if q.multi_select && !q.options.is_empty() {
        lines.push(
            Line::from("    (multi-select not yet supported — treated as single)")
                .fg(semantic.text.dim)
                .italic(),
        );
    }

    // 当前未选任何项的提示
    if q.options.is_empty() {
        lines.push(Line::from("    (no options provided)").fg(semantic.text.dim));
    }
}

/// 编译期断言：QuestionOption 字段可读（防止上游 DTO 变更未发现）
#[cfg(test)]
mod tests {
    use peri_acp_types::event_data::QuestionOption;

    #[test]
    fn test_question_option_struct_fields() {
        // 编译期验证 QuestionOption 字段（确保上游 DTO 改名时编译失败）
        let opt = QuestionOption {
            label: "test".to_string(),
            description: "desc".to_string(),
        };
        assert_eq!(opt.label, "test");
        assert_eq!(opt.description, "desc");
    }
}
