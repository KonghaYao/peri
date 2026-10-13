//! raw 工具结果保真契约测试（设计 §7.1：content/structuredContent/isError/_meta
//! 与未知扩展字段无损保留）。

use super::*;
use serde_json::json;

#[test]
fn raw_result_roundtrip_preserves_every_field() {
    let value = json!({
        "content": [
            {"type": "text", "text": "hello"},
            {"type": "image", "data": "aGk=", "mimeType": "image/png"},
            {"type": "resource", "resource": {"uri": "ui://app", "text": "<html/>"}}
        ],
        "structuredContent": {"answer": 42},
        "_meta": {"vendor": {"opaque": true}},
        "isError": true,
        "futureField": {"preserved": [1, 2, 3]}
    });
    let decoded: RawToolResult = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(&decoded).unwrap(), value);
}

#[test]
fn raw_result_accessors_read_typed_facts() {
    let result = RawToolResult::new(json!({
        "content": [{"type": "text", "text": "hello"}],
        "structuredContent": {"answer": 42},
        "_meta": {"peri.invocation": {"invocationId": "inv-1"}},
        "isError": true
    }));

    assert!(result.is_error());
    assert_eq!(result.content().len(), 1);
    assert_eq!(result.content()[0]["text"], "hello");
    assert_eq!(result.structured_content().unwrap()["answer"], 42);
    assert_eq!(
        result.meta().unwrap()["peri.invocation"]["invocationId"],
        "inv-1"
    );
}

#[test]
fn raw_result_default_absent_facts_are_not_inferred() {
    let result = RawToolResult::new(json!({}));

    // 缺省即未报告：不得把缺字段读成成功或失败。
    assert!(!result.is_error());
    assert!(result.content().is_empty());
    assert!(result.structured_content().is_none());
    assert!(result.meta().is_none());
    // 不补全字段：往返后仍是空对象。
    assert_eq!(serde_json::to_value(&result).unwrap(), json!({}));
}

#[test]
fn raw_result_unexpected_field_shape_does_not_lose_payload() {
    // `content` 不是数组时，逐字段结构体会整份解析失败；raw 承载仍然保真。
    let value = json!({"content": "not-an-array", "structuredContent": {"answer": 42}});
    let result: RawToolResult = serde_json::from_value(value.clone()).unwrap();

    assert!(result.content().is_empty());
    assert_eq!(result.structured_content().unwrap()["answer"], 42);
    assert_eq!(result.as_value(), &value);
    assert_eq!(serde_json::to_value(&result).unwrap(), value);
}

#[test]
fn raw_result_null_structured_content_is_absent() {
    let result = RawToolResult::new(json!({"structuredContent": null}));
    assert!(result.structured_content().is_none());
    // 原文仍然保留 `null`——访问器收窄语义，raw 不改写。
    assert_eq!(
        serde_json::to_value(&result).unwrap(),
        json!({"structuredContent": null})
    );
}

#[test]
fn remote_tool_error_carries_protocol_and_in_band_payloads() {
    let protocol = RemoteToolError::from_jsonrpc(JsonRpcError {
        code: -32602,
        message: "invalid params".into(),
        data: Some(json!({"field": "name"})),
    });
    assert_eq!(protocol.jsonrpc.as_ref().unwrap().code, -32602);
    assert!(protocol.in_band.is_none());

    let in_band = RemoteToolError::from_in_band(RawToolResult::new(json!({
        "content": [{"type": "text", "text": "boom"}],
        "isError": true
    })));
    assert!(in_band.jsonrpc.is_none());
    assert!(in_band.in_band.as_ref().unwrap().is_error());

    let encoded = serde_json::to_value(&in_band).unwrap();
    assert!(encoded.get("jsonrpc").is_none());
    assert_eq!(encoded["inBand"]["isError"], true);
}

#[test]
fn json_rpc_error_omits_absent_data() {
    let encoded = serde_json::to_value(JsonRpcError {
        code: -1,
        message: "bad".into(),
        data: None,
    })
    .unwrap();
    assert_eq!(encoded, json!({"code": -1, "message": "bad"}));

    let decoded: JsonRpcError =
        serde_json::from_value(json!({"code": -1, "message": "bad", "data": {"x": 1}})).unwrap();
    assert_eq!(decoded.data, Some(json!({"x": 1})));
}
