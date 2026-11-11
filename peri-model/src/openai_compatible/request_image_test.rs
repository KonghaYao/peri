use super::*;
use crate::{JsonObject, MediaType, ToolCall, ToolResult};

fn image(data: &str) -> ContentBlock {
    ContentBlock::Image {
        source: ImageSource::Base64 {
            media_type: MediaType::new("image/png"),
            data: data.into(),
        },
    }
}

fn calls(ids: &[&str]) -> ModelMessage {
    ModelMessage::assistant(
        Vec::new(),
        ids.iter()
            .map(|id| ToolCall::new(*id, "Read", JsonObject::default()))
            .collect(),
    )
}

fn result(id: &str, content: Vec<ContentBlock>, is_error: bool) -> ModelMessage {
    ModelMessage::tool_result(ToolResult {
        id: None,
        tool_call_id: id.into(),
        name: "Read".into(),
        content,
        is_error,
    })
}

fn body(messages: Vec<ModelMessage>) -> Value {
    body_for_test(
        &OpenAiConfig::new(
            Url::parse("https://fixture.example/v1/").unwrap(),
            "fixture-key",
            "fixture-vision-model",
        ),
        &ModelRequest::new(messages),
    )
}

/// [回归测试] 图片不能塞入 tool 角色，也不能插在同批并行工具结果之间。
#[test]
fn test_tool_images_follow_the_complete_parallel_result_batch() {
    let request = body(vec![
        calls(&["a", "b", "c"]),
        result("a", vec![ContentBlock::text("first"), image("AQID")], false),
        result("b", vec![ContentBlock::text("text only")], false),
        result(
            "c",
            vec![
                image("BAUG"),
                ContentBlock::Image {
                    source: ImageSource::Url {
                        url: "https://fixture.example/photo.png".into(),
                    },
                },
            ],
            false,
        ),
    ]);
    let messages = request["messages"].as_array().unwrap();
    assert_eq!(
        messages
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["assistant", "tool", "tool", "tool", "user"]
    );
    for (index, id) in ["a", "b", "c"].iter().enumerate() {
        assert_eq!(messages[index + 1]["tool_call_id"], *id);
        assert!(
            messages[index + 1]["content"].is_string(),
            "tool 角色仅发送文本"
        );
    }
    assert_eq!(messages[1]["content"], "first");
    assert_eq!(messages[2]["content"], "text only");
    let attachments = messages[4]["content"].as_array().unwrap();
    assert_eq!(attachments.len(), 5);
    assert!(attachments[0]["text"]
        .as_str()
        .unwrap()
        .contains("\"tool_call_id\":\"a\""));
    assert_eq!(
        attachments[1]["image_url"]["url"],
        "data:image/png;base64,AQID"
    );
    assert!(attachments[2]["text"]
        .as_str()
        .unwrap()
        .contains("\"tool_call_id\":\"c\""));
    assert_eq!(
        attachments[3]["image_url"]["url"],
        "data:image/png;base64,BAUG"
    );
    assert_eq!(
        attachments[4]["image_url"]["url"],
        "https://fixture.example/photo.png"
    );
}

#[test]
fn test_tool_images_stay_with_their_batch_before_later_user_input() {
    let source = vec![
        calls(&["a"]),
        result("a", vec![image("AQID")], false),
        ModelMessage::user_text("next task"),
        calls(&["b"]),
        result("b", vec![image("BAUG")], false),
        ModelMessage::assistant_text("done"),
    ];
    let request = body(source.clone());
    let messages = request["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 8);
    assert_eq!(
        messages[2]["content"][1]["image_url"]["url"],
        "data:image/png;base64,AQID"
    );
    assert_eq!(messages[3]["content"], "next task");
    assert_eq!(
        messages[6]["content"][1]["image_url"]["url"],
        "data:image/png;base64,BAUG"
    );
    assert_eq!(messages[7]["content"], "done");
    assert_eq!(body(source), request, "重复投影不能累积附件或改变顺序");
}

#[test]
fn test_text_only_tool_result_keeps_existing_shape_without_attachment_message() {
    let request = body(vec![
        calls(&["a"]),
        result("a", vec![ContentBlock::text("ok")], false),
    ]);
    assert_eq!(request["messages"].as_array().unwrap().len(), 2);
    assert_eq!(
        request["messages"][1],
        json!({"role":"tool", "tool_call_id":"a", "content":"ok"})
    );
}

#[test]
fn test_error_image_is_preserved_as_tool_evidence() {
    let request = body(vec![
        calls(&["failed"]),
        result("failed", vec![image("AQID")], true),
    ]);
    assert_eq!(request["messages"][1]["content"], "");
    assert_eq!(
        request["messages"][2]["content"][1]["image_url"]["url"],
        "data:image/png;base64,AQID"
    );
    assert!(request["messages"][2]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("\"is_error\":true"));
}

#[test]
fn test_user_image_remains_in_its_original_message() {
    let request = body(vec![ModelMessage::User {
        content: vec![image("AQID")],
    }]);
    assert_eq!(request["messages"].as_array().unwrap().len(), 1);
    assert_eq!(
        request["messages"][0]["content"][0]["image_url"]["url"],
        "data:image/png;base64,AQID"
    );
}
