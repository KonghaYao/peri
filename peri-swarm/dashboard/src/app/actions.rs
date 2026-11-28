//! Actions, credential flows and worker-result handling for [`App`].

use std::time::Instant;

use super::forms::{CredentialsState, ObservabilityState, OnboardingState, PendingCredentials};
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
            WorkerEvent::ObservabilityTested { epoch, result } => {
                if epoch != self.epoch || !self.observability_testing {
                    return Vec::new();
                }
                self.observability_testing = false;
                self.observability.status = Some(match result {
                    Ok(check) => SettingsStatus::Ok(text::observability_test_ok(
                        check.elapsed,
                        check.streams,
                    )),
                    Err(error) => SettingsStatus::Error(text::observability_error(&error)),
                });
                Vec::new()
            }
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
                        for poll in [
                            &mut self.core_metrics.poll,
                            &mut self.agent_metrics.poll,
                            &mut self.request_log.poll,
                        ] {
                            poll.reset_backoff();
                        }
                        self.core_metrics.reload_wanted = true;
                        self.agent_metrics.reload_wanted = true;
                        self.request_log.reload_wanted = true;
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
            WorkerEvent::CoreMetricsLoaded {
                epoch,
                generation,
                window,
                result,
            } => self.handle_core_metrics_loaded(epoch, generation, window, result),
            WorkerEvent::AgentMetricsLoaded {
                epoch,
                generation,
                window,
                result,
            } => self.handle_agent_metrics_loaded(epoch, generation, window, result),
            WorkerEvent::RequestLogLoaded {
                epoch,
                generation,
                window,
                page,
                result,
            } => self.handle_request_log_loaded(epoch, generation, window, page, result),
            WorkerEvent::QueryLoaded {
                epoch,
                generation,
                result,
            } => self.handle_query_loaded(epoch, generation, result),
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
            ActionId::ConfigureObservability => {
                self.open_observability();
                Vec::new()
            }
            ActionId::TestObservability => self.test_observability(),
            ActionId::SaveObservability => self.save_observability(),
            ActionId::CancelObservability => {
                if self.screen == Screen::SettingsObservability {
                    self.go_back();
                }
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
            ActionId::RefreshCoreMetrics
            | ActionId::RefreshAgentMetrics
            | ActionId::RefreshRequestLog => self.metrics_action(action),
            ActionId::RunQuery => self.run_query(),
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

    fn open_observability(&mut self) {
        let existing = self.observability_config().cloned();
        self.observability = ObservabilityState::new(existing.as_ref());
        self.observability_testing = false;
        self.overlay = None;
        self.screen = Screen::SettingsObservability;
        self.focus = super::types::Focus::Field(FieldId::ObservabilityBaseUrl);
    }

    pub(super) fn back_to_settings(&mut self) {
        if self.validation == Some(ValidationPurpose::Credentials) {
            self.validation = None;
            self.pending = None;
        }
        self.observability_testing = false;
        self.screen = Screen::Main;
        self.page = Page::Settings;
        self.focus = super::types::Focus::Content;
        self.credentials = CredentialsState::default();
        self.observability = ObservabilityState::default();
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

    /// Validate the observability form and return the normalized values.
    ///
    /// A blank password keeps the stored one; the saved secret is never put
    /// back into the form.
    fn observability_values(&mut self) -> Option<(String, String, String, String)> {
        self.observability.base_url_error = None;
        self.observability.org_error = None;
        self.observability.user_error = None;
        self.observability.password_error = None;
        self.observability.form_error = None;

        let raw_base_url = self.observability.base_url.value().trim().to_owned();
        let base_url = match config::normalize_observability_base_url(&raw_base_url) {
            Ok(base_url) => base_url,
            Err(error) => {
                self.observability.base_url_error = Some(text::observability_config_error(&error));
                return None;
            }
        };
        let org = self.observability.org.value().trim().to_owned();
        if org.is_empty() {
            self.observability.org_error = Some(text::observability_config_error(
                &config::ObservabilityConfigError::EmptyOrg,
            ));
            return None;
        }
        let user = self.observability.user.value().trim().to_owned();
        if user.is_empty() {
            self.observability.user_error = Some(text::observability_config_error(
                &config::ObservabilityConfigError::EmptyUser,
            ));
            return None;
        }
        let typed_password = self.observability.password.value().to_owned();
        let password = if typed_password.trim().is_empty() {
            match self.observability_config() {
                Some(stored) => stored.password().to_owned(),
                None => {
                    self.observability.password_error = Some(text::observability_config_error(
                        &config::ObservabilityConfigError::EmptyPassword,
                    ));
                    return None;
                }
            }
        } else {
            typed_password
        };
        self.observability.base_url.set_value(&base_url);
        self.observability.org.set_value(&org);
        self.observability.user.set_value(&user);
        Some((base_url, org, user, password))
    }

    fn test_observability(&mut self) -> Vec<Effect> {
        if self.screen != Screen::SettingsObservability || self.observability_testing {
            return Vec::new();
        }
        self.observability.status = None;
        let Some((base_url, org, user, password)) = self.observability_values() else {
            return Vec::new();
        };
        self.observability_testing = true;
        vec![Effect::TestObservability {
            epoch: self.epoch,
            base_url,
            org,
            user,
            password,
        }]
    }

    fn save_observability(&mut self) -> Vec<Effect> {
        if self.screen != Screen::SettingsObservability || self.observability_testing {
            return Vec::new();
        }
        self.observability.status = None;
        let Some((base_url, org, user, password)) = self.observability_values() else {
            return Vec::new();
        };
        let observability =
            match config::ObservabilityConfig::new(&base_url, &org, &user, &password) {
                Ok(observability) => observability,
                Err(error) => {
                    self.observability.form_error = Some(text::observability_config_error(&error));
                    return Vec::new();
                }
            };
        let Some(current) = self.config.take() else {
            self.observability.form_error = Some(text::NO_ACTIVE_CONNECTION.to_owned());
            return Vec::new();
        };
        let saved = current.clone().with_observability(observability.clone());
        if let Err(error) = self.store.save(&saved) {
            self.config = Some(current);
            self.observability.form_error = Some(text::config_error(&error));
            return Vec::new();
        }
        self.config = Some(saved);
        self.core_metrics = super::CoreMetricsState::new();
        self.request_log = super::RequestLogState::new();
        self.query = super::QueryState::new();
        self.observability = ObservabilityState::default();
        self.observability_testing = false;
        self.screen = Screen::Main;
        self.page = Page::Settings;
        self.focus = super::types::Focus::Content;
        self.settings_status = Some(SettingsStatus::Ok(text::OBSERVABILITY_SAVED.to_owned()));
        vec![Effect::AdoptObservability {
            base_url: observability.base_url().to_owned(),
            org: observability.org().to_owned(),
            user: observability.user().to_owned(),
            password: observability.password().to_owned(),
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
        self.reset_metrics_routes();
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
        // Editing gateway credentials must keep the optional observability
        // section: `Config::new` always starts with none.
        let config = match self
            .config
            .as_ref()
            .and_then(Config::observability)
            .cloned()
        {
            Some(observability) => config.with_observability(observability),
            None => config,
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
        self.reset_metrics_routes();
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
                self.observability = ObservabilityState::default();
                self.observability_testing = false;
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
