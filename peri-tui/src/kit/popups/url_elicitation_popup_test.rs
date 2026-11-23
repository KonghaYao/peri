//! URL 授权弹窗的行号契约：渲染行序与鼠标命中反推共用同一口径。
//!
//! 鼠标命中按「内容区行号」反推按钮行，行数算错一个就会让点击落到相邻行
//! （按钮失效或误触）。本文件只测纯函数，不依赖终端渲染。

use super::action_row;
use super::display_text;
use super::message_rows;
use super::openable_url;
use super::url_rows;
use super::wrap_width;

use crate::kit::atoms;
use peri_acp_types::event_data::UrlElicitation;
use ratatui_kit::prelude::*;
use serial_test::serial;

const SHORT_URL: &str = "https://example.test/authorize";

/// 宽弹窗保持 52 列版式，窄弹窗按可用宽度收窄。
#[test]
fn wrap_width_keeps_52_column_layout_and_narrows_on_small_popups() {
    assert_eq!(wrap_width(90), 52);
    assert_eq!(wrap_width(24), 22);
    assert_eq!(wrap_width(1), 1);
}

/// 短 message + 短 URL：服务器 / 空行 / message / 空行 / URL / 空行 / 提示 /
/// 空行 / 按钮 = 内容区第 9 行。
#[test]
fn action_row_counts_server_message_and_url_rows() {
    assert_eq!(action_row("Authorize", SHORT_URL, 90), 9);
}

/// 长 URL 换行后按钮行同步下移；窄弹窗换行更多、按钮行更靠下。
#[test]
fn action_row_follows_wrapped_url_rows() {
    let url = format!("https://example.test/{}", "p".repeat(120));
    assert!(url_rows(&url, 90) > 1, "长 URL 必须换行");
    assert_eq!(
        action_row("Authorize", &url, 90),
        7 + 1 + url_rows(&url, 90)
    );
    assert!(
        action_row("Authorize", &url, 24) > action_row("Authorize", &url, 90),
        "窄屏换行行数增加，按钮行必须继续下移"
    );
}

/// server 文本不可信：超长 message 最多占 4 行，不得把按钮挤出弹窗。
#[test]
fn message_rows_caps_untrusted_server_text() {
    let long = "m".repeat(500);
    assert_eq!(message_rows(&long, 90), 4);
    assert_eq!(
        action_row(&long, SHORT_URL, 90),
        7 + 4 + url_rows(SHORT_URL, 90)
    );
}

/// 空 message 仍占一行：按钮行不得与上一行重叠。
#[test]
fn empty_message_still_occupies_one_row() {
    assert_eq!(message_rows("", 90), 1);
    assert_eq!(action_row("", SHORT_URL, 90), 9);
}

/// 服务器文本不可信：ESC/C0 必须剥离（终端转义注入），单行字段里的换行 /
/// 制表符展开为空格（否则 ratatui 行号契约失效）。
#[test]
fn display_text_strips_terminal_escapes_and_flattens_layout_whitespace() {
    assert_eq!(display_text("safe"), "safe");
    assert_eq!(
        display_text("a\u{1b}[31mb\u{0}c\u{7f}d"),
        "a[31mbcd",
        "ESC / NUL / DEL 不得进入终端"
    );
    assert_eq!(display_text("line1\nline2\ttab"), "line1 line2 tab");
    assert_eq!(display_text("bidi\u{202e}"), "bidi\u{fffd}");
}

/// `o` 只对 http(s) 原文（无控制字符）打开：其他 scheme 交给系统 launcher
/// 会触发任意协议处理器，展示被过滤过的链接也不是同一目标。
#[test]
fn openable_url_allows_only_plain_web_links() {
    assert!(openable_url("https://example.test/authorize"));
    assert!(openable_url("http://127.0.0.1:8080/callback"));
    assert!(!openable_url("file:///Applications/Calculator.app"));
    assert!(!openable_url("javascript:alert(1)"));
    assert!(!openable_url("data:text/html,<script>"));
    assert!(
        !openable_url("example.test/authorize"),
        "无 scheme 不是网页链接"
    );
    assert!(
        !openable_url("https://example.test/\u{1b}[31m"),
        "控制字符会让 launcher 收到与展示不同的目标"
    );
}

/// 全局 atoms 的保存/恢复——断言失败也不污染其它测试。
struct PendingGuard(Option<atoms::PendingUrlElicitation>);

impl PendingGuard {
    fn install(message: &str, url: &str) -> Self {
        let previous = atoms::URL_ELICITATION_INFO.state().read().clone();
        *atoms::URL_ELICITATION_INFO.state().write() = Some(atoms::PendingUrlElicitation {
            owner: Default::default(),
            request_id_json: "\"url-1\"".into(),
            payload: UrlElicitation {
                server_name: "peri-browser".into(),
                message: message.into(),
                url: url.into(),
                elicitation_id: "e-1".into(),
            },
        });
        Self(previous)
    }
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        *atoms::URL_ELICITATION_INFO.state().write() = self.0.take();
    }
}

/// 缓冲区按行读成文本：跳过双宽字符的续接格。
fn row_text(buf: &ratatui_kit::ratatui::buffer::Buffer, y: u16) -> String {
    let mut text = String::new();
    let mut skip = 0u16;
    for x in buf.area.x..buf.area.right() {
        if skip > 0 {
            skip -= 1;
            continue;
        }
        let symbol = buf[(x, y)].symbol();
        text.push_str(symbol);
        skip = unicode_width::UnicodeWidthStr::width(symbol).saturating_sub(1) as u16;
    }
    text
}

/// 真实组件渲染：按钮行必须落在 `action_row`（鼠标命中反推的行号）上。
///
/// 两帧：`AreaTracker` 第一帧测宽，第二帧起按上一帧区域换行与命中。
#[test]
#[serial]
fn rendered_button_row_matches_mouse_hit_row() {
    const MESSAGE: &str = "Authorize the browser session";
    let _guard = PendingGuard::install(MESSAGE, SHORT_URL);
    let width = 70u16;
    let buf =
        ratatui_kit::test_util::render_frames(element!(super::UrlElicitationPopup()), width, 24, 2);
    let done = crate::i18n::tr("popup-url-elicitation-done");
    let cancel = crate::i18n::tr("oauth-btn-cancel");
    let rows: Vec<String> = (0..24).map(|y| row_text(&buf, y)).collect();
    let row = rows
        .iter()
        .position(|line| line.contains(&done) && line.contains(&cancel))
        .unwrap_or_else(|| panic!("按钮行必须同时渲染 {done} 与 {cancel}：{rows:?}"));

    assert_eq!(
        row as u16,
        1 + action_row(MESSAGE, SHORT_URL, width),
        "渲染按钮行 = 上边框 1 行 + 内容区 action_row；鼠标命中共用同一行号"
    );
    assert!(
        rows.iter().any(|line| line.contains(SHORT_URL)),
        "URL 必须完整展示（可换行）：{rows:?}"
    );
}

/// 不可信 message 含 ESC / 换行：渲染前过滤，按钮行仍落在「已过滤文本」的
/// 行号上（鼠标命中用同一份过滤结果）。
#[test]
#[serial]
fn rendered_untrusted_text_is_sanitized_without_moving_the_button_row() {
    const MESSAGE: &str = "Authorize\u{1b}[31m the\nbrowser";
    let _guard = PendingGuard::install(MESSAGE, SHORT_URL);
    let width = 70u16;
    let buf =
        ratatui_kit::test_util::render_frames(element!(super::UrlElicitationPopup()), width, 24, 2);
    let rows: Vec<String> = (0..24).map(|y| row_text(&buf, y)).collect();
    assert!(
        !rows.iter().any(|line| line.contains('\u{1b}')),
        "ESC 不得进入终端：{rows:?}"
    );
    let done = crate::i18n::tr("popup-url-elicitation-done");
    let cancel = crate::i18n::tr("oauth-btn-cancel");
    let row = rows
        .iter()
        .position(|line| line.contains(&done) && line.contains(&cancel))
        .unwrap_or_else(|| panic!("按钮行必须同时渲染 {done} 与 {cancel}：{rows:?}"));
    assert_eq!(
        row as u16,
        1 + action_row(&display_text(MESSAGE), SHORT_URL, width),
        "按钮行必须按过滤后的行数计算"
    );
}
