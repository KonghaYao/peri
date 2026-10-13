//! canonical 执行身份与来源契约测试（身份分离：invocation vs 模型 tool-call ID）。

use super::*;

// -- 执行身份 / 来源 ----------------------------------------------------------

#[test]
fn invocation_id_generate_is_uuid_shaped_and_distinct() {
    let first = InvocationId::generate();
    let second = InvocationId::generate();
    assert!(uuid::Uuid::parse_str(first.as_str()).is_ok());
    assert_ne!(first, second);
    // 与 `_meta["peri.invocation"].invocationId` 的线上字符串形状一致。
    assert_eq!(
        serde_json::to_value(&first).unwrap(),
        serde_json::Value::String(first.as_str().to_string())
    );
    assert_eq!(first.to_string().parse::<InvocationId>().unwrap(), first);
}

#[test]
fn invocation_origin_serializes_with_typed_kind() {
    let nested = InvocationOrigin::Nested {
        parent_invocation_id: InvocationId::new("inv-parent"),
    };
    let encoded = serde_json::to_value(&nested).unwrap();
    assert_eq!(
        encoded,
        serde_json::json!({"kind": "nested", "parentInvocationId": "inv-parent"})
    );
    assert_eq!(
        nested.parent_invocation_id().map(InvocationId::as_str),
        Some("inv-parent")
    );
    assert_eq!(nested.model_tool_call_id(), None);
    assert!(!nested.is_app());

    // App 来源：attachment 身份与连接纪元各自 typed，且与服务端世代分开表达
    //（取代此前把「连接附件」与「服务端世代」混为一谈的 `attachment` / `epoch`）。
    let app = InvocationOrigin::App {
        app_id: "app-session-1".into(),
        attachment_id: Some(crate::interaction::AttachmentId::new("connection-1")),
        connection_epoch: Some(crate::interaction::ConnectionEpoch::new(3)),
        server_generation: 5,
    };
    let encoded = serde_json::to_value(&app).unwrap();
    assert_eq!(encoded["kind"], "app");
    assert_eq!(encoded["appId"], "app-session-1");
    assert_eq!(encoded["attachmentId"], "connection-1");
    assert_eq!(encoded["connectionEpoch"], 3);
    assert_eq!(encoded["serverGeneration"], 5);
    assert!(app.is_app());
    assert_eq!(app.model_tool_call_id(), None);

    // 已签发但尚未附着连接的 App：附件事实整体缺失（不是 epoch 为 0 之类占位）。
    let unattached = InvocationOrigin::App {
        app_id: "app-session-2".into(),
        attachment_id: None,
        connection_epoch: None,
        server_generation: 5,
    };
    assert_eq!(
        serde_json::to_value(&unattached).unwrap()["attachmentId"],
        serde_json::Value::Null
    );

    let host = InvocationOrigin::Host {
        namespace: "peri/mcp/invoke".into(),
        seq: 7,
    };
    let encoded = serde_json::to_value(&host).unwrap();
    assert_eq!(
        encoded,
        serde_json::json!({"kind": "host", "namespace": "peri/mcp/invoke", "seq": 7})
    );
}

#[test]
fn effective_tool_call_roundtrips_typed_identity_and_origin() {
    let call = EffectiveToolCall {
        invocation_id: InvocationId::new("inv-1"),
        tool_name: "Read".into(),
        input: serde_json::json!({"file_path": "/tmp/a"}),
        origin: InvocationOrigin::App {
            app_id: "app-1".into(),
            attachment_id: None,
            connection_epoch: None,
            server_generation: 1,
        },
    };
    let encoded = serde_json::to_value(&call).unwrap();
    assert_eq!(encoded["invocationId"], "inv-1");
    let decoded: EffectiveToolCall = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded.invocation_id, call.invocation_id);
    assert_eq!(decoded.origin, call.origin);
}
