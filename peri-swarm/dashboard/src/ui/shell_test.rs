use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

use super::*;
use crate::app::{ActionId, App, Intent, Overlay, Page, Screen};
use crate::config::{self, Config, ConfigStore};
use crate::ui::hit::{HitMap, ScrollZone};
use crate::ui::text;

const SECRET: &str = "ui-test-key-never-displayed";

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

#[test]
fn breakpoints_pick_sidebar_widths_and_the_menu_button() {
    assert_eq!(sidebar_width(140), Some(SIDEBAR_WIDE));
    assert_eq!(sidebar_width(120), Some(SIDEBAR_WIDE));
    assert_eq!(sidebar_width(119), Some(SIDEBAR_MEDIUM));
    assert_eq!(sidebar_width(90), Some(SIDEBAR_MEDIUM));
    assert_eq!(sidebar_width(89), Some(SIDEBAR_NARROW));
    assert_eq!(sidebar_width(80), Some(SIDEBAR_NARROW));
    assert_eq!(sidebar_width(79), None);

    assert!(too_small(Rect::new(0, 0, 49, 12)));
    assert!(too_small(Rect::new(0, 0, 50, 11)));
    assert!(!too_small(Rect::new(0, 0, 50, 12)));
}

#[test]
fn every_sidebar_item_is_clickable_at_all_supported_sizes() {
    for (width, height) in [(80, 24), (100, 30), (140, 40)] {
        let (mut app, _store, _dir) = connected_app();
        let (_terminal, hits) = draw(&mut app, width, height);
        for page in Page::ALL {
            if page == app.page() {
                continue;
            }
            let rect = hits
                .region_rect(Intent::SelectPage(page))
                .unwrap_or_else(|| panic!("missing sidebar hit for {page:?} at {width}x{height}"));
            let intent = hits
                .click(false, rect.x, rect.y)
                .unwrap_or_else(|| panic!("sidebar click missed for {page:?}"));
            assert_eq!(intent, Intent::SelectPage(page));
            app.apply(intent);
            assert_eq!(app.page(), page);
        }
        assert_eq!(app.screen(), Screen::Main);
    }
}

#[test]
fn narrow_width_hides_the_sidebar_and_opens_the_menu_overlay() {
    let (mut app, _store, _dir) = connected_app();
    let (_terminal, hits) = draw(&mut app, 79, 24);
    assert!(hits.region_rect(Intent::SelectPage(Page::Vaults)).is_none());

    let menu = hits
        .region_rect(Intent::ToggleMenu)
        .expect("menu button hit");
    let intent = hits.click(false, menu.x, menu.y).expect("menu click");
    app.apply(intent);
    assert_eq!(app.overlay(), Some(Overlay::NarrowMenu));

    let (_terminal, hits) = draw(&mut app, 79, 24);
    let vaults = hits
        .region_rect(Intent::SelectMenuPage(Page::Vaults))
        .expect("menu entry");
    let intent = hits
        .click(true, vaults.x, vaults.y)
        .expect("menu entry click");
    app.apply(intent);
    assert_eq!(app.overlay(), None);
    assert_eq!(app.page(), Page::Vaults);
}

#[test]
fn modal_over_sign_out_blocks_background_clicks() {
    let (mut app, store, _dir) = connected_app();
    app.apply(Intent::OpenSignOut);
    let (_terminal, hits) = draw(&mut app, 100, 30);

    let sidebar = hits
        .region_rect(Intent::SelectPage(Page::Agents))
        .expect("background hit exists");
    assert!(
        hits.click(true, sidebar.x, sidebar.y).is_none(),
        "base layer must not receive clicks under a modal"
    );
    assert!(
        hits.outside_overlay(sidebar.x, sidebar.y),
        "the sidebar row lies outside the centred modal, so the click is a backdrop click"
    );

    let cancel = hits
        .region_rect(Intent::Activate(ActionId::CancelSignOut))
        .expect("cancel button");
    let intent = hits.click(true, cancel.x, cancel.y).expect("cancel click");
    app.apply(intent);
    assert_eq!(app.overlay(), None);
    assert!(store.path().exists());

    app.apply(Intent::OpenSignOut);
    let (_terminal, hits) = draw(&mut app, 100, 30);
    let panel = hits.overlay_area().expect("modal area");
    assert!(hits.outside_overlay(0, 0));
    assert!(!hits.outside_overlay(panel.x, panel.y));
    let confirm = hits
        .region_rect(Intent::Activate(ActionId::ConfirmSignOut))
        .expect("confirm button");
    let intent = hits
        .click(true, confirm.x, confirm.y)
        .expect("confirm click");
    app.apply(intent);
    assert_eq!(app.screen(), Screen::Onboarding);
    assert!(!store.path().exists());
}

#[test]
fn too_small_terminal_does_not_panic_and_still_quits() {
    let (mut app, _store, _dir) = connected_app();
    let (_terminal, hits) = draw(&mut app, 49, 11);
    assert!(hits.too_small);
    assert!(hits.click(false, 0, 0).is_none());

    let effects = app.handle_key(ratatui::crossterm::event::KeyEvent::new(
        ratatui::crossterm::event::KeyCode::Char('c'),
        ratatui::crossterm::event::KeyModifiers::CONTROL,
    ));
    assert!(matches!(effects.as_slice(), [crate::app::Effect::Quit]));
}

#[test]
fn every_page_renders_with_its_label() {
    let (mut app, _store, _dir) = connected_app();
    for page in Page::ALL {
        app.apply(Intent::SelectPage(page));
        let (terminal, _hits) = draw(&mut app, 100, 30);
        let text = buffer_text(&terminal);
        assert!(
            text.contains(text::page_label(page)),
            "page label missing for {page:?}"
        );
    }

    // The same pages must not panic at the smallest supported size.
    for page in Page::ALL {
        app.apply(Intent::SelectPage(page));
        let _ = draw(&mut app, 50, 12);
    }
}

#[test]
fn wheel_zones_and_sidebar_scrollbar_agree() {
    let (mut app, _store, _dir) = connected_app();
    let (width, height) = (80, 14);
    let (_terminal, hits) = draw(&mut app, width, height);

    assert_eq!(
        hits.wheel_zone(false, 2, 5),
        Some(ScrollZone::Sidebar),
        "point over the sidebar must scroll the sidebar"
    );
    let body_x = sidebar_width(width).expect("sidebar") + 2;
    assert_eq!(hits.wheel_zone(false, body_x, 5), Some(ScrollZone::Content));

    let bars = hits.scrollbars();
    let sidebar_bar = bars
        .iter()
        .find(|bar| bar.zone == ScrollZone::Sidebar)
        .copied()
        .expect("sidebar scrollbar");
    assert!(!sidebar_bar.area.is_empty());
    app.apply(Intent::ScrollSidebar(1));
    assert!(app.sidebar_scroll() > 0);
}
