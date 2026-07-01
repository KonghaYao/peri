//! v2 ModelPanel -- Model selection panel (PanelState trait implementation).
//!
//! Displays 3 model alias options (Opus / Sonnet / Haiku) plus settings rows
//! for max_tokens, thinking effort, and 1M context toggle. The user navigates
//! with arrow keys, selects a model with Enter, cycles effort/max_tokens with
//! Space/Left/Right, and closes with Esc.
//!
//! Side-effects (apply model, save config, sync to ACP) are returned as
//! `PanelEffect` instructions; the state machine translates them to actual
//! operations.

use ratatui::crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use tui_textarea::Input;

use peri_widgets::BorderedPanel;

use crate::app::panel_types::PanelKind;
use crate::panel::effect::PanelEffect;
use crate::panel::read_context::PanelReadContext;
use crate::panel::PanelState;
use crate::ui::theme;

// ---------------------------------------------------------------------------
// AliasTab
// ---------------------------------------------------------------------------

/// The three model alias tiers.
#[derive(Debug, Clone, PartialEq)]
pub enum AliasTab {
    Opus,
    Sonnet,
    Haiku,
}

impl AliasTab {
    /// Display label.
    pub fn label(&self) -> &str {
        match self {
            Self::Opus => "Opus",
            Self::Sonnet => "Sonnet",
            Self::Haiku => "Haiku",
        }
    }

    /// Config key (stored in `active_alias`).
    pub fn to_key(&self) -> &'static str {
        match self {
            Self::Opus => "opus",
            Self::Sonnet => "sonnet",
            Self::Haiku => "haiku",
        }
    }

    /// Short description for i18n key lookup.
    pub fn description(&self) -> &str {
        match self {
            Self::Opus => "Most capable for complex work",
            Self::Sonnet => "Balanced performance and speed",
            Self::Haiku => "Fastest for quick answers",
        }
    }
}

// ---------------------------------------------------------------------------
// Row index constants
// ---------------------------------------------------------------------------

/// Row: Opus selection.
const ROW_OPUS: usize = 0;
/// Row: Sonnet selection.
const ROW_SONNET: usize = 1;
/// Row: Haiku selection.
const ROW_HAIKU: usize = 2;
/// Row: Max Tokens setting.
const ROW_MAX_TOKENS: usize = 3;
/// Row: Thinking Effort setting.
const ROW_EFFORT: usize = 4;
/// Row: 1M Context toggle.
const ROW_1M_CONTEXT: usize = 5;
/// Total number of rows.
const ROW_COUNT: usize = 6;

// ---------------------------------------------------------------------------
// ModelPanel
// ---------------------------------------------------------------------------

/// v2 Model selection panel.
///
/// UI-local state only (cursor, active_tab, effort, max_tokens, context_1m).
/// Side-effects (select model, update config, sync to ACP) are returned as
/// `PanelEffect` values.
#[derive(Debug)]
pub struct ModelPanel {
    /// Currently active model alias.
    active_tab: AliasTab,
    /// Thinking effort buffer ("low" / "medium" / "high" / "xhigh" / "max").
    buf_thinking_effort: String,
    /// Max tokens value.
    buf_max_tokens: u32,
    /// 1M context switch.
    buf_context_1m: bool,
    /// Cursor row (0..ROW_COUNT-1).
    cursor: usize,
}

impl ModelPanel {
    /// Construct an empty panel for the registry factory.
    ///
    /// Defaults to Opus, high effort, 32000 max tokens, 1M context off.
    pub fn empty() -> Self {
        Self {
            active_tab: AliasTab::Opus,
            buf_thinking_effort: "high".to_string(),
            buf_max_tokens: 32000,
            buf_context_1m: false,
            cursor: ROW_OPUS,
        }
    }

    /// Construct a panel from a `PeriConfig` reference.
    pub fn from_config(cfg: &crate::config::PeriConfig) -> Self {
        let app_config = &cfg.config;
        let effort = app_config
            .thinking
            .as_ref()
            .map(|t| t.effort.as_str())
            .unwrap_or("high");
        let max_tokens = app_config
            .thinking
            .as_ref()
            .map(|t| t.max_tokens)
            .unwrap_or(32000);
        let context_1m = app_config.context_1m.unwrap_or(false);
        Self::new(&app_config.active_alias, effort, max_tokens, context_1m)
    }

    /// Construct a panel from the live `App` state.
    pub fn from_app(app: &crate::app::App) -> Self {
        Self::from_config(&app.services.peri_config.read())
    }

    /// Construct a panel from initial config values.
    ///
    /// `active_alias`: current alias key ("opus"/"sonnet"/"haiku").
    /// `effort`: thinking effort string.
    /// `max_tokens`: max tokens value.
    /// `context_1m`: whether 1M context is enabled.
    pub fn new(active_alias: &str, effort: &str, max_tokens: u32, context_1m: bool) -> Self {
        let active_tab = match active_alias {
            "sonnet" => AliasTab::Sonnet,
            "haiku" => AliasTab::Haiku,
            _ => AliasTab::Opus,
        };
        let cursor = match active_tab {
            AliasTab::Opus => ROW_OPUS,
            AliasTab::Sonnet => ROW_SONNET,
            AliasTab::Haiku => ROW_HAIKU,
        };
        Self {
            active_tab,
            buf_thinking_effort: effort.to_string(),
            buf_max_tokens: max_tokens,
            buf_context_1m: context_1m,
            cursor,
        }
    }

    /// Cursor position (0-based row index).
    fn cursor(&self) -> usize {
        self.cursor
    }

    /// Cycle thinking effort: low -> medium -> high -> xhigh -> max -> low.
    fn cycle_effort(&mut self, reverse: bool) {
        if reverse {
            self.buf_thinking_effort = match self.buf_thinking_effort.as_str() {
                "low" => "max".to_string(),
                "max" => "xhigh".to_string(),
                "xhigh" => "high".to_string(),
                "high" => "medium".to_string(),
                _ => "low".to_string(),
            };
        } else {
            self.buf_thinking_effort = match self.buf_thinking_effort.as_str() {
                "low" => "medium".to_string(),
                "medium" => "high".to_string(),
                "high" => "xhigh".to_string(),
                "xhigh" => "max".to_string(),
                _ => "low".to_string(),
            };
        }
    }

    /// Max tokens preset values.
    const MAX_TOKENS_PRESETS: &[u32] = &[8000, 16000, 32000, 64000, 128000];

    /// Cycle max_tokens through presets.
    fn cycle_max_tokens(&mut self, reverse: bool) {
        let current = self.buf_max_tokens;
        let presets = Self::MAX_TOKENS_PRESETS;
        if let Some(pos) = presets.iter().position(|&v| v == current) {
            if reverse {
                let next = if pos == 0 { presets.len() - 1 } else { pos - 1 };
                self.buf_max_tokens = presets[next];
            } else {
                self.buf_max_tokens = presets[(pos + 1) % presets.len()];
            }
        } else {
            let pos = presets
                .partition_point(|&v| v < current)
                .min(presets.len() - 1);
            if reverse {
                self.buf_max_tokens = presets[pos.saturating_sub(1)];
            } else {
                self.buf_max_tokens = presets[pos];
            }
        }
    }

    /// Build the `PanelEffect` list for applying a model alias selection.
    fn apply_effects(&self, lc: &crate::i18n::LcRegistry) -> Vec<PanelEffect> {
        let alias_label = self.active_tab.label().to_string();
        vec![
            PanelEffect::UpdateConfig {
                key: "model".to_string(),
                value: self.active_tab.to_key().to_string(),
            },
            PanelEffect::UpdateConfig {
                key: "thinking_effort".to_string(),
                value: self.buf_thinking_effort.clone(),
            },
            PanelEffect::UpdateConfig {
                key: "max_tokens".to_string(),
                value: self.buf_max_tokens.to_string(),
            },
            PanelEffect::UpdateConfig {
                key: "context_1m".to_string(),
                value: self.buf_context_1m.to_string(),
            },
            PanelEffect::SendToAcp {
                event: "set_model".to_string(),
                data: serde_json::json!({
                    "alias": self.active_tab.to_key(),
                    "effort": self.buf_thinking_effort,
                }),
            },
            PanelEffect::ShowNotification(lc.tr_args(
                "app-model-switched",
                &[
                    ("alias".into(), alias_label.into()),
                    ("effort".into(), self.buf_thinking_effort.clone().into()),
                ],
            )),
            PanelEffect::Close,
        ]
    }

    /// Build the `PanelEffect` list for toggling 1M context (without closing).
    fn toggle_1m_effects(&self) -> Vec<PanelEffect> {
        vec![
            PanelEffect::UpdateConfig {
                key: "context_1m".to_string(),
                value: self.buf_context_1m.to_string(),
            },
            PanelEffect::SendToAcp {
                event: "set_config_option".to_string(),
                data: serde_json::json!({
                    "key": "context_1m",
                    "value": self.buf_context_1m,
                }),
            },
        ]
    }
}

impl PanelState for ModelPanel {
    fn kind(&self) -> PanelKind {
        PanelKind::Model
    }

    fn render(&mut self, f: &mut Frame, area: Rect, ctx: &PanelReadContext) {
        let lc = ctx.lc;

        let inner = BorderedPanel::new(Span::styled(
            lc.tr("model-panel-title"),
            Style::default()
                .fg(theme::THINKING)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme::BORDER))
        .render(f, area);

        let mut lines: Vec<Line> = Vec::new();

        // Description
        lines.push(Line::from(Span::styled(
            lc.tr("model-panel-description"),
            Style::default().fg(theme::MUTED),
        )));
        lines.push(Line::from(""));

        // Model rows: Opus / Sonnet / Haiku
        let rows: [(usize, &AliasTab, &str, &str); 3] = [
            (ROW_OPUS, &AliasTab::Opus, "Opus", "1"),
            (ROW_SONNET, &AliasTab::Sonnet, "Sonnet", "2"),
            (ROW_HAIKU, &AliasTab::Haiku, "Haiku", "3"),
        ];

        for (row_idx, alias, label, num) in &rows {
            let is_active = self.active_tab == **alias;
            let is_cursor = self.cursor() == *row_idx;

            let check = if is_active { "\u{2714}" } else { " " };
            let cursor_char = if is_cursor { "\u{276f}" } else { " " };

            let label_style = if is_active {
                Style::default()
                    .fg(theme::SAGE)
                    .add_modifier(Modifier::BOLD)
            } else if is_cursor {
                Style::default()
                    .fg(theme::THINKING)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(theme::TEXT)
                    .add_modifier(Modifier::BOLD)
            };

            let check_style = if is_active {
                Style::default().fg(theme::SAGE)
            } else {
                Style::default().fg(theme::MUTED)
            };

            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} ", cursor_char),
                    Style::default().fg(theme::THINKING),
                ),
                Span::styled(format!("{}. ", num), label_style),
                Span::styled(format!("{:8}", label), label_style),
                Span::styled(format!(" {}  ", check), check_style),
            ]));
        }

        lines.push(Line::from(""));

        // MaxTokens row
        {
            let is_cursor = self.cursor() == ROW_MAX_TOKENS;
            let radio_color = if is_cursor {
                theme::THINKING
            } else {
                theme::ACCENT
            };
            let label_style = if is_cursor {
                Style::default()
                    .fg(theme::THINKING)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(theme::MUTED)
                    .add_modifier(Modifier::BOLD)
            };
            let cursor_char = if is_cursor { "\u{276f}" } else { " " };

            let max_tokens_text = format!(
                "{}: {}",
                lc.tr("model-field-max-token"),
                self.buf_max_tokens
            );

            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} \u{25cf} ", cursor_char),
                    Style::default().fg(radio_color),
                ),
                Span::styled(max_tokens_text, label_style),
            ]));
        }

        // Effort row
        {
            let effort_key = match self.buf_thinking_effort.as_str() {
                "low" => "model-effort-low",
                "high" => "model-effort-high",
                "xhigh" => "model-effort-xhigh",
                "max" => "model-effort-max",
                _ => "model-effort-medium",
            };

            let is_cursor = self.cursor() == ROW_EFFORT;
            let radio_color = if is_cursor {
                theme::THINKING
            } else {
                theme::ACCENT
            };
            let effort_style = if is_cursor {
                Style::default()
                    .fg(theme::THINKING)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(theme::MUTED)
                    .add_modifier(Modifier::BOLD)
            };
            let cursor_char = if is_cursor { "\u{276f}" } else { " " };

            let effort_text = format!("{}: {}", lc.tr("model-field-effort"), lc.tr(effort_key));

            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} \u{25cf} ", cursor_char),
                    Style::default().fg(radio_color),
                ),
                Span::styled(effort_text, effort_style),
            ]));
        }

        // 1M Context row
        {
            let state_label = if self.buf_context_1m { "ON" } else { "OFF" };

            let is_cursor = self.cursor() == ROW_1M_CONTEXT;
            let radio_color = if is_cursor {
                theme::THINKING
            } else {
                theme::ACCENT
            };
            let label_style = if is_cursor {
                Style::default()
                    .fg(theme::THINKING)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(theme::MUTED)
                    .add_modifier(Modifier::BOLD)
            };
            let cursor_char = if is_cursor { "\u{276f}" } else { " " };

            let state_color = if self.buf_context_1m {
                theme::SAGE
            } else {
                theme::MUTED
            };

            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} \u{25cf} ", cursor_char),
                    Style::default().fg(radio_color),
                ),
                Span::styled(lc.tr("model-field-1m-context"), label_style),
                Span::styled(
                    state_label,
                    Style::default()
                        .fg(state_color)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
        }

        lines.push(Line::from(""));

        lines.truncate(inner.height as usize);
        f.render_widget(Paragraph::new(Text::from(lines)), inner);
    }

    fn handle_key(&mut self, input: Input, ctx: &PanelReadContext) -> Vec<PanelEffect> {
        use tui_textarea::Key;
        match input {
            // Esc: close
            Input { key: Key::Esc, .. } => vec![PanelEffect::Close],
            // Up: navigate up
            Input { key: Key::Up, .. } => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                }
                vec![]
            }
            // Down: navigate down
            Input { key: Key::Down, .. } => {
                if self.cursor < ROW_COUNT - 1 {
                    self.cursor += 1;
                }
                vec![]
            }
            // Enter: action depends on current row
            Input {
                key: Key::Enter, ..
            } => match self.cursor() {
                ROW_OPUS => {
                    self.active_tab = AliasTab::Opus;
                    self.apply_effects(ctx.lc)
                }
                ROW_SONNET => {
                    self.active_tab = AliasTab::Sonnet;
                    self.apply_effects(ctx.lc)
                }
                ROW_HAIKU => {
                    self.active_tab = AliasTab::Haiku;
                    self.apply_effects(ctx.lc)
                }
                ROW_EFFORT => {
                    self.cycle_effort(false);
                    vec![]
                }
                ROW_MAX_TOKENS => {
                    self.cycle_max_tokens(false);
                    vec![]
                }
                ROW_1M_CONTEXT => {
                    self.buf_context_1m = !self.buf_context_1m;
                    self.toggle_1m_effects()
                }
                _ => vec![],
            },
            // Space: cycle effort / max_tokens / toggle 1M depending on row
            Input {
                key: Key::Char(' '),
                ..
            } => {
                if self.cursor() == ROW_MAX_TOKENS {
                    self.cycle_max_tokens(false);
                } else if self.cursor() == ROW_1M_CONTEXT {
                    self.buf_context_1m = !self.buf_context_1m;
                    return self.toggle_1m_effects();
                } else {
                    self.cycle_effort(false);
                }
                vec![]
            }
            // Left: reverse cycle
            Input { key: Key::Left, .. } => {
                if self.cursor() == ROW_MAX_TOKENS {
                    self.cycle_max_tokens(true);
                } else if self.cursor() == ROW_1M_CONTEXT {
                    self.buf_context_1m = !self.buf_context_1m;
                    return self.toggle_1m_effects();
                } else {
                    self.cycle_effort(true);
                }
                vec![]
            }
            // Right: forward cycle
            Input {
                key: Key::Right, ..
            } => {
                if self.cursor() == ROW_MAX_TOKENS {
                    self.cycle_max_tokens(false);
                } else if self.cursor() == ROW_1M_CONTEXT {
                    self.buf_context_1m = !self.buf_context_1m;
                    return self.toggle_1m_effects();
                } else {
                    self.cycle_effort(false);
                }
                vec![]
            }
            // All other keys: consumed (no-op)
            _ => vec![],
        }
    }

    fn handle_mouse(
        &mut self,
        mouse: MouseEvent,
        area: Rect,
        ctx: &PanelReadContext,
    ) -> Vec<PanelEffect> {
        if mouse.kind == MouseEventKind::Down(ratatui::crossterm::event::MouseButton::Left) {
            // border_top=1
            let relative_y = mouse.row.saturating_sub(area.y);
            if relative_y >= 1 {
                let clicked = (relative_y - 1) as usize;
                if clicked < ROW_COUNT {
                    self.cursor = clicked;
                    return self.handle_key(
                        Input::from(KeyEvent::new(
                            KeyCode::Enter,
                            ratatui::crossterm::event::KeyModifiers::NONE,
                        )),
                        ctx,
                    );
                }
            }
        }
        vec![]
    }

    fn desired_height(&self, _screen_h: u16, _screen_w: u16) -> u16 {
        13
    }

    fn status_bar_hints(&self, lc: &crate::i18n::LcRegistry) -> Vec<(String, String)> {
        vec![
            (
                "\u{2191}\u{2193}".to_string(),
                lc.tr("key-move").to_string(),
            ),
            ("Enter".to_string(), lc.tr("key-confirm").to_string()),
            (
                "\u{2190}\u{2192}/Space".to_string(),
                lc.tr("key-effort").to_string(),
            ),
            ("Esc".to_string(), lc.tr("key-close").to_string()),
        ]
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use tui_textarea::Key;

    use super::*;
    use crate::panel::read_context::{PanelReadContext, ServiceRegistrySnapshot};
    use crate::panel::PanelState;

    /// Helper: build a minimal `PanelReadContext` for testing.
    fn make_ctx() -> PanelReadContext<'static> {
        thread_local! {
            static SNAPSHOT: ServiceRegistrySnapshot = ServiceRegistrySnapshot::new();
            static VMS: Vec<peri_acp_types::view_model::ViewModel> = const { Vec::new() };
            #[allow(clippy::missing_const_for_thread_local)]
            static CACHE: HashMap<String, serde_json::Value> = HashMap::new();
            static LC: crate::i18n::LcRegistry = crate::i18n::LcRegistry::default();
        }
        SNAPSHOT.with(|snapshot| {
            let snapshot: &'static ServiceRegistrySnapshot = unsafe { &*(snapshot as *const _) };
            VMS.with(|vms| {
                let vms: &'static Vec<peri_acp_types::view_model::ViewModel> =
                    unsafe { &*(vms as *const _) };
                CACHE.with(|cache| {
                    let cache: &'static HashMap<String, serde_json::Value> =
                        unsafe { &*(cache as *const _) };
                    LC.with(|lc| {
                        let lc: &'static crate::i18n::LcRegistry = unsafe { &*(lc as *const _) };
                        PanelReadContext {
                            services: snapshot.clone(),
                            view_models: vms,
                            scroll_offset: 0,
                            area: Rect::new(0, 0, 80, 24),
                            lc,
                            acp_query_cache: cache,
                        }
                    })
                })
            })
        })
    }

    fn esc_input() -> Input {
        Input {
            key: Key::Esc,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    fn up_input() -> Input {
        Input {
            key: Key::Up,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    fn down_input() -> Input {
        Input {
            key: Key::Down,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    fn enter_input() -> Input {
        Input {
            key: Key::Enter,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    fn space_input() -> Input {
        Input {
            key: Key::Char(' '),
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    #[test]
    fn test_kind_returns_model() {
        let panel = ModelPanel::empty();
        assert_eq!(panel.kind(), PanelKind::Model);
    }

    #[test]
    fn test_esc_close() {
        let mut panel = ModelPanel::empty();
        let ctx = make_ctx();
        let effects = panel.handle_key(esc_input(), &ctx);
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0], PanelEffect::Close);
    }

    #[test]
    fn test_up_down_navigation() {
        let mut panel = ModelPanel::empty();
        let ctx = make_ctx();

        // Starts at ROW_OPUS (0)
        assert_eq!(panel.cursor(), ROW_OPUS);

        // Down -> ROW_SONNET (1)
        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.cursor(), ROW_SONNET);

        // Down -> ROW_HAIKU (2)
        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.cursor(), ROW_HAIKU);

        // Down -> ROW_MAX_TOKENS (3)
        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.cursor(), ROW_MAX_TOKENS);

        // Down -> ROW_EFFORT (4)
        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.cursor(), ROW_EFFORT);

        // Down -> ROW_1M_CONTEXT (5)
        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.cursor(), ROW_1M_CONTEXT);

        // Down -> clamped at 5
        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.cursor(), ROW_1M_CONTEXT);

        // Up -> ROW_EFFORT (4)
        panel.handle_key(up_input(), &ctx);
        assert_eq!(panel.cursor(), ROW_EFFORT);

        // Up -> ROW_MAX_TOKENS (3)
        panel.handle_key(up_input(), &ctx);
        assert_eq!(panel.cursor(), ROW_MAX_TOKENS);

        // Up -> ROW_HAIKU (2)
        panel.handle_key(up_input(), &ctx);
        assert_eq!(panel.cursor(), ROW_HAIKU);

        // Up -> ROW_SONNET (1)
        panel.handle_key(up_input(), &ctx);
        assert_eq!(panel.cursor(), ROW_SONNET);

        // Up -> ROW_OPUS (0)
        panel.handle_key(up_input(), &ctx);
        assert_eq!(panel.cursor(), ROW_OPUS);

        // Up -> clamped at 0
        panel.handle_key(up_input(), &ctx);
        assert_eq!(panel.cursor(), ROW_OPUS);
    }

    #[test]
    fn test_enter_on_opus_applies_and_closes() {
        let mut panel = ModelPanel::empty();
        // Move to Sonnet first
        panel.active_tab = AliasTab::Sonnet;
        panel.cursor = ROW_SONNET;
        let ctx = make_ctx();

        let effects = panel.handle_key(enter_input(), &ctx);
        // Must contain Close
        assert!(effects.contains(&PanelEffect::Close));
        // Must contain UpdateConfig with model = "sonnet"
        let has_model_update = effects.iter().any(|e| {
            matches!(
                e,
                PanelEffect::UpdateConfig { key, value } if key == "model" && value == "sonnet"
            )
        });
        assert!(
            has_model_update,
            "Enter on Sonnet should emit UpdateConfig model=sonnet"
        );
        // active_tab should be Sonnet
        assert_eq!(panel.active_tab, AliasTab::Sonnet);
    }

    #[test]
    fn test_cycle_effort_with_space() {
        let mut panel = ModelPanel::empty();
        let ctx = make_ctx();
        // Default effort is "high"
        assert_eq!(panel.buf_thinking_effort, "high");

        panel.handle_key(space_input(), &ctx);
        assert_eq!(panel.buf_thinking_effort, "xhigh");

        panel.handle_key(space_input(), &ctx);
        assert_eq!(panel.buf_thinking_effort, "max");

        panel.handle_key(space_input(), &ctx);
        assert_eq!(panel.buf_thinking_effort, "low");

        panel.handle_key(space_input(), &ctx);
        assert_eq!(panel.buf_thinking_effort, "medium");
    }

    #[test]
    fn test_cycle_max_tokens_with_space() {
        let mut panel = ModelPanel::empty();
        let ctx = make_ctx();
        panel.cursor = ROW_MAX_TOKENS;
        assert_eq!(panel.buf_max_tokens, 32000);

        panel.handle_key(space_input(), &ctx);
        assert_eq!(panel.buf_max_tokens, 64000);

        panel.handle_key(space_input(), &ctx);
        assert_eq!(panel.buf_max_tokens, 128000);

        panel.handle_key(space_input(), &ctx);
        assert_eq!(panel.buf_max_tokens, 8000);
    }

    #[test]
    fn test_toggle_1m_context_with_space() {
        let mut panel = ModelPanel::empty();
        let ctx = make_ctx();
        panel.cursor = ROW_1M_CONTEXT;
        assert!(!panel.buf_context_1m);

        let effects = panel.handle_key(space_input(), &ctx);
        assert!(panel.buf_context_1m);
        // Should contain UpdateConfig for context_1m
        let has_1m_update = effects.iter().any(|e| {
            matches!(
                e,
                PanelEffect::UpdateConfig { key, value } if key == "context_1m" && value == "true"
            )
        });
        assert!(
            has_1m_update,
            "Space on 1M row should emit UpdateConfig context_1m"
        );
    }

    #[test]
    fn test_new_from_config_values() {
        let panel = ModelPanel::new("sonnet", "medium", 16000, true);
        assert_eq!(panel.active_tab, AliasTab::Sonnet);
        assert_eq!(panel.cursor(), ROW_SONNET);
        assert_eq!(panel.buf_thinking_effort, "medium");
        assert_eq!(panel.buf_max_tokens, 16000);
        assert!(panel.buf_context_1m);
    }

    #[test]
    fn test_desired_height() {
        let panel = ModelPanel::empty();
        assert_eq!(panel.desired_height(50, 80), 13);
    }

    #[test]
    fn test_render_does_not_panic() {
        let mut panel = ModelPanel::empty();
        let ctx = make_ctx();

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| panel.render(f, Rect::new(0, 0, 80, 20), &ctx))
            .unwrap();
    }

    #[test]
    fn test_render_with_sonnet_does_not_panic() {
        let mut panel = ModelPanel::new("sonnet", "low", 8000, true);
        panel.cursor = ROW_EFFORT;
        let ctx = make_ctx();

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| panel.render(f, Rect::new(0, 0, 80, 20), &ctx))
            .unwrap();
    }

    #[test]
    fn test_status_bar_hints() {
        let panel = ModelPanel::empty();
        let lc = crate::i18n::LcRegistry::default();
        let hints = panel.status_bar_hints(&lc);
        assert_eq!(hints.len(), 4);
    }

    #[test]
    fn test_alias_tab_to_key() {
        assert_eq!(AliasTab::Opus.to_key(), "opus");
        assert_eq!(AliasTab::Sonnet.to_key(), "sonnet");
        assert_eq!(AliasTab::Haiku.to_key(), "haiku");
    }

    #[test]
    fn test_alias_tab_description() {
        assert_eq!(
            AliasTab::Opus.description(),
            "Most capable for complex work"
        );
        assert_eq!(
            AliasTab::Sonnet.description(),
            "Balanced performance and speed"
        );
        assert_eq!(AliasTab::Haiku.description(), "Fastest for quick answers");
    }

    #[test]
    fn test_left_cycles_effort_reverse() {
        let mut panel = ModelPanel::empty();
        let ctx = make_ctx();
        assert_eq!(panel.buf_thinking_effort, "high");

        panel.handle_key(
            Input {
                key: Key::Left,
                ctrl: false,
                alt: false,
                shift: false,
            },
            &ctx,
        );
        assert_eq!(panel.buf_thinking_effort, "medium");

        panel.handle_key(
            Input {
                key: Key::Left,
                ctrl: false,
                alt: false,
                shift: false,
            },
            &ctx,
        );
        assert_eq!(panel.buf_thinking_effort, "low");
    }

    #[test]
    fn test_right_cycles_effort_forward() {
        let mut panel = ModelPanel::empty();
        let ctx = make_ctx();
        assert_eq!(panel.buf_thinking_effort, "high");

        panel.handle_key(
            Input {
                key: Key::Right,
                ctrl: false,
                alt: false,
                shift: false,
            },
            &ctx,
        );
        assert_eq!(panel.buf_thinking_effort, "xhigh");

        panel.handle_key(
            Input {
                key: Key::Right,
                ctrl: false,
                alt: false,
                shift: false,
            },
            &ctx,
        );
        assert_eq!(panel.buf_thinking_effort, "max");
    }
}
