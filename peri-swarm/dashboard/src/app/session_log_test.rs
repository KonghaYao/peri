use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::*;
use crate::app::{ActionId, App, Effect, FieldId, Focus, Intent, Page, Screen};
use crate::test_support::{
    connected_app, feed_page, feed_page_more, finish_page, finish_page_more, list_request,
    page_with, refresh_page, session, with_approvals, with_metadata,
};
use crate::view_model::StatusBucket;
use crate::worker::WorkerEvent;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn due(app: &mut App, now: Instant) -> Vec<Effect> {
    app.session_list_due(now).into_iter().collect()
}

/// `count` idle sessions named `sess_000`, `sess_001`, ...
fn sessions(count: usize) -> Vec<crate::api::Session> {
    (0..count)
        .map(|index| session(&format!("sess_{index:03}"), "idle", "idle", None, 1))
        .collect()
}

#[test]
fn first_page_loads_and_updates_state() {
    let (mut app, _dir) = connected_app();
    let effects = due(&mut app, Instant::now());
    assert_eq!(effects.len(), 1);
    let (epoch, generation, limit) = list_request(&effects);
    assert_eq!(limit, FETCH_STEP);
    assert!(app.session_log().loading());

    finish_page(
        &mut app,
        epoch,
        generation,
        limit,
        vec![session(
            "sess_1",
            "in_progress",
            "running",
            Some("gpt-5"),
            100,
        )],
    );

    let log = app.session_log();
    assert!(!log.loading());
    assert!(log.ever_loaded());
    assert_eq!(log.loaded(), 1);
    assert_eq!(log.counts().all, 1);
    assert_eq!(log.counts().in_progress, 1);
    assert_eq!(log.limit(), FETCH_STEP);
}

#[test]
fn first_page_failure_keeps_the_page_empty_and_reports_the_error() {
    let (mut app, _dir) = connected_app();
    crate::test_support::fail_first_page(
        &mut app,
        crate::api::ApiError::from_status(
            500,
            br#"{"error":{"code":"internal","message":"boom","details":null}}"#,
        ),
    );
    assert!(!app.session_log().loading());
    assert!(!app.session_log().ever_loaded());
    assert_eq!(app.session_log().loaded(), 0);
    assert!(!app.session_log().stale());
    assert!(app.session_log().error().is_some());
}

#[test]
fn chips_filter_and_case_insensitive_search_limit_rows() {
    let (mut app, _dir) = connected_app();
    feed_page(
        &mut app,
        vec![
            with_approvals(session("sess_1", "requires_action", "waiting", None, 10), 1),
            with_metadata(
                session("sess_2", "in_progress", "running", Some("GPT-5"), 20),
                &[("team", "Core")],
            ),
            session("sess_3", "idle", "idle", None, 30),
        ],
    );

    app.apply(Intent::SetFilter(StatusBucket::RequiresAction));
    assert_eq!(app.session_log().visible_len(), 1);
    assert_eq!(app.session_log().counts().requires_action, 1);

    app.apply(Intent::SetFilter(StatusBucket::All));
    app.set_field_value(crate::app::FieldId::SessionSearch, "CORE");
    assert_eq!(app.session_log().visible_len(), 1);
    assert_eq!(
        app.session_log()
            .visible_indices()
            .first()
            .map(|index| index.to_owned()),
        Some(1)
    );

    app.set_field_value(crate::app::FieldId::SessionSearch, "sess_3");
    assert_eq!(app.session_log().visible_len(), 1);
    app.set_field_value(crate::app::FieldId::SessionSearch, "no-such-row");
    assert_eq!(app.session_log().visible_len(), 0);
}

#[test]
fn display_pages_slice_the_filtered_rows() {
    let (mut app, _dir) = connected_app();
    feed_page(&mut app, sessions(25));
    let log = app.session_log();
    assert_eq!(log.page(), 0);
    assert_eq!(log.page_count(), 3);
    assert_eq!(log.page_len(), PAGE_SIZE);
    assert_eq!(log.page_indices(), (0..PAGE_SIZE).collect::<Vec<_>>());

    app.apply(Intent::GoToSessionPage(2));
    let log = app.session_log();
    assert_eq!(log.page(), 2);
    assert_eq!(log.page_count(), 3);
    assert_eq!(log.page_len(), 5);
    assert_eq!(log.page_indices(), (20..25).collect::<Vec<_>>());
    assert_eq!(log.row_cursor(), 0);
    assert_eq!(log.cursor_session_id(), Some("sess_020".to_owned()));
}

#[test]
fn sessions_loaded_old_to_new_are_shown_newest_first() {
    let (mut app, _dir) = connected_app();
    feed_page(
        &mut app,
        vec![
            session("old", "idle", "idle", None, 10),
            session("middle", "idle", "idle", None, 20),
            session("new", "idle", "idle", None, 30),
        ],
    );
    let ids: Vec<&str> = app
        .session_log()
        .sessions()
        .iter()
        .map(|row| row.id.as_str())
        .collect();
    assert_eq!(ids, vec!["new", "middle", "old"]);
    assert_eq!(app.session_log().sessions()[0].id, "new");
    assert_eq!(
        app.session_log().cursor_session_id().as_deref(),
        Some("new")
    );
}

#[test]
fn display_pages_hold_the_most_recent_sessions_first() {
    let (mut app, _dir) = connected_app();
    let rows: Vec<_> = (0..25i64)
        .map(|index| {
            // A permutation of 0..25, so page boundaries are unambiguous.
            let stamp = (index * 7 + 3) % 25;
            session(&format!("sess_{index:02}"), "idle", "idle", None, stamp)
        })
        .collect();
    let mut expected: Vec<(i64, String)> = rows
        .iter()
        .map(|row| (row.last_active_at, row.id.clone()))
        .collect();
    expected.sort_by_key(|row| std::cmp::Reverse(row.0));
    let expected: Vec<String> = expected.into_iter().map(|(_, id)| id).collect();

    feed_page(&mut app, rows);
    assert_eq!(app.session_log().page_count(), 3);
    assert_eq!(app.session_log().sessions()[0].last_active_at, 24);
    let first_stamps: Vec<i64> = app.session_log().sessions()[..PAGE_SIZE]
        .iter()
        .map(|row| row.last_active_at)
        .collect();
    assert_eq!(
        first_stamps,
        (15..=24).rev().collect::<Vec<_>>(),
        "page 1 holds the newest ten activity times"
    );
    let first_page: Vec<&str> = app.session_log().sessions()[..PAGE_SIZE]
        .iter()
        .map(|row| row.id.as_str())
        .collect();
    let expected_first: Vec<&str> = expected[..PAGE_SIZE].iter().map(String::as_str).collect();
    assert_eq!(first_page, expected_first, "page 1 holds the newest 10");

    app.apply(Intent::GoToSessionPage(2));
    let last_stamps: Vec<i64> = app.session_log().sessions()[PAGE_SIZE * 2..]
        .iter()
        .map(|row| row.last_active_at)
        .collect();
    assert_eq!(
        last_stamps,
        (0..=4).rev().collect::<Vec<_>>(),
        "the last page holds the oldest five activity times"
    );
    let last_page: Vec<&str> = app.session_log().sessions()[PAGE_SIZE * 2..]
        .iter()
        .map(|row| row.id.as_str())
        .collect();
    let expected_last: Vec<&str> = expected[PAGE_SIZE * 2..]
        .iter()
        .map(String::as_str)
        .collect();
    assert_eq!(last_page, expected_last, "the last page holds the oldest 5");
    assert_eq!(
        app.session_log().sessions().last().unwrap().id,
        expected[24]
    );
}

#[test]
fn equal_activity_times_keep_the_gateway_order() {
    let (mut app, _dir) = connected_app();
    feed_page(
        &mut app,
        vec![
            session("first", "idle", "idle", None, 42),
            session("second", "idle", "idle", None, 42),
            session("third", "idle", "idle", None, 42),
        ],
    );
    let ids: Vec<&str> = app
        .session_log()
        .sessions()
        .iter()
        .map(|row| row.id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["first", "second", "third"],
        "the sort is stable on a full timestamp tie"
    );
}

#[test]
fn equal_activity_breaks_ties_by_creation_time() {
    let (mut app, _dir) = connected_app();
    let mut older = session("created_first", "idle", "idle", None, 42);
    older.created_at = 40;
    let mut newer = session("created_second", "idle", "idle", None, 42);
    newer.created_at = 41;
    feed_page(&mut app, vec![older, newer]);
    let ids: Vec<&str> = app
        .session_log()
        .sessions()
        .iter()
        .map(|row| row.id.as_str())
        .collect();
    assert_eq!(ids, vec!["created_second", "created_first"]);
}

#[test]
fn an_empty_set_is_one_empty_page() {
    let (mut app, _dir) = connected_app();
    feed_page(&mut app, Vec::new());
    let log = app.session_log();
    assert_eq!(log.page(), 0);
    assert_eq!(log.page_count(), 1);
    assert_eq!(log.page_len(), 0);
    assert!(log.page_indices().is_empty());
    assert_eq!(log.cursor_session_id(), None);
}

#[test]
fn filter_and_search_return_to_the_first_page() {
    let (mut app, _dir) = connected_app();
    feed_page(&mut app, sessions(25));
    app.apply(Intent::GoToSessionPage(2));
    assert_eq!(app.session_log().page(), 2);

    app.apply(Intent::SetFilter(StatusBucket::Idle));
    assert_eq!(app.session_log().page(), 0);
    assert_eq!(app.session_log().pending_page(), None);

    app.apply(Intent::GoToSessionPage(2));
    assert_eq!(app.session_log().page(), 2);
    app.apply(Intent::FocusField {
        field: FieldId::SessionSearch,
        click: None,
    });
    app.handle_key(key(KeyCode::Char('s')));
    assert_eq!(app.session_log().search().value(), "s");
    assert_eq!(app.session_log().page(), 0);
}

#[test]
fn page_navigation_inside_the_loaded_set_needs_no_request() {
    let (mut app, _dir) = connected_app();
    feed_page(&mut app, sessions(25));

    assert!(app.apply(Intent::SessionPageNext).is_empty());
    assert_eq!(app.session_log().page(), 1);
    assert_eq!(app.focus(), Focus::Content);

    assert!(app.apply(Intent::SessionPagePrev).is_empty());
    assert_eq!(app.session_log().page(), 0);
    assert!(app.apply(Intent::SessionPagePrev).is_empty());
    assert_eq!(app.session_log().page(), 0);

    assert!(app.apply(Intent::GoToSessionPage(2)).is_empty());
    assert_eq!(app.session_log().page(), 2);
    assert!(app.apply(Intent::GoToSessionPage(3)).is_empty());
    assert_eq!(app.session_log().page(), 2);
}

#[test]
fn next_on_the_last_page_raises_the_limit_and_adopts_the_new_page() {
    let (mut app, _dir) = connected_app();
    feed_page_more(&mut app, sessions(PAGE_SIZE), true);
    assert_eq!(app.session_log().page_count(), 1);
    assert!(app.session_log().can_fetch_more());

    let effects = app.apply(Intent::SessionPageNext);
    let (epoch, generation, limit) = list_request(&effects);
    assert_eq!(limit, FETCH_STEP + FETCH_STEP);
    assert_eq!(app.session_log().pending_page(), Some(1));
    assert!(app.session_log().loading());

    finish_page_more(&mut app, epoch, generation, limit, sessions(20), false);
    let log = app.session_log();
    assert_eq!(log.page(), 1, "the pending page is adopted once it exists");
    assert_eq!(log.pending_page(), None);
    assert_eq!(log.page_len(), PAGE_SIZE);
    assert!(!log.can_fetch_more());
    assert!(app.apply(Intent::SessionPageNext).is_empty());
    assert_eq!(app.session_log().page(), 1);
}

#[test]
fn next_does_nothing_when_the_set_is_complete() {
    let (mut app, _dir) = connected_app();
    feed_page_more(&mut app, sessions(PAGE_SIZE), false);
    assert!(!app.session_log().can_fetch_more());
    assert!(app.apply(Intent::SessionPageNext).is_empty());
    assert_eq!(app.session_log().limit(), FETCH_STEP);
    assert_eq!(app.session_log().pending_page(), None);
}

#[test]
fn next_raises_the_limit_until_the_cap() {
    let (mut app, _dir) = connected_app();
    let mut rows = PAGE_SIZE;
    feed_page_more(&mut app, sessions(rows), true);

    let mut rounds = 0;
    while app.session_log().can_fetch_more() {
        let page = app.session_log().page();
        let effects = app.apply(Intent::SessionPageNext);
        let (epoch, generation, limit) = list_request(&effects);
        assert!(limit <= MAX_SESSIONS);
        assert_eq!(app.session_log().pending_page(), Some(page + 1));

        rows += PAGE_SIZE;
        finish_page_more(&mut app, epoch, generation, limit, sessions(rows), true);
        assert_eq!(app.session_log().page(), page + 1);
        rounds += 1;
        assert!(rounds <= 10, "the cap must bound the number of fetches");
    }
    assert_eq!(app.session_log().limit(), MAX_SESSIONS);
    assert_eq!(app.session_log().page(), 9);
    assert!(app.apply(Intent::SessionPageNext).is_empty());
    assert_eq!(app.session_log().page(), 9);
}

#[test]
fn a_pending_page_survives_an_older_response() {
    let (mut app, _dir) = connected_app();
    feed_page_more(&mut app, sessions(PAGE_SIZE), true);

    // A periodic fetch is already on the wire when the pager asks for page 1.
    let effects = due(&mut app, Instant::now() + Duration::from_secs(5));
    let (epoch, generation, limit) = list_request(&effects);
    assert_eq!(limit, FETCH_STEP);

    let effects = app.apply(Intent::SessionPageNext);
    assert!(effects.is_empty(), "the in-flight request keeps its slot");
    assert_eq!(app.session_log().pending_page(), Some(1));
    assert_eq!(app.session_log().limit(), FETCH_STEP + FETCH_STEP);

    // The older response was requested with the old limit, so it is discarded
    // and the raised fetch is queued instead.
    let effects = app.handle_worker(WorkerEvent::SessionsLoaded {
        epoch,
        generation,
        limit,
        result: Ok(page_with(sessions(PAGE_SIZE), true)),
    });
    let (epoch, generation, limit) = list_request(&effects);
    assert_eq!(limit, FETCH_STEP + FETCH_STEP);
    assert_eq!(app.session_log().pending_page(), Some(1));
    assert_eq!(app.session_log().page(), 0);

    finish_page_more(&mut app, epoch, generation, limit, sessions(20), false);
    assert_eq!(app.session_log().page(), 1);
    assert_eq!(app.session_log().pending_page(), None);
}

#[test]
fn a_pending_page_waits_for_data_that_contains_it() {
    let (mut app, _dir) = connected_app();
    feed_page_more(&mut app, sessions(PAGE_SIZE), true);
    let effects = app.apply(Intent::SessionPageNext);
    let (epoch, generation, limit) = list_request(&effects);

    // The larger fetch still returns one page, so the wait continues.
    finish_page_more(
        &mut app,
        epoch,
        generation,
        limit,
        sessions(PAGE_SIZE),
        true,
    );
    assert_eq!(app.session_log().page(), 0);
    assert_eq!(app.session_log().pending_page(), Some(1));

    let effects = app.apply(Intent::RefreshSessions);
    let (epoch, generation, limit) = list_request(&effects);
    finish_page_more(&mut app, epoch, generation, limit, sessions(20), false);
    assert_eq!(app.session_log().page(), 1);
    assert_eq!(app.session_log().pending_page(), None);
}

#[test]
fn a_pending_page_is_dropped_when_nothing_more_can_arrive() {
    let (mut app, _dir) = connected_app();
    feed_page_more(&mut app, sessions(PAGE_SIZE), true);
    let effects = app.apply(Intent::SessionPageNext);
    let (epoch, generation, limit) = list_request(&effects);

    finish_page_more(
        &mut app,
        epoch,
        generation,
        limit,
        sessions(PAGE_SIZE),
        false,
    );
    assert_eq!(app.session_log().page(), 0);
    assert_eq!(app.session_log().pending_page(), None);
    assert!(!app.session_log().has_more());
}

#[test]
fn a_failed_fetch_clears_the_pending_page() {
    let (mut app, _dir) = connected_app();
    feed_page_more(&mut app, sessions(PAGE_SIZE), true);
    let effects = app.apply(Intent::SessionPageNext);
    let (epoch, generation, limit) = list_request(&effects);

    app.handle_worker(WorkerEvent::SessionsLoaded {
        epoch,
        generation,
        limit,
        result: Err(crate::api::ApiError::from_status(
            500,
            br#"{"error":{"code":"internal","message":"boom","details":null}}"#,
        )),
    });
    assert_eq!(app.session_log().pending_page(), None);
    assert!(app.session_log().error().is_some());
    assert_eq!(app.session_log().page(), 0);
}

#[test]
fn shrinking_the_set_clamps_the_page_and_cursor() {
    let (mut app, _dir) = connected_app();
    feed_page(&mut app, sessions(25));
    app.apply(Intent::GoToSessionPage(2));
    for _ in 0..4 {
        app.handle_key(key(KeyCode::Down));
    }
    assert_eq!(app.session_log().row_cursor(), 4);

    refresh_page(&mut app, sessions(12));
    let log = app.session_log();
    assert_eq!(log.page(), 1, "the page is clamped to the loaded pages");
    assert_eq!(log.page_count(), 2);
    assert_eq!(log.row_cursor(), 1, "the cursor stays inside the page");
    assert_eq!(log.page_len(), 2);
}

#[test]
fn page_up_and_down_change_pages_only_from_the_content() {
    let (mut app, _dir) = connected_app();
    feed_page(&mut app, sessions(25));

    app.handle_key(key(KeyCode::PageDown));
    assert_eq!(app.session_log().page(), 1);
    app.handle_key(key(KeyCode::PageDown));
    assert_eq!(app.session_log().page(), 2);
    app.handle_key(key(KeyCode::PageDown));
    assert_eq!(app.session_log().page(), 2);
    app.handle_key(key(KeyCode::PageUp));
    assert_eq!(app.session_log().page(), 1);

    app.apply(Intent::FocusField {
        field: FieldId::SessionSearch,
        click: None,
    });
    assert!(app.handle_key(key(KeyCode::PageUp)).is_empty());
    assert_eq!(app.session_log().page(), 1);
}

#[test]
fn the_row_cursor_is_page_local() {
    let (mut app, _dir) = connected_app();
    feed_page(&mut app, sessions(25));
    app.apply(Intent::GoToSessionPage(1));
    assert_eq!(app.session_log().row_cursor(), 0);
    assert_eq!(
        app.session_log().cursor_session_id(),
        Some("sess_010".to_owned())
    );

    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.session_log().row_cursor(), 1);
    assert_eq!(
        app.session_log().cursor_session_id(),
        Some("sess_011".to_owned())
    );

    for _ in 0..20 {
        app.handle_key(key(KeyCode::Down));
    }
    assert_eq!(
        app.session_log().row_cursor(),
        PAGE_SIZE - 1,
        "down stays inside the page"
    );

    app.apply(Intent::SessionPageNext);
    assert_eq!(app.session_log().row_cursor(), 0);
    assert_eq!(
        app.session_log().cursor_session_id(),
        Some("sess_020".to_owned())
    );
}

#[test]
fn polls_never_overlap_and_follow_the_loaded_size_interval() {
    let (mut app, _dir) = connected_app();
    let start = Instant::now();
    let effects = due(&mut app, start);
    let (epoch, generation, limit) = list_request(&effects);
    assert!(app
        .session_list_due(start + Duration::from_secs(1))
        .is_none());
    finish_page(&mut app, epoch, generation, limit, Vec::new());
    assert!(app
        .session_list_due(start + Duration::from_secs(2))
        .is_none());
    assert!(app
        .session_list_due(start + Duration::from_secs(3))
        .is_some());

    let (mut app, _dir) = connected_app();
    let rows = (0..101)
        .map(|index| session(&format!("sess_{index}"), "idle", "idle", None, 1))
        .collect();
    feed_page(&mut app, rows);
    let after = Instant::now();
    assert!(app
        .session_list_due(after + Duration::from_secs(3))
        .is_none());
    assert!(app
        .session_list_due(after + Duration::from_secs(6))
        .is_some());

    let (mut app, _dir) = connected_app();
    let rows = (0..301)
        .map(|index| session(&format!("sess_{index}"), "idle", "idle", None, 1))
        .collect();
    feed_page(&mut app, rows);
    let after = Instant::now();
    assert!(app
        .session_list_due(after + Duration::from_secs(6))
        .is_none());
    assert!(app
        .session_list_due(after + Duration::from_secs(11))
        .is_some());
}

#[test]
fn stale_generation_is_discarded_and_a_reload_is_queued() {
    let (mut app, _dir) = connected_app();
    let effects = due(&mut app, Instant::now());
    let (epoch, generation, limit) = list_request(&effects);
    assert!(app.apply(Intent::RefreshSessions).is_empty());

    let effects = app.handle_worker(crate::worker::WorkerEvent::SessionsLoaded {
        epoch,
        generation,
        limit,
        result: Ok(crate::test_support::page(vec![session(
            "stale", "idle", "idle", None, 1,
        )])),
    });
    assert!(app.session_log().sessions().is_empty());
    let (_, _, reload_limit) = list_request(&effects);
    assert_eq!(reload_limit, limit);
}

#[test]
fn unauthorized_pauses_polling_and_keeps_the_last_good_page() {
    let (mut app, _dir) = connected_app();
    feed_page(&mut app, vec![session("sess_1", "idle", "idle", None, 1)]);
    let effects = app.apply(Intent::RefreshSessions);
    let (epoch, generation, limit) = list_request(&effects);
    app.handle_worker(crate::worker::WorkerEvent::SessionsLoaded {
        epoch,
        generation,
        limit,
        result: Err(crate::api::ApiError::from_status(
            401,
            br#"{"error":{"code":"unauthorized","message":"bad key","details":null}}"#,
        )),
    });

    assert!(app.is_paused());
    assert_eq!(app.session_log().loaded(), 1);
    assert!(app.session_log().stale());
    assert!(app.session_log().error().is_some());
    assert!(app
        .active_banner()
        .is_some_and(|banner| banner.unauthorized));
    assert!(app
        .session_list_due(Instant::now() + Duration::from_secs(60))
        .is_none());
}

#[test]
fn selection_survives_refresh_until_the_session_disappears() {
    let (mut app, _dir) = connected_app();
    feed_page(
        &mut app,
        vec![
            session("sess_1", "idle", "idle", None, 1),
            session("sess_2", "idle", "idle", None, 2),
        ],
    );
    app.apply(Intent::OpenSession("sess_2".to_owned()));
    assert_eq!(app.screen(), Screen::SessionDetail);
    assert_eq!(app.session_log().selected(), Some("sess_2"));
    app.apply(Intent::Back);
    assert_eq!(app.page(), Page::SessionLog);

    refresh_page(
        &mut app,
        vec![
            session("sess_1", "idle", "idle", None, 1),
            session("sess_2", "idle", "idle", None, 2),
        ],
    );
    assert_eq!(app.session_log().selected(), Some("sess_2"));

    refresh_page(&mut app, vec![session("sess_1", "idle", "idle", None, 1)]);
    assert_eq!(app.session_log().selected(), None);
}

#[test]
fn keyboard_cursor_opens_the_row_under_it() {
    let (mut app, _dir) = connected_app();
    feed_page(
        &mut app,
        vec![
            session("sess_1", "idle", "idle", None, 1),
            session("sess_2", "idle", "idle", None, 2),
        ],
    );
    // Newest activity first puts sess_2 on top.
    assert_eq!(
        app.session_log().cursor_session_id().as_deref(),
        Some("sess_2")
    );
    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.session_log().row_cursor(), 1);
    let effects = app.handle_key(key(KeyCode::Enter));
    assert!(effects.is_empty());
    assert_eq!(app.detail_id(), Some("sess_1"));
    assert_eq!(app.screen(), Screen::SessionDetail);
}

#[test]
fn action_buttons_route_to_the_same_flows() {
    let (mut app, _dir) = connected_app();
    assert!(app.apply(Intent::Activate(ActionId::NewSession)).is_empty());
    assert_eq!(app.screen(), Screen::SessionCreate);
    app.apply(Intent::Back);
    assert_eq!(app.screen(), Screen::Main);

    feed_page(&mut app, sessions(PAGE_SIZE));
    let effects = app.apply(Intent::Activate(ActionId::RefreshSessions));
    let (_, _, limit) = list_request(&effects);
    assert_eq!(limit, FETCH_STEP);
}
