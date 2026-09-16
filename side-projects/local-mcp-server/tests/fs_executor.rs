//! 进程内执行内核：工具分派、越界拒绝、取消检查点与 Glob 交付预算（R-003/R-005）。
//!
//! 单进程形态下没有 RPC 通道，因此这里全部用**真实临时工作区根**驱动
//! [`InProcessExecutor`]（无替身对象）：断言的是真实文件系统上的行为。
//!
//! 容器期的"宿主 ↔ 容器路径翻译与回程改写"用例对象已随 D-003 消失：进程内只有
//! **宿主路径单表示**，不存在需要改写的第二种表示。

#[path = "fs_support/mod.rs"]
mod support;

use std::sync::Arc;

use local_mcp_server::error::ToolError;
use local_mcp_server::runtime::InProcessExecutor;
use local_mcp_server::tasks::log::DirOutputPersist;
use local_mcp_server::tasks::{BashTaskConfig, BashTasks, TaskRegistry, TaskRegistryConfig};
use local_mcp_server::wire::{RequestContext, ToolExecutor};
use serde_json::json;

use support::{path_args, tool_request, tree};
use tokio_util::sync::CancellationToken;

/// 真执行器 + 真注册表的测试夹具（工作区根是临时目录）。
struct Fixture {
    root: tempfile::TempDir,
    executor: InProcessExecutor,
    registry: Arc<TaskRegistry>,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::TempDir::new().expect("工作区根");
        let log_dir = root.path().join(".local-mcp/logs");
        std::fs::create_dir_all(&log_dir).expect("日志目录");
        let persist: Arc<dyn local_mcp_server::tasks::log::OutputPersist> =
            Arc::new(DirOutputPersist::new(&log_dir));
        let mut config = TaskRegistryConfig::new(persist);
        config.poll_interval = None;
        let bash = Arc::new(BashTasks::new(BashTaskConfig::new(root.path(), &log_dir)));
        let registry =
            TaskRegistry::new(bash, Arc::new(local_mcp_server::tasks::SystemClock), config);
        let executor = InProcessExecutor::new(root.path().to_path_buf(), Arc::clone(&registry))
            .expect("执行器");
        Self {
            root,
            executor,
            registry,
        }
    }

    fn path(&self, relative: &str) -> String {
        self.root
            .path()
            .join(relative)
            .to_string_lossy()
            .to_string()
    }

    async fn call(
        &self,
        tool: &'static str,
        arguments: serde_json::Value,
    ) -> local_mcp_server::wire::ToolResponse {
        self.executor
            .execute(tool_request(tool, arguments))
            .await
            .expect("工具调用")
    }
}

#[tokio::test]
async fn test_five_file_tools_run_in_process_against_the_workspace_root() {
    let fixture = Fixture::new();
    let root = fixture.path("");

    // Write → Read → Edit → Glob → folder_operations：五条路径都在同一进程内直接执行。
    let write = fixture
        .call(
            "Write",
            json!({ "file_path": "notes/a.txt", "content": "alpha\n" }),
        )
        .await;
    assert!(!write.is_error, "{}", write.text);

    let read = fixture
        .call("Read", path_args(&fixture.path("notes/a.txt")))
        .await;
    assert!(!read.is_error, "{}", read.text);
    assert!(read.text.contains("alpha"), "{}", read.text);

    let edit = fixture
        .call(
            "Edit",
            json!({
                "file_path": "notes/a.txt",
                "old_string": "alpha",
                "new_string": "beta",
            }),
        )
        .await;
    assert!(!edit.is_error, "{}", edit.text);
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("notes/a.txt")).expect("读取"),
        "beta\n"
    );

    let glob = fixture
        .call("Glob", json!({ "pattern": "notes/*.txt" }))
        .await;
    assert!(!glob.is_error, "{}", glob.text);
    assert!(
        glob.text.contains(&root),
        "Glob 结果必须是宿主路径单表示: {}",
        glob.text
    );

    let folder = fixture
        .call(
            "folder_operations",
            json!({ "operation": "list", "folder_path": "notes" }),
        )
        .await;
    assert!(!folder.is_error, "{}", folder.text);
    assert!(folder.text.contains("a.txt"), "{}", folder.text);

    fixture.registry.close().await.expect("关闭");
}

#[tokio::test]
async fn test_out_of_root_paths_are_rejected_without_side_effects() {
    let tree = tree("basic");
    let fixture = Fixture::new();
    let outside = tree.outside_path("secret.txt");

    let response = fixture.call("Read", path_args(&outside)).await;
    assert!(response.is_error, "越界读取必须是工具业务错误");
    assert_eq!(response.structured["denied"], json!("outside_workspace"));
    assert!(
        !response.text.contains("OUTSIDE-SECRET-CONTENT"),
        "拒绝响应不得回显根外内容: {}",
        response.text
    );
    // 根外零副作用：哨兵文件内容未变。
    assert_eq!(
        std::fs::read_to_string(&outside).expect("根外文件应保持原样"),
        "OUTSIDE-SECRET-CONTENT\n"
    );

    fixture.registry.close().await.expect("关闭");
}

#[tokio::test]
async fn test_missing_or_mistyped_paths_follow_the_source_messages() {
    let fixture = Fixture::new();

    // `file_path` 缺失 → 源实现的必需参数文案（业务错误，不是协议错误）。
    let missing = fixture.call("Read", json!({ "offset": 1 })).await;
    assert!(missing.is_error);
    assert!(
        missing.text.contains("file_path"),
        "必须点名缺失字段: {}",
        missing.text
    );

    // `folder_path` 类型错误 → 同样由语义层给出源文案。
    let mistyped = fixture
        .call(
            "folder_operations",
            json!({ "operation": "list", "folder_path": 42 }),
        )
        .await;
    assert!(mistyped.is_error, "{}", mistyped.text);

    fixture.registry.close().await.expect("关闭");
}

#[tokio::test]
async fn test_unknown_tool_is_a_protocol_error() {
    let fixture = Fixture::new();
    let error = fixture
        .executor
        .execute(tool_request("NotATool", json!({})))
        .await
        .expect_err("未知工具是协议错误");
    assert_eq!(
        error,
        ToolError::UnknownTool {
            name: "NotATool".to_string()
        }
    );

    // 参数不是 JSON 对象 → 源文案的 InvalidRequest（协议错误）。
    let error = fixture
        .executor
        .execute(tool_request("Read", json!("not-an-object")))
        .await
        .expect_err("参数形状错误是协议错误");
    assert!(
        matches!(error, ToolError::InvalidRequest { .. }),
        "{error:?}"
    );

    fixture.registry.close().await.expect("关闭");
}

#[tokio::test]
async fn test_cancelled_request_is_refused_before_any_work() {
    let fixture = Fixture::new();
    let context = RequestContext::new("req-cancel", "principal-a", "instance-1");
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let mut request = tool_request("Read", path_args(&fixture.path("nope.txt")));
    request.context = context;
    request.context.cancellation = cancellation;

    let error = fixture
        .executor
        .execute(request)
        .await
        .expect_err("已取消的请求不得进入语义层");
    assert_eq!(
        error,
        ToolError::Internal {
            message: "Request cancelled.".to_string()
        }
    );

    fixture.registry.close().await.expect("关闭");
}

// ─────────────── Glob 交付预算（FC-GLOB-02）───────────────

#[tokio::test]
async fn test_glob_count_budget_truncates_and_persists_the_full_result() {
    let fixture = Fixture::new();
    let bulk = fixture.root.path().join("bulk");
    std::fs::create_dir_all(&bulk).expect("bulk 目录");
    // 1001 个文件触发源实现的条数预算（1000 条）。
    for index in 0..1001 {
        std::fs::write(bulk.join(format!("item-{index:04}.txt")), "x").expect("写文件");
    }

    let response = fixture.call("Glob", json!({ "pattern": "*.txt" })).await;
    assert!(!response.is_error, "{}", response.text);
    assert_eq!(
        response.structured["truncated"],
        json!(true),
        "超过 1000 条必须标记截断"
    );
    assert!(
        response.text.contains("[Output truncated:"),
        "{}",
        &response.text[response.text.len().saturating_sub(200)..]
    );

    let persisted = response.structured["persisted_path"]
        .as_str()
        .expect("超预算必须落盘");
    assert!(
        persisted.starts_with(&fixture.path("")),
        "落盘必须发生在工作区根内: {persisted}"
    );
    let full = std::fs::read_to_string(persisted).expect("读取落盘产物");
    assert_eq!(full.lines().count(), 1001, "落盘内容必须是完整结果");

    fixture.registry.close().await.expect("关闭");
}

#[tokio::test]
async fn test_glob_below_budget_is_not_truncated() {
    let fixture = Fixture::new();
    let bulk = fixture.root.path().join("bulk");
    std::fs::create_dir_all(&bulk).expect("bulk 目录");
    for index in 0..3 {
        std::fs::write(bulk.join(format!("item-{index}.txt")), "x").expect("写文件");
    }

    let response = fixture.call("Glob", json!({ "pattern": "*.txt" })).await;
    assert!(!response.is_error, "{}", response.text);
    assert_eq!(response.structured["truncated"], json!(false));
    assert!(
        response.structured["persisted_path"].is_null(),
        "未超预算不得落盘"
    );

    fixture.registry.close().await.expect("关闭");
}
