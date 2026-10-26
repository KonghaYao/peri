//! Login Edit 表单的行语法与列度量。
//!
//! [Why] 旧实现把「标签: 值」直接拼成一行交给 ratatui 硬裁切：标签宽度不同导致值
//! 起始列参差，长路径/JWT 被切且没有省略提示，聚焦时光标还会落到屏幕外。这里把
//! Edit 的视觉规则收敛成一份纯函数：
//!
//! - 分区标题（2 列）+ 单一标签列（按当前语言实际标签宽度，上限 24）+ 值列对齐；
//! - 一个字段占一个语义行（值预算 < 16 列的极窄屏才降级为标签/值两行，两行同一
//!   命中），键盘焦点、鼠标命中与滚动行号仍按「一行一目标」；
//! - 可编辑字段用围绕光标的一行窗口（`‹`/`›` 提示两侧被裁），只读字段中间截断；
//!   警告/错误按显示宽度悬挂换行。
//!
//! 本模块不读全局状态；默认 store 的判断在 `LoginEditState`，不在这里。

use ratatui_kit::ratatui::style::{Color, Modifier, Style};
use ratatui_kit::ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::i18n;
use crate::truncate::{middle_truncate_by_width, truncate_by_width, wrap_by_width};

use super::render::LoginRenderColors;

/// 字段缩进。
pub(super) const INDENT: usize = 4;
/// 焦点标记列宽（`❯ ` / 两个空格）。
const MARKER: usize = 2;
/// 标签列与值列之间的固定间隔。
const LABEL_GAP: usize = 2;
/// 动作行缩进（标签列起点，标记再占 2 列）。
const ACTION_INDENT: usize = INDENT + MARKER;
/// 标签列上限：超过时截断，避免长翻译把值列挤出屏幕。
const MAX_LABEL_COL: usize = 24;
/// 值预算低于该值时降级为“标签行 + 值行”。
const MIN_VALUE_BUDGET: usize = 16;
/// 页面标题/分区标题的缩进。
const SECTION_INDENT: usize = 2;

/// 表单中可能出现的全部字段标签。列宽按本次语言的全部标签测量，而不是当前可见
/// 字段——认证状态变化时值列不会左右跳动。
const FIELD_LABEL_KEYS: &[&str] = &[
    "login-field-type",
    "login-field-name",
    "login-field-auth-mode",
    "login-field-store",
    "login-field-base-url",
    "login-field-api-key",
    "login-field-fable-model",
    "login-field-opus-model",
    "login-field-sonnet-model",
    "login-field-haiku-model",
    "login-status-label",
];

/// 一帧 Edit 表单的列度量。绘制与命中必须使用同一实例。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct EditViewMetrics {
    /// 内容区可用列数（面板宽 - 1，预留最右滚动条列）。
    pub(super) content_width: usize,
    pub(super) label_col: usize,
    pub(super) value_col: usize,
    pub(super) value_budget: usize,
}

impl EditViewMetrics {
    pub(super) fn for_width(width: u16) -> Self {
        let content_width = usize::from(width).saturating_sub(1).max(1);
        let label_col = FIELD_LABEL_KEYS
            .iter()
            .map(|key| i18n::tr(key).width())
            .max()
            .unwrap_or(0)
            .min(MAX_LABEL_COL);
        let value_col = INDENT + MARKER + label_col + LABEL_GAP;
        let value_budget = content_width.saturating_sub(value_col).max(1);
        Self {
            content_width,
            label_col,
            value_col,
            value_budget,
        }
    }

    /// 极窄屏：值列放不下有意义的内容，降级为上下两行。
    pub(super) fn narrow(&self) -> bool {
        self.value_budget < MIN_VALUE_BUDGET
    }
}

/// 页面标题：2 列缩进 + focus 色粗体。
pub(super) fn title_line(
    metrics: &EditViewMetrics,
    text: &str,
    colors: &LoginRenderColors,
) -> Line<'static> {
    Line::from(vec![
        Span::raw(" ".repeat(SECTION_INDENT)),
        Span::styled(
            fit_value(text, metrics.content_width.saturating_sub(SECTION_INDENT)),
            Style::default()
                .fg(colors.focus)
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

/// 分区标题：2 列缩进 + muted 粗体标题（可带 dim 后缀）。
pub(super) fn section_line(
    metrics: &EditViewMetrics,
    title: &str,
    suffix: Option<String>,
    colors: &LoginRenderColors,
) -> Line<'static> {
    let budget = metrics.content_width.saturating_sub(SECTION_INDENT);
    let mut spans = vec![
        Span::raw(" ".repeat(SECTION_INDENT)),
        Span::styled(
            fit_value(title, budget),
            Style::default()
                .fg(colors.muted)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(suffix) = suffix {
        let used = title.width() + 2;
        if used + suffix.width() <= budget {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                fit_value(&suffix, budget.saturating_sub(used)),
                colors.label(),
            ));
        }
    }
    Line::from(spans)
}

/// 保存摘要等“非分区标题”的说明行：2 列缩进 + dim，尾部截断。
pub(super) fn summary_line(
    metrics: &EditViewMetrics,
    text: String,
    colors: &LoginRenderColors,
) -> Line<'static> {
    let budget = metrics.content_width.saturating_sub(SECTION_INDENT);
    Line::from(vec![
        Span::raw(" ".repeat(SECTION_INDENT)),
        Span::styled(fit_value(&text, budget), colors.label()),
    ])
}

/// 可编辑文本字段（聚焦时水平窗口保证光标可见；未聚焦时尾部截断）。
///
/// 极窄屏返回两行（标签、值），调用方必须给两行同一个命中。
pub(super) fn text_field_lines(
    metrics: &EditViewMetrics,
    label: &str,
    value: &str,
    cursor: usize,
    focused: bool,
    colors: &LoginRenderColors,
) -> Vec<Line<'static>> {
    if metrics.narrow() {
        let budget = metrics.content_width.saturating_sub(ACTION_INDENT).max(1);
        return vec![
            narrow_label_line(label, focused, colors),
            Line::from(prefix_spans(
                ACTION_INDENT,
                value_spans(value, cursor, budget, focused, colors),
            )),
        ];
    }
    let mut spans = field_prefix_spans(metrics, focused, label, colors);
    spans.extend(value_spans(
        value,
        cursor,
        metrics.value_budget,
        focused,
        colors,
    ));
    vec![Line::from(spans)]
}

/// choice 字段（类型、认证方式、档位选择器、绑定复选项）。
pub(super) fn choice_field_lines(
    metrics: &EditViewMetrics,
    label: &str,
    value: String,
    hint: Option<String>,
    focused: bool,
    colors: &LoginRenderColors,
) -> Vec<Line<'static>> {
    if metrics.narrow() {
        let budget = metrics.content_width.saturating_sub(ACTION_INDENT).max(1);
        let mut spans = vec![Span::styled(
            fit_value(&value, budget),
            Style::default().fg(colors.text),
        )];
        if let Some(hint) = hint
            && value.width() + 2 + hint.width() <= budget
        {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(hint, colors.label()));
        }
        return vec![
            narrow_label_line(label, focused, colors),
            Line::from(prefix_spans(ACTION_INDENT, spans)),
        ];
    }
    let mut spans = field_prefix_spans(metrics, focused, label, colors);
    spans.push(Span::styled(
        fit_value(&value, metrics.value_budget),
        Style::default().fg(colors.text),
    ));
    if let Some(hint) = hint
        && value.width() + 2 + hint.width() <= metrics.value_budget
    {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(hint, colors.label()));
    }
    vec![Line::from(spans)]
}

/// 只读 K/V 行：无命中，值中间截断；后缀放不下时落到值列续行。
pub(super) fn readonly_kv_lines(
    metrics: &EditViewMetrics,
    label: &str,
    value: &str,
    suffix: Option<&str>,
    colors: &LoginRenderColors,
) -> Vec<Line<'static>> {
    let (indent, budget) = if metrics.narrow() {
        (
            ACTION_INDENT,
            metrics.content_width.saturating_sub(ACTION_INDENT).max(1),
        )
    } else {
        (metrics.value_col, metrics.value_budget)
    };
    let mut spans = if metrics.narrow() {
        vec![Span::raw(" ".repeat(indent))]
    } else {
        readonly_prefix_spans(metrics, label, colors)
    };
    spans.push(Span::styled(
        middle_truncate_by_width(value, budget),
        Style::default().fg(colors.dim),
    ));
    let mut suffix_line = None;
    if let Some(suffix) = suffix {
        if value.width() + 2 + suffix.width() <= budget {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                format!("({suffix})"),
                Style::default().fg(colors.dim),
            ));
        } else {
            suffix_line = Some(continuation_line(metrics, suffix, colors));
        }
    }
    let mut lines = Vec::new();
    if metrics.narrow() {
        lines.push(narrow_label_line(label, false, colors));
    }
    lines.push(Line::from(spans));
    if let Some(line) = suffix_line {
        lines.push(line);
    }
    lines
}

/// 字段续行（锁定提示等）：值列起点 + dim 文本，与字段同一命中。
pub(super) fn continuation_line(
    metrics: &EditViewMetrics,
    text: &str,
    colors: &LoginRenderColors,
) -> Line<'static> {
    let (indent, budget) = if metrics.narrow() {
        (
            ACTION_INDENT,
            metrics.content_width.saturating_sub(ACTION_INDENT).max(1),
        )
    } else {
        (metrics.value_col, metrics.value_budget)
    };
    Line::from(prefix_spans(
        indent,
        vec![Span::styled(fit_value(text, budget), colors.label())],
    ))
}

/// 状态点色调。颜色只落在状态点上，正文保持可读色（警告/错误除外）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StatusTone {
    Success,
    Info,
    Warning,
    Error,
    Muted,
}

/// 一条登录状态。`max_lines` 限制悬挂换行数（长 email 最多 2 行，其余截断）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StatusEntry {
    pub(super) text: String,
    pub(super) tone: StatusTone,
    pub(super) max_lines: Option<usize>,
}

impl StatusEntry {
    pub(super) fn new(text: impl Into<String>, tone: StatusTone) -> Self {
        Self {
            text: text.into(),
            tone,
            max_lines: None,
        }
    }

    pub(super) fn limited(text: impl Into<String>, tone: StatusTone, max_lines: usize) -> Self {
        Self {
            text: text.into(),
            tone,
            max_lines: Some(max_lines),
        }
    }
}

/// 登录状态组：标签只出现在第一行，后续状态悬挂到值列。
pub(super) fn status_lines(
    metrics: &EditViewMetrics,
    label: &str,
    entries: &[StatusEntry],
    colors: &LoginRenderColors,
) -> Vec<Line<'static>> {
    let Some(first) = entries.first() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if metrics.narrow() {
        out.push(narrow_label_line(label, false, colors));
        for entry in entries {
            push_status_entry(&mut out, metrics, None, entry, colors);
        }
    } else {
        push_status_entry(&mut out, metrics, Some(label), first, colors);
        for entry in &entries[1..] {
            push_status_entry(&mut out, metrics, None, entry, colors);
        }
    }
    out
}

fn push_status_entry(
    out: &mut Vec<Line<'static>>,
    metrics: &EditViewMetrics,
    label: Option<&str>,
    entry: &StatusEntry,
    colors: &LoginRenderColors,
) {
    let narrow = metrics.narrow();
    let budget = if narrow {
        metrics
            .content_width
            .saturating_sub(ACTION_INDENT + 2)
            .max(1)
    } else {
        metrics.value_budget.saturating_sub(2).max(1)
    };
    let prefix = match label {
        Some(label) => field_prefix_spans(metrics, false, label, colors),
        None => prefix_spans(
            if narrow {
                ACTION_INDENT
            } else {
                metrics.value_col
            },
            Vec::new(),
        ),
    };
    let chunks = wrap_limited(&entry.text, budget, entry.max_lines);
    for (index, chunk) in chunks.iter().enumerate() {
        let mut spans = if index == 0 {
            prefix.clone()
        } else {
            prefix_spans(
                if narrow {
                    ACTION_INDENT + 2
                } else {
                    metrics.value_col + 2
                },
                Vec::new(),
            )
        };
        if index == 0 {
            spans.push(Span::styled("● ", dot_style(entry.tone, colors)));
        }
        spans.push(Span::styled(chunk.clone(), body_style(entry.tone, colors)));
        out.push(Line::from(spans));
    }
}

/// 动作组标题（无命中）。
pub(super) fn action_group_line(
    metrics: &EditViewMetrics,
    label: &str,
    colors: &LoginRenderColors,
) -> Line<'static> {
    Line::from(prefix_spans(
        INDENT,
        vec![Span::styled(
            fit_value(label, metrics.content_width.saturating_sub(INDENT)),
            Style::default()
                .fg(colors.muted)
                .add_modifier(Modifier::BOLD),
        )],
    ))
}

/// 单行动作区中的一个可点击标签（内容列区间，`end` 不含）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ActionSpan {
    /// 对应调用方动作列表的下标（窗口裁剪后仍指向原动作）。
    pub(super) index: usize,
    pub(super) start: usize,
    pub(super) end: usize,
}

/// 每个动作前的标记槽宽度（`❯ `/`» `/两个空格）；槽后固定 1 空格，
/// 因此未选中动作之间的可见间隔为 3 列。
const ACTION_MARKER: usize = 2;
const ACTION_GAP: usize = 1;

/// 认证动作单行：全部动作横向排列，绝不换行。
///
/// [Why] 认证动作只有 3-4 个短标签，竖排会占掉 4 行且与字段层级混淆。单行排列
/// 在 80 列仍能完整显示中文/英文四动作；更窄时只显示包含选中项的一段并用
/// `‹`/`›` 标明两侧还有动作，选中项始终可见。标记槽跟随选中动作，`❯`/`»`
/// 永远紧邻当前焦点项。
pub(super) fn action_row(
    metrics: &EditViewMetrics,
    labels: &[String],
    selected: Option<usize>,
    focused: bool,
    colors: &LoginRenderColors,
) -> (Line<'static>, Vec<ActionSpan>) {
    if labels.is_empty() {
        return (Line::from(""), Vec::new());
    }
    let n = labels.len();
    let widths: Vec<usize> = labels
        .iter()
        .map(|label| format!("[{label}]").width())
        .collect();
    let item_widths: Vec<usize> = widths.iter().map(|width| width + ACTION_MARKER).collect();
    let available = metrics.content_width.saturating_sub(ACTION_INDENT).max(1);
    let selected = selected.map(|index| index.min(n - 1));
    let full_row_width = |gap: usize| item_widths.iter().sum::<usize>() + gap * (n - 1);
    let (visible, gap) = if full_row_width(ACTION_GAP) <= available {
        ((0..n).collect::<Vec<_>>(), ACTION_GAP)
    } else {
        let anchor = selected.unwrap_or(0);
        let (start, end) = action_window(&item_widths, anchor, available, ACTION_GAP);
        ((start..=end).collect::<Vec<_>>(), ACTION_GAP)
    };

    let mut spans = vec![Span::raw(" ".repeat(ACTION_INDENT))];
    let mut column = ACTION_INDENT;
    let has_left = visible.first().copied().unwrap_or(0) > 0;
    if has_left {
        spans.push(Span::styled("‹", colors.label()));
        column += 1;
    }
    let has_right = visible.last().is_some_and(|index| index + 1 < labels.len());
    let mut ranges = Vec::with_capacity(visible.len());
    for (position, &index) in visible.iter().enumerate() {
        if position > 0 {
            spans.push(Span::raw(" ".repeat(gap)));
            column += gap;
        }
        let marker = if selected == Some(index) {
            if focused { "❯ " } else { "» " }
        } else {
            "  "
        };
        spans.push(Span::styled(
            marker,
            Style::default().fg(if focused && selected == Some(index) {
                colors.cursor
            } else {
                colors.dim
            }),
        ));
        column += ACTION_MARKER;
        // 单标签也放不下时截断；右侧指示符保留 1 列。
        let reserve = usize::from(has_right);
        let max_label = metrics
            .content_width
            .saturating_sub(column + reserve)
            .max(1);
        let label = fit_value(&format!("[{}]", labels[index]), max_label);
        let width = label.width();
        let start = column;
        let style = if selected == Some(index) {
            if focused {
                Style::default()
                    .fg(colors.focus)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(colors.text)
            }
        } else {
            Style::default().fg(colors.dim)
        };
        spans.push(Span::styled(label, style));
        column += width;
        ranges.push(ActionSpan {
            index,
            start,
            end: column,
        });
    }
    if has_right {
        spans.push(Span::styled("›", colors.label()));
    }
    (Line::from(spans), ranges)
}

/// 选择包含 `anchor` 且能放进 `available` 列的最长连续动作区间。
fn action_window(
    item_widths: &[usize],
    anchor: usize,
    available: usize,
    gap: usize,
) -> (usize, usize) {
    let mut best = (anchor, anchor);
    let mut best_count = 0usize;
    for start in (0..=anchor).rev() {
        for end in anchor..item_widths.len() {
            let items: usize = item_widths[start..=end].iter().sum();
            let gaps = gap * (end - start);
            let indicators = usize::from(start > 0) + usize::from(end + 1 < item_widths.len());
            if items + gaps + indicators <= available {
                let count = end - start + 1;
                if count > best_count || (count == best_count && end > best.1) {
                    best = (start, end);
                    best_count = count;
                }
            }
        }
    }
    if best_count == 0 {
        (anchor, anchor)
    } else {
        best
    }
}

/// 按钮行（Confirm）：4 列缩进 + 焦点标记 + `[标签]`。
pub(super) fn button_line(
    metrics: &EditViewMetrics,
    label: &str,
    focused: bool,
    colors: &LoginRenderColors,
) -> Line<'static> {
    let marker = if focused { "❯ " } else { "  " };
    let style = if focused {
        Style::default()
            .fg(colors.focus)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(colors.dim)
    };
    let budget = metrics.content_width.saturating_sub(INDENT + MARKER).max(1);
    Line::from(vec![
        Span::raw(" ".repeat(INDENT)),
        Span::styled(marker, Style::default().fg(colors.cursor)),
        Span::styled(fit_value(&format!("[{label}]"), budget), style),
    ])
}

/// 警告行：`⚠ ` 前缀 + 悬挂换行（续行缩进到 6 列）。
pub(super) fn warning_lines(
    metrics: &EditViewMetrics,
    text: &str,
    colors: &LoginRenderColors,
) -> Vec<Line<'static>> {
    marker_lines(metrics, text, colors.warning)
}

/// 错误行：与警告同构，使用 error 色并全量换行（不截断）。
pub(super) fn error_lines(
    metrics: &EditViewMetrics,
    text: &str,
    colors: &LoginRenderColors,
) -> Vec<Line<'static>> {
    marker_lines(metrics, text, colors.error)
}

fn marker_lines(metrics: &EditViewMetrics, text: &str, color: Color) -> Vec<Line<'static>> {
    let budget = metrics.content_width.saturating_sub(ACTION_INDENT).max(1);
    let style = Style::default().fg(color);
    wrap_by_width(text, budget)
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            if index == 0 {
                Line::from(vec![
                    Span::raw(" ".repeat(INDENT)),
                    Span::styled("⚠ ", style),
                    Span::styled(chunk, style),
                ])
            } else {
                Line::from(prefix_spans(
                    ACTION_INDENT,
                    vec![Span::styled(chunk, style)],
                ))
            }
        })
        .collect()
}

/// 操作提示/说明：4 列缩进 + dim，按宽度悬挂换行（不截断）。
pub(super) fn note_lines(
    metrics: &EditViewMetrics,
    text: &str,
    colors: &LoginRenderColors,
) -> Vec<Line<'static>> {
    let budget = metrics.content_width.saturating_sub(INDENT).max(1);
    wrap_by_width(text, budget)
        .into_iter()
        .map(|chunk| {
            Line::from(prefix_spans(
                INDENT,
                vec![Span::styled(chunk, colors.label())],
            ))
        })
        .collect()
}

/// 空行。
pub(super) fn blank_line() -> Line<'static> {
    Line::from("")
}

/// 聚焦值的水平窗口：`start..end` 为可见字符区间，光标保证在窗口内。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CursorWindow {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) cursor: usize,
    pub(super) show_left: bool,
    pub(super) show_right: bool,
}

/// 计算围绕光标的一行窗口（按显示宽度，CJK/emoji 不切半字）。
pub(super) fn cursor_window(value: &str, cursor: usize, budget: usize) -> CursorWindow {
    let chars: Vec<char> = value.chars().collect();
    let widths: Vec<usize> = chars
        .iter()
        .map(|ch| ch.width().unwrap_or(0).max(usize::from(!ch.is_control())))
        .collect();
    let len = chars.len();
    let cursor = cursor.min(len);
    let budget = budget.max(1);
    let total: usize = widths.iter().sum();
    let cursor_cell = if cursor >= len {
        1
    } else {
        widths[cursor].max(1)
    };
    if total + usize::from(cursor >= len) <= budget {
        return CursorWindow {
            start: 0,
            end: len,
            cursor,
            show_left: false,
            show_right: false,
        };
    }
    if budget < 4 {
        // 只放得下光标附近的一个字符；标记占列会把内容挤没。
        let start = cursor.min(len.saturating_sub(1));
        return CursorWindow {
            start,
            end: (start + 1).min(len),
            cursor,
            show_left: false,
            show_right: false,
        };
    }
    // 从光标向左扩展，右侧预留 2 列（右标记 + 1 列上下文），保证光标可见。
    let mut start = cursor;
    while start > 0 {
        let candidate = start - 1;
        let left_marker = usize::from(candidate > 0);
        let used: usize = widths[candidate..cursor].iter().sum();
        let right_reserve = if cursor < len { 2 } else { 1 };
        if used + cursor_cell + left_marker + right_reserve <= budget {
            start = candidate;
        } else {
            break;
        }
    }
    let left_marker = usize::from(start > 0);
    let mut end = start;
    let mut used = 0usize;
    while end < len {
        let width = widths[end];
        let right_marker = usize::from(end + 1 < len);
        if left_marker + used + width + right_marker > budget {
            break;
        }
        used += width;
        end += 1;
    }
    CursorWindow {
        start,
        end,
        cursor,
        show_left: start > 0,
        show_right: end < len,
    }
}

/// 字段值：聚焦时输出光标窗口，否则尾部截断/占位。
fn value_spans(
    value: &str,
    cursor: usize,
    budget: usize,
    focused: bool,
    colors: &LoginRenderColors,
) -> Vec<Span<'static>> {
    if !focused {
        if value.is_empty() {
            return vec![Span::styled(
                fit_value(&i18n::tr("login-no-model"), budget),
                colors.label(),
            )];
        }
        return vec![Span::styled(
            fit_value(value, budget),
            Style::default().fg(colors.text),
        )];
    }
    let window = cursor_window(value, cursor, budget);
    let chars: Vec<char> = value.chars().collect();
    let mut spans = Vec::new();
    if window.show_left {
        spans.push(Span::styled("‹", colors.label()));
    }
    for index in window.start..window.end {
        let ch = chars[index];
        let style = if index == window.cursor {
            Style::default()
                .fg(colors.text)
                .bg(colors.cursor)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(colors.text)
        };
        spans.push(Span::styled(ch.to_string(), style));
    }
    if window.cursor >= chars.len() && window.end >= chars.len() {
        spans.push(Span::styled(" ", Style::default().bg(colors.cursor)));
    }
    if window.show_right {
        spans.push(Span::styled("›", colors.label()));
    }
    spans
}

fn narrow_label_line(label: &str, focused: bool, colors: &LoginRenderColors) -> Line<'static> {
    let marker = if focused { "❯ " } else { "  " };
    let label_style = if focused {
        colors.focused_label()
    } else {
        colors.label()
    };
    Line::from(vec![
        Span::raw(" ".repeat(INDENT)),
        Span::styled(marker, Style::default().fg(colors.cursor)),
        Span::styled(label.to_string(), label_style),
    ])
}

fn field_prefix_spans(
    metrics: &EditViewMetrics,
    focused: bool,
    label: &str,
    colors: &LoginRenderColors,
) -> Vec<Span<'static>> {
    let marker = if focused { "❯ " } else { "  " };
    let label_style = if focused {
        colors.focused_label()
    } else {
        colors.label()
    };
    vec![
        Span::raw(" ".repeat(INDENT)),
        Span::styled(marker, Style::default().fg(colors.cursor)),
        Span::styled(pad_label(label, metrics.label_col), label_style),
        Span::raw(" ".repeat(LABEL_GAP)),
    ]
}

fn readonly_prefix_spans(
    metrics: &EditViewMetrics,
    label: &str,
    colors: &LoginRenderColors,
) -> Vec<Span<'static>> {
    vec![
        Span::raw(" ".repeat(INDENT + MARKER)),
        Span::styled(pad_label(label, metrics.label_col), colors.label()),
        Span::raw(" ".repeat(LABEL_GAP)),
    ]
}

fn prefix_spans(indent: usize, mut spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    let mut out = vec![Span::raw(" ".repeat(indent))];
    out.append(&mut spans);
    out
}

fn pad_label(label: &str, label_col: usize) -> String {
    let width = label.width();
    if width < label_col {
        format!("{label}{}", " ".repeat(label_col - width))
    } else if width == label_col {
        label.to_string()
    } else {
        fit_value(label, label_col)
    }
}

/// 按显示宽度装进 `budget`；超宽时留 1 列给省略号。
fn fit_value(value: &str, budget: usize) -> String {
    if value.width() <= budget {
        value.to_string()
    } else {
        truncate_by_width(value, budget.saturating_sub(1))
    }
}

fn wrap_limited(text: &str, budget: usize, max_lines: Option<usize>) -> Vec<String> {
    let mut chunks = wrap_by_width(text, budget.max(1));
    if let Some(max) = max_lines
        && chunks.len() > max
    {
        chunks.truncate(max);
        if let Some(last) = chunks.last_mut() {
            let fitted = fit_value(last, budget.saturating_sub(1));
            *last = format!("{fitted}…");
        }
    }
    chunks
}

fn dot_style(tone: StatusTone, colors: &LoginRenderColors) -> Style {
    let color = match tone {
        StatusTone::Success => colors.focus,
        StatusTone::Info | StatusTone::Muted => colors.dim,
        StatusTone::Warning => colors.warning,
        StatusTone::Error => colors.error,
    };
    Style::default().fg(color)
}

fn body_style(tone: StatusTone, colors: &LoginRenderColors) -> Style {
    match tone {
        StatusTone::Warning => Style::default().fg(colors.warning),
        StatusTone::Error => Style::default().fg(colors.error),
        _ => Style::default().fg(colors.text),
    }
}

#[cfg(test)]
#[path = "edit_view_test.rs"]
mod tests;
