use super::*;
use serde_json::json;

fn terminal_state() -> Value {
    json!({
        "revision": 0,
        "nextAdmissionSequence": 1,
        "legacyUnknown": {},
        "works": {},
        "batches": {},
        "budgets": {},
        "obligations": {},
        "invocations": {},
        "deliveries": {},
        "admissions": {}
    })
}

#[test]
fn migration_quarantines_published_drafts_without_delivery_evidence() {
    for publication in [Value::Null, json!("missing-delivery")] {
        let mut state = terminal_state();
        state["stagedUserInputs"] = json!({
            "input": {"status": "published", "publicationId": publication}
        });
        assert!(classify_legacy(&state.to_string()).unwrap());
    }
}

#[test]
fn migration_accepts_only_proven_terminal_published_drafts() {
    let mut state = terminal_state();
    state["stagedUserInputs"] = json!({
        "input": {"status": "published", "publicationId": "delivery"}
    });
    state["deliveries"] = json!({"delivery": {"disposition": "withdrawn"}});
    assert!(!classify_legacy(&state.to_string()).unwrap());
    state["deliveries"]["delivery"]["disposition"] = Value::Null;
    assert!(classify_legacy(&state.to_string()).unwrap());
}

#[test]
fn migration_quarantines_orphan_terminal_acknowledgements() {
    let mut state = terminal_state();
    state["terminalAcknowledgements"] = json!({"unproven": {}});
    assert!(classify_legacy(&state.to_string()).unwrap());
}

#[test]
fn migration_quarantines_malformed_optional_responsibility_maps() {
    for field in [
        "stagedUserInputs",
        "terminalObligations",
        "terminalAcknowledgements",
    ] {
        let mut state = terminal_state();
        state[field] = json!([]);
        assert!(classify_legacy(&state.to_string()).unwrap(), "{field}");
    }
}
