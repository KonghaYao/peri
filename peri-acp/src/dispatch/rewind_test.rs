//! dispatch/rewind 单元测试（预算 + 执行）。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use peri_acp_types::{
    event::ExecutorEvent,
    messages::{BaseMessage, ContentBlock, ToolCallRequest},
};

use super::rewind_preview;
use crate::session::event_sink::EventSink;

// ── Mock EventSink（与 command/rewind_test.rs 同构）──

struct MockEventSink {
    events: Mutex<Vec<(String, String)>>,
}

impl MockEventSink {
    fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl EventSink for MockEventSink {
    async fn push_event(&self, session_id: &str, event: &ExecutorEvent, _context_window: u32) {
        let json = serde_json::to_string(event).unwrap_or_default();
        self.events
            .lock()
            .unwrap()
            .push((session_id.to_string(), json));
    }

    async fn push_done(&self, _session_id: &str, _stop_reason: &str, _request_id: Option<&str>) {}
}

/// 构造带工具调用的历史：U1 → A1(Edit) → U2 → A2(Write)
fn make_history_with_tools() -> Vec<BaseMessage> {
    vec![
        BaseMessage::human("第一轮问题"),
        BaseMessage::ai_with_tool_calls(
            "编辑文件",
            vec![ToolCallRequest {
                id: "tc-edit".into(),
                name: "Edit".into(),
                arguments: serde_json::json!({
                    "file_path": "src/main.rs",
                    "old_string": "old",
                    "new_string": "new",
                }),
            }],
        ),
        BaseMessage::human("第二轮问题"),
        BaseMessage::ai_with_tool_calls(
            "写文件",
            vec![ToolCallRequest {
                id: "tc-write".into(),
                name: "Write".into(),
                arguments: serde_json::json!({
                    "file_path": "new_file.txt",
                }),
            }],
        ),
    ]
}

#[tokio::test]
async fn test_preview_lists_file_changes_after_target() {
    let history = make_history_with_tools();
    let sink: Arc<dyn EventSink> = Arc::new(MockEventSink::new());
    let target_id = history[2].id().as_uuid().to_string(); // U2

    let result = rewind_preview(
        &serde_json::json!({ "target_message_id": target_id }),
        &history,
        &sink,
        "test-session",
    )
    .await
    .unwrap();

    let changes = result["file_changes"].as_array().unwrap();
    assert_eq!(changes.len(), 1, "目标之后只有 Write");
    assert_eq!(changes[0]["path"], "new_file.txt");
    assert_eq!(changes[0]["kind"], "write");
}

#[tokio::test]
async fn test_preview_reverse_order_newest_first() {
    let history = make_history_with_tools();
    let sink: Arc<dyn EventSink> = Arc::new(MockEventSink::new());
    let target_id = history[0].id().as_uuid().to_string(); // U1

    let result = rewind_preview(
        &serde_json::json!({ "target_message_id": target_id }),
        &history,
        &sink,
        "test-session",
    )
    .await
    .unwrap();

    let changes = result["file_changes"].as_array().unwrap();
    assert_eq!(changes.len(), 2);
    assert_eq!(changes[0]["path"], "new_file.txt", "逆序：最新变更在前");
    assert_eq!(changes[1]["path"], "src/main.rs");
    assert_eq!(changes[1]["kind"], "edit");
}

#[tokio::test]
async fn test_preview_target_not_found_returns_error() {
    let history = make_history_with_tools();
    let sink: Arc<dyn EventSink> = Arc::new(MockEventSink::new());

    let result = rewind_preview(
        &serde_json::json!({ "target_message_id": "nonexistent" }),
        &history,
        &sink,
        "test-session",
    )
    .await;

    assert!(result.is_err(), "目标不存在应返回错误");
}

#[tokio::test]
async fn test_preview_no_file_changes_returns_empty_list() {
    let history = vec![
        BaseMessage::human("你好"),
        BaseMessage::ai("你好！有什么可以帮你？"),
    ];
    let sink: Arc<dyn EventSink> = Arc::new(MockEventSink::new());
    let target_id = history[0].id().as_uuid().to_string();

    let result = rewind_preview(
        &serde_json::json!({ "target_message_id": target_id }),
        &history,
        &sink,
        "test-session",
    )
    .await
    .unwrap();

    assert_eq!(
        result["file_changes"].as_array().unwrap().len(),
        0,
        "无文件改动 → 空预算列表"
    );
}

/// Anthropic ContentBlock::ToolUse 格式也需提取（与 RewindCommand 同规则）。
#[tokio::test]
async fn test_preview_extracts_anthropic_tool_use() {
    let history = vec![
        BaseMessage::human("改一下"),
        BaseMessage::ai_from_blocks(vec![ContentBlock::tool_use(
            "block-1",
            "Edit",
            serde_json::json!({
                "file_path": "docs/readme.md",
                "old_string": "a",
                "new_string": "b",
            }),
        )]),
    ];
    let sink: Arc<dyn EventSink> = Arc::new(MockEventSink::new());
    let target_id = history[0].id().as_uuid().to_string();

    let result = rewind_preview(
        &serde_json::json!({ "target_message_id": target_id }),
        &history,
        &sink,
        "test-session",
    )
    .await
    .unwrap();

    let changes = result["file_changes"].as_array().unwrap();
    // P1 修复：ai_from_blocks 双路径（tool_calls + content_blocks）按 id 去重，
    // 同一变更只计一次。
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0]["path"], "docs/readme.md");
}

/// P0：dispatch 层参数缺 revert_files 时默认 true（与 command RewindArgs 双保险）。
#[test]
fn test_execute_args_missing_revert_files_defaults_true() {
    let args: super::RewindArgs = serde_json::from_value(serde_json::json!({
        "target_message_id": "msg-1",
    }))
    .unwrap();
    assert!(args.revert_files, "缺省应回退文件");
    assert_eq!(args.target_message_id, "msg-1");
}

/// P0：target_message_id 也缺失时返回参数错误（不再静默成功）。
#[test]
fn test_execute_args_missing_target_id_fails() {
    let result = serde_json::from_value::<super::RewindArgs>(serde_json::json!({}));
    assert!(result.is_err(), "缺 target_message_id 应解析失败");
}
