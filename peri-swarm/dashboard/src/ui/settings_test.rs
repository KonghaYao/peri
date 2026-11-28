use std::time::Duration;

use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Modifier;
use ratatui::Terminal;

use super::*;
use crate::app::{App, FieldId, Intent, Page, Screen};
use crate::config::{self, Config, ConfigStore, ObservabilityConfig};
use crate::observability::ObservabilityError;
use crate::ui::hit::HitMap;
use crate::worker::{ObservabilityCheck, WorkerEvent};

const SECRET: &str = "settings-test-key-never-displayed";
const OBS_SECRET: &str = "oo-settings-password-never-displayed";

fn connected_app() -> (App, ConfigStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ConfigStore::at(dir.path().join("config.toml"));
    let config = Config::new(config::DEFAULT_BASE_URL, SECRET).expect("config");
    store.save(&config).expect("save");
    (
        App::connected(config, Vec::new(), store.clone()),
        store,
        dir,
    )
}

fn observability() -> ObservabilityConfig {
    ObservabilityConfig::new(
        "http://127.0.0.1:5080",
        "obs-org",
        "logs-reader",
        OBS_SECRET,
    )
    .expect("observability")
}

fn connected_app_with_observability() -> (App, ConfigStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ConfigStore::at(dir.path().join("config.toml"));
    let config = Config::new(config::DEFAULT_BASE_URL, SECRET)
        .expect("config")
        .with_observability(observability());
    store.save(&config).expect("save");
    (
        App::connected(config, Vec::new(), store.clone()),
        store,
        dir,
    )
}

fn draw(app: &mut App, width: u16, height: u16) -> (Terminal<TestBackend>, HitMap) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    let mut hits = HitMap::default();
    terminal
        .draw(|frame| hits = crate::ui::render(frame, app))
        .expect("draw");
    (terminal, hits)
}

fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect()
}

fn find_label_row(buffer: &ratatui::buffer::Buffer, needle: &str) -> Option<(u16, u16)> {
    for y in buffer.area.y..buffer.area.bottom() {
        let row: String = (buffer.area.x..buffer.area.right())
            .map(|x| {
                buffer
                    .cell((x, y))
                    .map_or("", ratatui::buffer::Cell::symbol)
            })
            .collect();
        if let Some(offset) = row.find(needle) {
            let x = buffer.area.x + u16::try_from(offset).expect("column");
            return Some((x, y));
        }
    }
    None
}

fn click(hits: &HitMap, intent: Intent) -> Intent {
    let rect = hits.region_rect(intent).expect("hit region");
    hits.click(false, rect.x, rect.y).expect("resolved click")
}

#[test]
fn settings_shows_connection_facts_and_exposes_actions() {
    let (mut app, store, _dir) = connected_app();
    app.apply(Intent::OpenSettings);
    assert_eq!(app.page(), Page::Settings);

    let (terminal, hits) = draw(&mut app, 100, 30);
    let text = buffer_text(&terminal);
    assert!(text.contains(text::SETTINGS_TITLE));
    assert!(text.contains(config::DEFAULT_BASE_URL));
    assert!(text.contains(text::KEY_SET));
    assert!(text.contains(text::LABEL_CONTRACT));
    for action in [
        ActionId::TestConnection,
        ActionId::EditCredentials,
        ActionId::SignOut,
        ActionId::Quit,
    ] {
        assert!(
            hits.region_rect(Intent::Activate(action)).is_some(),
            "missing action {action:?}"
        );
    }
    assert!(store.path().exists());
}

#[test]
fn the_refresh_row_does_not_repeat_its_label_in_the_value() {
    let (mut app, _store, _dir) = connected_app();
    app.apply(Intent::OpenSettings);
    let (terminal, _hits) = draw(&mut app, 100, 30);
    let buffer = terminal.backend().buffer();
    let (x, y) = find_label_row(buffer, text::LABEL_REFRESH).expect("refresh row");
    assert!(x > 0);
    let row: String = (buffer.area.x..buffer.area.right())
        .map(|column| buffer[(column, y)].symbol())
        .collect();
    assert!(row.contains("never"), "{row}");
    assert!(!row.contains("last refresh:"), "{row}");
}

#[test]
fn the_sign_out_dialog_describes_tab_enter_and_escape_truthfully() {
    let (mut app, _store, _dir) = connected_app();
    app.apply(Intent::OpenSettings);
    app.apply(Intent::OpenSignOut);
    let (terminal, hits) = draw(&mut app, 100, 30);
    let text = buffer_text(&terminal);
    assert!(text.contains(text::SIGNOUT_HINT), "{text}");
    assert!(!text.contains("Enter confirms"), "{text}");
    assert_eq!(app.overlay_cursor(), 0, "Cancel stays the default");
    let cancel = hits
        .region_rect(Intent::Activate(ActionId::CancelSignOut))
        .expect("cancel button");
    assert!(cancel.width > 0);
}

#[test]
fn test_connection_button_starts_a_revalidation() {
    let (mut app, _store, _dir) = connected_app();
    app.apply(Intent::OpenSettings);
    let (_terminal, hits) = draw(&mut app, 100, 30);

    let intent = click(&hits, Intent::Activate(ActionId::TestConnection));
    let effects = app.apply(intent);
    assert!(matches!(
        effects.as_slice(),
        [crate::app::Effect::Revalidate { .. }]
    ));
    assert_eq!(
        app.validation_purpose(),
        Some(crate::worker::ValidationPurpose::TestConnection)
    );
}

#[test]
fn edit_credentials_page_focuses_fields_and_back_preserves_config() {
    let (mut app, store, _dir) = connected_app();
    app.apply(Intent::OpenSettings);
    let (_terminal, hits) = draw(&mut app, 100, 30);
    app.apply(click(&hits, Intent::Activate(ActionId::EditCredentials)));
    assert_eq!(app.screen(), Screen::SettingsCredentials);

    let (_terminal, hits) = draw(&mut app, 100, 30);
    let field_rect = hits
        .region_rect(Intent::FocusField {
            field: FieldId::CredentialsApiKey,
            click: None,
        })
        .expect("key field hit");
    let intent = hits
        .click(false, field_rect.x + 2, field_rect.y + 1)
        .expect("field click");
    assert!(matches!(
        intent,
        Intent::FocusField {
            field: FieldId::CredentialsApiKey,
            click: Some(_)
        }
    ));
    app.apply(intent);
    assert_eq!(
        app.focus(),
        crate::app::Focus::Field(FieldId::CredentialsApiKey)
    );

    let back = hits
        .region_rect(Intent::Activate(ActionId::Back))
        .expect("back hit");
    app.apply(hits.click(false, back.x, back.y).expect("back click"));
    assert_eq!(app.screen(), Screen::Main);
    assert_eq!(app.page(), Page::Settings);
    assert!(store.path().exists());
}

#[test]
fn quit_button_sets_quit() {
    let (mut app, _store, _dir) = connected_app();
    app.apply(Intent::OpenSettings);
    let (_terminal, hits) = draw(&mut app, 100, 30);
    let effects = app.apply(click(&hits, Intent::Activate(ActionId::Quit)));
    assert!(matches!(effects.as_slice(), [crate::app::Effect::Quit]));
    assert!(app.should_quit());
}

#[test]
fn busy_settings_disables_buttons_for_mouse_tab_and_enter() {
    let (mut app, _store, _dir) = connected_app();
    app.apply(Intent::OpenSettings);
    let (_terminal, hits) = draw(&mut app, 100, 30);
    assert!(hits
        .region_rect(Intent::Activate(ActionId::TestConnection))
        .is_some());
    app.apply(click(&hits, Intent::Activate(ActionId::TestConnection)));
    assert_eq!(
        app.focus(),
        crate::app::Focus::Action(ActionId::TestConnection)
    );

    let (terminal, hits) = draw(&mut app, 100, 30);
    for action in [
        ActionId::TestConnection,
        ActionId::EditCredentials,
        ActionId::SignOut,
        ActionId::Quit,
    ] {
        assert!(!app.action_enabled(action), "{action:?} stays enabled");
    }
    // The sidebar keeps its own sign-out region, so only the three page
    // buttons are asserted here.
    for action in [
        ActionId::TestConnection,
        ActionId::EditCredentials,
        ActionId::Quit,
    ] {
        assert!(
            hits.region_rect(Intent::Activate(action)).is_none(),
            "{action:?} still has a hit region"
        );
    }
    let label = text::action_label(ActionId::TestConnection);
    let (label_x, label_y) =
        find_label_row(terminal.backend().buffer(), label).expect("drawn label");
    let style = terminal
        .backend()
        .buffer()
        .cell((label_x, label_y))
        .expect("button cell")
        .style();
    assert!(style.add_modifier.contains(Modifier::DIM));

    assert!(app
        .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .is_empty());
    assert_eq!(
        app.validation_purpose(),
        Some(crate::worker::ValidationPurpose::TestConnection)
    );

    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(app.focus(), crate::app::Focus::Sidebar);
}

fn open_observability(app: &mut App) {
    app.apply(Intent::OpenSettings);
    let (_terminal, hits) = draw(app, 120, 40);
    app.apply(click(
        &hits,
        Intent::Activate(ActionId::ConfigureObservability),
    ));
    assert_eq!(app.screen(), Screen::SettingsObservability);
}

fn fill_observability(app: &mut App, password: &str) {
    assert!(app.set_field_value(FieldId::ObservabilityBaseUrl, "http://127.0.0.1:5080"));
    assert!(app.set_field_value(FieldId::ObservabilityOrg, "peri"));
    assert!(app.set_field_value(FieldId::ObservabilityUser, "dashboard"));
    assert!(app.set_field_value(FieldId::ObservabilityPassword, password));
}

#[test]
fn settings_reports_observability_as_not_configured() {
    let (mut app, _store, _dir) = connected_app();
    app.apply(Intent::OpenSettings);
    let (terminal, hits) = draw(&mut app, 120, 40);
    let text = buffer_text(&terminal);
    assert!(text.contains(text::SECTION_OBSERVABILITY));
    assert!(text.contains(text::OBSERVABILITY_NOT_CONFIGURED));
    assert!(text.contains(text::OBSERVABILITY_NOT_A_BOUNDARY));
    assert!(hits
        .region_rect(Intent::Activate(ActionId::ConfigureObservability))
        .is_some());
}

#[test]
fn settings_reports_configured_observability_without_the_password() {
    let (mut app, _store, _dir) = connected_app_with_observability();
    app.apply(Intent::OpenSettings);
    let (terminal, hits) = draw(&mut app, 120, 40);
    let text = buffer_text(&terminal);
    assert!(text.contains(text::OBSERVABILITY_CONFIGURED));
    assert!(text.contains("http://127.0.0.1:5080"));
    assert!(text.contains("obs-org"));
    assert!(text.contains("logs-reader"));
    assert!(!text.contains(OBS_SECRET));
    assert!(hits
        .region_rect(Intent::Activate(ActionId::ConfigureObservability))
        .is_some());
}

#[test]
fn observability_editor_masks_the_password() {
    let (mut app, _store, _dir) = connected_app();
    open_observability(&mut app);
    let (terminal, hits) = draw(&mut app, 120, 50);
    let text = buffer_text(&terminal);
    assert!(text.contains(text::OBSERVABILITY_TITLE));
    for field in [
        FieldId::ObservabilityBaseUrl,
        FieldId::ObservabilityOrg,
        FieldId::ObservabilityUser,
        FieldId::ObservabilityPassword,
    ] {
        assert!(
            hits.region_rect(Intent::FocusField { field, click: None })
                .is_some(),
            "missing {field:?}"
        );
    }

    assert!(app.set_field_value(FieldId::ObservabilityPassword, OBS_SECRET));
    let (terminal, _hits) = draw(&mut app, 120, 50);
    let text = buffer_text(&terminal);
    assert!(
        !text.contains(OBS_SECRET),
        "password rendered in clear text"
    );
    assert!(text.contains('\u{2022}'), "masked cell missing");
}

#[test]
fn saving_observability_keeps_the_gateway_key_and_never_echoes_the_password() {
    let (mut app, store, _dir) = connected_app();
    open_observability(&mut app);
    fill_observability(&mut app, OBS_SECRET);
    let (_terminal, hits) = draw(&mut app, 120, 50);

    let effects = app.apply(click(&hits, Intent::Activate(ActionId::SaveObservability)));
    assert!(matches!(
        effects.as_slice(),
        [crate::app::Effect::AdoptObservability { .. }]
    ));
    assert_eq!(app.screen(), Screen::Main);
    assert_eq!(app.page(), Page::Settings);
    assert_eq!(app.observability_config().map(|o| o.org()), Some("peri"));

    let loaded = store.load().expect("load").expect("saved");
    assert_eq!(loaded.config().api_key(), SECRET);
    let stored = loaded.config().observability().expect("observability");
    assert_eq!(stored.base_url(), "http://127.0.0.1:5080");
    assert_eq!(stored.org(), "peri");
    assert_eq!(stored.user(), "dashboard");
    assert_eq!(stored.password(), OBS_SECRET);

    let (terminal, _hits) = draw(&mut app, 120, 40);
    let text = buffer_text(&terminal);
    assert!(text.contains(text::OBSERVABILITY_CONFIGURED));
    assert!(text.contains(text::OBSERVABILITY_SAVED));
    assert!(!text.contains(OBS_SECRET));
}

#[test]
fn observability_test_uses_the_typed_values_and_reports_success() {
    let (mut app, _store, _dir) = connected_app();
    open_observability(&mut app);
    fill_observability(&mut app, OBS_SECRET);
    let (_terminal, hits) = draw(&mut app, 120, 50);

    let effects = app.apply(click(&hits, Intent::Activate(ActionId::TestObservability)));
    let epoch = match effects.as_slice() {
        [crate::app::Effect::TestObservability {
            epoch,
            base_url,
            org,
            user,
            password,
        }] => {
            assert_eq!(base_url, "http://127.0.0.1:5080");
            assert_eq!(org, "peri");
            assert_eq!(user, "dashboard");
            assert_eq!(password, OBS_SECRET);
            *epoch
        }
        other => panic!("unexpected effects: {} effects", other.len()),
    };
    assert!(app.observability_testing());

    app.handle_worker(WorkerEvent::ObservabilityTested {
        epoch,
        result: Ok(ObservabilityCheck {
            elapsed: Duration::from_millis(12),
            streams: Some(3),
        }),
    });
    assert!(!app.observability_testing());
    let (terminal, _hits) = draw(&mut app, 120, 50);
    let text = buffer_text(&terminal);
    assert!(text.contains("OpenObserve connection OK in 12 ms (3 streams)."));
}

#[test]
fn blank_password_reuses_the_stored_observability_secret() {
    let (mut app, store, _dir) = connected_app_with_observability();
    open_observability(&mut app);
    assert_eq!(app.observability().password().value(), "");
    let (_terminal, hits) = draw(&mut app, 120, 50);
    let effects = app.apply(click(&hits, Intent::Activate(ActionId::TestObservability)));
    let epoch = match effects.as_slice() {
        [crate::app::Effect::TestObservability {
            epoch, password, ..
        }] => {
            assert_eq!(password, OBS_SECRET);
            *epoch
        }
        other => panic!("unexpected effects: {} effects", other.len()),
    };
    app.handle_worker(WorkerEvent::ObservabilityTested {
        epoch,
        result: Ok(ObservabilityCheck {
            elapsed: Duration::from_millis(5),
            streams: None,
        }),
    });
    assert!(!app.observability_testing());

    assert!(app.set_field_value(FieldId::ObservabilityOrg, "other"));
    let (_terminal, hits) = draw(&mut app, 120, 50);
    app.apply(click(&hits, Intent::Activate(ActionId::SaveObservability)));
    let loaded = store.load().expect("load").expect("saved");
    let stored = loaded.config().observability().expect("observability");
    assert_eq!(stored.org(), "other");
    assert_eq!(stored.password(), OBS_SECRET);
}

#[test]
fn invalid_observability_url_is_rejected_without_an_effect() {
    let (mut app, _store, _dir) = connected_app();
    open_observability(&mut app);
    fill_observability(&mut app, OBS_SECRET);
    assert!(app.set_field_value(FieldId::ObservabilityBaseUrl, "ftp://127.0.0.1:5080"));
    let (_terminal, hits) = draw(&mut app, 120, 50);

    let effects = app.apply(click(&hits, Intent::Activate(ActionId::TestObservability)));
    assert!(effects.is_empty());
    assert!(!app.observability_testing());
    let error = app
        .observability()
        .base_url_error()
        .expect("base URL error");
    assert!(error.contains("http or https"), "{error}");
}

#[test]
fn observability_errors_render_without_secrets() {
    let (mut app, _store, _dir) = connected_app();
    open_observability(&mut app);
    fill_observability(&mut app, OBS_SECRET);
    let (_terminal, hits) = draw(&mut app, 120, 50);
    let _ = app.apply(click(&hits, Intent::Activate(ActionId::TestObservability)));
    let epoch = app.epoch();
    app.handle_worker(WorkerEvent::ObservabilityTested {
        epoch,
        result: Err(ObservabilityError::Http {
            status: 401,
            detail: " Unauthorized".to_owned(),
        }),
    });

    let (terminal, _hits) = draw(&mut app, 120, 50);
    let text = buffer_text(&terminal);
    assert!(text.contains("HTTP 401 Unauthorized"), "{text}");
    assert!(!text.contains(OBS_SECRET));
    assert!(!text.contains("Authorization"));
}
