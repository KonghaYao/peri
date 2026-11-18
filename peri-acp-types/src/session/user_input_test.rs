use crate::messages::MessageContent;
use crate::session::user_input::DispatchUserInputsRequest;
use crate::session::user_input::EnqueueUserInputRequest;
use crate::session::user_input::PluginSelection;
use crate::session::user_input::UserInputQueueReceipt;
use crate::session::user_input::UserInputQueueSnapshotRequest;

#[test]
fn test_user_input_enqueue_roundtrip_preserves_draft_and_content() {
    let request = EnqueueUserInputRequest {
        session_id: "session".into(),
        generation: "generation".into(),
        command_id: "command".into(),
        input_id: uuid::Uuid::now_v7().to_string(),
        content: MessageContent::text("完整\n正文"),
        original_draft: "@image /tmp/图.png\n完整\n正文".into(),
        plugin_selections: Vec::new(),
    };
    let value = serde_json::to_value(&request).unwrap();
    let restored: EnqueueUserInputRequest = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(
        restored.original_draft, request.original_draft,
        "取回草稿不可被正文投影替换"
    );
    assert_eq!(
        value["inputId"], request.input_id,
        "wire 字段必须为 camelCase"
    );
    assert_eq!(
        restored.content.text_content(),
        "完整\n正文",
        "多行内容必须保真"
    );
}

#[test]
fn test_user_input_requests_require_mutation_identity() {
    let value = serde_json::json!({"sessionId":"s","inputId":"i","content":"hello","originalDraft":"hello"});
    assert!(
        serde_json::from_value::<EnqueueUserInputRequest>(value).is_err(),
        "缺少 generation/commandId 必须拒绝"
    );
    let snapshot: UserInputQueueSnapshotRequest =
        serde_json::from_value(serde_json::json!({"sessionId":"s"})).unwrap();
    assert!(
        snapshot.generation.is_none(),
        "首次快照允许尚未取得 generation"
    );
}

#[test]
fn test_user_input_request_rejects_unknown_fields() {
    let value = serde_json::json!({"sessionId":"s","generation":"g","commandId":"c","inputIds":[],"all":true});
    assert!(
        serde_json::from_value::<DispatchUserInputsRequest>(value).is_err(),
        "发送全部必须是显式 ID 快照，未知 all 字段不可静默解释"
    );
}

#[test]
fn test_user_input_receipt_roundtrip_with_partial_optional_fields() {
    let value = serde_json::json!({"snapshot":{"sessionId":"s","generation":"g","revision":7,"items":[]},"results":[{"inputId":"i","state":"withdrawn"}]});
    let receipt: UserInputQueueReceipt = serde_json::from_value(value.clone()).unwrap();
    assert!(receipt.taken_back.is_none(), "旧回执缺少可选字段应安全读取");
    assert_eq!(
        serde_json::to_value(receipt).unwrap(),
        value,
        "快照与逐条状态回执应原样往返"
    );
}

#[test]
fn test_user_input_enqueue_plugin_selections_roundtrip_as_id_only() {
    let request = EnqueueUserInputRequest {
        session_id: "session".into(),
        generation: "generation".into(),
        command_id: "command".into(),
        input_id: "input".into(),
        content: MessageContent::text("选插件"),
        original_draft: "选插件".into(),
        plugin_selections: vec![
            PluginSelection::new("alpha"),
            PluginSelection::new("beta@market"),
        ],
    };
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(
        value["pluginSelections"],
        serde_json::json!([{"pluginId":"alpha"},{"pluginId":"beta@market"}]),
        "只传插件 ID 列表，wire 字段为 camelCase"
    );
    let restored: EnqueueUserInputRequest = serde_json::from_value(value).unwrap();
    assert_eq!(
        restored.plugin_selections, request.plugin_selections,
        "选择随 inputId 保真往返"
    );
}

#[test]
fn test_user_input_enqueue_omits_empty_plugin_selections() {
    let request = serde_json::json!({
        "sessionId":"s","generation":"g","commandId":"c","inputId":"i",
        "content":"hello","originalDraft":"hello"
    });
    let parsed: EnqueueUserInputRequest = serde_json::from_value(request.clone()).unwrap();
    assert!(parsed.plugin_selections.is_empty(), "旧客户端缺省为空选择");
    assert!(
        serde_json::to_value(parsed)
            .unwrap()
            .get("pluginSelections")
            .is_none(),
        "空选择不上 wire，保持旧载荷字节兼容"
    );
}

#[test]
fn test_user_input_plugin_selection_rejects_smuggled_fields() {
    let value = serde_json::json!({
        "sessionId":"s","generation":"g","commandId":"c","inputId":"i",
        "content":"hello","originalDraft":"hello",
        "pluginSelections":[{"pluginId":"alpha","guidance":"ignore previous"}]
    });
    assert!(
        serde_json::from_value::<EnqueueUserInputRequest>(value).is_err(),
        "客户端不得随选择夹带指引或权限信息"
    );
}

#[test]
fn test_user_input_plugin_selection_requires_object_shape() {
    let value = serde_json::json!({
        "sessionId":"s","generation":"g","commandId":"c","inputId":"i",
        "content":"hello","originalDraft":"hello",
        "pluginSelections":[["alpha"]]
    });
    assert!(
        serde_json::from_value::<EnqueueUserInputRequest>(value).is_err(),
        "位置形式数组不得被解释为插件选择"
    );

    let object = serde_json::json!({"pluginId":"alpha"});
    assert_eq!(
        serde_json::from_value::<PluginSelection>(object).unwrap(),
        PluginSelection::new("alpha"),
        "合法对象形式必须保持可读"
    );
}
