//! ratatui-kit OAuthPopup component.
//!
//! OAuth 授权弹窗：从 `OAUTH_INFO` atom 读取真实授权信息（由 ACP server
//! `OauthNeeded` 事件写入）。交互全部走按钮/导航键，不用快捷键：
//! - **按钮行**（Tab 切入，←→ 选择，Enter 激活；鼠标左键直接点击按钮）：
//!   - [ 打开浏览器 ]：调用系统 `open` 打开 `auth_url`（best-effort）
//!   - [ 复制链接 ]：完整授权链接复制到系统剪贴板
//!   - [ 取消 ]：`mcp/oauth_cancel` RPC + 关闭 popup
//! - **授权码输入框**（Tab 切回 / 鼠标点击输入行，终端粘贴授权码）：
//!   - Enter 提交：`mcp/oauth_callback` RPC 回传后台（手动兜底路径；
//!     state 由后台从授权 URL 解析，本地不缓存凭据）
//!   - 授权码为空时 Enter 关闭 popup（localhost 回调路径由后台自动收码）
//! - **Esc**：取消授权 + 关闭
//!
//! 鼠标命中：`use_event_handler_with_options(..., EventOptions { hit_test: true })`
//! 由 ratatui-kit 按组件绘制区域过滤区域外事件；行号反推用
//! `panel_mouse::AreaTracker`（内容区在 TOP border 之下，内容行号 =
//! `mouse.row - (area.y + 1)`）。
//!
//! I20-D：popup 展示 agent 实际触发的 server_name + 完整 auth_url（换行
//! 展示不截断），用户能据此判断该不该授权。

use fluent_bundle::FluentValue;
use peri_acp_types::event_data::OauthNeeded;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers},
    prelude::*,
    ratatui::{
        style::{Modifier, Style, Stylize},
        text::{Line, Span},
    },
};

use crate::i18n;
use crate::kit::atoms::{ACP_CLIENT_HANDLE, LANG_VERSION, OAUTH_INFO};
use crate::kit::panel_mouse::{AreaTracker, left_down};
use crate::kit::popup_overlay::close_popup;
use peri_theme::atoms::THEME_ATOM;

/// 提交授权码（手动兜底路径）：`mcp/oauth_callback` RPC → host 侧投递到
/// 进行中的 OAuth 流程（state 由后台从授权 URL 解析，TUI 无需感知）。
fn submit_oauth_callback(server_name: &str, code: String) {
    if let Some(client_handle) = ACP_CLIENT_HANDLE.get() {
        let client = client_handle.clone();
        let server_name = server_name.to_string();
        tokio::spawn(async move {
            let params = serde_json::json!({
                "server_name": server_name,
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
    close_popup();
}

/// 取消进行中的授权流程：`mcp/oauth_cancel` RPC（oneshot 通道关闭 →
/// 后台 OAuth 流程 Cancelled → 推送 OauthFailed 事件）。
fn cancel_oauth(server_name: &str) {
    if let Some(client_handle) = ACP_CLIENT_HANDLE.get() {
        let client = client_handle.clone();
        let server_name = server_name.to_string();
        tokio::spawn(async move {
            let params = serde_json::json!({ "server_name": server_name });
            if let Err(e) = client.send_raw_request("mcp/oauth_cancel", params).await {
                tracing::warn!(error = %e, "mcp/oauth_cancel RPC failed");
            }
        });
    }
}

/// 复制文本到系统剪贴板（best-effort，失败仅记日志）。
fn copy_to_clipboard(text: &str) -> bool {
    match arboard::Clipboard::new() {
        Ok(mut cb) => match cb.set_text(text.to_string()) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(error = %e, "OAuthPopup: 剪贴板写入失败");
                false
            }
        },
        Err(e) => {
            tracing::warn!(error = %e, "OAuthPopup: 剪贴板打开失败");
            false
        }
    }
}

/// 按字符宽度换行（长 URL 完整展示，不截断；字符级切片避免 CJK panic）。
fn wrap_text(s: &str, width: usize) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let end = (i + width).min(chars.len());
        out.push(chars[i..end].iter().collect());
        i = end;
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// 按钮行/输入框在内容区中的行号（渲染行序契约，鼠标命中反推用）。
/// 内容区行号 0 起：空行、Server、空行、prompt、空行、URL×n、空行、按钮行、
/// 空行、授权码行、空行、hint → 按钮行 = 6 + n，授权码行 = 8 + n。
fn content_rows(auth_url: &str) -> (u16, u16) {
    let n = wrap_text(auth_url, 52).len() as u16;
    (6 + n, 8 + n)
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
    // 焦点在按钮行（false = 授权码输入框）
    let btn_focus = hooks.use_state(|| false);
    // 按钮行选中项
    let btn_idx = hooks.use_state(|| 0usize);
    // 复制链接成功提示（临时显示 "✓ copied"）
    let copied = hooks.use_state(|| false);
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
            // 激活按钮（键盘 Enter / 鼠标左键点击共用）
            // 0 = 打开浏览器，1 = 复制链接，2 = 取消
            let activate = |idx: usize, info: &OauthNeeded| match idx {
                0 => open_auth_url_in_browser(&info.auth_url),
                1 => {
                    if copy_to_clipboard(&info.auth_url) {
                        *copied.write() = true;
                    }
                }
                _ => {
                    cancel_oauth(&info.server_name);
                    close_popup();
                }
            };

            // ── 鼠标：按钮行左键点击 = 激活按钮；授权码行 = 焦点切回输入框 ──
            if let Event::Mouse(mouse) = event {
                let Some((row, col)) = left_down(&mouse) else {
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
                let (btn_row, input_row) = content_rows(&info.auth_url);
                if content_row == btn_row {
                    // 按列命中按钮：[ label ]，间隔 2 空格，内容从 area.x 起
                    let labels = [
                        i18n::tr("oauth-btn-open"),
                        i18n::tr("oauth-btn-copy"),
                        i18n::tr("oauth-btn-cancel"),
                    ];
                    let mut x = area.x;
                    for (i, label) in labels.iter().enumerate() {
                        let w = format!("[ {label} ]").chars().count() as u16;
                        if col >= x && col < x + w {
                            activate(i, info);
                            return EventResult::Consumed;
                        }
                        x = x.saturating_add(w + 2);
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
                        close_popup();
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
                                close_popup();
                            }
                        } else {
                            // 授权码为空：localhost 回调路径由后台自动收码，
                            // 关闭 popup 等待完成事件。
                            close_popup();
                        }
                        EventResult::Consumed
                    }
                    (KeyModifiers::NONE, KeyCode::Esc) => {
                        if let Some(info) = &info_for_event {
                            cancel_oauth(&info.server_name);
                        }
                        close_popup();
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
            for url_line in wrap_text(&oauth.auth_url, 52) {
                lines.push(Line::from(format!("  {url_line}")).fg(semantic.text.muted));
            }
            lines.push(Line::from(""));

            // ── 按钮行：Tab 切入 / 鼠标点击，←→ 选择，Enter 激活 ──
            let btn_focused = *btn_focus.read();
            let sel = *btn_idx.read();
            let btn_style = |i: usize, focused: bool| -> Style {
                if focused && i == sel {
                    Style::new()
                        .fg(semantic.status.warning)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(semantic.text.primary)
                }
            };
            let labels = [
                i18n::tr("oauth-btn-open"),
                i18n::tr("oauth-btn-copy"),
                i18n::tr("oauth-btn-cancel"),
            ];
            let mut btn_line: Vec<Span<'_>> = Vec::new();
            for (i, label) in labels.iter().enumerate() {
                if i > 0 {
                    btn_line.push(Span::raw("  "));
                }
                let text = format!("[ {label} ]");
                btn_line.push(Span::styled(text, btn_style(i, btn_focused)));
            }
            lines.push(Line::from(btn_line));
            lines.push(Line::from(""));

            // ── 授权码输入框（手动兜底路径）：浏览器无法回跳（服务器/网络
            //    受限）时，将授权页显示的授权码粘贴到此处按 Enter 提交 ──
            let code_display = code_input.read().clone();
            let code_label = i18n::tr("oauth-callback-label");
            let code_focused = !btn_focused;
            let code_style = if code_focused {
                Style::new()
                    .fg(semantic.text.primary)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(semantic.text.primary)
            };
            let code_line = if code_display.is_empty() {
                format!("  {code_label} (empty)")
            } else {
                format!("  {code_label}: {code_display}")
            };
            lines.push(Line::from(code_line).style(code_style));
            lines.push(Line::from(""));
            let hint = if *copied.read() {
                i18n::tr("oauth-copied-hint")
            } else {
                i18n::tr("popup-oauth-action-hint")
            };
            lines.push(Line::from(hint).fg(semantic.text.dim));
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

/// I20-D：用系统命令打开 auth_url——best-effort，失败仅记日志不报错。
///
/// macOS 用 `open`，Linux 用 `xdg-open`，Windows 用 `cmd /C start`。任何错误
/// （命令不存在、权限不足等）都不应 panic——OAuth 弹窗仍展示完整 URL，
/// 用户可经「复制链接」按钮手动复制到浏览器。
fn open_auth_url_in_browser(url: &str) {
    use std::process::Command;

    let (program, args): (&str, Vec<&str>) = if cfg!(target_os = "macos") {
        ("open", vec![url])
    } else if cfg!(target_os = "windows") {
        ("cmd", vec!["/C", "start", "", url])
    } else {
        ("xdg-open", vec![url])
    };

    match Command::new(program).args(&args).spawn() {
        Ok(_child) => {
            tracing::info!(program, url, "OAuthPopup: 已调用系统命令打开浏览器");
        }
        Err(e) => {
            tracing::warn!(
                program,
                error = %e,
                "OAuthPopup: 调用系统浏览器失败，用户可复制 URL"
            );
        }
    }
}
