use serde_json::{json, Value};
use url::Url;

use super::{
    HistoryError, JsonObject, ResponsesHistoryV1, ResponsesSourceIdentity,
    RESPONSES_HISTORY_VERSION,
};

/// 合成标记：非凭据、非真实密文，只用于断言 Debug/错误不泄露原生负载。
const CIPHER_MARKER: &str = "cipher-marker-not-a-credential";
const ENDPOINT: &str = "https://api.example.com/v1/responses";
const MODEL: &str = "gpt-responses-test";

fn endpoint(raw: &str) -> Url {
    Url::parse(raw).unwrap()
}

fn message_item() -> Value {
    json!({
        "type": "message",
        "id": "msg_1",
        "status": "completed",
        "role": "assistant",
        "phase": "commentary",
        "content": [
            {"type": "output_text", "text": "hello", "annotations": []},
            {"type": "refusal", "refusal": "cannot comply"}
        ],
        "vendor_extra": {"trace": 1}
    })
}

fn reasoning_item() -> Value {
    json!({
        "type": "reasoning",
        "id": "rs_1",
        "summary": [{"type": "summary_text", "text": "thought"}],
        "encrypted_content": CIPHER_MARKER,
        "vendor_trace": "x"
    })
}

fn function_call_item() -> Value {
    json!({
        "type": "function_call",
        "id": "fc_1",
        "call_id": "call_1",
        "name": "shell",
        "status": "completed",
        "arguments": "{\"command\":\"pwd\"}"
    })
}

fn history() -> ResponsesHistoryV1 {
    let source = ResponsesSourceIdentity::capture(&endpoint(ENDPOINT), MODEL).unwrap();
    ResponsesHistoryV1::new(
        source,
        vec![message_item(), reasoning_item(), function_call_item()],
    )
    .unwrap()
}

fn as_value(object: &JsonObject) -> Value {
    Value::Object(object.as_map().clone().into_iter().collect())
}

fn case(item: Value) -> Result<ResponsesHistoryV1, HistoryError> {
    ResponsesHistoryV1::new(history().source().clone(), vec![item])
}

#[test]
fn non_string_status_is_rejected_in_constructor_and_deserialization() {
    for template in [reasoning_item(), message_item(), function_call_item()] {
        for status in [Value::Null, json!(false), json!(1), json!([]), json!({})] {
            let mut item = template.clone();
            item["status"] = status;
            assert_eq!(
                case(item.clone()).unwrap_err(),
                HistoryError::InvalidField { field: "status" }
            );

            let mut encoded = serde_json::to_value(history()).unwrap();
            encoded["items"] = json!([item]);
            let error = serde_json::from_value::<ResponsesHistoryV1>(encoded).unwrap_err();
            assert!(error.to_string().contains("invalid field value: status"));
        }
    }
}

#[test]
fn missing_status_is_only_allowed_for_reasoning() {
    for mut item in [reasoning_item(), message_item(), function_call_item()] {
        item.as_object_mut().unwrap().remove("status");
        if item["type"] == "reasoning" {
            assert!(case(item).is_ok());
        } else {
            assert_eq!(
                case(item).unwrap_err(),
                HistoryError::MissingField { field: "status" }
            );
        }
    }
}

#[test]
fn non_completed_string_status_is_rejected_for_every_item_kind() {
    for template in [reasoning_item(), message_item(), function_call_item()] {
        for status in ["in_progress", "incomplete", "failed", "", "unknown"] {
            let mut item = template.clone();
            item["status"] = json!(status);
            assert_eq!(case(item).unwrap_err(), HistoryError::IncompleteItem);
        }
    }
}

#[test]
fn empty_text_is_valid_but_non_string_text_is_rejected() {
    for text in ["", " \n\t"] {
        let mut message = message_item();
        message["content"][0]["text"] = json!(text);
        message["content"][1]["refusal"] = json!(text);
        let record = case(message).unwrap();
        assert_eq!(record.visible_text(), text);
        assert_eq!(record.refusals(), vec![text]);
        let mut reasoning = reasoning_item();
        reasoning["summary"][0]["text"] = json!(text);
        assert_eq!(case(reasoning).unwrap().reasoning_summary_text(), text);
    }
    let mut message = message_item();
    message["content"][0]["text"] = json!(false);
    assert_eq!(
        case(message).unwrap_err(),
        HistoryError::InvalidField { field: "text" }
    );
}

#[test]
fn projection_preserves_annotations_and_supplies_empty_legacy_annotations() {
    let annotation = json!({"type": "url_citation", "url": "https://example.test", "title": "reference", "start_index": 0, "end_index": 1});
    let mut message = message_item();
    message["content"][0]["annotations"] = json!([annotation]);
    let projected = case(message.clone())
        .unwrap()
        .project_input_items(&endpoint(ENDPOINT), MODEL)
        .unwrap();
    assert_eq!(
        as_value(&projected[0])["content"][0]["annotations"],
        json!([annotation])
    );
    message["content"][0]
        .as_object_mut()
        .unwrap()
        .remove("annotations");
    let projected = case(message)
        .unwrap()
        .project_input_items(&endpoint(ENDPOINT), MODEL)
        .unwrap();
    assert_eq!(
        as_value(&projected[0])["content"][0]["annotations"],
        json!([])
    );
}

#[test]
fn serde_roundtrip_keeps_items_and_source_match() {
    let encoded = serde_json::to_value(history()).unwrap();
    assert_eq!(encoded["version"], json!(RESPONSES_HISTORY_VERSION));

    let decoded: ResponsesHistoryV1 = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded.item_count(), 3);
    assert_eq!(decoded.version(), RESPONSES_HISTORY_VERSION);
    decoded.verify_source(&endpoint(ENDPOINT), MODEL).unwrap();
    assert!(decoded
        .source()
        .matches(&endpoint(ENDPOINT), MODEL)
        .unwrap());

    // 未知非语义字段随原始 JSON 往返保留。
    let re_encoded = serde_json::to_value(&decoded).unwrap();
    assert_eq!(re_encoded["items"][0]["vendor_extra"], json!({"trace": 1}));
    assert_eq!(
        re_encoded["items"][1]["encrypted_content"],
        json!(CIPHER_MARKER)
    );
}

#[test]
fn source_mismatch_is_typed_for_different_endpoint_path_query_and_model() {
    let history = history();
    for (raw, model) in [
        ("https://api.example.com/v2/responses", MODEL),
        ("https://api.example.com/v1/responses?debug=1", MODEL),
        ("https://other.example.com/v1/responses", MODEL),
        (ENDPOINT, "gpt-other"),
    ] {
        let url = endpoint(raw);
        assert_eq!(
            history.verify_source(&url, model),
            Err(HistoryError::SourceMismatch)
        );
        assert!(!history.source().matches(&url, model).unwrap());
        assert_eq!(
            history.project_input_items(&url, model).unwrap_err(),
            HistoryError::SourceMismatch
        );
    }
}

#[test]
fn default_port_is_normalized_but_custom_port_is_not() {
    let default_port = ResponsesSourceIdentity::capture(
        &endpoint("https://api.example.com:443/v1/responses"),
        MODEL,
    )
    .unwrap();
    default_port.verify(&endpoint(ENDPOINT), MODEL).unwrap();

    let custom_port = ResponsesSourceIdentity::capture(
        &endpoint("https://api.example.com:8443/v1/responses"),
        MODEL,
    )
    .unwrap();
    assert!(!custom_port.matches(&endpoint(ENDPOINT), MODEL).unwrap());
}

#[test]
fn nonce_is_fresh_and_identity_persists_without_plaintext_url() {
    let first = ResponsesSourceIdentity::capture(&endpoint(ENDPOINT), MODEL).unwrap();
    let second = ResponsesSourceIdentity::capture(&endpoint(ENDPOINT), MODEL).unwrap();
    let first_repr = serde_json::to_value(&first).unwrap();
    let second_repr = serde_json::to_value(&second).unwrap();

    let nonce = first_repr["nonce"].as_str().unwrap();
    assert_eq!(nonce.len(), 32);
    assert_eq!(first_repr["digest"].as_str().unwrap().len(), 64);
    assert_ne!(first_repr["nonce"], second_repr["nonce"]);
    assert_ne!(first_repr["digest"], second_repr["digest"]);
    assert!(!first_repr.to_string().contains("api.example.com"));

    // 重开（反序列化）后仍可校验，不依赖固定 endpoint 白名单。
    let reopened: ResponsesSourceIdentity = serde_json::from_value(first_repr.clone()).unwrap();
    reopened.verify(&endpoint(ENDPOINT), MODEL).unwrap();

    // Debug 不显示 nonce/digest。
    let rendered = format!("{reopened:?}");
    assert!(!rendered.contains(nonce));
    assert!(!rendered.contains(first_repr["digest"].as_str().unwrap()));
}

#[test]
fn invalid_endpoint_and_model_are_rejected() {
    let identity = ResponsesSourceIdentity::capture(&endpoint(ENDPOINT), MODEL).unwrap();
    for raw in [
        "ftp://api.example.com/v1/responses",
        "https://user:placeholder@api.example.com/v1/responses",
        "https://api.example.com/v1/responses#fragment",
    ] {
        let url = endpoint(raw);
        assert_eq!(
            ResponsesSourceIdentity::capture(&url, MODEL),
            Err(HistoryError::InvalidEndpoint)
        );
        assert_eq!(
            identity.matches(&url, MODEL),
            Err(HistoryError::InvalidEndpoint)
        );
    }
    assert_eq!(
        ResponsesSourceIdentity::capture(&endpoint(ENDPOINT), "   "),
        Err(HistoryError::InvalidModel)
    );
}

#[test]
fn unsupported_version_and_unknown_semantic_item_fail_closed() {
    let mut bumped = serde_json::to_value(history()).unwrap();
    bumped["version"] = json!(RESPONSES_HISTORY_VERSION + 1);
    let error = serde_json::from_value::<ResponsesHistoryV1>(bumped).unwrap_err();
    assert!(error
        .to_string()
        .contains("unsupported responses history version"));

    assert_eq!(
        case(json!({"type": "web_search_call", "id": "ws_1", "status": "completed"})).unwrap_err(),
        HistoryError::UnsupportedItemType
    );
}

#[test]
fn malformed_or_incomplete_items_fail_closed() {
    let cases = [
        (json!("not-an-object"), HistoryError::ItemNotObject),
        (
            json!({"id": "msg_1"}),
            HistoryError::MissingField { field: "type" },
        ),
        (
            json!({"type": "message", "id": "msg_1", "role": "assistant", "status": "in_progress", "content": []}),
            HistoryError::IncompleteItem,
        ),
        (
            json!({"type": "message", "id": "msg_1", "role": "assistant", "content": []}),
            HistoryError::MissingField { field: "status" },
        ),
        (
            json!({"type": "reasoning", "id": "rs_1", "status": "incomplete", "summary": []}),
            HistoryError::IncompleteItem,
        ),
        (
            json!({"type": "message", "id": "msg_1", "role": "user", "status": "completed", "content": []}),
            HistoryError::InvalidField { field: "role" },
        ),
        (
            json!({"type": "message", "id": "msg_1", "role": "assistant", "status": "completed",
                   "content": [{"type": "input_image", "image_url": "https://x/1.png"}]}),
            HistoryError::InvalidField { field: "content" },
        ),
        (
            json!({"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "shell",
                   "status": "completed", "arguments": "not-json"}),
            HistoryError::InvalidArguments,
        ),
        (
            json!({"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "shell",
                   "status": "completed", "arguments": "[1,2]"}),
            HistoryError::InvalidArguments,
        ),
    ];
    for (item, expected) in cases {
        assert_eq!(case(item).unwrap_err(), expected);
    }
}

#[test]
fn duplicate_item_id_and_call_id_are_rejected() {
    let source = history().source().clone();
    let duplicate_id = vec![
        json!({"type": "reasoning", "id": "dup_1", "summary": []}),
        json!({"type": "message", "id": "dup_1", "role": "assistant", "status": "completed", "content": []}),
    ];
    assert_eq!(
        ResponsesHistoryV1::new(source.clone(), duplicate_id).unwrap_err(),
        HistoryError::DuplicateItemId
    );

    let mut second_call = function_call_item();
    second_call["id"] = json!("fc_2");
    assert_eq!(
        ResponsesHistoryV1::new(source, vec![function_call_item(), second_call]).unwrap_err(),
        HistoryError::DuplicateCallId
    );
}

#[test]
fn reasoning_id_and_status_stay_optional_per_schema() {
    assert!(case(json!({"type": "reasoning", "summary": []})).is_ok());

    let calls = case(json!({
        "type": "reasoning",
        "status": "completed",
        "summary": [{"type": "summary_text", "text": "kept"}],
        "encrypted_content": CIPHER_MARKER
    }))
    .unwrap()
    .tool_calls();
    assert!(calls.is_empty());
}

#[test]
fn projection_keeps_phase_and_ciphertext_but_only_legal_fields() {
    let projected: Vec<Value> = history()
        .project_input_items(&endpoint(ENDPOINT), MODEL)
        .unwrap()
        .iter()
        .map(as_value)
        .collect();

    assert_eq!(projected[0]["type"], json!("message"));
    assert_eq!(projected[0]["role"], json!("assistant"));
    assert_eq!(projected[0]["phase"], json!("commentary"));
    assert_eq!(
        projected[0]["content"],
        json!([
            {"type": "output_text", "text": "hello", "annotations": []},
            {"type": "refusal", "refusal": "cannot comply"}
        ])
    );
    // 官方 id/annotations 保留，vendor_extra 等未知字段不回放。
    assert_eq!(projected[0]["id"], "msg_1");
    assert!(projected[0].get("vendor_extra").is_none());
    assert_eq!(projected[0].as_object().unwrap().len(), 6);

    assert_eq!(projected[1]["encrypted_content"], json!(CIPHER_MARKER));
    assert_eq!(
        projected[1]["summary"],
        json!([{"type": "summary_text", "text": "thought"}])
    );
    assert!(projected[1].get("vendor_trace").is_none());
    assert_eq!(projected[1].as_object().unwrap().len(), 4);

    assert_eq!(projected[2]["call_id"], json!("call_1"));
    assert_eq!(projected[2]["arguments"], json!("{\"command\":\"pwd\"}"));
    assert_eq!(projected[2].as_object().unwrap().len(), 5);
}

#[test]
fn derived_text_and_tools_are_not_duplicated() {
    let history = history();
    assert_eq!(history.visible_text(), "hello");
    assert_eq!(history.refusals(), vec!["cannot comply"]);

    let calls = history.tool_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id(), "call_1");
    assert_eq!(calls[0].name(), "shell");
    assert_eq!(
        calls[0].arguments().as_map().get("command"),
        Some(&json!("pwd"))
    );
}

#[test]
fn debug_and_errors_redact_native_payloads() {
    let history = history();
    let source_repr = serde_json::to_value(history.source()).unwrap();
    let nonce = source_repr["nonce"].as_str().unwrap().to_owned();
    let digest = source_repr["digest"].as_str().unwrap().to_owned();

    let rendered = format!("{history:?}");
    for marker in [
        CIPHER_MARKER,
        "hello",
        "cannot comply",
        "thought",
        "pwd",
        "call_1",
        "api.example.com",
        nonce.as_str(),
        digest.as_str(),
    ] {
        assert!(
            !rendered.contains(marker),
            "Debug 泄露 {marker}: {rendered}"
        );
    }

    let error = case(json!({
        "type": "message",
        "id": "msg_secret_id",
        "role": "assistant",
        "status": "in_progress",
        "content": [{"type": "output_text", "text": CIPHER_MARKER}]
    }))
    .unwrap_err();
    assert!(!format!("{error}").contains(CIPHER_MARKER));
    assert!(!format!("{error:?}").contains(CIPHER_MARKER));
    assert!(!format!("{error}").contains("msg_secret_id"));
}
