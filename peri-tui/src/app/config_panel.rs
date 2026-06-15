use std::any::Any;

use ratatui::{layout::Rect, Frame};
use tui_textarea::Input;

use super::{
    field_textarea::FieldTextarea,
    panel_component::PanelComponent,
    panel_manager::{EventResult, PanelContext, PanelKind},
    App,
};
use crate::config::PeriConfig;

// ─── 行索引常量 ─────────────────────────────────────────────────────────────────

pub const ROW_GENERAL_HEADER: usize = 0;
pub const ROW_AUTOCOMPACT: usize = 1;
pub const ROW_THRESHOLD: usize = 2;
pub const ROW_LANGUAGE: usize = 3;
pub const ROW_DIFF: usize = 4;
pub const ROW_STREAMING: usize = 5;
pub const ROW_PROACTIVENESS: usize = 6;
pub const ROW_SEPARATOR: usize = 7;
pub const ROW_OVERRIDES_HEADER: usize = 8;
pub const ROW_PERSONA: usize = 9;
pub const ROW_TONE: usize = 10;
pub const ROW_COUNT: usize = 11;

fn next_editable_row(current: usize, reverse: bool) -> usize {
    let editable: &[usize] = &[
        ROW_AUTOCOMPACT,
        ROW_THRESHOLD,
        ROW_LANGUAGE,
        ROW_DIFF,
        ROW_STREAMING,
        ROW_PROACTIVENESS,
        ROW_PERSONA,
        ROW_TONE,
    ];
    if reverse {
        editable
            .iter()
            .rev()
            .find(|&&r| r < current)
            .copied()
            .unwrap_or(editable[editable.len() - 1])
    } else {
        editable
            .iter()
            .find(|&&r| r > current)
            .copied()
            .unwrap_or(editable[0])
    }
}

fn is_text_row(row: usize) -> bool {
    matches!(row, ROW_THRESHOLD | ROW_PERSONA | ROW_TONE)
}

/// 屏幕行号 → 逻辑行号。
/// 渲染时每个可编辑字段占 2 行（值行 + 描述行），非编辑行占 1 行。
const SCREEN_LAYOUT: &[usize] = &[
    ROW_GENERAL_HEADER,   // screen 0
    ROW_AUTOCOMPACT,      // screen 1: value
    ROW_AUTOCOMPACT,      // screen 2: desc
    ROW_THRESHOLD,        // screen 3: value
    ROW_THRESHOLD,        // screen 4: desc
    ROW_LANGUAGE,         // screen 5: value
    ROW_LANGUAGE,         // screen 6: desc
    ROW_DIFF,             // screen 7: value
    ROW_DIFF,             // screen 8: desc
    ROW_STREAMING,        // screen 9: value
    ROW_STREAMING,        // screen 10: desc
    ROW_PROACTIVENESS,    // screen 11: value
    ROW_PROACTIVENESS,    // screen 12: desc
    ROW_SEPARATOR,        // screen 13
    ROW_OVERRIDES_HEADER, // screen 14
    ROW_PERSONA,          // screen 15: value
    ROW_PERSONA,          // screen 16: desc
    ROW_TONE,             // screen 17: value
    ROW_TONE,             // screen 18: desc
];

fn screen_to_logical_row(screen_line: usize) -> Option<usize> {
    SCREEN_LAYOUT.get(screen_line).copied()
}

fn save_config_now(panel: &mut ConfigPanel, ctx: &mut PanelContext<'_>) {
    let cfg_arc = ctx.services.peri_config.clone();
    let mut cfg = cfg_arc.write();
    if panel.apply_edit(&mut cfg, &ctx.services.lc).is_ok() {
        if let Some(ref lang) = cfg.config.language.clone() {
            let _ = ctx.services.lc.switch(lang);
        }
        let _ = App::save_config(&cfg, ctx.services.config_path_override.as_deref());
    }
}

// ─── ConfigPanel ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct ConfigPanel {
    pub cursor: usize,
    // 编辑缓冲区
    pub buf_autocompact: bool,
    pub field_threshold: FieldTextarea,
    pub buf_language: String, // "" = auto, "en", "zh-CN"
    pub field_persona: FieldTextarea,
    pub field_tone: FieldTextarea,
    pub buf_proactiveness: String, // "low" / "medium" / "high"
    pub buf_diff: bool,
    pub buf_streaming: String, // "streaming" / "block" / "none"
}

impl ConfigPanel {
    pub fn from_config(cfg: &PeriConfig) -> Self {
        let compact_config = cfg.config.compact.as_ref();
        let autocompact = compact_config
            .map(|c| c.auto_compact_enabled)
            .unwrap_or(true);
        let threshold = compact_config
            .map(|c| format!("{}", (c.auto_compact_threshold * 100.0) as u8))
            .unwrap_or_else(|| "85".to_string());
        let proactiveness = cfg
            .config
            .proactiveness
            .clone()
            .unwrap_or_else(|| "medium".to_string());
        let diff_enabled = cfg.config.diff_enabled;

        let mut field_threshold = FieldTextarea::single_line();
        field_threshold.set_value(&threshold);

        let mut field_persona = FieldTextarea::single_line();
        field_persona.set_value(cfg.config.persona.as_deref().unwrap_or(""));

        let mut field_tone = FieldTextarea::single_line();
        field_tone.set_value(cfg.config.tone.as_deref().unwrap_or(""));

        Self {
            cursor: ROW_AUTOCOMPACT,
            buf_autocompact: autocompact,
            field_threshold,
            buf_language: cfg.config.language.clone().unwrap_or_default(),
            field_persona,
            field_tone,
            buf_proactiveness: proactiveness,
            buf_diff: diff_enabled,
            buf_streaming: cfg
                .config
                .streaming_mode
                .clone()
                .unwrap_or_else(|| "streaming".to_string()),
        }
    }

    pub fn cursor_down(&mut self) {
        self.cursor = next_editable_row(self.cursor, false);
    }

    pub fn cursor_up(&mut self) {
        self.cursor = next_editable_row(self.cursor, true);
    }

    pub fn cycle_autocompact(&mut self) {
        self.buf_autocompact = !self.buf_autocompact;
    }

    pub fn cycle_proactiveness(&mut self) {
        self.buf_proactiveness = match self.buf_proactiveness.as_str() {
            "low" => "medium".to_string(),
            "medium" => "high".to_string(),
            _ => "low".to_string(),
        };
    }

    pub fn cycle_diff(&mut self) {
        self.buf_diff = !self.buf_diff;
    }

    pub fn cycle_streaming(&mut self, reverse: bool) {
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

    /// 可选语言列表："" (auto) → "en" → "zh-CN" → ""
    const LANGUAGE_OPTIONS: &[&str] = &["en", "zh-CN"];

    pub fn cycle_language(&mut self, reverse: bool) {
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
            // 未匹配时：forward → 第一个，reverse → 最后一个
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

    pub fn paste_text(&mut self, text: &str) {
        if let Some(field) = self.active_field() {
            let filtered: String = text.chars().filter(|&c| c != '\n' && c != '\r').collect();
            field.insert_text(&filtered);
        }
    }

    pub fn active_field(&mut self) -> Option<&mut FieldTextarea> {
        match self.cursor {
            ROW_THRESHOLD => Some(&mut self.field_threshold),
            ROW_PERSONA => Some(&mut self.field_persona),
            ROW_TONE => Some(&mut self.field_tone),
            _ => None,
        }
    }

    pub fn apply_edit(
        &mut self,
        cfg: &mut PeriConfig,
        _lc: &crate::i18n::LcRegistry,
    ) -> Result<(), String> {
        // autocompact + threshold
        let compact = cfg
            .config
            .compact
            .get_or_insert_with(peri_agent::agent::CompactConfig::default);
        compact.auto_compact_enabled = self.buf_autocompact;
        let threshold_val: u8 = self
            .field_threshold
            .value()
            .parse()
            .unwrap_or(85)
            .clamp(50, 99);
        compact.auto_compact_threshold = threshold_val as f64 / 100.0;

        // language: value is always valid (selected from LANGUAGE_OPTIONS)
        cfg.config.language = if self.buf_language.is_empty() {
            None
        } else {
            Some(self.buf_language.clone())
        };

        // persona
        cfg.config.persona = if self.field_persona.is_empty() {
            None
        } else {
            Some(self.field_persona.value())
        };

        // tone
        cfg.config.tone = if self.field_tone.is_empty() {
            None
        } else {
            Some(self.field_tone.value())
        };

        // proactiveness
        cfg.config.proactiveness = if self.buf_proactiveness == "medium" {
            None
        } else {
            Some(self.buf_proactiveness.clone())
        };

        // diff
        cfg.config.diff_enabled = self.buf_diff;

        // streaming mode
        cfg.config.streaming_mode = if self.buf_streaming == "streaming" {
            None
        } else {
            Some(self.buf_streaming.clone())
        };

        Ok(())
    }

    fn input_char(&mut self, c: char) {
        if let Some(field) = self.active_field() {
            field.input(Input {
                key: tui_textarea::Key::Char(c),
                ctrl: false,
                alt: false,
                shift: false,
            });
        }
    }

    fn handle_text_key(&mut self, input: Input) {
        if let Some(field) = self.active_field() {
            field.input(input);
        }
    }
}

impl PanelComponent for ConfigPanel {
    fn kind(&self) -> PanelKind {
        PanelKind::Config
    }

    fn handle_key(&mut self, input: Input, ctx: &mut PanelContext<'_>) -> EventResult {
        use tui_textarea::Key;
        match input {
            Input { key: Key::Esc, .. } => {
                if is_text_row(self.cursor) {
                    save_config_now(self, ctx);
                }
                EventResult::ClosePanel
            }
            Input { key: Key::Up, .. } => {
                if is_text_row(self.cursor) {
                    save_config_now(self, ctx);
                }
                self.cursor_up();
                EventResult::Consumed
            }
            Input { key: Key::Down, .. } => {
                if is_text_row(self.cursor) {
                    save_config_now(self, ctx);
                }
                self.cursor_down();
                EventResult::Consumed
            }
            Input {
                key: Key::Enter, ..
            } => EventResult::Consumed,
            Input {
                key: Key::Char(' '),
                ctrl: false,
                ..
            } => {
                match self.cursor {
                    ROW_AUTOCOMPACT | ROW_LANGUAGE | ROW_PROACTIVENESS | ROW_DIFF
                    | ROW_STREAMING => {
                        match self.cursor {
                            ROW_AUTOCOMPACT => self.cycle_autocompact(),
                            ROW_LANGUAGE => self.cycle_language(false),
                            ROW_PROACTIVENESS => self.cycle_proactiveness(),
                            ROW_DIFF => self.cycle_diff(),
                            ROW_STREAMING => self.cycle_streaming(false),
                            _ => {}
                        }
                        save_config_now(self, ctx);
                    }
                    _ => self.input_char(' '),
                }
                EventResult::Consumed
            }
            Input {
                key: Key::Left,
                ctrl: false,
                ..
            } => {
                match self.cursor {
                    ROW_AUTOCOMPACT | ROW_LANGUAGE | ROW_PROACTIVENESS | ROW_DIFF
                    | ROW_STREAMING => {
                        match self.cursor {
                            ROW_AUTOCOMPACT => self.cycle_autocompact(),
                            ROW_LANGUAGE => self.cycle_language(true),
                            ROW_PROACTIVENESS => self.cycle_proactiveness(),
                            ROW_DIFF => self.cycle_diff(),
                            ROW_STREAMING => self.cycle_streaming(true),
                            _ => {}
                        }
                        save_config_now(self, ctx);
                    }
                    _ => {
                        self.handle_text_key(input);
                    }
                }
                EventResult::Consumed
            }
            Input {
                key: Key::Right,
                ctrl: false,
                ..
            } => {
                match self.cursor {
                    ROW_AUTOCOMPACT | ROW_LANGUAGE | ROW_PROACTIVENESS | ROW_DIFF
                    | ROW_STREAMING => {
                        match self.cursor {
                            ROW_AUTOCOMPACT => self.cycle_autocompact(),
                            ROW_LANGUAGE => self.cycle_language(false),
                            ROW_PROACTIVENESS => self.cycle_proactiveness(),
                            ROW_DIFF => self.cycle_diff(),
                            ROW_STREAMING => self.cycle_streaming(false),
                            _ => {}
                        }
                        save_config_now(self, ctx);
                    }
                    _ => {
                        self.handle_text_key(input);
                    }
                }
                EventResult::Consumed
            }
            _ => {
                self.handle_text_key(input);
                EventResult::Consumed
            }
        }
    }

    fn handle_paste(&mut self, text: &str, _ctx: &mut PanelContext<'_>) -> EventResult {
        self.paste_text(text);
        EventResult::Consumed
    }

    fn handle_scroll(&mut self, _lines: i16, _ctx: &mut PanelContext<'_>) -> EventResult {
        EventResult::NotConsumed
    }

    fn set_scroll_offset(&mut self, _offset: u16) {}

    fn handle_mouse(
        &mut self,
        mouse: ratatui::crossterm::event::MouseEvent,
        area: Rect,
        _ctx: &mut PanelContext<'_>,
    ) -> EventResult {
        use ratatui::crossterm::event::{MouseButton, MouseEventKind};
        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            let relative_y = mouse.row.saturating_sub(area.y);
            if relative_y >= 1 {
                let screen_line = (relative_y - 1) as usize;
                if let Some(clicked) = screen_to_logical_row(screen_line) {
                    if matches!(
                        clicked,
                        ROW_AUTOCOMPACT
                            | ROW_THRESHOLD
                            | ROW_LANGUAGE
                            | ROW_DIFF
                            | ROW_STREAMING
                            | ROW_PROACTIVENESS
                            | ROW_PERSONA
                            | ROW_TONE
                    ) {
                        if is_text_row(self.cursor) && self.cursor != clicked {
                            save_config_now(self, _ctx);
                        }
                        self.cursor = clicked;
                        return EventResult::Consumed;
                    }
                }
            }
        }
        EventResult::NotConsumed
    }

    fn desired_height(&self, _screen_height: u16, _screen_width: u16) -> u16 {
        (SCREEN_LAYOUT.len() + 2) as u16
    }

    fn render(&mut self, f: &mut Frame, app: &mut App, area: Rect) {
        crate::ui::main_ui::panels::config::render_config_panel(f, self, app, area);
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn status_bar_hints(&self, lc: &crate::i18n::LcRegistry) -> Vec<(String, String)> {
        vec![
            ("↑↓".to_string(), lc.tr("hint-config-field")),
            ("Space".to_string(), lc.tr("hint-config-toggle")),
            ("Esc".to_string(), lc.tr("key-close")),
        ]
    }
}

#[cfg(test)]
#[path = "config_panel_test.rs"]
mod tests;
