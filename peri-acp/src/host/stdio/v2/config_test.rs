use serde_json::{json, Value};

use super::{option_accepts, option_to_v2, options_from, set_config_option};
use crate::host::stdio::v2::state::{Attach, Connection, Routed};
use crate::transport::types::RequestId;

fn mode_option() -> Value {
    json!({
        "configId": "mode",
        "name": "Session Mode",
        "type": "select",
        "currentValue": "default",
        "category": "mode",
        "options": [
            {"value": "default", "name": "Default"},
            {"value": "bypass", "name": "Bypass"}
        ]
    })
}

fn connection_with_config() -> Connection {
    let mut connection = Connection::default();
    connection.insert_slot("A", Attach::Attached).config = vec![mode_option()];
    connection
}

fn error_code(routed: Routed) -> i64 {
    match routed {
        Routed::Answered(Err(error)) => error.code,
        Routed::Answered(Ok(value)) => panic!("expected an error, got {value}"),
        Routed::Forward { method, params, .. } => {
            panic!("expected an error, forwarded {method} {params}")
        }
        Routed::Parked => panic!("expected an error, parked the request"),
    }
}

#[test]
fn option_projection_renames_identity_and_drops_private_metadata() {
    let projected = option_to_v2(json!({
        "id": "mode",
        "name": "Session Mode",
        "type": "select",
        "currentValue": "default",
        "category": "mode",
        "options": [{"group": "g1", "name": "Group", "options": [{"value": "default", "name": "Default"}]}],
        "_meta": {"private": true}
    }));
    assert_eq!(
        projected,
        json!({
            "configId": "mode",
            "name": "Session Mode",
            "type": "select",
            "currentValue": "default",
            "category": "mode",
            "options": [{"groupId": "g1", "name": "Group", "options": [{"value": "default", "name": "Default"}]}]
        })
    );
}

#[test]
fn replies_and_notifications_are_projected_from_their_v1_shape() {
    let reply = json!({
        "configOptions": [{"id": "mode", "name": "Session Mode", "type": "select",
                            "currentValue": "default", "options": [{"value": "default", "name": "Default"}]}],
        "_meta": {"peri.identity": "x"}
    });
    assert_eq!(
        options_from(&reply),
        vec![
            json!({"configId": "mode", "name": "Session Mode", "type": "select",
                    "currentValue": "default", "options": [{"value": "default", "name": "Default"}]})
        ]
    );
    assert!(options_from(&json!({"sessionId": "A"})).is_empty());
}

#[test]
fn a_select_only_accepts_the_values_it_lists() {
    let option = mode_option();
    assert!(option_accepts(&option, "default"));
    assert!(!option_accepts(&option, "nonsense"));
    let grouped = json!({
        "type": "select",
        "options": [{"groupId": "g", "name": "Group",
                     "options": [{"value": "nested", "name": "Nested"}]}]
    });
    assert!(option_accepts(&grouped, "nested"));
    // Unknown option types are forwarded: the pinned rule for a type the client
    // does not understand is to ignore it, not to reject it.
    assert!(option_accepts(&json!({"type": "_custom"}), "anything"));
}

#[test]
fn set_config_option_translates_the_pinned_value_shape() {
    let mut connection = connection_with_config();
    let routed = set_config_option(
        &mut connection,
        RequestId::Number(1),
        &json!({"sessionId": "A", "configId": "mode", "type": "id", "value": "bypass"}),
    );
    match routed {
        Routed::Forward { method, params, .. } => {
            assert_eq!(method, "session/set_config_option");
            assert_eq!(
                params,
                json!({"sessionId": "A", "configId": "mode", "value": "bypass"})
            );
        }
        _ => panic!("expected a forwarded request"),
    }
    assert!(connection.replies.contains_key(&RequestId::Number(1)));
}

#[test]
fn set_config_option_rejects_without_touching_the_configuration() {
    // An unknown session names itself, as every session-scoped request does.
    let mut connection = Connection::default();
    let error = match set_config_option(
        &mut connection,
        RequestId::Number(1),
        &json!({"sessionId": "ghost", "configId": "mode", "type": "id", "value": "bypass"}),
    ) {
        Routed::Answered(Err(error)) => error,
        _ => panic!("expected `-32002`"),
    };
    assert_eq!(error.code, -32002);
    assert_eq!(error.data.expect("sessionId data")["sessionId"], "ghost");
    assert!(connection.replies.is_empty());

    for params in [
        json!({"sessionId": "A", "type": "id", "value": "bypass"}),
        json!({"sessionId": "A", "configId": "mode", "type": "boolean", "value": true}),
        json!({"sessionId": "A", "configId": "mode", "type": "id"}),
        json!({"sessionId": "A", "configId": "mode", "value": "bypass"}),
        json!({"sessionId": "A", "configId": "nope", "type": "id", "value": "bypass"}),
        json!({"sessionId": "A", "configId": "mode", "type": "id", "value": "nonsense"}),
    ] {
        let mut connection = connection_with_config();
        assert_eq!(
            error_code(set_config_option(
                &mut connection,
                RequestId::Number(1),
                &params
            )),
            -32602,
            "{params}"
        );
        assert!(connection.replies.is_empty(), "{params}");
        assert_eq!(
            connection.slots["A"].config,
            vec![mode_option()],
            "{params} must leave the option list untouched"
        );
    }
}
