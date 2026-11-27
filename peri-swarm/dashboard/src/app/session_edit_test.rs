//! Edit-form validation and the update body.

use serde_json::json;

use crate::api::{update_session_body, PromptCacheOptions, Session};
use crate::test_support::{session, with_metadata};
use crate::ui::text;

use super::EditFormState;

fn edit_session() -> Session {
    let mut snapshot = with_metadata(
        session("sess_1", "idle", "active", Some("model-a"), 100),
        &[("env", "prod"), ("team", "swarm")],
    );
    snapshot.prompt_cache_options = Some(PromptCacheOptions {
        ttl: "30m".to_owned(),
    });
    snapshot.agent.instructions = Some("be brief".to_owned());
    snapshot
}

#[test]
fn edit_form_prefills_model_metadata_and_prompt_cache() {
    let form = EditFormState::from_session(&edit_session());
    assert_eq!(form.model().value(), "model-a");
    let keys: Vec<&str> = form
        .metadata()
        .iter()
        .map(|row| row.key().value())
        .collect();
    assert_eq!(keys, ["env", "team"]);
    assert_eq!(form.metadata()[0].value().value(), "prod");
    assert_eq!(form.prompt_cache_options().text, "{\"ttl\":\"30m\"}");
    assert!(!form.submitting());
}

#[test]
fn update_body_nests_the_model_and_never_sends_instructions() {
    let mut form = EditFormState::from_session(&edit_session());
    let spec = form.build_spec().expect("valid spec");
    let body = update_session_body(&spec).expect("body");
    assert_eq!(body["agent"]["model"], "model-a");
    assert!(
        body.get("instructions").is_none(),
        "the gateway rejects instructions on update"
    );
    assert_eq!(body["metadata"]["env"], "prod");
    assert_eq!(body["metadata"]["team"], "swarm");
    assert_eq!(body["prompt_cache_options"]["ttl"], "30m");
    assert!(form.model_error().is_none());
}

#[test]
fn blank_model_is_rejected_before_sending() {
    let mut form = EditFormState::from_session(&edit_session());
    form.model.set_value("   ");
    assert!(form.build_spec().is_none());
    assert_eq!(form.model_error(), Some(text::ERR_MODEL_REQUIRED));
}

#[test]
fn blank_prompt_cache_keeps_the_current_value() {
    let mut form = EditFormState::from_session(&edit_session());
    form.prompt_cache_options.text.clear();
    let spec = form.build_spec().expect("valid spec");
    assert!(spec.prompt_cache_options.is_none());
    let body = update_session_body(&spec).expect("body");
    assert!(body.get("prompt_cache_options").is_none());
}

#[test]
fn literal_null_clears_the_prompt_cache() {
    let mut form = EditFormState::from_session(&edit_session());
    form.prompt_cache_options.text = "null".to_owned();
    let spec = form.build_spec().expect("valid spec");
    assert_eq!(spec.prompt_cache_options, Some(serde_json::Value::Null));
    let body = update_session_body(&spec).expect("body");
    assert_eq!(body["prompt_cache_options"], json!(null));
}

#[test]
fn invalid_prompt_cache_json_is_reported_inline() {
    let mut form = EditFormState::from_session(&edit_session());
    form.prompt_cache_options.text = "{".to_owned();
    assert!(form.build_spec().is_none());
    assert!(form.prompt_cache_error().is_some());
    assert!(form.form_error().is_none());
}

#[test]
fn duplicate_metadata_keys_are_rejected() {
    let mut form = EditFormState::from_session(&edit_session());
    form.metadata[1].key.set_value("env");
    assert!(form.build_spec().is_none());
    assert!(form.metadata_error().is_some());
}

#[test]
fn empty_metadata_rows_clear_the_map() {
    let mut form = EditFormState::from_session(&edit_session());
    form.remove_row(0);
    form.remove_row(0);
    let spec = form.build_spec().expect("valid spec");
    let metadata = spec.metadata.clone().expect("metadata is always sent");
    assert!(metadata.is_empty());
    let body = update_session_body(&spec).expect("body");
    assert_eq!(body["metadata"], json!({}));
}

#[test]
fn submission_reuses_the_key_for_the_same_body_and_replaces_it_on_change() {
    let mut form = EditFormState::from_session(&edit_session());
    let (_, first) = form.prepare_submission().expect("first submission");
    assert!(form.submitting());
    form.finish(false);
    assert!(!form.submitting());
    let (_, retry) = form.prepare_submission().expect("retry");
    assert_eq!(retry, first, "same body keeps the idempotency key");

    form.model.set_value("model-b");
    let (_, changed) = form.prepare_submission().expect("changed submission");
    assert_ne!(changed, first, "changed body gets a new key");
}

#[test]
fn row_edits_clear_their_error() {
    let mut form = EditFormState::from_session(&edit_session());
    form.metadata[1].key.set_value("env");
    assert!(form.build_spec().is_none());
    form.add_row();
    assert!(form.metadata_error().is_none());
    form.remove_row(form.metadata().len() - 1);
}
