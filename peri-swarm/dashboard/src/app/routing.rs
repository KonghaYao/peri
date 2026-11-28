//! Keyboard and mouse-intent routing for [`App`].
//!
//! Routing order is modal > focused text field > content list/table > sidebar.
//! Field focus suppresses every global shortcut except Ctrl-C.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::text::Span;

use super::types::{
    ActionId, BannerActions, Effect, FieldId, Focus, Intent, Overlay, Page, Screen,
};
use super::App;
use crate::view_model::StatusBucket;

mod overlay;

impl App {
    /// Route one key press. Returns effects for the runtime.
    pub fn handle_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if key.kind != KeyEventKind::Press {
            return Vec::new();
        }
        if self.quit {
            return vec![Effect::Quit];
        }
        // Ctrl-C is the unconditional escape hatch, including from a text field.
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            self.quit = true;
            return vec![Effect::Quit];
        }
        if self.overlay.is_some() {
            return self.handle_overlay_key(key);
        }
        if let Focus::Field(field) = self.focus {
            return self.handle_field_key(field, key);
        }
        if key.code == KeyCode::Tab || key.code == KeyCode::BackTab {
            self.cycle_focus(key.code == KeyCode::BackTab);
            return Vec::new();
        }
        if key.code == KeyCode::Esc {
            if self.screen == Screen::Main
                && self.page == Page::SessionLog
                && self.focus == Focus::Content
                && !self.session_log.filters_clear()
            {
                self.session_log.search.clear();
                self.session_log.set_filter(StatusBucket::All);
                return Vec::new();
            }
            self.apply(Intent::Back);
            return Vec::new();
        }
        if matches!(key.modifiers, KeyModifiers::NONE | KeyModifiers::SHIFT) {
            if let KeyCode::Char(character) = key.code {
                match character.to_ascii_lowercase() {
                    'q' => return self.apply(Intent::Quit),
                    'r' => return self.refresh_all(),
                    'n' if self.screen == Screen::Main => {
                        self.open_create();
                        return Vec::new();
                    }
                    '/' if self.screen == Screen::Main && self.page == Page::SessionLog => {
                        self.focus = Focus::Field(FieldId::SessionSearch);
                        return Vec::new();
                    }
                    'h' | '?' => {
                        self.overlay = Some(Overlay::Help);
                        self.overlay_cursor = 0;
                        return Vec::new();
                    }
                    'm' => {
                        self.apply(Intent::ToggleMenu);
                        return Vec::new();
                    }
                    '1' if self.screen == Screen::Main && self.metrics_page_active() => {
                        return self.apply(Intent::SetMetricsWindow(
                            crate::metrics::TimeWindow::FifteenMinutes,
                        ));
                    }
                    '2' if self.screen == Screen::Main && self.metrics_page_active() => {
                        return self.apply(Intent::SetMetricsWindow(
                            crate::metrics::TimeWindow::OneHour,
                        ));
                    }
                    '3' if self.screen == Screen::Main && self.metrics_page_active() => {
                        return self.apply(Intent::SetMetricsWindow(
                            crate::metrics::TimeWindow::TwentyFourHours,
                        ));
                    }
                    'e' if self.active_banner().is_some() => {
                        self.apply(Intent::OpenErrorActions);
                        return Vec::new();
                    }
                    _ => {}
                }
            }
            if self.screen == Screen::Main
                && matches!(
                    self.page,
                    Page::AgentMetrics | Page::RequestLog | Page::Query
                )
            {
                match key.code {
                    KeyCode::PageUp if self.page == Page::Query => {
                        return self.apply(Intent::QueryPagePrev);
                    }
                    KeyCode::PageDown if self.page == Page::Query => {
                        return self.apply(Intent::QueryPageNext);
                    }
                    KeyCode::PageUp => return self.apply(Intent::MetricsPagePrev),
                    KeyCode::PageDown => return self.apply(Intent::MetricsPageNext),
                    _ => {}
                }
            }
        }
        self.handle_focus_key(key)
    }

    /// Insert pasted text into the focused field.
    pub fn handle_paste(&mut self, text: &str) {
        if self.overlay.is_some() {
            return;
        }
        if let Focus::Field(field) = self.focus {
            if let Some(target) = self.text_area_mut(field) {
                target.insert_str(text);
                if field == FieldId::QuerySql {
                    self.clear_query_validation();
                }
                return;
            }
            if let Some(target) = self.field_mut(field) {
                target.insert_str(text);
                if field == FieldId::SessionSearch {
                    self.session_log.reset_page();
                }
            }
        }
    }

    /// Apply a semantic intent.
    #[allow(clippy::too_many_lines)]
    pub fn apply(&mut self, intent: Intent) -> Vec<Effect> {
        if self.quit {
            return Vec::new();
        }
        match intent {
            Intent::SelectPage(page) => {
                self.overlay = None;
                self.select_page(page, Focus::Sidebar);
                Vec::new()
            }
            Intent::SelectMenuPage(page) => {
                if self.overlay == Some(Overlay::NarrowMenu) {
                    self.overlay = None;
                    self.select_page(page, Focus::Content);
                }
                Vec::new()
            }
            Intent::ToggleMenu => {
                self.toggle_menu();
                Vec::new()
            }
            Intent::OpenHelp => {
                self.overlay = Some(Overlay::Help);
                self.overlay_cursor = 0;
                Vec::new()
            }
            Intent::OpenSignOut => {
                self.open_sign_out();
                Vec::new()
            }
            Intent::OpenErrorActions => {
                if self
                    .active_banner()
                    .is_some_and(|banner| banner.actions != BannerActions::None)
                {
                    self.overlay = Some(Overlay::ErrorActions);
                    self.overlay_cursor = 0;
                }
                Vec::new()
            }
            Intent::CloseOverlay => {
                self.overlay = None;
                Vec::new()
            }
            Intent::Retry => self.retry(),
            Intent::OpenSettings => {
                self.open_settings();
                Vec::new()
            }
            Intent::FocusField { field, click } => {
                if self.field(field).is_some() {
                    self.focus = Focus::Field(field);
                    if let Some(click) = click {
                        if let Some(target) = self.field_mut(field) {
                            target.caret_from_column(
                                click.column,
                                click.width,
                                Self::field_is_masked(field),
                            );
                        }
                    }
                }
                Vec::new()
            }
            Intent::FocusSidebar => {
                self.focus = Focus::Sidebar;
                Vec::new()
            }
            Intent::FocusContent => {
                self.focus = Focus::Content;
                Vec::new()
            }
            Intent::FocusAction(action) => {
                self.focus = Focus::Action(action);
                Vec::new()
            }
            Intent::Activate(action) => {
                self.focus = Focus::Action(action);
                self.activate_action(action)
            }
            Intent::ScrollSidebar(delta) => {
                let max = super::types::NAV_ROW_COUNT.saturating_sub(self.sidebar_viewport);
                self.sidebar_scroll = peri_tui_components::scrollbar::bounded_offset(
                    self.sidebar_scroll,
                    delta.saturating_mul(i32::from(peri_tui_components::scrollbar::SCROLL_LINES)),
                    max,
                );
                Vec::new()
            }
            Intent::ScrollContent(delta) => {
                let offset = peri_tui_components::scrollbar::bounded_offset(
                    self.content_scroll,
                    delta.saturating_mul(i32::from(peri_tui_components::scrollbar::SCROLL_LINES)),
                    self.content_max,
                );
                self.set_content_scroll(offset);
                Vec::new()
            }
            Intent::ConfirmSignOut => self.activate_action(ActionId::ConfirmSignOut),
            Intent::CancelSignOut => self.activate_action(ActionId::CancelSignOut),
            Intent::Back => {
                self.go_back();
                Vec::new()
            }
            Intent::Quit => {
                self.quit = true;
                vec![Effect::Quit]
            }
            Intent::SetFilter(bucket) => {
                self.session_log.set_filter(bucket);
                Vec::new()
            }
            Intent::OpenSession(id) => {
                self.open_session(&id);
                Vec::new()
            }
            Intent::NewSession => {
                self.open_create();
                Vec::new()
            }
            Intent::RefreshSessions => self.refresh_sessions_now(),
            Intent::SessionPagePrev => self.show_page_prev(),
            Intent::SessionPageNext => self.show_page_next(),
            Intent::GoToSessionPage(page) => self.goto_session_page(page),
            intent @ (Intent::SetMetricsWindow(_)
            | Intent::MetricsPagePrev
            | Intent::MetricsPageNext) => self.apply_metrics_intent(intent),
            Intent::RunQuery => self.run_query(),
            Intent::QueryPagePrev => self.query_page_prev(),
            Intent::QueryPageNext => self.query_page_next(),
            Intent::SelectDetailTab(tab) => {
                self.detail_tab = tab;
                self.content_scroll = 0;
                self.content_max = 0;
                Vec::new()
            }
            Intent::AddMetadataRow => {
                if self.screen == Screen::SessionEdit {
                    self.edit.add_row();
                } else {
                    self.create.add_row();
                }
                Vec::new()
            }
            Intent::RemoveMetadataRow(index) => {
                if self.screen == Screen::SessionEdit {
                    self.edit.remove_row(index);
                } else {
                    self.create.remove_row(index);
                }
                self.focus_after_row_removal();
                Vec::new()
            }
            Intent::ToggleAdvanced => {
                self.create.toggle_advanced();
                Vec::new()
            }
            Intent::SubmitCreate => self.submit_create(),
            Intent::FocusTextArea(field) => {
                if self.text_area(field).is_some() {
                    self.focus = Focus::Field(field);
                }
                if let Some(area) = self.text_area_mut(field) {
                    area.cursor = area.text.chars().count();
                }
                Vec::new()
            }
            Intent::FocusTextAreaAt { field, click } => {
                self.focus_at_text_area(field, click);
                Vec::new()
            }
            intent @ (Intent::ToggleTimelineBlock(_)
            | Intent::LoadEarlierItems
            | Intent::JumpToLatest
            | Intent::LoadMoreTurns
            | Intent::RefreshDetail
            | Intent::SendMessage
            | Intent::AnswerApproval { .. }
            | Intent::OpenEditSession
            | Intent::SubmitEdit
            | Intent::OpenDeleteSession
            | Intent::OpenCancelTurn
            | Intent::ConfirmDeleteSession
            | Intent::CancelDeleteSession
            | Intent::ConfirmCancelTurn
            | Intent::CancelCancelTurn) => self.detail_intent(intent).unwrap_or_default(),
        }
    }

    /// Focus a multiline field and place the caret from a click.
    fn focus_at_text_area(&mut self, field: FieldId, click: super::types::TextAreaClick) {
        if self.text_area(field).is_none() {
            return;
        }
        self.focus = Focus::Field(field);
        let Some(area) = self.text_area_mut(field) else {
            return;
        };
        let width = usize::from(click.width.max(1));
        let height = usize::from(click.height.max(1));
        let wrapped = peri_tui_components::textarea::wrap_text(&area.text, area.cursor, width);
        let (start, _) = peri_tui_components::textarea::viewport_window(
            wrapped.total_visual_rows,
            wrapped.cursor_visual_row,
            height,
        );
        let index = start.saturating_add(usize::from(click.row));
        let Some(line) = wrapped.visual_lines.get(index) else {
            return;
        };
        let mut cells = 0usize;
        let mut offset = 0usize;
        for character in line.text.chars() {
            let cell_width = Span::raw(character.to_string()).width();
            if cells + cell_width > usize::from(click.column) {
                break;
            }
            cells += cell_width;
            offset += 1;
        }
        let total = area.text.chars().count();
        area.cursor = line.char_range.0.saturating_add(offset).min(total);
        area.desired_col = Some(cells);
    }

    pub(super) fn field_mut(
        &mut self,
        field: FieldId,
    ) -> Option<&mut crate::text_field::TextField> {
        match field {
            FieldId::OnboardingBaseUrl if self.screen == Screen::Onboarding => {
                Some(&mut self.onboarding.base_url)
            }
            FieldId::OnboardingApiKey if self.screen == Screen::Onboarding => {
                Some(&mut self.onboarding.api_key)
            }
            FieldId::CredentialsBaseUrl if self.screen == Screen::SettingsCredentials => {
                Some(&mut self.credentials.base_url)
            }
            FieldId::CredentialsApiKey if self.screen == Screen::SettingsCredentials => {
                Some(&mut self.credentials.api_key)
            }
            FieldId::ObservabilityBaseUrl if self.screen == Screen::SettingsObservability => {
                Some(&mut self.observability.base_url)
            }
            FieldId::ObservabilityOrg if self.screen == Screen::SettingsObservability => {
                Some(&mut self.observability.org)
            }
            FieldId::ObservabilityUser if self.screen == Screen::SettingsObservability => {
                Some(&mut self.observability.user)
            }
            FieldId::ObservabilityPassword if self.screen == Screen::SettingsObservability => {
                Some(&mut self.observability.password)
            }
            FieldId::SessionSearch
                if self.screen == Screen::Main && self.page == Page::SessionLog =>
            {
                Some(&mut self.session_log.search)
            }
            FieldId::CreateLogicalId if self.screen == Screen::SessionCreate => {
                Some(&mut self.create.logical_id)
            }
            FieldId::CreateModel if self.screen == Screen::SessionCreate => {
                Some(&mut self.create.model)
            }
            FieldId::CreateCwd if self.screen == Screen::SessionCreate => {
                Some(&mut self.create.cwd)
            }
            FieldId::CreateMetadataKey(index) if self.screen == Screen::SessionCreate => {
                self.create.metadata.get_mut(index).map(|row| &mut row.key)
            }
            FieldId::CreateMetadataValue(index) if self.screen == Screen::SessionCreate => self
                .create
                .metadata
                .get_mut(index)
                .map(|row| &mut row.value),
            FieldId::EditModel if self.screen == Screen::SessionEdit => Some(&mut self.edit.model),
            FieldId::EditMetadataKey(index) if self.screen == Screen::SessionEdit => {
                self.edit.metadata.get_mut(index).map(|row| &mut row.key)
            }
            FieldId::EditMetadataValue(index) if self.screen == Screen::SessionEdit => {
                self.edit.metadata.get_mut(index).map(|row| &mut row.value)
            }
            _ => None,
        }
    }

    /// Fields that participate in Tab order on the current screen.
    pub(super) fn focus_cycle(&self) -> Vec<Focus> {
        let mut cycle = match self.screen {
            Screen::Onboarding => vec![
                Focus::Field(FieldId::OnboardingBaseUrl),
                Focus::Field(FieldId::OnboardingApiKey),
                Focus::Action(ActionId::Submit),
            ],
            Screen::SettingsCredentials => vec![
                Focus::Field(FieldId::CredentialsBaseUrl),
                Focus::Field(FieldId::CredentialsApiKey),
                Focus::Action(ActionId::SaveCredentials),
                Focus::Action(ActionId::Back),
            ],
            Screen::SettingsObservability => vec![
                Focus::Field(FieldId::ObservabilityBaseUrl),
                Focus::Field(FieldId::ObservabilityOrg),
                Focus::Field(FieldId::ObservabilityUser),
                Focus::Field(FieldId::ObservabilityPassword),
                Focus::Action(ActionId::TestObservability),
                Focus::Action(ActionId::SaveObservability),
                Focus::Action(ActionId::CancelObservability),
            ],
            Screen::Main if self.page == Page::Settings => vec![
                Focus::Sidebar,
                Focus::Action(ActionId::TestConnection),
                Focus::Action(ActionId::EditCredentials),
                Focus::Action(ActionId::ConfigureObservability),
                Focus::Action(ActionId::SignOut),
                Focus::Action(ActionId::Quit),
            ],
            Screen::Main if self.page == Page::SessionLog => vec![
                Focus::Sidebar,
                Focus::Field(FieldId::SessionSearch),
                Focus::Action(ActionId::NewSession),
                Focus::Action(ActionId::RefreshSessions),
                Focus::Content,
            ],
            Screen::Main if self.page == Page::Query => vec![
                Focus::Sidebar,
                Focus::Field(FieldId::QuerySql),
                Focus::Action(ActionId::RunQuery),
                Focus::Content,
            ],
            Screen::Main => vec![Focus::Sidebar, Focus::Content],
            Screen::SessionDetail => {
                let mut cycle = vec![
                    Focus::Action(ActionId::Back),
                    Focus::Field(FieldId::DetailComposer),
                    Focus::Action(ActionId::SendMessage),
                    Focus::Action(ActionId::RefreshDetail),
                ];
                if self.detail.timeline().ever_loaded() {
                    cycle.push(Focus::Action(ActionId::LoadEarlierItems));
                    cycle.push(Focus::Action(ActionId::JumpToLatest));
                }
                cycle.push(Focus::Action(ActionId::LoadMoreTurns));
                if self.detail.session().is_some() {
                    cycle.push(Focus::Action(ActionId::EditSession));
                    cycle.push(Focus::Action(ActionId::DeleteSession));
                }
                if self.detail.is_running() {
                    cycle.push(Focus::Action(ActionId::CancelTurn));
                }
                if !self.detail.approvals().is_empty() {
                    cycle.push(Focus::Action(ActionId::ApproveFirst));
                    cycle.push(Focus::Action(ActionId::DenyFirst));
                }
                cycle.push(Focus::Content);
                cycle
            }
            Screen::SessionEdit => {
                let mut cycle = vec![Focus::Field(FieldId::EditModel)];
                for index in 0..self.edit.metadata().len() {
                    cycle.push(Focus::Field(FieldId::EditMetadataKey(index)));
                    cycle.push(Focus::Field(FieldId::EditMetadataValue(index)));
                }
                cycle.push(Focus::Action(ActionId::AddMetadata));
                cycle.push(Focus::Field(FieldId::EditPromptCache));
                cycle.push(Focus::Action(ActionId::EditSubmit));
                if self.detail.is_running() {
                    cycle.push(Focus::Action(ActionId::CancelTurn));
                }
                cycle.push(Focus::Action(ActionId::Back));
                cycle
            }
            Screen::SessionCreate => {
                let mut cycle = vec![
                    Focus::Field(FieldId::CreateLogicalId),
                    Focus::Field(FieldId::CreateModel),
                    Focus::Field(FieldId::CreateCwd),
                    Focus::Field(FieldId::CreateInstructions),
                ];
                for index in 0..self.create.metadata.len() {
                    cycle.push(Focus::Field(FieldId::CreateMetadataKey(index)));
                    cycle.push(Focus::Field(FieldId::CreateMetadataValue(index)));
                }
                cycle.push(Focus::Action(ActionId::AddMetadata));
                cycle.push(Focus::Action(ActionId::ToggleAdvanced));
                if self.create.advanced_open {
                    cycle.push(Focus::Field(FieldId::CreateTools));
                    cycle.push(Focus::Field(FieldId::CreateEnvironment));
                    cycle.push(Focus::Field(FieldId::CreatePromptCache));
                }
                cycle.push(Focus::Action(ActionId::CreateSubmit));
                cycle.push(Focus::Action(ActionId::Back));
                cycle
            }
        };
        cycle.retain(|focus| match focus {
            Focus::Action(action) => self.action_enabled(*action),
            _ => true,
        });
        cycle
    }

    pub(super) fn cycle_focus(&mut self, backwards: bool) {
        let cycle = self.focus_cycle();
        if cycle.is_empty() {
            return;
        }
        let index = cycle
            .iter()
            .position(|focus| *focus == self.focus)
            .unwrap_or(0);
        let next = if backwards {
            index.checked_sub(1).unwrap_or(cycle.len() - 1)
        } else {
            (index + 1) % cycle.len()
        };
        self.focus = cycle[next];
    }

    /// Whether the active page has a selectable time window.
    fn metrics_page_active(&self) -> bool {
        matches!(
            self.page,
            Page::CoreMetrics | Page::AgentMetrics | Page::RequestLog | Page::Query
        )
    }

    pub(super) fn select_page(&mut self, page: Page, focus: Focus) {
        self.screen = Screen::Main;
        self.page = page;
        self.focus = focus;
        self.ensure_sidebar_visible();
    }

    pub(super) fn go_back(&mut self) {
        match self.screen {
            Screen::SettingsCredentials => self.back_to_settings(),
            Screen::SettingsObservability => self.back_to_settings(),
            Screen::SessionEdit => {
                self.edit = super::EditFormState::default();
                self.screen = Screen::SessionDetail;
                self.focus = Focus::Content;
                self.detail.reload_wanted = true;
                self.content_scroll = 0;
            }
            Screen::SessionDetail | Screen::SessionCreate => {
                self.screen = Screen::Main;
                self.page = Page::SessionLog;
                self.focus = Focus::Content;
                self.content_scroll = 0;
            }
            Screen::Onboarding | Screen::Main => {
                self.focus = Focus::Content;
            }
        }
    }

    fn ensure_sidebar_visible(&mut self) {
        if self.sidebar_viewport == 0 || self.sidebar_viewport >= super::types::NAV_ROW_COUNT {
            self.sidebar_scroll = 0;
            return;
        }
        let row = super::types::page_row_index(self.page);
        let max = super::types::NAV_ROW_COUNT - self.sidebar_viewport;
        self.sidebar_scroll = self.sidebar_scroll.min(max);
        if row < self.sidebar_scroll {
            self.sidebar_scroll = row.min(max);
        } else if row >= self.sidebar_scroll + self.sidebar_viewport {
            self.sidebar_scroll = (row + 1 - self.sidebar_viewport).min(max);
        }
    }

    fn toggle_menu(&mut self) {
        if self.overlay == Some(Overlay::NarrowMenu) {
            self.overlay = None;
            return;
        }
        if self.screen != Screen::Main {
            return;
        }
        self.overlay = Some(Overlay::NarrowMenu);
        self.overlay_cursor = Page::ALL
            .iter()
            .position(|page| *page == self.page)
            .unwrap_or(0);
    }

    fn handle_field_key(&mut self, field: FieldId, key: KeyEvent) -> Vec<Effect> {
        // Editing the search returns to the first page of the narrower set.
        let before =
            (field == FieldId::SessionSearch).then(|| self.session_log.search.value().to_owned());
        // A validation message describes the previous text; clear it on edits,
        // but not when Ctrl-Enter just produced a new one.
        let before_sql = (field == FieldId::QuerySql).then(|| self.query.sql.text.clone());
        let effects = self.handle_field_input(field, key);
        if before.is_some_and(|before| before != self.session_log.search.value()) {
            self.session_log.reset_page();
        }
        if before_sql.is_some_and(|before| before != self.query.sql.text) {
            self.clear_query_validation();
        }
        effects
    }

    fn handle_field_input(&mut self, field: FieldId, key: KeyEvent) -> Vec<Effect> {
        if key.code == KeyCode::Tab || key.code == KeyCode::BackTab {
            self.cycle_focus(key.code == KeyCode::BackTab);
            return Vec::new();
        }
        if self.text_area(field).is_some() {
            return self.handle_text_area_key(field, key);
        }
        if key.code == KeyCode::Esc {
            if self.screen == Screen::SessionCreate || self.screen == Screen::SessionEdit {
                self.go_back();
            } else {
                self.focus = Focus::Content;
            }
            return Vec::new();
        }
        if key.code == KeyCode::Enter {
            let action = match self.screen {
                Screen::Onboarding => Some(ActionId::Submit),
                Screen::SettingsCredentials => Some(ActionId::SaveCredentials),
                Screen::SettingsObservability => Some(ActionId::SaveObservability),
                Screen::SessionEdit => Some(ActionId::EditSubmit),
                _ => None,
            };
            if let Some(action) = action {
                self.focus = Focus::Action(action);
                return self.activate_action(action);
            }
            if self.screen == Screen::Main && self.page == Page::SessionLog {
                self.focus = Focus::Content;
            }
            return Vec::new();
        }
        if matches!(key.modifiers, KeyModifiers::NONE | KeyModifiers::SHIFT) {
            if let KeyCode::Char(character) = key.code {
                if let Some(target) = self.field_mut(field) {
                    target.insert_str(&character.to_string());
                }
                return Vec::new();
            }
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let Some(target) = self.field_mut(field) else {
            return Vec::new();
        };
        if control {
            match key.code {
                KeyCode::Char('a') | KeyCode::Char('A') if !alt => {
                    target.select_all();
                    return Vec::new();
                }
                KeyCode::Char('z') | KeyCode::Char('Z') if !shift && !alt => {
                    target.undo();
                    return Vec::new();
                }
                KeyCode::Char('z' | 'Z' | 'y' | 'Y') if !alt => {
                    target.redo();
                    return Vec::new();
                }
                _ => {}
            }
        }
        let word = control || alt;
        match key.code {
            KeyCode::Backspace if word => target.delete_word_left(),
            KeyCode::Backspace => target.backspace(),
            KeyCode::Delete if word => target.delete_word_right(),
            KeyCode::Delete => target.delete_forward(),
            KeyCode::Left if word && shift => target.extend_word_left(),
            KeyCode::Left if word => target.move_word_left(),
            KeyCode::Left if shift => target.extend_left(),
            KeyCode::Left => target.move_left(),
            KeyCode::Right if word && shift => target.extend_word_right(),
            KeyCode::Right if word => target.move_word_right(),
            KeyCode::Right if shift => target.extend_right(),
            KeyCode::Right => target.move_right(),
            KeyCode::Home if shift => target.extend_home(),
            KeyCode::Home => target.move_home(),
            KeyCode::End if shift => target.extend_end(),
            KeyCode::End => target.move_end(),
            _ => {}
        }
        Vec::new()
    }

    fn handle_text_area_key(&mut self, field: FieldId, key: KeyEvent) -> Vec<Effect> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Enter {
            return match self.screen {
                Screen::SessionCreate => self.activate_action(ActionId::CreateSubmit),
                Screen::SessionDetail => self.activate_action(ActionId::SendMessage),
                Screen::SessionEdit => self.activate_action(ActionId::EditSubmit),
                Screen::Main if self.page == Page::Query => {
                    self.activate_action(ActionId::RunQuery)
                }
                _ => Vec::new(),
            };
        }
        if key.code == KeyCode::Esc {
            if self.screen == Screen::SessionCreate || self.screen == Screen::SessionEdit {
                self.go_back();
            } else {
                self.focus = Focus::Content;
            }
            return Vec::new();
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let Some(area) = self.text_area_mut(field) else {
            return Vec::new();
        };
        if control {
            match key.code {
                KeyCode::Char('a') | KeyCode::Char('A') if !alt => {
                    area.select_all();
                    return Vec::new();
                }
                KeyCode::Char('z') | KeyCode::Char('Z') if !shift && !alt => {
                    area.undo();
                    return Vec::new();
                }
                KeyCode::Char('z' | 'Z' | 'y' | 'Y') if !alt => {
                    area.redo();
                    return Vec::new();
                }
                _ => {}
            }
        }
        let word = control || alt;
        match key.code {
            KeyCode::Enter => area.insert_char('\n'),
            KeyCode::Backspace if word => area.delete_word_backward(),
            KeyCode::Backspace => area.backspace(),
            KeyCode::Delete if word => area.delete_word_forward(),
            KeyCode::Delete => area.delete_forward(),
            KeyCode::Left if word && shift => area.cursor_word_left_extend(),
            KeyCode::Left if word => area.cursor_word_left(),
            KeyCode::Left if shift => area.cursor_left_extend(),
            KeyCode::Left => area.cursor_left(),
            KeyCode::Right if word && shift => area.cursor_word_right_extend(),
            KeyCode::Right if word => area.cursor_word_right(),
            KeyCode::Right if shift => area.cursor_right_extend(),
            KeyCode::Right => area.cursor_right(),
            KeyCode::Up if shift => {
                area.cursor_line_up_extend();
            }
            KeyCode::Up => {
                area.cursor_line_up();
            }
            KeyCode::Down if shift => {
                area.cursor_line_down_extend();
            }
            KeyCode::Down => {
                area.cursor_line_down();
            }
            KeyCode::Home if control => area.cursor_home(),
            KeyCode::Home if shift => area.cursor_line_home_extend(),
            KeyCode::Home => area.cursor_line_home(),
            KeyCode::End if control => area.cursor_end(),
            KeyCode::End if shift => area.cursor_line_end_extend(),
            KeyCode::End => area.cursor_line_end(),
            KeyCode::Char(character)
                if matches!(key.modifiers, KeyModifiers::NONE | KeyModifiers::SHIFT) =>
            {
                area.insert_char(character);
            }
            _ => {}
        }
        Vec::new()
    }

    fn handle_focus_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        match self.focus {
            Focus::Sidebar => match key.code {
                KeyCode::Up => {
                    let previous = super::types::previous_page(self.page);
                    self.select_page(previous, Focus::Sidebar);
                    Vec::new()
                }
                KeyCode::Down => {
                    let next = super::types::next_page(self.page);
                    self.select_page(next, Focus::Sidebar);
                    Vec::new()
                }
                KeyCode::Right | KeyCode::Enter => {
                    self.focus = Focus::Content;
                    Vec::new()
                }
                _ => Vec::new(),
            },
            Focus::Content => match key.code {
                KeyCode::Left if self.screen == Screen::SessionDetail => {
                    self.cycle_detail_tab(-1);
                    Vec::new()
                }
                KeyCode::Right if self.screen == Screen::SessionDetail => {
                    self.cycle_detail_tab(1);
                    Vec::new()
                }
                KeyCode::Left => {
                    self.focus = Focus::Sidebar;
                    Vec::new()
                }
                KeyCode::Up if self.screen == Screen::Main && self.page == Page::SessionLog => {
                    self.session_log.move_cursor(-1);
                    Vec::new()
                }
                KeyCode::Down if self.screen == Screen::Main && self.page == Page::SessionLog => {
                    self.session_log.move_cursor(1);
                    Vec::new()
                }
                KeyCode::PageUp if self.screen == Screen::Main && self.page == Page::SessionLog => {
                    self.show_page_prev()
                }
                KeyCode::PageDown
                    if self.screen == Screen::Main && self.page == Page::SessionLog =>
                {
                    self.show_page_next()
                }
                KeyCode::Enter if self.screen == Screen::Main && self.page == Page::SessionLog => {
                    match self.session_log.cursor_session_id() {
                        Some(id) => {
                            self.open_session(&id);
                            Vec::new()
                        }
                        None => Vec::new(),
                    }
                }
                _ => Vec::new(),
            },
            Focus::Action(action) => match key.code {
                KeyCode::Enter | KeyCode::Char(' ') => self.activate_action(action),
                KeyCode::Down => {
                    self.cycle_focus(false);
                    Vec::new()
                }
                KeyCode::Up => {
                    self.cycle_focus(true);
                    Vec::new()
                }
                _ => Vec::new(),
            },
            Focus::Field(_) => Vec::new(),
        }
    }
}
