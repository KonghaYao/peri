use super::*;

#[test]
fn test_user_input_enqueue_roundtrip_preserves_draft_and_content() {
    let request = EnqueueUserInputRequest {
        session_id: "session".into(),
        generation: "generation".into(),
        command_id: "command".into(),
        input_id: uuid::Uuid::now_v7().to_string(),
        content: MessageContent::text("完整\n正文"),
        original_draft: "@image /tmp/图.png\n完整\n正文".into(),
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
