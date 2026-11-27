//! Application state machine.
//!
//! Everything here is pure state plus config-file effects: no terminal, no
//! network and no threads. Key and mouse input become [`Intent`] values, and
//! state changes return [`Effect`] values the runtime executes. Rendering lives
//! in [`crate::ui`] and only reads this state.

use std::time::{Duration, Instant};

use crate::api;
use crate::config::{Config, ConfigStore, ConfigWarning};
use crate::poll::HEALTH_INTERVAL;
use crate::text_field::TextField;
use crate::ui::text;
use crate::worker::ValidationPurpose;

mod actions;
mod detail;
mod forms;
mod routing;
mod session_create;
mod session_detail;
mod session_edit;
mod session_flow;
mod session_log;
mod types;

pub use forms::{CredentialsState, OnboardingState};
pub use session_create::{CreateFormState, MetadataRow};
pub use session_detail::{
    pretty_json, ActionError, ApprovalEntry, RequiredAction, SessionDetailState, TimelineEntry,
    TimelineKind, TurnsState,
};
pub use session_edit::EditFormState;
pub use session_log::{SessionLogState, LIST_MAX, LIST_PAGE_SIZE};
pub use types::{
    next_page, page_row_index, previous_page, ActionId, BannerActions, BannerTone, BannerView,
    DetailTab, Effect, FieldClick, FieldId, Focus, HealthState, Intent, NavItem, Overlay, Page,
    Screen, SettingsStatus, SidebarGroup, TextAreaClick, NAV_ENTRIES, NAV_ROW_COUNT,
};

use forms::PendingCredentials;

/// Whole dashboard state.
pub struct App {
    screen: Screen,
    page: Page,
    focus: Focus,
    overlay: Option<Overlay>,
    overlay_cursor: usize,
    onboarding: OnboardingState,
    credentials: CredentialsState,
    settings_status: Option<SettingsStatus>,
    config: Option<Config>,
    store: ConfigStore,
    config_warning: Option<String>,
    pending: Option<PendingCredentials>,
    validation: Option<ValidationPurpose>,
    banner: Option<BannerView>,
    contract: Option<String>,
    health: HealthState,
    last_refresh: Option<Instant>,
    last_latency: Option<Duration>,
    last_health_request: Option<Instant>,
    health_in_flight: bool,
    paused: bool,
    detail: SessionDetailState,
    detail_tab: DetailTab,
    edit: EditFormState,
    session_log: SessionLogState,
    create: CreateFormState,
    viewport_width: u16,
    sidebar_scroll: usize,
    sidebar_viewport: usize,
    content_scroll: usize,
    content_max: usize,
    epoch: u64,
    quit: bool,
}

impl App {
    /// First-run onboarding with default values.
    #[must_use]
    pub fn onboarding(store: ConfigStore) -> Self {
        Self::onboarding_with_error(store, None)
    }

    /// First-run onboarding, optionally showing a config read error.
    #[must_use]
    pub fn onboarding_with_error(store: ConfigStore, error: Option<String>) -> Self {
        let mut app = Self {
            screen: Screen::Onboarding,
            page: Page::SessionLog,
            focus: Focus::Field(FieldId::OnboardingBaseUrl),
            overlay: None,
            overlay_cursor: 0,
            onboarding: OnboardingState::default(),
            credentials: CredentialsState::default(),
            settings_status: None,
            config: None,
            store,
            config_warning: None,
            pending: None,
            validation: None,
            banner: None,
            contract: None,
            health: HealthState::Unknown,
            last_refresh: None,
            last_latency: None,
            last_health_request: None,
            health_in_flight: false,
            paused: false,
            detail: SessionDetailState::default(),
            detail_tab: DetailTab::Timeline,
            edit: EditFormState::default(),
            session_log: SessionLogState::new(),
            create: CreateFormState::default(),
            viewport_width: 0,
            sidebar_scroll: 0,
            sidebar_viewport: 0,
            content_scroll: 0,
            content_max: 0,
            epoch: 0,
            quit: false,
        };
        if let Some(error) = error {
            app.onboarding.form_error = Some(text::config_read_failed(&error));
        }
        app
    }

    /// Dashboard entered with already-loaded credentials.
    #[must_use]
    pub fn connected(config: Config, warnings: Vec<ConfigWarning>, store: ConfigStore) -> Self {
        let config_warning = warnings.first().map(|warning| match warning {
            ConfigWarning::PermissionsTooOpen { mode } => text::permissions_warning(*mode),
        });
        Self {
            screen: Screen::Main,
            page: Page::SessionLog,
            focus: Focus::Content,
            overlay: None,
            overlay_cursor: 0,
            onboarding: OnboardingState::default(),
            credentials: CredentialsState::default(),
            settings_status: None,
            config: Some(config),
            store,
            config_warning,
            pending: None,
            validation: None,
            banner: None,
            contract: None,
            health: HealthState::Unknown,
            last_refresh: None,
            last_latency: None,
            last_health_request: None,
            health_in_flight: false,
            paused: false,
            detail: SessionDetailState::default(),
            detail_tab: DetailTab::Timeline,
            edit: EditFormState::default(),
            session_log: SessionLogState::new(),
            create: CreateFormState::default(),
            viewport_width: 0,
            sidebar_scroll: 0,
            sidebar_viewport: 0,
            content_scroll: 0,
            content_max: 0,
            epoch: 0,
            quit: false,
        }
    }

    // -----------------------------------------------------------------------
    // Read accessors
    // -----------------------------------------------------------------------

    /// Active screen.
    #[must_use]
    pub fn screen(&self) -> Screen {
        self.screen
    }

    /// Active page.
    #[must_use]
    pub fn page(&self) -> Page {
        self.page
    }

    /// Keyboard focus.
    #[must_use]
    pub fn focus(&self) -> Focus {
        self.focus
    }

    /// Active overlay.
    #[must_use]
    pub fn overlay(&self) -> Option<Overlay> {
        self.overlay
    }

    /// Cursor inside the active overlay.
    #[must_use]
    pub fn overlay_cursor(&self) -> usize {
        self.overlay_cursor
    }

    /// Validation in flight, if any.
    #[must_use]
    pub fn validation_purpose(&self) -> Option<ValidationPurpose> {
        self.validation
    }

    /// Onboarding form.
    #[must_use]
    pub fn onboarding_form(&self) -> &OnboardingState {
        &self.onboarding
    }

    /// Credential editor form.
    #[must_use]
    pub fn credentials(&self) -> &CredentialsState {
        &self.credentials
    }

    /// Last settings action result.
    #[must_use]
    pub fn settings_status(&self) -> Option<&SettingsStatus> {
        self.settings_status.as_ref()
    }

    /// Loaded base URL, when connected.
    #[must_use]
    pub fn config_base_url(&self) -> Option<&str> {
        self.config.as_ref().map(Config::base_url)
    }

    /// Whether credentials are loaded.
    #[must_use]
    pub fn has_config(&self) -> bool {
        self.config.is_some()
    }

    /// Current health state.
    #[must_use]
    pub fn health(&self) -> &HealthState {
        &self.health
    }

    /// Last successful health poll.
    #[must_use]
    pub fn last_refresh(&self) -> Option<Instant> {
        self.last_refresh
    }

    /// Last successful health latency.
    #[must_use]
    pub fn last_latency(&self) -> Option<Duration> {
        self.last_latency
    }

    /// Last observed contract header.
    #[must_use]
    pub fn contract(&self) -> Option<&str> {
        self.contract.as_deref()
    }

    /// Whether the gateway contract differs from the expected version.
    #[must_use]
    pub fn contract_mismatch(&self) -> bool {
        self.contract
            .as_deref()
            .is_some_and(|value| value != api::CONTRACT_VERSION)
    }

    /// Whether authenticated polling is paused by a 401.
    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Detail route id (the open session, if any).
    #[must_use]
    pub fn detail_id(&self) -> Option<&str> {
        self.detail.id()
    }

    /// Detail page state.
    #[must_use]
    pub fn detail(&self) -> &SessionDetailState {
        &self.detail
    }

    /// Edit form state.
    #[must_use]
    pub fn edit_form(&self) -> &EditFormState {
        &self.edit
    }

    /// Active detail tab.
    #[must_use]
    pub fn detail_tab(&self) -> DetailTab {
        self.detail_tab
    }

    /// Whether the detail timeline currently follows the newest items.
    ///
    /// Only the timeline tab defines following: the other tabs have unrelated
    /// content heights and must not move the timeline's anchor.
    #[must_use]
    pub fn detail_timeline_following(&self) -> bool {
        self.screen == Screen::SessionDetail
            && self.detail_tab == DetailTab::Timeline
            && self.detail.timeline().following()
    }

    /// Whether new timeline items arrived while the view was scrolled up.
    #[must_use]
    pub fn timeline_unseen(&self) -> bool {
        self.detail.timeline().unseen()
    }

    /// Session log state (loaded page, filter, search, polling).
    #[must_use]
    pub fn session_log(&self) -> &SessionLogState {
        &self.session_log
    }

    /// Create-session form state.
    #[must_use]
    pub fn create_form(&self) -> &CreateFormState {
        &self.create
    }

    /// Terminal width of the most recently completed frame.
    #[must_use]
    pub fn viewport_width(&self) -> u16 {
        self.viewport_width
    }

    /// Record the terminal width of the frame being rendered.
    pub fn set_viewport_width(&mut self, width: u16) {
        self.viewport_width = width;
    }

    /// Whether the runtime should exit.
    #[must_use]
    pub fn should_quit(&self) -> bool {
        self.quit
    }

    /// Largest content wheel offset for the most recent frame.
    #[must_use]
    pub fn content_max(&self) -> usize {
        self.content_max
    }

    /// Sidebar wheel offset.
    #[must_use]
    pub fn sidebar_scroll(&self) -> usize {
        self.sidebar_scroll
    }

    /// Content wheel offset.
    #[must_use]
    pub fn content_scroll(&self) -> usize {
        self.content_scroll
    }

    /// UI epoch used to discard stale worker responses.
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Base URL shown in the header.
    #[must_use]
    pub fn header_base_url(&self) -> String {
        if let Some(base_url) = self.config_base_url() {
            return base_url.to_owned();
        }
        let candidate = self.onboarding.base_url().value().trim();
        if candidate.is_empty() {
            text::NOT_CONFIGURED.to_owned()
        } else {
            candidate.to_owned()
        }
    }

    /// Banner the shell should show, including derived warnings.
    #[must_use]
    pub fn active_banner(&self) -> Option<BannerView> {
        if let Some(banner) = &self.banner {
            return Some(banner.clone());
        }
        if let Some(actual) = self
            .contract
            .as_deref()
            .filter(|value| *value != api::CONTRACT_VERSION)
        {
            return Some(BannerView {
                tone: BannerTone::Warning,
                code: Some(actual.to_owned()),
                message: text::contract_mismatch(actual, api::CONTRACT_VERSION),
                actions: BannerActions::Settings,
                unauthorized: false,
            });
        }
        self.config_warning.as_ref().map(|message| BannerView {
            tone: BannerTone::Warning,
            code: None,
            message: message.clone(),
            actions: BannerActions::None,
            unauthorized: false,
        })
    }

    /// Field for a focus target, when it belongs to the active screen.
    #[must_use]
    pub fn field(&self, field: FieldId) -> Option<&TextField> {
        match field {
            FieldId::OnboardingBaseUrl if self.screen == Screen::Onboarding => {
                Some(self.onboarding.base_url())
            }
            FieldId::OnboardingApiKey if self.screen == Screen::Onboarding => {
                Some(self.onboarding.api_key())
            }
            FieldId::CredentialsBaseUrl if self.screen == Screen::SettingsCredentials => {
                Some(self.credentials.base_url())
            }
            FieldId::CredentialsApiKey if self.screen == Screen::SettingsCredentials => {
                Some(self.credentials.api_key())
            }
            FieldId::SessionSearch
                if self.screen == Screen::Main && self.page == Page::SessionLog =>
            {
                Some(self.session_log.search())
            }
            FieldId::CreateLogicalId if self.screen == Screen::SessionCreate => {
                Some(self.create.logical_id())
            }
            FieldId::CreateModel if self.screen == Screen::SessionCreate => {
                Some(self.create.model())
            }
            FieldId::CreateCwd if self.screen == Screen::SessionCreate => Some(self.create.cwd()),
            FieldId::CreateMetadataKey(index) if self.screen == Screen::SessionCreate => {
                self.create.metadata.get(index).map(MetadataRow::key)
            }
            FieldId::CreateMetadataValue(index) if self.screen == Screen::SessionCreate => {
                self.create.metadata.get(index).map(MetadataRow::value)
            }
            FieldId::EditModel if self.screen == Screen::SessionEdit => Some(self.edit.model()),
            FieldId::EditMetadataKey(index) if self.screen == Screen::SessionEdit => {
                self.edit.metadata.get(index).map(MetadataRow::key)
            }
            FieldId::EditMetadataValue(index) if self.screen == Screen::SessionEdit => {
                self.edit.metadata.get(index).map(MetadataRow::value)
            }
            _ => None,
        }
    }

    /// Multiline field for a focus target, when it belongs to the active screen.
    #[must_use]
    pub fn text_area(
        &self,
        field: FieldId,
    ) -> Option<&peri_tui_components::textarea::TextAreaState> {
        match (self.screen, field) {
            (Screen::SessionCreate, FieldId::CreateInstructions) => {
                Some(self.create.instructions())
            }
            (Screen::SessionCreate, FieldId::CreateTools) => Some(self.create.tools()),
            (Screen::SessionCreate, FieldId::CreateEnvironment) => Some(self.create.environment()),
            (Screen::SessionCreate, FieldId::CreatePromptCache) => {
                Some(self.create.prompt_cache_options())
            }
            (Screen::SessionDetail, FieldId::DetailComposer) => Some(self.detail.composer()),
            (Screen::SessionEdit, FieldId::EditPromptCache) => {
                Some(self.edit.prompt_cache_options())
            }
            _ => None,
        }
    }

    /// Multiline field for a focus target, mutably.
    pub fn text_area_mut(
        &mut self,
        field: FieldId,
    ) -> Option<&mut peri_tui_components::textarea::TextAreaState> {
        match (self.screen, field) {
            (Screen::SessionCreate, FieldId::CreateInstructions) => {
                Some(&mut self.create.instructions)
            }
            (Screen::SessionCreate, FieldId::CreateTools) => Some(&mut self.create.tools),
            (Screen::SessionCreate, FieldId::CreateEnvironment) => {
                Some(&mut self.create.environment)
            }
            (Screen::SessionCreate, FieldId::CreatePromptCache) => {
                Some(&mut self.create.prompt_cache_options)
            }
            (Screen::SessionDetail, FieldId::DetailComposer) => Some(&mut self.detail.composer),
            (Screen::SessionEdit, FieldId::EditPromptCache) => {
                Some(&mut self.edit.prompt_cache_options)
            }
            _ => None,
        }
    }

    /// Whether a field is a secret and must render masked.
    #[must_use]
    pub fn field_is_masked(field: FieldId) -> bool {
        matches!(
            field,
            FieldId::OnboardingApiKey | FieldId::CredentialsApiKey
        )
    }

    /// Replace a field value (used by tests and form prefills).
    pub fn set_field_value(&mut self, field: FieldId, value: &str) -> bool {
        if let Some(target) = self.field_mut(field) {
            target.set_value(value);
            true
        } else {
            false
        }
    }

    /// Replace a multiline field value and move the caret to the end.
    pub fn set_text_area_value(&mut self, field: FieldId, value: &str) -> bool {
        if let Some(target) = self.text_area_mut(field) {
            target.text = value.to_owned();
            target.cursor = target.text.chars().count();
            true
        } else {
            false
        }
    }

    /// Record the sidebar rows available in the most recent frame.
    pub fn set_sidebar_viewport(&mut self, rows: u16) {
        self.sidebar_viewport = usize::from(rows);
        let max = NAV_ROW_COUNT.saturating_sub(self.sidebar_viewport);
        self.sidebar_scroll = self.sidebar_scroll.min(max);
    }

    /// Set the sidebar wheel offset, clamped to the current viewport.
    pub fn set_sidebar_scroll(&mut self, offset: usize) {
        let max = NAV_ROW_COUNT.saturating_sub(self.sidebar_viewport);
        self.sidebar_scroll = offset.min(max);
    }

    /// Record the body rows, content height and effective scroll offset of the
    /// most recent frame.
    pub fn set_content_layout(&mut self, viewport: u16, content: u16, scroll: usize) {
        self.content_max = usize::from(content).saturating_sub(usize::from(viewport));
        self.content_scroll = scroll.min(self.content_max);
        self.record_timeline_following();
    }

    /// Set the content wheel offset, clamped to the last measured content.
    ///
    /// Landing on the current bottom restores the follow intent immediately, so
    /// a page update that arrives before the next frame re-anchors instead of
    /// reporting unseen items.
    pub fn set_content_scroll(&mut self, offset: usize) {
        self.content_scroll = offset.min(self.content_max);
        self.record_timeline_following();
    }

    /// Keep the timeline follow intent in sync with the content offset.
    fn record_timeline_following(&mut self) {
        if self.screen == Screen::SessionDetail && self.detail_tab == DetailTab::Timeline {
            let at_bottom = self.content_scroll >= self.content_max;
            self.detail.set_following(at_bottom);
        }
    }

    /// Health poll scheduling. Returns an effect when a poll is due.
    pub fn health_poll_due(&mut self, now: Instant) -> Option<Effect> {
        if self.config.is_none()
            || self.health_in_flight
            || self.screen == Screen::Onboarding
            || self.screen == Screen::SettingsCredentials
            || self.overlay == Some(Overlay::ConfirmSignOut)
        {
            return None;
        }
        if let Some(last) = self.last_health_request {
            if now.saturating_duration_since(last) < HEALTH_INTERVAL {
                return None;
            }
        }
        self.last_health_request = Some(now);
        self.health_in_flight = true;
        Some(Effect::Health { epoch: self.epoch })
    }
}

#[cfg(test)]
#[path = "app_test.rs"]
mod tests;
