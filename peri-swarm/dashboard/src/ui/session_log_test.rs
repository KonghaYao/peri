use ratatui::backend::TestBackend;
use ratatui::Terminal;

use crate::api::ApiError;
use crate::app::{ActionId, App, FieldId, Focus, Intent, Page};
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

        for action in [
            ActionId::NewSession,
            ActionId::RefreshSessions,
            ActionId::LoadMoreSessions,
        ] {
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
        assert_eq!(app.screen(), crate::app::Screen::SessionDetail);
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
    assert!(!text.contains(text::column_header(Column::Metadata)));

    let (terminal, _) = draw(&mut app, 80, 30);
    let text = buffer_text(&terminal);
    assert!(text.contains("2 pending"));
    assert!(!text.contains(text::column_header(Column::Metadata)));
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
fn five_hundred_rows_and_load_more_stay_reachable_by_scrolling() {
    let (mut app, _dir) = connected_app();
    let sessions: Vec<_> = (0..500)
        .map(|index| {
            session(
                &format!("sess_{index:03}"),
                "idle",
                "idle",
                None,
                NOW - i64::from(index),
            )
        })
        .collect();
    feed_page(&mut app, sessions);

    // At the top only the first rows are visible; the tail is not a hit target.
    let (terminal, hits) = draw(&mut app, 100, 24);
    let first = Intent::OpenSession("sess_000".to_owned());
    assert!(hits.region_rect(first.clone()).is_some());
    assert!(
        hits.region_rect(Intent::OpenSession("sess_499".to_owned()))
            .is_none(),
        "a row outside the viewport must not be clickable"
    );
    assert!(!buffer_text(&terminal).contains("sess_499"));

    // Scrolling to the measured bottom reaches the last row and the footer.
    app.set_content_scroll(usize::MAX);
    let (terminal, hits) = draw(&mut app, 100, 24);
    assert_eq!(app.content_scroll(), app.content_max());
    assert!(buffer_text(&terminal).contains("sess_499"));
    let last = Intent::OpenSession("sess_499".to_owned());
    let rect = hits.region_rect(last.clone()).expect("last row reachable");
    assert_eq!(hits.click(false, rect.x, rect.y), Some(last));
    assert!(rect.y >= 2 && rect.bottom() <= 24, "row is inside the body");

    let load = Intent::Activate(ActionId::LoadMoreSessions);
    let rect = hits.region_rect(load.clone()).expect("load more reachable");
    assert_eq!(hits.click(false, rect.x, rect.y), Some(load));
    assert!(
        rect.y >= 2 && rect.bottom() <= 24,
        "footer is inside the body"
    );
}
