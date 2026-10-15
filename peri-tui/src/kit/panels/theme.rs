//! ratatui-kit ThemePanel component.
//!
//! 列出可用主题（builtin + ~/.peri/themes/），顶部提供选中主题的 markdown
//! 预览，Enter 切换主题，Esc 关闭。布局仿 ThreadBrowser 面板。
//!
//! 交互模式：
//! - 打开面板 → 切到持久化主题色，记住原始主题（Esc 恢复用）
//! - j/k/↑/↓ 导航 → 实时切换全局颜色（实时预览）
//! - Enter → 持久化到 ~/.peri/settings.json
//! - Esc → 恢复原始主题色并关闭面板

use std::collections::HashMap;
use std::time::Duration;

use fluent_bundle::FluentValue;
use ratatui_kit::prelude::Atom as AtomStatic;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Rect},
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms::{
    LANG_VERSION, NOTIFICATION, Notification, PERI_CONFIG_HANDLE, TUI_CONFIG_HANDLE,
};
use crate::kit::list_nav::{
    clamp_selection, next_selection, previous_selection, scroll_start_for_selected,
};
use crate::kit::markdown::{self, MarkdownSegment};
use crate::kit::panel_mouse::{AreaTracker, ListLayout, hit_item, is_scrollbar_column};
use peri_theme::atoms::{PALETTE_ATOM, PERI_COLORS_ATOM, THEME_ATOM};
use peri_theme::bridge::ThemeDefinitionExt;
use peri_theme::loader::list_available_themes;
use peri_theme::theme::ThemeMode;

mod download;

use download::trigger_download_themes;
#[cfg(test)]
use download::{DownloadRequest, trigger_download_themes_with};

static THEME_LIST: AtomStatic<Vec<String>> = AtomStatic::new(list_available_themes);

/// 按主题模式将当前目录快照分为深色和浅色主题。
fn classify_theme_catalog(
    catalog: &[String],
    mut mode_for_theme: impl FnMut(&str) -> Option<ThemeMode>,
) -> (Vec<String>, Vec<String>) {
    let mut dark = Vec::new();
    let mut light = Vec::new();

    for name in catalog {
        match mode_for_theme(name) {
            Some(ThemeMode::Dark | ThemeMode::HighContrast) => dark.push(name.clone()),
            Some(ThemeMode::Light) => light.push(name.clone()),
            None => continue,
        }
    }

    (dark, light)
}

/// 下载至少一个主题后，以重新扫描的目录快照替换当前 catalog。
fn refresh_theme_catalog_after_download(
    catalog: &mut Vec<String>,
    success_count: usize,
    scan_catalog: impl FnOnce() -> Vec<String>,
) {
    if success_count > 0 {
        *catalog = scan_catalog();
    }
}

/// 预览用样本 markdown，覆盖主要样式（标题、粗体/斜体/删除线、行内代码、引用）。
const SAMPLE_MD: &str = "# Heading\n**bold** \u{00b7} *italic* \u{00b7} ~strikethrough~\n`code` and plain text.\n\n> blockquote sample";

/// 预览最大宽度（面板 50 - border 2 - 内部余量 2）。
const PREVIEW_WIDTH: usize = 46;

/// 为指定主题名生成 markdown 预览行。
fn build_preview(theme_name: &str) -> Vec<Line<'static>> {
    let theme = match peri_theme::loader::load_theme(theme_name) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("failed to load theme '{}' for preview: {}", theme_name, e);
            return vec![Line::from(Span::styled(
                "(preview unavailable)",
                Style::new(),
            ))];
        }
    };
    let palette = theme.to_palette();
    let md_text_fg = theme.component.markdown.text;
    let segments = markdown::parse_markdown(SAMPLE_MD, PREVIEW_WIDTH, palette, md_text_fg);
    segments
        .iter()
        .flat_map(|s| match s {
            MarkdownSegment::Text(lines) => lines.clone(),
            MarkdownSegment::Table(_) => vec![],
            MarkdownSegment::Image(_) => vec![],
        })
        .collect()
}

/// 读取持久化配置中的主题名。
fn persisted_theme_name() -> String {
    TUI_CONFIG_HANDLE
        .get()
        .and_then(|h| h.read().theme.clone())
        .unwrap_or_else(|| "peri-dark".to_string())
}

/// 持久化当前选中主题到 settings.json。
fn persist_theme(name: &str) {
    let Some(tui_handle) = TUI_CONFIG_HANDLE.get() else {
        return;
    };
    let Some(peri_handle) = PERI_CONFIG_HANDLE.get() else {
        return;
    };
    let mut tui = tui_handle.write();
    tui.theme = Some(name.to_string());
    let tui_snapshot = tui.clone();
    drop(tui);
    let mut peri = peri_handle.write();
    tui_snapshot.sync_to_extra(&mut peri.config.extra);
    let snap = peri.clone();
    drop(peri);
    match crate::config::save_effective(&snap) {
        Ok(()) => {
            *NOTIFICATION.state().write() = Some(Notification {
                message: i18n::tr("config-saved").to_string(),
                until: peri_time::monotonic_now() + Duration::from_secs(1),
            });
        }
        Err(e) => {
            tracing::error!("failed to persist theme '{}': {}", name, e);
            *NOTIFICATION.state().write() = Some(Notification {
                message: i18n::tr_args(
                    "config-save-failed",
                    &[(
                        "error".to_string(),
                        FluentValue::from(e.to_string().as_str()),
                    )],
                ),
                until: peri_time::monotonic_now() + Duration::from_secs(2),
            });
        }
    }
}

/// 读取持久化配置中的 daily_color 开关状态。
fn daily_color_enabled() -> bool {
    TUI_CONFIG_HANDLE
        .get()
        .map(|h| h.read().daily_color)
        .unwrap_or(false)
}

/// 切换每日色彩的开关状态，并持久化到 settings.json。
fn toggle_daily_color() {
    let Some(tui_handle) = TUI_CONFIG_HANDLE.get() else {
        return;
    };
    let Some(peri_handle) = PERI_CONFIG_HANDLE.get() else {
        return;
    };
    let mut tui = tui_handle.write();
    tui.daily_color = !tui.daily_color;
    // 开启时清除旧日期，使启动时立即生效
    if tui.daily_color {
        tui.daily_color_date = None;
    }
    let tui_snapshot = tui.clone();
    drop(tui);
    let mut peri = peri_handle.write();
    tui_snapshot.sync_to_extra(&mut peri.config.extra);
    let snap = peri.clone();
    drop(peri);
    match crate::config::save_effective(&snap) {
        Ok(()) => {
            *NOTIFICATION.state().write() = Some(Notification {
                message: i18n::tr("config-saved").to_string(),
                until: peri_time::monotonic_now() + Duration::from_secs(1),
            });
        }
        Err(e) => {
            tracing::error!("failed to toggle daily_color: {}", e);
            *NOTIFICATION.state().write() = Some(Notification {
                message: i18n::tr_args(
                    "config-save-failed",
                    &[(
                        "error".to_string(),
                        FluentValue::from(e.to_string().as_str()),
                    )],
                ),
                until: peri_time::monotonic_now() + Duration::from_secs(2),
            });
        }
    }
}

const VISIBLE_ITEMS: usize = 8;

/// 列表 header 的固定行数：tab 栏 1 + tab 提示 1 + 空行 1 + 预览标题 1 + 分隔线 1。
const LIST_HEADER_FIXED_ROWS: usize = 5;

/// 列表上方 header 行数——绘制与鼠标命中（`hit_item`）的唯一推导处。
///
/// 预览正文行数随选中主题变化（预览为空时渲染一行 `(no preview)`），故本面板不接入
/// 固定行高的 `shared::list::ListViewport`：它的 `visible_items` 按内容区行数推导，
/// 会改变本面板「可见 8 项」的既有语义。行度量在这里推导一次，渲染按同一行数铺行，
/// 命中布局取同一个值（此前两处各自手算，命中少算了预览标题行，点击与高亮差一行）。
fn list_header_rows(preview_len: usize) -> u16 {
    let rows = LIST_HEADER_FIXED_ROWS + preview_len.max(1);
    u16::try_from(rows).unwrap_or(u16::MAX)
}

/// 目录快照按主题模式分类的结果（dark / light 两个列表）。
///
/// 渲染、计数与命中快照都取自同一份推导，避免各自重新分类出不同的顺序。
struct ThemeCatalogTabs {
    dark: Vec<String>,
    light: Vec<String>,
}

impl ThemeCatalogTabs {
    fn of(catalog: &[String]) -> Self {
        let (dark, light) = classify_theme_catalog(catalog, |name| {
            peri_theme::loader::load_theme(name)
                .ok()
                .map(|theme| theme.mode)
        });
        Self { dark, light }
    }

    /// 当前 tab 的列表：0 = dark，其余 = light。
    fn list(&self, tab: u8) -> &[String] {
        if tab == 0 { &self.dark } else { &self.light }
    }

    /// 另一个 tab 的主题数（空列表提示用）。
    fn other_count(&self, tab: u8) -> usize {
        if tab == 0 {
            self.light.len()
        } else {
            self.dark.len()
        }
    }
}

/// 选中身份在本帧列表中的落点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Selection {
    /// 身份命中的下标；身份缺失时为收敛位（已 clamp 进列表，不越界）
    index: usize,
    /// 身份已不在列表中（被删除 / 切到不含它的 tab）
    missing: bool,
}

/// 由稳定身份推导选中落点：命中 → 该主题的下标；缺失 → 收敛到 `fallback` 处的相邻项。
fn resolve_selection(list: &[String], selected_theme: &str, fallback: usize) -> Selection {
    match list.iter().position(|name| name == selected_theme) {
        Some(index) => Selection {
            index,
            missing: false,
        },
        None => Selection {
            index: clamp_selection(fallback, list.len()),
            missing: true,
        },
    }
}

/// 打开面板时切到持久化主题（effect 边界；只在挂载首帧运行一次）。
fn switch_to_persisted_theme(current_name: String) {
    let persisted = persisted_theme_name();
    if persisted != current_name {
        switch_theme_atoms(&persisted);
    }
}

/// 选中身份收敛（effect 边界）：身份不在列表时改写为相邻项，并记录本帧落点。
///
/// 渲染只读状态（TUI-RENDER-001）；`last_index` 只用于下一次收敛，故写入不通知。
fn converge_selection(
    selected_theme: State<String>,
    last_index: State<usize>,
    list: &[String],
    selection: Selection,
) {
    *last_index.write_no_update() = selection.index;
    if !selection.missing {
        return;
    }
    if let Some(name) = list.get(selection.index) {
        *selected_theme.write() = name.clone();
    }
}

/// 预览缓存写入（effect 边界）：缓存只是加速，渲染期未命中时按同一函数现算。
fn cache_preview(
    cache: State<HashMap<String, Vec<Line<'static>>>>,
    pending: Option<(String, Vec<Line<'static>>)>,
) {
    if let Some((name, lines)) = pending {
        // 派生缓存：不通知重渲染（渲染结果的取用不依赖缓存是否已写入）
        cache.write_no_update().insert(name, lines);
    }
}

#[component]
pub fn ThemePanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    // ── hooks：无条件、稳定顺序，全部先于任何条件分支（TUI-HOOK-001）──
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let _ = hooks.use_atom(&LANG_VERSION);
    // 外部滚动状态——面板滚轮仲裁（panel_scroll.rs）驱动，统一 3 行/格 + 节流
    let sv = hooks.use_state(ScrollViewState::default);

    // 打开面板时记住切换前的主题名（Esc 恢复用）
    let original_theme = hooks.use_state(|| theme_def.read().name.to_string());
    // 选中身份：主题名（稳定 key，不是列表下标）——目录刷新 / 重新分类后仍指向同一主题
    let selected_theme = hooks.use_state(|| theme_def.read().name.to_string());
    // 上一帧落点：身份消失（被删除 / 切 tab）时收敛到相邻项用
    let last_index = hooks.use_state(|| 0usize);

    // tab 状态：0 = dark, 1 = light
    let tab = hooks.use_state(|| 0u8);

    // 预览缓存：theme_name → 渲染后的 Line 列表（渲染只读，写入在 effect 边界）
    let preview_cache = hooks.use_state(HashMap::<String, Vec<Line<'static>>>::new);

    // 订阅主题目录快照；下载成功后的写入会唤醒当前已打开的面板。
    let theme_catalog = hooks.use_atom(&THEME_LIST);

    // 面板绘制区域（上一帧）——鼠标点击行号反推
    let area;
    {
        let tracker = hooks.use_hook(AreaTracker::new);
        area = tracker.rect;
    }
    // 面板上一帧区域（滚轮仲裁注册用）——hook 前置，渲染段不再调用 hook
    let previous_size = hooks.use_previous_size();

    // 打开面板 → 切到持久化主题；只在挂载首帧执行（渲染 body 不写状态——TUI-RENDER-001）
    let mount_theme_name = theme_def.read().name.to_string();
    hooks.use_effect(move || switch_to_persisted_theme(mount_theme_name), ());

    // ── 本帧推导：tab 列表、选中落点、预览行数 ─────────────────────────────────
    let tabs = ThemeCatalogTabs::of(&theme_catalog.read());
    let current_tab = *tab.read();
    let themes = tabs.list(current_tab).to_vec();
    let other_count = tabs.other_count(current_tab);
    let count = themes.len();

    // 选中落点由稳定身份推导；身份消失时在 effect 边界收敛（渲染只读状态）
    let selection = resolve_selection(&themes, &selected_theme.read(), *last_index.read());
    let sel = selection.index;
    let convergence_list = themes.clone();
    hooks.use_effect(
        move || converge_selection(selected_theme, last_index, &convergence_list, selection),
        (selection.missing, selection.index, count),
    );

    // 预览：缓存命中直接取用；未命中按同一函数现算，写入交给 effect 边界
    let selected_name = themes.get(sel).cloned().unwrap_or_default();
    let cached_preview = preview_cache.read().get(&selected_name).cloned();
    let preview_missing = cached_preview.is_none();
    let preview_lines = cached_preview.unwrap_or_else(|| build_preview(&selected_name));
    let preview_to_cache = preview_missing.then(|| (selected_name.clone(), preview_lines.clone()));
    let preview_cache_deps = preview_to_cache.as_ref().map(|(name, _)| name.clone());
    hooks.use_effect(
        move || cache_preview(preview_cache, preview_to_cache),
        preview_cache_deps,
    );

    // 行布局：header = tab 1 + 提示 1 + 空行 1 + 预览标题 1 + 预览正文 N + 分隔线 1；
    // 每主题 1 行；footer 1。绘制按此铺行，鼠标命中取同一个 header 行数。
    let header_rows = list_header_rows(preview_lines.len());
    let scroll_start = if count == 0 {
        0
    } else {
        scroll_start_for_selected(sel, count, VISIBLE_ITEMS)
    };
    let orig = original_theme.read().clone();
    let persisted_name = persisted_theme_name();

    // ── 键盘 + 鼠标处理：判定集中在 handle_theme_event，组件只做注册 ──
    let event_ctx = ThemeEventCtx {
        area,
        tab,
        selected_theme,
        themes: themes.clone(), // 本帧列表快照：命中索引与动作目标同源
        selected_index: sel,
        header_rows,
        scroll_start,
        original_theme: orig,
    };
    hooks.use_event_handler_with_options(
        EventScope::Current,
        EventPriority::Normal,
        EventOptions { hit_test: true },
        move |event| handle_theme_event(event, &event_ctx),
    );

    #[cfg(test)]
    theme_test::visit_mounted_panel(theme_test::MountedPanel {
        tab,
        selected_theme,
        last_index,
        preview_cache,
        original_theme,
    });

    // ── 渲染段（只读；状态写入都在上面的 effect / 事件边界，TUI-RENDER-001）──
    let guard = theme_def.read();
    let semantic = &guard.semantic;

    let header_style = Style::new().fg(semantic.text.primary).bold();
    let muted_style = Style::new().fg(semantic.text.muted).italic();
    let dim_style = Style::new().fg(semantic.text.dim);
    let selected_style = Style::new().fg(guard.component.panel.title).bold();
    let separator_style = Style::new().fg(semantic.border.dim);

    let mut lines: Vec<Line<'_>> = Vec::new();

    // ── Tab 切换栏 ──
    let tab_reverse_fg = semantic.surface.default; // 反色前景 = 背景色
    let tab_active_style = Style::new().fg(tab_reverse_fg).bg(semantic.accent);
    let tab_inactive_style = Style::new().fg(semantic.text.muted);
    let (dark_style, light_style) = if current_tab == 0 {
        (tab_active_style, tab_inactive_style)
    } else {
        (tab_inactive_style, tab_active_style)
    };
    let dark_count = tabs.dark.len();
    let light_count = tabs.light.len();
    lines.push(Line::from(vec![
        Span::styled(
            format!(" {} ({}) ", i18n::tr("panel-theme-tab-dark"), dark_count),
            dark_style,
        ),
        Span::styled("  ", Style::new()),
        Span::styled(
            format!(" {} ({}) ", i18n::tr("panel-theme-tab-light"), light_count),
            light_style,
        ),
    ]));
    lines.push(Line::from(vec![Span::styled(
        i18n::tr("panel-theme-tab-hint"),
        muted_style,
    )]));
    lines.push(Line::from(""));

    // ── 预览（正文行数与命中布局同源：至少 1 行）──
    lines.push(Line::from(vec![Span::styled(
        format!(
            "  {} (\u{2191}/\u{2193} navigate)",
            i18n::tr("panel-theme-preview")
        ),
        header_style,
    )]));

    if preview_lines.is_empty() {
        lines.push(Line::from(vec![Span::styled("  (no preview)", dim_style)]));
    } else {
        for line in &preview_lines {
            let mut spans = vec![Span::styled("  ", dim_style)];
            spans.extend(line.spans.iter().cloned());
            lines.push(Line::from(spans));
        }
    }

    // ── 分隔符 ──
    lines.push(Line::from(vec![Span::styled(
        "\u{2500}".repeat(PREVIEW_WIDTH),
        separator_style,
    )]));

    // ── 主题列表 ──
    if themes.is_empty() {
        lines.push(Line::from(i18n::tr("panel-theme-empty")).fg(semantic.text.muted));
        if other_count > 0 {
            lines.push(Line::from(vec![Span::styled(
                format!("  ({} themes in the other tab)", other_count),
                muted_style,
            )]));
        }
    } else {
        for (i, name) in themes
            .iter()
            .enumerate()
            .skip(scroll_start)
            .take(VISIBLE_ITEMS)
        {
            let is_persisted = *name == persisted_name;
            let is_selected = i == sel;
            let cursor = if is_selected { ">" } else { " " };
            let active_mark = if is_persisted {
                i18n::tr("panel-theme-active-mark")
            } else {
                String::new()
            };
            let display = format!(" {} {}{}", cursor, name, active_mark);

            let style = if is_selected {
                selected_style
            } else if is_persisted {
                Style::new().fg(semantic.status.success)
            } else {
                Style::new().fg(semantic.text.primary)
            };

            lines.push(Line::from(Span::styled(display, style)));
        }
    }

    // ── Footer ──
    let daily_status = if daily_color_enabled() {
        i18n::tr("panel-theme-daily-on")
    } else {
        i18n::tr("panel-theme-daily-off")
    };
    let download_label = i18n::tr("panel-theme-download-label");
    let footer_hint = i18n::tr_args(
        "panel-theme-footer-hint",
        &[
            (
                "status".to_string(),
                FluentValue::String(daily_status.into()),
            ),
            (
                "download".to_string(),
                FluentValue::String(download_label.into()),
            ),
        ],
    );
    lines.push(Line::from(vec![Span::styled(footer_hint, muted_style)]));

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    // 面板滚轮仲裁注册（每帧覆盖写入，area 用上一帧组件区域）
    crate::kit::panel_scroll::register_panel_scroll(PanelKind::Theme, previous_size, sv);

    panel_shell!(PanelKind::Theme, {
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

/// 面板事件处理——判定集中在这里、组件只做注册，测试经 `tests::mount_panel` 取得真实
/// 句柄后直接投喂事件（`State<T>` 无公开构造 API）。
///
/// 鼠标命中与绘制共用同一份推导：`ctx.header_rows` 是本帧绘制的 header 行数
/// （[`list_header_rows`]），`ctx.themes` 是本帧列表快照。
fn handle_theme_event(event: Event, ctx: &ThemeEventCtx) -> EventResult {
    // 鼠标：区域内左键点击 = 选中该项并执行 Enter 动作（click as enter）
    if let Event::Mouse(mouse) = event {
        if let Some(area) = ctx.area
            && !is_scrollbar_column(&mouse, area)
            && let Some(idx) = hit_item(&mouse, area, ctx.hit_layout())
        {
            select_theme_at(ctx, idx, true);
            return EventResult::Consumed;
        }
        return match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => EventResult::Consumed,
            _ => EventResult::Ignored,
        };
    }
    if let Event::Key(key) = event
        && (key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat)
    {
        match key.code {
            KeyCode::Tab => {
                // 切换 dark/light tab；选中身份不变，新 tab 不含该主题时由 effect 收敛
                let next = if *ctx.tab.read() == 0 { 1u8 } else { 0u8 };
                *ctx.tab.write() = next;
                return EventResult::Consumed;
            }
            KeyCode::Up => {
                if !ctx.themes.is_empty() {
                    select_theme_at(ctx, previous_selection(ctx.selected_index), false);
                }
                return EventResult::Consumed;
            }
            KeyCode::Down => {
                if !ctx.themes.is_empty() {
                    select_theme_at(
                        ctx,
                        next_selection(ctx.selected_index, ctx.themes.len()),
                        false,
                    );
                }
                return EventResult::Consumed;
            }
            KeyCode::Enter => {
                if !ctx.themes.is_empty() {
                    select_theme_at(ctx, ctx.selected_index, true);
                }
                return EventResult::Consumed;
            }
            KeyCode::Esc => {
                // 先恢复原始主题（预览回滚），再由容器关闭面板
                switch_theme_atoms(&ctx.original_theme);
                return EventResult::Ignored;
            }
            _ if key.modifiers == KeyModifiers::CONTROL => match key.code {
                KeyCode::Char('t') | KeyCode::Char('T') => {
                    // 将同步写盘移到独立线程，避免阻塞 TUI 主事件循环
                    std::thread::spawn(toggle_daily_color);
                    return EventResult::Consumed;
                }
                KeyCode::Char('d') | KeyCode::Char('D') => {
                    trigger_download_themes();
                    return EventResult::Consumed;
                }
                _ => {}
            },
            _ => {}
        }
    }
    EventResult::Ignored
}

/// 选中第 `idx` 项：选中身份改为该项主题名（稳定 key），实时切换全局颜色即预览；
/// `persist` 为 true 时按 Enter / 点击语义落盘。
fn select_theme_at(ctx: &ThemeEventCtx, idx: usize, persist: bool) {
    let Some(name) = ctx.themes.get(idx).cloned() else {
        return;
    };
    *ctx.selected_theme.write() = name.clone();
    switch_theme_atoms(&name);
    if persist {
        // 将同步写盘移到独立线程，避免阻塞 TUI 主事件循环
        std::thread::spawn(move || persist_theme(&name));
    }
}

/// 事件处理上下文：本帧视口 + 面板句柄 + 列表快照。
///
/// `State<T>` 无公开构造 API，只能从组件挂载取得；集中成一个结构体，事件入口签名不再
/// 随状态增长（命中布局与绘制必须共用同一个 `header_rows` 与本帧列表快照）。
struct ThemeEventCtx {
    area: Option<Rect>,
    tab: State<u8>,
    selected_theme: State<String>,
    /// 本帧主题列表快照：命中索引与动作目标同源
    themes: Vec<String>,
    /// 本帧选中落点（选中身份在列表中的下标）
    selected_index: usize,
    /// 本帧列表上方 header 行数——与绘制同源
    header_rows: u16,
    scroll_start: usize,
    /// 打开面板时的主题名（Esc 恢复预览用）
    original_theme: String,
}

impl ThemeEventCtx {
    /// 鼠标命中布局——header 行数取自本帧绘制同一份推导，不另算一份。
    fn hit_layout(&self) -> ListLayout {
        ListLayout {
            header_rows: self.header_rows,
            item_rows: 1,
            footer_rows: 1,
            visible_items: VISIBLE_ITEMS as u16,
            scroll_start: self.scroll_start,
            item_count: self.themes.len(),
        }
    }
}

/// 仅更新内存中的全局 Atom，不持久化。
fn switch_theme_atoms(name: &str) {
    match peri_theme::loader::load_theme(name) {
        Ok(theme) => {
            let palette = theme.to_palette();
            let peri = std::sync::Arc::new(theme.to_peri_colors());

            let mut theme_state = THEME_ATOM.state();
            theme_state.set(theme);

            let mut palette_state = PALETTE_ATOM.state();
            palette_state.set(palette);

            let mut peri_state = PERI_COLORS_ATOM.state();
            peri_state.set(peri);
        }
        Err(e) => {
            tracing::error!("failed to switch theme to '{}': {}", name, e);
        }
    }
}

#[cfg(test)]
#[path = "theme_test.rs"]
mod theme_test;

#[cfg(test)]
#[path = "theme_download_test.rs"]
mod theme_download_test;
