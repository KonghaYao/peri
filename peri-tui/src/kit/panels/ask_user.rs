//! ratatui-kit AskUserPanel component.
//!
//! 用户问答面板——当 agent 调用 AskUserQuestion 工具时，自动作为 Panel 内联渲染
//! 在 MessageArea 和 InputArea 之间（替代弹窗形式）。
//!
//! 面板逻辑复用 ask_user_popup 的 Tab 交互模型，但通过 panel_shell! 渲染。

use crate::app::panel_types::PanelKind;
use crate::components::textarea::wrap_text as textarea_wrap;
use crate::i18n;
use peri_acp_types::event_data::AskUser;
use ratatui_kit::{
    crossterm::event::{Event, KeyEventKind, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{
        layout::Constraint,
        style::{Color, Modifier, Style, Stylize},
        text::Line,
    },
};

use crate::kit::acp_types::PendingInteraction;
use crate::kit::ask_user_action::AskUserResponseAction;
use crate::kit::atoms::{ASK_USER_PENDING, ASK_USER_RESPONSE_TX, LANG_VERSION};
use crate::kit::panel_mouse::{AreaTracker, is_scrollbar_column};
use crate::kit::panel_registry;
use peri_theme::atoms::THEME_ATOM;
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

/// 自定义文本输入的视口行数上限
const TYPING_VIEWPORT_ROWS: usize = 3;

mod form;
mod typing;

#[cfg(test)]
use form::build_answers_map;
use form::{FormOutcome, FormState};

/// The scroll owner stays with the panel; reset it only alongside a new form owner.
fn reset_for_owner_change(
    form: &mut FormState,
    interaction: Option<&PendingInteraction<AskUser>>,
    scroll: &mut ScrollViewState,
) -> bool {
    if !form.reset_for_owner_change(interaction) {
        return false;
    }
    *scroll = ScrollViewState::default();
    true
}

#[component]
pub fn AskUserPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let pending_store = hooks.use_atom(&ASK_USER_PENDING);
    // 外部滚动状态——面板滚轮仲裁（panel_scroll.rs）驱动，统一 3 行/格 + 节流
    let sv = hooks.use_state(ScrollViewState::default);
    let interaction = pending_store.read().clone();
    let pending: Option<AskUser> = interaction.as_ref().map(|p| p.payload.clone());
    let _ = pending_store;
    let _ = hooks.use_atom(&LANG_VERSION);
    // 动态换行宽度：跟随终端实际宽度，避免宽终端下内容被压缩在 80 列内
    let (term_w, _) = hooks.use_terminal_size();
    let wrap_width = if term_w > 0 {
        (term_w as usize).saturating_sub(2).max(40)
    } else {
        80
    };

    let form = hooks.use_state(FormState::default);
    // Derived local state is reset without publishing from render.
    reset_for_owner_change(
        &mut form.write_no_update(),
        interaction.as_ref(),
        &mut sv.write_no_update(),
    );

    let pending_for_closure = pending.clone();
    let interaction_for_closure = interaction.clone();

    // 面板绘制区域（上一帧）——鼠标点击行号反推
    let previous_size = hooks.use_previous_size();
    let area;
    {
        let tracker = hooks.use_hook(AreaTracker::new);
        area = tracker.rect;
    }

    // ── 事件处理 ────────────────────────────────────────────────────────────
    hooks.use_event_handler_with_options(
        EventScope::Current,
        EventPriority::High,
        EventOptions { hit_test: true },
        move |event| {
            // 鼠标：区域内左键点击 = 激活对应行（选项=选中，自定义输入=进入 typing）。
            // 选项区行高动态（wrap 折行），与渲染同构计算行分布。
            if let Event::Mouse(mouse) = event {
                if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
                    return EventResult::Ignored;
                }
                let Some(area) = area else {
                    return EventResult::Ignored;
                };
                // 如果当前有 popup 打开（如确认弹窗），让 popup 的 handler 处理事件
                if crate::kit::atoms::POPUP_KIND.state().read().is_some() {
                    return EventResult::Ignored;
                }
                // 顶部/底部边框行与滚动条列不命中
                let row = mouse.row;
                if row <= area.y || row >= area.y + area.height.saturating_sub(1) {
                    return EventResult::Consumed;
                }
                if is_scrollbar_column(&mouse, area) {
                    return EventResult::Consumed;
                }
                // Typing 模式：点击仅消费（光标定位不在本次范围）
                if form.read().is_typing {
                    return EventResult::Consumed;
                }
                let visual = row - area.y - 1;
                // Release the shared form guard before a mouse action writes it.
                let focused = form.read().focused;
                if let Some(au) = pending_for_closure.as_ref()
                    && !au.questions.is_empty()
                    && let Some(q) = au.questions.get(focused.min(au.questions.len() - 1))
                {
                    let q_idx = focused.min(au.questions.len() - 1);
                    // 行分布：0 空行 / 1 Tab / 2 分隔线 / 3 空行 / 问题 wrap 行 / 空行 / 选项区 / 自定义输入区
                    let mut cur = 4u16;
                    let question_text = if q.question.is_empty() {
                        q.header.clone()
                    } else {
                        format!("  {}", q.question)
                    };
                    cur += wrap_text(&question_text, wrap_width).len() as u16;
                    cur += 1; // 问题与选项间的空行

                    for (opt_i, opt) in q.options.iter().enumerate() {
                        let label_rows =
                            wrap_text(&format!("  ○ {}", opt.label), wrap_width).len() as u16;
                        let desc_rows = if opt.description.is_empty() {
                            0
                        } else {
                            wrap_text(&format!("    {}", opt.description), wrap_width).len() as u16
                        };
                        if visual >= cur && visual < cur + label_rows + desc_rows {
                            // Space 语义：选中/取消该选项
                            form.write().toggle_option(q_idx, opt_i, q.multi_select);
                            return EventResult::Consumed;
                        }
                        cur += label_rows + desc_rows;
                    }

                    // 自定义输入区
                    let custom_rows = {
                        let has_custom = form
                            .read()
                            .custom_answers
                            .get(q_idx)
                            .map(|ca| ca.is_some())
                            .unwrap_or(false);
                        if has_custom {
                            let existing = form
                                .read()
                                .custom_answers
                                .get(q_idx)
                                .cloned()
                                .flatten()
                                .unwrap_or_default();
                            wrap_text(&existing, wrap_width.saturating_sub(4)).len() as u16
                        } else {
                            1
                        }
                    };
                    if visual >= cur && visual < cur + custom_rows {
                        // Space 在自定义选项的语义：进入 typing
                        form.write().begin_custom_input(q_idx);
                        return EventResult::Consumed;
                    }
                }
                // 区域内点击（未命中行）也消费，防止穿透
                return EventResult::Consumed;
            }
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

            if !form.read().accepts_key(&key, pending_for_closure.as_ref()) {
                return EventResult::Ignored;
            }
            let outcome = form
                .write()
                .handle_key(&key, pending_for_closure.as_ref(), wrap_width);
            match outcome {
                FormOutcome::Ignored => EventResult::Ignored,
                FormOutcome::Consumed => EventResult::Consumed,
                FormOutcome::Submit(answers) => {
                    if let Some(snapshot) = interaction_for_closure.as_ref()
                        && let Some(tx) = ASK_USER_RESPONSE_TX.get()
                    {
                        let _ = tx.send(AskUserResponseAction::Submit {
                            owner: snapshot.owner.clone(),
                            request_id_str: snapshot.request_id_json.clone(),
                            answers,
                        });
                        panel_registry::close_ask_user_panel_for_owner(&snapshot.owner);
                    }
                    EventResult::Consumed
                }
                FormOutcome::RequestCancel => {
                    // ESC requests confirmation; the popup retains the request owner.
                    let Some(snapshot) = interaction_for_closure.as_ref() else {
                        return EventResult::Consumed;
                    };
                    let payload = crate::kit::atoms::ConfirmPayload {
                        title: i18n::tr("popup-confirm-reject-title"),
                        message: i18n::tr("popup-confirm-reject-message"),
                        details: vec![],
                        pending_action: crate::kit::atoms::ConfirmAction::RejectAskUser {
                            owner: snapshot.owner.clone(),
                            request_id_json: snapshot.request_id_json.clone(),
                        },
                    };
                    *crate::kit::atoms::CONFIRM_PAYLOAD.state().write() = Some(payload);
                    crate::kit::popup_overlay::open_popup(crate::kit::atoms::PopupKind::Confirm);
                    EventResult::Consumed
                }
            }
        },
    );

    // ── 渲染 ────────────────────────────────────────────────────────────────
    let form_read = form.read();
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
            let focused_idx = (form_read.focused).min(au.questions.len() - 1);
            let answers_read = &form_read.answers;
            let typing = form_read.is_typing;

            // Tab 行：反色高亮当前 tab（accent 底色 + surface 字色），禁用 [ ]
            lines.push(Line::from(""));
            let tab_spans: Vec<ratatui::text::Span> = au
                .questions
                .iter()
                .enumerate()
                .flat_map(|(i, q)| {
                    let answered = answers_read.get(i).map(|v| !v.is_empty()).unwrap_or(false)
                        || form_read
                            .custom_answers
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

                let has_custom_answer_current = form_read
                    .custom_answers
                    .get(focused_idx)
                    .map(|ca| ca.is_some())
                    .unwrap_or(false);

                // 预设选项列表
                let selected_indices = answers_read.get(focused_idx).cloned().unwrap_or_default();
                let fopt = form_read.focused_option;

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
                    let st_read = &form_read.typing_state;
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
                } else if has_custom_answer_current {
                    // 已有自定义答案：显示为选中状态
                    let existing = form_read
                        .custom_answers
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
                            || form_read
                                .custom_answers
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
                        || form_read
                            .custom_answers
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

    // 面板滚轮仲裁注册（每帧覆盖写入，area 用上一帧组件区域）
    crate::kit::panel_scroll::register_panel_scroll(PanelKind::AskUser, previous_size, sv);

    panel_shell!(PanelKind::AskUser, {
        element!(
            ScrollView(
                scrollbars: panel_registry::clean_scrollbars(),
                state: Some(sv),
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

#[cfg(test)]
#[path = "ask_user_test.rs"]
mod tests;
