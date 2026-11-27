use std::time::Instant;

use ratatui::backend::TestBackend;
use ratatui::Terminal;

use crate::app::{ActionId, App, Effect, FieldId, Intent};
use crate::test_support::{connected_app, session};
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

/// A click on a multiline field registers a row/column-resolved target.
fn has_text_area_target(hits: &HitMap, field: FieldId) -> bool {
    hits.regions().iter().any(|region| {
        matches!(
            &region.intent,
            Intent::FocusTextAreaAt { field: hit, .. } if *hit == field
        )
    })
}

#[test]
fn every_create_control_has_a_hit_target() {
    let (mut app, _dir) = connected_app();
    app.apply(Intent::NewSession);
    let (_terminal, hits) = draw(&mut app, 110, 44);

    for field in [
        FieldId::CreateLogicalId,
        FieldId::CreateModel,
        FieldId::CreateCwd,
    ] {
        assert!(
            hits.region_rect(Intent::FocusField { field, click: None })
                .is_some(),
            "missing {field:?}"
        );
    }
    assert!(has_text_area_target(&hits, FieldId::CreateInstructions));
    for action in [
        ActionId::AddMetadata,
        ActionId::ToggleAdvanced,
        ActionId::CreateSubmit,
        ActionId::Back,
    ] {
        assert!(
            hits.region_rect(Intent::Activate(action)).is_some(),
            "missing {action:?}"
        );
    }
}

#[test]
fn advanced_json_fields_appear_after_the_toggle() {
    let (mut app, _dir) = connected_app();
    app.apply(Intent::NewSession);
    let (_terminal, hits) = draw(&mut app, 110, 44);
    assert!(!has_text_area_target(&hits, FieldId::CreateTools));

    let toggle = hits
        .region_rect(Intent::Activate(ActionId::ToggleAdvanced))
        .expect("toggle");
    let clicked = hits.click(false, toggle.x, toggle.y).expect("toggle click");
    app.apply(clicked);
    assert!(app.create_form().advanced_open());

    let (_terminal, hits) = draw(&mut app, 110, 70);
    for field in [
        FieldId::CreateTools,
        FieldId::CreateEnvironment,
        FieldId::CreatePromptCache,
    ] {
        assert!(has_text_area_target(&hits, field), "missing {field:?}");
    }
}

#[test]
fn metadata_rows_render_remove_targets() {
    let (mut app, _dir) = connected_app();
    app.apply(Intent::NewSession);
    app.apply(Intent::AddMetadataRow);
    assert_eq!(app.create_form().metadata().len(), 1);
    let (_terminal, hits) = draw(&mut app, 110, 48);
    assert!(hits.region_rect(Intent::RemoveMetadataRow(0)).is_some());
}

#[test]
fn submitting_replaces_the_create_button_and_shows_progress() {
    let (mut app, _dir) = connected_app();
    app.apply(Intent::NewSession);
    let effects = app.apply(Intent::SubmitCreate);
    assert!(matches!(
        effects.as_slice(),
        [crate::app::Effect::CreateSession { .. }]
    ));
    assert!(app.create_form().submitting());

    let (terminal, hits) = draw(&mut app, 110, 44);
    assert!(hits
        .region_rect(Intent::Activate(ActionId::CreateSubmit))
        .is_none());
    assert!(buffer_text(&terminal).contains(text::CREATE_SUBMITTING));
}

#[test]
fn inline_validation_errors_are_rendered() {
    let (mut app, _dir) = connected_app();
    app.apply(Intent::NewSession);
    assert!(app.set_field_value(FieldId::CreateLogicalId, "bad id!"));
    assert!(app.apply(Intent::SubmitCreate).is_empty());
    let (terminal, _) = draw(&mut app, 110, 44);
    assert!(buffer_text(&terminal).contains("Logical ID must match"));
}

#[test]
fn edit_form_renders_its_controls_and_read_only_instructions() {
    let (mut app, _dir) = connected_app();
    let mut snapshot = session("sess_1", "idle", "active", Some("model-x"), 100);
    snapshot.agent.instructions = Some("be brief".to_owned());
    app.apply(Intent::OpenSession("sess_1".to_owned()));
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
        session_id: "sess_1".to_owned(),
        result: Ok(Box::new(snapshot)),
    });
    app.apply(Intent::OpenEditSession);
    assert_eq!(app.screen(), crate::app::Screen::SessionEdit);

    app.apply(Intent::AddMetadataRow);
    let (terminal, hits) = draw(&mut app, 110, 60);
    let text = buffer_text(&terminal);
    assert!(text.contains(text::EDIT_TITLE));
    assert!(text.contains("model-x"));
    assert!(text.contains("be brief"));
    assert!(text.contains("Read-only"));
    assert!(hits
        .region_rect(Intent::FocusField {
            field: FieldId::EditModel,
            click: None,
        })
        .is_some());
    assert!(has_text_area_target(&hits, FieldId::EditPromptCache));
    assert!(hits
        .region_rect(Intent::FocusField {
            field: FieldId::EditMetadataKey(0),
            click: None,
        })
        .is_some());
    for action in [ActionId::AddMetadata, ActionId::EditSubmit, ActionId::Back] {
        assert!(
            hits.region_rect(Intent::Activate(action)).is_some(),
            "missing {action:?}"
        );
    }
    assert!(hits.region_rect(Intent::RemoveMetadataRow(0)).is_some());

    // The same intents edit the edit form's metadata rows.
    let before = app.edit_form().metadata().len();
    let remove = hits
        .region_rect(Intent::RemoveMetadataRow(0))
        .expect("remove");
    app.apply(hits.click(false, remove.x, remove.y).expect("remove click"));
    assert_eq!(app.edit_form().metadata().len(), before - 1);
}

#[test]
fn clicking_a_multiline_field_places_the_caret_on_the_clicked_row() {
    let (mut app, _dir) = connected_app();
    app.apply(Intent::NewSession);
    assert!(app.set_text_area_value(FieldId::CreateInstructions, "abc\ndef\nghi"));
    let (_terminal, hits) = draw(&mut app, 110, 44);
    let rect = hits
        .regions()
        .iter()
        .find_map(|region| match &region.intent {
            Intent::FocusTextAreaAt {
                field: FieldId::CreateInstructions,
                ..
            } => Some(region.rect),
            _ => None,
        })
        .expect("instructions hit region");

    let intent = hits
        .click(false, rect.x + 2, rect.y + 2)
        .expect("resolved click");
    assert!(matches!(
        intent,
        Intent::FocusTextAreaAt {
            field: FieldId::CreateInstructions,
            click: crate::app::TextAreaClick {
                row: 1,
                column: 1,
                ..
            },
        }
    ));
    app.apply(intent);
    let area = app
        .text_area(FieldId::CreateInstructions)
        .expect("instructions field");
    assert_eq!(area.cursor, 5);
    assert_eq!(
        app.focus(),
        crate::app::Focus::Field(FieldId::CreateInstructions)
    );
}
