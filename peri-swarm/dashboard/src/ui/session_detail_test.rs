use std::time::Instant;

use serde_json::{json, Value};

use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;

use crate::api::{ApprovalDecision, ItemsPage, Turns};
use crate::app::{ActionId, App, DetailTab, Effect, FieldId, Focus, Intent};
use crate::test_support::{connected_app, session, with_approvals};
use crate::ui::hit::HitMap;
use crate::ui::text;
use crate::worker::WorkerEvent;

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

/// Open the detail page and finish the snapshot request, leaving the two page
/// requests in flight; the frame layout is what the hit tests care about.
fn detail_app(snapshot: crate::api::Session) -> (App, tempfile::TempDir) {
    let (mut app, dir) = connected_app();
    let id = snapshot.id.clone();
    app.apply(Intent::OpenSession(id.clone()));
    let effects = app.detail_due(Instant::now());
    let generation = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::GetSession { generation, .. } => Some(*generation),
            _ => None,
        })
        .expect("detail group");
    let epoch = app.epoch();
    app.handle_worker(WorkerEvent::SessionLoaded {
        epoch,
        generation,
        session_id: id,
        result: Ok(Box::new(snapshot)),
    });
    (app, dir)
}

fn detail_session() -> crate::api::Session {
    session("sess_1", "idle", "active", Some("model-x"), 100)
}

fn composer_intent(hits: &HitMap) -> Intent {
    hits.regions()
        .iter()
        .find(|region| {
            matches!(
                region.intent,
                Intent::FocusTextAreaAt {
                    field: FieldId::DetailComposer,
                    ..
                }
            )
        })
        .map(|region| region.intent.clone())
        .expect("composer region")
}

#[test]
fn detail_skeleton_renders_header_tabs_and_back() {
    let (mut app, _dir) = connected_app();
    app.apply(Intent::OpenSession("sess_42".to_owned()));
    let (terminal, hits) = draw(&mut app, 100, 30);
    let text = buffer_text(&terminal);
    assert!(text.contains(text::DETAIL_TITLE));
    assert!(text.contains("sess_42"));
    assert!(text.contains(DetailTab::Timeline.label()));
    assert!(hits.region_rect(Intent::Activate(ActionId::Back)).is_some());
    for tab in DetailTab::ALL {
        assert!(
            hits.region_rect(Intent::SelectDetailTab(tab)).is_some(),
            "missing {tab:?}"
        );
    }
}

#[test]
fn tab_clicks_and_arrow_keys_change_the_active_tab() {
    let (mut app, _dir) = connected_app();
    app.apply(Intent::OpenSession("sess_42".to_owned()));
    let (_terminal, hits) = draw(&mut app, 100, 30);

    let turns = hits
        .region_rect(Intent::SelectDetailTab(DetailTab::Turns))
        .expect("turns tab");
    let clicked = hits.click(false, turns.x, turns.y).expect("tab click");
    app.apply(clicked);
    assert_eq!(app.detail_tab(), DetailTab::Turns);

    app.apply(Intent::Back);
    app.apply(Intent::OpenSession("sess_42".to_owned()));
    assert_eq!(app.detail_tab(), DetailTab::Timeline);
    for expected in [DetailTab::Turns, DetailTab::Info, DetailTab::Timeline] {
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.detail_tab(), expected);
    }
    app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    assert_eq!(app.detail_tab(), DetailTab::Info);
}

#[test]
fn tab_hit_regions_cover_the_drawn_labels() {
    let (mut app, _dir) = connected_app();
    app.apply(Intent::OpenSession("sess_42".to_owned()));
    let (terminal, hits) = draw(&mut app, 100, 30);
    let buffer = terminal.backend().buffer();
    let area = buffer.area;
    for tab in DetailTab::ALL {
        let rect = hits
            .region_rect(Intent::SelectDetailTab(tab))
            .unwrap_or_else(|| panic!("missing {tab:?}"));
        let cells: Vec<&str> = (area.x..area.right())
            .map(|x| {
                buffer
                    .cell((x, rect.y))
                    .map_or("", ratatui::buffer::Cell::symbol)
            })
            .collect();
        let width = u16::try_from(tab.label().chars().count()).expect("width");
        let start_index = cells
            .windows(usize::from(width))
            .position(|window| window.concat() == tab.label())
            .unwrap_or_else(|| panic!("label {tab:?} not drawn"));
        let start = area.x + u16::try_from(start_index).expect("column");
        assert_eq!(rect.x, start, "{tab:?} hit region starts on the label");
        assert_eq!(rect.width, width, "{tab:?} hit region covers the label");
        for x in [start, start + width / 2, start + width - 1] {
            let clicked = hits
                .click(false, x, rect.y)
                .unwrap_or_else(|| panic!("{tab:?} not clickable at column {x}"));
            assert!(
                matches!(clicked, Intent::SelectDetailTab(hit) if hit == tab),
                "{tab:?} click at column {x} resolved to the wrong target"
            );
        }
        if start > area.x {
            let gap = hits.click(false, start - 1, rect.y);
            assert!(
                !matches!(gap, Some(Intent::SelectDetailTab(hit)) if hit == tab),
                "{tab:?} must not claim the gap before its label"
            );
        }
    }
}

#[test]
fn back_returns_to_the_session_log() {
    let (mut app, _dir) = connected_app();
    app.apply(Intent::OpenSession("sess_42".to_owned()));
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.screen(), crate::app::Screen::Main);
    assert_eq!(app.page(), crate::app::Page::SessionLog);
}

#[test]
fn detail_controls_have_hit_targets_at_80_100_and_140() {
    for width in [80, 100, 140] {
        let (mut app, _dir) = detail_app(with_approvals(
            session("sess_1", "requires_action", "active", Some("model-x"), 100),
            1,
        ));
        let (terminal, hits) = draw(&mut app, width, 40);
        assert!(buffer_text(&terminal).contains("sess_1"), "width {width}");

        for intent in [
            Intent::Activate(ActionId::Back),
            Intent::Activate(ActionId::RefreshDetail),
            Intent::Activate(ActionId::EditSession),
            Intent::Activate(ActionId::DeleteSession),
            Intent::Activate(ActionId::CancelTurn),
            Intent::Activate(ActionId::SendMessage),
        ] {
            assert!(
                hits.region_rect(intent.clone()).is_some(),
                "missing {intent:?} at width {width}"
            );
        }
        for tab in DetailTab::ALL {
            assert!(
                hits.region_rect(Intent::SelectDetailTab(tab)).is_some(),
                "missing {tab:?} at width {width}"
            );
        }
        let composer = composer_intent(&hits);
        assert!(
            matches!(
                composer,
                Intent::FocusTextAreaAt {
                    field: FieldId::DetailComposer,
                    ..
                }
            ),
            "width {width}"
        );

        let mut decisions = Vec::new();
        for region in hits.regions() {
            if let Intent::AnswerApproval {
                request_id,
                decision,
            } = &region.intent
            {
                assert_eq!(request_id, "appr_0", "width {width}");
                decisions.push(*decision);
            }
        }
        assert!(
            decisions.contains(&ApprovalDecision::Approve),
            "width {width}"
        );
        assert!(decisions.contains(&ApprovalDecision::Deny), "width {width}");
    }
}

#[test]
fn tab_and_composer_clicks_change_state_at_every_width() {
    for width in [80, 100, 140] {
        let (mut app, _dir) = detail_app(detail_session());
        let (_terminal, hits) = draw(&mut app, width, 40);
        let turns = hits
            .region_rect(Intent::SelectDetailTab(DetailTab::Turns))
            .expect("turns tab");
        let clicked = hits.click(false, turns.x, turns.y).expect("tab click");
        app.apply(clicked);
        assert_eq!(app.detail_tab(), DetailTab::Turns, "width {width}");

        let (_terminal, hits) = draw(&mut app, width, 40);
        let composer = hits
            .regions()
            .iter()
            .find(|region| {
                matches!(
                    region.intent,
                    Intent::FocusTextAreaAt {
                        field: FieldId::DetailComposer,
                        ..
                    }
                )
            })
            .map(|region| region.rect)
            .expect("composer region");
        let clicked = hits
            .click(false, composer.x + 1, composer.y + 1)
            .expect("composer click");
        assert!(
            matches!(
                clicked,
                Intent::FocusTextAreaAt {
                    field: FieldId::DetailComposer,
                    ..
                }
            ),
            "width {width}"
        );
        app.apply(clicked);
        assert_eq!(app.focus(), Focus::Field(FieldId::DetailComposer));
    }
}

#[test]
fn approval_clicks_answer_the_listed_request() {
    let (mut app, _dir) = detail_app(with_approvals(
        session("sess_1", "requires_action", "active", Some("model-x"), 100),
        1,
    ));
    let (_terminal, hits) = draw(&mut app, 100, 40);
    let approve = Intent::AnswerApproval {
        request_id: "appr_0".to_owned(),
        decision: ApprovalDecision::Approve,
    };
    let rect = hits.region_rect(approve.clone()).expect("approve region");
    let clicked = hits.click(false, rect.x, rect.y).expect("approve click");
    assert_eq!(clicked, approve);
    let effects = app.apply(clicked);
    assert_eq!(effects.len(), 1);
    assert!(app.detail().approval_in_flight());
}

#[test]
fn delete_confirm_is_modal_at_every_width() {
    for width in [80, 100, 140] {
        let (mut app, _dir) = detail_app(detail_session());
        app.apply(Intent::OpenDeleteSession);
        let (terminal, hits) = draw(&mut app, width, 40);
        assert!(buffer_text(&terminal).contains(text::DELETE_TITLE));

        let confirm = Intent::Activate(ActionId::ConfirmDeleteSession);
        let cancel = Intent::Activate(ActionId::CancelDeleteSession);
        assert!(hits.region_rect(confirm.clone()).is_some(), "width {width}");
        assert!(hits.region_rect(cancel.clone()).is_some(), "width {width}");

        let back = hits
            .region_rect(Intent::Activate(ActionId::Back))
            .expect("back region");
        assert_eq!(
            hits.click(true, back.x, back.y),
            None,
            "modal blocks the base layer at width {width}"
        );
        let panel = hits.overlay_area().expect("overlay panel");
        assert_eq!(
            hits.click(true, panel.x + 1, panel.y + 1),
            None,
            "clicks inside the panel but off a button are swallowed"
        );

        let rect = hits.region_rect(cancel.clone()).expect("cancel region");
        let clicked = hits.click(true, rect.x, rect.y).expect("cancel click");
        assert_eq!(clicked, cancel);
        app.apply(clicked);
        assert_eq!(app.overlay(), None);
    }
}

#[test]
fn cancel_turn_confirm_has_both_modal_actions() {
    let (mut app, _dir) = detail_app(session(
        "sess_1",
        "in_progress",
        "active",
        Some("model-x"),
        100,
    ));
    app.apply(Intent::OpenCancelTurn);
    let (terminal, hits) = draw(&mut app, 100, 40);
    assert!(buffer_text(&terminal).contains(text::CANCEL_TURN_TITLE));

    let confirm = Intent::Activate(ActionId::ConfirmCancelTurn);
    let cancel = Intent::Activate(ActionId::CancelCancelTurn);
    assert!(hits.region_rect(confirm.clone()).is_some());
    assert!(hits.region_rect(cancel.clone()).is_some());

    let rect = hits.region_rect(cancel.clone()).expect("cancel region");
    app.apply(hits.click(true, rect.x, rect.y).expect("cancel click"));
    assert_eq!(app.overlay(), None);

    app.apply(Intent::OpenCancelTurn);
    let (_terminal, hits) = draw(&mut app, 100, 40);
    let rect = hits.region_rect(confirm.clone()).expect("confirm region");
    let clicked = hits.click(true, rect.x, rect.y).expect("confirm click");
    assert_eq!(clicked, confirm);
    let effects = app.apply(clicked);
    assert_eq!(effects.len(), 1);
    assert_eq!(app.overlay(), None);
}

// ---------------------------------------------------------------------------
// Timeline viewport, follow mode and the fixed jump control
// ---------------------------------------------------------------------------

const TIMELINE_LIMIT: u32 = 50;

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

fn turns_page() -> Turns {
    Turns {
        object: Some("list".to_owned()),
        data: Vec::new(),
        has_more: false,
        next_cursor: None,
    }
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

fn many_entries() -> Vec<Value> {
    // The gateway returns the newest page first (desc); the view re-reverses it.
    (0..100)
        .rev()
        .map(|index| {
            message_item(
                &format!("msg_{index:03}"),
                &format!("turn_{index:03}"),
                &format!("entry-number-{index:03}"),
            )
        })
        .collect()
}

fn timeline_app(
    entries: Vec<Value>,
    cursor: Option<&str>,
    has_more: bool,
) -> (App, tempfile::TempDir) {
    let (mut app, dir) = connected_app();
    let snapshot = detail_session();
    let id = snapshot.id.clone();
    app.apply(Intent::OpenSession(id.clone()));
    let effects = app.detail_due(Instant::now());
    let generation = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::GetSession { generation, .. } => Some(*generation),
            _ => None,
        })
        .expect("detail group");
    let epoch = app.epoch();
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
        limit: TIMELINE_LIMIT,
        after: None,
        result: Ok(Box::new(items_page(entries, cursor, has_more))),
    });
    app.handle_worker(WorkerEvent::TurnsLoaded {
        epoch,
        generation,
        session_id: id,
        limit: TIMELINE_LIMIT,
        after: None,
        result: Ok(Box::new(turns_page())),
    });
    (app, dir)
}

fn merge_items(app: &mut App, items: Vec<Value>) {
    let epoch = app.epoch();
    let generation = app.detail().poll.generation();
    let session_id = app.detail_id().expect("open session").to_owned();
    app.handle_worker(WorkerEvent::ItemsLoaded {
        epoch,
        generation,
        session_id,
        limit: TIMELINE_LIMIT,
        after: None,
        result: Ok(Box::new(items_page(items, None, false))),
    });
}

#[test]
fn timeline_follows_the_newest_entries_after_page_updates() {
    let (mut app, _dir) = timeline_app(many_entries(), None, false);
    let (terminal, _hits) = draw(&mut app, 100, 40);
    let text = buffer_text(&terminal);
    assert!(
        text.contains("entry-number-099"),
        "follow mode must show the latest timeline item"
    );
    assert!(!text.contains("entry-number-000"));
    assert!(app.detail_timeline_following());
    assert_eq!(app.content_scroll(), app.content_max());
    let max_before = app.content_max();

    merge_items(
        &mut app,
        vec![
            message_item("msg_new", "turn_new", "entry-number-newest"),
            message_item("msg_099", "turn_099", "entry-number-099"),
        ],
    );
    let (terminal, hits) = draw(&mut app, 100, 40);
    let text = buffer_text(&terminal);
    assert!(text.contains("entry-number-newest"));
    assert!(text.contains("entry-number-099"));
    assert!(!text.contains("entry-number-000"));
    assert!(app.detail_timeline_following());
    assert!(app.content_max() > max_before);
    assert_eq!(app.content_scroll(), app.content_max());
    assert!(!app.timeline_unseen());
    assert!(
        hits.region_rect(Intent::Activate(ActionId::JumpToLatest))
            .is_none(),
        "the jump control is hidden while following"
    );
}

#[test]
fn landing_on_the_bottom_keeps_following_across_an_unrendered_merge() {
    let (mut app, _dir) = timeline_app(many_entries(), None, false);
    let (_terminal, _hits) = draw(&mut app, 100, 40);
    let max_before = app.content_max();
    assert!(max_before > 0, "the fixture must overflow the viewport");
    assert!(app.detail_timeline_following());

    // Scroll up and render once, so the follow intent is false.
    app.set_content_scroll(0);
    let (_terminal, _hits) = draw(&mut app, 100, 40);
    assert!(!app.detail_timeline_following());

    // Land on the current bottom without rendering the frame that would have
    // recorded it, then let a poll merge append a newer item.
    app.set_content_scroll(max_before);
    assert!(
        app.detail_timeline_following(),
        "landing on the bottom restores following immediately"
    );
    merge_items(
        &mut app,
        vec![message_item("msg_new", "turn_new", "entry-number-newest")],
    );
    assert!(app.detail_timeline_following());
    assert!(!app.timeline_unseen());

    // The next frame re-anchors to the new bottom.
    let (_terminal, _hits) = draw(&mut app, 100, 40);
    assert!(app.content_max() > max_before);
    assert_eq!(app.content_scroll(), app.content_max());
    assert!(app.detail_timeline_following());
    assert!(!app.timeline_unseen());
}

#[test]
fn scrolled_up_timeline_keeps_its_offset_and_exposes_a_fixed_jump_control() {
    let (mut app, _dir) = timeline_app(many_entries(), Some("cursor-1"), true);
    let (_terminal, _hits) = draw(&mut app, 100, 40);
    assert!(app.content_scroll() > 0);

    app.set_content_scroll(0);
    let (terminal, hits) = draw(&mut app, 100, 40);
    assert_eq!(app.content_scroll(), 0);
    assert!(!app.detail_timeline_following());
    assert!(!buffer_text(&terminal).contains("entry-number-099"));

    // `Load earlier` sits at the top of the scrolling content.
    let load = Intent::Activate(ActionId::LoadEarlierItems);
    let rect = hits.region_rect(load.clone()).expect("load earlier at top");
    assert_eq!(hits.click(false, rect.x, rect.y), Some(load));

    // `Jump to latest` stays fixed at the bottom-right and is clickable even
    // though the scrolling content is showing its top.
    let jump = Intent::Activate(ActionId::JumpToLatest);
    assert!(
        buffer_text(&terminal).contains(text::action_label(ActionId::JumpToLatest)),
        "the fixed jump control must be drawn"
    );
    let jump_rect = hits
        .region_rect(jump.clone())
        .expect("fixed jump control is visible");
    assert_eq!(
        hits.click(false, jump_rect.x, jump_rect.y),
        Some(jump.clone())
    );
    assert!(jump_rect.y >= 2 && jump_rect.bottom() <= 40);

    merge_items(
        &mut app,
        vec![
            message_item("msg_new", "turn_new", "entry-number-newest"),
            message_item("msg_099", "turn_099", "entry-number-099"),
        ],
    );
    let (terminal, hits) = draw(&mut app, 100, 40);
    assert_eq!(
        app.content_scroll(),
        0,
        "a scrolled-up view keeps its offset"
    );
    assert!(app.timeline_unseen());
    assert!(!app.detail_timeline_following());
    assert!(!buffer_text(&terminal).contains("entry-number-newest"));
    let jump_rect = hits
        .region_rect(jump.clone())
        .expect("jump stays visible while unseen");
    assert_eq!(
        hits.click(false, jump_rect.x, jump_rect.y),
        Some(jump.clone())
    );

    app.apply(jump);
    let (terminal, _) = draw(&mut app, 100, 40);
    assert!(buffer_text(&terminal).contains("entry-number-newest"));
    assert!(app.detail_timeline_following());
    assert!(!app.timeline_unseen());
    assert_eq!(app.content_scroll(), app.content_max());
}
