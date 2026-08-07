//! ratatui-kit ModelPanel component.
//!
//! 左右分栏 Profile 编辑器：
//! - 左侧：4 个固定档位卡片（fable / opus / sonnet / haiku），↑/↓ 选择即切换 active profile；
//! - 右侧：当前 profile 的 K/V 编辑行（Provider / Model / Effort / Max tokens / 1m enable）。
//!
//! 右侧 `→`/`←` 切换字段值并立即写入内存 + 持久化 + 推送 ACP（无 Enter/Save 步骤）。

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms::{
    ACP_CLIENT_HANDLE, LANG_VERSION, MODEL_HIGHLIGHT_UNTIL, NOTIFICATION, Notification,
    PERI_CONFIG_HANDLE, SERVICE_SNAPSHOT,
};
use crate::kit::list_nav::{next_selection, previous_selection};
use crate::kit::panel_mouse::{AreaTracker, ListLayout, hit_item};
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
use peri_theme::theme::ThemeDefinition;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

// ---------------------------------------------------------------------------
// 静态常量
// ---------------------------------------------------------------------------

/// 固定四档（顺序即显示顺序：fable → opus → sonnet → haiku）
/// pub(crate)：状态栏模型快速切换弹窗（model_quick_switch.rs）复用同一顺序。
pub(crate) const PROFILE_KEYS: [&str; 4] = ["fable", "opus", "sonnet", "haiku"];

/// Effort 五级
const EFFORT_LEVELS: &[&str] = &["low", "medium", "high", "xhigh", "max"];
/// Max tokens 预设
const MAX_TOKEN_PRESETS: &[u32] = &[4096, 8192, 16000, 32000, 64000];

/// 右侧字段索引
const FIELD_PROVIDER: usize = 0;
const FIELD_MODEL: usize = 1;
const FIELD_EFFORT: usize = 2;
const FIELD_MAX_TOKENS: usize = 3;
const FIELD_CONTEXT_1M: usize = 4;
const FIELD_COUNT: usize = 5;

/// 右侧 K/V 值右边缘目标列——所有行的值右对齐到该列（key 宽度不同也能对齐）。
/// 宽屏目标值；窄屏时按右列可用宽度收缩（见渲染处 `right_w` 计算）。
const VALUE_ALIGN_COL: usize = 40;

#[component]
pub fn ModelPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    // 左侧 profile 无独立光标：键盘导航基于 active_alias（SERVICE_SNAPSHOT 的事实源）
    // 移动，与渲染高亮（active_idx）天然一致——此前 cursor 固定从 0（fable）初始化，
    // active ≠ fable 时按 ↓ 会从当前档位跳到错误的下一档。
    let right_cursor = hooks.use_state(|| 0usize); // 右侧字段光标
    let right_focus = hooks.use_state(|| false); // 是否在右侧编辑焦点
    // 渲染版本计数器——edit_field/switch_active_alias 修改 PERI_CONFIG_HANDLE 后
    // 递增此计数，触发 ModelPanel 重渲染以显示最新值。
    let render_version = hooks.use_state(|| 0u64);
    // S6c: 订阅 SERVICE_SNAPSHOT——active alias 来自 atom，确保面板和 status bar 一致
    let snapshot = hooks.use_atom(&SERVICE_SNAPSHOT);
    let active_alias = snapshot.read().model_alias.clone();
    let _ = snapshot; // StoreState 是 Copy，无需显式 drop
    let _lang_ver = hooks.use_atom(&LANG_VERSION);

    // 左侧 profile 列表的滚动状态——鼠标点击行号反推需要滚动偏移（外部受控，
    // 否则列表滚动后点击命中错位）。`hooks.use_state` 顺序稳定，位于条件渲染之前。
    let left_scroll = hooks.use_state(ScrollViewState::default);
    // 右栏详情滚动——面板滚轮仲裁（panel_scroll.rs）驱动，统一 3 行/格 + 节流
    let right_scroll = hooks.use_state(ScrollViewState::default);
    // 面板绘制区域（上一帧）——鼠标点击行号反推（值拷贝模式，见 panel_mouse.rs）
    let area;
    {
        let tracker = hooks.use_hook(AreaTracker::new);
        area = tracker.rect;
    }

    let rv = render_version;
    // 事件闭包 move 捕获；渲染部分仍使用 active_alias，故此处克隆一份给闭包
    let handler_alias = active_alias.clone();
    let left_scroll_for_handler = left_scroll;
    hooks.use_event_handler_with_options(
        EventScope::Current,
        EventPriority::Normal,
        EventOptions { hit_test: true },
        {
            move |event| {
                // 鼠标：区域内左键点击左侧 profile 卡片行 = 选中并切换（click as enter）。
                // 左侧栏 = 主区宽 45%（panel_shell 左右边框各 1 列）；ScrollView 滚动条
                // 占其最右 1 列，点击该列排除（不触发切换）。
                if let Event::Mouse(mouse) = event {
                    if let Some(area) = area {
                        let left_w = (area.width.saturating_sub(2)) * 45 / 100;
                        let left_max_col = area.x.saturating_add(1).saturating_add(left_w);
                        if mouse.column < left_max_col.saturating_sub(1)
                            && let Some(idx) = hit_item(
                                &mouse,
                                area,
                                ListLayout {
                                    header_rows: 2, // 标题行 + 空行
                                    item_rows: 3,   // 每档卡片 3 行（主行/模型行/摘要行）
                                    footer_rows: 1, // 底部导航提示
                                    visible_items: PROFILE_KEYS.len() as u16,
                                    scroll_start: left_scroll_for_handler.read().offset().y
                                        as usize,
                                    item_count: PROFILE_KEYS.len(),
                                },
                            )
                        {
                            switch_active_alias(idx);
                            return EventResult::Consumed;
                        }
                    }
                    // 面板区域内未命中行/右侧 K/V 区：消费防穿透（与其它面板一致）
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
                    KeyCode::Esc => {
                        if *right_focus.read() {
                            // 退出右侧编辑焦点
                            *right_focus.write() = false;
                        } else {
                            // 全局 Esc 由 panel_overlay 处理；此处返回 Ignored 让其关闭面板
                            return EventResult::Ignored;
                        }
                    }
                    KeyCode::Up => {
                        if *right_focus.read() {
                            let mut c = right_cursor.write();
                            *c = previous_selection(*c);
                        } else {
                            // 从当前 active 档位出发上移（与渲染高亮一致）
                            let idx = PROFILE_KEYS
                                .iter()
                                .position(|k| *k == handler_alias)
                                .unwrap_or(1);
                            switch_active_alias(previous_selection(idx));
                        }
                    }
                    KeyCode::Down => {
                        if *right_focus.read() {
                            let mut c = right_cursor.write();
                            *c = next_selection(*c, FIELD_COUNT);
                        } else {
                            // 从当前 active 档位出发下移（与渲染高亮一致）
                            let idx = PROFILE_KEYS
                                .iter()
                                .position(|k| *k == handler_alias)
                                .unwrap_or(1);
                            switch_active_alias(next_selection(idx, PROFILE_KEYS.len()));
                        }
                    }
                    KeyCode::Tab => {
                        // 左右焦点切换：左侧 → 右侧，右侧 → 左侧
                        let rf = *right_focus.read();
                        *right_focus.write() = !rf;
                    }
                    KeyCode::Right => {
                        if *right_focus.read() {
                            edit_field(handler_alias.clone(), *right_cursor.read(), true);
                            *rv.write() += 1;
                        } else {
                            // 进入右侧编辑焦点
                            *right_focus.write() = true;
                        }
                    }
                    KeyCode::Left => {
                        if *right_focus.read() {
                            edit_field(handler_alias.clone(), *right_cursor.read(), false);
                            *rv.write() += 1;
                        } else {
                            *right_focus.write() = false;
                        }
                    }
                    _ => {}
                }
                EventResult::Consumed
            }
        },
    );

    let theme = theme_def.read();

    // ── 标题 ──
    let title_line = Line::from(vec![Span::styled(
        i18n::tr("model-panel-title"),
        Style::new().fg(theme.semantic.text.primary).bold(),
    )]);

    // ── 左侧：Profile 卡片 ──
    let active_idx = PROFILE_KEYS
        .iter()
        .position(|k| *k == active_alias)
        .unwrap_or(1);
    let mut left_lines: Vec<Line<'static>> = Vec::new();
    for (i, key) in PROFILE_KEYS.iter().enumerate() {
        let cfg = PERI_CONFIG_HANDLE.get().map(|h| h.read().clone());
        let (provider_label, model_label, effort_label, window_label) = if let Some(cfg) = &cfg {
            let profile = cfg.config.profiles.get(key).unwrap();
            let prov = if profile.provider.is_empty() {
                cfg.config.providers.first()
            } else {
                cfg.config
                    .providers
                    .iter()
                    .find(|p| p.id == profile.provider)
            };
            let model = profile
                .model
                .clone()
                .filter(|m| !m.is_empty())
                .or_else(|| {
                    prov.and_then(|p| p.models.get_model(key))
                        .map(str::to_string)
                })
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| key.to_string());
            let window = if profile.context_1m { "1m" } else { "200k" };
            (
                prov.map(|p| p.display_name().to_string())
                    .unwrap_or_else(|| profile.provider.clone()),
                model,
                profile.effort.clone(),
                window.to_string(),
            )
        } else {
            (
                String::new(),
                key.to_string(),
                "xhigh".to_string(),
                "200k".to_string(),
            )
        };
        let is_active = i == active_idx;
        let mark = if is_active { "●" } else { "○" };
        left_lines.push(Line::from(vec![Span::styled(
            format!(" {} {} · {}", mark, key, provider_label),
            if is_active {
                Style::new().fg(theme.semantic.status.success).bold()
            } else {
                Style::new().fg(theme.semantic.text.primary)
            },
        )]));
        // 模型名行：含 effort 后缀（如 "gpt-5.6-luna high"）时后缀用 model accent 色
        let mut model_spans = vec![Span::raw("    ")];
        model_spans.extend(styled_model_name(&model_label, &theme));
        left_lines.push(Line::from(model_spans));
        // 摘要行：effort 用 effort 色，窗口标识用 token_context 色
        left_lines.push(Line::from(vec![
            Span::styled("    ", Style::new()),
            Span::styled(effort_label, Style::new().fg(theme.semantic.effort).bold()),
            Span::styled(" · ", Style::new().fg(theme.semantic.text.muted)),
            Span::styled(window_label, Style::new().fg(theme.semantic.token_context)),
        ]));
    }

    // ── 右侧：当前 profile 的 K/V 编辑行 ──
    let mut right_lines: Vec<Line<'static>> = Vec::new();
    let (current_effort, current_max_tokens, current_ctx) = PERI_CONFIG_HANDLE
        .get()
        .map(|h| {
            let c = h.read();
            let profile = c.config.profiles.get(&active_alias);
            (
                profile
                    .map(|p| p.effort.clone())
                    .unwrap_or_else(|| "xhigh".to_string()),
                profile.map(|p| p.max_tokens).unwrap_or(32000),
                profile.map(|p| p.context_1m).unwrap_or(false),
            )
        })
        .unwrap_or_else(|| ("xhigh".to_string(), 32000, false));

    let (provider_label, model_label) = PERI_CONFIG_HANDLE
        .get()
        .map(|h| {
            let c = h.read();
            let profile = c.config.profiles.get(&active_alias);
            let prov = profile.and_then(|pf| {
                if pf.provider.is_empty() {
                    c.config.providers.first()
                } else {
                    c.config.providers.iter().find(|p| p.id == pf.provider)
                }
            });
            let model = profile
                .and_then(|pf| pf.model.clone().filter(|m| !m.is_empty()))
                .or_else(|| {
                    prov.and_then(|p| p.models.get_model(&active_alias))
                        .map(str::to_string)
                })
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| active_alias.clone());
            (
                prov.map(|p| p.display_name().to_string())
                    .unwrap_or_else(|| profile.map(|pf| pf.provider.clone()).unwrap_or_default()),
                model,
            )
        })
        .unwrap_or_else(|| (String::new(), active_alias.clone()));

    // ── 响应式右列宽度：窄屏时 VALUE_ALIGN_COL 收缩，避免值被截断 ──
    // 首帧 use_previous_size 返回 width=0，退守 80 列（宽屏对齐），下一帧修正。
    let prev_size = hooks.use_previous_size();
    let panel_w = if prev_size.width > 0 {
        prev_size.width as usize
    } else {
        80
    };
    let align = right_align_col(panel_w);

    let rows: Vec<(&str, String)> = vec![
        ("Provider", provider_label),
        ("Model", model_label),
        ("Effort", current_effort),
        ("Max tokens", current_max_tokens.to_string()),
        (
            "1m enable",
            if current_ctx { "on" } else { "off" }.to_string(),
        ),
    ];
    for (fi, (k, v)) in rows.iter().enumerate() {
        let is_focus = *right_focus.read() && fi == *right_cursor.read();
        let mark = if is_focus { "❯" } else { " " };
        let key_span = format!(" {} {} ", mark, k);
        let key_len = UnicodeWidthStr::width(key_span.as_str());
        let value_len = UnicodeWidthStr::width(v.as_str());
        // key 宽度 + 填充 + 值宽度 = align，值右边缘对齐到同一列
        let pad = align.saturating_sub(key_len + value_len);
        right_lines.push(Line::from(vec![
            Span::styled(
                key_span,
                if is_focus {
                    Style::new().fg(theme.component.panel.title).bold()
                } else {
                    Style::new().fg(theme.semantic.text.muted)
                },
            ),
            Span::styled(
                format!("{}{}", " ".repeat(pad), v),
                Style::new().fg(theme.semantic.text.primary),
            ),
        ]));
    }

    // ── 中间分隔线 ──
    let divider_style = Style::new().fg(theme.semantic.border.default);
    let divider_lines: Vec<Line<'_>> = (0..30_usize)
        .map(|_| Line::from(Span::styled("│", divider_style)))
        .collect();

    // ── 底部导航提示 ──
    let hint_line = Line::from(i18n::tr("panel-model-nav-hint")).fg(theme.semantic.text.dim);

    let left_para = Paragraph::new(ratatui::text::Text::from(left_lines));
    let right_para = Paragraph::new(ratatui::text::Text::from(right_lines));
    let divider_para = Paragraph::new(ratatui::text::Text::from(divider_lines));

    drop(theme);

    // 面板滚轮仲裁注册（双栏：按 45% 切分左右区域，divider 列并入右侧）
    let (left_area, right_area) = crate::kit::panel_scroll::split_vertical(prev_size, 45);
    crate::kit::panel_scroll::register_panel_scrolls(
        PanelKind::Model,
        vec![
            crate::kit::panel_scroll::PanelScrollSlot {
                area: left_area,
                state: left_scroll,
            },
            crate::kit::panel_scroll::PanelScrollSlot {
                area: right_area,
                state: right_scroll,
            },
        ],
    );

    panel_shell!(PanelKind::Model, {
        View(height: Constraint::Length(1)) {
            Text(text: title_line)
        }
        View(height: Constraint::Length(1)) {}
        View(
            flex_direction: Direction::Horizontal,
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            View(width: Constraint::Percentage(45), height: Constraint::Fill(1)) {
                ScrollView(
                    scrollbars: crate::kit::panel_registry::clean_scrollbars(),
                    state: Some(left_scroll),
                    width: Constraint::Fill(1),
                    height: Constraint::Fill(1),
                ) {
                    Text(text: left_para)
                }
            }
            View(width: Constraint::Length(1), height: Constraint::Fill(1)) {
                Text(text: divider_para)
            }
            View(width: Constraint::Fill(1), height: Constraint::Fill(1)) {
                ScrollView(
                    scrollbars: crate::kit::panel_registry::clean_scrollbars(),
                    state: Some(right_scroll),
                    width: Constraint::Fill(1),
                    height: Constraint::Fill(1),
                ) {
                    Text(text: right_para)
                }
            }
        }
        View(height: Constraint::Length(1)) {
            Text(text: hint_line)
        }
    })
}

/// 切换左侧光标指向的档位为 active profile（立即写入 + 持久化 + 推送 ACP）。
/// pub(crate)：状态栏模型快速切换弹窗复用此切换逻辑。
pub(crate) fn switch_active_alias(idx: usize) {
    let Some(key) = PROFILE_KEYS.get(idx) else {
        return;
    };
    let Some(handle) = PERI_CONFIG_HANDLE.get() else {
        return;
    };
    let mut cfg = handle.write();
    if cfg.config.active_alias != *key {
        cfg.config.active_alias = key.to_string();
        tracing::info!(alias = key, "ModelPanel: active_alias switched");
    }
    let snap = cfg.clone();
    drop(cfg);
    notify_save_result(crate::config::save(&snap));
    let resolved_name = resolve_model_name_for_alias(&snap.config, key);
    let s_handle = SERVICE_SNAPSHOT.state();
    let mut svc_snap = s_handle.read().clone();
    svc_snap.model_alias = key.to_string();
    svc_snap.model_name = resolved_name;
    svc_snap.effort = snap
        .config
        .profiles
        .get(key)
        .map(|p| p.effort.clone())
        .unwrap_or_else(|| "xhigh".to_string());
    *s_handle.write() = svc_snap;
    *MODEL_HIGHLIGHT_UNTIL.state().write() = Some(Instant::now() + Duration::from_secs(2));
    // 推送配置到 ACP 服务端，使 alias 切换立即生效
    tokio::spawn(async move {
        if let Some(client) = ACP_CLIENT_HANDLE.get()
            && let Err(e) = client.update_config(&snap).await
        {
            tracing::warn!(error = %e, "ModelPanel: update_config push failed");
        }
    });
}

/// 编辑右侧字段（forward=true 前进 / false 后退）。立即写入 + 持久化 + 推送 ACP。
fn edit_field(alias: String, field: usize, forward: bool) {
    let Some(handle) = PERI_CONFIG_HANDLE.get() else {
        return;
    };
    let mut cfg = handle.write();

    // 先读取当前值（不可变，纯 clone），避免跨 guard 的字段级借用冲突
    let provider_ids: Vec<String> = cfg.config.providers.iter().map(|p| p.id.clone()).collect();
    let profile_provider = cfg
        .config
        .profiles
        .get(&alias)
        .map(|p| p.provider.clone())
        .unwrap_or_default();
    // 当前显示的模型名：profile.model 未手动设置时回退到 provider 同档位映射，
    // 否则 FIELD_MODEL 定位 idx 落空（unwrap_or(0)）导致首次 → 恰好选中 fallback
    // 显示值、视觉上"切换未生效"。
    let current_model = cfg
        .config
        .profiles
        .get(&alias)
        .and_then(|p| p.model.clone())
        .or_else(|| {
            cfg.config
                .providers
                .iter()
                .find(|p| p.id == profile_provider)
                .and_then(|p| p.models.get_model(&alias))
                .map(str::to_string)
        })
        .unwrap_or_default();
    let current_effort = cfg
        .config
        .profiles
        .get(&alias)
        .map(|p| p.effort.clone())
        .unwrap_or_else(|| "xhigh".to_string());
    let current_max = cfg
        .config
        .profiles
        .get(&alias)
        .map(|p| p.max_tokens)
        .unwrap_or(32000);
    let current_ctx = cfg
        .config
        .profiles
        .get(&alias)
        .map(|p| p.context_1m)
        .unwrap_or(false);

    match field {
        FIELD_PROVIDER => {
            if provider_ids.is_empty() {
                return;
            }
            let idx = provider_ids
                .iter()
                .position(|i| *i == profile_provider)
                .unwrap_or(0);
            let next = provider_ids
                [(idx + if forward { 1 } else { provider_ids.len() - 1 }) % provider_ids.len()]
            .clone();
            // 联动：目标 provider 同档位映射 → 覆盖 profile.model；无映射 → None 触发回退
            let mapped = cfg
                .config
                .providers
                .iter()
                .find(|p| p.id == next)
                .and_then(|p| p.models.get_model(&alias))
                .map(str::to_string)
                .filter(|m| !m.is_empty());
            if let Some(profile) = cfg.config.profiles.get_mut(&alias) {
                profile.provider = next;
                profile.model = mapped;
            }
        }
        FIELD_MODEL => {
            let provider = cfg
                .config
                .providers
                .iter()
                .find(|p| p.id == profile_provider);
            let Some(provider) = provider else {
                return;
            };
            // 候选 = provider 四个档位的全部模型名（去空、去重）+ 当前手动模型保底；
            // 直接读字段而非 get_model，避免 fable 空回退 opus 造成重复
            let mut models: Vec<String> = Vec::new();
            for tier_model in [
                &provider.models.opus,
                &provider.models.sonnet,
                &provider.models.haiku,
                &provider.models.fable,
            ] {
                if !tier_model.is_empty() && !models.contains(tier_model) {
                    models.push(tier_model.clone());
                }
            }
            if !models.contains(&current_model) && !current_model.is_empty() {
                models.insert(0, current_model.clone());
            }
            if models.is_empty() {
                return;
            }
            let idx = models.iter().position(|m| *m == current_model).unwrap_or(0);
            let next =
                models[(idx + if forward { 1 } else { models.len() - 1 }) % models.len()].clone();
            if let Some(profile) = cfg.config.profiles.get_mut(&alias) {
                profile.model = Some(next);
            }
        }
        FIELD_EFFORT => {
            let cur = EFFORT_LEVELS
                .iter()
                .position(|e| *e == current_effort)
                .unwrap_or(0);
            let next = EFFORT_LEVELS
                [(cur + if forward { 1 } else { EFFORT_LEVELS.len() - 1 }) % EFFORT_LEVELS.len()]
            .to_string();
            if let Some(profile) = cfg.config.profiles.get_mut(&alias) {
                profile.effort = next;
            }
        }
        FIELD_MAX_TOKENS => {
            let cur = MAX_TOKEN_PRESETS
                .iter()
                .position(|v| *v == current_max)
                .unwrap_or(0);
            let next = MAX_TOKEN_PRESETS[(cur
                + if forward {
                    1
                } else {
                    MAX_TOKEN_PRESETS.len() - 1
                })
                % MAX_TOKEN_PRESETS.len()];
            if let Some(profile) = cfg.config.profiles.get_mut(&alias) {
                profile.max_tokens = next;
            }
        }
        FIELD_CONTEXT_1M => {
            if let Some(profile) = cfg.config.profiles.get_mut(&alias) {
                profile.context_1m = !current_ctx;
            }
        }
        _ => return,
    }
    let snap = cfg.clone();
    drop(cfg);
    notify_save_result(crate::config::save(&snap));
    // 推送 ACP 服务端 + 刷新 SERVICE_SNAPSHOT（模型名可能变化）
    let resolved = resolve_model_name_for_alias(&snap.config, &alias);
    let s_handle = SERVICE_SNAPSHOT.state();
    let mut svc = s_handle.read().clone();
    if alias == snap.config.active_alias {
        svc.model_name = resolved;
        svc.effort = snap
            .config
            .profiles
            .get(&alias)
            .map(|p| p.effort.clone())
            .unwrap_or_else(|| "xhigh".to_string());
        let provider_type = snap
            .config
            .profiles
            .get(&alias)
            .and_then(|pf| snap.config.providers.iter().find(|p| p.id == pf.provider))
            .map(|p| p.provider_type.clone())
            .unwrap_or_default();
        if !provider_type.is_empty() {
            svc.provider_name = provider_type;
        }
    }
    *s_handle.write() = svc;
    tokio::spawn(async move {
        if let Some(client) = ACP_CLIENT_HANDLE.get()
            && let Err(e) = client.update_config(&snap).await
        {
            tracing::warn!(error = %e, "ModelPanel: update_config push failed");
        }
    });
}

/// 解析档位实际模型名：Profile.model > ProviderModels 映射 > alias label。
fn resolve_model_name_for_alias(app_config: &crate::config::AppConfig, alias: &str) -> String {
    let profile = app_config.profiles.get(alias);
    let provider = profile.and_then(|pf| {
        if pf.provider.is_empty() {
            app_config.providers.first()
        } else {
            app_config.providers.iter().find(|p| p.id == pf.provider)
        }
    });
    profile
        .and_then(|pf| pf.model.clone().filter(|m| !m.is_empty()))
        .or_else(|| {
            provider
                .and_then(|p| p.models.get_model(alias))
                .map(str::to_string)
        })
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| alias.to_string())
}

/// 模型名内嵌 effort 后缀（如 "gpt-5.6-luna high"）：主色用 model_info，后缀用 model accent 色高亮。
fn styled_model_name(model: &str, theme: &ThemeDefinition) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let lower = model.to_lowercase();
    for level in [" low", " medium", " high", " xhigh", " max"] {
        if let Some(pos) = lower.rfind(level) {
            let (head, tail) = model.split_at(pos + 1);
            spans.push(Span::styled(
                head.to_string(),
                Style::new().fg(theme.semantic.model_info),
            ));
            spans.push(Span::styled(
                tail.to_string(),
                Style::new().fg(theme.semantic.model_accent).bold(),
            ));
            return spans;
        }
    }
    spans.push(Span::styled(
        model.to_string(),
        Style::new().fg(theme.semantic.model_info),
    ));
    spans
}

fn notify_save_result(result: Result<(), anyhow::Error>) {
    match result {
        Ok(()) => {
            *NOTIFICATION.state().write() = Some(Notification {
                message: i18n::tr("config-saved").to_string(),
                until: Instant::now() + Duration::from_secs(1),
            });
        }
        Err(e) => {
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

/// 右侧 K/V 行对齐列：宽屏保持 VALUE_ALIGN_COL 右对齐；窄屏收缩到右列可容纳的最大宽度。
///
/// 布局为 `Percentage(45) | Length(1) | Fill(1)`，右列宽 = 面板宽 - 45% - 1。
/// 行总宽 = key_span + pad + value = align，故 align 上限为右列宽 - 4（mark + 两侧空格余量），
/// 保证窄屏下行宽不超过右列可视区域，值不被截断。
fn right_align_col(panel_width: usize) -> usize {
    let right_w = panel_width.saturating_sub(panel_width * 45 / 100 + 1);
    VALUE_ALIGN_COL.min(right_w.saturating_sub(4))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn right_align_col_keeps_wide_screen_value() {
        // 宽屏（≥100 列）：保持 40 列右对齐目标
        assert_eq!(right_align_col(100), 40);
        assert_eq!(right_align_col(120), 40);
    }

    #[test]
    fn right_align_col_shrinks_on_narrow_screen() {
        // 窄屏：收缩到右列可容纳宽度（面板 - 45% 左列 - 分隔线 - 4 余量）
        assert_eq!(right_align_col(80), 39); // 80-36-1-4
        assert_eq!(right_align_col(60), 28); // 60-27-1-4
        assert_eq!(right_align_col(50), 23); // 50-22-1-4
        assert_eq!(right_align_col(40), 17); // 40-18-1-4
    }

    #[test]
    fn right_align_col_never_underflows() {
        // 极窄：saturating 保证不为负
        assert_eq!(right_align_col(10), 1); // 10-4-1-4
        assert_eq!(right_align_col(0), 0); // 0*45/100=0 → 0-0-1 saturating → 0
    }

    #[test]
    fn pad_fits_align_column() {
        // pad + key + value = align（值右边缘对齐），且 pad 不为负
        let align = right_align_col(60);
        let key_len = 9; // "    Model "
        let value_len = 12; // "gpt-5.6-luna"
        let pad = align.saturating_sub(key_len + value_len);
        assert_eq!(pad + key_len + value_len, align);
        assert!(pad < 32, "行宽应不超过右列可视宽度");
    }
}
