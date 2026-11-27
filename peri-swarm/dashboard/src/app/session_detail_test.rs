//! Detail data flow: polling, timeline, approvals, composer and mutations.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::api::{
    update_session_body, ApiError, ApprovalDecision, ApprovalReceipt, InputReceipt, ItemsPage,
    Session, Turn, Turns,
};
use crate::app::{
    ActionId, App, DetailTab, Effect, Intent, Overlay, Screen, TimelineEntry, TimelineKind,
};
use crate::test_support::{connected_app, session, with_approvals};
use crate::worker::{MutationData, MutationKind, WorkerEvent};

use super::{
    parse_required_actions, RequiredAction, TimelineState, TIMELINE_MAX, TIMELINE_PAGE_SIZE,
    TURNS_PAGE_SIZE,
};

fn session_id() -> &'static str {
    "sess_1"
}

fn detail_session(status: &str) -> Session {
    session("sess_1", status, "active", Some("model-x"), 100)
}

fn empty_items() -> ItemsPage {
    items_page(Vec::new(), None, false)
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

fn empty_turns() -> Turns {
    turns_page(Vec::new(), None, false)
}

fn turns_page(data: Vec<Turn>, cursor: Option<&str>, has_more: bool) -> Turns {
    Turns {
        object: Some("list".to_owned()),
        data,
        has_more,
        next_cursor: cursor.map(str::to_owned),
    }
}

fn turn(id: &str, status: &str) -> Turn {
    Turn {
        id: id.to_owned(),
        status: status.to_owned(),
        created_at: 1,
        started_at: None,
        completed_at: None,
        error: None,
        usage: None,
    }
}

fn message_item(id: &str, turn_id: &str, role: &str, text: &str) -> Value {
    let block = if role == "user" {
        "input_text"
    } else {
        "output_text"
    };
    json!({
        "id": id,
        "type": "message",
        "turn_id": turn_id,
        "role": role,
        "content": [{ "type": block, "text": text }]
    })
}

fn call_item(id: &str, turn_id: &str, name: &str) -> Value {
    json!({
        "id": id,
        "type": "function_call",
        "turn_id": turn_id,
        "name": name,
        "arguments": { "query": "rust" }
    })
}

fn open_detail(app: &mut App) -> u64 {
    app.apply(Intent::OpenSession(session_id().to_owned()));
    let effects = app.detail_due(Instant::now());
    assert_eq!(effects.len(), 3, "one refresh group");
    generation_of(&effects)
}

fn generation_of(effects: &[Effect]) -> u64 {
    match effects.first() {
        Some(Effect::GetSession { generation, .. }) => *generation,
        _ => panic!("unexpected effect"),
    }
}

fn finish_group(app: &mut App, generation: u64, snapshot: Session, items: ItemsPage, turns: Turns) {
    let epoch = app.epoch();
    let id = snapshot.id.clone();
    app.handle_worker(WorkerEvent::SessionLoaded {
        epoch,
        generation,
        session_id: id.clone(),
        result: Ok(Box::new(snapshot)),
    });
    app.handle_worker(WorkerEvent::ItemsLoaded {
        epoch,
        generation,
        session_id: id.clone(),
        limit: TIMELINE_PAGE_SIZE,
        after: None,
        result: Ok(Box::new(items)),
    });
    app.handle_worker(WorkerEvent::TurnsLoaded {
        epoch,
        generation,
        session_id: id,
        limit: TURNS_PAGE_SIZE,
        after: None,
        result: Ok(Box::new(turns)),
    });
}

#[test]
fn first_group_requests_the_snapshot_and_both_newest_pages() {
    let (mut app, _dir) = connected_app();
    app.apply(Intent::OpenSession(session_id().to_owned()));
    let effects = app.detail_due(Instant::now());
    assert_eq!(effects.len(), 3);
    let generation = generation_of(&effects);
    assert!(matches!(
        &effects[0],
        Effect::GetSession { session_id: id, .. } if id == session_id()
    ));
    assert!(matches!(
        &effects[1],
        Effect::ListItems {
            limit: TIMELINE_PAGE_SIZE,
            after: None,
            ..
        }
    ));
    assert!(matches!(
        &effects[2],
        Effect::ListTurns {
            limit: TURNS_PAGE_SIZE,
            after: None,
            ..
        }
    ));
    assert!(matches!(
        &effects[1],
        Effect::ListItems { generation: g, .. } if *g == generation
    ));
    // One group is on the wire at a time.
    assert!(app.detail_due(Instant::now()).is_empty());
}

#[test]
fn poll_interval_depends_on_the_session_status() {
    let (mut app, _dir) = connected_app();
    let generation = open_detail(&mut app);
    finish_group(
        &mut app,
        generation,
        detail_session("idle"),
        empty_items(),
        empty_turns(),
    );

    let last = app.detail.last_request.expect("first group timestamp");
    assert!(app.detail_due(last + Duration::from_secs(4)).is_empty());
    let effects = app.detail_due(last + Duration::from_secs(5));
    assert_eq!(effects.len(), 3, "idle refreshes after 5s");
    let generation = generation_of(&effects);
    finish_group(
        &mut app,
        generation,
        detail_session("in_progress"),
        empty_items(),
        empty_turns(),
    );

    let active = app.detail.last_request.expect("second group timestamp");
    assert!(app.detail_due(active + Duration::from_secs(1)).is_empty());
    assert_eq!(
        app.detail_due(active + Duration::from_secs(2)).len(),
        3,
        "running sessions refresh after 2s"
    );
}

#[test]
fn refresh_with_an_old_generation_is_discarded() {
    let (mut app, _dir) = connected_app();
    let generation = open_detail(&mut app);
    finish_group(
        &mut app,
        generation,
        detail_session("idle"),
        empty_items(),
        empty_turns(),
    );
    let effects = app.apply(Intent::RefreshDetail);
    let current = generation_of(&effects);
    assert_ne!(current, generation);

    let epoch = app.epoch();
    app.handle_worker(WorkerEvent::SessionLoaded {
        epoch,
        generation,
        session_id: session_id().to_owned(),
        result: Ok(Box::new(detail_session("failed"))),
    });
    assert_eq!(
        app.detail()
            .session()
            .map(|snapshot| snapshot.status.as_str()),
        Some("idle"),
        "stale generation must not replace the snapshot"
    );
}

#[test]
fn failed_refresh_keeps_the_last_good_snapshot_and_marks_it_stale() {
    let (mut app, _dir) = connected_app();
    let generation = open_detail(&mut app);
    finish_group(
        &mut app,
        generation,
        detail_session("idle"),
        items_page(
            vec![message_item("item_1", "turn_1", "user", "hi")],
            None,
            false,
        ),
        empty_turns(),
    );
    assert_eq!(app.detail().timeline().loaded(), 1);

    let effects = app.apply(Intent::RefreshDetail);
    let generation = generation_of(&effects);
    let epoch = app.epoch();
    let error = ApiError::from_status(
        500,
        b"{\"error\":{\"code\":\"internal\",\"message\":\"boom\"}}",
    );
    app.handle_worker(WorkerEvent::SessionLoaded {
        epoch,
        generation,
        session_id: session_id().to_owned(),
        result: Err(error),
    });
    let error = ApiError::from_status(500, b"{}");
    app.handle_worker(WorkerEvent::ItemsLoaded {
        epoch,
        generation,
        session_id: session_id().to_owned(),
        limit: TIMELINE_PAGE_SIZE,
        after: None,
        result: Err(error.clone()),
    });
    app.handle_worker(WorkerEvent::TurnsLoaded {
        epoch,
        generation,
        session_id: session_id().to_owned(),
        limit: TURNS_PAGE_SIZE,
        after: None,
        result: Err(error),
    });

    assert!(app.detail().session().is_some());
    assert!(app.detail().stale());
    assert!(app.detail().error().is_some());
    assert_eq!(app.detail().timeline().loaded(), 1);
}

#[test]
fn unauthorized_refresh_pauses_polling() {
    let (mut app, _dir) = connected_app();
    let generation = open_detail(&mut app);
    finish_group(
        &mut app,
        generation,
        detail_session("idle"),
        empty_items(),
        empty_turns(),
    );
    let effects = app.apply(Intent::RefreshDetail);
    let generation = generation_of(&effects);
    let epoch = app.epoch();
    let unauthorized = ApiError::from_status(
        401,
        b"{\"error\":{\"code\":\"unauthorized\",\"message\":\"bad key\"}}",
    );
    app.handle_worker(WorkerEvent::SessionLoaded {
        epoch,
        generation,
        session_id: session_id().to_owned(),
        result: Err(unauthorized.clone()),
    });
    app.handle_worker(WorkerEvent::ItemsLoaded {
        epoch,
        generation,
        session_id: session_id().to_owned(),
        limit: TIMELINE_PAGE_SIZE,
        after: None,
        result: Err(unauthorized.clone()),
    });
    app.handle_worker(WorkerEvent::TurnsLoaded {
        epoch,
        generation,
        session_id: session_id().to_owned(),
        limit: TURNS_PAGE_SIZE,
        after: None,
        result: Err(unauthorized),
    });

    assert!(app.is_paused());
    assert!(app.detail.poll.is_paused());
    assert!(app
        .detail_due(Instant::now() + Duration::from_secs(60))
        .is_empty());
}

#[test]
fn required_actions_parse_mcp_approvals_and_explain_other_types() {
    let actions = vec![
        json!({
            "type": "mcp_approval_request",
            "request_id": "appr_7",
            "turn_id": "turn_9",
            "request": {
                "type": "mcp_tool_call",
                "server_label": "github",
                "name": "search_issues",
                "arguments": { "query": "bugs" },
                "message": "Allow the search?"
            }
        }),
        json!({ "type": "user_input_request" }),
        json!({}),
    ];
    let parsed = parse_required_actions(&actions);
    assert_eq!(parsed.len(), 3);
    match &parsed[0] {
        RequiredAction::Approval(entry) => {
            assert_eq!(entry.request_id, "appr_7");
            assert_eq!(entry.turn_id, "turn_9");
            assert_eq!(entry.server_label, "github");
            assert_eq!(entry.tool.as_deref(), Some("search_issues"));
            assert_eq!(entry.arguments, json!({ "query": "bugs" }));
            assert_eq!(entry.message.as_deref(), Some("Allow the search?"));
        }
        other => panic!("expected an approval, got {other:?}"),
    }
    assert_eq!(
        parsed[1],
        RequiredAction::Unsupported {
            kind: "user_input_request".to_owned()
        }
    );
    assert!(matches!(
        &parsed[2],
        RequiredAction::Unsupported { kind } if !kind.is_empty()
    ));
}

#[test]
fn approval_event_body_matches_the_official_projection() {
    let approve = crate::api::approval_event_body("appr_1", ApprovalDecision::Approve);
    assert_eq!(
        approve,
        json!({
            "events": [{
                "type": "agent.session.input.mcp_approval_request_result",
                "request_id": "appr_1",
                "response": { "type": "mcp_tool_call", "decision": "approve" }
            }]
        })
    );
    let deny = crate::api::approval_event_body("appr_1", ApprovalDecision::Deny);
    assert_eq!(
        deny["events"][0]["response"]["decision"],
        Value::String("deny".to_owned())
    );
}

#[test]
fn approve_and_deny_send_the_request_id_and_lock_while_in_flight() {
    for decision in [ApprovalDecision::Approve, ApprovalDecision::Deny] {
        let (mut app, _dir) = connected_app();
        let generation = open_detail(&mut app);
        let snapshot = with_approvals(detail_session("requires_action"), 1);
        finish_group(&mut app, generation, snapshot, empty_items(), empty_turns());
        assert!(app.detail().requires_action());
        assert_eq!(app.detail().approvals().len(), 1);

        let effects = app.apply(Intent::AnswerApproval {
            request_id: "appr_0".to_owned(),
            decision,
        });
        match effects.as_slice() {
            [Effect::AnswerApproval {
                request_id,
                decision: sent,
                ..
            }] => {
                assert_eq!(request_id, "appr_0");
                assert_eq!(*sent, decision);
            }
            _ => panic!("unexpected effects"),
        }
        assert!(app.detail().approval_in_flight());
        assert!(
            app.apply(Intent::AnswerApproval {
                request_id: "appr_0".to_owned(),
                decision,
            })
            .is_empty(),
            "a second answer is blocked while the first is in flight"
        );

        let epoch = app.epoch();
        app.handle_worker(WorkerEvent::MutationFinished {
            epoch,
            session_id: session_id().to_owned(),
            kind: MutationKind::Approval,
            result: Ok(MutationData::Approval(Box::new(ApprovalReceipt {
                request_id: "appr_0".to_owned(),
                decision: Some(decision.as_str().to_owned()),
                reason: "user".to_owned(),
                input_id: None,
                disposition: "applied".to_owned(),
            }))),
        });
        assert!(app.detail().approvals().is_empty());
        assert!(app.detail().approval_error().is_none());
    }
}

#[test]
fn unsupported_actions_have_no_approval_buttons() {
    let (mut app, _dir) = connected_app();
    let generation = open_detail(&mut app);
    let mut snapshot = detail_session("requires_action");
    snapshot.required_actions = vec![json!({ "type": "user_input_request" })];
    finish_group(&mut app, generation, snapshot, empty_items(), empty_turns());
    assert!(app.detail().approvals().is_empty());
    assert!(app.first_approval().is_none());
    assert!(app
        .apply(Intent::AnswerApproval {
            request_id: "appr_0".to_owned(),
            decision: ApprovalDecision::Approve,
        })
        .is_empty());
}

#[test]
fn composer_sends_once_and_reuses_the_key_for_the_same_text_retry() {
    let (mut app, _dir) = connected_app();
    let generation = open_detail(&mut app);
    finish_group(
        &mut app,
        generation,
        detail_session("idle"),
        empty_items(),
        empty_turns(),
    );

    app.detail.composer.text = "hello".to_owned();
    app.detail.composer.cursor = "hello".chars().count();
    let effects = app.apply(Intent::SendMessage);
    let key = match effects.as_slice() {
        [Effect::SendInput { text, key, .. }] => {
            assert_eq!(text, "hello");
            key.clone()
        }
        _ => panic!("unexpected effects"),
    };
    assert!(app.detail().send_in_flight());
    assert!(app.apply(Intent::SendMessage).is_empty());

    let epoch = app.epoch();
    app.handle_worker(WorkerEvent::MutationFinished {
        epoch,
        session_id: session_id().to_owned(),
        kind: MutationKind::SendInput,
        result: Err(ApiError::from_status(500, b"{}")),
    });
    assert!(app.detail().composer_error().is_some());
    assert!(!app.detail().send_in_flight());
    assert_eq!(
        app.detail().composer().text,
        "hello",
        "text is kept on failure"
    );

    let effects = app.apply(Intent::SendMessage);
    match effects.as_slice() {
        [Effect::SendInput { key: retry, .. }] => assert_eq!(retry, &key),
        _ => panic!("unexpected effects"),
    }
}

#[test]
fn composer_clears_on_success_and_rejects_blank_text() {
    let (mut app, _dir) = connected_app();
    let generation = open_detail(&mut app);
    finish_group(
        &mut app,
        generation,
        detail_session("idle"),
        empty_items(),
        empty_turns(),
    );

    app.detail.composer.text = "  ".to_owned();
    assert!(app.apply(Intent::SendMessage).is_empty());
    assert_eq!(
        app.detail().composer_error(),
        Some(crate::ui::text::ERR_MESSAGE_EMPTY)
    );

    app.detail.composer.text = "hi".to_owned();
    let effects = app.apply(Intent::SendMessage);
    assert_eq!(effects.len(), 1);
    let epoch = app.epoch();
    app.handle_worker(WorkerEvent::MutationFinished {
        epoch,
        session_id: session_id().to_owned(),
        kind: MutationKind::SendInput,
        result: Ok(MutationData::Input(Box::new(InputReceipt {
            input_id: "input_1".to_owned(),
            accepted: true,
            disposition: "inserted".to_owned(),
            mode: "turn".to_owned(),
            last_item_seq: 5,
        }))),
    });
    assert_eq!(app.detail().composer().text, "");
    assert_eq!(app.detail().composer().cursor, 0);
    assert!(app.detail().composer_error().is_none());
    assert!(app.detail().send.is_none());
}

#[test]
fn composer_is_disabled_while_approvals_are_required() {
    let (mut app, _dir) = connected_app();
    let generation = open_detail(&mut app);
    finish_group(
        &mut app,
        generation,
        with_approvals(detail_session("requires_action"), 1),
        empty_items(),
        empty_turns(),
    );
    app.detail.composer.text = "blocked".to_owned();
    assert!(app.detail().composer_disabled());
    assert!(app.apply(Intent::SendMessage).is_empty());
    assert!(app.detail().send.is_none());
    assert!(app.detail().composer_error().is_none());
}

#[test]
fn timeline_page_is_reversed_once_and_keeps_official_fields() {
    let mut timeline = TimelineState::default();
    let page = items_page(
        vec![
            json!({
                "id": "reason_1",
                "type": "reasoning",
                "turn_id": "turn_2",
                "summary": [{ "type": "summary_text", "text": "thinking" }]
            }),
            call_item("call_1", "turn_2", "search"),
            message_item("msg_2", "turn_2", "assistant", "answer"),
            message_item("msg_1", "turn_2", "user", "question"),
        ],
        Some("4"),
        true,
    );
    timeline.apply_first_page(&page);
    let ids: Vec<&str> = timeline
        .entries()
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();
    assert_eq!(ids, ["msg_1", "msg_2", "call_1", "reason_1"]);

    assert_eq!(timeline.entries()[0].kind, TimelineKind::UserMessage);
    assert_eq!(timeline.entries()[0].text, "question");
    assert_eq!(timeline.entries()[1].kind, TimelineKind::AssistantMessage);
    assert_eq!(timeline.entries()[1].text, "answer");
    assert_eq!(timeline.entries()[2].kind, TimelineKind::ToolCall);
    assert_eq!(timeline.entries()[2].tool.as_deref(), Some("search"));
    assert_eq!(
        timeline.entries()[2].arguments.as_deref(),
        Some("{\n  \"query\": \"rust\"\n}")
    );
    assert_eq!(timeline.entries()[3].kind, TimelineKind::Reasoning);
    assert_eq!(timeline.entries()[3].text, "thinking");
    assert!(timeline.has_more());
    assert!(timeline.can_load_earlier());
}

#[test]
fn tool_block_expansion_survives_a_refresh_with_the_same_id() {
    let mut timeline = TimelineState::default();
    let page = items_page(vec![call_item("call_1", "turn_1", "search")], None, false);
    timeline.apply_first_page(&page);
    assert!(timeline.entries()[0].has_details());
    timeline.toggle_block("call_1");
    assert!(timeline.is_expanded("call_1"));

    timeline.apply_first_page(&page);
    assert!(
        timeline.is_expanded("call_1"),
        "stable ids keep expansion across refreshes"
    );
    timeline.toggle_block("call_1");
    assert!(!timeline.is_expanded("call_1"));
}

#[test]
fn follow_mode_marks_unseen_items_only_when_scrolled_up() {
    let mut timeline = TimelineState::default();
    timeline.apply_first_page(&items_page(
        vec![message_item("msg_1", "turn_1", "user", "one")],
        None,
        false,
    ));
    assert!(timeline.following());
    assert!(!timeline.unseen());

    timeline.set_following(false);
    timeline.apply_first_page(&items_page(
        vec![
            message_item("msg_2", "turn_1", "assistant", "two"),
            message_item("msg_1", "turn_1", "user", "one"),
        ],
        None,
        false,
    ));
    assert!(timeline.unseen());

    timeline.jump_to_latest();
    assert!(timeline.following());
    assert!(!timeline.unseen());
}

#[test]
fn timeline_caps_at_five_hundred_loaded_items() {
    let timeline = TimelineState {
        entries: (0..TIMELINE_MAX)
            .map(|index| TimelineEntry {
                id: format!("item_{index}"),
                turn_id: "turn_1".to_owned(),
                kind: TimelineKind::Other,
                text: String::new(),
                tool: None,
                arguments: None,
                call_id: None,
                status: None,
                error: None,
                label: "other".to_owned(),
            })
            .collect(),
        has_more: true,
        ..TimelineState::default()
    };
    assert!(timeline.cap_reached());
    assert!(!timeline.can_load_earlier());
}

#[test]
fn load_earlier_uses_the_cursor_and_prepends_older_items() {
    let (mut app, _dir) = connected_app();
    let generation = open_detail(&mut app);
    finish_group(
        &mut app,
        generation,
        detail_session("idle"),
        items_page(
            vec![message_item("msg_2", "turn_1", "assistant", "new")],
            Some("2"),
            true,
        ),
        empty_turns(),
    );

    let effects = app.apply(Intent::LoadEarlierItems);
    match effects.as_slice() {
        [Effect::ListItems {
            session_id: id,
            limit,
            after: Some(after),
            ..
        }] => {
            assert_eq!(id, session_id());
            assert_eq!(*limit, TIMELINE_PAGE_SIZE);
            assert_eq!(after, "2");
        }
        _ => panic!("unexpected effects"),
    }
    assert!(app.detail().timeline().loading_more());

    let epoch = app.epoch();
    app.handle_worker(WorkerEvent::ItemsLoaded {
        epoch,
        generation,
        session_id: session_id().to_owned(),
        limit: TIMELINE_PAGE_SIZE,
        after: Some("2".to_owned()),
        result: Ok(Box::new(items_page(
            vec![message_item("msg_1", "turn_1", "user", "old")],
            None,
            false,
        ))),
    });
    let ids: Vec<&str> = app
        .detail()
        .timeline()
        .entries()
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();
    assert_eq!(ids, ["msg_1", "msg_2"]);
    assert!(!app.detail().timeline().has_more());
}

#[test]
fn turns_load_more_uses_the_next_cursor_and_appends() {
    let (mut app, _dir) = connected_app();
    let generation = open_detail(&mut app);
    finish_group(
        &mut app,
        generation,
        detail_session("idle"),
        empty_items(),
        turns_page(vec![turn("turn_2", "completed")], Some("turn_2"), true),
    );
    assert!(app.detail().turns().can_load_more());

    let effects = app.apply(Intent::LoadMoreTurns);
    match effects.as_slice() {
        [Effect::ListTurns {
            session_id: id,
            limit,
            after: Some(after),
            ..
        }] => {
            assert_eq!(id, session_id());
            assert_eq!(*limit, TURNS_PAGE_SIZE);
            assert_eq!(after, "turn_2");
        }
        _ => panic!("unexpected effects"),
    }

    let epoch = app.epoch();
    app.handle_worker(WorkerEvent::TurnsLoaded {
        epoch,
        generation,
        session_id: session_id().to_owned(),
        limit: TURNS_PAGE_SIZE,
        after: Some("turn_2".to_owned()),
        result: Ok(Box::new(turns_page(
            vec![turn("turn_1", "completed")],
            None,
            false,
        ))),
    });
    let ids: Vec<&str> = app
        .detail()
        .turns()
        .rows()
        .iter()
        .map(|row| row.id.as_str())
        .collect();
    assert_eq!(ids, ["turn_2", "turn_1"]);
    assert!(!app.detail().turns().has_more());
}

#[test]
fn delete_conflict_offers_cancel_and_bad_gateway_explains_the_host() {
    let (mut app, _dir) = connected_app();
    let generation = open_detail(&mut app);
    finish_group(
        &mut app,
        generation,
        detail_session("in_progress"),
        empty_items(),
        empty_turns(),
    );

    app.apply(Intent::OpenDeleteSession);
    assert_eq!(app.overlay(), Some(Overlay::ConfirmDeleteSession));
    let effects = app.apply(Intent::ConfirmDeleteSession);
    assert_eq!(app.overlay(), None);
    assert!(matches!(effects.as_slice(), [Effect::DeleteSession { .. }]));

    let epoch = app.epoch();
    app.handle_worker(WorkerEvent::MutationFinished {
        epoch,
        session_id: session_id().to_owned(),
        kind: MutationKind::Delete,
        result: Err(ApiError::from_status(
            409,
            b"{\"error\":{\"code\":\"turn_in_progress\",\"message\":\"turn running\",\"details\":{\"suggest\":\"cancel\"}}}",
        )),
    });
    let error = app.detail().action_error().expect("action error");
    assert!(error.offer_cancel);
    assert!(error.message.contains("Cancel"));

    app.apply(Intent::OpenDeleteSession);
    app.apply(Intent::ConfirmDeleteSession);
    let epoch = app.epoch();
    app.handle_worker(WorkerEvent::MutationFinished {
        epoch,
        session_id: session_id().to_owned(),
        kind: MutationKind::Delete,
        result: Err(ApiError::from_status(
            502,
            b"{\"error\":{\"code\":\"bad_gateway\",\"message\":\"host down\"}}",
        )),
    });
    let error = app.detail().action_error().expect("action error");
    assert!(!error.offer_cancel);
    assert!(error.message.contains("could not reach its host"));
}

#[test]
fn delete_success_returns_to_the_list_and_refreshes_it() {
    let (mut app, _dir) = connected_app();
    let generation = open_detail(&mut app);
    finish_group(
        &mut app,
        generation,
        detail_session("idle"),
        empty_items(),
        empty_turns(),
    );
    app.apply(Intent::OpenDeleteSession);
    app.apply(Intent::ConfirmDeleteSession);
    let epoch = app.epoch();
    let effects = app.handle_worker(WorkerEvent::MutationFinished {
        epoch,
        session_id: session_id().to_owned(),
        kind: MutationKind::Delete,
        result: Ok(MutationData::Deleted),
    });
    assert_eq!(app.screen(), Screen::Main);
    assert_eq!(app.page(), crate::app::Page::SessionLog);
    assert!(app.detail_id().is_none());
    assert_eq!(app.detail_tab(), DetailTab::Timeline);
    assert!(matches!(effects.as_slice(), [Effect::ListSessions { .. }]));
}

#[test]
fn cancel_turn_requires_confirmation() {
    let (mut app, _dir) = connected_app();
    let generation = open_detail(&mut app);
    finish_group(
        &mut app,
        generation,
        detail_session("in_progress"),
        empty_items(),
        empty_turns(),
    );
    assert!(app.detail().is_running());

    app.apply(Intent::OpenCancelTurn);
    assert_eq!(app.overlay(), Some(Overlay::ConfirmCancelTurn));
    app.apply(Intent::CancelCancelTurn);
    assert_eq!(app.overlay(), None);
    assert!(!app.detail().action_error().is_some());

    app.apply(Intent::OpenCancelTurn);
    let effects = app.apply(Intent::ConfirmCancelTurn);
    assert_eq!(app.overlay(), None);
    match effects.as_slice() {
        [Effect::CancelTurn { session_id: id, .. }] => assert_eq!(id, session_id()),
        _ => panic!("unexpected effects"),
    }

    let epoch = app.epoch();
    app.handle_worker(WorkerEvent::MutationFinished {
        epoch,
        session_id: session_id().to_owned(),
        kind: MutationKind::CancelTurn,
        result: Ok(MutationData::Input(Box::new(InputReceipt {
            input_id: "input_2".to_owned(),
            accepted: true,
            disposition: "inserted".to_owned(),
            mode: "steer".to_owned(),
            last_item_seq: 6,
        }))),
    });
    assert!(app.detail().action_error().is_none());
    assert!(app.detail().reload_wanted);
}

#[test]
fn edit_action_prefills_and_success_returns_to_the_detail() {
    let (mut app, _dir) = connected_app();
    let generation = open_detail(&mut app);
    let snapshot = with_approvals(detail_session("idle"), 0);
    finish_group(&mut app, generation, snapshot, empty_items(), empty_turns());

    app.apply(Intent::OpenEditSession);
    assert_eq!(app.screen(), Screen::SessionEdit);
    assert_eq!(app.edit_form().model().value(), "model-x");
    assert_eq!(
        app.focus(),
        crate::app::Focus::Field(crate::app::FieldId::EditModel)
    );

    app.edit.model.set_value("model-y");
    let effects = app.apply(Intent::SubmitEdit);
    let spec = match effects.as_slice() {
        [Effect::UpdateSession { spec, .. }] => spec,
        _ => panic!("unexpected effects"),
    };
    let body = update_session_body(spec).expect("body");
    assert_eq!(body["agent"]["model"], "model-y");
    assert!(body.get("instructions").is_none());

    let epoch = app.epoch();
    app.handle_worker(WorkerEvent::MutationFinished {
        epoch,
        session_id: session_id().to_owned(),
        kind: MutationKind::Update,
        result: Ok(MutationData::Updated(Box::new(detail_session("idle")))),
    });
    assert_eq!(app.screen(), Screen::SessionDetail);
    assert!(app.detail().reload_wanted);
}

#[test]
fn edit_conflict_offers_the_cancel_turn_action() {
    let (mut app, _dir) = connected_app();
    let generation = open_detail(&mut app);
    finish_group(
        &mut app,
        generation,
        detail_session("in_progress"),
        empty_items(),
        empty_turns(),
    );
    app.apply(Intent::OpenEditSession);
    assert_eq!(app.screen(), Screen::SessionEdit);
    app.apply(Intent::SubmitEdit);
    let epoch = app.epoch();
    app.handle_worker(WorkerEvent::MutationFinished {
        epoch,
        session_id: session_id().to_owned(),
        kind: MutationKind::Update,
        result: Err(ApiError::from_status(
            409,
            b"{\"error\":{\"code\":\"turn_in_progress\",\"message\":\"running\"}}",
        )),
    });
    assert_eq!(app.screen(), Screen::SessionEdit);
    assert!(app.edit_form().offer_cancel());
    assert!(app.edit_form().form_error().unwrap().contains("cancel"));
    assert!(!app.edit_form().submitting());
}

#[test]
fn refresh_action_is_a_no_op_outside_the_detail_page() {
    let (mut app, _dir) = connected_app();
    let effects = app.apply(Intent::RefreshDetail);
    assert!(effects.is_empty());
    assert_eq!(app.screen(), Screen::Main);
    assert!(app.apply(Intent::LoadEarlierItems).is_empty());
    assert!(app.apply(Intent::LoadMoreTurns).is_empty());
    assert!(app.apply(Intent::SendMessage).is_empty());
    assert!(app.detail_action(ActionId::Back).is_none());
}
