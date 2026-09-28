//! `LspSyncMiddleware` 同步测试。
//!
//! 全部用例经**替身端口**（显式实现 A30 的三个同步方法，`AtomicUsize` 计数 +
//! 顺序轨迹）断言调用顺序、参数与降级语义——不依赖真实 language server，
//! 也不以「未报错」代替计数断言。

use std::{
    any::Any,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use peri_acp_types::ports::{LspPoolPort, LspSyncError};
use peri_agent::{
    messages::BaseMessage, middleware::state::MiddlewareState, session::MessageQueue,
};

use super::*;

/// 端口调用轨迹（顺序敏感）：ready 探测 / 内容变更 / 保存。
#[derive(Debug, Clone, PartialEq, Eq)]
enum SyncStep {
    Ready(PathBuf),
    Change { path: PathBuf, text: String },
    Save(PathBuf),
}

/// 替身端口：`ready` / `change_error` 可配置，三个方法各自计数并记录轨迹。
struct StubPort {
    ready: bool,
    change_error: bool,
    steps: Mutex<Vec<SyncStep>>,
    ready_calls: AtomicUsize,
    change_calls: AtomicUsize,
    save_calls: AtomicUsize,
}

impl StubPort {
    fn new(ready: bool) -> Self {
        Self {
            ready,
            change_error: false,
            steps: Mutex::new(Vec::new()),
            ready_calls: AtomicUsize::new(0),
            change_calls: AtomicUsize::new(0),
            save_calls: AtomicUsize::new(0),
        }
    }

    /// `did_change` 返回 typed 错误的替身（ready 为真，确保走到发送段）。
    fn with_change_error() -> Self {
        Self {
            change_error: true,
            ..Self::new(true)
        }
    }

    fn steps(&self) -> Vec<SyncStep> {
        self.steps.lock().unwrap().clone()
    }

    fn counts(&self) -> (usize, usize, usize) {
        (
            self.ready_calls.load(Ordering::SeqCst),
            self.change_calls.load(Ordering::SeqCst),
            self.save_calls.load(Ordering::SeqCst),
        )
    }
}

#[async_trait]
impl LspPoolPort for StubPort {
    fn as_any(&self) -> &dyn Any {
        self
    }

    async fn shutdown(&self) {}

    fn ready_for(&self, path: &Path) -> bool {
        self.ready_calls.fetch_add(1, Ordering::SeqCst);
        self.steps
            .lock()
            .unwrap()
            .push(SyncStep::Ready(path.to_path_buf()));
        self.ready
    }

    async fn did_change(&self, path: &Path, text: &str) -> Result<(), LspSyncError> {
        self.change_calls.fetch_add(1, Ordering::SeqCst);
        self.steps.lock().unwrap().push(SyncStep::Change {
            path: path.to_path_buf(),
            text: text.to_string(),
        });
        if self.change_error {
            return Err(LspSyncError::Protocol {
                reason: "替身端口强制失败".to_string(),
            });
        }
        Ok(())
    }

    async fn did_save(&self, path: &Path) -> Result<(), LspSyncError> {
        self.save_calls.fetch_add(1, Ordering::SeqCst);
        self.steps
            .lock()
            .unwrap()
            .push(SyncStep::Save(path.to_path_buf()));
        Ok(())
    }
}

/// 用替身端口（保持具体类型以便断言）构造被测中间件。
fn make_middleware(port: &Arc<StubPort>) -> LspSyncMiddleware {
    LspSyncMiddleware::new(Arc::clone(port) as Arc<dyn LspPoolPort>)
}

/// 最小 hook 态：`after_tool` 只用 `StateView::cwd()`。
struct TestState {
    cwd: String,
    messages: Vec<BaseMessage>,
    queue: MessageQueue,
    recall: Vec<String>,
}

impl TestState {
    fn new(cwd: &str) -> Self {
        Self {
            cwd: cwd.to_string(),
            messages: Vec::new(),
            queue: MessageQueue::new(),
            recall: Vec::new(),
        }
    }
}

impl MiddlewareState for TestState {
    fn cwd(&self) -> &str {
        &self.cwd
    }

    fn messages(&self) -> &[BaseMessage] {
        &self.messages
    }

    fn add_message(&mut self, message: BaseMessage) {
        self.messages.push(message);
    }

    fn replace_message(&mut self, message: BaseMessage) -> bool {
        let Some(existing) = self
            .messages
            .iter_mut()
            .find(|existing| existing.id() == message.id())
        else {
            return false;
        };
        *existing = message;
        true
    }

    fn current_step(&self) -> usize {
        0
    }

    fn push_recall(&mut self, item: String) {
        self.recall.push(item);
    }

    fn drain_recall(&mut self) -> Vec<String> {
        std::mem::take(&mut self.recall)
    }

    fn v2_queue(&self) -> &MessageQueue {
        &self.queue
    }
}

/// 跑一次 `after_tool`（工具结果固定为成功，中间件不得改写它）。
async fn run_after_tool(
    mw: &LspSyncMiddleware,
    state: &mut TestState,
    tool_name: &str,
    input: serde_json::Value,
) -> AgentResult<()> {
    let tool_call = ToolCall::new("call-test", tool_name, input);
    let result = ToolResult::success("call-test", tool_name, "工具原始结果");
    mw.after_tool(state, &tool_call, &result).await
}

/// 门禁：`Write` 后顺序严格 ready → didChange → didSave，计数各 1，
/// 端口收到的文本与磁盘一致，路径为绝对路径。
#[tokio::test]
async fn write_sync_orders_change_then_save() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("main.rs");
    std::fs::write(&file, "fn main() {}\n").unwrap();

    let port = Arc::new(StubPort::new(true));
    let mw = make_middleware(&port);
    let mut state = TestState::new(dir.path().to_str().unwrap());

    let outcome = run_after_tool(
        &mw,
        &mut state,
        TOOL_WRITE,
        serde_json::json!({ "file_path": file.to_str().unwrap() }),
    )
    .await;

    assert!(outcome.is_ok(), "同步恒不得改变工具结果: {outcome:?}");
    assert_eq!(
        port.counts(),
        (1, 1, 1),
        "ready / didChange / didSave 应各调用一次: {:?}",
        port.steps()
    );
    assert_eq!(
        port.steps(),
        vec![
            SyncStep::Ready(file.clone()),
            SyncStep::Change {
                path: file.clone(),
                text: "fn main() {}\n".to_string(),
            },
            SyncStep::Save(file.clone()),
        ],
        "同步顺序必须是 ready → didChange → didSave，且文本来自磁盘"
    );
    for step in port.steps() {
        let path = match &step {
            SyncStep::Ready(path) | SyncStep::Save(path) => path,
            SyncStep::Change { path, .. } => path,
        };
        assert!(path.is_absolute(), "端口必须收到绝对路径: {path:?}");
    }
}

/// ready 为假 ⇒ 不读文件、不发任何通知（文件真实存在且可读时也如此）。
///
/// 「门禁先于文件访问」由姊妹用例 `not_ready_with_missing_path_skips_file_access`
/// 直接可观察（路径不存在时 ready 探测仍恰一次）；本用例断言通知计数为 0。
#[tokio::test]
async fn not_ready_skips_read_and_notifications() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("main.rs");
    std::fs::write(&file, "fn main() {}\n").unwrap();

    let port = Arc::new(StubPort::new(false));
    let mw = make_middleware(&port);
    let mut state = TestState::new(dir.path().to_str().unwrap());

    let outcome = run_after_tool(
        &mw,
        &mut state,
        TOOL_WRITE,
        serde_json::json!({ "file_path": file.to_str().unwrap() }),
    )
    .await;

    assert!(outcome.is_ok(), "{outcome:?}");
    // ready 探测是门禁谓词自身，恰一次；被断言「全 0」的是读文件之后的通知计数。
    assert_eq!(
        port.counts(),
        (1, 0, 0),
        "ready 为假时不得发送 didChange / didSave: {:?}",
        port.steps()
    );
    assert_eq!(
        port.steps(),
        vec![SyncStep::Ready(file.clone())],
        "ready 判定为假后必须立即返回，不读文件、不发通知"
    );
}

/// ready 为假 + 路径不存在：`ready_for` 仍恰调用一次、通知计数为 0。
///
/// 轨迹只有 `Ready` 一项 ⇒ 文件访问发生在 ready 判定**之后**（若实现先读文件，
/// 这条不存在的路径会让 `ready_for` 一次都不被调用，计数退化为 `(0, 0, 0)`）。
#[tokio::test]
async fn not_ready_with_missing_path_skips_file_access() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("does-not-exist.rs");

    let port = Arc::new(StubPort::new(false));
    let mw = make_middleware(&port);
    let mut state = TestState::new(dir.path().to_str().unwrap());

    let outcome = run_after_tool(
        &mw,
        &mut state,
        TOOL_EDIT,
        serde_json::json!({ "file_path": missing.to_str().unwrap() }),
    )
    .await;

    assert!(outcome.is_ok(), "{outcome:?}");
    assert_eq!(port.counts(), (1, 0, 0), "{:?}", port.steps());
    assert_eq!(
        port.steps(),
        vec![SyncStep::Ready(missing.clone())],
        "ready 门禁必须先于任何文件访问"
    );
}

/// `did_change` 返回 Err ⇒ 仍然尝试 `did_save`，`after_tool` 恒 `Ok(())`。
#[tokio::test]
async fn change_error_still_attempts_save() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("main.rs");
    std::fs::write(&file, "fn main() {}\n").unwrap();

    let port = Arc::new(StubPort::with_change_error());
    let mw = make_middleware(&port);
    let mut state = TestState::new(dir.path().to_str().unwrap());

    let outcome = run_after_tool(
        &mw,
        &mut state,
        TOOL_WRITE,
        serde_json::json!({ "file_path": file.to_str().unwrap() }),
    )
    .await;

    assert!(outcome.is_ok(), "typed error 只做 debug 降级: {outcome:?}");
    assert_eq!(port.counts(), (1, 1, 1), "{:?}", port.steps());
    assert_eq!(
        port.steps(),
        vec![
            SyncStep::Ready(file.clone()),
            SyncStep::Change {
                path: file.clone(),
                text: "fn main() {}\n".to_string(),
            },
            SyncStep::Save(file.clone()),
        ],
        "didChange 失败后仍必须尝试 didSave，且顺序不变"
    );
}

/// 相对 `file_path` 以 `state.cwd()`（会话工作目录）解析为绝对路径。
///
/// 解析目标只存在于该目录下、且不落在进程 cwd（cargo 从 `peri-middlewares/`
/// 运行，`src/main.rs` 不存在）⇒ 任一以进程 cwd 兜底的实现都会读取失败、
/// 通知计数退化为 0。
#[tokio::test]
async fn relative_path_resolved_against_state_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("src").join("main.rs");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "fn edit() {}\n").unwrap();
    assert!(file.is_absolute(), "夹具前提：解析目标为绝对路径");

    let port = Arc::new(StubPort::new(true));
    let mw = make_middleware(&port);
    let mut state = TestState::new(dir.path().to_str().unwrap());

    let outcome = run_after_tool(
        &mw,
        &mut state,
        TOOL_EDIT,
        serde_json::json!({ "file_path": "src/main.rs" }),
    )
    .await;

    assert!(outcome.is_ok(), "{outcome:?}");
    assert_eq!(
        port.steps(),
        vec![
            SyncStep::Ready(file.clone()),
            SyncStep::Change {
                path: file.clone(),
                text: "fn edit() {}\n".to_string(),
            },
            SyncStep::Save(file.clone()),
        ],
        "相对路径必须按 state.cwd() 拼成绝对路径后交给端口"
    );
}

/// 非 `Write` / `Edit` 调用一律立即返回：不探测 ready、不读文件、不发通知。
#[tokio::test]
async fn non_write_tools_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("main.rs");
    std::fs::write(&file, "fn main() {}\n").unwrap();

    for tool_name in ["Read", "Bash", "LSP", "mcp__lsp__LSP"] {
        let port = Arc::new(StubPort::new(true));
        let mw = make_middleware(&port);
        let mut state = TestState::new(dir.path().to_str().unwrap());

        let outcome = run_after_tool(
            &mw,
            &mut state,
            tool_name,
            serde_json::json!({ "file_path": file.to_str().unwrap() }),
        )
        .await;

        assert!(outcome.is_ok(), "{outcome:?}");
        assert_eq!(port.counts(), (0, 0, 0), "{tool_name} 不应触发同步");
        assert!(port.steps().is_empty(), "{tool_name}: {:?}", port.steps());
    }
}

/// `file_path` 缺失 / 非字符串 ⇒ 立即 `Ok(())`，端口零调用（含 ready 探测）。
#[tokio::test]
async fn missing_or_non_string_file_path_skips_port() {
    let dir = tempfile::tempdir().unwrap();

    for input in [
        serde_json::json!({}),
        serde_json::json!({ "file_path": 42 }),
        serde_json::json!({ "file_path": null }),
    ] {
        let port = Arc::new(StubPort::new(true));
        let mw = make_middleware(&port);
        let mut state = TestState::new(dir.path().to_str().unwrap());

        let outcome = run_after_tool(&mw, &mut state, TOOL_WRITE, input.clone()).await;

        assert!(outcome.is_ok(), "{input}: {outcome:?}");
        assert_eq!(port.counts(), (0, 0, 0), "{input} 不得触达端口");
        assert!(port.steps().is_empty(), "{input}: {:?}", port.steps());
    }
}

/// ready 为真但文件不存在 ⇒ 读失败只 debug 降级，`after_tool` 仍 `Ok(())`，
/// 且不发任何通知（不补拉进程、不用工具结果内容冒充磁盘内容）。
#[tokio::test]
async fn read_failure_degrades_to_ok() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.rs");

    let port = Arc::new(StubPort::new(true));
    let mw = make_middleware(&port);
    let mut state = TestState::new(dir.path().to_str().unwrap());

    let outcome = run_after_tool(
        &mw,
        &mut state,
        TOOL_WRITE,
        serde_json::json!({ "file_path": missing.to_str().unwrap() }),
    )
    .await;

    assert!(outcome.is_ok(), "读失败不得上抛: {outcome:?}");
    assert_eq!(port.counts(), (1, 0, 0), "{:?}", port.steps());
    assert_eq!(
        port.steps(),
        vec![SyncStep::Ready(missing.clone())],
        "读失败后不得发送通知"
    );
}
