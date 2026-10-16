//! ratatui-kit MemoryPanel component.
//!
//! H1h（Iteration 14）：从 MEMORY_LIST atom 读取真实 memory 文件列表（由
//! service_snapshot 扫描 `~/.claude/memory/*.md` 派生，2s 刷新）。
//!
//! Enter 调用 `$EDITOR`（fallback `vi`）打开文件——通过 spawn_blocking + Detach
//! 执行，避免阻塞渲染线程。

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms::{LANG_VERSION, MEMORY_LIST, MemoryEntry};
use crate::kit::list_nav::{next_selection, previous_selection};
use crate::kit::panel_mouse::{AreaTracker, hit_item, is_scrollbar_column};
use crate::kit::panels::shared::list::{ListMetrics, ListViewport, content_rows};
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Rect},
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

/// 列表行度量：header 3 行（统计、导航提示、空行）+ 每项 1 行 + footer 0 行。
///
/// 渲染的 header 与命中布局此前由 `VISIBLE_ITEMS = 13`（按面板高度 18 手算）
/// 与手写 `ListLayout` 各自表达；现在两者都取自同一个 `ListViewport`。
const LIST_METRICS: ListMetrics = ListMetrics::new(3, 1, 0);

#[component]
pub fn MemoryPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let selected = hooks.use_state(|| 0usize);
    // 外部滚动状态——面板滚轮仲裁（panel_scroll.rs）驱动，统一 3 行/格 + 节流
    let sv = hooks.use_state(ScrollViewState::default);
    let store = hooks.use_atom(&MEMORY_LIST);
    let entries: Vec<MemoryEntry> = store.read().clone();
    let _ = store;
    let _ = hooks.use_atom(&LANG_VERSION);
    let count = entries.len();

    // 面板绘制区域（上一帧）——鼠标点击行号反推，同时供滚轮仲裁注册
    let area;
    {
        let tracker = hooks.use_hook(AreaTracker::new);
        area = tracker.rect;
    }

    // 列表视口：可见项数由面板内容区行数推导（首帧未测量时用 `panel_overlay` 的预测
    // 高度），绘制（`window`）与鼠标命中（`hit_layout`）共用这一份结果，选中项保持
    // 可见（issue 2026-07-06-panels-selection-no-scroll-follow）。
    let (_term_w, term_h) = hooks.use_terminal_size();
    let viewport = ListViewport::for_content_rows(
        content_rows(area, term_h),
        LIST_METRICS,
        entries.len(),
        *selected.read(),
    );

    hooks.use_event_handler_with_options(
        EventScope::Current,
        EventPriority::Normal,
        EventOptions { hit_test: true },
        move |event| {
            handle_memory_event(
                event,
                &MemoryEventCtx {
                    area,
                    selected,
                    viewport,
                },
            )
        },
    );

    #[cfg(test)]
    tests::visit_mounted_panel(tests::MountedPanel { selected });

    let sel = *selected.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    // 头部摘要
    lines.push(Line::from(vec![Span::styled(
        i18n::tr_args(
            "panel-memory-stats",
            &[("count".to_string(), FluentValue::from(count as i64))],
        ),
        Style::new()
            .fg(theme_def.read().semantic.text.primary)
            .bold(),
    )]));
    lines.push(Line::from(vec![Span::styled(
        i18n::tr("panel-memory-nav-hint"),
        Style::new()
            .fg(theme_def.read().semantic.text.muted)
            .italic(),
    )]));
    lines.push(Line::from(""));

    if entries.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("panel-memory-empty"),
            Style::new().fg(theme_def.read().semantic.text.muted),
        )]));
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("panel-memory-empty-hint"),
            Style::new().fg(theme_def.read().semantic.text.muted),
        )]));
    } else {
        for (i, entry) in viewport.window(&entries) {
            let is_selected = i == sel;
            let cursor = if is_selected { ">" } else { " " };
            let name_style = if is_selected {
                Style::new()
                    .fg(theme_def.read().component.panel.title)
                    .bold()
            } else {
                Style::new().fg(theme_def.read().semantic.text.primary)
            };

            // size 人类可读
            let size_str = format_size(entry.size_bytes);
            // 相对时间
            let time_str = entry
                .modified
                .map(format_relative_time)
                .unwrap_or_else(|| i18n::tr("common-na"));

            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} ", cursor),
                    Style::new().fg(theme_def.read().component.panel.title),
                ),
                Span::styled(entry.path.clone(), name_style),
                Span::styled(
                    format!("   {}  {}", size_str, time_str),
                    Style::new().fg(theme_def.read().semantic.text.dim),
                ),
            ]));
        }
    }

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    // 面板滚轮仲裁注册（每帧覆盖写入；区域与列表命中同源，均取自 AreaTracker，
    // 首帧未测量时为 0 尺寸，下一帧起生效——首帧滚轮不经本面板仲裁）
    crate::kit::panel_scroll::register_panel_scroll(
        PanelKind::Memory,
        area.unwrap_or_default(),
        sv,
    );

    panel_shell!(PanelKind::Memory, {
            ScrollView(
                scrollbars: crate::kit::panel_registry::clean_scrollbars(),
                state: Some(sv),
                width: Constraint::Fill(1),
                height: Constraint::Fill(1),
            ) {
                Text(text: content)
            }
    })
}

/// 人类可读的字节大小。
fn format_size(bytes: u64) -> String {
    const UNITS: &[(&str, u64)] = &[
        ("panel-memory-unit-b", 1),
        ("panel-memory-unit-kb", 1024),
        ("panel-memory-unit-mb", 1024 * 1024),
        ("panel-memory-unit-gb", 1024 * 1024 * 1024),
    ];
    for (key, threshold) in UNITS.iter().rev() {
        if bytes >= *threshold {
            let v = bytes as f64 / *threshold as f64;
            let unit = i18n::tr(key);
            if v >= 10.0 {
                return format!("{:.0} {}", v, unit);
            } else {
                return format!("{:.1} {}", v, unit);
            }
        }
    }
    format!("{} {}", bytes, i18n::tr("panel-memory-unit-b"))
}

/// 相对时间（"3m ago" / "2h ago" / "5d ago" / "2026-06-01"）。
fn format_relative_time(ts: chrono::DateTime<chrono::Utc>) -> String {
    use chrono::Utc;
    let now: chrono::DateTime<Utc> = peri_time::now_wall().into();
    let delta = now.signed_duration_since(ts);
    let secs = delta.num_seconds();
    if secs < 60 {
        return i18n::tr("panel-memory-time-just-now");
    }
    let mins = secs / 60;
    if mins < 60 {
        return i18n::tr_args(
            "panel-memory-time-min-ago",
            &[("n".to_string(), FluentValue::from(mins))],
        );
    }
    let hours = mins / 60;
    if hours < 24 {
        return i18n::tr_args(
            "panel-memory-time-hour-ago",
            &[("n".to_string(), FluentValue::from(hours))],
        );
    }
    let days = hours / 24;
    if days < 30 {
        return i18n::tr_args(
            "panel-memory-time-day-ago",
            &[("n".to_string(), FluentValue::from(days))],
        );
    }
    // 超过 30 天显示日期
    ts.format("%Y-%m-%d").to_string()
}

/// 用 `$EDITOR`（fallback `vi`）打开 memory 文件，detach 不阻塞 TUI。
fn open_memory_in_editor(rel_path: &str) {
    use std::path::PathBuf;
    use std::process::Command;

    // 测试构建：请求交给接缝记录并直接返回，单元测试绝不 spawn 真实编辑器进程；
    // 路径解析与 spawn 的错误分支因此只能由人工/端到端验证覆盖。
    #[cfg(test)]
    if tests::intercept_editor_open(rel_path) {
        return;
    }

    let Some(home) = dirs_next::home_dir() else {
        tracing::warn!("open_memory_in_editor: home_dir unknown");
        return;
    };
    let full: PathBuf = home.join(".claude").join("memory").join(rel_path);
    if !full.exists() {
        tracing::warn!(?full, "open_memory_in_editor: file missing");
        return;
    }

    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    // 拆分 EDITOR 可能包含的参数（如 "code -w" / "nvim -f"）
    let mut parts = editor.split_whitespace();
    let Some(bin) = parts.next() else {
        tracing::warn!("open_memory_in_editor: empty EDITOR");
        return;
    };
    let mut cmd = Command::new(bin);
    for arg in parts {
        cmd.arg(arg);
    }
    cmd.arg(&full);

    tracing::info!(?full, bin, "MemoryPanel: spawning editor");
    if let Err(e) = cmd.spawn() {
        tracing::warn!(?e, bin, "MemoryPanel: editor spawn failed");
    }
}

/// 面板事件处理——判定集中在这里、组件只做注册，测试可经 `tests::mount_panel`
/// 捕获真实句柄后直接投喂事件（`State<T>` 无公开构造 API）。
///
/// 鼠标命中与绘制共用 `ctx.viewport`（`hit_layout()` / `window()` 同源），
/// 面板高度变化时两者看到的可见项数一致。
fn handle_memory_event(event: Event, ctx: &MemoryEventCtx) -> EventResult {
    // 鼠标：区域内左键点击 = 选中该项并执行 Enter 动作（click as enter）
    if let Event::Mouse(mouse) = event {
        if let Some(area) = ctx.area
            && !is_scrollbar_column(&mouse, area)
            && let Some(idx) = hit_item(&mouse, area, ctx.viewport.hit_layout())
        {
            *ctx.selected.write() = idx;
            open_entry_at(idx);
            return EventResult::Consumed;
        }
        return match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => EventResult::Consumed,
            _ => EventResult::Ignored,
        };
    }
    let Event::Key(key) = event else {
        return EventResult::Ignored;
    };
    if key.kind != KeyEventKind::Press {
        return EventResult::Ignored;
    }
    match key.code {
        KeyCode::Esc => close_panel(),
        KeyCode::Up => {
            let mut s = ctx.selected.write();
            *s = previous_selection(*s);
        }
        KeyCode::Down => {
            let mut s = ctx.selected.write();
            let count = MEMORY_LIST.state().read().len();
            if count > 0 {
                *s = next_selection(*s, count);
            }
        }
        KeyCode::Enter => {
            let sel = *ctx.selected.read();
            open_entry_at(sel);
        }
        _ => {}
    }
    EventResult::Consumed
}

/// 事件处理上下文：本帧视口 + 面板句柄。
///
/// `State<T>` 无公开构造 API，只能从组件挂载取得；集中成一个结构体，事件入口
/// 签名不再随状态/视口增长（渲染与鼠标命中必须共用同一个 `viewport`）。
struct MemoryEventCtx {
    area: Option<Rect>,
    selected: State<usize>,
    viewport: ListViewport,
}

/// 打开当前列表第 `idx` 项——动作时从最新的 `MEMORY_LIST` 取条目（列表刷新会换
/// 下标，进入时缓存 path 会打开错文件），path 直接交给 `open_memory_in_editor`。
fn open_entry_at(idx: usize) {
    if let Some(entry) = MEMORY_LIST.state().read().get(idx) {
        open_memory_in_editor(&entry.path);
    }
}

fn close_panel() {
    // I19-A: 弹栈而非清空整个栈，避免同时打开多个不同组面板时关闭一个会全部关闭
    crate::kit::panel_registry::close_active_panel();
}

#[cfg(test)]
#[path = "memory_test.rs"]
mod tests;
