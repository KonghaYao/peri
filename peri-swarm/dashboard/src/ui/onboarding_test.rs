use ratatui::backend::TestBackend;
use ratatui::Terminal;

use super::*;
use crate::app::{App, FieldId};
use crate::config::ConfigStore;
use crate::ui::hit::HitMap;

fn test_terminal(width: u16, height: u16) -> Terminal<TestBackend> {
    Terminal::new(TestBackend::new(width, height)).expect("test terminal")
}

#[test]
fn cleartext_warning_only_for_non_loopback_http() {
    assert!(!cleartext_warning("http://127.0.0.1:8080"));
    assert!(!cleartext_warning("http://localhost:8080"));
    assert!(!cleartext_warning("http://[::1]:8080"));
    assert!(cleartext_warning("http://swarm.example.com"));
    assert!(!cleartext_warning("https://swarm.example.com"));
    assert!(!cleartext_warning("not a url"));
}

#[test]
fn click_submits_after_typing_into_the_key_field() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ConfigStore::at(dir.path().join("config.toml"));
    let mut app = App::onboarding(store.clone());
    let mut terminal = test_terminal(100, 30);
    let mut hits = HitMap::default();
    terminal
        .draw(|frame| hits = crate::ui::render(frame, &mut app))
        .expect("draw");

    let key_rect = hits
        .region_rect(Intent::FocusField {
            field: FieldId::OnboardingApiKey,
            click: None,
        })
        .expect("key field hit");
    let focus = hits.click(false, key_rect.x + 1, key_rect.y + 1).unwrap();
    assert!(matches!(focus, Intent::FocusField { click: Some(_), .. }));
    app.apply(focus);
    app.handle_paste("secret-key");

    let submit = hits
        .region_rect(Intent::Activate(ActionId::Submit))
        .expect("submit hit");
    let intent = hits.click(false, submit.x, submit.y).unwrap();
    let effects = app.apply(intent);
    assert!(matches!(
        effects.as_slice(),
        [crate::app::Effect::Validate { .. }]
    ));
    assert!(!store.path().exists(), "validation must not write the file");
}

#[test]
fn submit_without_a_key_shows_a_field_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ConfigStore::at(dir.path().join("config.toml"));
    let mut app = App::onboarding(store);
    let effects = app.apply(Intent::Activate(ActionId::Submit));
    assert!(effects.is_empty());
    assert!(app.onboarding_form().api_key_error().is_some());
}
