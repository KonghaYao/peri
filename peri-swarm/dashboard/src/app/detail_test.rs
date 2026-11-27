//! Detail scheduling regressions: follow layout, pagination overlap and the
//! refresh invalidation rules.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::api::{ItemsPage, Session, Turn, Turns};
use crate::app::{App, Effect, Intent};
use crate::test_support::{connected_app, session};
use crate::worker::WorkerEvent;

use super::SessionDetailState;
use crate::app::session_detail::{TIMELINE_MAX, TIMELINE_PAGE_SIZE, TURNS_PAGE_SIZE};

const SESSION: &str = "sess_1";

fn detail_session(status: &str) -> Session {
    session(SESSION, status, "active", Some("model-x"), 100)
}

fn items_page(data: Vec<Value>, cursor: Option<&str>, has_more: bool) -> ItemsPage {
    ItemsPage {
        object: Some("list".to_owned()),
        items: Vec::new(),
        data,
        next_cursor: cursor.map(|value| Value::String(value.to_owned())),
        has_more,
        order: Some("desc".to_owned()),
        last_item_seq: None,
    }
}

fn empty_items() -> ItemsPage {
    items_page(Vec::new(), None, false)
}

fn turns_page(data: Vec<Turn>, cursor: Option<&str>, has_more: bool) -> Turns {
    Turns {
        object: Some("list".to_owned()),
        data,
        has_more,
        next_cursor: cursor.map(str::to_owned),
    }
}

fn empty_turns() -> Turns {
    turns_page(Vec::new(), None, false)
}

fn message_item(id: &str, turn_id: &str, text: &str) -> Value {
    json!({
        "id": id,
        "type": "message",
        "turn_id": turn_id,
        "role": "assistant",
        "content": [{ "type": "output_text", "text": text }]
    })
}

fn turn(id: &str) -> Turn {
    Turn {
        id: id.to_owned(),
        status: "completed".to_owned(),
        created_at: 1,
        started_at: None,
        completed_at: None,
        error: None,
        usage: None,
    }
}

fn open_group(app: &mut App) -> u64 {
    app.apply(Intent::OpenSession(SESSION.to_owned()));
    let effects = app.detail_due(Instant::now());
    assert_eq!(effects.len(), 3, "one refresh group");
    match effects.first() {
        Some(Effect::GetSession { generation, .. }) => *generation,
        _ => panic!("unexpected effect"),
    }
}

fn finish_group(app: &mut App, generation: u64, items: ItemsPage, turns: Turns) {
    let epoch = app.epoch();
    app.handle_worker(WorkerEvent::SessionLoaded {
        epoch,
        generation,
        session_id: SESSION.to_owned(),
        result: Ok(Box::new(detail_session("idle"))),
    });
    app.handle_worker(WorkerEvent::ItemsLoaded {
        epoch,
        generation,
        session_id: SESSION.to_owned(),
        limit: TIMELINE_PAGE_SIZE,
        after: None,
        result: Ok(Box::new(items)),
    });
    app.handle_worker(WorkerEvent::TurnsLoaded {
        epoch,
        generation,
        session_id: SESSION.to_owned(),
        limit: TURNS_PAGE_SIZE,
        after: None,
        result: Ok(Box::new(turns)),
    });
}

fn ready_app(items: ItemsPage, turns: Turns) -> App {
    let (mut app, _dir) = connected_app();
    let generation = open_group(&mut app);
    finish_group(&mut app, generation, items, turns);
    app
}

#[test]
fn load_earlier_waits_for_the_refresh_group() {
    let (mut app, _dir) = connected_app();
    app.apply(Intent::OpenSession(SESSION.to_owned()));
    let effects = app.detail_due(Instant::now());
    assert_eq!(effects.len(), 3);

    assert!(
        app.apply(Intent::LoadEarlierItems).is_empty(),
        "a page request must not overlap the in-flight group"
    );
    assert!(!app.detail().timeline().loading_more());

    // Finish the group with a cursor; the page request is allowed again.
    let generation = match effects.first() {
        Some(Effect::GetSession { generation, .. }) => *generation,
        _ => panic!("unexpected effect"),
    };
    finish_group(
        &mut app,
        generation,
        items_page(
            vec![message_item("msg_2", "turn_1", "two")],
            Some("2"),
            true,
        ),
        turns_page(vec![turn("turn_1")], Some("turn_1"), true),
    );

    let effects = app.apply(Intent::LoadEarlierItems);
    assert_eq!(effects.len(), 1, "pagination starts after the group");
    assert!(app.detail().timeline().loading_more());
    assert!(app
        .detail_due(Instant::now() + Duration::from_secs(60))
        .is_empty());
}

#[test]
fn load_more_turns_waits_for_the_refresh_group() {
    let (mut app, _dir) = connected_app();
    app.apply(Intent::OpenSession(SESSION.to_owned()));
    let effects = app.detail_due(Instant::now());
    let generation = match effects.first() {
        Some(Effect::GetSession { generation, .. }) => *generation,
        _ => panic!("unexpected effect"),
    };
    assert!(app.apply(Intent::LoadMoreTurns).is_empty());
    assert!(!app.detail().turns().loading_more());

    finish_group(
        &mut app,
        generation,
        empty_items(),
        turns_page(vec![turn("turn_2")], Some("turn_2"), true),
    );
    let effects = app.apply(Intent::LoadMoreTurns);
    assert_eq!(effects.len(), 1);
    assert!(app.detail().turns().loading_more());
}

#[test]
fn manual_refresh_releases_a_stale_load_earlier_marker() {
    let mut app = ready_app(
        items_page(
            vec![message_item("msg_2", "turn_1", "two")],
            Some("2"),
            true,
        ),
        empty_turns(),
    );
    let effects = app.apply(Intent::LoadEarlierItems);
    assert_eq!(effects.len(), 1);
    assert!(app.detail().timeline().loading_more());
    let page_generation = app.detail().poll.generation();
    assert_eq!(
        app.detail_due(Instant::now() + Duration::from_secs(60))
            .len(),
        0,
        "a page in flight pauses the group refresh"
    );

    let loaded = app.detail().timeline().loaded();
    let refresh = app.apply(Intent::RefreshDetail);
    assert_eq!(refresh.len(), 3, "refresh starts a new group immediately");
    assert!(
        !app.detail().timeline().loading_more(),
        "the invalidated page marker must be released"
    );

    // The old page response arrives late; it is discarded and cannot leave a
    // marker behind.
    let epoch = app.epoch();
    app.handle_worker(WorkerEvent::ItemsLoaded {
        epoch,
        generation: page_generation,
        session_id: SESSION.to_owned(),
        limit: TIMELINE_PAGE_SIZE,
        after: Some("2".to_owned()),
        result: Ok(Box::new(items_page(
            vec![message_item("msg_1", "turn_1", "one")],
            None,
            false,
        ))),
    });
    assert_eq!(app.detail().timeline().loaded(), loaded);
    assert!(!app.detail().timeline().loading_more());

    // Finish the refresh group and prove polling can start again.
    let generation = match refresh.first() {
        Some(Effect::GetSession { generation, .. }) => *generation,
        _ => panic!("unexpected effect"),
    };
    finish_group(&mut app, generation, empty_items(), empty_turns());
    let next = app.detail_due(Instant::now() + Duration::from_secs(60));
    assert_eq!(next.len(), 3, "detail polling must not be stranded");
}

#[test]
fn manual_refresh_releases_a_stale_load_more_turns_marker() {
    let mut app = ready_app(
        empty_items(),
        turns_page(vec![turn("turn_2")], Some("turn_2"), true),
    );
    let effects = app.apply(Intent::LoadMoreTurns);
    assert_eq!(effects.len(), 1);
    assert!(app.detail().turns().loading_more());
    let page_generation = app.detail().poll.generation();

    let refresh = app.apply(Intent::RefreshDetail);
    assert_eq!(refresh.len(), 3);
    assert!(!app.detail().turns().loading_more());

    let epoch = app.epoch();
    app.handle_worker(WorkerEvent::TurnsLoaded {
        epoch,
        generation: page_generation,
        session_id: SESSION.to_owned(),
        limit: TURNS_PAGE_SIZE,
        after: Some("turn_2".to_owned()),
        result: Ok(Box::new(turns_page(vec![turn("turn_1")], None, false))),
    });
    assert_eq!(app.detail().turns().rows().len(), 1);
    assert!(!app.detail().turns().loading_more());
}

#[test]
fn polling_merge_keeps_at_most_the_newest_five_hundred_items() {
    let mut detail = SessionDetailState::for_session(SESSION);
    let newest_first: Vec<Value> = (0..TIMELINE_MAX)
        .rev()
        .map(|index| message_item(&format!("item_{index:03}"), "turn_1", "old"))
        .collect();
    detail
        .timeline
        .apply_first_page(&items_page(newest_first, None, false));
    assert_eq!(detail.timeline().loaded(), TIMELINE_MAX);

    detail.timeline.apply_first_page(&items_page(
        vec![
            message_item("item_501", "turn_1", "newest"),
            message_item("item_500", "turn_1", "new"),
            message_item("item_499", "turn_1", "old"),
            message_item("item_498", "turn_1", "old"),
        ],
        None,
        false,
    ));
    let entries = detail.timeline().entries();
    assert_eq!(
        entries.len(),
        TIMELINE_MAX,
        "merge must not grow past the cap"
    );
    assert_eq!(
        entries.last().map(|entry| entry.id.as_str()),
        Some("item_501"),
        "the newest entry is retained"
    );
    assert_eq!(
        entries.first().map(|entry| entry.id.as_str()),
        Some("item_002"),
        "the two oldest entries are dropped"
    );
}

#[test]
fn layout_following_and_scrolled_up_are_distinguished() {
    let (mut app, _dir) = connected_app();
    app.apply(Intent::OpenSession(SESSION.to_owned()));
    app.set_content_layout(20, 100, 80);
    assert!(app.detail_timeline_following());
    assert_eq!(app.content_max(), 80);

    app.set_content_layout(20, 100, 10);
    assert!(!app.detail_timeline_following());
    assert!(!app.timeline_unseen());

    // A view that was at the bottom is re-anchored after content grows.
    app.set_content_layout(20, 100, 80);
    assert!(app.detail_timeline_following());
}
