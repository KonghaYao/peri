//! v2 ConfigPanel -- Configuration panel (PanelState trait implementation).
//!
//! Displays and edits peri config fields organized in two groups:
//!   - General: Autocompact, Cache Warning, Compact Threshold, Language,
//!     Inline Diff, Streaming Mode, Proactiveness
//!   - Prompt Overrides: Persona, Tone
//!
//! Toggle fields (Autocompact, Cache Warning, Diff) cycle with Space/Left/Right.
//! Cycle fields (Language, Streaming, Proactiveness) cycle through options with
//! Space/Left/Right. Text fields (Threshold, Persona, Tone) accept keyboard
//! input when focused. Navigation with Up/Down; close with Esc.
//!
//! All changes produce `PanelEffect::UpdateConfig` + `PanelEffect::SendToAcp`
//! instructions; the state machine translates them to actual operations.
//!
//! Text fields use a lightweight `TextField` (String + cursor) instead of
//! `tui_textarea::TextArea` to satisfy the `Send` bound required by `PanelState`.

use ratatui::Frame;
use ratatui::crossterm::event::{MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use tui_textarea::Input;

use peri_widgets::BorderedPanel;

use crate::app::panel_types::PanelKind;
use crate::panel::PanelState;
use crate::panel::effect::PanelEffect;
use crate::panel::read_context::PanelReadContext;
use crate::ui::theme;
use unicode_width::UnicodeWidthStr;

// ---------------------------------------------------------------------------
// Lightweight text field (Send-safe, replaces FieldTextarea)
// ---------------------------------------------------------------------------

/// Minimal single-line text editor state (String + byte cursor).
///
/// Satisfies `Send` (unlike `tui_textarea::TextArea` which is not thread-safe
/// due to `ratatui_widgets::block::shadow::Effect`).
#[derive(Debug, Clone)]
struct TextField {
    text: String,
    cursor: usize, // byte offset into `text`
}

impl TextField {
    fn new(value: &str) -> Self {
        Self {
            text: value.to_string(),
            cursor: value.len(),
        }
    }

    fn value(&self) -> String {
        self.text.clone()
    }

    fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    fn insert_char(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    fn delete_backward(&mut self) {
        if self.cursor > 0 {
            let prev = self.text[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, c)| (i, c.len_utf8()));
            if let Some((byte_idx, char_len)) = prev {
                self.text.remove(byte_idx);
                self.cursor -= char_len;
            }
        }
    }

    fn delete_forward(&mut self) {
        if self.cursor < self.text.len() {
            let next = self.text[self.cursor..]
                .char_indices()
                .nth(0)
                .map(|(_, c)| c.len_utf8());
            if let Some(char_len) = next {
                self.text.drain(self.cursor..self.cursor + char_len);
            }
        }
    }

    fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.text[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
    }

    fn move_right(&mut self) {
        if self.cursor < self.text.len() {
            self.cursor = self.text[self.cursor..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| self.cursor + i)
                .unwrap_or(self.text.len());
        }
    }

    fn move_home(&mut self) {
        self.cursor = 0;
    }

    fn move_end(&mut self) {
        self.cursor = self.text.len();
    }

    fn insert_text(&mut self, s: &str) {
        for c in s.chars() {
            self.insert_char(c);
        }
    }

    fn handle_input(&mut self, input: Input) {
        use tui_textarea::Key;
        match input {
            Input {
                key: Key::Char(c), ..
            } => self.insert_char(c),
            Input {
                key: Key::Backspace,
                ..
            } => self.delete_backward(),
            Input {
                key: Key::Delete, ..
            } => self.delete_forward(),
            Input { key: Key::Left, .. } => self.move_left(),
            Input {
                key: Key::Right, ..
            } => self.move_right(),
            Input { key: Key::Home, .. } => self.move_home(),
            Input { key: Key::End, .. } => self.move_end(),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Row index constants
// ---------------------------------------------------------------------------

const ROW_GENERAL_HEADER: usize = 0;
const ROW_AUTOCOMPACT: usize = 1;
const ROW_CACHE_WARNING: usize = 2;
const ROW_THRESHOLD: usize = 3;
const ROW_LANGUAGE: usize = 4;
const ROW_DIFF: usize = 5;
const ROW_STREAMING: usize = 6;
const ROW_PROACTIVENESS: usize = 7;
const ROW_SEPARATOR: usize = 8;
const ROW_OVERRIDES_HEADER: usize = 9;
const ROW_PERSONA: usize = 10;
const ROW_TONE: usize = 11;
const ROW_COUNT: usize = 12;

/// Editable rows (all except headers/separator).
const EDITABLE_ROWS: &[usize] = &[
    ROW_AUTOCOMPACT,
    ROW_CACHE_WARNING,
    ROW_THRESHOLD,
    ROW_LANGUAGE,
    ROW_DIFF,
    ROW_STREAMING,
    ROW_PROACTIVENESS,
    ROW_PERSONA,
    ROW_TONE,
];

/// Text-input rows (use TextField).
const TEXT_ROWS: &[usize] = &[ROW_THRESHOLD, ROW_PERSONA, ROW_TONE];

/// Screen layout: each editable row occupies 2 screen lines (value + description),
/// non-editable rows occupy 1.
const SCREEN_LAYOUT: &[usize] = &[
    ROW_GENERAL_HEADER,   // screen 0
    ROW_AUTOCOMPACT,      // screen 1: value
    ROW_AUTOCOMPACT,      // screen 2: desc
    ROW_CACHE_WARNING,    // screen 3: value
    ROW_CACHE_WARNING,    // screen 4: desc
    ROW_THRESHOLD,        // screen 5: value
    ROW_THRESHOLD,        // screen 6: desc
    ROW_LANGUAGE,         // screen 7: value
    ROW_LANGUAGE,         // screen 8: desc
    ROW_DIFF,             // screen 9: value
    ROW_DIFF,             // screen 10: desc
    ROW_STREAMING,        // screen 11: value
    ROW_STREAMING,        // screen 12: desc
    ROW_PROACTIVENESS,    // screen 13: value
    ROW_PROACTIVENESS,    // screen 14: desc
    ROW_SEPARATOR,        // screen 15
    ROW_OVERRIDES_HEADER, // screen 16
    ROW_PERSONA,          // screen 17: value
    ROW_PERSONA,          // screen 18: desc
    ROW_TONE,             // screen 19: value
    ROW_TONE,             // screen 20: desc
];

fn is_text_row(row: usize) -> bool {
    TEXT_ROWS.contains(&row)
}

fn next_editable_row(current: usize, reverse: bool) -> usize {
    if reverse {
        EDITABLE_ROWS
            .iter()
            .rev()
            .find(|&&r| r < current)
            .copied()
            .unwrap_or(EDITABLE_ROWS[EDITABLE_ROWS.len() - 1])
    } else {
        EDITABLE_ROWS
            .iter()
            .find(|&&r| r > current)
            .copied()
            .unwrap_or(EDITABLE_ROWS[0])
    }
}

fn screen_to_logical_row(screen_line: usize) -> Option<usize> {
    SCREEN_LAYOUT.get(screen_line).copied()
}

/// i18n key for a given row's label.
fn field_label_key(row: usize) -> &'static str {
    match row {
        ROW_AUTOCOMPACT => "config-field-autocompact",
        ROW_CACHE_WARNING => "config-field-cache-warning",
        ROW_THRESHOLD => "config-field-compact-threshold",
        ROW_LANGUAGE => "config-field-language",
        ROW_DIFF => "config-field-diff",
        ROW_STREAMING => "config-field-streaming",
        ROW_PERSONA => "config-field-persona",
        ROW_TONE => "config-field-tone",
        ROW_PROACTIVENESS => "config-field-proactiveness",
        _ => "???",
    }
}

fn lang_display(code: &str) -> &str {
    match code {
        "en" => "English",
        "zh-CN" => "\u{7B80}\u{4F53}\u{4E2D}\u{6587}",
        _ => "auto",
    }
}

// ---------------------------------------------------------------------------
// ConfigPanel
// ---------------------------------------------------------------------------

/// v2 Configuration panel.
///
/// Holds all form state locally (boolean toggles, cycle buffers, text fields).
/// All side-effects are returned as `PanelEffect` instructions.
#[derive(Debug)]
pub struct ConfigPanel {
    /// Current cursor row (one of `EDITABLE_ROWS`).
    cursor: usize,
    // Boolean toggle fields
    buf_autocompact: bool,
    buf_show_cache_warning: bool,
    buf_diff: bool,
    // Cycle fields
    buf_language: String,
    buf_streaming: String,
    buf_proactiveness: String,
    // Text fields (Send-safe lightweight editors)
    field_threshold: TextField,
    field_persona: TextField,
    field_tone: TextField,
}

impl ConfigPanel {
    /// Construct an empty panel for the registry factory.
    ///
    /// All fields use sensible defaults.
    pub fn empty() -> Self {
        Self {
            cursor: ROW_AUTOCOMPACT,
            buf_autocompact: true,
            buf_show_cache_warning: true,
            buf_diff: true,
            buf_language: String::new(),
            buf_streaming: "streaming".to_string(),
            buf_proactiveness: "medium".to_string(),
            field_threshold: TextField::new("85"),
            field_persona: TextField::new(""),
            field_tone: TextField::new(""),
        }
    }

    /// Construct a panel from a `PeriConfig` reference.
    pub fn from_config(cfg: &crate::config::PeriConfig) -> Self {
        let app_config = &cfg.config;
        let autocompact = app_config
            .compact
            .as_ref()
            .map(|c| c.auto_compact_enabled)
            .unwrap_or(true);
        let show_cache_warning = app_config.show_cache_warning;
        let threshold = app_config
            .compact
            .as_ref()
            .map(|c| format!("{:.0}", c.auto_compact_threshold * 100.0))
            .unwrap_or_else(|| "85".to_string());
        let language = app_config.language.as_deref().unwrap_or("");
        let diff_enabled = app_config.diff_enabled;
        let streaming_mode = app_config.streaming_mode.as_deref().unwrap_or("streaming");
        let proactiveness = app_config.proactiveness.as_deref().unwrap_or("medium");
        let persona = app_config.persona.as_deref().unwrap_or("");
        let tone = app_config.tone.as_deref().unwrap_or("");
        Self::new(
            autocompact,
            show_cache_warning,
            &threshold,
            language,
            diff_enabled,
            streaming_mode,
            proactiveness,
            persona,
            tone,
        )
    }

    /// Construct a panel from the live `App` state.
    pub fn from_app(app: &crate::app::App) -> Self {
        Self::from_config(&app.services.peri_config.read())
    }

    /// Construct a panel from initial config values.
    ///
    /// This is the primary constructor used when opening the panel with
    /// current configuration loaded.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        autocompact: bool,
        show_cache_warning: bool,
        threshold: &str,
        language: &str,
        diff_enabled: bool,
        streaming_mode: &str,
        proactiveness: &str,
        persona: &str,
        tone: &str,
    ) -> Self {
        Self {
            cursor: ROW_AUTOCOMPACT,
            buf_autocompact: autocompact,
            buf_show_cache_warning: show_cache_warning,
            buf_diff: diff_enabled,
            buf_language: language.to_string(),
            buf_streaming: streaming_mode.to_string(),
            buf_proactiveness: proactiveness.to_string(),
            field_threshold: TextField::new(threshold),
            field_persona: TextField::new(persona),
            field_tone: TextField::new(tone),
        }
    }

    fn cursor_down(&mut self) {
        self.cursor = next_editable_row(self.cursor, false);
    }

    fn cursor_up(&mut self) {
        self.cursor = next_editable_row(self.cursor, true);
    }

    fn active_field(&mut self) -> Option<&mut TextField> {
        match self.cursor {
            ROW_THRESHOLD => Some(&mut self.field_threshold),
            ROW_PERSONA => Some(&mut self.field_persona),
            ROW_TONE => Some(&mut self.field_tone),
            _ => None,
        }
    }

    // -- Toggle / cycle helpers --

    fn cycle_autocompact(&mut self) {
        self.buf_autocompact = !self.buf_autocompact;
    }

    fn cycle_cache_warning(&mut self) {
        self.buf_show_cache_warning = !self.buf_show_cache_warning;
    }

    fn cycle_diff(&mut self) {
        self.buf_diff = !self.buf_diff;
    }

    fn cycle_proactiveness(&mut self, reverse: bool) {
        self.buf_proactiveness = match self.buf_proactiveness.as_str() {
            "low" => "medium".to_string(),
            "medium" => "high".to_string(),
            "high" => "low".to_string(),
            _ => "medium".to_string(),
        };
        if reverse {
            // Reverse cycle: undo one step forward by going around
            self.cycle_proactiveness(false);
            self.cycle_proactiveness(false);
        }
    }

    fn cycle_streaming(&mut self, reverse: bool) {
        self.buf_streaming = if reverse {
            match self.buf_streaming.as_str() {
                "none" => "block".to_string(),
                "block" => "streaming".to_string(),
                _ => "none".to_string(),
            }
        } else {
            match self.buf_streaming.as_str() {
                "streaming" => "block".to_string(),
                "block" => "none".to_string(),
                _ => "streaming".to_string(),
            }
        };
    }

    const LANGUAGE_OPTIONS: &[&str] = &["en", "zh-CN"];

    fn cycle_language(&mut self, reverse: bool) {
        let current = self.buf_language.as_str();
        let next = match Self::LANGUAGE_OPTIONS.iter().position(|&o| o == current) {
            Some(p) => {
                if reverse {
                    if p == 0 {
                        Self::LANGUAGE_OPTIONS.len() - 1
                    } else {
                        p - 1
                    }
                } else {
                    (p + 1) % Self::LANGUAGE_OPTIONS.len()
                }
            }
            None => {
                if reverse {
                    Self::LANGUAGE_OPTIONS.len() - 1
                } else {
                    0
                }
            }
        };
        self.buf_language = Self::LANGUAGE_OPTIONS[next].to_string();
    }

    // -- Effect builders --

    /// Build `PanelEffect` list for the current state of all fields.
    fn save_effects(&self) -> Vec<PanelEffect> {
        let mut effects = Vec::new();
        // Autocompact
        effects.push(PanelEffect::UpdateConfig {
            key: "auto_compact_enabled".to_string(),
            value: self.buf_autocompact.to_string(),
        });
        // Cache warning
        effects.push(PanelEffect::UpdateConfig {
            key: "show_cache_warning".to_string(),
            value: self.buf_show_cache_warning.to_string(),
        });
        // Threshold
        effects.push(PanelEffect::UpdateConfig {
            key: "auto_compact_threshold".to_string(),
            value: self.field_threshold.value(),
        });
        // Language
        effects.push(PanelEffect::UpdateConfig {
            key: "language".to_string(),
            value: self.buf_language.clone(),
        });
        // Diff
        effects.push(PanelEffect::UpdateConfig {
            key: "diff_enabled".to_string(),
            value: self.buf_diff.to_string(),
        });
        // Streaming
        effects.push(PanelEffect::UpdateConfig {
            key: "streaming_mode".to_string(),
            value: self.buf_streaming.clone(),
        });
        // Proactiveness
        effects.push(PanelEffect::UpdateConfig {
            key: "proactiveness".to_string(),
            value: self.buf_proactiveness.clone(),
        });
        // Persona
        effects.push(PanelEffect::UpdateConfig {
            key: "persona".to_string(),
            value: self.field_persona.value(),
        });
        // Tone
        effects.push(PanelEffect::UpdateConfig {
            key: "tone".to_string(),
            value: self.field_tone.value(),
        });
        // Notify ACP
        effects.push(PanelEffect::SendToAcp {
            event: "config/update".to_string(),
            data: serde_json::json!({
                "auto_compact_enabled": self.buf_autocompact,
                "show_cache_warning": self.buf_show_cache_warning,
                "auto_compact_threshold": self.field_threshold.value(),
                "language": self.buf_language,
                "diff_enabled": self.buf_diff,
                "streaming_mode": self.buf_streaming,
                "proactiveness": self.buf_proactiveness,
                "persona": self.field_persona.value(),
                "tone": self.field_tone.value(),
            }),
        });
        effects
    }

    fn handle_text_key(&mut self, input: Input) {
        if let Some(field) = self.active_field() {
            field.handle_input(input);
        }
    }
}

impl PanelState for ConfigPanel {
    fn kind(&self) -> PanelKind {
        PanelKind::Config
    }

    fn render(&mut self, f: &mut Frame, area: Rect, ctx: &PanelReadContext) {
        let lc = ctx.lc;

        let inner = BorderedPanel::new(Span::styled(
            lc.tr("config-panel-title"),
            Style::default()
                .fg(theme::THINKING)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme::BORDER))
        .render(f, area);

        // Dynamic label column width (CJK-safe).
        let label_display_widths: Vec<usize> = (0..ROW_COUNT)
            .filter_map(|row| {
                let key = field_label_key(row);
                if key == "???" {
                    None
                } else {
                    Some(UnicodeWidthStr::width(lc.tr(key).as_str()))
                }
            })
            .collect();
        let label_column_width = label_display_widths.iter().max().copied().unwrap_or(14);

        let mut lines: Vec<Line> = Vec::new();

        let active_style = Style::default()
            .fg(theme::THINKING)
            .add_modifier(Modifier::BOLD);
        let inactive_style = Style::default().fg(theme::MUTED);
        let desc_style = Style::default().fg(theme::MUTED);
        let value_style = Style::default().fg(theme::TEXT);

        for row in 0..ROW_COUNT {
            match row {
                ROW_GENERAL_HEADER => {
                    lines.push(Line::from(vec![Span::styled(
                        lc.tr("config-group-general"),
                        Style::default()
                            .fg(theme::SAGE)
                            .add_modifier(Modifier::BOLD),
                    )]));
                }
                ROW_SEPARATOR => {
                    lines.push(Line::from(""));
                }
                ROW_OVERRIDES_HEADER => {
                    lines.push(Line::from(vec![Span::styled(
                        lc.tr("config-group-prompt-overrides"),
                        Style::default()
                            .fg(theme::SAGE)
                            .add_modifier(Modifier::BOLD),
                    )]));
                }
                // Boolean toggle rows
                ROW_AUTOCOMPACT | ROW_CACHE_WARNING | ROW_DIFF => {
                    let is_active = self.cursor == row;
                    let label_style = if is_active {
                        active_style
                    } else {
                        Style::default().fg(theme::TEXT)
                    };

                    let val = match row {
                        ROW_AUTOCOMPACT => self.buf_autocompact,
                        ROW_CACHE_WARNING => self.buf_show_cache_warning,
                        ROW_DIFF => self.buf_diff,
                        _ => unreachable!(),
                    };
                    let desc_key = match row {
                        ROW_AUTOCOMPACT => "config-desc-autocompact",
                        ROW_CACHE_WARNING => "config-desc-cache-warning",
                        ROW_DIFF => "config-desc-diff",
                        _ => "",
                    };

                    let on_span = if val {
                        Span::styled(format!("[{}]", lc.tr("config-value-on")), active_style)
                    } else {
                        Span::styled(lc.tr("config-value-on"), inactive_style)
                    };
                    let off_span = if val {
                        Span::styled(lc.tr("config-value-off"), inactive_style)
                    } else {
                        Span::styled(format!("[{}]", lc.tr("config-value-off")), active_style)
                    };

                    lines.push(Line::from(vec![
                        Span::styled("  ", Style::default()),
                        Span::styled(
                            format!(
                                "{:<width$}",
                                lc.tr(field_label_key(row)),
                                width = label_column_width
                            ),
                            label_style,
                        ),
                        on_span,
                        Span::styled("  ", Style::default()),
                        off_span,
                    ]));
                    lines.push(Line::from(Span::styled(
                        format!("      {}", lc.tr(desc_key)),
                        desc_style,
                    )));
                }
                // Language cycle row
                ROW_LANGUAGE => {
                    let is_active = self.cursor == row;
                    let label_style = if is_active {
                        active_style
                    } else {
                        Style::default().fg(theme::TEXT)
                    };

                    let options = Self::LANGUAGE_OPTIONS;
                    let mut value_spans: Vec<Span> = Vec::new();
                    for (i, code) in options.iter().enumerate() {
                        let display = lang_display(code);
                        let is_selected = *code == self.buf_language.as_str();
                        if is_selected {
                            value_spans.push(Span::styled(format!("[{}]", display), active_style));
                        } else {
                            value_spans.push(Span::styled(display.to_string(), inactive_style));
                        }
                        if i < options.len() - 1 {
                            value_spans.push(Span::styled("  ", Style::default()));
                        }
                    }
                    let mut line_spans = vec![
                        Span::styled("  ", Style::default()),
                        Span::styled(
                            format!(
                                "{:<width$}",
                                lc.tr(field_label_key(row)),
                                width = label_column_width
                            ),
                            label_style,
                        ),
                    ];
                    line_spans.extend(value_spans);
                    lines.push(Line::from(line_spans));
                    lines.push(Line::from(Span::styled(
                        format!("      {}", lc.tr("config-desc-language")),
                        desc_style,
                    )));
                }
                // Streaming cycle row
                ROW_STREAMING => {
                    let is_active = self.cursor == row;
                    let label_style = if is_active {
                        active_style
                    } else {
                        Style::default().fg(theme::TEXT)
                    };

                    let vals = ["streaming", "block", "none"];
                    let mut value_spans: Vec<Span> = Vec::new();
                    for (i, v) in vals.iter().enumerate() {
                        if *v == self.buf_streaming.as_str() {
                            value_spans.push(Span::styled(format!("[{}]", v), active_style));
                        } else {
                            value_spans.push(Span::styled(v.to_string(), inactive_style));
                        }
                        if i < vals.len() - 1 {
                            value_spans.push(Span::styled("  ", Style::default()));
                        }
                    }
                    let mut line_spans = vec![
                        Span::styled("  ", Style::default()),
                        Span::styled(
                            format!(
                                "{:<width$}",
                                lc.tr(field_label_key(row)),
                                width = label_column_width
                            ),
                            label_style,
                        ),
                    ];
                    line_spans.extend(value_spans);
                    lines.push(Line::from(line_spans));
                    lines.push(Line::from(Span::styled(
                        format!("      {}", lc.tr("config-desc-streaming")),
                        desc_style,
                    )));
                }
                // Proactiveness cycle row
                ROW_PROACTIVENESS => {
                    let is_active = self.cursor == row;
                    let label_style = if is_active {
                        active_style
                    } else {
                        Style::default().fg(theme::TEXT)
                    };

                    let vals = ["low", "medium", "high"];
                    let mut value_spans: Vec<Span> = Vec::new();
                    for (i, v) in vals.iter().enumerate() {
                        if *v == self.buf_proactiveness.as_str() {
                            value_spans.push(Span::styled(format!("[{}]", v), active_style));
                        } else {
                            value_spans.push(Span::styled(v.to_string(), inactive_style));
                        }
                        if i < vals.len() - 1 {
                            value_spans.push(Span::styled("  ", Style::default()));
                        }
                    }
                    let mut line_spans = vec![
                        Span::styled("  ", Style::default()),
                        Span::styled(
                            format!(
                                "{:<width$}",
                                lc.tr(field_label_key(row)),
                                width = label_column_width
                            ),
                            label_style,
                        ),
                    ];
                    line_spans.extend(value_spans);
                    lines.push(Line::from(line_spans));
                    lines.push(Line::from(Span::styled(
                        format!("      {}", lc.tr("config-desc-proactiveness")),
                        desc_style,
                    )));
                }
                // Text rows: threshold, persona, tone
                ROW_THRESHOLD | ROW_PERSONA | ROW_TONE => {
                    let is_active = self.cursor == row;
                    let label_style = if is_active {
                        active_style
                    } else {
                        Style::default().fg(theme::TEXT)
                    };

                    let desc_key = match row {
                        ROW_THRESHOLD => "config-desc-threshold",
                        ROW_PERSONA => "config-desc-persona",
                        ROW_TONE => "config-desc-tone",
                        _ => "",
                    };

                    let field = match row {
                        ROW_THRESHOLD => &self.field_threshold,
                        ROW_PERSONA => &self.field_persona,
                        ROW_TONE => &self.field_tone,
                        _ => unreachable!(),
                    };

                    let value_display = if is_active {
                        field.text.clone()
                    } else if field.is_empty() {
                        "-".to_string()
                    } else {
                        field.text.clone()
                    };

                    lines.push(Line::from(vec![
                        Span::styled("  ", Style::default()),
                        Span::styled(
                            format!(
                                "{:<width$}",
                                lc.tr(field_label_key(row)),
                                width = label_column_width
                            ),
                            label_style,
                        ),
                        Span::styled(" ", Style::default()),
                        Span::styled(value_display, value_style),
                    ]));

                    lines.push(Line::from(Span::styled(
                        format!("      {}", lc.tr(desc_key)),
                        desc_style,
                    )));
                }
                _ => {}
            }
        }

        lines.truncate(inner.height as usize);
        f.render_widget(Paragraph::new(Text::from(lines)), inner);
    }

    fn handle_key(&mut self, input: Input, _ctx: &PanelReadContext) -> Vec<PanelEffect> {
        use tui_textarea::Key;
        match input {
            Input { key: Key::Esc, .. } => {
                // Save on close if on a text row
                if is_text_row(self.cursor) {
                    return self
                        .save_effects()
                        .into_iter()
                        .chain(std::iter::once(PanelEffect::Close))
                        .collect();
                }
                vec![PanelEffect::Close]
            }
            Input { key: Key::Up, .. } => {
                if is_text_row(self.cursor) {
                    let effects = self.save_effects();
                    self.cursor_up();
                    effects
                } else {
                    self.cursor_up();
                    vec![]
                }
            }
            Input { key: Key::Down, .. } => {
                if is_text_row(self.cursor) {
                    let effects = self.save_effects();
                    self.cursor_down();
                    effects
                } else {
                    self.cursor_down();
                    vec![]
                }
            }
            Input {
                key: Key::Enter, ..
            } => vec![],
            Input {
                key: Key::Char(' '),
                ctrl: false,
                ..
            } => {
                match self.cursor {
                    ROW_AUTOCOMPACT | ROW_CACHE_WARNING | ROW_LANGUAGE | ROW_PROACTIVENESS
                    | ROW_DIFF | ROW_STREAMING => {
                        match self.cursor {
                            ROW_AUTOCOMPACT => self.cycle_autocompact(),
                            ROW_CACHE_WARNING => self.cycle_cache_warning(),
                            ROW_LANGUAGE => self.cycle_language(false),
                            ROW_PROACTIVENESS => self.cycle_proactiveness(false),
                            ROW_DIFF => self.cycle_diff(),
                            ROW_STREAMING => self.cycle_streaming(false),
                            _ => {}
                        }
                        self.save_effects()
                    }
                    _ => {
                        // Text row: insert space character
                        self.handle_text_key(input);
                        vec![]
                    }
                }
            }
            Input {
                key: Key::Left,
                ctrl: false,
                ..
            } => match self.cursor {
                ROW_AUTOCOMPACT | ROW_CACHE_WARNING | ROW_LANGUAGE | ROW_PROACTIVENESS
                | ROW_DIFF | ROW_STREAMING => {
                    match self.cursor {
                        ROW_AUTOCOMPACT => self.cycle_autocompact(),
                        ROW_CACHE_WARNING => self.cycle_cache_warning(),
                        ROW_LANGUAGE => self.cycle_language(true),
                        ROW_PROACTIVENESS => self.cycle_proactiveness(true),
                        ROW_DIFF => self.cycle_diff(),
                        ROW_STREAMING => self.cycle_streaming(true),
                        _ => {}
                    }
                    self.save_effects()
                }
                _ => {
                    self.handle_text_key(input);
                    vec![]
                }
            },
            Input {
                key: Key::Right,
                ctrl: false,
                ..
            } => match self.cursor {
                ROW_AUTOCOMPACT | ROW_CACHE_WARNING | ROW_LANGUAGE | ROW_PROACTIVENESS
                | ROW_DIFF | ROW_STREAMING => {
                    match self.cursor {
                        ROW_AUTOCOMPACT => self.cycle_autocompact(),
                        ROW_CACHE_WARNING => self.cycle_cache_warning(),
                        ROW_LANGUAGE => self.cycle_language(false),
                        ROW_PROACTIVENESS => self.cycle_proactiveness(false),
                        ROW_DIFF => self.cycle_diff(),
                        ROW_STREAMING => self.cycle_streaming(false),
                        _ => {}
                    }
                    self.save_effects()
                }
                _ => {
                    self.handle_text_key(input);
                    vec![]
                }
            },
            _ => {
                self.handle_text_key(input);
                vec![]
            }
        }
    }

    fn handle_mouse(
        &mut self,
        mouse: MouseEvent,
        area: Rect,
        _ctx: &PanelReadContext,
    ) -> Vec<PanelEffect> {
        if mouse.kind == MouseEventKind::Down(ratatui::crossterm::event::MouseButton::Left) {
            let relative_y = mouse.row.saturating_sub(area.y);
            if relative_y >= 1 {
                let screen_line = (relative_y - 1) as usize;
                if let Some(clicked) = screen_to_logical_row(screen_line) {
                    if EDITABLE_ROWS.contains(&clicked) {
                        // Save text field before navigating away
                        let mut effects = vec![];
                        if is_text_row(self.cursor) && self.cursor != clicked {
                            effects = self.save_effects();
                        }
                        self.cursor = clicked;
                        return effects;
                    }
                }
            }
        }
        vec![]
    }

    fn handle_paste(&mut self, text: &str, _ctx: &PanelReadContext) -> Vec<PanelEffect> {
        if let Some(field) = self.active_field() {
            let filtered: String = text.chars().filter(|&c| c != '\n' && c != '\r').collect();
            field.insert_text(&filtered);
        }
        vec![]
    }

    fn desired_height(&self, _screen_h: u16, _screen_w: u16) -> u16 {
        (SCREEN_LAYOUT.len() + 2) as u16
    }

    fn status_bar_hints(&self, lc: &crate::i18n::LcRegistry) -> Vec<(String, String)> {
        vec![
            ("\u{2191}\u{2193}".to_string(), lc.tr("hint-config-field")),
            ("Space".to_string(), lc.tr("hint-config-toggle")),
            ("Esc".to_string(), lc.tr("key-close")),
        ]
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tui_textarea::Key;

    use super::*;
    use crate::panel::PanelState;
    use crate::panel::read_context::{PanelReadContext, ServiceRegistrySnapshot};

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

    fn space_input() -> Input {
        Input {
            key: Key::Char(' '),
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    #[test]
    fn test_kind_returns_config() {
        let panel = ConfigPanel::empty();
        assert_eq!(panel.kind(), PanelKind::Config);
    }

    #[test]
    fn test_esc_close() {
        let mut panel = ConfigPanel::empty();
        let ctx = make_ctx();
        // Default cursor is ROW_AUTOCOMPACT (not a text row), so no save effects
        let effects = panel.handle_key(esc_input(), &ctx);
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0], PanelEffect::Close);
    }

    #[test]
    fn test_esc_close_with_save_on_text_row() {
        let mut panel = ConfigPanel::empty();
        panel.cursor = ROW_THRESHOLD;
        let ctx = make_ctx();
        let effects = panel.handle_key(esc_input(), &ctx);
        // Must contain Close + save effects
        assert!(
            effects.contains(&PanelEffect::Close),
            "Esc on text row should emit Close"
        );
        let has_threshold_update = effects.iter().any(|e| {
            matches!(
                e,
                PanelEffect::UpdateConfig { key, .. } if key == "auto_compact_threshold"
            )
        });
        assert!(
            has_threshold_update,
            "Esc on text row should emit save effects"
        );
    }

    #[test]
    fn test_field_navigation() {
        let mut panel = ConfigPanel::empty();
        let ctx = make_ctx();

        // Start at ROW_AUTOCOMPACT (1)
        assert_eq!(panel.cursor, ROW_AUTOCOMPACT);

        // Down -> ROW_CACHE_WARNING (2)
        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.cursor, ROW_CACHE_WARNING);

        // Down -> ROW_THRESHOLD (3)
        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.cursor, ROW_THRESHOLD);

        // Down -> ROW_LANGUAGE (4)
        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.cursor, ROW_LANGUAGE);

        // Down -> ROW_DIFF (5)
        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.cursor, ROW_DIFF);

        // Down -> ROW_STREAMING (6)
        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.cursor, ROW_STREAMING);

        // Down -> ROW_PROACTIVENESS (7)
        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.cursor, ROW_PROACTIVENESS);

        // Down -> ROW_PERSONA (10, skipping separator/header)
        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.cursor, ROW_PERSONA);

        // Down -> ROW_TONE (11)
        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.cursor, ROW_TONE);

        // Down -> wrap to ROW_AUTOCOMPACT (1)
        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.cursor, ROW_AUTOCOMPACT);

        // Up -> wrap to ROW_TONE (11)
        panel.handle_key(up_input(), &ctx);
        assert_eq!(panel.cursor, ROW_TONE);

        // Up -> ROW_PERSONA (10)
        panel.handle_key(up_input(), &ctx);
        assert_eq!(panel.cursor, ROW_PERSONA);

        // Up -> ROW_PROACTIVENESS (7)
        panel.handle_key(up_input(), &ctx);
        assert_eq!(panel.cursor, ROW_PROACTIVENESS);
    }

    #[test]
    fn test_space_toggles_autocompact() {
        let mut panel = ConfigPanel::empty();
        panel.cursor = ROW_AUTOCOMPACT;
        let ctx = make_ctx();
        assert!(panel.buf_autocompact);

        let effects = panel.handle_key(space_input(), &ctx);
        assert!(!panel.buf_autocompact);
        // Should contain UpdateConfig for auto_compact_enabled
        let has_update = effects.iter().any(|e| {
            matches!(
                e,
                PanelEffect::UpdateConfig { key, value }
                    if key == "auto_compact_enabled" && value == "false"
            )
        });
        assert!(has_update, "Space on autocompact should emit UpdateConfig");
    }

    #[test]
    fn test_space_cycles_language() {
        let mut panel = ConfigPanel::empty();
        panel.cursor = ROW_LANGUAGE;
        let ctx = make_ctx();
        assert!(panel.buf_language.is_empty());

        panel.handle_key(space_input(), &ctx);
        assert_eq!(panel.buf_language, "en");

        panel.handle_key(space_input(), &ctx);
        assert_eq!(panel.buf_language, "zh-CN");

        panel.handle_key(space_input(), &ctx);
        assert_eq!(panel.buf_language, "en");
    }

    #[test]
    fn test_space_cycles_streaming() {
        let mut panel = ConfigPanel::empty();
        panel.cursor = ROW_STREAMING;
        let ctx = make_ctx();
        assert_eq!(panel.buf_streaming, "streaming");

        panel.handle_key(space_input(), &ctx);
        assert_eq!(panel.buf_streaming, "block");

        panel.handle_key(space_input(), &ctx);
        assert_eq!(panel.buf_streaming, "none");

        panel.handle_key(space_input(), &ctx);
        assert_eq!(panel.buf_streaming, "streaming");
    }

    #[test]
    fn test_new_from_config_values() {
        let panel = ConfigPanel::new(
            false, false, "70", "zh-CN", false, "block", "low", "concise", "formal",
        );
        assert!(!panel.buf_autocompact);
        assert!(!panel.buf_show_cache_warning);
        assert!(!panel.buf_diff);
        assert_eq!(panel.buf_language, "zh-CN");
        assert_eq!(panel.buf_streaming, "block");
        assert_eq!(panel.buf_proactiveness, "low");
        assert_eq!(panel.field_threshold.value(), "70");
        assert_eq!(panel.field_persona.value(), "concise");
        assert_eq!(panel.field_tone.value(), "formal");
    }

    #[test]
    fn test_desired_height() {
        let panel = ConfigPanel::empty();
        assert_eq!(panel.desired_height(50, 80), 23);
    }

    #[test]
    fn test_render_does_not_panic() {
        let mut panel = ConfigPanel::empty();
        let ctx = make_ctx();

        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| panel.render(f, Rect::new(0, 0, 80, 25), &ctx))
            .unwrap();
    }

    #[test]
    fn test_render_with_custom_config_does_not_panic() {
        let mut panel = ConfigPanel::new(
            true,
            true,
            "90",
            "en",
            true,
            "streaming",
            "high",
            "friendly",
            "casual",
        );
        panel.cursor = ROW_PERSONA;
        let ctx = make_ctx();

        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| panel.render(f, Rect::new(0, 0, 80, 25), &ctx))
            .unwrap();
    }

    #[test]
    fn test_status_bar_hints() {
        let panel = ConfigPanel::empty();
        let lc = crate::i18n::LcRegistry::default();
        let hints = panel.status_bar_hints(&lc);
        assert_eq!(hints.len(), 3);
    }

    #[test]
    fn test_paste_into_text_field() {
        let mut panel = ConfigPanel::empty();
        panel.cursor = ROW_PERSONA;
        let ctx = make_ctx();

        let effects = panel.handle_paste("hello world", &ctx);
        assert_eq!(panel.field_persona.value(), "hello world");
        assert!(effects.is_empty());
    }

    #[test]
    fn test_paste_filters_newlines() {
        let mut panel = ConfigPanel::empty();
        panel.cursor = ROW_PERSONA;
        let ctx = make_ctx();

        panel.handle_paste("line1\nline2\rline3", &ctx);
        assert_eq!(panel.field_persona.value(), "line1line2line3");
    }

    #[test]
    fn test_text_field_type_char() {
        let mut panel = ConfigPanel::empty();
        panel.cursor = ROW_THRESHOLD;
        let ctx = make_ctx();
        // Default is "85", clear it first
        panel.field_threshold.text.clear();
        panel.field_threshold.cursor = 0;

        panel.handle_key(
            Input {
                key: Key::Char('7'),
                ctrl: false,
                alt: false,
                shift: false,
            },
            &ctx,
        );
        panel.handle_key(
            Input {
                key: Key::Char('0'),
                ctrl: false,
                alt: false,
                shift: false,
            },
            &ctx,
        );
        assert_eq!(panel.field_threshold.value(), "70");
    }

    #[test]
    fn test_text_field_backspace() {
        let mut panel = ConfigPanel::empty();
        panel.cursor = ROW_THRESHOLD;
        let ctx = make_ctx();
        // Default is "85"
        assert_eq!(panel.field_threshold.value(), "85");

        panel.handle_key(
            Input {
                key: Key::Backspace,
                ctrl: false,
                alt: false,
                shift: false,
            },
            &ctx,
        );
        assert_eq!(panel.field_threshold.value(), "8");
    }

    #[test]
    fn test_save_effects_includes_all_fields() {
        let panel = ConfigPanel::empty();
        let effects = panel.save_effects();
        // 9 UpdateConfig + 1 SendToAcp = 10
        assert_eq!(effects.len(), 10);
        let config_count = effects
            .iter()
            .filter(|e| matches!(e, PanelEffect::UpdateConfig { .. }))
            .count();
        assert_eq!(config_count, 9);
        let has_acp = effects
            .iter()
            .any(|e| matches!(e, PanelEffect::SendToAcp { .. }));
        assert!(has_acp);
    }
}
