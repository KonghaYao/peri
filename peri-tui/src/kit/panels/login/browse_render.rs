//! Login Browse 列表的视口与行构造——绘制窗口与鼠标命中的唯一推导处。
//!
//! [Why] Browse 曾用写死的可见项数裁剪列表，提示行又随内容顺排，终端变高后
//! 剩余空间堆在提示之后；鼠标命中另写一遍「3/4 行可变高」的线性累加，绘制与
//! 命中会给出不同的项。这里把每项固定为 4 行（ID、认证状态、Base URL、分隔
//! 空行），由共享 [`ListViewport`] 按面板内容区行数推导可见项与滚动起点；绘制
//! 取 `window()`、事件取 `hit_layout()`，两者来自同一实例。
//!
//! 行度量固定为 `header 1 / item 4 / footer 1`：面板最小高度 8 时内容区 6 行，
//! 仍能显示 1 项；若 footer 取 2 行则只剩 0 项。列表项自带的末尾空行已经提供
//! 视觉间隔，不再额外占用 footer。
//!
//! Edit/Auth 表单仍按行语义布局，不接入本模块。

use ratatui_kit::ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::i18n;
use crate::kit::atoms::ProviderSummary;
use crate::kit::panels::shared::list::{ListMetrics, ListViewport};

use super::render::{make_hint_line_for_login, provider_type_display};

/// Browse 列表内容的固定行度量：计数行 1 行、每项 4 行、提示行 1 行。
pub(super) const BROWSE_METRICS: ListMetrics = ListMetrics::new(1, 4, 1);

/// Base URL 的最大展示字符数；超出部分由列表宽度裁剪（不换行，保持固定行高）。
const MAX_BASE_URL_CHARS: usize = 70;

/// Browse 渲染配色（由调用方从当前主题派生；本模块不读全局状态）。
#[derive(Debug, Clone, Copy)]
pub(super) struct BrowseRenderColors {
    /// 选中标记与选中行（面板标题色）
    pub(super) marker: Color,
    pub(super) text: Color,
    pub(super) dim: Color,
    pub(super) muted: Color,
    pub(super) success: Color,
    pub(super) error: Color,
    /// 提示行按键强调色
    pub(super) accent: Color,
}

/// 唯一视口推导入口：内容区行数 → 可见项数 / 滚动起点 / 绘制窗口 / 命中布局。
pub(super) fn viewport(content_rows: u16, item_count: usize, selected: usize) -> ListViewport {
    ListViewport::for_content_rows(content_rows, BROWSE_METRICS, item_count, selected)
}

/// 计数行（内容区第 1 行）。
pub(super) fn header_line(count: usize, colors: &BrowseRenderColors) -> Line<'static> {
    Line::from(vec![Span::styled(
        format!("  {} providers configured", count),
        Style::new().fg(colors.text).add_modifier(Modifier::BOLD),
    )])
}

/// 列表体：空列表返回空态提示，否则按视口窗口生成，每项严格 4 行。
pub(super) fn body_lines(
    viewport: ListViewport,
    providers: &[ProviderSummary],
    selected: usize,
    colors: &BrowseRenderColors,
) -> Vec<Line<'static>> {
    if providers.is_empty() {
        return vec![Line::from(vec![Span::styled(
            format!("  {}", i18n::tr("login-empty-hint")),
            Style::new().fg(colors.muted),
        )])];
    }

    let mut lines = Vec::with_capacity(viewport.visible_items * BROWSE_METRICS.item_rows as usize);
    for (index, provider) in viewport.window(providers) {
        lines.extend(item_lines(index == selected, provider, colors));
    }
    lines
}

/// 单项固定 4 行：ID 行、认证状态行、Base URL 行、分隔空行。
fn item_lines(
    is_cursor: bool,
    provider: &ProviderSummary,
    colors: &BrowseRenderColors,
) -> [Line<'static>; 4] {
    let cursor_mark = if is_cursor { ">" } else { " " };
    let row_style = if is_cursor {
        Style::new().fg(colors.marker).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(colors.text)
    };

    let (key_marker, key_color) = if provider.has_chatgpt_plan {
        (i18n::tr("login-browse-plan-unchecked"), colors.muted)
    } else if provider.has_api_key {
        ("api key: configured".to_string(), colors.success)
    } else {
        ("api key: missing".to_string(), colors.error)
    };

    [
        Line::from(vec![
            Span::styled(format!(" {} ", cursor_mark), Style::new().fg(colors.marker)),
            Span::styled(
                format!(
                    "{}  ({})",
                    provider.id,
                    provider_type_display(&provider.provider_type)
                ),
                row_style,
            ),
        ]),
        Line::from(vec![Span::styled(
            format!("   {}", key_marker),
            Style::new().fg(key_color),
        )]),
        Line::from(vec![Span::styled(
            format!("   base url: {}", base_url_display(provider)),
            Style::new().fg(colors.dim),
        )]),
        Line::from(""),
    ]
}

/// Base URL 展示值：缺省用 `—` 占位，保证每项行高一致。
fn base_url_display(provider: &ProviderSummary) -> String {
    provider
        .base_url
        .as_deref()
        .map(|url| url.chars().take(MAX_BASE_URL_CHARS).collect())
        .unwrap_or_else(|| "—".to_string())
}

/// 操作提示行（内容区最后一行）；文案与按键与既有 Browse 一致。
pub(super) fn hint_line(colors: &BrowseRenderColors) -> Line<'static> {
    make_hint_line_for_login(
        vec![
            (
                "\u{2191}/\u{2193}".to_string(),
                i18n::tr("hint-login-browse"),
            ),
            ("Enter".to_string(), i18n::tr("hint-login-edit")),
            ("Ctrl+N".to_string(), i18n::tr("hint-login-new")),
            ("Ctrl+D".to_string(), i18n::tr("hint-login-delete")),
            ("Esc".to_string(), i18n::tr("hint-login-close")),
        ],
        colors.dim,
        colors.accent,
    )
}
