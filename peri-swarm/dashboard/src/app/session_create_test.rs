use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::api::{CreateSessionSpec, CreatedSession};
use crate::app::{App, Effect, FieldId, Intent, Page, Screen};
use crate::test_support::{connected_app, session};
use crate::worker::WorkerEvent;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

fn open_form(app: &mut App) {
    app.apply(Intent::NewSession);
    assert_eq!(app.screen(), Screen::SessionCreate);
}

fn create_request(effects: &[Effect]) -> (u64, CreateSessionSpec, String) {
    match effects {
        [Effect::CreateSession { epoch, spec, key }] => {
            (*epoch, spec.clone(), key.as_str().to_owned())
        }
        _ => panic!("expected exactly one CreateSession effect"),
    }
}

#[test]
fn invalid_logical_id_blocks_submission() {
    let (mut app, _dir) = connected_app();
    open_form(&mut app);
    assert!(app.set_field_value(FieldId::CreateLogicalId, "bad id!"));

    let effects = app.apply(Intent::SubmitCreate);
    assert!(effects.is_empty());
    assert!(app
        .create_form()
        .logical_id_error()
        .is_some_and(|error| error.contains("Logical ID")));
    assert!(!app.create_form().submitting());
    assert_eq!(app.screen(), Screen::SessionCreate);
}

#[test]
fn metadata_requires_keys_and_unique_names() {
    let (mut app, _dir) = connected_app();
    open_form(&mut app);

    app.apply(Intent::AddMetadataRow);
    app.apply(Intent::AddMetadataRow);
    assert_eq!(app.create_form().metadata().len(), 2);
    assert!(app.set_field_value(FieldId::CreateMetadataValue(0), "orphan"));
    let effects = app.apply(Intent::SubmitCreate);
    assert!(effects.is_empty());
    assert!(app
        .create_form()
        .metadata_error()
        .is_some_and(|error| error.contains("must not be empty")));

    assert!(app.set_field_value(FieldId::CreateMetadataKey(0), "team"));
    assert!(app.set_field_value(FieldId::CreateMetadataValue(0), "core"));
    assert!(app.set_field_value(FieldId::CreateMetadataKey(1), "team"));
    assert!(app.set_field_value(FieldId::CreateMetadataValue(1), "other"));
    let effects = app.apply(Intent::SubmitCreate);
    assert!(effects.is_empty());
    assert!(app
        .create_form()
        .metadata_error()
        .is_some_and(|error| error.contains("unique")));

    app.apply(Intent::RemoveMetadataRow(1));
    assert_eq!(app.create_form().metadata().len(), 1);
    let effects = app.apply(Intent::SubmitCreate);
    let (_, spec, _) = create_request(&effects);
    assert_eq!(spec.metadata.get("team").map(String::as_str), Some("core"));
}

#[test]
fn advanced_json_must_parse_before_submission() {
    let (mut app, _dir) = connected_app();
    open_form(&mut app);
    app.apply(Intent::ToggleAdvanced);
    assert!(app.create_form().advanced_open());

    assert!(app.set_text_area_value(FieldId::CreateTools, "[{\"name\":\"search\"}]"));
    assert!(app.set_text_area_value(FieldId::CreateEnvironment, "{not json"));
    let effects = app.apply(Intent::SubmitCreate);
    assert!(effects.is_empty());
    assert!(app.create_form().tools_error().is_none());
    assert!(app
        .create_form()
        .environment_error()
        .is_some_and(|error| error.contains("does not parse")));

    assert!(app.set_text_area_value(FieldId::CreateEnvironment, "{\"kind\":\"none\"}"));
    let effects = app.apply(Intent::SubmitCreate);
    let (_, spec, _) = create_request(&effects);
    assert!(spec.tools.is_some());
    assert!(spec.environment.is_some());
}

#[test]
fn valid_submission_builds_the_spec_and_navigates_to_detail() {
    let (mut app, _dir) = connected_app();
    open_form(&mut app);
    assert!(app.set_field_value(FieldId::CreateLogicalId, "alpha-1"));
    assert!(app.set_field_value(FieldId::CreateModel, "gpt-5"));
    assert!(app.set_field_value(FieldId::CreateCwd, "/tmp/work"));
    assert!(app.set_text_area_value(FieldId::CreateInstructions, "Be kind.\nTwo lines"));
    app.apply(Intent::AddMetadataRow);
    assert!(app.set_field_value(FieldId::CreateMetadataKey(0), "team"));
    assert!(app.set_field_value(FieldId::CreateMetadataValue(0), "core"));

    let effects = app.apply(Intent::SubmitCreate);
    let (epoch, spec, key) = create_request(&effects);
    assert!(app.create_form().submitting());
    assert_eq!(spec.logical_id.as_deref(), Some("alpha-1"));
    assert_eq!(spec.model.as_deref(), Some("gpt-5"));
    assert_eq!(spec.cwd.as_deref(), Some("/tmp/work"));
    assert_eq!(spec.instructions.as_deref(), Some("Be kind.\nTwo lines"));
    assert_eq!(spec.metadata.get("team").map(String::as_str), Some("core"));
    assert!(spec.tools.is_none() && spec.environment.is_none());
    assert!(!key.is_empty());

    let created = CreatedSession {
        session: session("sess_9", "idle", "idle", Some("gpt-5"), 1),
        created: true,
    };
    app.handle_worker(WorkerEvent::SessionCreated {
        epoch,
        result: Ok(Box::new(created)),
    });

    assert_eq!(app.screen(), Screen::SessionDetail);
    assert_eq!(app.detail_id(), Some("sess_9"));
    assert_eq!(app.session_log().selected(), Some("sess_9"));
    assert!(!app.create_form().submitting());
}

#[test]
fn blank_optional_fields_are_omitted() {
    let (mut app, _dir) = connected_app();
    open_form(&mut app);
    let effects = app.apply(Intent::SubmitCreate);
    let (_, spec, _) = create_request(&effects);
    assert!(spec.logical_id.is_none());
    assert!(spec.model.is_none());
    assert!(spec.cwd.is_none());
    assert!(spec.instructions.is_none());
    assert!(spec.metadata.is_empty());
}

#[test]
fn identity_conflict_is_shown_on_the_logical_id() {
    let (mut app, _dir) = connected_app();
    open_form(&mut app);
    assert!(app.set_field_value(FieldId::CreateLogicalId, "taken"));
    let effects = app.apply(Intent::SubmitCreate);
    let (epoch, _, _) = create_request(&effects);

    app.handle_worker(WorkerEvent::SessionCreated {
        epoch,
        result: Err(crate::api::ApiError::from_status(
            409,
            br#"{"error":{"code":"conflict","message":"logical id already used","details":null}}"#,
        )),
    });

    assert_eq!(app.screen(), Screen::SessionCreate);
    assert!(!app.create_form().submitting());
    let error = app
        .create_form()
        .logical_id_error()
        .expect("conflict error on the logical id");
    assert!(error.contains("already exists"));
    assert!(
        error.contains("logical id already used"),
        "the gateway message must be shown: {error}"
    );
    assert!(
        error.contains("conflict") && error.contains("HTTP 409"),
        "the gateway code and status must be shown: {error}"
    );
}

#[test]
fn escape_returns_to_the_session_log() {
    let (mut app, _dir) = connected_app();
    open_form(&mut app);
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.screen(), Screen::Main);
    assert_eq!(app.page(), Page::SessionLog);
}

#[test]
fn removing_a_focused_row_moves_focus_to_the_add_button() {
    let (mut app, _dir) = connected_app();
    open_form(&mut app);
    app.apply(Intent::AddMetadataRow);
    assert_eq!(app.create_form().metadata().len(), 1);
    app.apply(Intent::FocusField {
        field: FieldId::CreateMetadataKey(0),
        click: None,
    });
    app.apply(Intent::RemoveMetadataRow(0));
    assert_eq!(app.create_form().metadata().len(), 0);
    assert_eq!(
        app.focus(),
        crate::app::Focus::Action(crate::app::ActionId::AddMetadata)
    );
}

#[test]
fn multiline_field_keys_select_words_and_lines() {
    let (mut app, _dir) = connected_app();
    open_form(&mut app);
    let field = FieldId::CreateInstructions;
    let original = "alpha beta\ngamma delta";
    assert!(app.set_text_area_value(field, original));
    app.apply(Intent::FocusTextArea(field));
    let cursor = |app: &App| app.text_area(field).expect("field").cursor;
    let selection = |app: &App| app.text_area(field).expect("field").selection_range();
    assert_eq!(cursor(&app), 22);

    app.handle_key(key(KeyCode::Home));
    assert_eq!(cursor(&app), 11);
    app.handle_key(key(KeyCode::End));
    assert_eq!(cursor(&app), 22);
    app.handle_key(ctrl(KeyCode::Home));
    assert_eq!(cursor(&app), 0);

    app.handle_key(ctrl(KeyCode::Right));
    assert_eq!(cursor(&app), 6);
    app.handle_key(KeyEvent::new(
        KeyCode::Right,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
    assert_eq!((cursor(&app), selection(&app)), (11, Some((6, 11))));
    app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::SHIFT));
    assert_eq!((cursor(&app), selection(&app)), (22, Some((6, 22))));

    app.handle_key(ctrl(KeyCode::Char('a')));
    assert_eq!(selection(&app), Some((0, 22)));
    app.handle_key(key(KeyCode::Char('x')));
    assert_eq!(app.text_area(field).expect("field").text, "x");

    assert!(app.set_text_area_value(field, original));
    app.apply(Intent::FocusTextArea(field));
    app.handle_key(ctrl(KeyCode::Backspace));
    assert_eq!(app.text_area(field).expect("field").text, "alpha beta\n");

    assert!(app.set_text_area_value(field, original));
    app.apply(Intent::FocusTextArea(field));
    app.handle_key(key(KeyCode::Char('!')));
    app.handle_key(ctrl(KeyCode::Char('z')));
    assert_eq!(app.text_area(field).expect("field").text, original);
    app.handle_key(ctrl(KeyCode::Char('y')));
    assert_eq!(
        app.text_area(field).expect("field").text,
        "alpha beta\ngamma delta!"
    );
}
