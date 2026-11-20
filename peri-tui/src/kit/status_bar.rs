//! ratatui-kit StatusBar component.
//!
//! S9：完整双行布局——
//! - **Row 1**：权限模式 → cwd basename → provider/model → bg tasks
//!   全部从 SERVICE_SNAPSHOT atom 派生（S5 落地）；高亮计时器控制闪烁。
//!   CPU%/MEM/ctx 已迁移 composer footer 资源线（input_area.rs）。
//! - **Row 2**：状态相关的快捷键 hints（popup/mention/slash/默认 4 态切换）。

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms;
use crate::kit::mouse_router;

use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::crossterm::event::Event;
use ratatui_kit::crossterm::event::MouseButton;
use ratatui_kit::crossterm::event::MouseEventKind;
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::layout::Alignment;
use ratatui_kit::ratatui::layout::Constraint;
use ratatui_kit::ratatui::layout::Direction;
use ratatui_kit::ratatui::layout::Flex;
use ratatui_kit::ratatui::layout::Rect;
use ratatui_kit::ratatui::style::Modifier;
use ratatui_kit::ratatui::style::Style;
use ratatui_kit::ratatui::style::Stylize;
use ratatui_kit::ratatui::text::Line;
use ratatui_kit::ratatui::text::Span;
use ratatui_kit::ratatui::widgets::Paragraph;
use std::time::Duration;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// 状态栏第 1 行：权限模式 · cwd · provider/model · bg tasks
///（CPU%/MEM/ctx 已迁移 composer footer 资源线，见 input_area.rs）
#[component]
fn StatusBarRow1(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let _lang = hooks.use_atom(&atoms::LANG_VERSION);
    let snap = hooks.use_atom(&atoms::SERVICE_SNAPSHOT);
    let model_hl = hooks.use_atom(&atoms::MODEL_HIGHLIGHT_UNTIL);
    let provider_hl = hooks.use_atom(&atoms::PROVIDER_HIGHLIGHT_UNTIL);
    let mode_hl = hooks.use_atom(&atoms::MODE_HIGHLIGHT_UNTIL);
    let bg_tasks = hooks.use_atom(&atoms::BG_TASKS);
    let preparing = hooks.use_atom(&atoms::SESSION_PREPARING);
    let goal_store = hooks.use_atom(&atoms::GOAL_SNAPSHOT);

    let snap = snap.read().clone();
    let goal = goal_store.read().clone();
    let now = peri_time::monotonic_now();
    // provider 不单独显示；provider 或 model 任一变化都让模型段闪烁提醒
    let model_highlighted = model_hl.read().as_ref().is_some_and(|t| *t > now)
        || provider_hl.read().as_ref().is_some_and(|t| *t > now);
    let mode_highlighted = mode_hl.read().as_ref().is_some_and(|t| *t > now);

    let mut spans: Vec<Span<'static>> = Vec::new();

    // 1. 权限模式
    let mode_label = permission_mode_display(&snap.permission_mode);
    if !mode_label.is_empty() {
        let color = permission_mode_color(&snap.permission_mode);
        let mut style = Style::default().fg(color);
        if mode_highlighted {
            style = style.add_modifier(Modifier::BOLD | Modifier::SLOW_BLINK);
        }
        spans.push(Span::styled(format!(" {}", mode_label), style));
    }

    // 2. cwd basename
    spans.push(separator());
    spans.push(Span::styled(
        cwd_basename(&snap.cwd),
        Style::default().fg(statusbar().muted),
    ));

    // 3. alias model effort —— 档位名 + 模型名 + 推理力度
    // 由 model_segment_parts 做去重：alias 与模型名相同时只显示一次；
    // 模型名尾部已含 effort 后缀（如 "gpt-5.6-luna high"）时不重复追加。
    // model_start/model_end 记录模型段在 spans 中的索引范围，供鼠标点击区域计算。
    let model_parts = model_segment_parts(&snap.model_alias, &snap.model_name, &snap.effort);
    let mut model_start = None;
    let mut model_end = 0usize;
    if !model_parts.is_empty() {
        model_start = Some(spans.len());
        spans.push(separator());
        let theme = THEME_ATOM.state().read().semantic;
        // 末段是 effort 时拆分为独立 span，但样式与模型名一致（model_info 色，不另用 effort 色）
        let (head, effort_part) =
            if model_parts.len() >= 2 && model_parts.last().is_some_and(|p| p == &snap.effort) {
                let mut h = model_parts.clone();
                let e = h
                    .pop()
                    .expect("model parts contains the effort suffix after the length check");
                (h, Some(e))
            } else {
                (model_parts, None)
            };
        let mut model_style = Style::default().fg(theme.model_info);
        if model_highlighted {
            model_style = model_style.add_modifier(Modifier::BOLD | Modifier::SLOW_BLINK);
        }
        spans.push(Span::styled(head.join(" "), model_style));
        if let Some(e) = effort_part {
            spans.push(Span::styled(format!(" {e}"), model_style));
        }
        model_end = spans.len();
    }

    // 4. 当前 goal（简洁状态，点击打开详情）
    let mut goal_start = None;
    let mut goal_end = 0usize;
    if let Some(goal) = &goal {
        goal_start = Some(spans.len());
        spans.push(separator());
        let status = match goal.status {
            Some(peri_acp_types::goal::GoalStatus::Active) => i18n::tr("goal-status-active"),
            Some(peri_acp_types::goal::GoalStatus::Complete) => i18n::tr("goal-status-complete"),
            Some(peri_acp_types::goal::GoalStatus::Blocked) => i18n::tr("goal-status-blocked"),
            None => i18n::tr("ui-empty"),
        };
        spans.push(Span::styled(
            format!("Goal: {status} · ↻{}", goal.continuation_count),
            Style::default().fg(THEME_ATOM.state().read().semantic.accent),
        ));
        goal_end = spans.len();
    }

    // 5. 后台任务计数
    let bg = bg_tasks.read();
    let shell_c = bg.iter().filter(|t| t.kind == "shell").count();
    let agent_c = bg.iter().filter(|t| t.kind == "agent").count();
    let wf_c = bg.iter().filter(|t| t.kind == "workflow").count();
    if shell_c > 0 || agent_c > 0 || wf_c > 0 {
        spans.push(separator());
        let mut parts = vec![];
        if shell_c > 0 {
            parts.push(format!("{} shell", shell_c));
        }
        if agent_c > 0 {
            parts.push(format!("{} agent", agent_c));
        }
        if wf_c > 0 {
            parts.push(format!("{} workflow", wf_c));
        }
        spans.push(Span::styled(
            parts.join(" "),
            Style::default().fg(THEME_ATOM.state().read().semantic.loading),
        ));
    }

    // 6. 会话建立中
    //    这段窗口里输入既不在待发送队列、也没有发出请求，只有这里能说明正在做什么。
    if let Some(label) = preparing_label(*preparing.read()) {
        spans.push(separator());
        spans.push(Span::styled(
            label,
            Style::default().fg(THEME_ATOM.state().read().semantic.loading),
        ));
    }

    // 一次布局同时产出可见行与命中区间：绘制与点击消费同一份结果，
    // 不再各自模拟折行（窄屏长词、第三行裁剪时两套计算会错位）。
    // 首帧 use_previous_size 返回 width=0：布局退守单行，命中为空。
    let prev_size = hooks.use_previous_size();
    let StatusBarLayout {
        lines,
        model_hits,
        goal_hits,
    } = layout_status_bar(
        &spans,
        prev_size.width,
        ROW1_MAX_LINES,
        model_start.map(|start| start..model_end),
        goal_start.map(|start| start..goal_end),
    );
    let row_height = (lines.len() as u16).max(1);

    // ── 模型段点击区域：鼠标左键点击 alias/model 文本 → 打开快速切换弹窗 ──
    // AreaTracker 值拷贝模式（仿 input_area.rs）：每帧从 hook 取出 rect 副本，
    // 闭包按值捕获，避免 Arc 重建导致 handler 读到 None。
    let row1_area;
    {
        let tracker = hooks.use_hook(|| AreaTracker { rect: None });
        row1_area = tracker.rect;
    }
    hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
        if let Event::Mouse(mouse) = event {
            if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
                return EventResult::Ignored;
            }
            // 弹窗或面板激活时不响应——防止遮挡区域误触（如弹窗覆盖状态栏）
            if mouse_router::is_occluded() {
                return EventResult::Ignored;
            }
            if let Some(area) = row1_area {
                // 命中区间来自与绘制相同的布局：被裁掉的第三行与空白格不再有热区。
                if let Some(anchor_x) = hit_test(&model_hits, area, mouse.column, mouse.row) {
                    // 锚点 = 模型段命中起点屏幕坐标——小弹窗定位在该点上方
                    *atoms::MODEL_SWITCH_ANCHOR.state().write() = Some((anchor_x, mouse.row));
                    crate::kit::popup_overlay::open_popup(atoms::PopupKind::ModelQuickSwitch);
                    return EventResult::Consumed;
                }
                if hit_test(&goal_hits, area, mouse.column, mouse.row).is_some() {
                    crate::kit::panel_registry::open_panel(PanelKind::Goal);
                    return EventResult::Consumed;
                }
            }
        }
        EventResult::Ignored
    });

    element!(
        View(
            flex_direction: Direction::Horizontal,
            width: Constraint::Fill(1),
            height: Constraint::Length(row_height),
        ) {
            Text(text: Paragraph::new(lines))
        }
    )
}

/// 状态栏第 2 行：状态相关的快捷键 hints + 复制提示
#[component]
fn StatusBarRow2(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let _lang = hooks.use_atom(&atoms::LANG_VERSION);
    // I19-C：原代码读 POPUP_ACTIVE（dead atom，open/close_popup 从不同步）
    // 导致 popup hints 永远不显示。改读 POPUP_KIND.is_some()。
    let popup_kind = hooks.use_atom(&atoms::POPUP_KIND);
    let at_active = hooks.use_atom(&atoms::AT_MENTION_ACTIVE);
    let slash_active = hooks.use_atom(&atoms::SLASH_HINT_ACTIVE);
    let copy_until = hooks.use_atom(&atoms::COPY_MESSAGE_UNTIL);
    let copy_count = hooks.use_atom(&atoms::COPY_CHAR_COUNT);
    let quit_pending = hooks.use_atom(&atoms::QUIT_PENDING_SINCE);

    let is_popup = popup_kind.read().is_some();
    let is_at = *at_active.read();
    let is_slash = *slash_active.read();
    let now = peri_time::monotonic_now();

    // 复制提示优先于其他 hints。
    // [TRAP] 只读 atom 判断过期——禁止在 render body 中写 atom（render→write→render 自激）。
    // mark_copy_message 总是用新 Instant 覆盖 atom，旧 Some(until) 残留不影响下次显示。
    let copy_active = copy_until.read().is_some_and(|until| now < until);
    if copy_active {
        let char_count = *copy_count.read();
        let hint = i18n::tr_args(
            "statusbar-copied",
            &[("count".to_string(), FluentValue::from(char_count as u64))],
        );
        return element!(
            View(
                flex_direction: Direction::Horizontal,
                width: Constraint::Fill(1),
                height: Constraint::Length(1),
                justify_content: Flex::Center,
            ) {
                Text(text: Paragraph::new(
                    Line::from(hint).fg(statusbar().text)
                ).centered())
            }
        );
    }

    // Ctrl+C 退出待确认提示——在 hint 行显示，不挤占通知栏。
    let quit_active = quit_pending
        .read()
        .is_some_and(|t| now.duration_since(t) < Duration::from_secs(1));
    if quit_active {
        let hint = i18n::tr("statusbar-hint-quit-pending");
        return element!(
            View(
                flex_direction: Direction::Horizontal,
                width: Constraint::Fill(1),
                height: Constraint::Length(1),
                justify_content: Flex::End,
            ) {
                Text(text: Paragraph::new(
                    Line::from(hint).fg(statusbar().text)
                ).right_aligned())
            }
        );
    }

    let hints = if is_popup {
        Line::from(i18n::tr("statusbar-hint-popup")).fg(statusbar().muted)
    } else if is_at || is_slash {
        Line::from(i18n::tr("statusbar-hint-menu")).fg(statusbar().muted)
    } else {
        Line::from(i18n::tr("statusbar-hint-main")).fg(statusbar().muted)
    };

    element!(
        View(
            flex_direction: Direction::Horizontal,
            width: Constraint::Fill(1),
            height: Constraint::Length(1),
            justify_content: Flex::End,
        ) {
            Text(text: Paragraph::new(hints), alignment: Alignment::Right)
        }
    )
}

/// 通知行：固定高度 1，位于状态栏第 3 行（原视觉缓冲空行位置）。
/// 平时渲染空行，有通知时显示消息文本。高度恒定 → 通知出现/消失不引起
/// 任何行高变化，无行抖动；不再作为 StatusBar 顶部的动态插入行。
#[component]
fn NotifRow(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    // 渲染前检查过期——不写 atom，过期自动忽略。
    // 下次事件处理器写 NOTIFICATION 会用新值覆盖旧 Some。
    let notif_store = hooks.use_atom(&atoms::NOTIFICATION);
    let show_notif = notif_store
        .read()
        .as_ref()
        .is_some_and(|n| peri_time::monotonic_now() < n.until);
    let notif_text = if show_notif {
        notif_store
            .read()
            .as_ref()
            .map(|n| n.message.clone())
            .unwrap_or_default()
    } else {
        String::new()
    };

    let statusbar_tokens = statusbar();
    let text = if show_notif && !notif_text.is_empty() {
        Line::from(Span::styled(
            notif_text,
            Style::default()
                .fg(statusbar_tokens.text)
                .add_modifier(Modifier::BOLD),
        ))
    } else {
        Line::from("")
    };

    element!(
        View(
            flex_direction: Direction::Horizontal,
            width: Constraint::Fill(1),
            height: Constraint::Length(1),
        ) {
            Text(text: Paragraph::new(text))
        }
    )
}

#[derive(Default, Props)]
pub struct StatusBarProps {
    /// §11 高度降级：`h < 12` 时隐藏 Row2 key hints（Row1Only，高度 2）。
    pub hide_hints: bool,
    /// §11 高度降级：`h < 8` 时完全隐藏（高度 0，高度让给 transcript/composer）。
    pub hidden: bool,
}

#[component]
pub fn StatusBar(props: &StatusBarProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let _lang = hooks.use_atom(&atoms::LANG_VERSION);

    // [§3.1] 居中带由 AppShell 的 `CenterBand` 包装在 draw 期收窄；Row1 的
    // 折行与点击列都以收窄后的区域为准（`use_previous_size` / AreaTracker 在其后）。

    // §11：h<8 完全隐藏；h<12 仅 Row1 + NotifRow（无 key hints / 缓冲行）。
    // Row2 是独立组件，条件渲染不违反 hook 顺序（计划 Slice 1c 裁决）。
    let height = if props.hidden {
        0
    } else if props.hide_hints {
        2
    } else {
        4
    };

    element!(
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Length(height),
        ) {
            StatusBarRow1()
            if !props.hidden && !props.hide_hints {
                element!(StatusBarRow2())
            } else {
                element!(View(height: Constraint::Length(0), width: Constraint::Length(0)))
            }
            if props.hidden {
                element!(View(height: Constraint::Length(0), width: Constraint::Length(0)))
            } else {
                element!(NotifRow())
            }
            if !props.hidden && !props.hide_hints {
                // 第 4 行留空（视觉缓冲，Row1 折行为双行时自动压缩此区域）
                element!(Text(text: Paragraph::new(Line::from(""))))
            } else {
                element!(View(height: Constraint::Length(0), width: Constraint::Length(0)))
            }
        }
    )
}

// ── 辅助函数 ─────────────────────────────────────────────────────────────

/// Row1 最多显示两行；超出部分裁掉且不产生热区。
const ROW1_MAX_LINES: usize = 2;

/// Row1 的一次布局：可见行与模型/Goal 热区同源。
struct StatusBarLayout {
    /// 实际绘制的行（已按宽度折行并裁到可见高度）。
    lines: Vec<Line<'static>>,
    /// 模型段命中区间 `(行号, x 起点, x 终点)`，相对 Row1 区域。
    model_hits: Vec<(u16, u16, u16)>,
    /// Goal 段命中区间，语义同上。
    goal_hits: Vec<(u16, u16, u16)>,
}

/// 折行后的一个显示单元格；`span` 记录它来自哪个原始 span，供命中区间归属。
struct Cell {
    span: usize,
    text: String,
    width: usize,
}

impl Cell {
    fn new(span: usize, grapheme: &str) -> Self {
        Self {
            span,
            text: grapheme.to_string(),
            width: grapheme.width(),
        }
    }
}

/// 一次布局产出可见行与命中区间：绘制端直接渲染 `lines`，命中也只看这些单元格。
///
/// 折行以词为单位；行首放不下的长词按 grapheme 显示宽度拆到后续行，前导空白在
/// 换行处丢弃。`max_lines` 之外的内容既不绘制也不参与命中。
fn layout_status_bar(
    spans: &[Span<'static>],
    width: u16,
    max_lines: usize,
    model: Option<std::ops::Range<usize>>,
    goal: Option<std::ops::Range<usize>>,
) -> StatusBarLayout {
    let cells = wrap_cells(spans, usize::from(width), max_lines);
    // 首帧宽度未知（0）时无法校验列坐标，不给出可能错位的热区。
    let hits_available = width > 0;
    StatusBarLayout {
        lines: cells_to_lines(&cells, spans),
        model_hits: if hits_available {
            model
                .map(|range| hit_ranges(&cells, &range))
                .unwrap_or_default()
        } else {
            Vec::new()
        },
        goal_hits: if hits_available {
            goal.map(|range| hit_ranges(&cells, &range))
                .unwrap_or_default()
        } else {
            Vec::new()
        },
    }
}

/// 把 spans 折成携带来源 span 的单元格行。
fn wrap_cells(spans: &[Span<'static>], width: usize, max_lines: usize) -> Vec<Vec<Cell>> {
    if max_lines == 0 {
        return Vec::new();
    }
    if width == 0 {
        // 首帧宽度未知：保持单行，由绘制端按实际区域裁剪。
        return vec![
            spans
                .iter()
                .enumerate()
                .flat_map(|(span, text)| {
                    text.content
                        .graphemes(true)
                        .map(move |grapheme| Cell::new(span, grapheme))
                })
                .collect(),
        ];
    }

    let mut builder = LineBuilder::new(width, max_lines);
    let mut word: Vec<Cell> = Vec::new();
    let mut spaces: Vec<Cell> = Vec::new();
    for (span, text) in spans.iter().enumerate() {
        for grapheme in text.content.graphemes(true) {
            let cell = Cell::new(span, grapheme);
            // 宽于整行的字素无法显示（与 ratatui 的 WordWrapper 一致）。
            if cell.width > width {
                continue;
            }
            if grapheme.chars().all(char::is_whitespace) {
                if !word.is_empty() {
                    builder.append_word(&mut word, &mut spaces);
                    if builder.exhausted() {
                        return builder.finish();
                    }
                }
                spaces.push(cell);
            } else {
                word.push(cell);
            }
        }
    }
    if !word.is_empty() {
        builder.append_word(&mut word, &mut spaces);
    }
    builder.finish()
}

/// 词级折行的可变状态；只保留前 `max_lines` 行。
struct LineBuilder {
    width: usize,
    max_lines: usize,
    lines: Vec<Vec<Cell>>,
    line: Vec<Cell>,
    line_width: usize,
}

impl LineBuilder {
    fn new(width: usize, max_lines: usize) -> Self {
        Self {
            width,
            max_lines,
            lines: Vec::new(),
            line: Vec::new(),
            line_width: 0,
        }
    }

    /// 已收集满 `max_lines` 行且当前行没有内容：后续内容不可见。
    fn exhausted(&self) -> bool {
        self.lines.len() >= self.max_lines && self.line.is_empty()
    }

    fn break_line(&mut self) {
        if !self.line.is_empty() {
            self.lines.push(std::mem::take(&mut self.line));
            self.line_width = 0;
        }
    }

    fn push(&mut self, cell: Cell) {
        self.line_width += cell.width;
        self.line.push(cell);
    }

    /// 追加一个词（及其词前空白）。放不下时换行；超宽词按显示宽度拆分。
    fn append_word(&mut self, word: &mut Vec<Cell>, spaces: &mut Vec<Cell>) {
        if self.exhausted() {
            word.clear();
            spaces.clear();
            return;
        }
        let word_width: usize = word.iter().map(|cell| cell.width).sum();
        let space_width: usize = spaces.iter().map(|cell| cell.width).sum();
        if self.line_width + space_width + word_width <= self.width {
            for cell in spaces.drain(..) {
                self.push(cell);
            }
            for cell in word.drain(..) {
                self.push(cell);
            }
            return;
        }
        // 换行：行尾/行首的空白丢弃，避免下一行顶格前多出空格。
        spaces.clear();
        self.break_line();
        if self.exhausted() {
            word.clear();
            return;
        }
        if word_width <= self.width {
            for cell in word.drain(..) {
                self.push(cell);
            }
        } else {
            self.split_word(word);
        }
    }

    /// 行首超宽词：按显示宽度切到后续行，不拆开 grapheme。
    fn split_word(&mut self, word: &mut Vec<Cell>) {
        for cell in std::mem::take(word) {
            if self.exhausted() {
                return;
            }
            if self.line_width > 0 && self.line_width + cell.width > self.width {
                self.break_line();
                if self.exhausted() {
                    return;
                }
            }
            self.push(cell);
        }
    }

    fn finish(mut self) -> Vec<Vec<Cell>> {
        if self.lines.len() < self.max_lines && !self.line.is_empty() {
            self.lines.push(self.line);
        }
        if self.lines.is_empty() {
            self.lines.push(Vec::new());
        }
        self.lines
    }
}

/// 把单元格行还原为带原始样式的绘制行；相邻同源单元格合并为一个 span。
fn cells_to_lines(cells: &[Vec<Cell>], spans: &[Span<'static>]) -> Vec<Line<'static>> {
    cells
        .iter()
        .map(|line| {
            let mut grouped: Vec<(usize, String)> = Vec::new();
            for cell in line {
                match grouped.last_mut() {
                    Some((span, text)) if *span == cell.span => text.push_str(&cell.text),
                    _ => grouped.push((cell.span, cell.text.clone())),
                }
            }
            Line::from(
                grouped
                    .into_iter()
                    .map(|(span, text)| Span::styled(text, spans[span].style))
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

/// 可见单元格中属于 `region` 的连续区间，按绘制行号返回。
fn hit_ranges(cells: &[Vec<Cell>], region: &std::ops::Range<usize>) -> Vec<(u16, u16, u16)> {
    let mut hits = Vec::new();
    for (line_idx, line) in cells.iter().enumerate() {
        let mut x = 0u16;
        let mut start = None;
        for cell in line {
            let inside = region.contains(&cell.span);
            match (inside, start) {
                (true, None) => start = Some(x),
                (false, Some(range_start)) => {
                    hits.push((line_idx as u16, range_start, x));
                    start = None;
                }
                _ => {}
            }
            x = x.saturating_add(cell.width as u16);
        }
        if let Some(range_start) = start {
            hits.push((line_idx as u16, range_start, x));
        }
    }
    hits
}

/// 在 Row1 的命中区间中查找鼠标位置，返回区间起点的屏幕 x（弹窗锚点）。
fn hit_test(hits: &[(u16, u16, u16)], area: Rect, column: u16, row: u16) -> Option<u16> {
    hits.iter()
        .find(|&&(line, start, end)| {
            row == area.y.saturating_add(line)
                && column >= area.x.saturating_add(start)
                && column < area.x.saturating_add(end)
        })
        .map(|&(_, start, _)| area.x.saturating_add(start))
}

/// 追踪 Row1 组件区域，供鼠标点击→打开 alias 快速切换弹窗使用。
/// 仿 input_area.rs AreaTracker：rect 是值类型（Copy），每帧 pre_component_draw
/// 更新后在 handler 注册前取出副本传给闭包。
struct AreaTracker {
    rect: Option<Rect>,
}

impl Hook for AreaTracker {
    fn pre_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        self.rect = Some(drawer.area);
    }
}

fn statusbar() -> peri_theme::component::StatusBarTokens {
    THEME_ATOM.state().read().component.statusbar
}

fn separator() -> Span<'static> {
    Span::styled(" · ", Style::default().fg(statusbar().muted))
}

/// 把 atom 中的 permission_mode 字符串映射为显示标签。
fn permission_mode_display(mode: &str) -> String {
    match mode {
        "" => i18n::tr("statusbar-initializing"),
        "accept-edit" => i18n::tr("statusbar-permission-accept-edit"),
        "auto-mode" => i18n::tr("statusbar-permission-auto"),
        "bypass" => i18n::tr("statusbar-permission-bypass"),
        _ => i18n::tr("statusbar-permission-dont-ask"),
    }
}

/// 准备阶段的状态栏文案：准备在途时有明确状态，结束后回到常规内容。
///
/// 只做「事实 → 文案」映射；位置与样式由 `StatusBarRow1` 决定。
fn preparing_label(preparing: bool) -> Option<String> {
    preparing.then(|| i18n::tr("statusbar-preparing"))
}

fn permission_mode_color(mode: &str) -> ratatui::style::Color {
    match mode {
        "accept-edit" => statusbar().mode_accept_edit,
        "auto-mode" => statusbar().mode_auto,
        "bypass" => statusbar().mode_bypass,
        _ => statusbar().text,
    }
}

/// 从 cwd 路径取 basename（最后一节）。空或异常返回原串。
fn cwd_basename(cwd: &str) -> String {
    std::path::Path::new(cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| cwd.to_string())
}

/// 状态栏模型段三段式 `alias model effort` 的组成部分。
///
/// 去重规则：
/// - alias 与模型名相同时（配置回退到 alias）只显示一次；
/// - 模型名尾部已含 effort 后缀（如 `gpt-5.6-luna high`）时不重复追加，
///   避免出现 `high high`。
fn model_segment_parts(alias: &str, model_name: &str, effort: &str) -> Vec<String> {
    let model = if !model_name.is_empty() {
        model_name
    } else {
        alias
    };
    let mut parts = Vec::new();
    if !alias.is_empty() && alias != model {
        parts.push(alias.to_string());
    }
    if !model.is_empty() {
        parts.push(model.to_string());
    }
    if !effort.is_empty()
        && !model.is_empty()
        && !model.to_lowercase().ends_with(&format!(" {effort}"))
    {
        parts.push(effort.to_string());
    }
    parts
}

#[cfg(test)]
#[path = "status_bar_test.rs"]
mod tests;
