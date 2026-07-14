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

#[component]
pub fn AskUserPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let pending_store = hooks.use_atom(&ASK_USER_PENDING);
    let pending: Option<AskUser> = pending_store.read().clone();
    let _ = pending_store;
    let _ = hooks.use_atom(&LANG_VERSION);

    let focused = hooks.use_state(|| 0usize);
    let answers = hooks.use_state(Vec::<Option<usize>>::new);
    let focused_option = hooks.use_state(|| 0usize);
    let session_fingerprint = hooks.use_state(Vec::<String>::new);

    let question_count = pending.as_ref().map(|q| q.questions.len()).unwrap_or(0);

    let current_fingerprint: Vec<String> = pending
        .as_ref()
        .map(|p| p.questions.iter().map(|q| q.id.clone()).collect())
        .unwrap_or_default();
    if *session_fingerprint.read() != current_fingerprint {
        *focused.write() = 0;
        *answers.write() = vec![None; question_count];
        *focused_option.write() = 0;
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

        // Space：选中/取消当前高亮的选项
        if (key.modifiers, key.code) == (KeyModifiers::NONE, KeyCode::Char(' ')) {
            let q_idx = *focused.read();
            let opt_idx = *focused_option.read();
            if let Some(au) = pending_for_closure.as_ref()
                && let Some(q) = au.questions.get(q_idx)
                && opt_idx < q.options.len()
            {
                let new_val = if answers.read().get(q_idx).copied().flatten() == Some(opt_idx) {
                    None
                } else {
                    Some(opt_idx)
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
            Some(ListNavAction::MoveUp) => {
                let mut fo = focused_option.write();
                *fo = previous_selection(*fo);
                EventResult::Consumed
            }
            Some(ListNavAction::MoveDown) => {
                let limit = pending_for_closure
                    .as_ref()
                    .and_then(|au| au.questions.get(*focused.read()))
                    .map(|q| q.options.len())
                    .unwrap_or(0);
                if limit > 0 {
                    let mut fo = focused_option.write();
                    *fo = next_selection(*fo, limit);
                }
                EventResult::Consumed
            }
            Some(ListNavAction::CycleForward) if question_count > 0 => {
                let mut f = focused.write();
                *f = cycle_next(*f, question_count);
                let mut fo = focused_option.write();
                *fo = answers.read().get(*f).copied().flatten().unwrap_or(0);
                EventResult::Consumed
            }
            Some(ListNavAction::CycleBackward) if question_count > 0 => {
                let mut f = focused.write();
                *f = cycle_previous(*f, question_count);
                let mut fo = focused_option.write();
                *fo = answers.read().get(*f).copied().flatten().unwrap_or(0);
                EventResult::Consumed
            }
            Some(ListNavAction::Confirm) => {
                let q_idx = *focused.read();
                let all_answered = answers.read().iter().enumerate().all(|(i, a)| {
                    a.is_some()
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
                        let is_answered = answers.read().get(next).copied().flatten().is_some();
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
                    let answers_map =
                        build_answers_map(pending_for_closure.as_ref(), &answers_snapshot);
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
                    let answered = answers_read.get(i).copied().flatten().is_some();
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
                lines.push(
                    Line::from(if q.question.is_empty() {
                        q.header.clone()
                    } else {
                        format!("  {}", q.question)
                    })
                    .fg(semantic.text.primary),
                );
                lines.push(Line::from(""));

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
                    if !opt.description.is_empty() {
                        lines.push(
                            Line::from(format!("    {}", opt.description)).fg(semantic.text.dim),
                        );
                    }
                }

                if q.options.is_empty() {
                    lines.push(
                        Line::from(i18n::tr("panel-ask-user-no-options")).fg(semantic.text.dim),
                    );
                }
            }

            lines.push(Line::from(""));
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
                        Line::from(i18n::tr("panel-ask-user-hint-tab-multi-answered"))
                            .fg(semantic.text.dim),
                    );
                } else {
                    lines.push(
                        Line::from(i18n::tr("panel-ask-user-hint-tab-multi-unanswered"))
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
                        Line::from(i18n::tr("panel-ask-user-hint-single-answered"))
                            .fg(semantic.text.dim),
                    );
                } else {
                    lines.push(
                        Line::from(i18n::tr("panel-ask-user-hint-single-unanswered"))
                            .fg(semantic.text.dim),
                    );
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
