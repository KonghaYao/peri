//! ratatui-kit ModelPanel component.
//!
//! 左右分栏 Profile 编辑器：
//! - 左侧：4 个固定档位卡片（fable / opus / sonnet / haiku），↑/↓ 选择即切换 active profile；
//! - 右侧：当前 profile 的 K/V 编辑行（Provider / Model / Effort / Max tokens / 1m enable）。
//!
//! 右侧 `→`/`←` 切换字段值并立即写入内存 + 持久化 + 推送 ACP（无 Enter/Save 步骤）。
//! 所有 provider（含 Responses / ChatGPT plan）共用同一套行与动作，没有按 provider
//! 类型分叉的编辑面板或字段。右栏 K/V 行点击 = 聚焦并执行与键盘相同的动作；底部
//! “管理 Provider”按钮与 `Ctrl+L` 等价：经 panel_registry 打开 Login 管理当前 provider。

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms::{LANG_VERSION, PERI_CONFIG_HANDLE, SERVICE_SNAPSHOT};
use crate::kit::panel_mouse::AreaTracker;
use crate::kit::panels::shared::list::{ListMetrics, ListViewport, content_rows};
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};
use unicode_width::UnicodeWidthStr;

mod edit;
mod editor_view;
mod panel_input;
mod panel_render;
pub(crate) use edit::switch_active_alias;

// ---------------------------------------------------------------------------
// 静态常量
// ---------------------------------------------------------------------------

/// 固定四档（顺序即显示顺序：fable → opus → sonnet → haiku）
/// pub(crate)：状态栏模型快速切换弹窗（model_quick_switch.rs）复用同一顺序。
pub(crate) const PROFILE_KEYS: [&str; 4] = ["fable", "opus", "sonnet", "haiku"];

/// 右侧字段索引
const FIELD_PROVIDER: usize = 0;
const FIELD_MODEL: usize = 1;
const FIELD_EFFORT: usize = 2;
const FIELD_MAX_TOKENS: usize = 3;
const FIELD_CONTEXT_1M: usize = 4;
const FIELD_COUNT: usize = 5;

/// 右栏字段标签——顺序即 `FIELD_*` 索引顺序（编辑动作按索引定位字段，
/// 数组长度绑定 `FIELD_COUNT`，字段增删在编译期同步）。
const FIELD_LABELS: [&str; FIELD_COUNT] =
    ["Provider", "Model", "Effort", "Max tokens", "1m enable"];

/// 右侧 K/V 值右边缘目标列——所有行的值右对齐到该列（key 宽度不同也能对齐）。
/// 宽屏目标值；窄屏时按右列可用宽度收缩（见渲染处 `right_w` 计算）。
const VALUE_ALIGN_COL: usize = 40;

/// 左栏 profile 列表的行度量：header 2 行（标题、空行）+ 每档 3 行（主行、模型行、
/// 摘要行）+ footer 1 行（底部导航提示）。
///
/// 渲染（`window`）与鼠标命中（`hit_layout`）都取自本度量推导出的同一
/// `ListViewport`；此前命中布局把 `visible_items` 写死为 4 档，面板变矮后仍按
/// 4 档反推行号。
const LIST_METRICS: ListMetrics = ListMetrics::new(2, 3, 1);
use panel_input::{ModelEventCtx, active_profile_idx, current_active_alias, handle_model_event};
use panel_render::{provider_button_label, right_align_col, styled_model_name};

/// 首帧未测量自身区域时的面板宽度兜底（`use_previous_size` 首帧 width=0 的同一退守）。
const FALLBACK_PANEL_W: usize = 80;

#[component]
pub fn ModelPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    // hooks 全部无条件前置（TUI-HOOK-001）：这里到事件注册之间不得出现条件分支或
    // 提前 return——渲染路径变化时 hook 数量与类型必须一致。
    let theme_def = hooks.use_atom(&THEME_ATOM);
    // 左侧 profile 无独立光标：键盘导航基于宿主配置的 active_alias。
    // 移动，与渲染高亮（active_idx）天然一致——此前 cursor 固定从 0（fable）初始化，
    // active ≠ fable 时按 ↓ 会从当前档位跳到错误的下一档。
    let right_cursor = hooks.use_state(|| 0usize); // 右侧字段光标
    let right_focus = hooks.use_state(|| false); // 是否在右侧编辑焦点
    // 渲染版本计数器——edit_field/switch_active_alias 修改 PERI_CONFIG_HANDLE 后
    // 递增此计数，触发 ModelPanel 重渲染以显示最新值。
    let render_version = hooks.use_state(|| 0u64);
    // Model 面板编辑宿主配置；active alias 必须来自配置事实源，不能混用会话状态栏
    // 的 SERVICE_SNAPSHOT（会话可能仍为另一档模型）。订阅 snapshot 仅用于会话变化时
    // 触发重绘，面板选择和编辑始终读取 PERI_CONFIG_HANDLE。
    let _snapshot = hooks.use_atom(&SERVICE_SNAPSHOT);
    let active_alias = current_active_alias();
    // 左栏高亮、键盘导航起点与视口跟随都取同一档位下标：身份是静态 `PROFILE_KEYS`
    // 上的 key，配置刷新不会让下标指向别的档位。
    let active_idx = active_profile_idx(&active_alias);
    let _lang_ver = hooks.use_atom(&LANG_VERSION);

    // 打开面板即重读权威配置：外部写入后重新打开 /model 必须看到最新快照与新
    // revision，而不是进程启动时的缓存。无配置源（单测/lenient）时保持原值。
    hooks.use_effect(
        move || {
            if crate::kit::atoms::CONFIG_SOURCE_HANDLE.get().is_some() {
                match crate::kit::config_service::reload_configuration() {
                    Ok(_) => *render_version.write() += 1,
                    Err(error) => {
                        tracing::warn!(error = %error, "ModelPanel: 打开面板时重新载入配置失败")
                    }
                }
            }
        },
        (),
    );

    // 左侧 profile 列表的滚动状态（ScrollView 自身状态；列表窗口由
    // `viewport` 推导，鼠标命中不再读这里的偏移）。
    let left_scroll = hooks.use_state(ScrollViewState::default);
    // 右栏详情滚动——面板滚轮仲裁（panel_scroll.rs）驱动，统一 3 行/格 + 节流
    let right_scroll = hooks.use_state(ScrollViewState::default);
    // 面板绘制区域（上一帧，绝对坐标）——鼠标命中反推、滚轮仲裁注册与右列
    // 文本对齐共用这一份区域。
    let area = hooks.use_hook(AreaTracker::new).rect;

    // 左栏列表视口：绘制（window）与鼠标命中（hit_layout）共用同一实例，可见档位数
    // 由面板内容区行数推导——终端/面板高度变化后两者一起变化。
    let (_term_w, term_h) = hooks.use_terminal_size();
    let viewport = ListViewport::for_content_rows(
        content_rows(area, term_h),
        LIST_METRICS,
        PROFILE_KEYS.len(),
        active_idx,
    );

    hooks.use_event_handler_with_options(
        EventScope::Current,
        EventPriority::Normal,
        EventOptions { hit_test: true },
        move |event| {
            handle_model_event(
                event,
                &ModelEventCtx {
                    area,
                    right_cursor,
                    right_focus,
                    render_version,
                    right_scroll,
                    viewport,
                },
            )
        },
    );

    #[cfg(test)]
    tests::visit_mounted_panel(tests::MountedPanel {
        right_cursor,
        right_focus,
        render_version,
        right_scroll,
    });

    // ── 渲染（只读状态：TUI-RENDER-001，写状态只在事件与 effect 边界）──
    let theme = theme_def.read();

    // ── 标题 ──
    let title_line = Line::from(vec![Span::styled(
        i18n::tr("model-panel-title"),
        Style::new().fg(theme.semantic.text.primary).bold(),
    )]);

    // ── 左侧：Profile 卡片（窗口与鼠标命中共用组件里的同一视口）──
    let mut left_lines: Vec<Line<'static>> = Vec::new();
    for (i, key) in viewport.window(&PROFILE_KEYS) {
        let key = *key;
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
            let window = if profile.context_1m {
                "1m".to_owned()
            } else {
                "200k".to_owned()
            };
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
    // 面板宽度取自与鼠标命中同一份 `AreaTracker` 区域（首帧未测量时退守宽屏对齐）。
    let panel_w = area.map_or(FALLBACK_PANEL_W, |rect| rect.width as usize);
    let align = right_align_col(panel_w);

    // 值按 `FIELD_*` 索引顺序排列（Provider / Model / Effort / Max tokens / 1m enable）；
    // 数组长度绑定 `FIELD_COUNT`，字段增删在编译期与本表同步。
    let values: [String; FIELD_COUNT] = [
        provider_label,
        model_label,
        current_effort,
        current_max_tokens.to_string(),
        if current_ctx { "on" } else { "off" }.to_string(),
    ];
    // 右栏始终是同一张 K/V 表，所有 provider 共用同一渲染路径。
    for (fi, (label, value)) in FIELD_LABELS.iter().zip(values.iter()).enumerate() {
        let is_focus = *right_focus.read() && fi == *right_cursor.read();
        let mark = if is_focus { "❯" } else { " " };
        let key_span = format!(" {} {} ", mark, label);
        let key_len = UnicodeWidthStr::width(key_span.as_str());
        let value_len = UnicodeWidthStr::width(value.as_str());
        // key 宽度 + 填充 + 值宽度 = align，值右边缘对齐到同一列
        let pad = align.saturating_sub(key_len + value_len);
        let spans = vec![
            Span::styled(
                key_span,
                if is_focus {
                    Style::new().fg(theme.component.panel.title).bold()
                } else {
                    Style::new().fg(theme.semantic.text.muted)
                },
            ),
            Span::raw(" ".repeat(pad)),
            Span::styled(value.clone(), Style::new().fg(theme.semantic.text.primary)),
        ];
        right_lines.push(Line::from(spans));
    }
    let hint_text = i18n::tr("panel-model-nav-hint");

    // ── 中间分隔线 ──
    let divider_style = Style::new().fg(theme.semantic.border.default);
    let divider_lines: Vec<Line<'_>> = (0..30_usize)
        .map(|_| Line::from(Span::styled("│", divider_style)))
        .collect();

    // ── 底部：“管理 Provider”按钮 + 键盘提示 ──
    // 按钮文本与命中区共用 `provider_button_label()`；点击与 Ctrl+L 同一动作。
    let hint_line = Line::from(vec![
        Span::styled(
            provider_button_label(),
            Style::new().fg(theme.component.panel.title).bold(),
        ),
        Span::styled(
            format!("  {hint_text}"),
            Style::new().fg(theme.semantic.text.dim),
        ),
    ]);

    let left_para = Paragraph::new(ratatui::text::Text::from(left_lines));
    let right_para = Paragraph::new(ratatui::text::Text::from(right_lines));
    let divider_para = Paragraph::new(ratatui::text::Text::from(divider_lines));

    drop(theme);

    // 面板滚轮仲裁注册（双栏：按 45% 切分左右区域，divider 列并入右侧）——
    // 区域与鼠标命中同源，均取 AreaTracker 的上一帧区域（首帧未测量时为 0 尺寸，
    // 下一帧起生效）。
    let (left_area, right_area) =
        crate::kit::panel_scroll::split_vertical(area.unwrap_or_default(), 45);
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

#[cfg(test)]
#[path = "model_test.rs"]
mod tests;
