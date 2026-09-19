//! dispatch/rewind 单元测试（预算 + 执行）。

use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use peri_acp_types::messages::{BaseMessage, ContentBlock, ToolCallRequest};
use peri_acp_types::{event::ExecutorEvent, store::ThreadStore, thread::ThreadMeta};
use peri_agent::thread::FilesystemThreadStore;
use peri_controller::Controller;
use tokio_util::sync::CancellationToken;

use super::{rewind_execute, rewind_preview};

/// 记录事件的最小 sink：RPC 路径只需要 push_event / push_done 不阻塞。
#[derive(Default)]
struct RecordingEventSink {
    events: std::sync::Mutex<Vec<ExecutorEvent>>,
}

#[async_trait]
impl crate::session::event_sink::EventSink for RecordingEventSink {
    async fn push_event(&self, _session_id: &str, event: &ExecutorEvent, _context_window: u32) {
        self.events.lock().unwrap().push(event.clone());
    }

    async fn push_done(&self, _session_id: &str, _stop_reason: &str, _request_id: Option<&str>) {}
}

/// 跨平台绝对临时目录字符串（Windows 下 `/tmp` 不是绝对路径，
/// rewind_preview 要求 session cwd 必须绝对）。
fn temp_dir_str() -> String {
    std::env::temp_dir().to_string_lossy().into_owned()
}

/// 绝对临时目录下的相对拼接，用于构造工具参数中的绝对路径。
fn temp_dir_join(rel: &str) -> String {
    std::env::temp_dir()
        .join(rel)
        .to_string_lossy()
        .into_owned()
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

/// 携带 Responses 原生历史的 assistant 消息（密文 + 来源身份 + 可见文本）。
fn ai_with_native_history() -> BaseMessage {
    BaseMessage::ai(peri_acp_types::messages::MessageContent::Blocks(vec![
        ContentBlock::text("答案正文"),
        ContentBlock::responses_native_history(serde_json::json!({
            "version": 1,
            "source": {"nonce": "NONCEHEX", "digest": "DIGESTHEX"},
            "items": [{"type": "reasoning", "encrypted_content": "CIPHERTEXT"}],
        })),
    ]))
}

const PRIVATE_MARKERS: [&str; 4] = ["CIPHERTEXT", "encrypted_content", "NONCEHEX", "DIGESTHEX"];

/// RPC 出口是观测面：retained_messages 里的原生密文不得进入 response，且
/// canonical 历史与 SQLite 保持原样，预览指纹计算口径不变。
#[tokio::test]
async fn test_execute_redacts_native_history_in_rpc_response() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap().to_string();
    let store: Arc<dyn ThreadStore> =
        Arc::new(FilesystemThreadStore::new(dir.path().join("threads")));
    let thread_id = store.create_thread(ThreadMeta::new(&cwd)).await.unwrap();
    let history = vec![
        BaseMessage::human("第一轮问题"),
        ai_with_native_history(),
        BaseMessage::human("第二轮问题"),
    ];
    for message in &history {
        store
            .append_message(&thread_id, message.clone())
            .await
            .unwrap();
    }
    let canonical_before = store.load_messages(&thread_id).await.unwrap();
    let target = history[2].id().as_uuid().to_string();
    let preview_params = serde_json::json!({
        "target_message_id": target,
        "revert_files": false,
    });
    let preview = rewind_preview(&preview_params, &history, &cwd, "test-session")
        .await
        .unwrap();

    let controller = Controller::new(store.clone());
    let sink: Arc<dyn crate::session::event_sink::EventSink> =
        Arc::new(RecordingEventSink::default());
    let response = rewind_execute(
        &serde_json::json!({
            "sessionId": "test-session",
            "target_message_id": target,
            "revert_files": false,
            "preview_fingerprint": preview["preview_fingerprint"],
        }),
        history.clone(),
        &cwd,
        &Arc::new(crate::provider::PeriConfig::default()),
        &sink,
        None,
        &CancellationToken::new(),
        &controller,
        Some(thread_id.clone()),
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(response["status"], "executed");
    let wire = response.to_string();
    for private in PRIVATE_MARKERS {
        assert!(
            !wire.contains(private),
            "RPC response 泄漏原生私有载荷: {private}"
        );
    }
    // 可见事实与消息身份保留，客户端仍能重建历史。
    assert!(wire.contains("答案正文"));
    assert_eq!(response["history"].as_array().unwrap().len(), 2);
    assert_eq!(
        response["history"][1]["id"],
        serde_json::json!(history[1].id().as_uuid().to_string())
    );

    // canonical/SQLite 保真：目标之后被删除，保留消息仍带密文。
    let canonical_after = store.load_messages(&thread_id).await.unwrap();
    assert_eq!(canonical_after.len(), canonical_before.len() - 1);
    let retained = serde_json::to_string(&canonical_after).unwrap();
    assert!(retained.contains("CIPHERTEXT"));
    assert!(retained.contains("NONCEHEX"));
}

#[tokio::test]
async fn test_preview_lists_file_changes_after_target() {
    let history = make_history_with_tools();
    let target_id = history[2].id().as_uuid().to_string(); // U2

    let result = rewind_preview(
        &serde_json::json!({ "target_message_id": target_id }),
        &history,
        &temp_dir_str(),
        "test-session",
    )
    .await
    .unwrap();

    let changes = result["file_changes"].as_array().unwrap();
    assert_eq!(changes.len(), 1, "目标之后只有 Write");
    assert_eq!(
        Path::new(changes[0]["path"].as_str().unwrap()),
        Path::new("new_file.txt")
    );
    assert_eq!(changes[0]["kind"], "write");
    let fingerprint = result["preview_fingerprint"].as_str().unwrap();
    assert_eq!(fingerprint.len(), 64);
    assert!(fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()));
}

#[tokio::test]
async fn test_preview_reverse_order_newest_first() {
    let history = make_history_with_tools();
    let target_id = history[0].id().as_uuid().to_string(); // U1

    let result = rewind_preview(
        &serde_json::json!({ "target_message_id": target_id }),
        &history,
        &temp_dir_str(),
        "test-session",
    )
    .await
    .unwrap();

    let changes = result["file_changes"].as_array().unwrap();
    assert_eq!(changes.len(), 2);
    assert_eq!(
        Path::new(changes[0]["path"].as_str().unwrap()),
        Path::new("new_file.txt"),
        "逆序：最新变更在前"
    );
    assert_eq!(
        Path::new(changes[1]["path"].as_str().unwrap()),
        Path::new("src/main.rs")
    );
    assert_eq!(changes[1]["kind"], "edit");
}

#[tokio::test]
async fn test_preview_target_not_found_returns_error() {
    let history = make_history_with_tools();

    let result = rewind_preview(
        &serde_json::json!({ "target_message_id": "nonexistent" }),
        &history,
        &temp_dir_str(),
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
    let target_id = history[0].id().as_uuid().to_string();

    let result = rewind_preview(
        &serde_json::json!({ "target_message_id": target_id }),
        &history,
        &temp_dir_str(),
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
    let target_id = history[0].id().as_uuid().to_string();

    let result = rewind_preview(
        &serde_json::json!({ "target_message_id": target_id }),
        &history,
        &temp_dir_str(),
        "test-session",
    )
    .await
    .unwrap();

    let changes = result["file_changes"].as_array().unwrap();
    // P1 修复：ai_from_blocks 双路径（tool_calls + content_blocks）按 id 去重，
    // 同一变更只计一次。
    assert_eq!(changes.len(), 1);
    assert_eq!(
        Path::new(changes[0]["path"].as_str().unwrap()),
        Path::new("docs/readme.md")
    );
}

#[tokio::test]
async fn test_preview_normalizes_inside_absolute_path_to_project_relative() {
    let inside_path = temp_dir_join("project/src/../src/lib.rs");
    let history = vec![
        BaseMessage::human("改一下"),
        BaseMessage::ai_with_tool_calls(
            "编辑文件",
            vec![ToolCallRequest {
                id: "inside".into(),
                name: "Edit".into(),
                arguments: serde_json::json!({
                    "file_path": inside_path,
                    "old_string": "a",
                    "new_string": "b",
                }),
            }],
        ),
    ];
    let target_id = history[0].id().as_uuid().to_string();
    let result = rewind_preview(
        &serde_json::json!({ "target_message_id": target_id }),
        &history,
        &temp_dir_join("project"),
        "test-session",
    )
    .await
    .unwrap();

    assert_eq!(
        Path::new(result["file_changes"][0]["path"].as_str().unwrap()),
        Path::new("src/lib.rs")
    );
}

#[tokio::test]
async fn test_preview_rejects_path_outside_session_cwd() {
    let outside_path = temp_dir_join("other/secret.txt");
    let history = vec![
        BaseMessage::human("改一下"),
        BaseMessage::ai_with_tool_calls(
            "编辑文件",
            vec![ToolCallRequest {
                id: "outside".into(),
                name: "Write".into(),
                arguments: serde_json::json!({ "file_path": outside_path }),
            }],
        ),
    ];
    let target_id = history[0].id().as_uuid().to_string();
    let error = rewind_preview(
        &serde_json::json!({ "target_message_id": target_id }),
        &history,
        &temp_dir_join("project"),
        "test-session",
    )
    .await
    .unwrap_err();

    assert!(error.message.contains("outside the session cwd"));
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
