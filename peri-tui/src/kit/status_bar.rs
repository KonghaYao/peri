//! ratatui-kit StatusBar component.
//!
//! S9：完整双行布局——
//! - **Row 1**：权限模式 → cwd basename → provider/model → bg tasks
//!   全部从 SERVICE_SNAPSHOT atom 派生（S5 落地）；高亮计时器控制闪烁。
//!   CPU%/MEM/ctx 已迁移 composer footer 资源线（input_area.rs）。
//! - **Row 2**：状态相关的快捷键 hints（popup/mention/slash/默认 4 态切换）。

use crate::i18n;
use crate::kit::atoms;
use crate::kit::mouse_router;
use crate::kit::popup_overlay::open_popup;
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    crossterm::event::{Event, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{
        layout::{Alignment, Constraint, Direction, Flex, Rect},
        style::{Modifier, Style, Stylize},
        text::{Line, Span},
        widgets::{Paragraph, Wrap},
    },
};
use std::time::{Duration, Instant};

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

    let snap = snap.read().clone();
    let now = Instant::now();
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
                let e = h.pop().unwrap();
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

    // 4. 后台任务计数
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

    // 根据可用宽度动态决定是否需要折行（单行 → 双行）
    let total_width = Line::from(spans.clone()).width() as u16;
    let prev_size = hooks.use_previous_size();
    // 首帧 use_previous_size 返回 width=0，退守单行；后续帧宽度超过才启用双行折行
    let needs_wrap = prev_size.width > 0 && total_width > prev_size.width;
    let row_height: u16 = if needs_wrap { 2 } else { 1 };

    // ── 模型段点击区域：鼠标左键点击 alias/model 文本 → 打开快速切换弹窗 ──
    // AreaTracker 值拷贝模式（仿 input_area.rs）：每帧从 hook 取出 rect 副本，
    // 闭包按值捕获，避免 Arc 重建导致 handler 读到 None。
    let row1_area;
    {
        let tracker = hooks.use_hook(|| AreaTracker { rect: None });
        row1_area = tracker.rect;
    }
    // 模拟 WordWrapper 词级折行（Wrap { trim: false }，对齐 ratatui-widgets 0.3.2
    // reflow.rs 语义）计算模型段的点击区域列表——
    // 每个区域 `(line_idx, x_start, x_end)` 对应模型段内一个词折行后的位置。
    // [Bug 修复] 旧实现只记录循环结束后的 line_idx（= 最后一个 span 所在行），
    // 折行点落在模型段之后时模型文本在第 0 行而点击判定用第 1 行，点击永久失效。
    // 现在按模型段内每个词各自记录行号，模型段跨行/尾部折行时返回多个区域；
    // 换行判定按 WordWrapper 逐字符增量检查的等价形式（词前缀放不下才换行，
    // 词恰好放满整行时留在行尾），与真实渲染逐位对齐，
    // 不再有 span 内部断行的 1-2 列死区。
    let model_click: Vec<(u16, u16, u16)> =
        if let (Some(start), Some(area)) = (model_start, row1_area) {
            model_click_areas(&spans, area.width as usize, row_height, start, model_end)
        } else {
            Vec::new()
        };
    hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
        if let Event::Mouse(mouse) = event {
            if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
                return EventResult::Ignored;
            }
            // 弹窗或面板激活时不响应——防止遮挡区域误触（如弹窗覆盖状态栏）
            if mouse_router::is_occluded() {
                return EventResult::Ignored;
            }
            if let Some(area) = row1_area
                && let Some((_, x_start, _)) = model_click.iter().copied().find(|&(li, xs, xe)| {
                    mouse.row == area.y.saturating_add(li)
                        && mouse.column >= area.x.saturating_add(xs)
                        && mouse.column < area.x.saturating_add(xe)
                })
            {
                // 锚点 = 模型段起点屏幕坐标——小弹窗定位在该点上方
                *atoms::MODEL_SWITCH_ANCHOR.state().write() =
                    Some((area.x.saturating_add(x_start), mouse.row));
                open_popup(atoms::PopupKind::ModelQuickSwitch);
                return EventResult::Consumed;
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
            Text(text: Paragraph::new(Line::from(spans)).wrap(Wrap { trim: false }))
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
    let now = Instant::now();

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
        .is_some_and(|n| Instant::now() < n.until);
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

/// [升级护栏] 本函数是对 ratatui-widgets 内部 reflow 算法（`WordWrapper`）的
/// 复刻模拟——与 message_area 的"真实渲染复刻"方案形成两套 wrap 语义认知。
/// 升级 ratatui-widgets 依赖（即使 patch 版本）时，必须运行
/// `cargo test -p peri-tui --lib status_bar_test` 的 ground-truth 差分测试
/// （TestBackend 真实渲染逐位对比 + 随机差分），并重新评估下方列出的已知差异。
/// Cargo.toml 中 ratatui 依赖处有同步标注。
///
/// 模拟 WordWrapper 词级折行（对齐 ratatui-widgets 0.3.2 reflow.rs `Wrap{trim:false}`
/// 语义，经 TestBackend ground-truth 渲染逐位验证），返回模型段内每个**词**的
/// 点击区域 `(line_idx, x_start, x_end)`（相对 Row1 区域）。
///
/// 词级规则（reflow.rs 逐行核对 + TestBackend 差分实验确认）：
/// - 词 = 非空白段；空白（含跨 span 边界累积）为词前宽度 `ws`；
/// - append 前检查：`line_x > 0 && line_x + ws + w - cw_last >= area_w` 且行非空 →
///   词整体换到新行。等价推导：reflow.rs L129 `pending_word_overflow` 是逐字符增量
///   检查（`line_width + ws + 词内前缀宽 >= max`，检查时不含当前字符），词内前缀
///   最大 = `w - cw_last`（cw_last = 词尾字符宽）→ 触发条件即上式。词恰好放满
///   整行（ASCII 词尾时 `line + ws + w == area_w`）**留在行尾**，走 line_full 行推出；
/// - 换行时行尾回填（L139-150）：行尾剩余空间内的词前空白被丢弃（视觉等同行尾
///   空格），剩余空白随词到下一行——下一行可能顶格；
/// - 行首词（`line_x == 0`）不触发换行——超宽词放行首（untrimmed_overflow 近似；
///   WordWrapper 对行首超宽词会拆分跨行，模拟整词放行首，超界部分天然不可达）。
///   注意边界是 `ws + w > area_w`（词 + 前导空白超宽，L107-109 逐字符检查），
///   不只是 `w > area_w`：此时区域覆盖真实前缀（可点），词尾剩余字符无区域
///   （点击失效）。状态栏模型段首词为 sep "·"（宽 1）、模型段恰在行首才触发，
///   罕见且无害；
/// - 文本末尾 flush（L178-181）无溢出检查：**最后一个词永不换行**（超界截断
///   显示），模拟对所有词（含最后一个）做换行检查。状态栏中 MEM 段无条件
///   存在，模型段永不可能是最后一段 → 差异当前不可达；若尾部段改为可选
///   需重新评估；
/// - append 后检查 `line_full`：`line_x >= area_w` → 行推出（词留在行尾）。
///
/// 每个词一个区域；点击词（含词前空白）即可命中。模型段跨行时每行各有区域，
/// 折行点落在模型段之后（尾部 CPU/MEM/bg/ctx 折到下一行）时模型段仍整段在第 0 行——
/// 修复前 `line_idx` 取循环结束后的值（= 最后一个 span 所在行），
/// 导致点击判定错位一行、弹窗永久无法打开。
fn model_click_areas(
    spans: &[Span<'static>],
    area_w: usize,
    row_height: u16,
    model_start: usize,
    model_end: usize,
) -> Vec<(u16, u16, u16)> {
    // 防御：不可渲染场景直接返回空（组件内 row_height ∈ {1,2}，0 不可达）
    if area_w == 0 || spans.is_empty() || model_start >= model_end || row_height == 0 {
        return Vec::new();
    }
    // 词流：把 spans 拼接为字符流逐字符扫描，词可跨 span 边界合并；
    // in_model 按词起点所在 span 是否 ∈ [model_start, model_end) 标记。
    let mut words: Vec<(bool, usize, usize, usize)> = Vec::new(); // (in_model, ws, w, cw_last)
    let mut cur_ws = 0usize;
    let mut cur_w = 0usize;
    let mut cur_cw_last = 0usize;
    let mut cur_model = false;
    let mut in_word = false;
    for (span_idx, s) in spans.iter().enumerate() {
        let in_model = span_idx >= model_start && span_idx < model_end;
        for c in s.content.chars() {
            if c.is_whitespace() {
                if in_word {
                    words.push((cur_model, cur_ws, cur_w, cur_cw_last));
                    in_word = false;
                    cur_ws = 0;
                }
                cur_ws += char_width(c);
            } else {
                if !in_word {
                    in_word = true;
                    cur_model = in_model;
                    cur_w = 0;
                }
                let w = char_width(c);
                cur_w += w;
                cur_cw_last = w;
            }
        }
    }
    if in_word {
        words.push((cur_model, cur_ws, cur_w, cur_cw_last));
    }
    // 词级折行模拟（与 WordWrapper 同构）：
    let max_line = row_height.saturating_sub(1);
    let mut areas = Vec::new();
    let mut line_idx = 0u16;
    let mut line_x = 0usize;
    for (in_model, mut ws, w, cw_last) in words {
        // 词 append 前：词前缀（不含词尾字符）放不下（>=）且行非空 → 词整体换行。
        // 逐字符增量检查的等价形式：前缀最大 = w - cw_last。
        // 词恰好填满整行时留在行尾（line_full 语义）。
        if line_x > 0 && line_x + ws + w - cw_last >= area_w && line_idx < max_line {
            // 行尾回填：行尾剩余空间内的词前空白被丢弃（视觉等同行尾空格），
            // 剩余空白随词到下一行（下一行可能顶格）。
            let remaining = area_w.saturating_sub(line_x);
            ws = ws.saturating_sub(remaining);
            line_idx += 1;
            line_x = 0;
        }
        if in_model {
            areas.push((line_idx, line_x as u16, (line_x + ws + w) as u16));
        }
        line_x += ws + w;
        // append 后：行满（>=）→ 行推出（词留在行尾）
        if line_x >= area_w && line_idx < max_line {
            line_idx += 1;
            line_x = 0;
        }
    }
    areas
}

/// 单个字符的终端显示宽度（UnicodeWidthChar 语义：CJK/全角=2、零宽=0）。
fn char_width(c: char) -> usize {
    unicode_width::UnicodeWidthChar::width(c).unwrap_or(0)
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
        "accept-edit" => i18n::tr("statusbar-permission-accept-edit"),
        "auto-mode" => i18n::tr("statusbar-permission-auto"),
        "bypass" => i18n::tr("statusbar-permission-bypass"),
        _ => i18n::tr("statusbar-permission-dont-ask"),
    }
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
