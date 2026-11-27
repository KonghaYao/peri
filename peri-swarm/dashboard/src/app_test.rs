use std::fs;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tempfile::TempDir;

use super::*;
use crate::api::{ApiError, Health};
use crate::config::{self, Config, ConfigStore};
use crate::worker::{
    HealthReport, ValidationFailure, ValidationPurpose, ValidationStage, WorkerEvent,
};

const SECRET: &str = "it-key-never-displayed-0123456789";

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

fn temp_store(dir: &TempDir) -> ConfigStore {
    ConfigStore::at(dir.path().join("config.toml"))
}

fn onboarding_app(dir: &TempDir) -> (App, ConfigStore) {
    let store = temp_store(dir);
    (App::onboarding(store.clone()), store)
}

fn connected_app(dir: &TempDir) -> (App, ConfigStore) {
    let store = temp_store(dir);
    let config = Config::new(config::DEFAULT_BASE_URL, SECRET).expect("valid config");
    store.save(&config).expect("save config");
    (App::connected(config, Vec::new(), store.clone()), store)
}

fn unauthorized_body() -> &'static [u8] {
    br#"{"error":{"code":"unauthorized","message":"bad key","details":null}}"#
}

fn validation_epoch(effects: &[Effect]) -> u64 {
    match effects {
        [Effect::Validate { epoch, .. }] => *epoch,
        _ => panic!("expected one Validate effect"),
    }
}

fn revalidate_epoch(effects: &[Effect]) -> u64 {
    match effects {
        [Effect::Revalidate { epoch, .. }] => *epoch,
        _ => panic!("expected one Revalidate effect"),
    }
}

fn health_report(
    app: &App,
    result: Result<Health, ApiError>,
    contract: Option<&str>,
) -> HealthReport {
    HealthReport {
        epoch: app.epoch(),
        result,
        latency: Some(Duration::from_millis(7)),
        checked_at: Instant::now(),
        contract: contract.map(str::to_owned),
    }
}

#[test]
fn navigation_lists_all_thirteen_pages_in_three_groups() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (app, _store) = onboarding_app(&dir);

    assert_eq!(Page::ALL.len(), 13);
    assert_eq!(NAV_ENTRIES.len(), 13);
    assert_eq!(NAV_ROW_COUNT, 16);
    assert_eq!(app.page(), Page::SessionLog);
    assert_eq!(app.screen(), Screen::Onboarding);

    let monitor = Page::ALL
        .iter()
        .filter(|page| page.group() == SidebarGroup::Monitor)
        .count();
    let resources = Page::ALL
        .iter()
        .filter(|page| page.group() == SidebarGroup::Resources)
        .count();
    let platform = Page::ALL
        .iter()
        .filter(|page| page.group() == SidebarGroup::Platform)
        .count();
    assert_eq!((monitor, resources, platform), (5, 5, 3));

    assert!(Page::SessionLog.is_implemented());
    assert!(Page::Settings.is_implemented());
    for page in Page::ALL {
        if page != Page::SessionLog && page != Page::Settings {
            assert!(!page.is_implemented(), "{page:?}");
        }
    }
    assert_eq!(page_row_index(Page::CoreMetrics), 1);
    assert_eq!(page_row_index(Page::SessionLog), 4);
    assert_eq!(page_row_index(Page::Settings), 15);
}

#[test]
fn onboarding_validates_then_saves_once_and_enters_dashboard() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut app, store) = onboarding_app(&dir);

    app.handle_key(key(KeyCode::Tab));
    app.handle_paste(SECRET);
    let effects = app.handle_key(key(KeyCode::Enter));
    assert_eq!(
        app.validation_purpose(),
        Some(ValidationPurpose::Onboarding)
    );
    let epoch = validation_epoch(&effects);
    assert!(!store.path().exists(), "validation must not write config");

    let effects = app.handle_worker(WorkerEvent::ValidationFinished {
        epoch,
        purpose: ValidationPurpose::Onboarding,
        result: Ok(Duration::from_millis(12)),
    });
    assert_eq!(app.screen(), Screen::Main);
    assert_eq!(app.page(), Page::SessionLog);
    assert!(store.path().exists());
    let loaded = store.load().expect("load").expect("saved");
    assert_eq!(loaded.config().base_url(), config::DEFAULT_BASE_URL);

    assert_eq!(effects.len(), 2);
    assert!(matches!(effects[0], Effect::Adopt { .. }));
    assert!(matches!(effects[1], Effect::Health { .. }));
}

#[test]
fn onboarding_failure_stays_put_and_writes_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut app, store) = onboarding_app(&dir);
    app.handle_key(key(KeyCode::Tab));
    app.handle_paste(SECRET);
    let epoch = validation_epoch(&app.handle_key(key(KeyCode::Enter)));

    let effects = app.handle_worker(WorkerEvent::ValidationFinished {
        epoch,
        purpose: ValidationPurpose::Onboarding,
        result: Err(ValidationFailure {
            stage: ValidationStage::Authentication,
            error: ApiError::from_status(401, unauthorized_body()),
        }),
    });

    assert!(effects.is_empty());
    assert_eq!(app.screen(), Screen::Onboarding);
    assert!(!store.path().exists());
    let message = app.onboarding_form().form_error().expect("form error");
    assert!(!message.contains(SECRET));
    assert_eq!(app.validation_purpose(), None);

    let effects = app.handle_worker(WorkerEvent::ValidationFinished {
        epoch,
        purpose: ValidationPurpose::Onboarding,
        result: Err(ValidationFailure {
            stage: ValidationStage::Health,
            error: ApiError::local("connection refused"),
        }),
    });
    assert!(effects.is_empty());
    assert!(!store.path().exists());
}

#[test]
fn missing_api_key_is_rejected_before_any_request() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut app, store) = onboarding_app(&dir);

    let effects = app.handle_key(key(KeyCode::Enter));
    assert!(effects.is_empty());
    assert!(app.onboarding_form().api_key_error().is_some());
    assert_eq!(app.validation_purpose(), None);
    assert!(!store.path().exists());
}

#[test]
fn invalid_base_url_is_a_field_error_and_writes_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut app, store) = onboarding_app(&dir);
    assert!(app.set_field_value(FieldId::OnboardingBaseUrl, "ftp://example.com"));
    app.set_field_value(FieldId::OnboardingApiKey, SECRET);

    let effects = app.handle_key(key(KeyCode::Enter));
    assert!(effects.is_empty());
    assert!(app.onboarding_form().base_url_error().is_some());
    assert!(!store.path().exists());
}

#[test]
fn stale_validation_results_are_ignored() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut app, store) = onboarding_app(&dir);
    app.handle_key(key(KeyCode::Tab));
    app.handle_paste(SECRET);
    let epoch = validation_epoch(&app.handle_key(key(KeyCode::Enter)));

    let effects = app.handle_worker(WorkerEvent::ValidationFinished {
        epoch: epoch + 99,
        purpose: ValidationPurpose::Onboarding,
        result: Ok(Duration::from_millis(1)),
    });
    assert!(effects.is_empty());
    assert_eq!(app.screen(), Screen::Onboarding);
    assert!(!store.path().exists());
}

#[test]
fn sign_out_deletes_config_only_after_confirmation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut app, store) = connected_app(&dir);

    app.apply(Intent::OpenSignOut);
    assert_eq!(app.overlay(), Some(Overlay::ConfirmSignOut));
    assert!(store.path().exists(), "opening the dialog must not delete");

    app.apply(Intent::CancelSignOut);
    assert_eq!(app.overlay(), None);
    assert!(store.path().exists());

    app.apply(Intent::OpenSignOut);
    let effects = app.apply(Intent::ConfirmSignOut);
    assert!(matches!(effects.as_slice(), [Effect::DropClient]));
    assert!(!store.path().exists());
    assert_eq!(app.screen(), Screen::Onboarding);
    assert!(!app.has_config());
}

#[test]
fn sign_out_delete_failure_keeps_dashboard_and_shows_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ConfigStore::at(dir.path().join("config-as-directory"));
    fs::create_dir_all(store.path()).expect("create directory");
    let config = Config::new(config::DEFAULT_BASE_URL, SECRET).expect("valid config");
    let mut app = App::connected(config, Vec::new(), store.clone());

    app.apply(Intent::OpenSignOut);
    let effects = app.apply(Intent::ConfirmSignOut);

    assert!(effects.is_empty());
    assert_eq!(app.screen(), Screen::Main);
    assert!(app.has_config());
    assert!(store.path().is_dir());
    let banner = app.active_banner().expect("delete failure banner");
    assert_eq!(banner.tone, BannerTone::Error);
    assert!(app.overlay().is_none());
}

#[test]
fn unauthorized_health_pauses_until_reauthentication() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut app, _store) = connected_app(&dir);
    let epoch = app.epoch();

    app.handle_worker(WorkerEvent::HealthFinished(HealthReport {
        epoch,
        result: Err(ApiError::from_status(401, unauthorized_body())),
        latency: None,
        checked_at: Instant::now(),
        contract: None,
    }));
    assert!(app.is_paused());
    let banner = app.active_banner().expect("unauthorized banner");
    assert!(banner.unauthorized);
    assert_eq!(banner.actions, BannerActions::RetryAndSettings);
    assert_eq!(banner.code.as_deref(), Some("unauthorized"));

    // A healthy /healthz does not prove the key works; the pause must survive.
    app.handle_worker(WorkerEvent::HealthFinished(health_report(
        &app,
        Ok(Health {
            status: "ok".to_owned(),
        }),
        Some("1"),
    )));
    assert!(app.is_paused());
    assert!(app
        .active_banner()
        .is_some_and(|banner| banner.unauthorized));

    let effects = app.apply(Intent::Retry);
    let epoch = revalidate_epoch(&effects);
    assert_eq!(
        app.validation_purpose(),
        Some(ValidationPurpose::BannerRetry)
    );
    app.handle_worker(WorkerEvent::ValidationFinished {
        epoch,
        purpose: ValidationPurpose::BannerRetry,
        result: Ok(Duration::from_millis(5)),
    });
    assert!(!app.is_paused());
    assert!(app.active_banner().is_none());
}

#[test]
fn contract_mismatch_warns_but_keeps_pages_reachable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut app, _store) = connected_app(&dir);

    app.handle_worker(WorkerEvent::HealthFinished(health_report(
        &app,
        Ok(Health {
            status: "ok".to_owned(),
        }),
        Some("2"),
    )));

    assert!(app.contract_mismatch());
    let banner = app.active_banner().expect("contract warning");
    assert_eq!(banner.tone, BannerTone::Warning);
    assert!(!app.is_paused());
    app.apply(Intent::SelectPage(Page::Agents));
    assert_eq!(app.page(), Page::Agents);
}

#[test]
fn health_poll_runs_on_its_interval_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut app, _store) = connected_app(&dir);
    let now = Instant::now();

    assert!(app.health_poll_due(now).is_some());
    assert!(app.health_poll_due(now + Duration::from_secs(1)).is_none());

    app.handle_worker(WorkerEvent::HealthFinished(health_report(
        &app,
        Ok(Health {
            status: "ok".to_owned(),
        }),
        Some("1"),
    )));
    assert!(app.health_poll_due(now + Duration::from_secs(5)).is_none());
    assert!(app.health_poll_due(now + Duration::from_secs(10)).is_some());
}

#[test]
fn typing_in_a_field_suppresses_global_shortcuts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut app, _store) = onboarding_app(&dir);

    let effects = app.handle_key(key(KeyCode::Char('q')));
    assert!(effects.is_empty());
    assert!(!app.should_quit());
    assert!(app.onboarding_form().base_url().value().ends_with('q'));

    let effects = app.handle_key(ctrl(KeyCode::Char('c')));
    assert!(matches!(effects.as_slice(), [Effect::Quit]));
    assert!(app.should_quit());
}

#[test]
fn keyboard_focus_cycle_reaches_both_fields_and_submit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut app, _store) = onboarding_app(&dir);

    assert_eq!(app.focus(), Focus::Field(FieldId::OnboardingBaseUrl));
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(app.focus(), Focus::Field(FieldId::OnboardingApiKey));
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(app.focus(), Focus::Action(ActionId::Submit));
    app.handle_key(key(KeyCode::BackTab));
    assert_eq!(app.focus(), Focus::Field(FieldId::OnboardingApiKey));
}

#[test]
fn narrow_menu_navigation_switches_the_page() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut app, _store) = connected_app(&dir);

    app.apply(Intent::ToggleMenu);
    assert_eq!(app.overlay(), Some(Overlay::NarrowMenu));
    assert_eq!(app.overlay_cursor(), 3);

    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.page(), Page::RequestLog);
    assert_eq!(app.overlay(), None);
    assert_eq!(app.focus(), Focus::Content);
}

#[test]
fn create_placeholder_route_returns_to_the_log() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut app, _store) = connected_app(&dir);

    app.handle_key(key(KeyCode::Char('n')));
    assert_eq!(app.screen(), Screen::SessionCreate);
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.screen(), Screen::Main);
    assert_eq!(app.page(), Page::SessionLog);
}

#[test]
fn keyboard_navigation_moves_through_the_sidebar() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut app, _store) = connected_app(&dir);

    app.apply(Intent::FocusSidebar);
    app.handle_key(key(KeyCode::Up));
    assert_eq!(app.page(), Page::SandboxMetrics);
    app.handle_key(key(KeyCode::Up));
    assert_eq!(app.page(), Page::AgentMetrics);
    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.page(), Page::SandboxMetrics);
}

#[test]
fn settings_edit_keeps_the_old_config_until_saved() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut app, store) = connected_app(&dir);
    let original = store
        .load()
        .expect("load")
        .expect("saved")
        .config()
        .base_url()
        .to_owned();

    app.apply(Intent::OpenSettings);
    app.apply(Intent::Activate(ActionId::EditCredentials));
    assert_eq!(app.screen(), Screen::SettingsCredentials);
    assert!(app
        .credentials()
        .base_url()
        .value()
        .starts_with("http://127.0.0.1:8080"));

    // Back without saving preserves the stored file and the live config.
    app.apply(Intent::Back);
    assert_eq!(app.screen(), Screen::Main);
    assert_eq!(
        store
            .load()
            .expect("load")
            .expect("saved")
            .config()
            .base_url(),
        original
    );
    assert_eq!(app.config_base_url(), Some(original.as_str()));
}

#[test]
fn credential_edit_revalidates_before_saving() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut app, store) = connected_app(&dir);
    let original = store
        .load()
        .expect("load")
        .expect("saved")
        .config()
        .base_url()
        .to_owned();

    app.apply(Intent::OpenSettings);
    app.apply(Intent::Activate(ActionId::EditCredentials));
    assert!(app.set_field_value(FieldId::CredentialsBaseUrl, "https://swarm.example.com/v1"));

    let effects = app.apply(Intent::Activate(ActionId::SaveCredentials));
    let epoch = validation_epoch(&effects);
    assert_eq!(
        app.validation_purpose(),
        Some(ValidationPurpose::Credentials)
    );
    // Still old on disk while the network check is in flight.
    assert_eq!(
        store
            .load()
            .expect("load")
            .expect("saved")
            .config()
            .base_url(),
        original
    );

    app.handle_worker(WorkerEvent::ValidationFinished {
        epoch,
        purpose: ValidationPurpose::Credentials,
        result: Ok(Duration::from_millis(4)),
    });
    assert_eq!(app.screen(), Screen::Main);
    assert_eq!(app.page(), Page::Settings);
    let saved = store.load().expect("load").expect("saved");
    assert_eq!(saved.config().base_url(), "https://swarm.example.com");
}

#[test]
fn test_connection_never_writes_or_adopts_credentials() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut app, store) = connected_app(&dir);
    let before = fs::read(store.path()).expect("read config");

    let effects = app.apply(Intent::Activate(ActionId::TestConnection));
    assert!(matches!(effects.as_slice(), [Effect::Revalidate { .. }]));
    assert_eq!(
        app.validation_purpose(),
        Some(ValidationPurpose::TestConnection)
    );

    app.handle_worker(WorkerEvent::ValidationFinished {
        epoch: app.epoch(),
        purpose: ValidationPurpose::TestConnection,
        result: Err(ValidationFailure {
            stage: ValidationStage::Health,
            error: ApiError::local("connection refused"),
        }),
    });
    assert_eq!(
        fs::read(store.path()).expect("read config"),
        before,
        "test connection must not rewrite the file"
    );
    assert!(matches!(
        app.settings_status(),
        Some(SettingsStatus::Error(_))
    ));
}
