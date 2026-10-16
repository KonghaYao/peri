//! Config 面板的行模型、行显示与纯变换。
//!
//! 行模型 = 静态基础行（[`CONFIG_ROWS`]）+ 注册表驱动的 beta 区块；命中、导航
//! 与渲染都以行模型的下标为身份——下标到语义静态稳定，不随外部刷新漂移。
//!
//! 本模块只读取句柄并做纯变换（`apply_*` 不触碰任何句柄）；写入与保存边界见
//! `actions`。

use crate::config::{PeriConfig, TuiConfig};
use crate::i18n;
use crate::kit::atoms::{PERI_CONFIG_HANDLE, PERMISSION_MODE_HANDLE, TUI_CONFIG_HANDLE};
use peri_acp_types::beta_flags::BETA_FLAGS;
use peri_acp_types::permission::PermissionMode;
use peri_theme::theme::ThemeDefinition;
use ratatui_kit::ratatui::{
    style::Style,
    text::{Line, Span},
};

// ---------------------------------------------------------------------------
// 行类型与基础行表
// ---------------------------------------------------------------------------

/// 行的交互类型：Toggle 反转，Cycle 在一组选项内前进/后退。
#[derive(Clone)]
pub(super) enum RowType {
    Toggle,
    Cycle(&'static [&'static str]),
}

pub(super) const ROW_SHOW_DIFF: usize = 0;
pub(super) const ROW_CACHE_WARN: usize = 1;
pub(super) const ROW_STREAMING: usize = 2;
pub(super) const ROW_1M_CONTEXT: usize = 3;
pub(super) const ROW_LANGUAGE: usize = 4;
pub(super) const ROW_ACTIVE_ALIAS: usize = 5;
pub(super) const ROW_PERMISSION_MODE: usize = 6;
pub(super) const ROW_SCROLL_FPS: usize = 7;

pub(super) const STREAMING_OPTS: &[&str] = &["streaming", "block", "none"];
pub(super) const LANGUAGE_OPTS: &[&str] = &["en", "zh-CN"];
pub(super) const ALIAS_OPTS: &[&str] = &["fable", "opus", "sonnet", "haiku"];
pub(super) const PERMISSION_OPTS: &[&str] = &["default", "accept-edit", "auto-mode", "bypass"];
pub(super) const FPS_OPTS: &[&str] = &["60", "30", "20"];

pub(super) const CONFIG_ROWS: &[(&str, RowType)] = &[
    ("config-field-diff", RowType::Toggle),
    ("config-field-cache-warning", RowType::Toggle),
    ("config-field-streaming", RowType::Cycle(STREAMING_OPTS)),
    ("config-field-1m-context", RowType::Toggle),
    ("config-field-language", RowType::Cycle(LANGUAGE_OPTS)),
    ("config-field-active-alias", RowType::Cycle(ALIAS_OPTS)),
    (
        "config-field-permission-mode",
        RowType::Cycle(PERMISSION_OPTS),
    ),
    ("config-field-scroll-fps", RowType::Cycle(FPS_OPTS)),
];

// ─── 行模型：基础行 + 注册表驱动的 beta flag 区块 ────────────────────────────
//
// 基础行仍是静态表；beta flag 区块按注册表顺序（`BETA_FLAGS`）逐条渲染 Toggle 行。
// 区块标题行也是行模型的一员（不可激活），使「每项一行」的命中契约（滚动 / 鼠标点击）
// 与既有面板能力保持一致。

/// 行模型的单行。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ConfigRow {
    /// 基础行（`CONFIG_ROWS` 索引）。
    Base(usize),
    /// beta 区块标题行：承载「新会话生效」标注，不可激活。
    BetaSection,
    /// 注册表条目（索引指向 `BETA_FLAGS`）：Toggle 行。
    BetaFlag(usize),
}

/// 当前行模型（注册表为空时不渲染空区块）。
pub(super) fn config_rows() -> Vec<ConfigRow> {
    let mut rows: Vec<ConfigRow> = (0..CONFIG_ROWS.len()).map(ConfigRow::Base).collect();
    if !BETA_FLAGS.is_empty() {
        rows.push(ConfigRow::BetaSection);
        rows.extend((0..BETA_FLAGS.len()).map(ConfigRow::BetaFlag));
    }
    rows
}

/// beta flag 行的描述：优先 i18n key `beta-desc-<id>`，缺失回退注册表 canonical 文本。
///
/// `i18n::tr` 未命中时原样返回 key（见 `i18n::format_key`），据此判定缺失。
pub(super) fn beta_description(id: &str, canonical: &str) -> String {
    let key = format!("beta-desc-{id}");
    let translated = i18n::tr(&key);
    if translated == key {
        canonical.to_string()
    } else {
        translated
    }
}

// ---------------------------------------------------------------------------
// 行显示
// ---------------------------------------------------------------------------

/// 一行的渲染：选中标记 + 标签 + 值（toggle 的 `[ON] OFF` / cycle 的选项串）。
///
/// 只依赖行模型与本行是否选中；可见范围由组件的 `ListViewport` 决定（绘制窗口
/// 与鼠标命中取自同一实例）。
pub(super) fn build_row_line(
    row: ConfigRow,
    is_active: bool,
    theme: &ThemeDefinition,
) -> Line<'static> {
    let cursor_mark = if is_active { "> " } else { "  " };
    let label_style = if is_active {
        Style::new().fg(theme.component.panel.title).bold()
    } else {
        Style::new().fg(theme.semantic.text.primary)
    };

    match row {
        // beta 区块标题行：不可激活，只承载「新会话生效」标注。
        ConfigRow::BetaSection => Line::from(vec![
            Span::styled(cursor_mark, Style::new()),
            Span::styled(
                i18n::tr("config-beta-section"),
                Style::new().fg(theme.semantic.text.muted).italic(),
            ),
        ]),
        // beta flag 行：注册表 id + 当前生效层覆盖值 + 描述（i18n 优先）。
        ConfigRow::BetaFlag(flag_index) => {
            let flag = &BETA_FLAGS[flag_index];
            let val = read_beta_toggle(flag.id);
            let (on_text, off_text) = toggle_texts(val);
            let (on_style, off_style) = toggle_styles(theme, val);
            Line::from(vec![
                Span::styled(cursor_mark, Style::new().fg(theme.component.panel.title)),
                Span::styled(format!("{:<22}", flag.id), label_style),
                Span::styled(on_text, on_style),
                Span::styled(" ", Style::new()),
                Span::styled(off_text, off_style),
                Span::styled(
                    format!("  {}", beta_description(flag.id, flag.description)),
                    Style::new().fg(theme.semantic.text.dim),
                ),
            ])
        }
        ConfigRow::Base(base_row) => {
            let (label, row_type) = &CONFIG_ROWS[base_row];
            match row_type {
                RowType::Toggle => {
                    let val = read_toggle(base_row);
                    let (on_text, off_text) = toggle_texts(val);
                    let (on_style, off_style) = toggle_styles(theme, val);
                    Line::from(vec![
                        Span::styled(cursor_mark, Style::new().fg(theme.component.panel.title)),
                        Span::styled(format!("{:<22}", i18n::tr(label)), label_style),
                        Span::styled(on_text, on_style),
                        Span::styled(" ", Style::new()),
                        Span::styled(off_text, off_style),
                    ])
                }
                RowType::Cycle(options) => {
                    let idx = read_cycle_idx(base_row, options);
                    let mut spans = vec![
                        Span::styled(cursor_mark, Style::new().fg(theme.component.panel.title)),
                        Span::styled(format!("{:<22}", i18n::tr(label)), label_style),
                    ];
                    for (j, opt) in options.iter().enumerate() {
                        let label = cycle_display_label(opt);
                        if j == idx {
                            spans.push(Span::styled(
                                format!("[{}]", label),
                                Style::new().fg(theme.semantic.status.success).bold(),
                            ));
                        } else {
                            spans.push(Span::styled(
                                format!(" {}", label),
                                Style::new().fg(theme.semantic.text.muted),
                            ));
                        }
                        if j < options.len() - 1 {
                            spans.push(Span::styled(" ", Style::new()));
                        }
                    }
                    Line::from(spans)
                }
            }
        }
    }
}

/// ON / OFF 文本对（`[ON] OFF` / ` ON [OFF]`）。
fn toggle_texts(val: bool) -> (String, String) {
    let on_label = i18n::tr("config-value-on");
    let off_label = i18n::tr("config-value-off");
    if val {
        (format!("[{}]", on_label), format!(" {}", off_label))
    } else {
        (format!(" {}", on_label), format!("[{}]", off_label))
    }
}

/// ON / OFF 样式对（生效项高亮绿色，未生效项红色 / 灰）。
fn toggle_styles(theme: &ThemeDefinition, val: bool) -> (Style, Style) {
    let on_style = if val {
        Style::new().fg(theme.semantic.status.success).bold()
    } else {
        Style::new().fg(theme.semantic.text.muted)
    };
    let off_style = if val {
        Style::new().fg(theme.semantic.text.muted)
    } else {
        Style::new().fg(theme.semantic.status.error).bold()
    };
    (on_style, off_style)
}

/// 将 cycle 选项的内部值转换为 i18n 展示文本。
/// 对于 streaming/language 选项走 i18n 翻译；alias/permission 等暂保持原样。
fn cycle_display_label(opt: &str) -> String {
    match opt {
        "streaming" => i18n::tr("config-streaming-value-streaming"),
        "block" => i18n::tr("config-streaming-value-block"),
        "none" => i18n::tr("config-streaming-value-none"),
        "en" => i18n::tr("config-language-value-en"),
        "zh-CN" => i18n::tr("config-language-value-zh"),
        "60" => i18n::tr("config-fps-value-60"),
        "30" => i18n::tr("config-fps-value-30"),
        "20" => i18n::tr("config-fps-value-20"),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// 行值读取
// ---------------------------------------------------------------------------

/// beta flag 行的显示值：**合并后**的有效值（未设置与显式 false 均显示为关闭）。
///
/// 有效值由权威面按合并规则计算；`PERI_CONFIG_HANDLE` 持有的就是该合并视图，
/// 面板只按 id 读它，不自行合并来源层。
pub(super) fn read_beta_toggle(id: &str) -> bool {
    PERI_CONFIG_HANDLE
        .get()
        .map(|handle| handle.read().config.betas.get(id).unwrap_or(false))
        .unwrap_or(false)
}

/// 读取 toggle 字段当前值（true=ON / false=OFF）。
fn read_toggle(row: usize) -> bool {
    match row {
        ROW_SHOW_DIFF => TUI_CONFIG_HANDLE
            .get()
            .map(|h| h.read().diff_enabled)
            .unwrap_or(false),
        ROW_CACHE_WARN => PERI_CONFIG_HANDLE
            .get()
            .map(|h| h.read().config.show_cache_warning.unwrap_or(false))
            .unwrap_or(false),
        ROW_1M_CONTEXT => PERI_CONFIG_HANDLE
            .get()
            .map(|h| {
                let c = h.read();
                c.config
                    .profiles
                    .get(&c.config.active_alias)
                    .map(|p| p.context_1m)
                    .unwrap_or(false)
            })
            .unwrap_or(false),
        _ => false,
    }
}

/// 读取 cycle 字段当前选项索引。
pub(super) fn read_cycle_idx(row: usize, options: &[&str]) -> usize {
    match row {
        ROW_STREAMING => {
            let cur = TUI_CONFIG_HANDLE
                .get()
                .map(|h| h.read().streaming_mode.clone())
                .unwrap_or_default()
                .unwrap_or_else(|| "streaming".to_string());
            options.iter().position(|o| *o == cur.as_str()).unwrap_or(0)
        }
        ROW_LANGUAGE => {
            let cur = PERI_CONFIG_HANDLE
                .get()
                .map(|h| h.read().config.language.clone())
                .unwrap_or_default()
                .unwrap_or_else(|| "en".to_string());
            options.iter().position(|o| *o == cur.as_str()).unwrap_or(0)
        }
        ROW_ACTIVE_ALIAS => {
            let cur = PERI_CONFIG_HANDLE
                .get()
                .map(|h| h.read().config.active_alias.clone())
                .unwrap_or_default();
            // default sonnet：按名称查找索引，避免 ALIAS_OPTS 顺序变化导致硬编码漂移
            let default_idx = options.iter().position(|o| *o == "sonnet").unwrap_or(0);
            if cur.is_empty() {
                default_idx
            } else {
                options
                    .iter()
                    .position(|o| *o == cur.as_str())
                    .unwrap_or(default_idx)
            }
        }
        ROW_PERMISSION_MODE => {
            let snapshot = crate::kit::atoms::SERVICE_SNAPSHOT
                .state()
                .read()
                .permission_mode
                .clone();
            let cur = if snapshot.is_empty() {
                PERMISSION_MODE_HANDLE
                    .get()
                    .map(|mode| permission_mode_label(mode.load()))
                    .unwrap_or("default")
                    .to_string()
            } else {
                snapshot
            };
            options
                .iter()
                .position(|option| *option == cur)
                .unwrap_or(0)
        }
        ROW_SCROLL_FPS => {
            let cur = TUI_CONFIG_HANDLE
                .get()
                .map(|h| {
                    h.read()
                        .scroll_fps
                        .map(|fps| fps.to_string())
                        .unwrap_or_else(|| "20".to_string())
                })
                .unwrap_or_else(|| "20".to_string());
            options.iter().position(|o| *o == cur.as_str()).unwrap_or(0)
        }
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// 纯函数变换与标签
// ---------------------------------------------------------------------------

/// 纯函数：toggle 行的值反转。返回反转后的新值（无效 row 返回 None）。
/// 仅处理 PeriConfig 上的非 TUI 字段。
#[allow(dead_code)]
pub(super) fn apply_toggle_row(cfg: &mut PeriConfig, row: usize) -> Option<bool> {
    let new_val = match row {
        ROW_CACHE_WARN => {
            let cur = cfg.config.show_cache_warning.unwrap_or(false);
            cfg.config.show_cache_warning = Some(!cur);
            !cur
        }
        ROW_1M_CONTEXT => {
            let alias = cfg.config.active_alias.clone();
            cfg.config
                .profiles
                .get_mut(&alias)
                .map(|p| {
                    p.context_1m = !p.context_1m;
                    p.context_1m
                })
                .unwrap_or(false)
        }
        _ => return None,
    };
    Some(new_val)
}

/// 纯函数：cycle 行前进/后退并写入 PeriConfig 新值。返回新选项在 options 中的索引。
/// 仅处理 PeriConfig 上的非 TUI 字段。
#[allow(dead_code)]
pub(super) fn apply_cycle_row(cfg: &mut PeriConfig, row: usize, forward: bool) -> Option<usize> {
    let options: &[&str] = match row {
        ROW_LANGUAGE => LANGUAGE_OPTS,
        ROW_ACTIVE_ALIAS => ALIAS_OPTS,
        ROW_PERMISSION_MODE => PERMISSION_OPTS,
        _ => return None,
    };
    let cur_idx = match row {
        ROW_LANGUAGE => LANGUAGE_OPTS
            .iter()
            .position(|s| cfg.config.language.as_deref() == Some(*s))
            .unwrap_or(0),
        ROW_ACTIVE_ALIAS => ALIAS_OPTS
            .iter()
            .position(|s| cfg.config.active_alias == *s)
            .unwrap_or(0),
        ROW_PERMISSION_MODE => 0, // 不持久化
        _ => return None,
    };
    let next = if forward {
        (cur_idx + 1) % options.len()
    } else {
        (cur_idx + options.len() - 1) % options.len()
    };
    let new_val = options[next];
    match row {
        ROW_LANGUAGE => cfg.config.language = Some(new_val.to_string()),
        ROW_ACTIVE_ALIAS => cfg.config.active_alias = new_val.to_string(),
        ROW_PERMISSION_MODE => {} // 由 PERMISSION_MODE_HANDLE 处理
        _ => return None,
    }
    Some(next)
}

/// 纯函数：cycle 行前进/后退并写入 TuiConfig 新值。返回新选项在 options 中的索引。
#[allow(dead_code)]
pub(super) fn apply_cycle_row_tui(cfg: &mut TuiConfig, row: usize, forward: bool) -> Option<usize> {
    let options: &[&str] = match row {
        ROW_STREAMING => STREAMING_OPTS,
        _ => return None,
    };
    let cur_idx = match row {
        ROW_STREAMING => STREAMING_OPTS
            .iter()
            .position(|s| cfg.streaming_mode.as_deref() == Some(*s))
            .unwrap_or(0),
        _ => return None,
    };
    let next = if forward {
        (cur_idx + 1) % options.len()
    } else {
        (cur_idx + options.len() - 1) % options.len()
    };
    let new_val = options[next];
    match row {
        ROW_STREAMING => cfg.streaming_mode = Some(new_val.to_string()),
        _ => return None,
    }
    Some(next)
}

pub(super) fn parse_permission_mode(s: &str) -> Option<PermissionMode> {
    match s {
        "default" => Some(PermissionMode::Default),
        "accept-edit" => Some(PermissionMode::AcceptEdit),
        "auto-mode" => Some(PermissionMode::AutoMode),
        "bypass" => Some(PermissionMode::Bypass),
        _ => None,
    }
}

pub(super) fn permission_mode_label(m: PermissionMode) -> &'static str {
    match m {
        PermissionMode::Default => "default",
        PermissionMode::AcceptEdit => "accept-edit",
        PermissionMode::AutoMode => "auto-mode",
        PermissionMode::Bypass => "bypass",
    }
}
