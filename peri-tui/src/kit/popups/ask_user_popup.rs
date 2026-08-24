//! ratatui-kit AskUserPopup component.
//!
//! 用户问答弹窗：从 `ASK_USER_PENDING` atom 读取真实问题列表（id/header/
//! question/options/multi_select）。Tab 键在问题间切换（tab 风格），↑/↓ 导航选项，
//! Space 选择/取消，Enter 提交，Esc 取消。
//!
//! 对齐 spec/global/domains/tui/tui-popups.md §7.2 AskUser Popup 规范：
//! - 只使用上下边框（popup_text_shell! TOP | BOTTOM）
//! - 多问题时顶部 tab 行：`[header]` 标记当前，` header ` 标记其他
//! - ●/○ 单选项（单选）/ ☑/☐ 多选项
//! - Tab::next-question · ↑/↓·选项 · Space::select · Enter::submit · Esc::cancel

use peri_acp_types::event_data::AskUser;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers},
    prelude::*,
    ratatui::{
        style::{Style, Stylize},
        text::Line,
    },
};

use crate::i18n;
use crate::kit::ask_user_action::AskUserResponseAction;
use crate::kit::atoms::{ASK_USER_PENDING, ASK_USER_RESPONSE_TX, LANG_VERSION};
use crate::kit::list_nav::{
    ListNavAction, classify_list_nav, cycle_next, cycle_previous, next_selection,
    previous_selection,
};
use crate::kit::popup_overlay::close_ask_user_popup_for_request;
use peri_theme::atoms::THEME_ATOM;
use serde_json::json;

#[component]
pub fn AskUserPopup(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let pending_store = hooks.use_atom(&ASK_USER_PENDING);
    let pending = pending_store.read().clone();
    let _ = pending_store;

    // 当前选中的问题 tab 索引
    let focused = hooks.use_state(|| 0usize);
    // 每个问题的当前选中 option index（单选语义；multi_select 暂按单选处理）
    let answers = hooks.use_state(Vec::<Option<usize>>::new);
    // 当前问题内的高亮选项索引（用于 ↑/↓ 浏览 + Space 选中），None = 未浏览过
    let focused_option = hooks.use_state(|| 0usize);
    // session 指纹——检测 payload 变化时复位状态
    let session_fingerprint = hooks.use_state(Vec::<String>::new);

    let question_count = pending
        .as_ref()
        .map(|p| p.payload.questions.len())
        .unwrap_or(0);

    // 检测 payload 变化——question id 列表不同则视为新 session
    let current_fingerprint: Vec<String> = pending
        .as_ref()
        .map(|p| p.payload.questions.iter().map(|q| q.id.clone()).collect())
        .unwrap_or_default();
    if *session_fingerprint.read() != current_fingerprint {
        *focused.write() = 0;
        *answers.write() = vec![None; question_count];
        *focused_option.write() = 0;
        *session_fingerprint.write() = current_fingerprint;
    }

    let pending_for_closure = pending.clone();

    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, move |event| {
        let Event::Key(key) = event else {
            return EventResult::Ignored;
        };
        if key.kind != KeyEventKind::Press {
            return EventResult::Ignored;
        }

        // ── Space：选中/取消当前高亮的选项 ──
        if (key.modifiers, key.code) == (KeyModifiers::NONE, KeyCode::Char(' ')) {
            let q_idx = *focused.read();
            let opt_idx = *focused_option.read();
            if let Some(au) = pending_for_closure.as_ref()
                && let Some(q) = au.payload.questions.get(q_idx)
                && opt_idx < q.options.len()
            {
                let new_val = if answers.read().get(q_idx).copied().flatten() == Some(opt_idx) {
                    None // 取消选择
                } else {
                    Some(opt_idx) // 选中
                };
                let mut a = answers.write();
                if q_idx >= a.len() {
                    a.resize(q_idx + 1, None);
                }
                a[q_idx] = new_val;
            }
            return EventResult::Consumed;
        }

        match classify_list_nav(&key) {
            // ↑/↓：在当前问题的选项列表中移动高亮
            Some(ListNavAction::MoveUp) => {
                let mut fo = focused_option.write();
                *fo = previous_selection(*fo);
                EventResult::Consumed
            }
            Some(ListNavAction::MoveDown) => {
                let limit = pending_for_closure
                    .as_ref()
                    .and_then(|au| au.payload.questions.get(*focused.read()))
                    .map(|q| q.options.len())
                    .unwrap_or(0);
                if limit > 0 {
                    let mut fo = focused_option.write();
                    *fo = next_selection(*fo, limit);
                }
                EventResult::Consumed
            }
            // Tab / Shift+Tab：切换到下一个/上一个问题 tab
            Some(ListNavAction::CycleForward) if question_count > 0 => {
                let mut f = focused.write();
                let old = *f;
                *f = cycle_next(*f, question_count);
                // 切换 tab 时复位高亮选项
                let mut fo = focused_option.write();
                *fo = answers.read().get(*f).copied().flatten().unwrap_or(0);
                // 需要把旧 tab 的选项数量传进来以便 bounded read
                let _ = old;
                EventResult::Consumed
            }
            Some(ListNavAction::CycleBackward) if question_count > 0 => {
                let mut f = focused.write();
                *f = cycle_previous(*f, question_count);
                let mut fo = focused_option.write();
                *fo = answers.read().get(*f).copied().flatten().unwrap_or(0);
                EventResult::Consumed
            }
            // Enter：如果还有未回答的问题 → 跳转到下一个；全部已回答 → 提交
            Some(ListNavAction::Confirm) => {
                let q_idx = *focused.read();
                let all_answered = answers.read().iter().enumerate().all(|(i, a)| {
                    a.is_some()
                        || pending_for_closure
                            .as_ref()
                            .and_then(|au| au.payload.questions.get(i))
                            .map(|q| q.options.is_empty())
                            .unwrap_or(true)
                });
                if !all_answered {
                    // 找下一个未回答的问题
                    let qc = question_count;
                    let mut next = (q_idx + 1) % qc;
                    loop {
                        let is_answered = answers.read().get(next).copied().flatten().is_some();
                        let has_no_options = pending_for_closure
                            .as_ref()
                            .and_then(|au| au.payload.questions.get(next))
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
                    // 全部已确认 → 提交
                    let answers_map = build_answers_map(
                        pending_for_closure.as_ref().map(|p| &p.payload),
                        &answers.read(),
                    );
                    if let Some(snapshot) = pending_for_closure.as_ref()
                        && let Some(tx) = ASK_USER_RESPONSE_TX.get()
                    {
                        let _ = tx.send(AskUserResponseAction::Submit {
                            request_id_str: snapshot.request_id_json.clone(),
                            answers: answers_map,
                        });
                        close_ask_user_popup_for_request(&snapshot.request_id_json);
                    }
                    EventResult::Consumed
                }
            }
            // Esc：取消
            Some(ListNavAction::Cancel) => {
                if let Some(snapshot) = pending_for_closure.as_ref()
                    && let Some(tx) = ASK_USER_RESPONSE_TX.get()
                {
                    let _ = tx.send(AskUserResponseAction::Cancel {
                        request_id_str: snapshot.request_id_json.clone(),
                    });
                    close_ask_user_popup_for_request(&snapshot.request_id_json);
                }
                EventResult::Consumed
            }
            _ => EventResult::Ignored,
        }
    });
    let _ = hooks.use_atom(&LANG_VERSION);

    let popup_tokens = &theme_def.read().component.popup;
    let guard = theme_def.read();
    let semantic = &guard.semantic;
    let mut lines: Vec<Line<'_>> = Vec::new();

    match &pending {
        None => {
            lines.push(Line::from(""));
            lines.push(
                Line::from(i18n::tr("popup-ask-user-empty"))
                    .fg(semantic.text.muted)
                    .italic(),
            );
            lines.push(Line::from(""));
            lines.push(Line::from(i18n::tr("common-esc-close")).fg(semantic.text.dim));
        }
        Some(pending) if pending.payload.questions.is_empty() => {
            lines.push(Line::from(""));
            lines
                .push(Line::from(i18n::tr("popup-ask-user-malformed")).fg(semantic.status.warning));
            lines.push(Line::from(""));
            lines.push(Line::from(i18n::tr("common-esc-close")).fg(semantic.text.dim));
        }
        Some(pending) => {
            let au = &pending.payload;
            let focused_idx = (*focused.read()).min(au.questions.len() - 1);
            let answers_read = answers.read();

            // ── Tab 行：已答表示 ✓，当前用 [header] ──
            lines.push(Line::from(""));
            let tab_line = au
                .questions
                .iter()
                .enumerate()
                .map(|(i, q)| {
                    let answered = answers_read.get(i).copied().flatten().is_some();
                    let mark = if answered {
                        i18n::tr("popup-ask-user-answered-mark")
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
            // 分隔线
            lines.push(Line::from("─".repeat(80)).fg(semantic.border.default));

            // ── 当前问题的 content ──
            if let Some(q) = au.questions.get(focused_idx) {
                // 问题文本
                lines.push(Line::from(""));
                lines.push(
                    Line::from(if q.question.is_empty() {
                        q.header.clone()
                    } else {
                        format!("  {}", q.question)
                    })
                    .fg(semantic.text.primary),
                );
                lines.push(Line::from(""));

                // 选项列表
                let selected = answers_read.get(focused_idx).copied().flatten();
                let fopt = *focused_option.read();
                for (opt_i, opt) in q.options.iter().enumerate() {
                    let is_selected = selected == Some(opt_i);
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

                    lines.push(Line::from(format!("  {} {}", mark, opt.label)).style(style));

                    // 选项描述（如有，次行展示）
                    if !opt.description.is_empty() {
                        lines.push(
                            Line::from(format!("    {}", opt.description)).fg(semantic.text.dim),
                        );
                    }
                }

                if q.options.is_empty() {
                    lines.push(
                        Line::from(i18n::tr("popup-ask-user-no-options")).fg(semantic.text.dim),
                    );
                }
            }

            lines.push(Line::from(""));
            // 底部提示
            if au.questions.len() > 1 {
                let all_answered = answers_read.iter().enumerate().all(|(i, a)| {
                    a.is_some()
                        || au
                            .questions
                            .get(i)
                            .map(|q| q.options.is_empty())
                            .unwrap_or(true)
                });
                if all_answered {
                    lines.push(
                        Line::from(i18n::tr("popup-ask-user-hint-multi-submit"))
                            .fg(semantic.text.dim),
                    );
                } else {
                    lines.push(
                        Line::from(i18n::tr("popup-ask-user-hint-multi-next"))
                            .fg(semantic.text.dim),
                    );
                }
            } else {
                let is_answered = answers_read.first().copied().flatten().is_some()
                    || au
                        .questions
                        .first()
                        .map(|q| q.options.is_empty())
                        .unwrap_or(true);
                if is_answered {
                    lines.push(
                        Line::from(i18n::tr("popup-ask-user-hint-single-submit"))
                            .fg(semantic.text.dim),
                    );
                } else {
                    lines.push(
                        Line::from(i18n::tr("popup-ask-user-hint-single-unsubmitted"))
                            .fg(semantic.text.dim),
                    );
                }
            }
        }
    }

    popup_text_shell!(
        i18n::tr("popup-ask-user-title"),
        popup_tokens.action_primary,
        lines
    )
}

/// 将用户选中的答案映射为 serde_json::Value（CreateElicitationResponse content 格式）。
/// ElicitationContentValue 为 #[serde(untagged)]，String 变体直接序列化为纯字符串。
fn build_answers_map(pending: Option<&AskUser>, answers: &[Option<usize>]) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    if let Some(au) = pending {
        for (i, q) in au.questions.iter().enumerate() {
            let val = if let Some(Some(opt_idx)) = answers.get(i) {
                if let Some(opt) = q.options.get(*opt_idx) {
                    json!(opt.label)
                } else {
                    json!("")
                }
            } else {
                json!("")
            };
            map.insert(q.id.clone(), val);
        }
    }
    serde_json::Value::Object(map)
}

#[cfg(test)]
#[path = "ask_user_popup_test.rs"]
mod tests;
