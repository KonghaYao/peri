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
use std::sync::OnceLock;
use std::time::Duration;

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
    ratatui::{
        layout::Constraint,
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms::{LANG_VERSION, NOTIFICATION, Notification, PERI_CONFIG_HANDLE};
use crate::kit::list_nav::{next_selection, previous_selection, scroll_start_for_selected};
use crate::kit::markdown::{self, MarkdownSegment};
use fluent_bundle::FluentValue;
use peri_theme::atoms::{PALETTE_ATOM, PERI_COLORS_ATOM, THEME_ATOM};
use peri_theme::bridge::ThemeDefinitionExt;
use peri_theme::loader::list_available_themes;
use peri_theme::theme::ThemeMode;
use std::time::Instant;

static THEME_LIST: OnceLock<Vec<String>> = OnceLock::new();

fn get_theme_list() -> &'static Vec<String> {
    THEME_LIST.get_or_init(list_available_themes)
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
        })
        .collect()
}

/// 读取持久化配置中的主题名。
fn persisted_theme_name() -> String {
    PERI_CONFIG_HANDLE
        .get()
        .and_then(|h| h.read().config.theme.clone())
        .unwrap_or_else(|| "peri-dark".to_string())
}

/// 持久化当前选中主题到 settings.json。
fn persist_theme(name: &str) {
    let Some(handle) = PERI_CONFIG_HANDLE.get() else {
        return;
    };
    let mut cfg = handle.write();
    cfg.config.theme = Some(name.to_string());
    let snap = cfg.clone();
    drop(cfg);
    match crate::config::save(&snap) {
        Ok(()) => {
            *NOTIFICATION.state().write() = Some(Notification {
                message: i18n::tr("config-saved").to_string(),
                until: Instant::now() + Duration::from_secs(1),
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
                until: Instant::now() + Duration::from_secs(2),
            });
        }
    }
}

/// 主题列表中每个条目一行，面板高度 24 - border 2 - header 3 - preview 9 - separator 1 - footer 1 = 8。
const VISIBLE_ITEMS: usize = 8;

#[component]
pub fn ThemePanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let _ = hooks.use_atom(&LANG_VERSION);

    // 打开面板时：记住原始主题（Esc 恢复用），切到持久化主题
    let original_theme = hooks.use_state(|| theme_def.read().name.to_string());
    let _init_switch = hooks.use_state(|| {
        let persisted = persisted_theme_name();
        if persisted != theme_def.read().name.as_str() {
            switch_theme_atoms(&persisted);
        }
        true
    });

    // tab 状态：0 = dark, 1 = light
    let tab = hooks.use_state(|| 0u8);

    // 按 mode 分类所有主题（一次性初始化）
    let theme_kinds = hooks.use_state(|| {
        let mut dark = Vec::new();
        let mut light = Vec::new();
        for name in get_theme_list() {
            match peri_theme::loader::load_theme(name) {
                Ok(t) => match t.mode {
                    ThemeMode::Dark | ThemeMode::HighContrast => dark.push(name.clone()),
                    ThemeMode::Light => light.push(name.clone()),
                },
                Err(_) => continue,
            }
        }
        (dark, light)
    });

    // 根据 tab 获取当前主题列表
    let current_list = {
        let kinds = theme_kinds.read();
        match *tab.read() {
            0 => kinds.0.clone(),
            _ => kinds.1.clone(),
        }
    };
    let other_count = {
        let kinds = theme_kinds.read();
        match *tab.read() {
            0 => kinds.1.len(),
            _ => kinds.0.len(),
        }
    };

    // 选中索引：tab 切换时重置为 0
    let selected = hooks.use_state(|| {
        let themes = &current_list;
        let current = theme_def.read().name.to_string();
        themes.iter().position(|name| *name == current).unwrap_or(0)
    });
    // 监听 tab 变化：重置 selected
    let _reset_selection = hooks.use_state(|| {
        // HACK：用 use_state 的 drop→recreate 来重置
        // 这里放一个 tab guard，如果 tab 变了，外部 should_reset 会触发
        0usize
    });
    let prev_tab = hooks.use_state(|| 0u8);
    if *prev_tab.read() != *tab.read() {
        *prev_tab.write_no_update() = *tab.read();
        let current = theme_def.read().name.to_string();
        let pos = current_list.iter().position(|n| *n == current).unwrap_or(0);
        *selected.write_no_update() = pos;
    }

    // 预览缓存：theme_name → 渲染后的 Line 列表
    let preview_cache = hooks.use_state(HashMap::<String, Vec<Line<'static>>>::new);

    let persisted_name = persisted_theme_name();
    let themes = current_list.clone();
    let count = themes.len();
    let themes_for_closure = themes.clone(); // clone for closure capture

    let orig = original_theme.read().clone();

    // ── 键盘处理 ──
    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, {
        let count = themes_for_closure.len();
        move |event| {
            if let Event::Key(key) = event
                && (key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat)
            {
                match key.code {
                    KeyCode::Tab => {
                        // 切换 dark/light tab
                        let next = if *tab.read() == 0 { 1u8 } else { 0u8 };
                        *tab.write() = next;
                        return EventResult::Consumed;
                    }
                    KeyCode::Up => {
                        if count > 0 {
                            let idx = previous_selection(*selected.read());
                            *selected.write() = idx;
                            if let Some(name) = themes_for_closure.get(idx) {
                                switch_theme_atoms(name);
                            }
                        }
                        return EventResult::Consumed;
                    }
                    KeyCode::Down => {
                        if count > 0 {
                            let idx = next_selection(*selected.read(), count);
                            *selected.write() = idx;
                            if let Some(name) = themes_for_closure.get(idx) {
                                switch_theme_atoms(name);
                            }
                        }
                        return EventResult::Consumed;
                    }
                    KeyCode::Enter => {
                        if count > 0 {
                            let idx = *selected.read();
                            if let Some(name) = themes_for_closure.get(idx) {
                                switch_theme_atoms(name);
                                persist_theme(name);
                            }
                        }
                        return EventResult::Consumed;
                    }
                    KeyCode::Esc => {
                        switch_theme_atoms(&orig);
                        return EventResult::Ignored;
                    }
                    _ => {}
                }
            }
            EventResult::Ignored
        }
    });

    let sel = *selected.read();
    let guard = theme_def.read();
    let semantic = &guard.semantic;

    let header_style = Style::new().fg(semantic.text.primary).bold();
    let muted_style = Style::new().fg(semantic.text.muted).italic();
    let dim_style = Style::new().fg(semantic.text.dim);
    let selected_style = Style::new().fg(guard.component.panel.title).bold();
    let separator_style = Style::new().fg(semantic.border.dim);

    let mut lines: Vec<Line<'_>> = Vec::new();

    // ── Tab 切换栏 ──
    let current_tab = *tab.read();
    let tab_reverse_fg = semantic.surface.default; // 反色前景 = 背景色
    let tab_active_style = Style::new().fg(tab_reverse_fg).bg(semantic.accent);
    let tab_inactive_style = Style::new().fg(semantic.text.muted);
    let (dark_style, light_style) = if current_tab == 0 {
        (tab_active_style, tab_inactive_style)
    } else {
        (tab_inactive_style, tab_active_style)
    };
    let dark_count = theme_kinds.read().0.len();
    let light_count = theme_kinds.read().1.len();
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

    // ── 预览 ──
    let sel_name = themes.get(sel).cloned().unwrap_or_default();
    let preview_lines = {
        let cache = preview_cache.read();
        if let Some(cached) = cache.get(&sel_name) {
            cached.clone()
        } else {
            drop(cache);
            let lines = build_preview(&sel_name);
            preview_cache.write().insert(sel_name, lines.clone());
            lines
        }
    };

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
    let scroll_start = if count == 0 {
        0
    } else {
        scroll_start_for_selected(sel, count, VISIBLE_ITEMS)
    };

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
    lines.push(Line::from(vec![Span::styled(
        "  \u{2191}/\u{2193}::navigate  Enter::apply+save  Esc::revert  Tab::switch",
        muted_style,
    )]));

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    panel_shell!(PanelKind::Theme, {
        ScrollView(
            scrollbars: crate::kit::panel_registry::clean_scrollbars(),
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            Text(text: content)
        }
    })
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
