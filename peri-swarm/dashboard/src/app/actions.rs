//! Actions, credential flows and worker-result handling for [`App`].

use std::time::Instant;

use super::forms::{CredentialsState, OnboardingState, PendingCredentials};
use super::types::{
    ActionId, BannerActions, BannerTone, BannerView, Effect, FieldId, HealthState, Page, Screen,
    SettingsStatus,
};
use super::App;
use crate::api::ApiError;
use crate::config::{self, Config};
use crate::ui::text;
use crate::worker::{
    HealthReport, ValidationFailure, ValidationPurpose, ValidationStage, WorkerEvent,
};

impl App {
    /// Apply a worker event. Stale epochs and purposes are discarded.
    pub fn handle_worker(&mut self, event: WorkerEvent) -> Vec<Effect> {
        match event {
            WorkerEvent::ValidationFinished {
                epoch,
                purpose,
                result,
            } => {
                if epoch != self.epoch || self.validation != Some(purpose) {
                    return Vec::new();
                }
                self.validation = None;
                match (purpose, result) {
                    (ValidationPurpose::Onboarding, Ok(_)) => self.finish_onboarding(),
                    (ValidationPurpose::Onboarding, Err(failure)) => {
                        self.pending = None;
                        self.onboarding.form_error = Some(validation_message(&failure));
                        Vec::new()
                    }
                    (ValidationPurpose::Credentials, Ok(_)) => self.finish_credentials(),
                    (ValidationPurpose::Credentials, Err(failure)) => {
                        self.pending = None;
                        self.credentials.form_error = Some(validation_message(&failure));
                        Vec::new()
                    }
                    (ValidationPurpose::TestConnection, Ok(elapsed)) => {
                        self.settings_status = Some(SettingsStatus::Ok(text::test_ok(elapsed)));
                        Vec::new()
                    }
                    (ValidationPurpose::TestConnection, Err(failure)) => {
                        self.settings_status =
                            Some(SettingsStatus::Error(validation_message(&failure)));
                        Vec::new()
                    }
                    (ValidationPurpose::BannerRetry, Ok(_)) => {
                        self.paused = false;
                        self.banner = None;
                        self.session_log.poll.resume();
                        self.session_log.poll.reset_backoff();
                        self.session_log.reload_wanted = true;
                        self.settings_status =
                            Some(SettingsStatus::Ok(text::REAUTHENTICATED.to_owned()));
                        self.request_health()
                    }
                    (ValidationPurpose::BannerRetry, Err(failure)) => {
                        self.apply_failure_banner(&failure);
                        Vec::new()
                    }
                }
            }
            WorkerEvent::HealthFinished(report) => self.apply_health(report),
            WorkerEvent::SessionsLoaded {
                epoch,
                generation,
                limit,
                result,
            } => self.handle_sessions_loaded(epoch, generation, limit, result),
            WorkerEvent::SessionCreated { epoch, result } => {
                self.handle_session_created(epoch, result)
            }
            WorkerEvent::SessionLoaded {
                epoch,
                generation,
                session_id,
                result,
            } => self.handle_session_loaded(epoch, generation, session_id, result),
            WorkerEvent::ItemsLoaded {
                epoch,
                generation,
                session_id,
                after,
                result,
                ..
            } => self.handle_items_loaded(epoch, generation, session_id, after, result),
            WorkerEvent::TurnsLoaded {
                epoch,
                generation,
                session_id,
                after,
                result,
                ..
            } => self.handle_turns_loaded(epoch, generation, session_id, after, result),
            WorkerEvent::MutationFinished {
                epoch,
                session_id,
                kind,
                result,
            } => self.handle_mutation(epoch, session_id, kind, result),
            WorkerEvent::Fatal { message } => {
                self.health_in_flight = false;
                self.health = HealthState::Unreachable(message.clone());
                self.banner = Some(BannerView {
                    tone: BannerTone::Error,
                    code: None,
                    message,
                    actions: BannerActions::None,
                    unauthorized: false,
                });
                Vec::new()
            }
        }
    }

    pub(super) fn activate_action(&mut self, action: ActionId) -> Vec<Effect> {
        if !self.action_enabled(action) {
            return Vec::new();
        }
        match action {
            ActionId::Submit => self.submit_onboarding(),
            ActionId::TestConnection => self.test_connection(),
            ActionId::EditCredentials => {
                self.open_credentials();
                Vec::new()
            }
            ActionId::SignOut => {
                self.open_sign_out();
                Vec::new()
            }
            ActionId::Quit => {
                self.quit = true;
                vec![Effect::Quit]
            }
            ActionId::ShowGettingStarted => {
                self.overlay = Some(super::types::Overlay::Help);
                self.overlay_cursor = 0;
                Vec::new()
            }
            ActionId::SaveCredentials => self.submit_credentials(),
            ActionId::Back => {
                self.go_back();
                Vec::new()
            }
            ActionId::ConfirmSignOut => self.confirm_sign_out(),
            ActionId::CancelSignOut => {
                if self.overlay == Some(super::types::Overlay::ConfirmSignOut) {
                    self.overlay = None;
                }
                Vec::new()
            }
            ActionId::Retry => self.retry(),
            ActionId::OpenSettings => {
                self.open_settings();
                Vec::new()
            }
            ActionId::CloseHelp => {
                self.overlay = None;
                Vec::new()
            }
            ActionId::NewSession => {
                self.open_create();
                Vec::new()
            }
            ActionId::RefreshSessions => self.refresh_sessions_now(),
            ActionId::CreateSubmit => self.submit_create(),
            ActionId::AddMetadata => {
                self.create.add_row();
                Vec::new()
            }
            ActionId::ToggleAdvanced => {
                self.create.toggle_advanced();
                Vec::new()
            }
            ActionId::RefreshDetail
            | ActionId::LoadEarlierItems
            | ActionId::JumpToLatest
            | ActionId::LoadMoreTurns
            | ActionId::SendMessage
            | ActionId::ApproveFirst
            | ActionId::DenyFirst
            | ActionId::EditSession
            | ActionId::DeleteSession
            | ActionId::CancelTurn
            | ActionId::ConfirmDeleteSession
            | ActionId::CancelDeleteSession
            | ActionId::ConfirmCancelTurn
            | ActionId::CancelCancelTurn
            | ActionId::EditSubmit => self.detail_action(action).unwrap_or_default(),
        }
    }

    pub(super) fn open_settings(&mut self) {
        self.overlay = None;
        self.select_page(Page::Settings, super::types::Focus::Content);
    }

    pub(super) fn retry(&mut self) -> Vec<Effect> {
        if self.overlay == Some(super::types::Overlay::ErrorActions) {
            self.overlay = None;
        }
        if self.config.is_none() || self.validation.is_some() {
            return Vec::new();
        }
        self.validation = Some(ValidationPurpose::BannerRetry);
        vec![Effect::Revalidate {
            epoch: self.epoch,
            purpose: ValidationPurpose::BannerRetry,
        }]
    }

    pub(super) fn request_health(&mut self) -> Vec<Effect> {
        if self.config.is_none() || self.validation.is_some() {
            return Vec::new();
        }
        self.last_health_request = Some(Instant::now());
        self.health_in_flight = true;
        vec![Effect::Health { epoch: self.epoch }]
    }

    fn open_credentials(&mut self) {
        let Some(config) = &self.config else {
            return;
        };
        self.credentials = CredentialsState {
            base_url: crate::text_field::TextField::new(config.base_url()),
            api_key: crate::text_field::TextField::new(""),
            base_url_error: None,
            api_key_error: None,
            form_error: None,
        };
        self.overlay = None;
        self.screen = Screen::SettingsCredentials;
        self.focus = super::types::Focus::Field(FieldId::CredentialsBaseUrl);
    }

    pub(super) fn back_to_settings(&mut self) {
        if self.validation == Some(ValidationPurpose::Credentials) {
            self.validation = None;
            self.pending = None;
        }
        self.screen = Screen::Main;
        self.page = Page::Settings;
        self.focus = super::types::Focus::Content;
        self.credentials = CredentialsState::default();
    }

    pub(super) fn open_sign_out(&mut self) {
        if self.config.is_some() {
            self.overlay = Some(super::types::Overlay::ConfirmSignOut);
            self.overlay_cursor = 0;
        }
    }

    /// Drop every detail/edit route after a credential change.
    pub(super) fn reset_detail_routes(&mut self) {
        self.detail = super::SessionDetailState::default();
        self.detail_tab = super::types::DetailTab::Timeline;
        self.edit = super::EditFormState::default();
    }

    fn submit_onboarding(&mut self) -> Vec<Effect> {
        if self.screen != Screen::Onboarding || self.validation.is_some() {
            return Vec::new();
        }
        self.onboarding.base_url_error = None;
        self.onboarding.api_key_error = None;
        self.onboarding.form_error = None;

        let raw_base_url = self.onboarding.base_url.value().trim().to_owned();
        let base_url = match config::normalize_base_url(&raw_base_url) {
            Ok(base_url) => base_url,
            Err(error) => {
                self.onboarding.base_url_error = Some(text::base_url_error(&error));
                return Vec::new();
            }
        };
        let api_key = self.onboarding.api_key.value().trim().to_owned();
        if api_key.is_empty() {
            self.onboarding.api_key_error = Some(text::ERR_API_KEY_REQUIRED.to_owned());
            return Vec::new();
        }
        self.onboarding.base_url.set_value(&base_url);
        self.pending = Some(PendingCredentials {
            base_url: base_url.clone(),
            api_key: api_key.clone(),
        });
        self.validation = Some(ValidationPurpose::Onboarding);
        vec![Effect::Validate {
            epoch: self.epoch,
            purpose: ValidationPurpose::Onboarding,
            base_url,
            api_key,
        }]
    }

    fn submit_credentials(&mut self) -> Vec<Effect> {
        if self.screen != Screen::SettingsCredentials || self.validation.is_some() {
            return Vec::new();
        }
        self.credentials.base_url_error = None;
        self.credentials.api_key_error = None;
        self.credentials.form_error = None;

        let raw_base_url = self.credentials.base_url.value().trim().to_owned();
        let base_url = match config::normalize_base_url(&raw_base_url) {
            Ok(base_url) => base_url,
            Err(error) => {
                self.credentials.base_url_error = Some(text::base_url_error(&error));
                return Vec::new();
            }
        };
        let typed_key = self.credentials.api_key.value().trim().to_owned();
        let api_key = if typed_key.is_empty() {
            match &self.config {
                Some(config) => config.api_key().to_owned(),
                None => {
                    self.credentials.api_key_error = Some(text::ERR_API_KEY_REQUIRED.to_owned());
                    return Vec::new();
                }
            }
        } else {
            typed_key
        };
        self.credentials.base_url.set_value(&base_url);
        self.pending = Some(PendingCredentials {
            base_url: base_url.clone(),
            api_key: api_key.clone(),
        });
        self.validation = Some(ValidationPurpose::Credentials);
        vec![Effect::Validate {
            epoch: self.epoch,
            purpose: ValidationPurpose::Credentials,
            base_url,
            api_key,
        }]
    }

    fn test_connection(&mut self) -> Vec<Effect> {
        if self.config.is_none() || self.validation.is_some() {
            return Vec::new();
        }
        self.settings_status = None;
        self.validation = Some(ValidationPurpose::TestConnection);
        vec![Effect::Revalidate {
            epoch: self.epoch,
            purpose: ValidationPurpose::TestConnection,
        }]
    }

    fn finish_onboarding(&mut self) -> Vec<Effect> {
        let Some(pending) = self.pending.take() else {
            self.onboarding.form_error = Some(text::ERR_VALIDATION_STATE_LOST.to_owned());
            return Vec::new();
        };
        let config = match Config::new(&pending.base_url, &pending.api_key) {
            Ok(config) => config,
            Err(error) => {
                self.onboarding.form_error = Some(text::config_error(&error));
                return Vec::new();
            }
        };
        if let Err(error) = self.store.save(&config) {
            self.onboarding.form_error = Some(text::config_error(&error));
            return Vec::new();
        }
        let base_url = config.base_url().to_owned();
        let api_key = config.api_key().to_owned();
        self.config = Some(config);
        self.screen = Screen::Main;
        self.page = Page::SessionLog;
        self.focus = super::types::Focus::Content;
        self.onboarding = OnboardingState::default();
        self.session_log.reset();
        self.create = super::CreateFormState::default();
        self.reset_detail_routes();
        self.banner = None;
        self.paused = false;
        self.config_warning = None;
        self.contract = None;
        self.health = HealthState::Unknown;
        self.last_refresh = None;
        self.last_latency = None;
        self.health_in_flight = false;
        self.epoch = self.epoch.wrapping_add(1);
        let mut effects = vec![Effect::Adopt { base_url, api_key }];
        effects.extend(self.request_health());
        effects
    }

    fn finish_credentials(&mut self) -> Vec<Effect> {
        let Some(pending) = self.pending.take() else {
            self.credentials.form_error = Some(text::ERR_VALIDATION_STATE_LOST.to_owned());
            return Vec::new();
        };
        let config = match Config::new(&pending.base_url, &pending.api_key) {
            Ok(config) => config,
            Err(error) => {
                self.credentials.form_error = Some(text::config_error(&error));
                return Vec::new();
            }
        };
        if let Err(error) = self.store.save(&config) {
            self.credentials.form_error = Some(text::config_error(&error));
            return Vec::new();
        }
        let base_url = config.base_url().to_owned();
        let api_key = config.api_key().to_owned();
        self.config = Some(config);
        self.screen = Screen::Main;
        self.page = Page::Settings;
        self.focus = super::types::Focus::Content;
        self.credentials = CredentialsState::default();
        self.session_log.reset();
        self.create = super::CreateFormState::default();
        self.reset_detail_routes();
        self.banner = None;
        self.paused = false;
        self.settings_status = Some(SettingsStatus::Ok(text::CREDENTIALS_SAVED.to_owned()));
        self.contract = None;
        self.health = HealthState::Unknown;
        self.last_refresh = None;
        self.last_latency = None;
        self.health_in_flight = false;
        self.epoch = self.epoch.wrapping_add(1);
        let mut effects = vec![Effect::Adopt { base_url, api_key }];
        effects.extend(self.request_health());
        effects
    }

    fn confirm_sign_out(&mut self) -> Vec<Effect> {
        if self.overlay != Some(super::types::Overlay::ConfirmSignOut) || self.config.is_none() {
            return Vec::new();
        }
        match self.store.delete() {
            Ok(_) => {
                self.overlay = None;
                self.config = None;
                self.pending = None;
                self.validation = None;
                self.screen = Screen::Onboarding;
                self.page = Page::SessionLog;
                self.focus = super::types::Focus::Field(FieldId::OnboardingBaseUrl);
                self.onboarding = OnboardingState::default();
                self.credentials = CredentialsState::default();
                self.session_log.reset();
                self.create = super::CreateFormState::default();
                self.settings_status = None;
                self.banner = None;
                self.config_warning = None;
                self.contract = None;
                self.health = HealthState::Unknown;
                self.last_refresh = None;
                self.last_latency = None;
                self.last_health_request = None;
                self.health_in_flight = false;
                self.paused = false;
                self.reset_detail_routes();
                self.sidebar_scroll = 0;
                self.content_scroll = 0;
                self.epoch = self.epoch.wrapping_add(1);
                vec![Effect::DropClient]
            }
            Err(error) => {
                self.overlay = None;
                self.banner = Some(BannerView {
                    tone: BannerTone::Error,
                    code: None,
                    message: text::sign_out_failed(&text::config_error_detail(&error)),
                    actions: BannerActions::None,
                    unauthorized: false,
                });
                Vec::new()
            }
        }
    }

    fn apply_health(&mut self, report: HealthReport) -> Vec<Effect> {
        if report.epoch != self.epoch {
            return Vec::new();
        }
        self.health_in_flight = false;
        match report.result {
            Ok(health) => {
                if let Some(contract) = report.contract {
                    self.contract = Some(contract);
                }
                if health.status == "ok" {
                    self.health = HealthState::Healthy;
                } else {
                    self.health = HealthState::Degraded(health.status);
                }
                self.last_refresh = Some(report.checked_at);
                self.last_latency = report.latency;
                if self
                    .banner
                    .as_ref()
                    .is_some_and(|banner| !banner.unauthorized)
                {
                    self.banner = None;
                }
            }
            Err(error) => {
                if error.is_unauthorized() {
                    self.apply_unauthorized(&error);
                } else {
                    self.health = HealthState::Unreachable(error.message().to_owned());
                    self.banner = Some(BannerView {
                        tone: BannerTone::Error,
                        code: error
                            .code()
                            .map(str::to_owned)
                            .or_else(|| error.status().map(|status| status.to_string())),
                        message: text::gateway_error_banner(&text::api_error(&error)),
                        actions: BannerActions::RetryAndSettings,
                        unauthorized: false,
                    });
                }
            }
        }
        Vec::new()
    }

    fn apply_failure_banner(&mut self, failure: &ValidationFailure) {
        if failure.error.is_unauthorized() {
            self.apply_unauthorized(&failure.error);
        } else {
            self.banner = Some(BannerView {
                tone: BannerTone::Error,
                code: failure
                    .error
                    .code()
                    .map(str::to_owned)
                    .or_else(|| failure.error.status().map(|status| status.to_string())),
                message: validation_message(failure),
                actions: BannerActions::RetryAndSettings,
                unauthorized: false,
            });
        }
    }

    pub(super) fn apply_unauthorized(&mut self, error: &ApiError) {
        self.paused = true;
        self.session_log.poll.pause();
        self.banner = Some(BannerView {
            tone: BannerTone::Error,
            code: Some(
                error
                    .code()
                    .map_or_else(|| text::STATUS_401.to_owned(), str::to_owned),
            ),
            message: text::unauthorized_banner(error.message()),
            actions: BannerActions::RetryAndSettings,
            unauthorized: true,
        });
    }
}

fn validation_message(failure: &ValidationFailure) -> String {
    let detail = text::api_error_detail(
        failure.error.code(),
        failure.error.status(),
        failure.error.message(),
    );
    match failure.stage {
        ValidationStage::Health => text::health_check_failed(&detail),
        ValidationStage::Authentication if failure.error.is_unauthorized() => {
            text::ERR_AUTH_REJECTED.to_owned()
        }
        ValidationStage::Authentication => text::authentication_check_failed(&detail),
    }
}
