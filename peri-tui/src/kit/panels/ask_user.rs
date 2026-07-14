//! ratatui-kit AskUserPanel component.
//!
//! 用户问答面板——当 agent 调用 AskUserQuestion 工具时，自动作为 Panel 内联渲染
//! 在 MessageArea 和 InputArea 之间（替代弹窗形式）。
//!
//! 面板逻辑复用 ask_user_popup 的 Tab 交互模型，但通过 panel_shell! 渲染。

use crate::app::panel_types::PanelKind;
use crate::i18n;
use peri_acp_types::event_data::AskUser;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers},
    prelude::*,
    ratatui::{
        layout::Constraint,
        style::{Style, Stylize},
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

/// 面板交互模式：选项导航 vs 文本输入
#[derive(Clone, Debug, PartialEq)]
enum InputMode {
    /// 正在选项列表中导航（默认模式）
    Selecting,
    /// 正在输入自定义文本；携带当前输入的文本 buffer
    Typing { buffer: String },
}

#[component]
pub fn AskUserPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let pending_store = hooks.use_atom(&ASK_USER_PENDING);
    let pending: Option<AskUser> = pending_store.read().clone();
    let _ = pending_store;
    let _ = hooks.use_atom(&LANG_VERSION);

    let focused = hooks.use_state(|| 0usize);
    let answers = hooks.use_state(Vec::<Vec<usize>>::new);
    let focused_option = hooks.use_state(|| 0usize);
    let input_mode = hooks.use_state(|| InputMode::Selecting);
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
        *input_mode.write() = InputMode::Selecting;
        *custom_answers.write() = vec![None; question_count];
        *session_fingerprint.write() = current_fingerprint;
    }

    #[allow(dead_code)]
    fn cancel_ask_user() {
        if let Some(id_str) = ASK_USER_REQUEST_ID.state().read().clone()
            && let Some(tx) = ASK_USER_RESPONSE_TX.get()
        {
            let _ = tx.send(AskUserResponseAction::Cancel {
                request_id_str: id_str,
            });
        }
        panel_registry::close_panel(PanelKind::AskUser);
        *ASK_USER_PENDING.state().write() = None;
        *ASK_USER_REQUEST_ID.state().write() = None;
    }

    let pending_for_closure = pending.clone();

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

        // Typing 模式：捕获所有按键用于文本编辑
        if let InputMode::Typing { ref buffer } = *input_mode.read() {
            let mut buf = buffer.clone();
            let mut consumed = true;
            match (key.modifiers, key.code) {
                // Enter → 确认输入，保存到 custom_answers
                (KeyModifiers::NONE, KeyCode::Enter) => {
                    if !buf.trim().is_empty() {
                        let q_idx = *focused.read();
                        let mut ca = custom_answers.write();
                        if q_idx >= ca.len() {
                            ca.resize(q_idx + 1, None);
                        }
                        ca[q_idx] = Some(buf.trim().to_string());
                    }
                    *input_mode.write() = InputMode::Selecting;
                }
                // ESC → 取消输入，丢弃 buffer
                (KeyModifiers::NONE, KeyCode::Esc) => {
                    *input_mode.write() = InputMode::Selecting;
                }
                // Backspace → 删除最后一个字符
                (KeyModifiers::NONE, KeyCode::Backspace) => {
                    buf.pop();
                    *input_mode.write() = InputMode::Typing { buffer: buf };
                }
                // Ctrl+W → 删除最后一个词
                (KeyModifiers::CONTROL, KeyCode::Char('w'))
                | (KeyModifiers::CONTROL, KeyCode::Char('W')) => {
                    if let Some(pos) = buf.rfind(char::is_whitespace) {
                        buf.truncate(pos);
                    } else {
                        buf.clear();
                    }
                    *input_mode.write() = InputMode::Typing { buffer: buf };
                }
                // 可打印字符 → 追加
                (KeyModifiers::NONE, KeyCode::Char(c)) => {
                    buf.push(c);
                    *input_mode.write() = InputMode::Typing { buffer: buf };
                }
                (KeyModifiers::SHIFT, KeyCode::Char(c)) => {
                    buf.push(c);
                    *input_mode.write() = InputMode::Typing { buffer: buf };
                }
                _ => {
                    consumed = false;
                }
            }
            if consumed {
                return EventResult::Consumed;
            }
        }

        // Space：选中/取消当前高亮的选项（多选时 toggle 入 vec，单选时替换）
        if (key.modifiers, key.code) == (KeyModifiers::NONE, KeyCode::Char(' ')) {
            let q_idx = *focused.read();
            let opt_idx = *focused_option.read();
            // 自定义输入选项：进入 Typing 模式
            if let Some(au) = pending_for_closure.as_ref()
                && let Some(q) = au.questions.get(q_idx)
                && opt_idx == q.options.len()
            {
                *input_mode.write() = InputMode::Typing {
                    buffer: custom_answers
                        .read()
                        .get(q_idx)
                        .cloned()
                        .flatten()
                        .unwrap_or_default(),
                };
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
                    // 多选：toggle 选项索引在选中列表中
                    if let Some(pos) = a[q_idx].iter().position(|&x| x == opt_idx) {
                        a[q_idx].remove(pos);
                    } else {
                        a[q_idx].push(opt_idx);
                    }
                } else {
                    // 单选：替换为当前选项（再次按 Space 取消选中）
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
                if matches!(*input_mode.read(), InputMode::Typing { .. }) {
                    return EventResult::Consumed;
                }
                let mut fo = focused_option.write();
                *fo = previous_selection(*fo);
                EventResult::Consumed
            }
            Some(ListNavAction::MoveDown) => {
                if matches!(*input_mode.read(), InputMode::Typing { .. }) {
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
                }
                EventResult::Consumed
            }
            Some(ListNavAction::CycleForward) if question_count > 0 => {
                if matches!(*input_mode.read(), InputMode::Typing { .. }) {
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
                if matches!(*input_mode.read(), InputMode::Typing { .. }) {
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

            // Tab 行：已答表示 ✓，当前用 [header]
            lines.push(Line::from(""));
            let tab_line = au
                .questions
                .iter()
                .enumerate()
                .map(|(i, q)| {
                    let answered = answers_read.get(i).map(|v| !v.is_empty()).unwrap_or(false);
                    let mark = if answered {
                        i18n::tr("panel-ask-user-answered-mark")
                    } else {
                        String::new()
                    };
                    if i == focused_idx {
                        format!("[{}]{}", q.header, mark)
                    } else {
                        format!(" {} {}", q.header, mark)
                    }
                })
                .collect::<Vec<_>>()
                .join("  ");
            lines.push(Line::from(format!("  {}", tab_line)).fg(semantic.text.dim));
            lines.push(Line::from("─".repeat(60)).fg(semantic.border.default));

            if let Some(q) = au.questions.get(focused_idx) {
                lines.push(Line::from(""));
                let question_text = if q.question.is_empty() {
                    q.header.clone()
                } else {
                    format!("  {}", q.question)
                };
                // 超长问题文本折行（CJK 安全）
                for wrapped in wrap_text(&question_text, 80) {
                    lines.push(Line::from(wrapped).fg(semantic.text.primary));
                }
                lines.push(Line::from(""));

                let selected_indices = answers_read.get(focused_idx).cloned().unwrap_or_default();
                let fopt = *focused_option.read();
                for (opt_i, opt) in q.options.iter().enumerate() {
                    let typing = matches!(*input_mode.read(), InputMode::Typing { .. });
                    let has_custom_answer_current = custom_answers
                        .read()
                        .get(focused_idx)
                        .map(|ca| ca.is_some())
                        .unwrap_or(false);
                    let is_selected = if typing || has_custom_answer_current {
                        false
                    } else {
                        selected_indices.contains(&opt_i)
                    };
                    let is_focused_opt = opt_i == fopt;
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
                        Style::new().fg(popup_tokens.selected_fg)
                    } else {
                        Style::new().fg(semantic.text.primary)
                    };

                    // 选项 label 超长折行
                    let label_line = format!("  {} {}", mark, opt.label);
                    for wrapped in wrap_text(&label_line, 80) {
                        lines.push(Line::from(wrapped).style(style));
                    }
                    if !opt.description.is_empty() {
                        let desc_line = format!("    {}", opt.description);
                        for wrapped in wrap_text(&desc_line, 80) {
                            lines.push(Line::from(wrapped).fg(semantic.text.dim));
                        }
                    }
                }

                // ── 自定义输入入口（附加在预设选项之后） ──
                let custom_option_index = q.options.len();
                let has_custom_answer = custom_answers
                    .read()
                    .get(focused_idx)
                    .map(|ca| ca.is_some())
                    .unwrap_or(false);
                let is_custom_focused = fopt == custom_option_index;

                let (custom_mark, custom_text) =
                    if matches!(*input_mode.read(), InputMode::Typing { .. }) {
                        let buf = match &*input_mode.read() {
                            InputMode::Typing { buffer } => {
                                if buffer.is_empty() {
                                    "|".to_string()
                                } else {
                                    format!("{}|", buffer)
                                }
                            }
                            _ => "|".to_string(),
                        };
                        ("✎".to_string(), format!("  {}", buf))
                    } else if has_custom_answer {
                        let existing = custom_answers
                            .read()
                            .get(focused_idx)
                            .cloned()
                            .flatten()
                            .unwrap_or_default();
                        let mark = if q.multi_select { "☑" } else { "●" };
                        (mark.to_string(), format!("  {}", existing))
                    } else {
                        (
                            "✎".to_string(),
                            format!("  {}", i18n::tr("ask-user-placeholder")),
                        )
                    };

                let custom_style = if has_custom_answer {
                    Style::new().fg(popup_tokens.action_primary).bold()
                } else if is_custom_focused {
                    Style::new().fg(popup_tokens.selected_fg)
                } else {
                    Style::new().fg(semantic.text.dim)
                };

                let custom_label_line = format!("  {} {}", custom_mark, custom_text);
                for wrapped in wrap_text(&custom_label_line, 80) {
                    lines.push(Line::from(wrapped).style(custom_style));
                }

                if q.options.is_empty() {
                    lines.push(
                        Line::from(i18n::tr("panel-ask-user-no-options")).fg(semantic.text.dim),
                    );
                }
            }

            lines.push(Line::from(""));
            // 确定当前问题的多选模式，选择对应的提示文本
            let is_multi_select = au
                .questions
                .get(focused_idx)
                .map(|q| q.multi_select)
                .unwrap_or(false);
            if matches!(*input_mode.read(), InputMode::Typing { .. }) {
                lines
                    .push(Line::from(i18n::tr("panel-ask-user-hint-typing")).fg(semantic.text.dim));
            } else if au.questions.len() > 1 {
                let all_answered = answers_read.iter().enumerate().all(|(i, a)| {
                    !a.is_empty()
                        || au
                            .questions
                            .get(i)
                            .map(|q| q.options.is_empty())
                            .unwrap_or(true)
                });
                if all_answered {
                    let key = if is_multi_select {
                        "panel-ask-user-hint-tab-multi-select-answered"
                    } else {
                        "panel-ask-user-hint-tab-multi-answered"
                    };
                    lines.push(Line::from(i18n::tr(key)).fg(semantic.text.dim));
                } else {
                    let key = if is_multi_select {
                        "panel-ask-user-hint-tab-multi-select-unanswered"
                    } else {
                        "panel-ask-user-hint-tab-multi-unanswered"
                    };
                    lines.push(Line::from(i18n::tr(key)).fg(semantic.text.dim));
                }
            } else {
                let is_answered = answers_read.first().map(|v| !v.is_empty()).unwrap_or(false)
                    || au
                        .questions
                        .first()
                        .map(|q| q.options.is_empty())
                        .unwrap_or(true);
                if is_answered {
                    let key = if is_multi_select {
                        "panel-ask-user-hint-single-multi-select-answered"
                    } else {
                        "panel-ask-user-hint-single-answered"
                    };
                    lines.push(Line::from(i18n::tr(key)).fg(semantic.text.dim));
                } else {
                    let key = if is_multi_select {
                        "panel-ask-user-hint-single-multi-select-unanswered"
                    } else {
                        "panel-ask-user-hint-single-unanswered"
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
            let val = if let Some(custom_text) = custom {
                json!(custom_text)
            } else if q.multi_select {
                // 多选：返回 label 数组
                let labels: Vec<serde_json::Value> = selected
                    .iter()
                    .filter_map(|idx| q.options.get(*idx).map(|opt| json!(opt.label)))
                    .collect();
                json!(labels)
            } else {
                // 单选：返回单个 label
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
