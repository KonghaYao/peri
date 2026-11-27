use ratatui::backend::TestBackend;
use ratatui::Terminal;

use super::*;
use crate::app::{App, FieldId, Intent, Page, Screen};
use crate::config::{self, Config, ConfigStore};
use crate::ui::hit::HitMap;

const SECRET: &str = "settings-test-key-never-displayed";

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
