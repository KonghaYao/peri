use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::*;
use crate::app::{ActionId, App, Effect, Intent, Page, Screen};
use crate::test_support::{
    connected_app, feed_page, finish_page, list_request, refresh_page, session, with_approvals,
    with_metadata,
};
use crate::view_model::StatusBucket;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn due(app: &mut App, now: Instant) -> Vec<Effect> {
    app.session_list_due(now).into_iter().collect()
}

#[test]
fn first_page_loads_and_updates_state() {
    let (mut app, _dir) = connected_app();
    let effects = due(&mut app, Instant::now());
    assert_eq!(effects.len(), 1);
    let (epoch, generation, limit) = list_request(&effects);
    assert_eq!(limit, LIST_PAGE_SIZE);
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
    assert_eq!(log.limit(), LIST_PAGE_SIZE);
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
fn load_more_raises_the_limit_by_fifty_until_the_cap() {
    let (mut app, _dir) = connected_app();
    feed_page(&mut app, Vec::new());
    assert_eq!(app.session_log().limit(), LIST_PAGE_SIZE);

    let effects = app.apply(Intent::LoadMoreSessions);
    let (epoch, generation, limit) = list_request(&effects);
    assert_eq!(limit, LIST_PAGE_SIZE + LIST_PAGE_SIZE);
    finish_page(&mut app, epoch, generation, limit, Vec::new());

    while app.session_log().can_load_more() {
        let effects = app.apply(Intent::LoadMoreSessions);
        let (epoch, generation, limit) = list_request(&effects);
        assert!(limit <= LIST_MAX);
        finish_page(&mut app, epoch, generation, limit, Vec::new());
    }
    assert_eq!(app.session_log().limit(), LIST_MAX);
    assert!(app.apply(Intent::LoadMoreSessions).is_empty());
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
    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.session_log().row_cursor(), 1);
    let effects = app.handle_key(key(KeyCode::Enter));
    assert!(effects.is_empty());
    assert_eq!(app.detail_id(), Some("sess_2"));
    assert_eq!(app.screen(), Screen::SessionDetail);
}

#[test]
fn action_buttons_route_to_the_same_flows() {
    let (mut app, _dir) = connected_app();
    assert!(app.apply(Intent::Activate(ActionId::NewSession)).is_empty());
    assert_eq!(app.screen(), Screen::SessionCreate);
    app.apply(Intent::Back);
    assert_eq!(app.screen(), Screen::Main);

    feed_page(&mut app, Vec::new());
    let effects = app.apply(Intent::Activate(ActionId::LoadMoreSessions));
    let (_, _, limit) = list_request(&effects);
    assert_eq!(limit, LIST_PAGE_SIZE + LIST_PAGE_SIZE);
}
