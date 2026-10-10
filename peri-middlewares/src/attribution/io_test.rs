use std::path::{Path, PathBuf};

use peri_agent::{
    messages::BaseMessage, middleware::state::MiddlewareState, session::MessageQueue,
};

use super::*;

struct MemoryReader {
    text: Mutex<Option<String>>,
    reads: Mutex<Vec<PathBuf>>,
}

#[async_trait]
impl crate::workspace_io::WorkspaceFileReader for MemoryReader {
    async fn read_text(
        &self,
        path: &Path,
    ) -> Result<String, crate::workspace_io::WorkspaceReadError> {
        self.reads.lock().unwrap().push(path.to_path_buf());
        self.text
            .lock()
            .unwrap()
            .clone()
            .ok_or(crate::workspace_io::WorkspaceReadError::ReadFailed)
    }
}

struct TestState {
    queue: MessageQueue,
    messages: Vec<BaseMessage>,
}

impl MiddlewareState for TestState {
    fn cwd(&self) -> &str {
        "/execution-only/workspace"
    }
    fn messages(&self) -> &[BaseMessage] {
        &self.messages
    }
    fn add_message(&mut self, message: BaseMessage) {
        self.messages.push(message);
    }
    fn replace_message(&mut self, _message: BaseMessage) -> bool {
        false
    }
    fn current_step(&self) -> usize {
        0
    }
    fn push_recall(&mut self, _item: String) {}
    fn drain_recall(&mut self) -> Vec<String> {
        Vec::new()
    }
    fn v2_queue(&self) -> &MessageQueue {
        &self.queue
    }
}

#[tokio::test]
async fn attribution_reads_both_snapshots_from_execution_environment() {
    let reader = Arc::new(MemoryReader {
        text: Mutex::new(Some("same OLD tail".to_string())),
        reads: Mutex::new(Vec::new()),
    });
    let middleware = GitAttributionMiddleware::new("test", reader.clone());
    let mut state = TestState {
        queue: MessageQueue::new(),
        messages: Vec::new(),
    };
    let call = ToolCall::new(
        "edit",
        TOOL_EDIT,
        serde_json::json!({"file_path": "src/file.rs"}),
    );
    middleware.before_tool(&mut state, &call).await.unwrap();
    *reader.text.lock().unwrap() = Some("same 新内容 tail".to_string());
    let result = ToolResult::success("edit", TOOL_EDIT, "not file contents");
    middleware
        .after_tool(&mut state, &call, &result)
        .await
        .unwrap();
    assert_eq!(
        middleware.state.lock().unwrap().contributions["src/file.rs"].claude_chars,
        3
    );
    assert_eq!(
        *reader.reads.lock().unwrap(),
        vec![
            PathBuf::from("/execution-only/workspace/src/file.rs"),
            PathBuf::from("/execution-only/workspace/src/file.rs"),
        ]
    );
    assert!(middleware.pending_old_content.lock().unwrap().is_empty());
}

#[tokio::test]
async fn overlapping_calls_to_same_file_keep_independent_before_snapshots() {
    let reader = Arc::new(MemoryReader {
        text: Mutex::new(Some("one".to_string())),
        reads: Mutex::new(Vec::new()),
    });
    let middleware = GitAttributionMiddleware::new("test", reader.clone());
    let mut state = TestState {
        queue: MessageQueue::new(),
        messages: Vec::new(),
    };
    let first = ToolCall::new(
        "first",
        TOOL_WRITE,
        serde_json::json!({"file_path": "file"}),
    );
    let second = ToolCall::new(
        "second",
        TOOL_WRITE,
        serde_json::json!({"file_path": "file"}),
    );
    middleware.before_tool(&mut state, &first).await.unwrap();
    *reader.text.lock().unwrap() = Some("two".to_string());
    middleware.before_tool(&mut state, &second).await.unwrap();
    assert_eq!(
        middleware.pending_old_content.lock().unwrap()["first"],
        "one"
    );
    assert_eq!(
        middleware.pending_old_content.lock().unwrap()["second"],
        "two"
    );
    *reader.text.lock().unwrap() = None;
    middleware
        .after_tool(
            &mut state,
            &first,
            &ToolResult::success("first", TOOL_WRITE, "ok"),
        )
        .await
        .unwrap();
    assert_eq!(middleware.pending_old_content.lock().unwrap().len(), 1);
    assert!(middleware.state.lock().unwrap().contributions.is_empty());
}
