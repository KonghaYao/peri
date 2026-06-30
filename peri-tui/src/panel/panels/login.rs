//! v2 LoginPanel -- Provider management panel (PanelState trait implementation).
//!
//! Displays and edits provider entries in four modes:
//!   - Browse: list providers with cursor navigation
//!   - Edit: edit existing provider fields (Name, Type, BaseUrl, ApiKey, Models)
//!   - New: create a new provider
//!   - ConfirmDelete: confirm deletion with y/n
//!
//! All changes produce `PanelEffect::UpdateConfig` + `PanelEffect::SendToAcp`
//! instructions; the state machine translates them to actual operations.
//!
//! Text fields use a lightweight `TextField` (String + cursor) instead of
//! `tui_textarea::TextArea` to satisfy the `Send` bound required by `PanelState`.
//!
//! Data source: `app.services.peri_config.read()` via `from_app()` — P3 Integration 已完成。

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
// Lightweight text field (Send-safe, replaces FieldTextarea)
// ---------------------------------------------------------------------------

/// Minimal single-line text editor state (String + byte cursor).
///
/// Satisfies `Send` (unlike `tui_textarea::TextArea` which is not thread-safe
/// due to `ratatui_widgets::block::shadow::Effect`).
#[derive(Debug, Clone)]
struct TextField {
    text: String,
    cursor: usize,
}

impl TextField {
    fn new(value: &str) -> Self {
        Self {
            text: value.to_string(),
            cursor: value.len(),
        }
    }

    fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    fn set_value(&mut self, value: &str) {
        self.text = value.to_string();
        self.cursor = value.len();
    }

    fn value(&self) -> String {
        self.text.clone()
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
// Local DTOs (data source: ServiceRegistrySnapshot injection in P3 Integration)
// ---------------------------------------------------------------------------

/// Provider entry DTO for display and editing.
#[derive(Debug, Clone)]
struct ProviderEntry {
    name: String,
    provider_type: String,
    base_url: String,
    opus_model: String,
    sonnet_model: String,
    haiku_model: String,
}

impl ProviderEntry {
    fn display_name(&self) -> &str {
        if self.name.is_empty() {
            "Unnamed"
        } else {
            &self.name
        }
    }
}

/// Panel mode.
#[derive(Debug, Clone, PartialEq)]
enum LoginMode {
    /// List browsing with cursor navigation.
    Browse,
    /// Edit existing provider fields.
    Edit { field: LoginField },
    /// Create new provider.
    New { field: LoginField },
    /// Delete confirmation (holds cursor index of target).
    ConfirmDelete(usize),
}

/// Editable fields within a provider.
#[derive(Debug, Clone, PartialEq)]
enum LoginField {
    Name,
    Type,
    BaseUrl,
    ApiKey,
    OpusModel,
    SonnetModel,
    HaikuModel,
}

impl LoginField {
    fn next(&self) -> Self {
        match self {
            Self::Name => Self::Type,
            Self::Type => Self::BaseUrl,
            Self::BaseUrl => Self::ApiKey,
            Self::ApiKey => Self::OpusModel,
            Self::OpusModel => Self::SonnetModel,
            Self::SonnetModel => Self::HaikuModel,
            Self::HaikuModel => Self::Name,
        }
    }

    fn prev(&self) -> Self {
        match self {
            Self::Name => Self::HaikuModel,
            Self::Type => Self::Name,
            Self::BaseUrl => Self::Type,
            Self::ApiKey => Self::BaseUrl,
            Self::OpusModel => Self::ApiKey,
            Self::SonnetModel => Self::OpusModel,
            Self::HaikuModel => Self::SonnetModel,
        }
    }
}

// ---------------------------------------------------------------------------
// Default model names (mirrors old DEFAULT_MODELS constant)
// ---------------------------------------------------------------------------

/// (provider_type, opus, sonnet, haiku)
const DEFAULT_MODELS: &[(&str, &str, &str, &str)] = &[
    (
        "anthropic",
        "claude-opus-4-7",
        "claude-sonnet-4-6",
        "claude-haiku-4-5",
    ),
    ("openai", "gpt-4o", "gpt-4o-mini", "gpt-3.5-turbo"),
];

const PROVIDER_TYPES: &[&str] = &["openai", "anthropic"];

// ---------------------------------------------------------------------------
// LoginPanel
// ---------------------------------------------------------------------------

/// v2 LoginPanel -- Provider management.
pub struct LoginPanel {
    /// Provider list (local DTO snapshot).
    providers: Vec<ProviderEntry>,
    /// Browse mode cursor.
    cursor: usize,
    /// Current mode.
    mode: LoginMode,
    /// Edit buffers (used in Edit/New modes).
    buf_name: TextField,
    buf_type: String,
    buf_base_url: TextField,
    buf_api_key: TextField,
    buf_opus_model: TextField,
    buf_sonnet_model: TextField,
    buf_haiku_model: TextField,
}

impl std::fmt::Debug for LoginPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginPanel")
            .field("providers", &self.providers)
            .field("cursor", &self.cursor)
            .field("mode", &self.mode)
            .finish()
    }
}

// Safety: LoginPanel only contains Send-safe types (String, usize, TextField, Vec<ProviderEntry>).
unsafe impl Send for LoginPanel {}

impl LoginPanel {
    /// Create an empty panel (no providers, Browse mode).
    pub fn empty() -> Self {
        Self {
            providers: Vec::new(),
            cursor: 0,
            mode: LoginMode::Browse,
            buf_name: TextField::new(""),
            buf_type: String::new(),
            buf_base_url: TextField::new(""),
            buf_api_key: TextField::new(""),
            buf_opus_model: TextField::new(""),
            buf_sonnet_model: TextField::new(""),
            buf_haiku_model: TextField::new(""),
        }
    }

    /// Construct a panel from a `PeriConfig` reference, reading provider list.
    pub fn from_config(cfg: &crate::config::PeriConfig) -> Self {
        let providers: Vec<ProviderEntry> = cfg
            .config
            .providers
            .iter()
            .map(|p| ProviderEntry {
                name: p.name.clone().unwrap_or_default(),
                provider_type: p.provider_type.clone(),
                base_url: p.base_url.clone(),
                opus_model: p.models.opus.clone(),
                sonnet_model: p.models.sonnet.clone(),
                haiku_model: p.models.haiku.clone(),
            })
            .collect();
        let mut panel = Self::empty();
        panel.providers = providers;
        panel
    }

    /// Construct a panel from the live `App` state.
    pub fn from_app(app: &crate::app::App) -> Self {
        Self::from_config(&app.services.peri_config.read())
    }

    /// Create with initial provider entries for testing.
    #[cfg(test)]
    fn with_providers(providers: Vec<ProviderEntry>) -> Self {
        let mut panel = Self::empty();
        panel.providers = providers;
        panel
    }

    // -- Browse mode operations --

    fn move_cursor(&mut self, delta: isize) {
        if self.providers.is_empty() {
            return;
        }
        let new =
            (self.cursor as isize + delta).clamp(0, self.providers.len() as isize - 1) as usize;
        self.cursor = new;
    }

    fn enter_edit(&mut self) {
        if let Some(p) = self.providers.get(self.cursor) {
            self.buf_name.set_value(&p.name);
            self.buf_type = p.provider_type.clone();
            self.buf_base_url.set_value(&p.base_url);
            self.buf_api_key.clear(); // masked, don't prefill
            self.buf_opus_model.set_value(&p.opus_model);
            self.buf_sonnet_model.set_value(&p.sonnet_model);
            self.buf_haiku_model.set_value(&p.haiku_model);
            self.mode = LoginMode::Edit {
                field: LoginField::Name,
            };
        }
    }

    fn enter_new(&mut self) {
        self.buf_name.clear();
        self.buf_type = "openai".to_string();
        self.buf_base_url.clear();
        self.buf_api_key.clear();
        self.buf_opus_model.clear();
        self.buf_sonnet_model.clear();
        self.buf_haiku_model.clear();
        self.auto_fill_models_for_type();
        self.mode = LoginMode::New {
            field: LoginField::Name,
        };
    }

    fn request_delete(&mut self) {
        if !self.providers.is_empty() {
            self.mode = LoginMode::ConfirmDelete(self.cursor);
        }
    }

    // -- Edit/New mode operations --

    fn active_field(&self) -> LoginField {
        match &self.mode {
            LoginMode::Edit { field } | LoginMode::New { field } => field.clone(),
            _ => LoginField::Name,
        }
    }

    fn set_active_field(&mut self, field: LoginField) {
        match &mut self.mode {
            LoginMode::Edit { field: f } | LoginMode::New { field: f } => *f = field,
            _ => {}
        }
    }

    fn field_next(&mut self) {
        let next = self.active_field().next();
        self.set_active_field(next);
    }

    fn field_prev(&mut self) {
        let prev = self.active_field().prev();
        self.set_active_field(prev);
    }

    fn cycle_type(&mut self) {
        if self.active_field() != LoginField::Type {
            return;
        }
        let cur = PROVIDER_TYPES
            .iter()
            .position(|&t| t == self.buf_type)
            .unwrap_or(0);
        self.buf_type = PROVIDER_TYPES[(cur + 1) % PROVIDER_TYPES.len()].to_string();
        self.auto_fill_models_for_type();
    }

    fn auto_fill_models_for_type(&mut self) {
        let new_defaults = DEFAULT_MODELS
            .iter()
            .find(|(t, _, _, _)| *t == self.buf_type);
        let (opus_default, sonnet_default, haiku_default) = match new_defaults {
            Some((_, o, s, h)) => (o.to_string(), s.to_string(), h.to_string()),
            None => return,
        };

        let all_defaults: Vec<(String, String, String)> = DEFAULT_MODELS
            .iter()
            .map(|(_, o, s, h)| (o.to_string(), s.to_string(), h.to_string()))
            .collect();

        let is_default_or_empty = |val: &str| -> bool {
            if val.is_empty() {
                return true;
            }
            all_defaults
                .iter()
                .any(|(o, s, h)| val == o || val == s || val == h)
        };

        if is_default_or_empty(&self.buf_opus_model.value()) {
            self.buf_opus_model.set_value(&opus_default);
        }
        if is_default_or_empty(&self.buf_sonnet_model.value()) {
            self.buf_sonnet_model.set_value(&sonnet_default);
        }
        if is_default_or_empty(&self.buf_haiku_model.value()) {
            self.buf_haiku_model.set_value(&haiku_default);
        }
    }

    /// Get a mutable reference to the active text field (None for Type field).
    fn active_text_field_mut(&mut self) -> Option<&mut TextField> {
        match self.active_field() {
            LoginField::Name => Some(&mut self.buf_name),
            LoginField::Type => None,
            LoginField::BaseUrl => Some(&mut self.buf_base_url),
            LoginField::ApiKey => Some(&mut self.buf_api_key),
            LoginField::OpusModel => Some(&mut self.buf_opus_model),
            LoginField::SonnetModel => Some(&mut self.buf_sonnet_model),
            LoginField::HaikuModel => Some(&mut self.buf_haiku_model),
        }
    }

    /// Save current edit/new data, producing config update effects.
    fn save_edit(&mut self) -> Vec<PanelEffect> {
        let is_new = matches!(self.mode, LoginMode::New { .. });
        let name = self.buf_name.value().trim().to_string();
        if name.is_empty() {
            return vec![PanelEffect::ShowNotification(
                "Provider name cannot be empty".to_string(),
            )];
        }

        let mut effects = Vec::new();
        let id = name.to_lowercase().replace(' ', "_");

        // Build update config effects for each field
        effects.push(PanelEffect::UpdateConfig {
            key: format!("provider.{}.name", id),
            value: name.clone(),
        });
        effects.push(PanelEffect::UpdateConfig {
            key: format!("provider.{}.type", id),
            value: self.buf_type.clone(),
        });
        effects.push(PanelEffect::UpdateConfig {
            key: format!("provider.{}.base_url", id),
            value: self.buf_base_url.value(),
        });
        effects.push(PanelEffect::UpdateConfig {
            key: format!("provider.{}.api_key", id),
            value: self.buf_api_key.value(),
        });
        effects.push(PanelEffect::UpdateConfig {
            key: format!("provider.{}.opus_model", id),
            value: self.buf_opus_model.value(),
        });
        effects.push(PanelEffect::UpdateConfig {
            key: format!("provider.{}.sonnet_model", id),
            value: self.buf_sonnet_model.value(),
        });
        effects.push(PanelEffect::UpdateConfig {
            key: format!("provider.{}.haiku_model", id),
            value: self.buf_haiku_model.value(),
        });

        // Activate the provider
        effects.push(PanelEffect::UpdateConfig {
            key: "active_provider_id".to_string(),
            value: id.clone(),
        });

        // Notify ACP server
        effects.push(PanelEffect::SendToAcp {
            event: "update_provider".to_string(),
            data: serde_json::json!({
                "id": id,
                "name": name,
                "is_new": is_new,
            }),
        });

        let notification = if is_new {
            format!("Provider '{}' created", name)
        } else {
            format!("Provider '{}' saved", name)
        };
        effects.push(PanelEffect::ShowNotification(notification));
        effects.push(PanelEffect::Close);

        // Return to browse mode
        self.mode = LoginMode::Browse;
        effects
    }

    /// Confirm deletion, producing config update effects.
    fn confirm_delete_action(&mut self) -> Vec<PanelEffect> {
        let mut effects = Vec::new();
        if let Some(p) = self.providers.get(self.cursor) {
            let name = p.display_name().to_string();
            let id = name.to_lowercase().replace(' ', "_");

            effects.push(PanelEffect::UpdateConfig {
                key: format!("delete_provider.{}", id),
                value: "true".to_string(),
            });
            effects.push(PanelEffect::SendToAcp {
                event: "delete_provider".to_string(),
                data: serde_json::json!({
                    "id": id,
                    "name": name,
                }),
            });
            effects.push(PanelEffect::ShowNotification(format!(
                "Provider '{}' deleted",
                name
            )));
        }

        // Return to browse
        self.mode = LoginMode::Browse;
        effects
    }
}

// ---------------------------------------------------------------------------
// PanelState implementation
// ---------------------------------------------------------------------------

impl PanelState for LoginPanel {
    fn kind(&self) -> PanelKind {
        PanelKind::Login
    }

    fn render(&mut self, f: &mut Frame, area: Rect, _ctx: &PanelReadContext) {
        let border_color = match &self.mode {
            LoginMode::Browse => theme::BORDER,
            LoginMode::Edit { .. } => theme::WARNING,
            LoginMode::New { .. } => theme::SAGE,
            LoginMode::ConfirmDelete(_) => theme::ERROR,
        };

        let title = match &self.mode {
            LoginMode::Browse => "Providers",
            LoginMode::Edit { .. } => "Edit Provider",
            LoginMode::New { .. } => "New Provider",
            LoginMode::ConfirmDelete(_) => "Delete Provider",
        };

        let inner = BorderedPanel::new(Span::styled(
            title,
            Style::default()
                .fg(theme::THINKING)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(border_color))
        .render(f, area);

        match &self.mode {
            LoginMode::Browse => {
                render_browse(f, inner, &self.providers, self.cursor);
            }

            LoginMode::Edit { field } | LoginMode::New { field } => {
                render_edit(
                    f,
                    inner,
                    field,
                    &self.buf_name,
                    &self.buf_type,
                    &self.buf_base_url,
                    &self.buf_api_key,
                    &self.buf_opus_model,
                    &self.buf_sonnet_model,
                    &self.buf_haiku_model,
                );
            }

            LoginMode::ConfirmDelete(_) => {
                render_confirm_delete(f, inner, &self.providers, self.cursor);
            }
        }
    }

    fn handle_key(&mut self, input: Input, _ctx: &PanelReadContext) -> Vec<PanelEffect> {
        use tui_textarea::Key;

        match &self.mode {
            LoginMode::Browse => match input {
                Input { key: Key::Esc, .. } => vec![PanelEffect::Close],
                Input { key: Key::Up, .. } => {
                    self.move_cursor(-1);
                    vec![]
                }
                Input { key: Key::Down, .. } => {
                    self.move_cursor(1);
                    vec![]
                }
                Input {
                    key: Key::Char('e'),
                    ctrl: false,
                    alt: false,
                    shift: false,
                }
                | Input {
                    key: Key::Enter,
                    ctrl: false,
                    alt: false,
                    shift: false,
                } => {
                    self.enter_edit();
                    vec![]
                }
                Input {
                    key: Key::Char('n'),
                    ctrl: false,
                    alt: false,
                    shift: false,
                } => {
                    self.enter_new();
                    vec![]
                }
                Input {
                    key: Key::Char('d'),
                    ctrl: false,
                    alt: false,
                    shift: false,
                } => {
                    self.request_delete();
                    vec![]
                }
                _ => vec![],
            },

            LoginMode::Edit { .. } | LoginMode::New { .. } => {
                let is_type_field = self.active_field() == LoginField::Type;
                match input {
                    Input { key: Key::Esc, .. } => {
                        self.mode = LoginMode::Browse;
                        vec![]
                    }
                    Input { key: Key::Up, .. } => {
                        self.field_prev();
                        vec![]
                    }
                    Input { key: Key::Down, .. } => {
                        self.field_next();
                        vec![]
                    }
                    Input {
                        key: Key::Tab,
                        shift: false,
                        ..
                    } => {
                        self.field_next();
                        vec![]
                    }
                    Input {
                        key: Key::Tab,
                        shift: true,
                        ..
                    } => {
                        self.field_prev();
                        vec![]
                    }
                    Input { key: Key::Left, .. }
                    | Input {
                        key: Key::Right, ..
                    } if is_type_field => {
                        self.cycle_type();
                        vec![]
                    }
                    Input {
                        key: Key::Char(' '),
                        ..
                    } => {
                        if is_type_field {
                            self.cycle_type();
                        } else if let Some(field) = self.active_text_field_mut() {
                            field.insert_char(' ');
                        }
                        vec![]
                    }
                    Input {
                        key: Key::Enter, ..
                    } => self.save_edit(),
                    _ => {
                        if !is_type_field {
                            if let Some(field) = self.active_text_field_mut() {
                                field.handle_input(input);
                            }
                        }
                        vec![]
                    }
                }
            }

            LoginMode::ConfirmDelete(_) => match input {
                Input {
                    key: Key::Char('y'),
                    ctrl: false,
                    alt: false,
                    shift: false,
                } => self.confirm_delete_action(),
                Input {
                    key: Key::Enter, ..
                } => self.confirm_delete_action(),
                Input { key: Key::Esc, .. } => {
                    self.mode = LoginMode::Browse;
                    vec![]
                }
                Input {
                    key: Key::Char('n'),
                    ctrl: false,
                    alt: false,
                    shift: false,
                } => {
                    self.mode = LoginMode::Browse;
                    vec![]
                }
                _ => {
                    // Any other key cancels
                    self.mode = LoginMode::Browse;
                    vec![]
                }
            },
        }
    }

    fn handle_paste(&mut self, text: &str, _ctx: &PanelReadContext) -> Vec<PanelEffect> {
        let filtered: String = text.chars().filter(|&c| c != '\n' && c != '\r').collect();
        if let Some(field) = self.active_text_field_mut() {
            field.insert_text(&filtered);
        }
        vec![]
    }

    fn desired_height(&self, _screen_h: u16, _screen_w: u16) -> u16 {
        match &self.mode {
            LoginMode::Browse => 14,
            LoginMode::Edit { .. } | LoginMode::New { .. } => 20,
            LoginMode::ConfirmDelete(_) => 14,
        }
    }

    fn status_bar_hints(&self, _lc: &crate::i18n::LcRegistry) -> Vec<(String, String)> {
        match &self.mode {
            LoginMode::Browse => vec![
                ("\u{2191}\u{2193}".to_string(), "Navigate".to_string()),
                ("e".to_string(), "Edit".to_string()),
                ("n".to_string(), "New".to_string()),
                ("d".to_string(), "Delete".to_string()),
                ("Esc".to_string(), "Close".to_string()),
            ],
            LoginMode::Edit { .. } | LoginMode::New { .. } => vec![
                ("\u{2191}\u{2193}".to_string(), "Field".to_string()),
                ("Tab".to_string(), "Next".to_string()),
                ("Enter".to_string(), "Save".to_string()),
                ("Space".to_string(), "Toggle".to_string()),
                ("Esc".to_string(), "Back".to_string()),
            ],
            LoginMode::ConfirmDelete(_) => vec![
                ("y".to_string(), "Confirm".to_string()),
                ("n/Esc".to_string(), "Cancel".to_string()),
            ],
        }
    }
}

// ---------------------------------------------------------------------------
// Render helpers
// ---------------------------------------------------------------------------

fn render_browse(f: &mut Frame, area: Rect, providers: &[ProviderEntry], cursor: usize) {
    let mut lines: Vec<Line> = Vec::new();
    for (i, p) in providers.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }
        let is_cursor = i == cursor;
        let bullet = if is_cursor { "\u{25cf}" } else { "\u{25cb}" };
        let cursor_char = if is_cursor { "\u{276f}" } else { " " };
        let row_style = if is_cursor {
            Style::default().fg(theme::THINKING)
        } else {
            Style::default().fg(theme::TEXT)
        };
        let cursor_style = Style::default().fg(theme::THINKING);
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", cursor_char), cursor_style),
            Span::styled(format!("{} ", bullet), row_style),
            Span::styled(
                format!("{} ", p.display_name()),
                row_style.add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("({})", p.provider_type),
                Style::default().fg(theme::MUTED),
            ),
        ]));
        // Model sub-row
        let fmt_model = |v: &str| -> String {
            if v.is_empty() {
                "(none)".to_string()
            } else {
                v.to_string()
            }
        };
        lines.push(Line::from(vec![
            Span::styled("       ", Style::default().fg(theme::MUTED)),
            Span::styled(
                "Opus ",
                Style::default()
                    .fg(theme::MUTED)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(fmt_model(&p.opus_model), Style::default().fg(theme::MUTED)),
            Span::styled(
                "  Sonnet ",
                Style::default()
                    .fg(theme::MUTED)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                fmt_model(&p.sonnet_model),
                Style::default().fg(theme::MUTED),
            ),
            Span::styled(
                "  Haiku ",
                Style::default()
                    .fg(theme::MUTED)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(fmt_model(&p.haiku_model), Style::default().fg(theme::MUTED)),
        ]));
    }
    if providers.is_empty() {
        lines.push(Line::from(Span::styled(
            "No providers configured. Press 'n' to add one.",
            Style::default().fg(theme::MUTED),
        )));
    }
    lines.truncate(area.height as usize);
    f.render_widget(Paragraph::new(Text::from(lines)), area);
}

#[allow(clippy::too_many_arguments)]
fn render_edit(
    f: &mut Frame,
    area: Rect,
    active_field: &LoginField,
    buf_name: &TextField,
    buf_type: &str,
    buf_base_url: &TextField,
    buf_api_key: &TextField,
    buf_opus_model: &TextField,
    buf_sonnet_model: &TextField,
    buf_haiku_model: &TextField,
) {
    let mut lines: Vec<Line> = vec![Line::from("")];

    // Type display (radio-style)
    let type_display = PROVIDER_TYPES
        .iter()
        .map(|t| {
            if *t == buf_type {
                format!("[{}]", t)
            } else {
                t.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("  ");

    let fields: &[(LoginField, &str, String)] = &[
        (LoginField::Name, "Name        ", buf_name.value()),
        (LoginField::Type, "Type        ", type_display.clone()),
        (LoginField::BaseUrl, "Base URL    ", buf_base_url.value()),
        (
            LoginField::ApiKey,
            "API Key     ",
            mask_api_key(&buf_api_key.value()),
        ),
        (
            LoginField::OpusModel,
            "Opus Model  ",
            buf_opus_model.value(),
        ),
        (
            LoginField::SonnetModel,
            "Sonnet Model",
            buf_sonnet_model.value(),
        ),
        (
            LoginField::HaikuModel,
            "Haiku Model ",
            buf_haiku_model.value(),
        ),
    ];

    for (field, label, raw_value) in fields {
        let is_active = field == active_field;
        let is_text_field = *field != LoginField::Type;

        // Active text field: render cursor indicator instead of static value
        let value_display = if is_active && is_text_field {
            let val = raw_value;
            let cursor_pos = match field {
                LoginField::Name => buf_name.cursor,
                LoginField::BaseUrl => buf_base_url.cursor,
                LoginField::ApiKey => buf_api_key.cursor,
                LoginField::OpusModel => buf_opus_model.cursor,
                LoginField::SonnetModel => buf_sonnet_model.cursor,
                LoginField::HaikuModel => buf_haiku_model.cursor,
                _ => 0,
            };
            let (before, after) = val.split_at(cursor_pos);
            format!("{}|{}", before, after)
        } else {
            raw_value.clone()
        };

        let (label_style, value_style) = if is_active {
            (
                Style::default()
                    .fg(theme::THINKING)
                    .add_modifier(Modifier::BOLD),
                Style::default().fg(theme::THINKING),
            )
        } else {
            (
                Style::default().fg(theme::MUTED),
                Style::default().fg(theme::TEXT),
            )
        };

        lines.push(Line::from(vec![
            Span::styled(format!("  {} ", label), label_style),
            Span::styled(" ", Style::default()),
            Span::styled(value_display, value_style),
        ]));
    }

    lines.truncate(area.height as usize);
    f.render_widget(Paragraph::new(Text::from(lines)), area);
}

fn render_confirm_delete(f: &mut Frame, area: Rect, providers: &[ProviderEntry], cursor: usize) {
    let mut list_lines: Vec<Line> = Vec::new();
    for (i, p) in providers.iter().enumerate() {
        let is_cursor = i == cursor;
        let bullet = if is_cursor { "\u{25cf}" } else { "\u{25cb}" };
        let cursor_char = if is_cursor { "\u{276f}" } else { " " };
        let row_style = if is_cursor {
            Style::default().fg(theme::THINKING)
        } else {
            Style::default().fg(theme::TEXT)
        };
        let cursor_style = Style::default().fg(theme::THINKING);
        list_lines.push(Line::from(vec![
            Span::styled(format!("{} ", cursor_char), cursor_style),
            Span::styled(format!("{} ", bullet), row_style),
            Span::styled(
                p.display_name().to_string(),
                row_style.add_modifier(Modifier::BOLD),
            ),
        ]));
    }
    list_lines.truncate(area.height.saturating_sub(3) as usize);
    f.render_widget(Paragraph::new(Text::from(list_lines)), area);

    let confirm_y = area.y + area.height.saturating_sub(2);
    let confirm_area = Rect {
        y: confirm_y,
        height: 2,
        ..area
    };
    if let Some(p) = providers.get(cursor) {
        let confirm_lines = vec![
            Line::from(""),
            Line::from(vec![
                Span::styled("Delete ", Style::default().fg(theme::TEXT)),
                Span::styled(
                    p.display_name().to_string(),
                    Style::default()
                        .fg(theme::ERROR)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("? (y/n)", Style::default().fg(theme::TEXT)),
            ]),
        ];
        f.render_widget(Paragraph::new(Text::from(confirm_lines)), confirm_area);
    }
}

fn mask_api_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    let len = chars.len();
    if len <= 8 {
        return "*".repeat(len);
    }
    let prefix: String = chars[..4].iter().collect();
    let suffix: String = chars[len - 4..].iter().collect();
    format!("{}****{}", prefix, suffix)
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
                            services: snapshot,
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

    fn tab_input() -> Input {
        Input {
            key: Key::Tab,
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

    fn char_input(c: char) -> Input {
        Input {
            key: Key::Char(c),
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    fn y_input() -> Input {
        char_input('y')
    }

    fn e_input() -> Input {
        char_input('e')
    }

    fn n_input() -> Input {
        char_input('n')
    }

    fn d_input() -> Input {
        char_input('d')
    }

    fn space_input() -> Input {
        Input {
            key: Key::Char(' '),
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    fn sample_providers() -> Vec<ProviderEntry> {
        vec![
            ProviderEntry {
                name: "Anthropic".to_string(),
                provider_type: "anthropic".to_string(),
                base_url: "https://api.anthropic.com".to_string(),
                opus_model: "claude-opus-4-7".to_string(),
                sonnet_model: "claude-sonnet-4-6".to_string(),
                haiku_model: "claude-haiku-4-5".to_string(),
            },
            ProviderEntry {
                name: "OpenAI".to_string(),
                provider_type: "openai".to_string(),
                base_url: "https://api.openai.com".to_string(),
                opus_model: "gpt-4o".to_string(),
                sonnet_model: "gpt-4o-mini".to_string(),
                haiku_model: "gpt-3.5-turbo".to_string(),
            },
        ]
    }

    #[test]
    fn test_kind_returns_correct_variant() {
        let panel = LoginPanel::empty();
        assert_eq!(panel.kind(), PanelKind::Login);
    }

    #[test]
    fn test_esc_close_from_browse() {
        let mut panel = LoginPanel::with_providers(sample_providers());
        let ctx = make_ctx();
        let effects = panel.handle_key(esc_input(), &ctx);
        assert_eq!(effects, vec![PanelEffect::Close]);
    }

    #[test]
    fn test_esc_from_edit_returns_to_browse() {
        let mut panel = LoginPanel::with_providers(sample_providers());
        let ctx = make_ctx();
        panel.enter_edit();
        assert!(matches!(panel.mode, LoginMode::Edit { .. }));

        let effects = panel.handle_key(esc_input(), &ctx);
        assert!(effects.is_empty());
        assert!(matches!(panel.mode, LoginMode::Browse));
    }

    #[test]
    fn test_e_enters_edit_mode() {
        let mut panel = LoginPanel::with_providers(sample_providers());
        let ctx = make_ctx();
        assert!(matches!(panel.mode, LoginMode::Browse));

        panel.handle_key(e_input(), &ctx);
        assert!(matches!(
            panel.mode,
            LoginMode::Edit {
                field: LoginField::Name
            }
        ));
        // Verify fields are populated from selected provider
        assert_eq!(panel.buf_name.value(), "Anthropic");
        assert_eq!(panel.buf_type, "anthropic");
    }

    #[test]
    #[test]
    fn test_enter_enters_edit_mode() {
        let mut panel = LoginPanel::with_providers(sample_providers());
        let ctx = make_ctx();
        assert!(matches!(panel.mode, LoginMode::Browse));

        panel.handle_key(enter_input(), &ctx);
        assert!(
            matches!(
                panel.mode,
                LoginMode::Edit {
                    field: LoginField::Name
                }
            ),
            "Enter should enter edit mode on selected provider"
        );
        assert_eq!(panel.buf_name.value(), "Anthropic");
        assert_eq!(panel.buf_type, "anthropic");
    }

    #[test]
    fn test_n_enters_new_mode() {
        let mut panel = LoginPanel::with_providers(sample_providers());
        let ctx = make_ctx();
        assert!(matches!(panel.mode, LoginMode::Browse));

        panel.handle_key(n_input(), &ctx);
        assert!(matches!(
            panel.mode,
            LoginMode::New {
                field: LoginField::Name
            }
        ));
        assert!(panel.buf_name.text.is_empty());
        assert_eq!(panel.buf_type, "openai");
    }

    #[test]
    fn test_delete_flow() {
        let mut panel = LoginPanel::with_providers(sample_providers());
        let ctx = make_ctx();

        // d enters confirm delete
        panel.handle_key(d_input(), &ctx);
        assert!(matches!(panel.mode, LoginMode::ConfirmDelete(_)));

        // y confirms deletion
        let effects = panel.handle_key(y_input(), &ctx);
        assert!(matches!(panel.mode, LoginMode::Browse));
        assert!(!effects.is_empty());
        let has_delete_acp = effects.iter().any(
            |e| matches!(e, PanelEffect::SendToAcp { event, .. } if event == "delete_provider"),
        );
        assert!(has_delete_acp, "y should emit delete SendToAcp");
    }

    #[test]
    fn test_tab_cycles_fields() {
        let mut panel = LoginPanel::with_providers(sample_providers());
        let ctx = make_ctx();
        panel.enter_edit();

        // Start at Name
        assert_eq!(panel.active_field(), LoginField::Name);

        // Tab -> Type
        panel.handle_key(tab_input(), &ctx);
        assert_eq!(panel.active_field(), LoginField::Type);

        // Tab -> BaseUrl
        panel.handle_key(tab_input(), &ctx);
        assert_eq!(panel.active_field(), LoginField::BaseUrl);

        // Tab -> ApiKey
        panel.handle_key(tab_input(), &ctx);
        assert_eq!(panel.active_field(), LoginField::ApiKey);

        // Tab -> OpusModel
        panel.handle_key(tab_input(), &ctx);
        assert_eq!(panel.active_field(), LoginField::OpusModel);

        // Tab -> SonnetModel
        panel.handle_key(tab_input(), &ctx);
        assert_eq!(panel.active_field(), LoginField::SonnetModel);

        // Tab -> HaikuModel
        panel.handle_key(tab_input(), &ctx);
        assert_eq!(panel.active_field(), LoginField::HaikuModel);

        // Tab wraps to Name
        panel.handle_key(tab_input(), &ctx);
        assert_eq!(panel.active_field(), LoginField::Name);
    }

    #[test]
    fn test_render_does_not_panic_browse() {
        let mut panel = LoginPanel::with_providers(sample_providers());
        let ctx = make_ctx();

        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| panel.render(f, Rect::new(0, 0, 80, 25), &ctx))
            .unwrap();
    }

    #[test]
    fn test_render_does_not_panic_edit() {
        let mut panel = LoginPanel::with_providers(sample_providers());
        panel.enter_edit();
        let ctx = make_ctx();

        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| panel.render(f, Rect::new(0, 0, 80, 25), &ctx))
            .unwrap();
    }

    #[test]
    fn test_render_does_not_panic_new() {
        let mut panel = LoginPanel::with_providers(sample_providers());
        panel.enter_new();
        let ctx = make_ctx();

        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| panel.render(f, Rect::new(0, 0, 80, 25), &ctx))
            .unwrap();
    }

    #[test]
    fn test_render_does_not_panic_confirm_delete() {
        let mut panel = LoginPanel::with_providers(sample_providers());
        panel.request_delete();
        let ctx = make_ctx();

        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| panel.render(f, Rect::new(0, 0, 80, 25), &ctx))
            .unwrap();
    }

    #[test]
    fn test_save_edit_produces_config_effects() {
        let mut panel = LoginPanel::with_providers(sample_providers());
        panel.enter_edit();
        let ctx = make_ctx();

        // Fill in the name (it's already "Anthropic" from enter_edit)
        let effects = panel.handle_key(enter_input(), &ctx);
        assert!(!effects.is_empty());

        let has_update_config = effects
            .iter()
            .any(|e| matches!(e, PanelEffect::UpdateConfig { .. }));
        assert!(has_update_config, "Enter should emit UpdateConfig effects");

        let has_close = effects.contains(&PanelEffect::Close);
        assert!(has_close, "Enter should emit Close");
    }

    #[test]
    fn test_save_new_empty_name_shows_notification() {
        let mut panel = LoginPanel::with_providers(sample_providers());
        panel.enter_new();
        let ctx = make_ctx();

        // Don't fill name, just press Enter
        let effects = panel.handle_key(enter_input(), &ctx);
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], PanelEffect::ShowNotification(_)));
    }

    #[test]
    fn test_browse_cursor_navigation() {
        let mut panel = LoginPanel::with_providers(sample_providers());
        let ctx = make_ctx();
        assert_eq!(panel.cursor, 0);

        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.cursor, 1);

        // Clamp at end
        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.cursor, 1);

        panel.handle_key(up_input(), &ctx);
        assert_eq!(panel.cursor, 0);

        // Clamp at start
        panel.handle_key(up_input(), &ctx);
        assert_eq!(panel.cursor, 0);
    }

    #[test]
    fn test_space_cycles_type_in_edit() {
        let mut panel = LoginPanel::with_providers(sample_providers());
        panel.enter_edit();
        let ctx = make_ctx();

        // Navigate to Type field
        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.active_field(), LoginField::Type);
        assert_eq!(panel.buf_type, "anthropic");

        // Space cycles type
        panel.handle_key(space_input(), &ctx);
        assert_eq!(panel.buf_type, "openai");

        panel.handle_key(space_input(), &ctx);
        assert_eq!(panel.buf_type, "anthropic");
    }

    #[test]
    fn test_type_switch_auto_fills_models() {
        let mut panel = LoginPanel::with_providers(sample_providers());
        panel.enter_new();
        let ctx = make_ctx();

        // Default is openai, check model defaults
        assert_eq!(panel.buf_opus_model.value(), "gpt-4o");
        assert_eq!(panel.buf_sonnet_model.value(), "gpt-4o-mini");
        assert_eq!(panel.buf_haiku_model.value(), "gpt-3.5-turbo");

        // Switch to Type field and cycle
        panel.handle_key(down_input(), &ctx);
        panel.handle_key(space_input(), &ctx);
        assert_eq!(panel.buf_type, "anthropic");
        assert_eq!(panel.buf_opus_model.value(), "claude-opus-4-7");
        assert_eq!(panel.buf_sonnet_model.value(), "claude-sonnet-4-6");
        assert_eq!(panel.buf_haiku_model.value(), "claude-haiku-4-5");
    }

    #[test]
    fn test_paste_filters_newlines() {
        let mut panel = LoginPanel::with_providers(sample_providers());
        panel.enter_edit();
        let ctx = make_ctx();

        // Clear the field first (enter_edit pre-fills from provider)
        panel.buf_name.clear();
        panel.handle_paste("hello\nworld\r\nfoo", &ctx);
        assert_eq!(panel.buf_name.value(), "helloworldfoo");
    }

    #[test]
    fn test_desired_height() {
        let mut panel = LoginPanel::with_providers(sample_providers());
        assert_eq!(panel.desired_height(50, 80), 14);

        panel.enter_edit();
        assert_eq!(panel.desired_height(50, 80), 20);

        panel.request_delete();
        assert_eq!(panel.desired_height(50, 80), 14);
    }

    #[test]
    fn test_status_bar_hints() {
        let panel = LoginPanel::with_providers(sample_providers());
        let lc = crate::i18n::LcRegistry::default();
        let hints = panel.status_bar_hints(&lc);
        assert_eq!(hints.len(), 5);
    }
}
