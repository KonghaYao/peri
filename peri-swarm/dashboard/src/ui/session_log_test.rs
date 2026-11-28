use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

use crate::api::ApiError;
use crate::app::{ActionId, App, FieldId, Focus, Intent, Page, Screen};
use crate::test_support::{
    connected_app, feed_page, list_request, session, with_approvals, with_metadata,
};
use crate::ui::hit::HitMap;
use crate::ui::text;
use crate::view_model::{Column, StatusBucket};
use crate::worker::WorkerEvent;

const NOW: i64 = 1_800_000_000;

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

fn rect_text(terminal: &Terminal<TestBackend>, rect: Rect) -> String {
    let buffer = terminal.backend().buffer();
    (rect.x..rect.right())
        .map(|column| buffer[(column, rect.y)].symbol())
        .collect()
}

/// `count` idle sessions named `sess_000`, `sess_001`, ...
fn sessions(count: usize) -> Vec<crate::api::Session> {
    (0..count)
        .map(|index| session(&format!("sess_{index:03}"), "idle", "idle", None, NOW))
        .collect()
}

/// Draw with the content scrolled to its measured bottom.
fn draw_scrolled(app: &mut App, width: u16, height: u16) -> (Terminal<TestBackend>, HitMap) {
    draw(app, width, height);
    app.set_content_scroll(usize::MAX);
    draw(app, width, height)
}

#[test]
fn toolbar_chips_and_rows_are_clickable_at_every_supported_width() {
    for (width, height) in [(80, 24), (100, 30), (140, 40)] {
        let (mut app, _dir) = connected_app();
        feed_page(
            &mut app,
            vec![
                session("sess_a", "in_progress", "running", Some("gpt-5"), NOW - 5),
                session("sess_b", "idle", "idle", None, NOW - 30),
            ],
        );
        let (_terminal, hits) = draw(&mut app, width, height);

        for action in [ActionId::NewSession, ActionId::RefreshSessions] {
            let intent = Intent::Activate(action);
            let rect = hits
                .region_rect(intent.clone())
                .unwrap_or_else(|| panic!("missing {action:?} at {width}"));
            assert_eq!(hits.click(false, rect.x, rect.y), Some(intent));
        }
        for bucket in StatusBucket::ALL {
            let intent = Intent::SetFilter(bucket);
            let rect = hits
                .region_rect(intent.clone())
                .unwrap_or_else(|| panic!("missing chip {bucket:?} at {width}"));
            assert_eq!(hits.click(false, rect.x, rect.y), Some(intent));
        }

        let row_intent = Intent::OpenSession("sess_a".to_owned());
        let row = hits
            .region_rect(row_intent.clone())
            .unwrap_or_else(|| panic!("missing row at {width}"));
        assert_eq!(hits.click(false, row.x, row.y), Some(row_intent));
        app.apply(Intent::OpenSession("sess_a".to_owned()));
        assert_eq!(app.page(), Page::SessionLog);
        assert_eq!(app.screen(), Screen::SessionDetail);
    }
}

#[test]
fn status_approvals_and_metadata_follow_the_column_breakpoints() {
    let (mut app, _dir) = connected_app();
    feed_page(
        &mut app,
        vec![with_approvals(
            with_metadata(
                session("sess_a", "requires_action", "waiting", Some("gpt-5"), NOW),
                &[("team", "core")],
            ),
            2,
        )],
    );

    let (terminal, _) = draw(&mut app, 140, 30);
    let text = buffer_text(&terminal);
    assert!(text.contains("gpt-5"));
    assert!(text.contains("Requires action"));
    assert!(text.contains("2 pending"));
    assert!(text.contains("team=core"));
    assert!(text.contains(text::column_header(Column::Metadata)));

    let (terminal, _) = draw(&mut app, 100, 30);
    let text = buffer_text(&terminal);
    assert!(text.contains("2 pending"));
    // The summary is not drawn, but its absence is announced.
    assert!(!text.contains("team=core"));
    assert!(text.contains(text::METADATA_HIDDEN), "{text}");

    let (terminal, _) = draw(&mut app, 80, 30);
    let text = buffer_text(&terminal);
    // Compact tier: approvals give way to the metadata summary, and the
    // session id keeps a distinguishing tail.
    assert!(text.contains("team=core"), "{text}");
    assert!(
        text.contains(text::column_header(Column::Metadata)),
        "{text}"
    );
    assert!(!text.contains("2 pending"), "{text}");
}

#[test]
fn a_narrow_table_still_distinguishes_sessions_sharing_a_prefix() {
    let (mut app, _dir) = connected_app();
    feed_page(
        &mut app,
        vec![
            session("lt-20261128-003321", "idle", "idle", None, NOW),
            session("lt-20261128-003322", "idle", "idle", None, NOW),
        ],
    );
    let (terminal, _) = draw(&mut app, 80, 24);
    let text = buffer_text(&terminal);
    assert!(text.contains("003321"), "{text}");
    assert!(text.contains("003322"), "{text}");
}

#[test]
fn a_401_pause_replaces_the_spinner_instead_of_loading_forever() {
    let (mut app, _dir) = connected_app();
    app.handle_worker(WorkerEvent::HealthFinished(crate::worker::HealthReport {
        epoch: app.epoch(),
        result: Err(ApiError::from_status(
            401,
            br#"{"error":{"code":"unauthorized","message":"bad key","details":null}}"#,
        )),
        latency: None,
        checked_at: std::time::Instant::now(),
        contract: None,
    }));
    assert!(app.is_paused());
    let (terminal, _) = draw(&mut app, 100, 30);
    let text = buffer_text(&terminal);
    // The message wraps across rows, so match its first phrase.
    assert!(text.contains("Polling paused"), "{text}");
    assert!(text.contains("rejected the API key"), "{text}");
    assert!(!text.contains(text::LOADING_SESSIONS), "{text}");
}

#[test]
fn loading_empty_and_error_states_render_without_panicking() {
    let (mut app, _dir) = connected_app();
    let (terminal, _) = draw(&mut app, 100, 30);
    assert!(buffer_text(&terminal).contains(text::LOADING_SESSIONS));

    feed_page(&mut app, Vec::new());
    let (terminal, hits) = draw(&mut app, 100, 30);
    let text = buffer_text(&terminal);
    assert!(text.contains(text::EMPTY_SESSIONS_TITLE));
    assert!(hits
        .region_rect(Intent::Activate(ActionId::NewSession))
        .is_some());

    let effects = app.apply(Intent::RefreshSessions);
    let (epoch, generation, limit) = list_request(&effects);
    app.handle_worker(WorkerEvent::SessionsLoaded {
        epoch,
        generation,
        limit,
        result: Err(ApiError::from_status(
            500,
            br#"{"error":{"code":"internal","message":"boom","details":null}}"#,
        )),
    });
    let (terminal, _) = draw(&mut app, 100, 30);
    assert!(buffer_text(&terminal).contains(&text::list_error("")));
}

#[test]
fn search_click_focuses_the_field_and_typing_filters_rows() {
    let (mut app, _dir) = connected_app();
    feed_page(
        &mut app,
        vec![
            session("sess_a", "idle", "idle", None, NOW),
            session("sess_b", "idle", "idle", None, NOW),
        ],
    );
    let (_terminal, hits) = draw(&mut app, 100, 30);
    let intent = Intent::FocusField {
        field: FieldId::SessionSearch,
        click: None,
    };
    let rect = hits.region_rect(intent).expect("search field");
    let clicked = hits
        .click(false, rect.x.saturating_add(2), rect.y)
        .expect("search click");
    app.apply(clicked);
    assert_eq!(app.focus(), Focus::Field(FieldId::SessionSearch));

    app.handle_key(ratatui::crossterm::event::KeyEvent::new(
        ratatui::crossterm::event::KeyCode::Char('b'),
        ratatui::crossterm::event::KeyModifiers::NONE,
    ));
    assert_eq!(app.session_log().search().value(), "b");
    assert_eq!(app.session_log().visible_len(), 1);
}

#[test]
fn filter_chip_click_changes_the_visible_rows() {
    let (mut app, _dir) = connected_app();
    feed_page(
        &mut app,
        vec![
            session("sess_a", "failed", "failed", None, NOW),
            session("sess_b", "idle", "idle", None, NOW),
        ],
    );
    let (_terminal, hits) = draw(&mut app, 100, 30);
    let intent = Intent::SetFilter(StatusBucket::Failed);
    let rect = hits.region_rect(intent).expect("failed chip");
    let clicked = hits.click(false, rect.x, rect.y).expect("chip click");
    app.apply(clicked);
    assert_eq!(app.session_log().filter(), StatusBucket::Failed);
    assert_eq!(app.session_log().visible_len(), 1);
}

#[test]
fn pager_regions_cover_the_drawn_labels() {
    let (mut app, _dir) = connected_app();
    feed_page(&mut app, sessions(25));
    app.apply(Intent::GoToSessionPage(1));
    let (terminal, hits) = draw_scrolled(&mut app, 100, 40);

    // Page 2 of 3, so every page number is drawn.
    for (page, label) in [(0, "1"), (1, "2"), (2, "3")] {
        let intent = Intent::GoToSessionPage(page);
        let rect = hits
            .region_rect(intent.clone())
            .unwrap_or_else(|| panic!("missing page {page} region"));
        assert_eq!(rect_text(&terminal, rect), label);
        assert_eq!(hits.click(false, rect.x, rect.y), Some(intent));
    }
    for (intent, label) in [
        (Intent::SessionPagePrev, text::PAGE_PREV),
        (Intent::SessionPageNext, text::PAGE_NEXT),
    ] {
        let rect = hits
            .region_rect(intent.clone())
            .unwrap_or_else(|| panic!("missing {intent:?} region"));
        assert_eq!(rect_text(&terminal, rect), label);
        assert_eq!(hits.click(false, rect.x, rect.y), Some(intent));
    }

    let previous = hits.region_rect(Intent::SessionPagePrev).expect("previous");
    let first = hits
        .region_rect(Intent::GoToSessionPage(0))
        .expect("first page");
    assert_eq!(
        first.x,
        previous.right() + 1,
        "controls are one space apart"
    );
}

#[test]
fn clicking_a_page_number_and_next_moves_the_table() {
    let (mut app, _dir) = connected_app();
    feed_page(&mut app, sessions(25));
    let (terminal, hits) = draw_scrolled(&mut app, 100, 40);
    assert!(buffer_text(&terminal).contains("sess_000"));
    assert!(!buffer_text(&terminal).contains("sess_010"));

    let intent = Intent::GoToSessionPage(1);
    let rect = hits.region_rect(intent.clone()).expect("page 2");
    app.apply(hits.click(false, rect.x, rect.y).expect("page click"));
    assert_eq!(app.session_log().page(), 1);
    assert_eq!(app.focus(), Focus::Content);

    let (terminal, hits) = draw_scrolled(&mut app, 100, 40);
    assert!(buffer_text(&terminal).contains("sess_010"));
    assert!(!buffer_text(&terminal).contains("sess_000"));

    let next = Intent::SessionPageNext;
    let rect = hits.region_rect(next.clone()).expect("next");
    app.apply(hits.click(false, rect.x, rect.y).expect("next click"));
    assert_eq!(app.session_log().page(), 2);

    let (terminal, _) = draw_scrolled(&mut app, 100, 40);
    let text = buffer_text(&terminal);
    assert!(text.contains("sess_020"));
    assert!(text.contains("sess_024"));
    assert!(!text.contains("sess_010"));
}

#[test]
fn disabled_pager_controls_are_not_clickable() {
    let (mut app, _dir) = connected_app();
    feed_page(&mut app, sessions(25));

    // First page: Previous is drawn but inert.
    let (terminal, hits) = draw_scrolled(&mut app, 100, 40);
    assert!(hits.region_rect(Intent::SessionPagePrev).is_none());
    let current = hits
        .region_rect(Intent::GoToSessionPage(0))
        .expect("current page region");
    assert_eq!(rect_text(&terminal, current), "1");
    assert_eq!(
        hits.click(false, current.x.saturating_sub(7), current.y),
        None
    );

    // Last page without more data: Next is drawn but inert.
    app.apply(Intent::GoToSessionPage(2));
    let (terminal, hits) = draw_scrolled(&mut app, 100, 40);
    assert!(hits.region_rect(Intent::SessionPageNext).is_none());
    assert!(hits.region_rect(Intent::SessionPagePrev).is_some());
    let current = hits
        .region_rect(Intent::GoToSessionPage(2))
        .expect("current page region");
    assert_eq!(rect_text(&terminal, current), "3");
    assert_eq!(
        hits.click(false, current.right().saturating_add(1), current.y),
        None
    );
}

#[test]
fn the_table_renders_only_the_current_page() {
    let (mut app, _dir) = connected_app();
    feed_page(&mut app, sessions(25));
    let (terminal, _) = draw_scrolled(&mut app, 100, 40);
    let text = buffer_text(&terminal);
    for index in 0..10 {
        assert!(text.contains(&format!("sess_{index:03}")));
    }
    for index in 10..25 {
        assert!(
            !text.contains(&format!("sess_{index:03}")),
            "sess_{index:03} belongs to a later page"
        );
    }
}

#[test]
fn a_single_page_still_shows_the_pager() {
    let (mut app, _dir) = connected_app();
    feed_page(&mut app, sessions(3));
    let (terminal, hits) = draw_scrolled(&mut app, 100, 24);
    let text = buffer_text(&terminal);
    assert!(text.contains(text::PAGE_PREV));
    assert!(text.contains(text::PAGE_NEXT));
    let current = hits
        .region_rect(Intent::GoToSessionPage(0))
        .expect("page 1 region");
    assert_eq!(rect_text(&terminal, current), "1");
    assert!(hits.region_rect(Intent::SessionPagePrev).is_none());
    assert!(hits.region_rect(Intent::SessionPageNext).is_none());
}

#[test]
fn paging_keys_and_hints_are_documented() {
    let (mut app, _dir) = connected_app();
    feed_page(&mut app, sessions(25));
    let (terminal, _) = draw(&mut app, 140, 40);
    let text = buffer_text(&terminal);
    assert!(text::FOOTER_SESSION_LOG.contains("PgUp/PgDn page"));
    assert!(text.contains("PgUp/PgDn page"));
    assert!(text::HELP_SECTIONS
        .iter()
        .any(|section| section.lines.iter().any(|line| line.contains("PgUp/PgDn"))));
    assert!(!text.contains("Load more"));

    app.apply(Intent::OpenHelp);
    let (terminal, _) = draw(&mut app, 140, 40);
    assert!(buffer_text(&terminal).contains("PgUp/PgDn"));
    app.apply(Intent::CloseOverlay);
    assert_eq!(app.session_log().page(), 0);

    app.handle_key(ratatui::crossterm::event::KeyEvent::new(
        ratatui::crossterm::event::KeyCode::PageDown,
        ratatui::crossterm::event::KeyModifiers::NONE,
    ));
    assert_eq!(app.session_log().page(), 1);
    app.handle_key(ratatui::crossterm::event::KeyEvent::new(
        ratatui::crossterm::event::KeyCode::PageUp,
        ratatui::crossterm::event::KeyModifiers::NONE,
    ));
    assert_eq!(app.session_log().page(), 0);
}

#[test]
fn five_hundred_rows_page_ten_at_a_time_and_the_last_page_is_reachable() {
    let (mut app, _dir) = connected_app();
    feed_page(&mut app, sessions(500));
    assert_eq!(app.session_log().page_count(), 50);

    // At the top only the first page's rows are drawn.
    let (terminal, _) = draw(&mut app, 100, 24);
    assert!(buffer_text(&terminal).contains("sess_000"));
    assert!(!buffer_text(&terminal).contains("sess_010"));

    // The pager stays inside the body once the page is scrolled to its bottom.
    let (_terminal, hits) = draw_scrolled(&mut app, 100, 24);
    let next = Intent::SessionPageNext;
    let rect = hits.region_rect(next.clone()).expect("next reachable");
    assert_eq!(hits.click(false, rect.x, rect.y), Some(next));
    assert!(
        rect.y >= 2 && rect.bottom() <= 24,
        "pager is inside the body"
    );

    let last = Intent::GoToSessionPage(49);
    let rect = hits
        .region_rect(last.clone())
        .expect("last page reachable from the pager");
    assert_eq!(hits.click(false, rect.x, rect.y), Some(last.clone()));
    app.apply(last);
    assert_eq!(app.session_log().page(), 49);

    let (terminal, hits) = draw_scrolled(&mut app, 100, 24);
    assert!(buffer_text(&terminal).contains("sess_499"));
    let row = Intent::OpenSession("sess_499".to_owned());
    let rect = hits.region_rect(row.clone()).expect("last row reachable");
    assert_eq!(hits.click(false, rect.x, rect.y), Some(row));
}
