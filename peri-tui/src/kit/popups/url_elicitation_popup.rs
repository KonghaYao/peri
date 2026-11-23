//! URL 模式 MCP elicitation 弹窗。
//!
//! MCP server 通过 `elicitation/create`（`mode: "url"`）请求用户在浏览器完成
//! 授权；notifier 把它投影成 `AcpEventData::UrlElicitation`，bridge 写
//! `URL_ELICITATION_INFO` + `PopupKind::UrlElicitation` 打开本弹窗。
//!
//! 交互：
//! - **Enter / 「完成」按钮**：回 `{"action": "accept"}`——用户声明已在浏览器
//!   完成流程（授权结果由 server 侧校验，本端不伪造凭据）。
//! - **Esc / 「取消」按钮**：回 `{"action": "decline"}`——用户拒绝，操作可继续。
//! - **o**：用系统浏览器打开链接（从不自动打开：URL 必须由用户显式触发）。
//! - **c**：复制链接（复制成功/失败经 `NOTIFICATION` 提示）。
//! - 鼠标只命中底部按钮行（误点弹窗其他位置不会代替用户声明「已完成」）。
//!
//! 过滤：message / server / URL 全部来自 MCP server，进终端前过
//! `sanitize_for_terminal`（剥离 ESC/C0/C1，防转义序列注入）并把单行字段里的
//! 换行/制表符展开为空格；`o` 只对 http(s) 且原文与展示一致的链接调用系统
//! 浏览器——其他 scheme 交给 OS launcher 等于执行任意协议处理器。
//!
//! 行号契约（渲染与鼠标命中同源）：空行、服务器、空行、message×n、空行、
//! URL×m、空行、open/copy 提示、空行、按钮行、Enter/Esc 提示——按钮行 =
//! `7 + n + m`（内容区行号 0 起）。

use ratatui_kit::crossterm::event::Event;
use ratatui_kit::crossterm::event::KeyCode;
use ratatui_kit::crossterm::event::KeyEventKind;
use ratatui_kit::crossterm::event::KeyModifiers;
use ratatui_kit::crossterm::event::MouseButton;
use ratatui_kit::crossterm::event::MouseEventKind;
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::layout::Rect;
use ratatui_kit::ratatui::style::Style;
use ratatui_kit::ratatui::style::Stylize;
use ratatui_kit::ratatui::text::Line;

use crate::i18n;
use crate::kit::ask_user_action::AskUserResponseAction;
use crate::kit::atoms;
use crate::kit::image_safety::{UrlKind, classify_url, sanitize_for_terminal};

use peri_tui_components::overlay::{OverlayAction, OverlayFrame, OverlayHit, actions, hit};
use peri_tui_components::panel_mouse::AreaTracker;

use peri_acp_types::event_data::UrlElicitation;
use peri_theme::atoms::THEME_ATOM;

/// 授权链接换行宽度上限。
const URL_WRAP_WIDTH: usize = 52;
/// 内容行的左缩进列数。
const CONTENT_INDENT: usize = 2;
/// message 最多占用行数（server 文本不可信，超出截断以免挤掉按钮）。
const MAX_MESSAGE_LINES: usize = 4;

/// 渲染宽度：弹窗宽度扣除缩进，上限 [`URL_WRAP_WIDTH`]。
fn wrap_width(popup_width: u16) -> usize {
    usize::from(popup_width)
        .saturating_sub(CONTENT_INDENT)
        .clamp(1, URL_WRAP_WIDTH)
}

/// 服务器文本进终端前的规范化。
///
/// `sanitize_for_terminal` 剥离 ESC/C0/C1 并可见化零宽/bidi 控制；URL 与
/// message 都是单行字段，保留下来的换行/制表符展开为空格，避免 ratatui
/// `Line` 内部出现换行（渲染与鼠标行号都会错位）。
fn display_text(raw: &str) -> String {
    sanitize_for_terminal(raw)
        .chars()
        .map(|c| match c {
            '\n' | '\t' => ' ',
            other => other,
        })
        .collect()
}

/// 是否允许交给系统浏览器打开。
///
/// 只接受 http(s)；且原文必须与展示文本一致（带控制字符的链接与用户看到的
/// 不是同一目标）。其他 scheme 交给 `open` / `xdg-open` 会触发任意协议处理器。
fn openable_url(raw: &str) -> bool {
    matches!(classify_url(raw), UrlKind::RemoteHttp) && display_text(raw) == raw
}

/// 弹窗渲染所用的已过滤文本（渲染与行号反推共用同一份）。
#[derive(Clone)]
struct RenderedElicitation {
    server_name: String,
    message: String,
    url: String,
}

impl RenderedElicitation {
    fn new(payload: &UrlElicitation) -> Self {
        Self {
            server_name: display_text(&payload.server_name),
            message: display_text(&payload.message),
            url: display_text(&payload.url),
        }
    }
}

/// message 的行数（渲染与鼠标命中共用）。
fn message_rows(message: &str, popup_width: u16) -> u16 {
    let wrapped = crate::kit::text_util::wrap_text(message, wrap_width(popup_width));
    wrapped.len().clamp(1, MAX_MESSAGE_LINES) as u16
}

/// URL 的行数。
fn url_rows(url: &str, popup_width: u16) -> u16 {
    crate::kit::text_util::wrap_text(url, wrap_width(popup_width))
        .len()
        .max(1) as u16
}

/// 按钮行（内容区行号，0 起）。
fn action_row(message: &str, url: &str, popup_width: u16) -> u16 {
    7 + message_rows(message, popup_width) + url_rows(url, popup_width)
}

/// 把（可能很长的）文本按宽度换行、缩进并截断。
fn indented_lines(text: &str, popup_width: u16, max_rows: Option<usize>) -> Vec<String> {
    let width = wrap_width(popup_width);
    let mut rows = crate::kit::text_util::wrap_text(text, width);
    if let Some(max_rows) = max_rows {
        rows.truncate(max_rows);
    }
    rows.into_iter()
        .map(|row| format!("{}{}", " ".repeat(CONTENT_INDENT), row))
        .collect()
}

fn copy_to_clipboard(text: &str) -> bool {
    match crate::kit::clipboard::copy_text(text) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(error = ?error, "UrlElicitationPopup: 剪贴板写入失败");
            false
        }
    }
}

fn notify(message: String) {
    *atoms::NOTIFICATION.state().write() = Some(atoms::Notification {
        message,
        until: peri_time::monotonic_now() + std::time::Duration::from_secs(3),
    });
}

fn open_url(url: &str) {
    if let Err(error) = crate::kit::auth_browser::open_browser(url) {
        tracing::warn!(%error, "UrlElicitationPopup: 调用系统浏览器失败，用户可复制链接");
    }
}

/// 回应用户决策并关闭弹窗。
fn respond(pending: &atoms::PendingUrlElicitation, accepted: bool) {
    if let Some(tx) = atoms::ASK_USER_RESPONSE_TX.get() {
        let _ = tx.send(AskUserResponseAction::UrlElicitation {
            owner: pending.owner.clone(),
            request_id_str: pending.request_id_json.clone(),
            accepted,
        });
    } else {
        tracing::warn!("UrlElicitationPopup: ASK_USER_RESPONSE_TX not set, response dropped");
    }
    crate::kit::popup_overlay::close_popup();
}

#[component]
pub fn UrlElicitationPopup(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let pending_store = hooks.use_atom(&atoms::URL_ELICITATION_INFO);
    let pending = pending_store.read().clone();
    let _ = pending_store;
    // 语言切换后按钮/提示文案需要重渲染。
    let _ = hooks.use_atom(&atoms::LANG_VERSION);

    // 弹窗绘制区域（上一帧）——鼠标命中按内容行反推按钮行。
    let area: Option<Rect>;
    {
        let tracker = hooks.use_hook(AreaTracker::new);
        area = tracker.rect;
    }

    let guard = theme_def.read();
    let popup_tokens = &guard.component.popup;
    let semantic = &guard.semantic;
    let popup_width = area.map_or(URL_WRAP_WIDTH as u16 + CONTENT_INDENT as u16, |a| a.width);
    let rendered = pending
        .as_ref()
        .map(|p| RenderedElicitation::new(&p.payload));

    let action_line = rendered.as_ref().map(|_| {
        actions(
            &i18n::tr("popup-url-elicitation-done"),
            &i18n::tr("oauth-btn-cancel"),
            &guard,
        )
    });
    let rendered_action_row = rendered
        .as_ref()
        .map(|r| action_row(&r.message, &r.url, popup_width));
    let handled_pending = pending.clone();
    let handled_rendered = rendered.clone();
    let handled_action_line = action_line.clone();

    hooks.use_event_handler_with_options(
        EventScope::Current,
        EventPriority::High,
        EventOptions { hit_test: true },
        move |event| {
            let Some(pending) = handled_pending.as_ref() else {
                return EventResult::Ignored;
            };
            let Some(rendered) = handled_rendered.as_ref() else {
                return EventResult::Ignored;
            };
            match &event {
                Event::Mouse(mouse) if mouse.kind == MouseEventKind::Down(MouseButton::Left) => {
                    let Some(area) = area else {
                        return EventResult::Ignored;
                    };
                    if hit(mouse, area) != OverlayHit::Inside {
                        return EventResult::Ignored;
                    }
                    let Some(row) = rendered_action_row else {
                        return EventResult::Ignored;
                    };
                    // 内容区行号 = 鼠标行 − (弹窗上沿 + 1)；列相对弹窗左沿。
                    if mouse.row != area.y.saturating_add(1).saturating_add(row) {
                        return EventResult::Ignored;
                    }
                    match handled_action_line
                        .as_ref()
                        .and_then(|line| line.action_at(mouse.column.saturating_sub(area.x)))
                    {
                        Some(OverlayAction::Confirm) => respond(pending, true),
                        Some(OverlayAction::Back) => respond(pending, false),
                        None => return EventResult::Ignored,
                    }
                    EventResult::Consumed
                }
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    match (key.modifiers, key.code) {
                        (KeyModifiers::NONE, KeyCode::Enter) => respond(pending, true),
                        (KeyModifiers::NONE, KeyCode::Esc) => respond(pending, false),
                        (KeyModifiers::NONE, KeyCode::Char('o')) => {
                            if openable_url(&pending.payload.url) {
                                open_url(&rendered.url);
                            } else {
                                notify(i18n::tr("popup-url-elicitation-open-blocked"));
                            }
                        }
                        (KeyModifiers::NONE, KeyCode::Char('c')) => {
                            if copy_to_clipboard(&rendered.url) {
                                notify(i18n::tr("popup-url-elicitation-copied"));
                            } else {
                                notify(i18n::tr("popup-url-elicitation-copy-failed"));
                            }
                        }
                        _ => return EventResult::Ignored,
                    }
                    EventResult::Consumed
                }
                _ => EventResult::Ignored,
            }
        },
    );

    let mut lines: Vec<Line<'_>> = Vec::new();
    match (&pending, &rendered) {
        (Some(_), Some(rendered)) => {
            lines.push(Line::from(""));
            let server = if rendered.server_name.is_empty() {
                i18n::tr("popup-url-elicitation-server-unknown")
            } else {
                i18n::tr_args(
                    "popup-url-elicitation-server",
                    &[(
                        "server".into(),
                        fluent_bundle::FluentValue::from(rendered.server_name.as_str()),
                    )],
                )
            };
            lines.push(
                Line::from(format!("{}{server}", " ".repeat(CONTENT_INDENT)))
                    .fg(popup_tokens.action_primary)
                    .bold(),
            );
            lines.push(Line::from(""));
            for row in indented_lines(&rendered.message, popup_width, Some(MAX_MESSAGE_LINES)) {
                lines.push(Line::from(row).fg(semantic.text.primary));
            }
            lines.push(Line::from(""));
            for row in indented_lines(&rendered.url, popup_width, None) {
                lines.push(Line::from(row).fg(semantic.text.muted));
            }
            lines.push(Line::from(""));
            lines.push(
                Line::from(format!(
                    "{}{}",
                    " ".repeat(CONTENT_INDENT),
                    i18n::tr("popup-url-elicitation-hints")
                ))
                .fg(semantic.text.dim),
            );
            lines.push(Line::from(""));
            // 行号契约的运行时护栏：鼠标命中用 `action_row` 反推，渲染行序一旦
            // 偏离（多/少一行）点击就会落到相邻行。debug 构建与测试恒校验。
            debug_assert_eq!(
                lines.len() as u16,
                action_row(&rendered.message, &rendered.url, popup_width),
                "URL 授权弹窗按钮行必须与鼠标命中行号同源"
            );
            match &action_line {
                Some(row) => lines.push(row.line.clone()),
                None => lines.push(
                    Line::from(i18n::tr("popup-url-elicitation-hints")).fg(semantic.text.dim),
                ),
            }
            lines.push(
                Line::from(format!(
                    "{}{}",
                    " ".repeat(CONTENT_INDENT),
                    i18n::tr("popup-url-elicitation-keys")
                ))
                .fg(semantic.text.dim),
            );
        }
        _ => {
            lines.push(Line::from(""));
            lines.push(
                Line::from(i18n::tr("popup-url-elicitation-empty"))
                    .fg(semantic.text.muted)
                    .italic(),
            );
            lines.push(Line::from(""));
            lines.push(Line::from(i18n::tr("common-esc-close")).fg(semantic.text.dim));
        }
    }

    let frame = OverlayFrame::new(Line::from(i18n::tr("popup-url-elicitation-title")))
        .border(Style::new().fg(popup_tokens.border))
        .title_style(Style::new().fg(popup_tokens.action_primary).bold());
    let text_render = ratatui_kit::ratatui::widgets::Paragraph::new(
        ratatui_kit::ratatui::text::Text::from(lines),
    )
    .block(frame.widget());

    element!(
        View(
            flex_direction: ratatui_kit::ratatui::layout::Direction::Vertical,
            width: ratatui_kit::ratatui::layout::Constraint::Fill(1),
            height: ratatui_kit::ratatui::layout::Constraint::Fill(1),
        ) {
            Text(text: text_render)
        }
    )
}

#[cfg(test)]
#[path = "url_elicitation_popup_test.rs"]
mod tests;
