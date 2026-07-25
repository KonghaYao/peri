//! ratatui-kit AskUserPanel component.
//!
//! 用户问答面板——当 agent 调用 AskUserQuestion 工具时，自动作为 Panel 内联渲染
//! 在 MessageArea 和 InputArea 之间（替代弹窗形式）。
//!
//! 面板逻辑复用 ask_user_popup 的 Tab 交互模型，但通过 panel_shell! 渲染。

use crate::app::panel_types::PanelKind;
use crate::components::textarea::{TextAreaState, wrap_text as textarea_wrap};
use crate::i18n;
use peri_acp_types::event_data::AskUser;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers},
    prelude::*,
    ratatui::{
        layout::Constraint,
        style::{Color, Modifier, Style, Stylize},
        text::Line,
    },
};

use crate::kit::ask_user_action::AskUserResponseAction;
use crate::kit::atoms::{
    ASK_USER_PENDING, ASK_USER_REQUEST_ID, ASK_USER_RESPONSE_TX, LANG_VERSION,
};
use crate::kit::list_nav::{
    ListNavAction, classify_list_nav, cycle_next, cycle_previous, next_selection,
    previous_selection,
};
use crate::kit::panel_registry;
use peri_theme::atoms::THEME_ATOM;
use serde_json::json;
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

/// 自定义文本输入的视口行数上限
const TYPING_VIEWPORT_ROWS: usize = 3;

#[component]
pub fn AskUserPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let pending_store = hooks.use_atom(&ASK_USER_PENDING);
    let pending: Option<AskUser> = pending_store.read().clone();
    let _ = pending_store;
    let _ = hooks.use_atom(&LANG_VERSION);
    // 动态换行宽度：跟随终端实际宽度，避免宽终端下内容被压缩在 80 列内
    let (term_w, _) = hooks.use_terminal_size();
    let wrap_width = if term_w > 0 {
        (term_w as usize).saturating_sub(2).max(40)
    } else {
        80
    };

    let focused = hooks.use_state(|| 0usize);
    let answers = hooks.use_state(Vec::<Vec<usize>>::new);
    let focused_option = hooks.use_state(|| 0usize);
    // 是否处于自定义文本输入模式
    let is_typing = hooks.use_state(|| false);
    // 自定义输入 textarea 状态（仅 typing 期间有效）
    let typing_state = hooks.use_state(TextAreaState::default);
    // 每个问题的自定义文本答案（与 answers 并行，互不冲突）
    let custom_answers = hooks.use_state(Vec::<Option<String>>::new);
    let session_fingerprint = hooks.use_state(Vec::<String>::new);

    let question_count = pending.as_ref().map(|q| q.questions.len()).unwrap_or(0);

    let current_fingerprint: Vec<String> = pending
        .as_ref()
        .map(|p| p.questions.iter().map(|q| q.id.clone()).collect())
        .unwrap_or_default();
    if *session_fingerprint.read() != current_fingerprint {
        *focused.write() = 0;
        *answers.write() = vec![vec![]; question_count];
        *focused_option.write() = 0;
        *is_typing.write() = false;
        *typing_state.write() = TextAreaState::default();
        *custom_answers.write() = vec![None; question_count];
        *session_fingerprint.write() = current_fingerprint;
    }

    let pending_for_closure = pending.clone();

    // ── 事件处理 ────────────────────────────────────────────────────────────
    hooks.use_event_handler(EventScope::Current, EventPriority::High, move |event| {
        let Event::Key(key) = event else {
            return EventResult::Ignored;
        };
        if key.kind != KeyEventKind::Press {
            return EventResult::Ignored;
        }

        // 如果当前有 popup 打开（如确认弹窗），让 popup 的 handler 处理事件
        if crate::kit::atoms::POPUP_KIND.state().read().is_some() {
            return EventResult::Ignored;
        }

        // ── Typing 模式：委托给 TextAreaState ──
        if *is_typing.read() {
            let mut st = typing_state.write();
            let consumed = match key.code {
                // Enter → 确认输入
                KeyCode::Enter if key.modifiers == KeyModifiers::NONE => {
                    let text = st.text.trim().to_string();
                    if !text.is_empty() {
                        let q_idx = *focused.read();
                        let mut ca = custom_answers.write();
                        if q_idx >= ca.len() {
                            ca.resize(q_idx + 1, None);
                        }
                        ca[q_idx] = Some(text);
                    }
                    *is_typing.write() = false;
                    true
                }
                // ESC → 取消输入
                KeyCode::Esc if key.modifiers == KeyModifiers::NONE => {
                    *is_typing.write() = false;
                    true
                }
                // Backspace
                KeyCode::Backspace if key.modifiers == KeyModifiers::NONE => {
                    st.backspace();
                    true
                }
                // Delete
                KeyCode::Delete if key.modifiers == KeyModifiers::NONE => {
                    st.delete_forward();
                    true
                }
                // Ctrl+W → 删词
                KeyCode::Char('w' | 'W') if key.modifiers == KeyModifiers::CONTROL => {
                    st.delete_word_backward();
                    true
                }
                // Ctrl+U → 清空行
                KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => {
                    st.clear();
                    true
                }
                // Ctrl+A → 行首
                KeyCode::Char('a') if key.modifiers == KeyModifiers::CONTROL => {
                    st.cursor_line_home();
                    true
                }
                // Ctrl+E → 行尾
                KeyCode::Char('e') if key.modifiers == KeyModifiers::CONTROL => {
                    st.cursor_line_end();
                    true
                }
                // 左箭头
                KeyCode::Left if key.modifiers == KeyModifiers::NONE => {
                    st.cursor_left();
                    true
                }
                KeyCode::Left if key.modifiers == KeyModifiers::CONTROL => {
                    st.cursor_word_left();
                    true
                }
                // 右箭头
                KeyCode::Right if key.modifiers == KeyModifiers::NONE => {
                    st.cursor_right();
                    true
                }
                KeyCode::Right if key.modifiers == KeyModifiers::CONTROL => {
                    st.cursor_word_right();
                    true
                }
                // 上/下箭头：视觉行移动；到顶时回到选项列表
                KeyCode::Up if key.modifiers == KeyModifiers::NONE => {
                    let moved = st.cursor_visual_up(wrap_width);
                    if !moved {
                        // 已在最顶：退出 typing，回到预设选项
                        *is_typing.write() = false;
                        let q = pending_for_closure
                            .as_ref()
                            .and_then(|au| au.questions.get(*focused.read()));
                        if let Some(q) = q {
                            *focused_option.write() = q.options.len().saturating_sub(1);
                        }
                    }
                    true
                }
                KeyCode::Down if key.modifiers == KeyModifiers::NONE => {
                    let _ = st.cursor_visual_down(wrap_width);
                    true
                }
                // Ctrl+Z → undo
                KeyCode::Char('z') if key.modifiers == KeyModifiers::CONTROL => {
                    st.undo();
                    true
                }
                // Ctrl+Shift+Z / Ctrl+Y → redo
                KeyCode::Char('Z') if key.modifiers == KeyModifiers::CONTROL => {
                    st.redo();
                    true
                }
                KeyCode::Char('y') if key.modifiers == KeyModifiers::CONTROL => {
                    st.redo();
                    true
                }
                // 可见字符插入
                KeyCode::Char(c)
                    if key.modifiers == KeyModifiers::NONE
                        || key.modifiers == KeyModifiers::SHIFT =>
                {
                    st.insert_char(c);
                    true
                }
                _ => false,
            };
            if consumed {
                return EventResult::Consumed;
            }
        }

        // ── 非 Typing 模式：选项导航 ──

        // Space：选中/取消当前高亮的选项（或手动进入 typing）
        if (key.modifiers, key.code) == (KeyModifiers::NONE, KeyCode::Char(' ')) {
            let q_idx = *focused.read();
            let opt_idx = *focused_option.read();
            // 自定义输入选项：手动进入 Typing 模式
            if let Some(au) = pending_for_closure.as_ref()
                && let Some(q) = au.questions.get(q_idx)
                && opt_idx == q.options.len()
            {
                let existing = custom_answers
                    .read()
                    .get(q_idx)
                    .cloned()
                    .flatten()
                    .unwrap_or_default();
                let mut ts = typing_state.write();
                ts.replace_all_no_undo(existing);
                ts.clear_undo_history();
                *is_typing.write() = true;
                return EventResult::Consumed;
            }
            if let Some(au) = pending_for_closure.as_ref()
                && let Some(q) = au.questions.get(q_idx)
                && opt_idx < q.options.len()
            {
                let mut a = answers.write();
                if q_idx >= a.len() {
                    a.resize(q_idx + 1, vec![]);
                }
                if q.multi_select {
                    if let Some(pos) = a[q_idx].iter().position(|&x| x == opt_idx) {
                        a[q_idx].remove(pos);
                    } else {
                        a[q_idx].push(opt_idx);
                    }
                } else {
                    a[q_idx] = if a[q_idx].first() == Some(&opt_idx) {
                        vec![]
                    } else {
                        vec![opt_idx]
                    };
                }
            }
            return EventResult::Consumed;
        }

        match classify_list_nav(&key) {
            Some(ListNavAction::MoveUp) => {
                if *is_typing.read() {
                    return EventResult::Consumed;
                }
                let mut fo = focused_option.write();
                *fo = previous_selection(*fo);
                EventResult::Consumed
            }
            Some(ListNavAction::MoveDown) => {
                if *is_typing.read() {
                    return EventResult::Consumed;
                }
                let limit = pending_for_closure
                    .as_ref()
                    .and_then(|au| au.questions.get(*focused.read()))
                    .map(|q| q.options.len() + 1)
                    .unwrap_or(0);
                if limit > 0 {
                    let mut fo = focused_option.write();
                    *fo = next_selection(*fo, limit);
                    // 到达自定义输入位置时自动激活 typing
                    if *fo == limit.saturating_sub(1) {
                        let q_idx = *focused.read();
                        let existing = pending_for_closure
                            .as_ref()
                            .and_then(|au| au.questions.get(q_idx))
                            .map(|_q| {
                                custom_answers
                                    .read()
                                    .get(q_idx)
                                    .cloned()
                                    .flatten()
                                    .unwrap_or_default()
                            })
                            .unwrap_or_default();
                        let mut ts = typing_state.write();
                        ts.replace_all_no_undo(existing);
                        ts.clear_undo_history();
                        *is_typing.write() = true;
                    }
                }
                EventResult::Consumed
            }

            Some(ListNavAction::CycleForward) if question_count > 0 => {
                if *is_typing.read() {
                    return EventResult::Consumed;
                }
                let mut f = focused.write();
                *f = cycle_next(*f, question_count);
                let mut fo = focused_option.write();
                *fo = answers
                    .read()
                    .get(*f)
                    .and_then(|v| v.first().copied())
                    .unwrap_or(0);
                EventResult::Consumed
            }
            Some(ListNavAction::CycleBackward) if question_count > 0 => {
                if *is_typing.read() {
                    return EventResult::Consumed;
                }
                let mut f = focused.write();
                *f = cycle_previous(*f, question_count);
                let mut fo = focused_option.write();
                *fo = answers
                    .read()
                    .get(*f)
                    .and_then(|v| v.first().copied())
                    .unwrap_or(0);
                EventResult::Consumed
            }
            Some(ListNavAction::Confirm) => {
                let q_idx = *focused.read();
                let all_answered = answers.read().iter().enumerate().all(|(i, a)| {
                    !a.is_empty()
                        || custom_answers
                            .read()
                            .get(i)
                            .map(|ca| ca.is_some())
                            .unwrap_or(false)
                        || pending_for_closure
                            .as_ref()
                            .and_then(|au| au.questions.get(i))
                            .map(|q| q.options.is_empty())
                            .unwrap_or(true)
                });
                if !all_answered {
                    let qc = question_count;
                    let mut next = (q_idx + 1) % qc;
                    loop {
                        let is_answered = answers
                            .read()
                            .get(next)
                            .map(|v| !v.is_empty())
                            .unwrap_or(false)
                            || custom_answers
                                .read()
                                .get(next)
                                .map(|ca| ca.is_some())
                                .unwrap_or(false);
                        let has_no_options = pending_for_closure
                            .as_ref()
                            .and_then(|au| au.questions.get(next))
                            .map(|q| q.options.is_empty())
                            .unwrap_or(true);
                        if !is_answered && !has_no_options {
                            break;
                        }
                        next = (next + 1) % qc;
                        if next == q_idx {
                            break;
                        }
                    }
                    *focused.write() = next;
                    *focused_option.write() = 0;
                    EventResult::Consumed
                } else {
                    let answers_snapshot = answers.read().clone();
                    let custom_snapshot = custom_answers.read().clone();
                    let answers_map = build_answers_map(
                        pending_for_closure.as_ref(),
                        &answers_snapshot,
                        &custom_snapshot,
                    );
                    if let Some(id_str) = ASK_USER_REQUEST_ID.state().read().clone()
                        && let Some(tx) = ASK_USER_RESPONSE_TX.get()
                    {
                        let _ = tx.send(AskUserResponseAction::Submit {
                            request_id_str: id_str,
                            answers: answers_map,
                        });
                    }
                    panel_registry::close_panel(PanelKind::AskUser);
                    *ASK_USER_PENDING.state().write() = None;
                    *ASK_USER_REQUEST_ID.state().write() = None;
                    EventResult::Consumed
                }
            }
            Some(ListNavAction::Cancel) => {
                // ESC → 打开确认弹窗而非直接取消
                let payload = crate::kit::atoms::ConfirmPayload {
                    title: i18n::tr("popup-confirm-reject-title"),
                    message: i18n::tr("popup-confirm-reject-message"),
                    details: vec![],
                    pending_action: crate::kit::atoms::ConfirmAction::RejectAskUser,
                };
                *crate::kit::atoms::CONFIRM_PAYLOAD.state().write() = Some(payload);
                crate::kit::popup_overlay::open_popup(crate::kit::atoms::PopupKind::Confirm);
                EventResult::Consumed
            }
            _ => EventResult::Ignored,
        }
    });

    // ── 渲染 ────────────────────────────────────────────────────────────────
    let popup_tokens = &theme_def.read().component.popup;
    let guard = theme_def.read();
    let semantic = &guard.semantic;
    let mut lines: Vec<Line<'_>> = Vec::new();

    match &pending {
        None => {
            lines.push(Line::from(""));
            lines.push(
                Line::from(i18n::tr("panel-ask-user-empty"))
                    .fg(semantic.text.muted)
                    .italic(),
            );
        }
        Some(au) if au.questions.is_empty() => {
            lines.push(Line::from(""));
            lines
                .push(Line::from(i18n::tr("panel-ask-user-malformed")).fg(semantic.status.warning));
        }
        Some(au) => {
            let focused_idx = (*focused.read()).min(au.questions.len() - 1);
            let answers_read = answers.read();
            let typing = *is_typing.read();

            // Tab 行：反色高亮当前 tab（accent 底色 + surface 字色），禁用 [ ]
            lines.push(Line::from(""));
            let tab_spans: Vec<ratatui::text::Span> = au
                .questions
                .iter()
                .enumerate()
                .flat_map(|(i, q)| {
                    let answered = answers_read.get(i).map(|v| !v.is_empty()).unwrap_or(false)
                        || custom_answers
                            .read()
                            .get(i)
                            .map(|ca| ca.is_some())
                            .unwrap_or(false);
                    let mark = if answered {
                        i18n::tr("panel-ask-user-answered-mark")
                    } else {
                        String::new()
                    };
                    let tab_key = format!("{}{}", q.header, mark);
                    let styled = if i == focused_idx {
                        ratatui::text::Span::styled(
                            format!(" {} ", tab_key),
                            Style::new()
                                .fg(semantic.surface.default)
                                .bg(popup_tokens.action_primary)
                                .add_modifier(Modifier::BOLD),
                        )
                    } else {
                        ratatui::text::Span::styled(
                            format!(" {} ", tab_key),
                            Style::new().fg(semantic.text.dim),
                        )
                    };
                    vec![styled, ratatui::text::Span::from(" ")]
                })
                .collect();
            lines.push(Line::from(tab_spans));
            lines.push(Line::from("─".repeat(wrap_width)).fg(semantic.border.default));

            if let Some(q) = au.questions.get(focused_idx) {
                lines.push(Line::from(""));
                let question_text = if q.question.is_empty() {
                    q.header.clone()
                } else {
                    format!("  {}", q.question)
                };
                for wrapped in wrap_text(&question_text, wrap_width) {
                    lines.push(Line::from(wrapped).fg(semantic.text.primary));
                }
                lines.push(Line::from(""));

                let has_custom_answer_current = custom_answers
                    .read()
                    .get(focused_idx)
                    .map(|ca| ca.is_some())
                    .unwrap_or(false);

                // 预设选项列表
                let selected_indices = answers_read.get(focused_idx).cloned().unwrap_or_default();
                let fopt = *focused_option.read();

                // Typing 模式下隐藏预设选项的选中状态
                for (opt_i, opt) in q.options.iter().enumerate() {
                    let is_selected = if typing || has_custom_answer_current {
                        false
                    } else {
                        selected_indices.contains(&opt_i)
                    };
                    let is_focused_opt = !typing && opt_i == fopt;
                    let mark = if is_selected {
                        if q.multi_select { "☑" } else { "●" }
                    } else if q.multi_select {
                        "☐"
                    } else {
                        "○"
                    };

                    let style = if is_selected {
                        Style::new().fg(popup_tokens.action_primary).bold()
                    } else if is_focused_opt {
                        Style::new()
                            .fg(popup_tokens.action_primary)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::new().fg(semantic.text.primary)
                    };

                    let label_line = format!("  {} {}", mark, opt.label);
                    for wrapped in wrap_text(&label_line, wrap_width) {
                        lines.push(Line::from(wrapped).style(style));
                    }
                    if !opt.description.is_empty() {
                        let desc_line = format!("    {}", opt.description);
                        for wrapped in wrap_text(&desc_line, wrap_width) {
                            lines.push(Line::from(wrapped).fg(semantic.text.dim));
                        }
                    }
                }

                // ── 自定义输入入口 ──
                let custom_option_index = q.options.len();
                let is_custom_focused = !typing && fopt == custom_option_index;

                if typing {
                    // Typing 模式：使用 TextArea 渲染
                    let st_read = typing_state.read();
                    let wrap = textarea_wrap(&st_read.text, st_read.cursor, wrap_width);
                    let total_rows = wrap.total_visual_rows.max(1);
                    let viewport = total_rows.min(TYPING_VIEWPORT_ROWS);

                    let cursor_style = Style::default()
                        .fg(Color::Reset)
                        .bg(popup_tokens.action_primary)
                        .add_modifier(Modifier::BOLD);
                    let placeholder_style = Style::default().fg(semantic.text.dim);
                    let default_style = Style::default().bg(Color::Reset);

                    let typed_lines = crate::components::textarea::render_multiline_with_cursor(
                        &st_read.text,
                        st_read.cursor,
                        cursor_style,
                        None,
                        cursor_style,
                        Some(&i18n::tr("ask-user-placeholder")),
                        placeholder_style,
                        default_style,
                        wrap_width,
                        viewport,
                        false,
                        true,
                    );
                    for line in typed_lines {
                        // 保留 textarea 返回的 Span 级样式（含光标高亮），仅前置缩进
                        let indent = ratatui::text::Span::from("    ");
                        let mut spans = vec![indent];
                        spans.extend(line.spans.iter().cloned());
                        lines.push(Line::from(spans));
                    }
                    let _ = st_read;
                } else if has_custom_answer_current {
                    // 已有自定义答案：显示为选中状态
                    let existing = custom_answers
                        .read()
                        .get(focused_idx)
                        .cloned()
                        .flatten()
                        .unwrap_or_default();
                    let mark = if q.multi_select { "☑" } else { "●" };
                    let custom_style = Style::new().fg(popup_tokens.action_primary).bold();
                    for wrapped in wrap_text(&existing, wrap_width.saturating_sub(4)) {
                        lines.push(
                            Line::from(format!("    {} {}", mark, wrapped)).style(custom_style),
                        );
                    }
                } else {
                    // 未输入：显示占位提示
                    let custom_style = if is_custom_focused {
                        Style::new()
                            .fg(popup_tokens.action_primary)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::new().fg(semantic.text.dim)
                    };
                    lines.push(
                        Line::from(format!("    {}", i18n::tr("ask-user-placeholder")))
                            .style(custom_style),
                    );
                }

                if q.options.is_empty() {
                    lines.push(
                        Line::from(i18n::tr("panel-ask-user-no-options")).fg(semantic.text.dim),
                    );
                }
            }

            lines.push(Line::from(""));
            // 提示行：根据当前模式选择文本
            if typing {
                lines
                    .push(Line::from(i18n::tr("panel-ask-user-hint-typing")).fg(semantic.text.dim));
            } else {
                let is_multi_select = au
                    .questions
                    .get(focused_idx)
                    .map(|q| q.multi_select)
                    .unwrap_or(false);
                if au.questions.len() > 1 {
                    let all_answered = answers_read.iter().enumerate().all(|(i, a)| {
                        !a.is_empty()
                            || custom_answers
                                .read()
                                .get(i)
                                .map(|ca| ca.is_some())
                                .unwrap_or(false)
                            || au
                                .questions
                                .get(i)
                                .map(|q| q.options.is_empty())
                                .unwrap_or(true)
                    });
                    let key = if all_answered {
                        if is_multi_select {
                            "panel-ask-user-hint-tab-multi-select-answered"
                        } else {
                            "panel-ask-user-hint-tab-multi-answered"
                        }
                    } else {
                        if is_multi_select {
                            "panel-ask-user-hint-tab-multi-select-unanswered"
                        } else {
                            "panel-ask-user-hint-tab-multi-unanswered"
                        }
                    };
                    lines.push(Line::from(i18n::tr(key)).fg(semantic.text.dim));
                } else {
                    let is_answered = answers_read.first().map(|v| !v.is_empty()).unwrap_or(false)
                        || custom_answers
                            .read()
                            .first()
                            .map(|ca| ca.is_some())
                            .unwrap_or(false)
                        || au
                            .questions
                            .first()
                            .map(|q| q.options.is_empty())
                            .unwrap_or(true);
                    let key = if is_answered {
                        if is_multi_select {
                            "panel-ask-user-hint-single-multi-select-answered"
                        } else {
                            "panel-ask-user-hint-single-answered"
                        }
                    } else {
                        if is_multi_select {
                            "panel-ask-user-hint-single-multi-select-unanswered"
                        } else {
                            "panel-ask-user-hint-single-unanswered"
                        }
                    };
                    lines.push(Line::from(i18n::tr(key)).fg(semantic.text.dim));
                }
            }
        }
    }

    panel_shell!(PanelKind::AskUser, {
        element!(
            ScrollView(
                scrollbars: panel_registry::clean_scrollbars(),
                width: Constraint::Fill(1),
                height: Constraint::Fill(1),
            ) {
                Text(text: ratatui_kit::ratatui::text::Text::from(lines))
            }
        )
    })
}

/// CJK 安全的文本折行：按 max_width 列宽拆分文本为多行。
/// 优先在空白字符处断行，其次在字符边界处断开。
fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![text.to_string()];
    }
    if text.width() <= max_width {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    let mut byte_pos = 0;
    while byte_pos < text.len() {
        let mut cur_width = 0usize;
        let mut content_end = byte_pos;
        for (i, c) in text[byte_pos..].char_indices() {
            let cw = c.width().unwrap_or(0);
            if content_end > byte_pos && cur_width + cw > max_width {
                break;
            }
            cur_width += cw;
            content_end = byte_pos + i + c.len_utf8();
        }
        // 优先在空白字符处断行
        let mut break_at = content_end;
        for (i, c) in text[byte_pos..content_end].char_indices().rev() {
            if c.is_whitespace() {
                break_at = byte_pos + i;
                break;
            }
        }
        if break_at <= byte_pos {
            break_at = content_end;
        }
        let segment = text[byte_pos..break_at].trim();
        if !segment.is_empty() {
            lines.push(segment.to_string());
        }
        byte_pos = break_at;
        // 跳过连续空白
        while byte_pos < text.len()
            && text[byte_pos..]
                .chars()
                .next()
                .map(|c| c.is_whitespace())
                .unwrap_or(false)
        {
            byte_pos += text[byte_pos..]
                .chars()
                .next()
                .map(|c| c.len_utf8())
                .unwrap_or(0);
        }
    }
    if lines.is_empty() {
        vec![text.to_string()]
    } else {
        lines
    }
}

fn build_answers_map(
    pending: Option<&AskUser>,
    answers: &[Vec<usize>],
    custom_answers: &[Option<String>],
) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    if let Some(au) = pending {
        for (i, q) in au.questions.iter().enumerate() {
            let custom = custom_answers.get(i).cloned().flatten();
            let selected: Vec<usize> = answers.get(i).cloned().unwrap_or_default();
            let val = if q.multi_select {
                // 多选：合并预设选项 labels + 自定义文本
                let mut labels: Vec<serde_json::Value> = selected
                    .iter()
                    .filter_map(|idx| q.options.get(*idx).map(|opt| json!(opt.label)))
                    .collect();
                if let Some(custom_text) = custom
                    && !custom_text.is_empty()
                {
                    labels.push(json!(custom_text));
                }
                if labels.is_empty() {
                    json!([])
                } else {
                    json!(labels)
                }
            } else if let Some(custom_text) = custom {
                // 单选：自定义文本优先
                json!(custom_text)
            } else {
                // 单选：仅预设选项
                selected
                    .first()
                    .and_then(|idx| q.options.get(*idx).map(|opt| json!(opt.label)))
                    .unwrap_or(json!(""))
            };
            map.insert(q.id.clone(), val);
        }
    }
    serde_json::Value::Object(map)
}

#[cfg(test)]
#[path = "ask_user_test.rs"]
mod tests;
