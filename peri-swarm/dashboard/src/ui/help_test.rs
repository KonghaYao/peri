use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use tempfile::TempDir;

use crate::app::{App, Intent, Overlay};
use crate::config::{self, Config, ConfigStore};
use crate::ui::hit::HitMap;
use crate::ui::text;

fn connected_app() -> (App, TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ConfigStore::at(dir.path().join("config.toml"));
    let config = Config::new(config::DEFAULT_BASE_URL, "synthetic-key").expect("config");
    store.save(&config).expect("save");
    (App::connected(config, Vec::new(), store), dir)
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

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn help_content_scrolls_to_the_last_section() {
    let (mut app, _dir) = connected_app();
    app.apply(Intent::OpenHelp);
    let (terminal, hits) = draw(&mut app, 80, 24);
    let first = buffer_text(&terminal);
    assert!(first.contains("Credentials"), "{first}");
    // The last section does not fit on the first screen.
    assert!(!first.contains("Mouse and keyboard"), "{first}");
    // The page scrollbar behind the overlay must not be visible.
    assert!(hits.scrollbars().is_empty(), "no background scrollbar");

    // End jumps to the bottom; the overlay clamps against the measured height.
    app.handle_key(key(KeyCode::End));
    let (terminal, _hits) = draw(&mut app, 80, 24);
    let last = buffer_text(&terminal);
    assert!(last.contains("Mouse and keyboard"), "{last}");
    assert!(last.contains("Ctrl-C quits from anywhere"), "{last}");
    assert!(last.contains(text::HELP_HINT), "{last}");

    // Down/Up move one row and PgUp returns towards the top.
    let bottom = app.help_scroll();
    assert!(bottom > 0);
    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.help_scroll(), bottom);
    app.handle_key(key(KeyCode::PageUp));
    assert!(app.help_scroll() < bottom);
    app.handle_key(key(KeyCode::Home));
    assert_eq!(app.help_scroll(), 0);
}

#[test]
fn help_escape_still_closes_and_reset_starts_at_the_top() {
    let (mut app, _dir) = connected_app();
    app.apply(Intent::OpenHelp);
    let _ = draw(&mut app, 80, 24);
    app.handle_key(key(KeyCode::End));
    assert!(app.help_scroll() > 0);
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.overlay(), None);

    app.apply(Intent::OpenHelp);
    assert_eq!(app.overlay(), Some(Overlay::Help));
    assert_eq!(app.help_scroll(), 0);
}

#[test]
fn the_help_footer_matches_the_overlay_keys() {
    let (mut app, _dir) = connected_app();
    app.apply(Intent::OpenHelp);
    let (terminal, _hits) = draw(&mut app, 120, 40);
    let footer = buffer_text(&terminal);
    assert!(footer.contains(text::FOOTER_HELP), "{footer}");
    assert!(!footer.contains("click a choice"), "{footer}");
}
