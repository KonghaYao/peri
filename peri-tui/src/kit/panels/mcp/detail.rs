//! MCP 详情视图的行构造与命中几何（给定 server 摘要与面板宽度 → 渲染行）。
//!
//! 从 `mcp.rs` 拆出：[Why] 组件装配、事件路由与详情布局同处一个文件会越过单文件
//! 上限（STD-SIZE-001）。本模块只做纯推导——不触碰 atom、不发 RPC、不写状态；
//! 渲染、鼠标命中与键盘选择共用同一份布局结果，避免三处偏移各自漂移。

use crate::i18n;
use crate::kit::atoms::{McpResourceSummary, McpServerSummary};
use crate::kit::text_util::wrap_text;
use crate::truncate::truncate_by_width;
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::ratatui::{
    style::{Modifier, Style, Stylize},
    text::{Line, Span},
};

mod tables;
use self::tables::{SectionSpec, TableRow};

/// 详情视图正文换行宽度（错误摘要与 URL 共用）。
///
/// 面板实际宽度未知且行号契约（鼠标命中反推行号）必须与终端宽度无关，故取固定
/// 值：正文缩进 4 列 + 52 列 ≈ 56 列，覆盖默认面板宽度。与 URL 同宽，使两块
/// 长文本的换行行为一致。
pub(super) const DETAIL_WRAP_WIDTH: usize = 52;

/// 详情视图 header（标题）行号——server 名/状态/版本/授权徽标与右对齐复制按钮
/// 同行。鼠标命中反推用；列偏移由 [`detail_buttons`] 按面板宽度算出。
pub(super) const DETAIL_HEADER_ROW: u16 = 1;

/// header 右对齐元素预留的右边缘列数：ScrollView 的垂直滚动条画在内容最右列
/// （内容溢出时它占该列），右对齐的复制按钮必须避开，否则会被滚动条覆盖。
pub(super) const DETAIL_RIGHT_RESERVE: u16 = 1;

/// 标题与右对齐复制按钮之间的最小间距（列）：两者都可见的前提。
const DETAIL_HEADER_MIN_GAP: u16 = 1;

/// 组件区域尚未测量（首帧）时的内容宽度兜底——仅影响该帧的右对齐位置。
pub(super) const DETAIL_FALLBACK_WIDTH: u16 = 80;

/// 面板侧技能投影：`name` 为 `{server}:{skill}` 全名（来自命令面投影）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SkillEntry {
    pub(super) name: String,
    pub(super) description: String,
}

/// 详情视图可激活操作，视觉顺序：header 复制 → 底行 授权 → 底行 返回。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DetailAction {
    CopyError,
    Auth,
    Restart,
    Stop,
    Back,
}

impl DetailAction {
    fn label(self) -> String {
        match self {
            Self::CopyError => i18n::tr("panel-mcp-detail-btn-copy"),
            Self::Auth => i18n::tr("panel-mcp-detail-btn-auth"),
            Self::Restart => i18n::tr("panel-mcp-detail-btn-restart"),
            Self::Stop => i18n::tr("panel-mcp-detail-btn-stop"),
            Self::Back => i18n::tr("panel-mcp-detail-btn-back"),
        }
    }

    /// 按钮渲染文本：复制按钮沿用 chat md 复制按钮的「左右各 1 空格」块形式
    /// （`message_area/render.rs::copy_button_line`）；其余沿用 chat 选项按钮的
    /// `[ label ]` 形式（`message_area/render/interaction.rs`）。
    pub(super) fn text(self, label: &str) -> String {
        match self {
            Self::CopyError => format!(" {label} "),
            Self::Auth | Self::Restart | Self::Stop | Self::Back => format!("[ {label} ]"),
        }
    }
}

/// 详情视图按钮：操作 + 渲染文本 + 选择索引（`detail_actions` 下标）+ 所在
/// 内容区行号/列偏移 + 命中宽度。渲染、鼠标命中与键盘选择共用这一份布局，
/// 避免三处各自维护偏移而漂移。
#[derive(Debug, Clone)]
pub(super) struct DetailButton {
    pub(super) action: DetailAction,
    /// 渲染文本（随操作形态不同：` 复制 ` / `[ 授权 ]`）
    pub(super) text: String,
    pub(super) sel: usize,
    pub(super) row: u16,
    pub(super) col: u16,
    /// 鼠标命中宽度（列）。右对齐按钮在极窄终端被面板裁切时收窄到可见范围，
    /// 保证命中区间落在面板内。
    pub(super) hit_width: u16,
}

impl DetailButton {
    /// 渲染宽度（显示宽度，CJK label 为双宽字符）。仅布局测试断言使用（生产路径
    /// 在 `detail_buttons` 内按 text 直接计宽），故 cfg(test)。
    #[cfg(test)]
    pub(super) fn width(&self) -> u16 {
        unicode_width::UnicodeWidthStr::width(self.text.as_str()) as u16
    }
}

/// 按钮样式——复用 chat 消息区按钮的视觉（样式为组件内私有内联实现，故在此
/// 精确复刻构造并注明出处；未改动 chat 组件）：
///
/// - 复制（header）：`message_area/render.rs:91` `copy_button_line` 的 md 复制
///   按钮——`fg(accent) + REVERSED` 反色块。选中态叠加 `BOLD`：chat「当前项」
///   样式（`message_area/mod.rs:719`）是 `bg(surface.selection) + BOLD`，反色块下
///   bg 不可见，故取其中的 BOLD 增量。
/// - 授权/返回（底行）：`message_area/render/interaction.rs` 选项（权限）按钮——
///   默认 `fg(text.primary)`，选中态同 `message_area/mod.rs:719` 的
///   `bg(surface.selection) + BOLD`（与改动前的 MCP 底行按钮一致）。
pub(super) fn button_style(
    action: DetailAction,
    selected: bool,
    theme: &peri_theme::theme::ThemeDefinition,
) -> Style {
    let semantic = &theme.semantic;
    match action {
        DetailAction::CopyError => {
            let style = Style::default()
                .fg(semantic.accent)
                .add_modifier(Modifier::REVERSED);
            if selected {
                style.add_modifier(Modifier::BOLD)
            } else {
                style
            }
        }
        DetailAction::Auth | DetailAction::Restart | DetailAction::Stop | DetailAction::Back => {
            let style = Style::default().fg(semantic.text.primary);
            if selected {
                style
                    .bg(semantic.surface.selection)
                    .add_modifier(Modifier::BOLD)
            } else {
                style
            }
        }
    }
}

/// 详情视图按钮的选择索引空间（视觉顺序）：[复制?] → [授权?] → [返回]。
///
/// 无错误摘要时不显示复制按钮——没有可复制的内容，且保持常见（连接失败以外的）
/// 详情布局与行号契约不变。
pub(super) fn detail_actions(s: &McpServerSummary) -> Vec<DetailAction> {
    let mut actions = Vec::with_capacity(3);
    if s.error_summary.is_some() {
        actions.push(DetailAction::CopyError);
    }
    if s.needs_auth {
        actions.push(DetailAction::Auth);
    }
    if s.control.restart {
        actions.push(DetailAction::Restart);
    }
    if s.control.stop {
        actions.push(DetailAction::Stop);
    }
    actions.push(DetailAction::Back);
    actions
}

/// 选择索引 → 操作；索引越界（详情打开期间列表被刷新）返回 None。
pub(super) fn detail_action_at(s: &McpServerSummary, idx: usize) -> Option<DetailAction> {
    detail_actions(s).into_iter().nth(idx)
}

/// 进入详情时的默认选中：显式落在 [ 返回 ]，避免 Enter 误触授权/复制。
pub(super) fn default_detail_btn(s: &McpServerSummary) -> usize {
    detail_actions(s).len().saturating_sub(1)
}

/// 详情视图按钮布局（行号 + 列偏移，列从内容区左边缘计）。
///
/// `skills_count` 参与四段表格的行数推导，渲染与鼠标命中必须传同一个值。
/// `width` 是面板内容宽度：复制按钮右对齐（`width − 滚动条预留 − 按钮宽`），
/// 底行按钮左起排布。
pub(super) fn detail_buttons(
    s: &McpServerSummary,
    skills_count: usize,
    width: u16,
) -> Vec<DetailButton> {
    let bottom_row = detail_btn_row(s, skills_count, width);
    let mut buttons: Vec<DetailButton> = Vec::with_capacity(3);
    // 底行按钮从左到右排列：`[ label ]` 之间 2 空格
    let mut bottom_col = 0u16;
    for (sel, action) in detail_actions(s).into_iter().enumerate() {
        let text = action.text(&action.label());
        let text_width = unicode_width::UnicodeWidthStr::width(text.as_str()) as u16;
        let header = action == DetailAction::CopyError;
        // 右对齐：按钮右缘落在滚动条预留列之前；极窄终端下贴左列渲染（被面板
        // 裁切也要保持可见可点击——按钮优先于标题）
        let (row, col) = if header {
            (
                DETAIL_HEADER_ROW,
                width.saturating_sub(DETAIL_RIGHT_RESERVE + text_width),
            )
        } else {
            (bottom_row, bottom_col)
        };
        let hit_width = if header {
            text_width.min(width.saturating_sub(col))
        } else {
            text_width
        };
        if !header {
            bottom_col = col + text_width + 2;
        }
        buttons.push(DetailButton {
            action,
            text,
            sel,
            row,
            col,
            hit_width,
        });
    }
    buttons
}

/// 按显示宽度截断样式化片段（保留逐段样式；超长补省略号）。
///
/// 省略号可能多占 1 列——调用方按「最小间距」吸收该溢出。
fn truncate_segments(segments: &[(String, Style)], max_width: u16) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(segments.len());
    let mut remaining = max_width as usize;
    for (text, style) in segments {
        if remaining == 0 {
            break;
        }
        let width = unicode_width::UnicodeWidthStr::width(text.as_str());
        if width <= remaining {
            spans.push(Span::styled(text.clone(), *style));
            remaining -= width;
        } else {
            let cut = truncate_by_width(text, remaining);
            remaining =
                remaining.saturating_sub(unicode_width::UnicodeWidthStr::width(cut.as_str()));
            spans.push(Span::styled(cut, *style));
        }
    }
    spans
}

/// 渲染 header（标题）行：标题片段 +（有错误摘要时）右对齐的复制按钮。
///
/// 窄终端下按键优先：标题截断到「按钮起始列 − 最小间距」，按钮始终渲染并保持
/// 右对齐（`DetailButton::col` 与鼠标命中同源）；没有按钮时原样渲染标题。
fn render_header_row(
    title: &[(String, Style)],
    copy: Option<&DetailButton>,
    sel: usize,
    theme: &peri_theme::theme::ThemeDefinition,
) -> Line<'static> {
    let Some(button) = copy else {
        return Line::from(
            title
                .iter()
                .map(|(text, style)| Span::styled(text.clone(), *style))
                .collect::<Vec<_>>(),
        );
    };
    let mut spans = truncate_segments(title, button.col.saturating_sub(DETAIL_HEADER_MIN_GAP));
    let used: u16 = spans
        .iter()
        .map(|span| unicode_width::UnicodeWidthStr::width(span.content.as_ref()) as u16)
        .sum();
    spans.push(Span::raw(
        " ".repeat(button.col.saturating_sub(used) as usize),
    ));
    spans.push(Span::styled(
        button.text.clone(),
        button_style(button.action, button.sel == sel, theme),
    ));
    Line::from(spans)
}

/// 工具行：按名称排序；展示条数可能小于全量 `tools_count`。
fn tool_rows(s: &McpServerSummary) -> Vec<TableRow> {
    let mut rows: Vec<TableRow> = s
        .tools
        .iter()
        .map(|tool| TableRow {
            name: tool.name.clone(),
            description: tool.description.clone().unwrap_or_default(),
        })
        .collect();
    rows.sort_by(|left, right| left.name.cmp(&right.name));
    rows
}

/// 技能行：按全名（`{server}:{skill}`）排序。
fn skill_rows(skills: &[SkillEntry]) -> Vec<TableRow> {
    let mut rows: Vec<TableRow> = skills
        .iter()
        .map(|skill| TableRow {
            name: skill.name.clone(),
            description: skill.description.clone(),
        })
        .collect();
    rows.sort_by(|left, right| left.name.cmp(&right.name));
    rows
}

/// 代理行：只从 `agent://` 资源派生，按名称排序。
fn agent_rows(s: &McpServerSummary) -> Vec<TableRow> {
    let mut rows: Vec<TableRow> = s.resources.iter().filter_map(agent_row).collect();
    rows.sort_by(|left, right| left.name.cmp(&right.name));
    rows
}

/// 单个 `agent://` 资源 → 代理行；非 agent 资源返回 None。
///
/// 名称取最终 `agent.md` 段之前的一段，覆盖
/// `agent://builtin/{id}/agent.md`、`agent://project/{id}/agent.md`、
/// `agent://plugin/{plugin}/{id}/agent.md`、`agent://remote/{id}/agent.md`；
/// 结构不符时回退 resource.name，再回退 URI。
fn agent_row(resource: &McpResourceSummary) -> Option<TableRow> {
    let scheme_end = resource.uri.find("://")?;
    if !resource.uri[..scheme_end].eq_ignore_ascii_case("agent") {
        return None;
    }
    let path = &resource.uri[scheme_end + 3..];
    // query / fragment 不属于路径段，避免把参数误当代理名。
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let segments: Vec<&str> = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    let name = segments
        .iter()
        .rposition(|segment| segment.eq_ignore_ascii_case("agent.md"))
        .and_then(|index| index.checked_sub(1).map(|previous| segments[previous]))
        .map(str::to_string)
        .or_else(|| resource.name.clone().filter(|name| !name.trim().is_empty()))
        .unwrap_or_else(|| resource.uri.clone());
    Some(TableRow {
        name,
        description: resource.description.clone().unwrap_or_default(),
    })
}

/// 订阅行：资源行（非空才显示）+ 三个列表变化标志行。
fn subscription_rows(s: &McpServerSummary) -> Vec<TableRow> {
    let Some(subscriptions) = s.subscriptions.as_ref().filter(|sub| !sub.is_empty()) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    if !subscriptions.resources.is_empty() {
        rows.push(TableRow {
            name: i18n::tr("panel-mcp-sub-resources"),
            description: subscriptions.resources.join(", "),
        });
    }
    let on = i18n::tr("panel-mcp-sub-on");
    let off = i18n::tr("panel-mcp-sub-off");
    for (key, enabled) in [
        (
            "panel-mcp-sub-tools-list-changed",
            subscriptions.tools_list_changed,
        ),
        (
            "panel-mcp-sub-prompts-list-changed",
            subscriptions.prompts_list_changed,
        ),
        (
            "panel-mcp-sub-resources-list-changed",
            subscriptions.resources_list_changed,
        ),
    ] {
        rows.push(TableRow {
            name: i18n::tr(key),
            description: if enabled { on.clone() } else { off.clone() },
        });
    }
    rows
}

/// 订阅标题计数：资源数 + 开启的列表变化标志数（与显示行数不同）。
fn subscription_count(s: &McpServerSummary) -> usize {
    s.subscriptions
        .as_ref()
        .filter(|subscriptions| !subscriptions.is_empty())
        .map_or(0, |subscriptions| {
            subscriptions.resources.len()
                + usize::from(subscriptions.tools_list_changed)
                + usize::from(subscriptions.prompts_list_changed)
                + usize::from(subscriptions.resources_list_changed)
        })
}

/// 四段表格的总行数（含每段前的空行；与渲染路径逐行同源）。
///
/// `width` 参与行数推导：面板过窄、表格退回占位行时，每段从「表格」收缩为
/// 单行占位，按钮行号随之同步上移（渲染与鼠标命中共用同一宽度）。
fn tables_line_count(s: &McpServerSummary, skills_count: usize, width: u16) -> u16 {
    let tools_more = s.tools_count > s.tools.len();
    let tools = tables::section_line_count(s.tools.len(), tools_more, width);
    let skills = tables::section_line_count(skills_count, false, width);
    let agents = tables::section_line_count(agent_rows(s).len(), false, width);
    let subscriptions = tables::section_line_count(subscription_rows(s).len(), false, width);
    (tools + skills + agents + subscriptions) as u16
}

/// 详情视图底行按钮行号（渲染 / 鼠标命中反推共用契约）。
///
/// 内容区行号 0 起：空行、标题（+右对齐复制按钮）、cache、protocol、connected、
/// 空行、[错误标题 + 错误×n + 空行]、URL 标签、URL×n、四段表格（工具、技能、
/// 代理、订阅，每段含前导空行）、空行 → 按钮行
/// = 8 + url_lines + (错误行数 + 2) + tables_total。
pub(super) fn detail_btn_row(s: &McpServerSummary, skills_count: usize, width: u16) -> u16 {
    let url_lines = s
        .url
        .as_ref()
        .map(|url| wrap_text(url, DETAIL_WRAP_WIDTH).len() as u16)
        .unwrap_or(1);
    let error_block = detail_error_lines(s)
        .map(|lines| lines as u16 + 2)
        .unwrap_or(0);
    8 + url_lines + error_block + tables_line_count(s, skills_count, width)
}

/// 错误摘要换行后的行数；None = 无错误摘要。
pub(super) fn detail_error_lines(s: &McpServerSummary) -> Option<usize> {
    s.error_summary
        .as_deref()
        .map(|error| wrap_text(error, DETAIL_WRAP_WIDTH).len())
}

/// 渲染底行按钮行：`DetailButton::col` 与 `render_button_row` 的输出列一一对应
/// （左起排布、按钮间 2 空格），样式见 [`button_style`]。
fn render_button_row(
    buttons: &[DetailButton],
    sel: usize,
    theme: &peri_theme::theme::ThemeDefinition,
) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(buttons.len() * 2);
    for (i, button) in buttons.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            button.text.clone(),
            button_style(button.action, button.sel == sel, theme),
        ));
    }
    Line::from(spans)
}

/// RFC3339（UTC）时间戳 → 本地时间指定格式；解析失败返回 None（界面不显示）。
pub(super) fn format_connected_at(value: &str, fmt: &str) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|utc| utc.with_timezone(&chrono::Local).format(fmt).to_string())
}

fn cache_status_label(status: Option<&str>) -> Option<String> {
    cache_status_label_key(status).map(i18n::tr)
}

pub(super) fn cache_status_label_key(status: Option<&str>) -> Option<&'static str> {
    match status {
        Some("version_cached") => Some("panel-mcp-cache-version-hit"),
        Some("mcpp_cached") => Some("panel-mcp-cache-protocol-hit"),
        Some("cached") => Some("panel-mcp-cache-hit"),
        Some("stored_after_fetch") => Some("panel-mcp-cache-saved"),
        Some("cache_ready") => Some("panel-mcp-cache-ready"),
        Some("cache_disabled") => Some("panel-mcp-cache-disabled"),
        Some("cache_disabled_by_config") => Some("panel-mcp-cache-disabled-config"),
        Some("cache_pending") => Some("panel-mcp-cache-pending"),
        Some("live_fetch") => Some("panel-mcp-cache-live-fetch"),
        _ => None,
    }
}

/// 追加四段详情表格：工具、技能、代理、订阅（顺序固定）。
///
/// 每段前导空行也由 [`tables::push_section`] 写入；行数与
/// [`tables_line_count`] 同源（含宽度导致的占位回退），供鼠标命中反推。
fn push_detail_tables(
    lines: &mut Vec<Line<'static>>,
    s: &McpServerSummary,
    skills: &[SkillEntry],
    width: u16,
    theme: &peri_theme::theme::ThemeDefinition,
) {
    let data_headers = (
        i18n::tr("panel-mcp-table-col-name"),
        i18n::tr("panel-mcp-table-col-description"),
    );
    let subscription_headers = (
        i18n::tr("panel-mcp-table-col-item"),
        i18n::tr("panel-mcp-table-col-value"),
    );
    let tool_rows = tool_rows(s);
    let tool_more = (s.tools_count > s.tools.len()).then(|| {
        i18n::tr_args(
            "panel-mcp-table-more",
            &[(
                "count".to_string(),
                FluentValue::from((s.tools_count - s.tools.len()) as i64),
            )],
        )
    });
    tables::push_section(
        lines,
        &SectionSpec {
            label: &i18n::tr("panel-mcp-table-tools"),
            count: s.tools_count,
            headers: (data_headers.0.as_str(), data_headers.1.as_str()),
            rows: &tool_rows,
            more: tool_more,
        },
        width,
        theme,
    );

    let skill_rows = skill_rows(skills);
    tables::push_section(
        lines,
        &SectionSpec {
            label: &i18n::tr("panel-mcp-table-skills"),
            count: skill_rows.len(),
            headers: (data_headers.0.as_str(), data_headers.1.as_str()),
            rows: &skill_rows,
            more: None,
        },
        width,
        theme,
    );

    let agent_rows = agent_rows(s);
    tables::push_section(
        lines,
        &SectionSpec {
            label: &i18n::tr("panel-mcp-table-agents"),
            count: agent_rows.len(),
            headers: (data_headers.0.as_str(), data_headers.1.as_str()),
            rows: &agent_rows,
            more: None,
        },
        width,
        theme,
    );

    let subscription_rows = subscription_rows(s);
    tables::push_section(
        lines,
        &SectionSpec {
            label: &i18n::tr("panel-mcp-table-subscriptions"),
            count: subscription_count(s),
            headers: (
                subscription_headers.0.as_str(),
                subscription_headers.1.as_str(),
            ),
            rows: &subscription_rows,
            more: None,
        },
        width,
        theme,
    );
}

/// 构造详情视图内容行（纯函数，测试友好）。
pub(super) fn build_detail_lines(
    s: &McpServerSummary,
    skills: &[SkillEntry],
    btn_sel: usize,
    theme: &peri_theme::theme::ThemeDefinition,
    width: u16,
) -> Vec<Line<'static>> {
    let semantic = &theme.semantic;
    let mut lines: Vec<Line<'_>> = Vec::new();
    lines.push(Line::from(""));
    // 标题（header）：server 名 + 状态 + 版本 + 授权徽标 [+ 右对齐复制按钮]。
    // 片段保留逐段样式，供窄终端下按预算截断（见 render_header_row）。
    let (status_icon, status_color) = derive_status_style(&s.status);
    let mut title: Vec<(String, Style)> = vec![
        (
            format!("  {}", s.name),
            Style::new().fg(theme.component.panel.title).bold(),
        ),
        (format!("  {}", status_icon), Style::new().fg(status_color)),
        (format!(" {}", s.status), Style::new().fg(status_color)),
    ];
    if let Some(version) = &s.version {
        title.push((format!(" · {version}"), Style::new().fg(semantic.text.dim)));
    }
    if s.needs_auth {
        title.push((
            format!("  {}", i18n::tr("panel-mcp-needs-auth")),
            Style::new().fg(semantic.status.warning),
        ));
    }
    // 复制按钮（仅错误摘要存在时；无错误无可复制内容）与底行按钮共用选择索引，
    // 右对齐渲染在 header 行（行号/列偏移由 detail_buttons 单一权威给出）。
    let buttons = detail_buttons(s, skills.len(), width);
    let copy_button = buttons
        .first()
        .filter(|button| button.action == DetailAction::CopyError);
    lines.push(render_header_row(&title, copy_button, btn_sel, theme));
    let cache_label = cache_status_label(s.cache_status.as_deref())
        .unwrap_or_else(|| i18n::tr("panel-mcp-detail-cache-none"));
    lines.push(
        Line::from(vec![
            Span::styled(
                format!("  {} ", i18n::tr("panel-mcp-detail-cache")),
                Style::new().fg(semantic.text.dim),
            ),
            Span::styled(cache_label, Style::new().fg(semantic.text.primary)),
        ])
        .fg(semantic.text.dim),
    );
    // MCP 协议版本（rmcp 协商结果，如 2026-07-28）；未协商出显示占位。
    let protocol_label = s
        .protocol_version
        .clone()
        .unwrap_or_else(|| i18n::tr("ui-empty"));
    lines.push(Line::from(vec![
        Span::styled(
            format!("  {} ", i18n::tr("panel-mcp-detail-protocol")),
            Style::new().fg(semantic.text.dim),
        ),
        Span::styled(protocol_label, Style::new().fg(semantic.text.primary)),
    ]));
    // 连接时间（RFC3339 → 本地完整时间）；未成功连接显示占位。
    let connected_label = s
        .connected_at
        .as_deref()
        .and_then(|value| format_connected_at(value, "%Y-%m-%d %H:%M:%S"))
        .unwrap_or_else(|| i18n::tr("ui-empty"));
    lines.push(Line::from(vec![
        Span::styled(
            format!("  {} ", i18n::tr("panel-mcp-detail-connected")),
            Style::new().fg(semantic.text.dim),
        ),
        Span::styled(connected_label, Style::new().fg(semantic.text.primary)),
    ]));
    lines.push(Line::from(""));
    if let Some(error) = &s.error_summary {
        lines.push(
            Line::from(format!("  {}", i18n::tr("panel-mcp-detail-error")))
                .fg(semantic.status.error),
        );
        // 长错误按面板可用宽度换行完整展示（行数与 detail_btn_row 同源）
        for error_line in wrap_text(error, DETAIL_WRAP_WIDTH) {
            lines.push(Line::from(format!("    {error_line}")).fg(semantic.text.muted));
        }
        lines.push(Line::from(""));
    }
    // URL（HTTP 传输）完整换行展示
    lines.push(Line::from(format!("  {}", i18n::tr("panel-mcp-detail-url"))).fg(semantic.text.dim));
    match &s.url {
        Some(url) => {
            for url_line in wrap_text(url, DETAIL_WRAP_WIDTH) {
                lines.push(Line::from(format!("    {url_line}")).fg(semantic.text.primary));
            }
        }
        None => {
            lines.push(Line::from(format!("    ({})", i18n::tr("ui-empty"))).fg(semantic.text.dim));
        }
    }
    // 四段表格（工具、技能、代理、订阅）：固定在 URL 块之后、底行按钮之前。
    push_detail_tables(&mut lines, s, skills, width, theme);
    lines.push(Line::from(""));
    // 按钮行：header 复制按钮之外的操作（[授权?] [返回]），列偏移与鼠标命中同源
    let bottom_buttons: Vec<DetailButton> = buttons
        .into_iter()
        .filter(|button| button.action != DetailAction::CopyError)
        .collect();
    lines.push(render_button_row(&bottom_buttons, btn_sel, theme));
    lines.push(Line::from(""));
    lines.push(Line::from(i18n::tr("panel-mcp-detail-hint")).fg(semantic.text.dim));
    lines
}

pub(super) fn derive_status_style(status: &str) -> (String, ratatui::style::Color) {
    if status == "connected" {
        (
            i18n::tr("panel-mcp-icon-connected"),
            THEME_ATOM.state().read().semantic.status.success,
        )
    } else if status.contains("error") || status.contains("failed") {
        (
            i18n::tr("panel-mcp-icon-error"),
            THEME_ATOM.state().read().semantic.status.error,
        )
    } else {
        (
            i18n::tr("panel-mcp-icon-unknown"),
            THEME_ATOM.state().read().semantic.text.muted,
        )
    }
}
