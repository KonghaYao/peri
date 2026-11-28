use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Terminal;

use super::*;
use crate::app::{App, Focus, Intent, Overlay, Page};
use crate::config::{self, Config, ConfigStore};
use crate::ui::hit::HitMap;

const SECRET: &str = "mouse-test-key-never-displayed";

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

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn walk(app: &mut App, hits: &HitMap, event: MouseEvent) -> Vec<Effect> {
    handle_mouse(app, hits, &mut None, event)
}

#[test]
fn clicking_a_field_focuses_it_and_places_the_caret() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ConfigStore::at(dir.path().join("config.toml"));
    let mut app = App::onboarding(store);
    app.set_field_value(crate::app::FieldId::OnboardingApiKey, "abcdef");
    let (_terminal, hits) = draw(&mut app, 100, 30);

    let rect = hits
        .region_rect(Intent::FocusField {
            field: crate::app::FieldId::OnboardingApiKey,
            click: None,
        })
        .expect("key field");
    walk(
        &mut app,
        &hits,
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            rect.x + 3,
            rect.y + 1,
        ),
    );
    assert_eq!(
        app.focus(),
        Focus::Field(crate::app::FieldId::OnboardingApiKey)
    );
    assert_eq!(
        app.field(crate::app::FieldId::OnboardingApiKey)
            .expect("field")
            .caret(),
        2
    );
}

#[test]
fn clicking_submit_starts_validation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ConfigStore::at(dir.path().join("config.toml"));
    let mut app = App::onboarding(store.clone());
    app.set_field_value(crate::app::FieldId::OnboardingApiKey, SECRET);
    let (_terminal, hits) = draw(&mut app, 100, 30);
    let button = hits
        .region_rect(Intent::Activate(crate::app::ActionId::Submit))
        .expect("submit button");

    let effects = walk(
        &mut app,
        &hits,
        mouse(MouseEventKind::Down(MouseButton::Left), button.x, button.y),
    );
    assert!(matches!(effects.as_slice(), [Effect::Validate { .. }]));
    assert!(!store.path().exists());
}

#[test]
fn modal_swallows_background_and_backdrop_clicks() {
    let (mut app, _store, _dir) = connected_app();
    app.apply(Intent::OpenSignOut);
    let (_terminal, hits) = draw(&mut app, 100, 30);
    let sidebar = hits
        .region_rect(Intent::SelectPage(Page::Agents))
        .expect("sidebar hit");

    walk(
        &mut app,
        &hits,
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            sidebar.x,
            sidebar.y,
        ),
    );
    assert_eq!(app.page(), Page::SessionLog);
    assert_eq!(app.overlay(), Some(Overlay::ConfirmSignOut));

    // The backdrop of a destructive confirmation never dismisses it.
    walk(
        &mut app,
        &hits,
        mouse(MouseEventKind::Down(MouseButton::Left), 0, 0),
    );
    assert_eq!(app.overlay(), Some(Overlay::ConfirmSignOut));

    let cancel = hits
        .region_rect(Intent::Activate(crate::app::ActionId::CancelSignOut))
        .expect("cancel");
    walk(
        &mut app,
        &hits,
        mouse(MouseEventKind::Down(MouseButton::Left), cancel.x, cancel.y),
    );
    assert_eq!(app.overlay(), None);
}

#[test]
fn backdrop_closes_help_and_the_narrow_menu() {
    let (mut app, _store, _dir) = connected_app();
    app.apply(Intent::OpenHelp);
    let (_terminal, hits) = draw(&mut app, 100, 30);
    walk(
        &mut app,
        &hits,
        mouse(MouseEventKind::Down(MouseButton::Left), 0, 0),
    );
    assert_eq!(app.overlay(), None);

    app.apply(Intent::ToggleMenu);
    let (_terminal, hits) = draw(&mut app, 79, 24);
    walk(
        &mut app,
        &hits,
        mouse(MouseEventKind::Down(MouseButton::Left), 0, 0),
    );
    assert_eq!(app.overlay(), None);
}

#[test]
fn wheel_scrolls_the_zone_under_the_pointer() {
    let (mut app, _store, _dir) = connected_app();
    let (_terminal, hits) = draw(&mut app, 80, 12);

    walk(&mut app, &hits, mouse(MouseEventKind::ScrollDown, 2, 5));
    assert!(app.sidebar_scroll() > 0, "sidebar wheel");

    let body_x = super::super::shell::sidebar_width(80).expect("sidebar") + 2;
    walk(
        &mut app,
        &hits,
        mouse(MouseEventKind::ScrollDown, body_x, 5),
    );
    assert!(app.content_scroll() > 0, "content wheel");
}

#[test]
fn dragging_the_thumb_moves_the_sidebar() {
    let (mut app, _store, _dir) = connected_app();
    let (_terminal, hits) = draw(&mut app, 80, 14);
    let bar = hits
        .scrollbars()
        .iter()
        .find(|bar| bar.zone == ScrollZone::Sidebar)
        .copied()
        .expect("sidebar scrollbar");
    let thumb_row = bar
        .area
        .y
        .saturating_add(1)
        .saturating_add(u16::try_from(bar.bar.thumb_start).expect("thumb"));
    let mut drag = None;

    handle_mouse(
        &mut app,
        &hits,
        &mut drag,
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            bar.area.x,
            thumb_row,
        ),
    );
    assert!(drag.is_some(), "pressing the thumb starts a drag");

    handle_mouse(
        &mut app,
        &hits,
        &mut drag,
        mouse(
            MouseEventKind::Drag(MouseButton::Left),
            bar.area.x,
            bar.area.bottom().saturating_sub(1),
        ),
    );
    assert_eq!(app.sidebar_scroll(), bar.bar.max_offset());

    handle_mouse(
        &mut app,
        &hits,
        &mut drag,
        mouse(
            MouseEventKind::Up(MouseButton::Left),
            bar.area.x,
            bar.area.bottom().saturating_sub(1),
        ),
    );
    assert!(drag.is_none());
}

#[test]
fn the_wheel_scrolls_the_help_overlay() {
    let (mut app, _store, _dir) = connected_app();
    app.apply(Intent::OpenHelp);
    let (_terminal, hits) = draw(&mut app, 80, 24);
    assert_eq!(app.help_scroll(), 0);
    walk(&mut app, &hits, mouse(MouseEventKind::ScrollDown, 40, 12));
    assert!(app.help_scroll() > 0, "wheel down scrolls help");
    let offset = app.help_scroll();
    walk(&mut app, &hits, mouse(MouseEventKind::ScrollUp, 40, 12));
    assert!(app.help_scroll() < offset, "wheel up scrolls back");
    assert_eq!(app.overlay(), Some(Overlay::Help));
}
