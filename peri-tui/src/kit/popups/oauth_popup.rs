//! ratatui-kit OAuthPopup component.
//!
//! OAuth 授权弹窗：从 `OAUTH_INFO` atom 读取真实授权信息（由 ACP server
//! `OauthNeeded` 事件写入）。交互全部走按钮/导航键，不用快捷键：
//! - **按钮行**（默认焦点；Tab 切到输入框，←→ 选择，Enter 激活；
//!   鼠标左键直接点击按钮；外观是通用 Button 的 Filled 反色块，无 `[ ]`）：
//!   - 打开浏览器：调用系统 `open` 打开 `auth_url`（best-effort）
//!   - 复制链接：完整授权链接复制到系统剪贴板
//!   - 取消：`mcp/oauth_cancel` RPC + 关闭 popup
//! - **授权码输入框**（Tab 切回 / 鼠标点击输入行，终端粘贴授权码）：
//!   - Enter 提交：`mcp/oauth_callback` RPC 回传后台（手动兜底路径；
//!     state 由后台从授权 URL 解析，本地不缓存凭据）
//!   - 授权码为空时 Enter 关闭 popup（localhost 回调路径由后台自动收码）
//! - **Esc**：取消授权 + 关闭
//!
//! 视觉契约：按钮行是通用 Button 的 Filled 反色块（焦点项 BOLD），输入框聚焦时
//! 整行 selection 背景反转——焦点在哪一目了然；复制/打开浏览器等瞬时反馈 3 秒
//! 自动消失。
//!
//! 鼠标命中：`use_event_handler_with_options(..., EventOptions { hit_test: true })`
//! 由 ratatui-kit 按组件绘制区域过滤区域外事件；行号反推用
//! `panel_mouse::AreaTracker`（内容区在 TOP border 之下，内容行号 =
//! `mouse.row - (area.y + 1)`）。
//!
//! I20-D：popup 展示 agent 实际触发的 server_name + 完整 auth_url（换行
//! 展示不截断），用户能据此判断该不该授权。

use std::time::Duration;

use fluent_bundle::FluentValue;

use ratatui_kit::crossterm::event::Event;
use ratatui_kit::crossterm::event::KeyCode;
use ratatui_kit::crossterm::event::KeyEventKind;
use ratatui_kit::crossterm::event::KeyModifiers;
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::style::Modifier;
use ratatui_kit::ratatui::style::Style;
use ratatui_kit::ratatui::style::Stylize;
use ratatui_kit::ratatui::text::Line;
use ratatui_kit::ratatui::text::Span;

use crate::i18n;

use peri_tui_components::button::{
    ButtonChrome, ButtonSpec, ButtonState, ButtonTone, ButtonVisual, button_visual,
};
use peri_tui_components::panel_mouse::AreaTracker;

use peri_theme::atoms::THEME_ATOM;
use peri_theme::theme::ThemeDefinition;

/// 提交授权码（手动兜底路径）：`mcp/oauth_callback` RPC → host 侧投递到
/// 进行中的 OAuth 流程（state 由后台从授权 URL 解析，TUI 无需感知）。
fn submit_oauth_callback(server_name: &str, code: String) {
    if let Some(client_handle) = ACP_CLIENT_HANDLE.get() {
        let client = client_handle.clone();
        let session_id = crate::kit::atoms::OAUTH_SESSION_ID.state().read().clone();
        if client.current_session_id() != session_id {
            return;
        }
        let server_name = server_name.to_string();
        tokio::spawn(async move {
            let params = serde_json::json!({
                "server_name": server_name,
                "sessionId": session_id,
                "code": code,
                "state": "",
            });
            if let Err(e) = client.send_raw_request("mcp/oauth_callback", params).await {
                tracing::warn!(error = %e, "mcp/oauth_callback RPC failed");
            }
        });
    } else {
        tracing::warn!(target: "oauth-popup", "ACP_CLIENT_HANDLE not set, oauth_callback skipped");
    }
    crate::kit::popup_overlay::close_popup();
}

/// 取消进行中的授权流程：`mcp/oauth_cancel` RPC（oneshot 通道关闭 →
/// 后台 OAuth 流程 Cancelled → 推送 OauthFailed 事件）。
fn cancel_oauth(server_name: &str) {
    if let Some(client_handle) = ACP_CLIENT_HANDLE.get() {
        let client = client_handle.clone();
        let session_id = crate::kit::atoms::OAUTH_SESSION_ID.state().read().clone();
        if client.current_session_id() != session_id {
            return;
        }
        let server_name = server_name.to_string();
        tokio::spawn(async move {
            let params = serde_json::json!({ "server_name": server_name, "sessionId": session_id });
            if let Err(e) = client.send_raw_request("mcp/oauth_cancel", params).await {
                tracing::warn!(error = %e, "mcp/oauth_cancel RPC failed");
            }
        });
    }
}

fn copy_to_clipboard(text: &str) -> bool {
    match crate::kit::clipboard::copy_text(text) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(error = ?error, "OAuthPopup: 剪贴板写入失败");
            false
        }
    }
}

/// 授权链接换行宽度上限（既有 52 列版式）。
const OAUTH_WRAP_WIDTH: usize = 52;
/// 授权链接行的左缩进列数（`"  "` 前缀）。
const OAUTH_URL_INDENT: usize = 2;
/// 组件区域尚未测量（首帧）时的弹窗宽度兜底：宽度 − 2 = 52，保持既有版式。
const OAUTH_FALLBACK_WIDTH: u16 = (OAUTH_WRAP_WIDTH + OAUTH_URL_INDENT) as u16;

/// 授权链接的实际换行宽度：弹窗宽度扣除行缩进，上限为 [`OAUTH_WRAP_WIDTH`]。
///
/// 渲染与 [`content_rows`]（鼠标命中反推行号）必须共用此函数，窄屏下不能各自
/// 按 52 列换行。
fn oauth_wrap_width(popup_width: u16) -> usize {
    usize::from(popup_width)
        .saturating_sub(OAUTH_URL_INDENT)
        .clamp(1, OAUTH_WRAP_WIDTH)
}

/// 按钮行/输入框在内容区中的行号（渲染行序契约，鼠标命中反推用）。
/// 内容区行号 0 起：空行、Server、空行、prompt、空行、URL×n、空行、按钮行、
/// 空行、授权码行、空行、hint → 按钮行 = 6 + n，授权码行 = 8 + n。
fn content_rows(auth_url: &str, popup_width: u16) -> (u16, u16) {
    let n = crate::kit::text_util::wrap_text(auth_url, oauth_wrap_width(popup_width)).len() as u16;
    (6 + n, 8 + n)
}

/// 按钮行三个动作的标签（渲染与鼠标命中同一来源）。
fn button_labels() -> [String; 3] {
    [
        i18n::tr("oauth-btn-open"),
        i18n::tr("oauth-btn-copy"),
        i18n::tr("oauth-btn-cancel"),
    ]
}

/// 按钮外观：Filled 反色块，tone 区分主次，`focused` = 按钮行聚焦且该项选中。
/// 渲染与命中宽度共用这一函数（`ButtonVisual.width` 是唯一列宽口径）。
fn button_visual_for(
    label: &str,
    tone: ButtonTone,
    focused: bool,
    theme: &ThemeDefinition,
) -> ButtonVisual {
    button_visual(
        ButtonSpec {
            label,
            chrome: ButtonChrome::Filled,
            tone,
            state: if focused {
                ButtonState::Focused
            } else {
                ButtonState::Normal
            },
        },
        theme,
    )
}

/// 单个按钮的通用 Button 外观：打开浏览器是主操作（Primary），复制/取消是次要
/// 操作（Secondary）；按钮行聚焦且当前选中时用 `Focused`。
fn button_visual_at(idx: usize, focused: bool, theme: &ThemeDefinition) -> ButtonVisual {
    let labels = button_labels();
    let label = labels.get(idx).map(String::as_str).unwrap_or_default();
    let tone = if idx == 0 {
        ButtonTone::Primary
    } else {
        ButtonTone::Secondary
    };
    button_visual_for(label, tone, focused, theme)
}

/// 按钮命中宽度：与渲染共用 [`button_visual_for`] 的 `ButtonVisual.width`
/// （CJK 标签按显示列宽，不能用 `chars().count()`——否则点击会落到相邻按钮）。
fn button_label_width(label: &str, theme: &ThemeDefinition) -> u16 {
    button_visual_for(label, ButtonTone::Secondary, false, theme).width
}

/// 按钮行渲染：三个 Filled 按钮横向排列（间隔 2 空格），焦点项用 `Focused`。
/// 文本、列宽与样式全部来自 [`button_visual_at`]，鼠标命中读同一份宽度。
fn button_row(focused: bool, selected: usize, theme: &ThemeDefinition) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(5);
    for i in 0..3 {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        let visual = button_visual_at(i, focused && i == selected, theme);
        spans.push(Span::styled(visual.text, visual.style));
    }
    Line::from(spans)
}

/// 瞬时反馈提示（复制链接 / 打开浏览器），3 秒后自动消失。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OauthHint {
    Copied,
    BrowserOpened,
}

#[component]
pub fn OAuthPopup(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    // I20-D：从 OAUTH_INFO atom 读取真实数据。atom 由 dispatch_and_notify 在
    // OauthNeeded 事件时写入。None 时显示占位（理论上不会发生——popup 只有在
    // POPUP_KIND=OAuth 时渲染，而该状态只在写入 OAUTH_INFO 同步设置）。
    let info_store = hooks.use_atom(&OAUTH_INFO);
    let info = info_store.read().clone();
    let _ = info_store;

    // 授权码输入框内容（手动兜底路径；终端粘贴直接进入）
    let code_input = hooks.use_state(String::new);
    // 焦点在按钮行（false = 授权码输入框）。默认按钮行——用户第一动作是
    // 打开浏览器/复制链接，授权码在浏览器里授权后才出现。
    let btn_focus = hooks.use_state(|| true);
    // 按钮行选中项
    let btn_idx = hooks.use_state(|| 0usize);
    // 瞬时反馈（复制/打开浏览器），Some 时优先于焦点提示显示
    let hint = hooks.use_state(|| None::<OauthHint>);
    // 反馈序号——防 3s 定时器清掉更新的提示（generation guard）
    let hint_seq = hooks.use_state(|| 0u64);
    // 组件绘制区域（上一帧，绝对坐标）——鼠标命中反推行号
    let area = hooks.use_hook(AreaTracker::new).rect;

    hooks.use_atom(&LANG_VERSION);

    // 闭包另持一份 info 副本（渲染端与事件端共用同一 atom 副本）
    let info_for_event = info.clone();

    hooks.use_event_handler_with_options(
        EventScope::Current,
        EventPriority::High, // 与 rewind_popup 一致：根层 Esc 为 Normal，同优先级先注册先消费
        EventOptions { hit_test: true },
        move |event| {
            // 写入瞬时反馈 + 3s 后自动清空（seq guard：旧定时器不干扰新提示）
            let show_hint = |h: OauthHint| {
                *hint.write() = Some(h);
                *hint_seq.write() = *hint_seq.read() + 1;
                let seq = *hint_seq.read();
                // ReactiveHandle 是 Copy，直接复制句柄（非 move）
                let h_copy = hint;
                let s_copy = hint_seq;
                tokio::spawn(async move {
                    peri_time::sleep(Duration::from_secs(3)).await;
                    if *s_copy.read() == seq {
                        *h_copy.write() = None;
                    }
                });
            };

            // 激活按钮（键盘 Enter / 鼠标左键点击共用）
            // 0 = 打开浏览器，1 = 复制链接，2 = 取消
            let activate = |idx: usize, info: &OauthNeeded| match idx {
                0 => {
                    open_auth_url_in_browser(&info.auth_url);
                    show_hint(OauthHint::BrowserOpened);
                }
                1 => {
                    if copy_to_clipboard(&info.auth_url) {
                        show_hint(OauthHint::Copied);
                    }
                }
                _ => {
                    cancel_oauth(&info.server_name);
                    crate::kit::popup_overlay::close_popup();
                }
            };

            // ── 鼠标：按钮行左键点击 = 激活按钮；授权码行 = 焦点切回输入框 ──
            if let Event::Mouse(mouse) = event {
                let Some((row, col)) = peri_tui_components::panel_mouse::left_down(&mouse) else {
                    return EventResult::Ignored;
                };
                let Some(area) = area else {
                    return EventResult::Consumed;
                };
                // 顶部边框行不可点
                if row < area.y.saturating_add(1) {
                    return EventResult::Consumed;
                }
                let content_row = row.saturating_sub(area.y).saturating_sub(1);
                let Some(info) = &info_for_event else {
                    return EventResult::Consumed;
                };
                let (btn_row, input_row) = content_rows(&info.auth_url, area.width);
                if content_row == btn_row {
                    // 按列命中按钮：Filled ` label `，间隔 2 空格，内容从 area.x 起。
                    // 宽度与渲染共用同一 `ButtonVisual`（主题只影响样式，取全局主题）。
                    let labels = button_labels();
                    let theme_state = THEME_ATOM.state();
                    let theme = theme_state.read();
                    let mut x = area.x;
                    {
                        let iteration_items = labels.iter().enumerate();
                        for (i, label) in iteration_items {
                            let w = button_label_width(label, &theme);
                            if col >= x && col < x + w {
                                activate(i, info);
                                return EventResult::Consumed;
                            }
                            x = x.saturating_add(w + 2);
                        }
                    }
                    return EventResult::Consumed;
                }
                if content_row == input_row {
                    *btn_focus.write() = false;
                }
                // popup 区域内左键点击一律消费，防止穿透
                return EventResult::Consumed;
            }

            let Event::Key(key) = event else {
                return EventResult::Ignored;
            };
            if key.kind != KeyEventKind::Press {
                return EventResult::Ignored;
            }
            // Tab：切换焦点（按钮行 ⇄ 授权码输入框）
            if key.code == KeyCode::Tab {
                let mut f = btn_focus.write();
                *f = !*f;
                if *f {
                    *btn_idx.write() = 0;
                }
                return EventResult::Consumed;
            }
            if *btn_focus.read() {
                // ── 按钮行焦点：←→ 选择，Enter 激活 ──
                match key.code {
                    KeyCode::Left => {
                        let mut i = btn_idx.write();
                        *i = if *i == 0 { 2 } else { *i - 1 };
                        EventResult::Consumed
                    }
                    KeyCode::Right => {
                        let mut i = btn_idx.write();
                        *i = (*i + 1) % 3;
                        EventResult::Consumed
                    }
                    KeyCode::Enter => {
                        let idx = *btn_idx.read();
                        if let Some(info) = &info_for_event {
                            activate(idx, info);
                        }
                        EventResult::Consumed
                    }
                    KeyCode::Esc => {
                        if let Some(info) = &info_for_event {
                            cancel_oauth(&info.server_name);
                        }
                        crate::kit::popup_overlay::close_popup();
                        EventResult::Consumed
                    }
                    _ => EventResult::Ignored,
                }
            } else {
                // ── 授权码输入框焦点：字符输入、Enter 提交、Esc 取消 ──
                match (key.modifiers, key.code) {
                    (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char(c)) => {
                        code_input.write().push(c);
                        EventResult::Consumed
                    }
                    (KeyModifiers::NONE, KeyCode::Backspace) => {
                        code_input.write().pop();
                        EventResult::Consumed
                    }
                    (KeyModifiers::NONE, KeyCode::Enter) => {
                        let code = code_input.read().clone();
                        if !code.is_empty() {
                            // 手动兜底路径：授权码回传后台（state 后台解析）
                            if let Some(info) = &info_for_event {
                                submit_oauth_callback(&info.server_name, code);
                            } else {
                                crate::kit::popup_overlay::close_popup();
                            }
                        } else {
                            // 授权码为空：localhost 回调路径由后台自动收码，
                            // 关闭 popup 等待完成事件。
                            crate::kit::popup_overlay::close_popup();
                        }
                        EventResult::Consumed
                    }
                    (KeyModifiers::NONE, KeyCode::Esc) => {
                        if let Some(info) = &info_for_event {
                            cancel_oauth(&info.server_name);
                        }
                        crate::kit::popup_overlay::close_popup();
                        EventResult::Consumed
                    }
                    _ => EventResult::Ignored,
                }
            }
        },
    );

    let popup_tokens = &theme_def.read().component.popup;
    let guard = theme_def.read();
    let semantic = &guard.semantic;
    let sel_bg = semantic.surface.selection;
    // 绘制与 `content_rows`（鼠标命中）共用同一宽度；首帧未测量时用兜底值。
    let popup_width = area.map_or(OAUTH_FALLBACK_WIDTH, |rect| rect.width);
    let mut lines: Vec<Line<'_>> = Vec::new();

    match &info {
        None => {
            // 理论上不会渲染此分支——POPUP_KIND=OAuth 暗示 OAUTH_INFO 已写入
            lines.push(Line::from(""));
            lines.push(
                Line::from("  No OAuth request pending.")
                    .fg(semantic.text.muted)
                    .italic(),
            );
            lines.push(Line::from(""));
            lines.push(Line::from(i18n::tr("common-esc-close")).fg(semantic.text.dim));
        }
        Some(oauth) => {
            lines.push(Line::from(""));
            lines.push(
                Line::from(format!("  Server: {}", oauth.server_name))
                    .fg(popup_tokens.action_primary)
                    .bold(),
            );
            lines.push(Line::from(""));
            lines.push(Line::from(i18n::tr("oauth-prompt")).fg(semantic.text.primary));
            lines.push(Line::from(""));
            // 完整授权链接按宽度换行展示（不截断），可经「复制链接」按钮复制
            for url_line in
                crate::kit::text_util::wrap_text(&oauth.auth_url, oauth_wrap_width(popup_width))
            {
                lines.push(Line::from(format!("  {url_line}")).fg(semantic.text.muted));
            }
            lines.push(Line::from(""));

            // ── 按钮行：通用 Button 的 Filled 外观，焦点项反色 + BOLD ──
            let btn_focused = *btn_focus.read();
            let sel = *btn_idx.read();
            lines.push(button_row(btn_focused, sel, &guard));
            lines.push(Line::from(""));

            // ── 授权码输入框（手动兜底路径）：浏览器无法回跳（服务器/网络
            //    受限）时，将授权页显示的授权码粘贴到此处按 Enter 提交 ──
            let code_display = code_input.read().clone();
            let code_label = i18n::tr("oauth-callback-label");
            let code_focused = !btn_focused;
            let code_style = if code_focused {
                Style::new()
                    .fg(semantic.text.primary)
                    .bg(sel_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(semantic.text.dim)
            };
            let code_line = if code_display.is_empty() {
                format!("  {code_label} (empty)")
            } else {
                format!("  {code_label}: {code_display}")
            };
            // 输入框聚焦时行尾追加光标
            let code_line = if code_focused {
                format!("{code_line}▏")
            } else {
                code_line
            };
            lines.push(Line::from(code_line).style(code_style));
            lines.push(Line::from(""));
            // 反馈提示优先，其次按焦点区显示操作指南
            let hint_text = match *hint.read() {
                Some(OauthHint::Copied) => i18n::tr("oauth-copied-hint"),
                Some(OauthHint::BrowserOpened) => i18n::tr("oauth-opened-hint"),
                None if btn_focused => i18n::tr("oauth-hint-btn-focus"),
                None => i18n::tr("oauth-hint-input-focus"),
            };
            lines.push(Line::from(hint_text).fg(semantic.text.dim));
        }
    }

    let title = match &info {
        Some(oauth) => i18n::tr_args(
            "oauth-title",
            &[(
                "server".to_string(),
                FluentValue::from(oauth.server_name.as_str()),
            )],
        ),
        None => " OAuth Authorization ".to_string(),
    };

    popup_text_shell!(title, popup_tokens.action_primary, lines)
}

/// I20-D：用系统命令打开 auth_url——best-effort。
///
/// 共用 [`crate::kit::auth_browser`]：不在日志中记录授权 URL（含 state/nonce），
/// 失败仅记日志，弹窗仍展示完整 URL，用户可经「复制链接」手动复制到浏览器。
fn open_auth_url_in_browser(url: &str) {
    if let Err(error) = crate::kit::auth_browser::open_browser(url) {
        tracing::warn!(%error, "OAuthPopup: 调用系统浏览器失败，用户可复制 URL");
    }
}

#[cfg(test)]
#[path = "oauth_popup_test.rs"]
mod tests;

use peri_acp_types::event_data::OauthNeeded;

use crate::kit::atoms::ACP_CLIENT_HANDLE;

use crate::kit::atoms::LANG_VERSION;

use crate::kit::atoms::OAUTH_INFO;
